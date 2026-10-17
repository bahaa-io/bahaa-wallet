use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

use bahaa_config::{ConfigError, Registry, Settings};
use bahaa_crypto::{ExposeSecret, SecretString, WordCount};
use bahaa_market::{Market, MarketConfig, MarketError, Request, Snapshot};
use bahaa_store::{Store, StoreError};
use bahaa_types::PriceId;
use bahaa_vault::KdfParams;
use thiserror::Error;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::time::{Instant, sleep_until};

use crate::{Keyring, KeyringError, WalletId, WalletInfo};

const VAULT_FILE: &str = "vault.bin";
const STORE_FILE: &str = "wallet.redb";
const QUEUE: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineConfig {
    pub data_dir: PathBuf,
    pub kdf: KdfParams,
    pub market: Option<MarketConfig>,
    pub auto_lock_unit: Duration,
}

impl EngineConfig {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            kdf: KdfParams::RECOMMENDED,
            market: MarketConfig::bahaa().ok(),
            auto_lock_unit: Duration::from_secs(60),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum EngineError {
    #[error(transparent)]
    Keyring(#[from] KeyringError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Market(#[from] MarketError),
    #[error("the wallet engine has stopped")]
    Stopped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    NoVault,
    Locked,
    Unlocked,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    StateChanged(State),
    WalletsChanged(Vec<WalletInfo>),
    SettingsChanged(Settings),
}

type Reply<T> = oneshot::Sender<Result<T, EngineError>>;

enum Command {
    Create(SecretString, Reply<()>),
    Unlock(SecretString, Reply<()>),
    Lock(Reply<()>),
    Touch,
    Wallets(Reply<Vec<WalletInfo>>),
    Generate(WordCount, SecretString, Reply<(WalletId, SecretString)>),
    Import(SecretString, SecretString, Reply<WalletId>),
    Remove(WalletId, Reply<()>),
    Reveal(WalletId, SecretString, Reply<SecretString>),
    ChangePassword(SecretString, SecretString, Reply<()>),
    Settings(Reply<Settings>),
    SetSettings(Settings, Reply<()>),
    Shutdown(Reply<()>),
}

impl Command {
    fn is_activity(&self) -> bool {
        !matches!(
            self,
            Self::Wallets(_) | Self::Settings(_) | Self::Shutdown(_)
        )
    }
}

#[derive(Clone)]
pub struct Engine {
    commands: mpsc::Sender<Command>,
    events: broadcast::Sender<Event>,
    state: watch::Receiver<State>,
    prices: watch::Receiver<Snapshot>,
}

impl Engine {
    pub fn start(config: EngineConfig) -> Result<Self, EngineError> {
        let EngineConfig {
            data_dir,
            kdf,
            market,
            auto_lock_unit,
        } = config;
        crate::keyring::private_dir(&data_dir)?;
        let store = Store::open(&data_dir.join(STORE_FILE))?;
        let settings = store.settings()?;
        let registry = Registry::builtin()?;
        let keyring = Keyring::new(data_dir.join(VAULT_FILE), kdf);
        let initial = if keyring.exists() {
            State::Locked
        } else {
            State::NoVault
        };
        let market = market.map(Market::spawn).transpose()?;
        let prices = market
            .as_ref()
            .map_or_else(|| watch::channel(Snapshot::default()).1, Market::subscribe);
        let (commands, inbox) = mpsc::channel(QUEUE);
        let (events, _) = broadcast::channel(QUEUE);
        let (publish, state) = watch::channel(initial);
        let worker = Worker {
            keyring: Some(keyring),
            store,
            registry,
            settings,
            market,
            inbox,
            events: events.clone(),
            state: publish,
            deadline: None,
            unit: auto_lock_unit,
        };
        tokio::spawn(worker.run());
        Ok(Self {
            commands,
            events,
            state,
            prices,
        })
    }

    pub fn state(&self) -> State {
        *self.state.borrow()
    }

    pub fn watch_state(&self) -> watch::Receiver<State> {
        self.state.clone()
    }

    pub fn events(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    pub fn prices(&self) -> watch::Receiver<Snapshot> {
        self.prices.clone()
    }

    pub fn touch(&self) {
        let _ = self.commands.try_send(Command::Touch);
    }

    pub async fn create(&self, password: SecretString) -> Result<(), EngineError> {
        self.ask(|reply| Command::Create(password, reply)).await
    }

    pub async fn unlock(&self, password: SecretString) -> Result<(), EngineError> {
        self.ask(|reply| Command::Unlock(password, reply)).await
    }

    pub async fn lock(&self) -> Result<(), EngineError> {
        self.ask(Command::Lock).await
    }

    pub async fn wallets(&self) -> Result<Vec<WalletInfo>, EngineError> {
        self.ask(Command::Wallets).await
    }

    pub async fn generate(
        &self,
        words: WordCount,
        passphrase: SecretString,
    ) -> Result<(WalletId, SecretString), EngineError> {
        self.ask(|reply| Command::Generate(words, passphrase, reply))
            .await
    }

    pub async fn import(
        &self,
        phrase: SecretString,
        passphrase: SecretString,
    ) -> Result<WalletId, EngineError> {
        self.ask(|reply| Command::Import(phrase, passphrase, reply))
            .await
    }

    pub async fn remove(&self, id: WalletId) -> Result<(), EngineError> {
        self.ask(|reply| Command::Remove(id, reply)).await
    }

    pub async fn reveal(
        &self,
        id: WalletId,
        password: SecretString,
    ) -> Result<SecretString, EngineError> {
        self.ask(|reply| Command::Reveal(id, password, reply)).await
    }

    pub async fn change_password(
        &self,
        old: SecretString,
        new: SecretString,
    ) -> Result<(), EngineError> {
        self.ask(|reply| Command::ChangePassword(old, new, reply))
            .await
    }

    pub async fn settings(&self) -> Result<Settings, EngineError> {
        self.ask(Command::Settings).await
    }

    pub async fn set_settings(&self, settings: Settings) -> Result<(), EngineError> {
        self.ask(|reply| Command::SetSettings(settings, reply))
            .await
    }

    pub async fn shutdown(&self) -> Result<(), EngineError> {
        self.ask(Command::Shutdown).await
    }

    async fn ask<T>(&self, make: impl FnOnce(Reply<T>) -> Command) -> Result<T, EngineError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(make(reply))
            .await
            .map_err(|_| EngineError::Stopped)?;
        answer.await.map_err(|_| EngineError::Stopped)?
    }
}

struct Worker {
    keyring: Option<Keyring>,
    store: Store,
    registry: Registry,
    settings: Settings,
    market: Option<Market>,
    inbox: mpsc::Receiver<Command>,
    events: broadcast::Sender<Event>,
    state: watch::Sender<State>,
    deadline: Option<Instant>,
    unit: Duration,
}

impl Worker {
    async fn run(mut self) {
        let shutdown = loop {
            let deadline = self.deadline;
            let expired = async move {
                match deadline {
                    Some(deadline) => sleep_until(deadline).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                command = self.inbox.recv() => match command {
                    None => break None,
                    Some(Command::Shutdown(reply)) => break Some(reply),
                    Some(command) => {
                        if command.is_activity() {
                            self.activity();
                        }
                        self.handle(command).await;
                    }
                },
                () = expired => self.lock(),
            }
        };
        drop(self);
        if let Some(reply) = shutdown {
            let _ = reply.send(Ok(()));
        }
    }

    async fn handle(&mut self, command: Command) {
        match command {
            Command::Create(password, reply) => {
                let result = self
                    .keyring(move |keyring| keyring.create(password.expose_secret().as_bytes()))
                    .await;
                self.after_unlock(result.is_ok());
                let _ = reply.send(result);
            }
            Command::Unlock(password, reply) => {
                let result = self
                    .keyring(move |keyring| keyring.unlock(password.expose_secret().as_bytes()))
                    .await;
                self.after_unlock(result.is_ok());
                let _ = reply.send(result);
            }
            Command::Lock(reply) => {
                self.lock();
                let _ = reply.send(Ok(()));
            }
            Command::Touch | Command::Shutdown(_) => {}
            Command::Wallets(reply) => {
                let result = self.keyring(|keyring| keyring.wallets()).await;
                let _ = reply.send(result);
            }
            Command::Generate(words, passphrase, reply) => {
                let result = self
                    .keyring(move |keyring| keyring.generate(words, passphrase.expose_secret()))
                    .await;
                self.after_change(result.is_ok()).await;
                let _ = reply.send(result);
            }
            Command::Import(phrase, passphrase, reply) => {
                let result = self
                    .keyring(move |keyring| {
                        keyring.import(phrase.expose_secret(), passphrase.expose_secret())
                    })
                    .await;
                self.after_change(result.is_ok()).await;
                let _ = reply.send(result);
            }
            Command::Remove(id, reply) => {
                let result = self.keyring(move |keyring| keyring.remove(id)).await;
                self.after_change(result.is_ok()).await;
                let _ = reply.send(result);
            }
            Command::Reveal(id, password, reply) => {
                let result = self
                    .keyring(move |keyring| keyring.reveal(id, password.expose_secret().as_bytes()))
                    .await;
                let _ = reply.send(result);
            }
            Command::ChangePassword(old, new, reply) => {
                let result = self
                    .keyring(move |keyring| {
                        keyring.change_password(
                            old.expose_secret().as_bytes(),
                            new.expose_secret().as_bytes(),
                        )
                    })
                    .await;
                let _ = reply.send(result);
            }
            Command::Settings(reply) => {
                let _ = reply.send(Ok(self.settings.clone()));
            }
            Command::SetSettings(settings, reply) => {
                let result = self
                    .store
                    .save_settings(&settings)
                    .map_err(EngineError::from);
                if result.is_ok() {
                    self.settings = settings.clone();
                    let _ = self.events.send(Event::SettingsChanged(settings));
                    self.activity();
                    self.refresh_prices();
                }
                let _ = reply.send(result);
            }
        }
    }

    async fn keyring<T: Send + 'static>(
        &mut self,
        work: impl FnOnce(&mut Keyring) -> Result<T, KeyringError> + Send + 'static,
    ) -> Result<T, EngineError> {
        let mut keyring = self.keyring.take().ok_or(EngineError::Stopped)?;
        let (keyring, result) = tokio::task::spawn_blocking(move || {
            let result = work(&mut keyring);
            (keyring, result)
        })
        .await
        .map_err(|_| EngineError::Stopped)?;
        self.keyring = Some(keyring);
        result.map_err(EngineError::from)
    }

    fn unlocked(&self) -> bool {
        self.keyring.as_ref().is_some_and(Keyring::is_unlocked)
    }

    fn after_unlock(&mut self, ok: bool) {
        if ok {
            self.set_state(State::Unlocked);
            self.activity();
            self.refresh_prices();
        }
    }

    async fn after_change(&mut self, ok: bool) {
        if ok && let Ok(wallets) = self.keyring(|keyring| keyring.wallets()).await {
            let _ = self.events.send(Event::WalletsChanged(wallets));
        }
    }

    fn lock(&mut self) {
        if let Some(keyring) = self.keyring.as_mut() {
            keyring.lock();
        }
        self.deadline = None;
        let state = if self.keyring.as_ref().is_some_and(Keyring::exists) {
            State::Locked
        } else {
            State::NoVault
        };
        self.set_state(state);
        self.refresh_prices();
    }

    fn activity(&mut self) {
        if self.unlocked() {
            let minutes = u32::from(self.settings.lock_after_minutes);
            self.deadline = Some(Instant::now() + self.unit * minutes);
        }
    }

    fn set_state(&self, state: State) {
        if *self.state.borrow() != state {
            self.state.send_replace(state);
            let _ = self.events.send(Event::StateChanged(state));
        }
    }

    fn refresh_prices(&self) {
        let Some(market) = &self.market else {
            return;
        };
        let wanted: BTreeSet<PriceId> = if self.unlocked() {
            self.registry
                .networks()
                .filter(|network| !network.testnet)
                .flat_map(|network| {
                    network
                        .assets()
                        .filter_map(|(_, coin)| coin.price.clone())
                        .collect::<Vec<_>>()
                })
                .collect()
        } else {
            BTreeSet::new()
        };
        market.want(Request {
            wanted,
            base: self.settings.fiat.price_base().to_owned(),
        });
    }
}
