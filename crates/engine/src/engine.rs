mod incoming;
mod send;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bahaa_chain::ChainError;
use bahaa_config::{ConfigError, Registry, Settings};
use bahaa_crypto::{ExposeSecret, SecretString, WordCount};
use bahaa_market::{Market, MarketConfig, MarketError, Request, Snapshot};
use bahaa_store::{CustomToken, Received, Scan, SentTx, Store, StoreError};
use bahaa_vault::KdfParams;
use thiserror::Error;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep_until};

use crate::chains::{self, AnyChain, Fetched};
use crate::portfolio::{self, Account, Held, NetworkHealth, Portfolio};
use crate::{Keyring, KeyringError, WalletId, WalletInfo};

pub use incoming::IncomingHistory;
use send::{Delivered, Prepared, Quoted};
pub use send::{Draft, DraftId, SendRequest};

const VAULT_FILE: &str = "vault.bin";
const STORE_FILE: &str = "wallet.redb";
const QUEUE: usize = 32;

#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub data_dir: PathBuf,
    pub kdf: KdfParams,
    pub market: Option<MarketConfig>,
    pub registry: Option<Registry>,
    pub auto_lock_unit: Duration,
    pub refresh_every: Duration,
    pub draft_ttl: Duration,
    pub track_every: Duration,
}

impl EngineConfig {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            kdf: KdfParams::RECOMMENDED,
            market: MarketConfig::bahaa().ok(),
            registry: None,
            auto_lock_unit: Duration::from_secs(60),
            refresh_every: Duration::from_secs(60),
            draft_ttl: Duration::from_secs(120),
            track_every: Duration::from_secs(4),
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
    #[error(transparent)]
    Chain(#[from] ChainError),
    #[error("there is no such send waiting for confirmation, prepare it again")]
    UnknownDraft,
    #[error("this fee quote has expired, prepare the send again")]
    DraftExpired,
    #[error(
        "the previous transaction from this account is still on its way, try again in a moment"
    )]
    StillSending,
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
    AccountsChanged(Vec<Account>),
    SettingsChanged(Settings),
    TxUpdated(Box<SentTx>),
    Received(Vec<Received>),
}

type Reply<T> = oneshot::Sender<Result<T, EngineError>>;

enum Command {
    Create(SecretString, Reply<()>),
    Unlock(SecretString, Reply<()>),
    Lock(Reply<()>),
    Touch,
    Refresh,
    Wallets(Reply<Vec<WalletInfo>>),
    Accounts(Reply<Vec<Account>>),
    Generate(WordCount, SecretString, Reply<(WalletId, SecretString)>),
    Import(SecretString, SecretString, Reply<WalletId>),
    Remove(WalletId, Reply<()>),
    Reveal(WalletId, SecretString, Reply<SecretString>),
    ChangePassword(SecretString, SecretString, Reply<()>),
    Settings(Reply<Settings>),
    SetSettings(Settings, Reply<()>),
    PrepareSend(SendRequest, Reply<Draft>),
    ConfirmSend(DraftId, SecretString, Reply<SentTx>),
    History(Reply<Vec<SentTx>>),
    Incoming(Reply<IncomingHistory>),
    Shutdown(Reply<()>),
}

enum Notice {
    Fetched(Fetched),
    Quoted(Box<Quoted>),
    Delivered(Box<Delivered>),
    Tracked(Vec<SentTx>),
}

impl Command {
    fn is_activity(&self) -> bool {
        !matches!(
            self,
            Self::Wallets(_)
                | Self::Accounts(_)
                | Self::Settings(_)
                | Self::History(_)
                | Self::Incoming(_)
                | Self::Shutdown(_)
        )
    }
}

#[derive(Clone)]
pub struct Engine {
    commands: mpsc::Sender<Command>,
    events: broadcast::Sender<Event>,
    state: watch::Receiver<State>,
    prices: watch::Receiver<Snapshot>,
    portfolio: watch::Receiver<Portfolio>,
}

impl Engine {
    pub fn start(config: EngineConfig) -> Result<Self, EngineError> {
        let EngineConfig {
            data_dir,
            kdf,
            market,
            registry,
            auto_lock_unit,
            refresh_every,
            draft_ttl,
            track_every,
        } = config;
        crate::keyring::private_dir(&data_dir)?;
        let store = Store::open(&data_dir.join(STORE_FILE))?;
        let settings = store.settings()?;
        let custom = Arc::new(store.tokens()?);
        let mut history = store.sent()?;
        history.sort_by_key(|tx| std::cmp::Reverse(tx.created_at));
        let scans = store.scans()?;
        let mut received = store.received()?;
        incoming::sort(&mut received);
        let registry = match registry {
            Some(registry) => registry,
            None => Registry::builtin()?,
        };
        let chains: Arc<Vec<AnyChain>> = Arc::new(chains::build(&registry));
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
        let (publish_state, state) = watch::channel(initial);
        let (publish_portfolio, portfolio) = watch::channel(Portfolio {
            fiat: settings.fiat,
            ..Portfolio::default()
        });
        let (notices_tx, notices) = mpsc::channel(QUEUE);
        let worker = Worker {
            keyring: Some(keyring),
            store,
            registry,
            chains,
            custom,
            settings,
            market,
            prices: market_prices(&prices),
            inbox,
            events: events.clone(),
            state: publish_state,
            portfolio: publish_portfolio,
            notices_tx,
            notices,
            accounts: Vec::new(),
            held: Vec::new(),
            health: Vec::new(),
            generation: 0,
            refreshing: None,
            deadline: None,
            next_refresh: None,
            unit: auto_lock_unit,
            refresh_every,
            session: 0,
            drafts: BTreeMap::new(),
            next_draft: 0,
            history,
            scans,
            received,
            broadcasting: BTreeSet::new(),
            tracking: None,
            next_track: None,
            draft_ttl,
            track_every,
        };
        tokio::spawn(worker.run());
        Ok(Self {
            commands,
            events,
            state,
            prices,
            portfolio,
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

    pub fn portfolio(&self) -> watch::Receiver<Portfolio> {
        self.portfolio.clone()
    }

    pub fn touch(&self) {
        let _ = self.commands.try_send(Command::Touch);
    }

    pub fn refresh(&self) {
        let _ = self.commands.try_send(Command::Refresh);
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

    pub async fn accounts(&self) -> Result<Vec<Account>, EngineError> {
        self.ask(Command::Accounts).await
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

    pub async fn prepare_send(&self, request: SendRequest) -> Result<Draft, EngineError> {
        self.ask(|reply| Command::PrepareSend(request, reply)).await
    }

    pub async fn confirm_send(
        &self,
        draft: DraftId,
        password: SecretString,
    ) -> Result<SentTx, EngineError> {
        self.ask(|reply| Command::ConfirmSend(draft, password, reply))
            .await
    }

    pub async fn history(&self) -> Result<Vec<SentTx>, EngineError> {
        self.ask(Command::History).await
    }

    pub async fn incoming(&self) -> Result<IncomingHistory, EngineError> {
        self.ask(Command::Incoming).await
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

fn market_prices(prices: &watch::Receiver<Snapshot>) -> watch::Receiver<Snapshot> {
    let mut prices = prices.clone();
    prices.mark_unchanged();
    prices
}

struct Worker {
    keyring: Option<Keyring>,
    store: Store,
    registry: Registry,
    chains: Arc<Vec<AnyChain>>,
    custom: Arc<Vec<CustomToken>>,
    settings: Settings,
    market: Option<Market>,
    prices: watch::Receiver<Snapshot>,
    inbox: mpsc::Receiver<Command>,
    events: broadcast::Sender<Event>,
    state: watch::Sender<State>,
    portfolio: watch::Sender<Portfolio>,
    notices_tx: mpsc::Sender<Notice>,
    notices: mpsc::Receiver<Notice>,
    accounts: Vec<Account>,
    held: Vec<Held>,
    health: Vec<NetworkHealth>,
    generation: u64,
    refreshing: Option<JoinHandle<()>>,
    deadline: Option<Instant>,
    next_refresh: Option<Instant>,
    unit: Duration,
    refresh_every: Duration,
    session: u64,
    drafts: BTreeMap<DraftId, Prepared>,
    next_draft: u64,
    history: Vec<SentTx>,
    scans: Vec<Scan>,
    received: Vec<Received>,
    broadcasting: BTreeSet<String>,
    tracking: Option<JoinHandle<()>>,
    next_track: Option<Instant>,
    draft_ttl: Duration,
    track_every: Duration,
}

fn answer<T>(reply: Reply<T>, result: Result<T, EngineError>) {
    let _ = reply.send(result);
}

async fn at(moment: Option<Instant>) {
    match moment {
        Some(moment) => sleep_until(moment).await,
        None => std::future::pending().await,
    }
}

impl Worker {
    async fn run(mut self) {
        let shutdown = loop {
            let deadline = self.deadline;
            let next_refresh = self.next_refresh;
            let next_track = self.next_track;
            let has_market = self.market.is_some();
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
                () = at(deadline) => self.lock(),
                () = at(next_refresh) => self.start_refresh(),
                () = at(next_track) => self.start_tracking(),
                Some(notice) = self.notices.recv() => self.notice(notice),
                Ok(()) = self.prices.changed(), if has_market => self.publish_portfolio(),
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
                self.opened(result, reply).await;
            }
            Command::Unlock(password, reply) => {
                let result = self
                    .keyring(move |keyring| keyring.unlock(password.expose_secret().as_bytes()))
                    .await;
                self.opened(result, reply).await;
            }
            Command::Lock(reply) => {
                self.lock();
                let _ = reply.send(Ok(()));
            }
            Command::Refresh => self.start_refresh(),
            Command::PrepareSend(request, reply) => self.prepare_send(&request, reply),
            Command::ConfirmSend(id, password, reply) => {
                self.confirm_send(id, password, reply).await;
            }
            Command::History(reply) => answer(reply, self.history()),
            Command::Incoming(reply) => answer(reply, self.incoming()),
            Command::Touch | Command::Shutdown(_) => {}
            Command::Wallets(reply) => {
                let result = self.keyring(|keyring| keyring.wallets()).await;
                let _ = reply.send(result);
            }
            Command::Accounts(reply) => {
                let result = if self.unlocked() {
                    Ok(self.accounts.clone())
                } else {
                    Err(EngineError::Keyring(KeyringError::Locked))
                };
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
            Command::Settings(reply) => answer(reply, Ok(self.settings.clone())),
            Command::SetSettings(settings, reply) => {
                let result = self
                    .store
                    .save_settings(&settings)
                    .map_err(EngineError::from);
                if result.is_ok() {
                    let testnets_changed = settings.show_testnets != self.settings.show_testnets;
                    self.settings = settings.clone();
                    let _ = self.events.send(Event::SettingsChanged(settings));
                    self.activity();
                    self.refresh_prices();
                    self.publish_portfolio();
                    if testnets_changed {
                        self.restart_refresh();
                    }
                }
                let _ = reply.send(result);
            }
        }
    }

    fn notice(&mut self, notice: Notice) {
        match notice {
            Notice::Fetched(fetched) => self.apply(fetched),
            Notice::Quoted(quoted) => self.quoted(*quoted),
            Notice::Delivered(delivered) => self.delivered(*delivered),
            Notice::Tracked(updates) => self.tracked(updates),
        }
    }

    async fn keyring<T: Send + 'static, E: Into<EngineError> + Send + 'static>(
        &mut self,
        work: impl FnOnce(&mut Keyring) -> Result<T, E> + Send + 'static,
    ) -> Result<T, EngineError> {
        let mut keyring = self.keyring.take().ok_or(EngineError::Stopped)?;
        let (keyring, result) = tokio::task::spawn_blocking(move || {
            let result = work(&mut keyring);
            (keyring, result)
        })
        .await
        .map_err(|_| EngineError::Stopped)?;
        self.keyring = Some(keyring);
        result.map_err(Into::into)
    }

    fn unlocked(&self) -> bool {
        self.keyring.as_ref().is_some_and(Keyring::is_unlocked)
    }

    async fn opened(&mut self, result: Result<(), EngineError>, reply: Reply<()>) {
        if result.is_ok() {
            self.after_unlock().await;
        }
        let _ = reply.send(result);
    }

    async fn after_unlock(&mut self) {
        self.set_state(State::Unlocked);
        self.activity();
        self.derive_accounts().await;
        self.start_tracking();
    }

    async fn after_change(&mut self, ok: bool) {
        if !ok {
            return;
        }
        if let Ok(wallets) = self.keyring(|keyring| keyring.wallets()).await {
            let _ = self.events.send(Event::WalletsChanged(wallets));
        }
        self.derive_accounts().await;
    }

    async fn derive_accounts(&mut self) {
        let chains = Arc::clone(&self.chains);
        let derived = self
            .keyring(move |keyring| {
                let families = chains::per_family(&chains);
                let mut accounts = Vec::new();
                for wallet in keyring.wallets()? {
                    let seed = keyring.seed(wallet.id)?;
                    for chain in &families {
                        accounts.push(Account {
                            wallet: wallet.id,
                            family: chain.family(),
                            index: 0,
                            address: chain.derive(seed, 0)?,
                        });
                    }
                }
                Ok::<_, EngineError>(accounts)
            })
            .await;
        if let Ok(accounts) = derived {
            let wallets: Vec<WalletId> = accounts.iter().map(|account| account.wallet).collect();
            self.held.retain(|held| wallets.contains(&held.wallet));
            self.accounts = accounts;
            let _ = self
                .events
                .send(Event::AccountsChanged(self.accounts.clone()));
            self.publish_portfolio();
            self.refresh_prices();
            self.restart_refresh();
        }
    }

    fn start_refresh(&mut self) {
        if !self.unlocked() || self.refreshing.is_some() {
            return;
        }
        self.next_refresh = None;
        let chains = Arc::clone(&self.chains);
        let accounts = self.accounts.clone();
        let custom = Arc::clone(&self.custom);
        let testnets = self.settings.show_testnets;
        let cursors = self.scans.clone();
        let generation = self.generation;
        let notices = self.notices_tx.clone();
        self.refreshing = Some(tokio::spawn(async move {
            let fetched =
                chains::fetch(&chains, &accounts, &custom, &cursors, testnets, generation).await;
            let _ = notices.send(Notice::Fetched(fetched)).await;
        }));
    }

    fn stop_refresh(&mut self) {
        if let Some(task) = self.refreshing.take() {
            task.abort();
        }
        self.generation = self.generation.wrapping_add(1);
        self.next_refresh = None;
    }

    fn restart_refresh(&mut self) {
        self.stop_refresh();
        self.start_refresh();
    }

    fn apply(&mut self, fetched: Fetched) {
        if fetched.generation != self.generation {
            return;
        }
        self.refreshing = None;
        self.held = fetched.held;
        self.health = fetched.health;
        self.record_scans(fetched.scans);
        self.next_refresh = Some(Instant::now() + self.refresh_every);
        self.refresh_prices();
        self.publish_portfolio();
    }

    fn lock(&mut self) {
        if let Some(keyring) = self.keyring.as_mut() {
            keyring.lock();
        }
        self.deadline = None;
        self.stop_refresh();
        self.stop_tracking();
        self.drafts.clear();
        self.session = self.session.wrapping_add(1);
        self.accounts.clear();
        self.held.clear();
        self.health.clear();
        let state = if self.keyring.as_ref().is_some_and(Keyring::exists) {
            State::Locked
        } else {
            State::NoVault
        };
        self.set_state(state);
        self.refresh_prices();
        self.publish_portfolio();
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

    fn publish_portfolio(&self) {
        let snapshot = self.prices.borrow().clone();
        let portfolio = portfolio::build(
            &self.registry,
            &self.custom,
            &self.held,
            self.health.clone(),
            &snapshot,
            self.settings.fiat,
        );
        self.portfolio.send_replace(portfolio);
    }

    fn refresh_prices(&self) {
        let Some(market) = &self.market else {
            return;
        };
        let wanted = if self.unlocked() {
            portfolio::wanted(&self.registry, &self.custom, &self.held)
        } else {
            BTreeSet::new()
        };
        market.want(Request {
            wanted,
            base: self.settings.fiat.price_base().to_owned(),
        });
    }
}
