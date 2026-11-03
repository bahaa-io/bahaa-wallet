mod accounts;
mod activity;
mod contacts;
mod incoming;
mod prices;
mod send;
mod swap;
mod tokens;

use std::collections::{BTreeMap, BTreeSet};
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bahaa_chain::{ChainError, Replace};
use bahaa_config::{ConfigError, Registry, Settings};
use bahaa_crypto::{ExposeSecret, SecretString};
use bahaa_market::{Market, MarketConfig, MarketError, Request, Snapshot};
use bahaa_store::{
    Contact, CustomToken, Outgoing, Received, Scan, SentTx, Store, StoreError, StoreKey,
    SwapRecord, TrustedBlock,
};
use bahaa_swap::{Providers, SwapError};
use bahaa_types::{Address, AssetId, BitcoinType, ChainFamily, NetworkId, SolanaPath, TonContract};
use bahaa_vault::KdfParams;
use futures_util::future::{join, join_all};
use thiserror::Error;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep_until};

use crate::caution::{Arrival, Caution};
use crate::chains::{self, AnyChain, NetworkBalances, NetworkHistory};
use crate::keyring;
use crate::portfolio::{self, Account, Held, NetworkHealth, Portfolio};
use crate::{Keyring, KeyringError, NewPhrase, PhraseKind, WalletId, WalletInfo, WalletKind};

pub use activity::{Activity, ActivityKind};
pub use incoming::IncomingHistory;
use send::{Delivered, Prepared, Quoted};
pub use send::{Draft, DraftId, SendRequest, Spend};
use swap::{PreparedSwap, SwapOrdered, SwapQuoted, Swappers};
pub use swap::{SwapDraft, SwapId, SwapRequest, SwapReview, Unavailable};
use tokens::TokenRead;

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
    pub swaps: Option<Providers>,
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
            swaps: Providers::builtin().ok(),
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
    #[error(transparent)]
    Swap(#[from] SwapError),
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
    #[error("this network is not set up in the wallet")]
    UnknownNetwork,
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
    SwapUpdated(Box<SwapRecord>),
    Received(Vec<Arrival>),
    ContactsChanged(Vec<Contact>),
    SentElsewhere(Vec<Outgoing>),
    TokensChanged(Vec<CustomToken>),
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
    Generate(NewPhrase, SecretString, Reply<(WalletId, SecretString)>),
    Import(
        SecretString,
        SecretString,
        Option<PhraseKind>,
        Reply<WalletId>,
    ),
    ImportKey(SecretString, ChainFamily, Reply<WalletId>),
    Watch(Address, bool, Reply<WalletId>),
    SetCounted(WalletId, bool, Reply<()>),
    Remove(WalletId, Reply<()>),
    AddAccount(WalletId, Reply<Vec<Account>>),
    RemoveAccount(WalletId, u32, Reply<()>),
    Choose(WalletId, Choice, Reply<()>),
    ReceiveAddress(NetworkId, Address, Reply<Address>),
    Reveal(WalletId, SecretString, Reply<SecretString>),
    ChangePassword(SecretString, SecretString, Reply<()>),
    CheckPassword(SecretString, Reply<()>),
    RenameWallet(WalletId, String, Reply<()>),
    Settings(Reply<Settings>),
    SetSettings(Settings, Reply<()>),
    PrepareSend(SendRequest, Reply<Draft>),
    Replace(NetworkId, String, Replace, Reply<Draft>),
    ConfirmSend(DraftId, SecretString, Reply<SentTx>),
    ConfirmOrder(DraftId, SecretString, Reply<SwapRecord>),
    PrepareSwap(SwapRequest, Reply<SwapDraft>),
    ChooseSwap(SwapId, usize, Reply<SwapReview>),
    Swaps(Reply<Vec<SwapRecord>>),
    History(Reply<Vec<SentTx>>),
    Incoming(Reply<IncomingHistory>),
    Outgoing(Reply<Vec<Outgoing>>),
    Activity(Reply<Vec<Activity>>),
    Contacts(Reply<Vec<Contact>>),
    SaveContact(Contact, Reply<()>),
    RemoveContact(NetworkId, Address, Reply<bool>),
    Cautions(Address, Reply<Vec<Caution>>),
    Tokens(Reply<Vec<CustomToken>>),
    AddToken(AssetId, Reply<CustomToken>),
    RemoveToken(AssetId, Reply<bool>),
    Shutdown(Reply<()>),
}

enum Notice {
    Balances(u64, Box<NetworkBalances>),
    History(u64, Box<NetworkHistory>),
    Refreshed(u64),
    Quoted(Box<Quoted>),
    Delivered(Box<Delivered>),
    Tracked(Vec<SentTx>),
    SwapQuoted(Box<SwapQuoted>),
    SwapOrdered(Box<SwapOrdered>),
    Polled(Vec<SwapRecord>),
    TokenRead(Box<TokenRead>),
    Detected(u64, Vec<(WalletId, Choice)>),
}

#[derive(Clone, Copy)]
enum Choice {
    Ton(TonContract),
    Solana(SolanaPath),
    Bitcoin(BitcoinType),
}

impl Choice {
    fn apply(self, keyring: &mut Keyring, id: WalletId) -> Result<(), KeyringError> {
        match self {
            Self::Ton(contract) => keyring.set_ton_contract(id, contract),
            Self::Solana(path) => keyring.set_solana_path(id, path),
            Self::Bitcoin(kind) => keyring.set_bitcoin_type(id, kind),
        }
    }
}

enum Choices {
    Solana(Address, Address),
    Bitcoin(Address, Address),
}

impl Command {
    fn is_activity(&self) -> bool {
        !matches!(
            self,
            Self::Wallets(_)
                | Self::Accounts(_)
                | Self::Settings(_)
                | Self::History(_)
                | Self::Swaps(_)
                | Self::Incoming(_)
                | Self::Outgoing(_)
                | Self::Activity(_)
                | Self::Contacts(_)
                | Self::Cautions(..)
                | Self::Tokens(_)
                | Self::ReceiveAddress(..)
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
    registry: Arc<Registry>,
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
            swaps,
        } = config;
        crate::keyring::private_dir(&data_dir)?;
        let store = Store::open(&data_dir.join(STORE_FILE))?;
        let settings = store.settings()?;
        let saved = prices::empty(settings.fiat.price_base());
        let registry = Arc::new(match registry {
            Some(registry) => registry,
            None => Registry::builtin()?,
        });
        let chains: Arc<Vec<AnyChain>> = Arc::new(chains::build(&registry));
        chains::restore(&chains, &store.trusted()?);
        let trusted = chains::trusted(&chains);
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
            registry: Arc::clone(&registry),
            chains,
            custom: Arc::new(Vec::new()),
            settings,
            market,
            prices: market_prices(&prices),
            saved,
            saved_at: None,
            unsaved: false,
            inbox,
            events: events.clone(),
            state: publish_state,
            portfolio: publish_portfolio,
            notices_tx,
            notices,
            accounts: Vec::new(),
            uncounted: Vec::new(),
            held: Vec::new(),
            health: Vec::new(),
            generation: 0,
            refreshing: None,
            scanning: BTreeMap::new(),
            loaded: BTreeSet::new(),
            scanned: BTreeSet::new(),
            deadline: None,
            next_refresh: None,
            unit: auto_lock_unit,
            refresh_every,
            session: 0,
            drafts: BTreeMap::new(),
            next_draft: 0,
            history: Vec::new(),
            scans: Vec::new(),
            received: Vec::new(),
            outgoing: Vec::new(),
            trusted,
            broadcasting: BTreeSet::new(),
            tracking: None,
            next_track: None,
            draft_ttl,
            track_every,
            swappers: swaps.as_ref().map(Swappers::new).transpose()?.map(Arc::new),
            swap_drafts: BTreeMap::new(),
            next_swap: 0,
            swaps: Vec::new(),
            polling: None,
            next_poll: None,
        };
        tokio::spawn(worker.run());
        Ok(Self {
            commands,
            events,
            state,
            prices,
            portfolio,
            registry,
        })
    }

    pub fn registry(&self) -> Arc<Registry> {
        Arc::clone(&self.registry)
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
        words: NewPhrase,
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
        self.ask(|reply| Command::Import(phrase, passphrase, None, reply))
            .await
    }

    pub async fn import_as(
        &self,
        phrase: SecretString,
        passphrase: SecretString,
        kind: PhraseKind,
    ) -> Result<WalletId, EngineError> {
        self.ask(|reply| Command::Import(phrase, passphrase, Some(kind), reply))
            .await
    }

    pub async fn import_key(
        &self,
        key: SecretString,
        family: ChainFamily,
    ) -> Result<WalletId, EngineError> {
        self.ask(|reply| Command::ImportKey(key, family, reply))
            .await
    }

    pub async fn watch(&self, address: Address, counted: bool) -> Result<WalletId, EngineError> {
        self.ask(|reply| Command::Watch(address, counted, reply))
            .await
    }

    pub async fn set_counted(&self, wallet: WalletId, counted: bool) -> Result<(), EngineError> {
        self.ask(|reply| Command::SetCounted(wallet, counted, reply))
            .await
    }

    pub async fn remove(&self, id: WalletId) -> Result<(), EngineError> {
        self.ask(|reply| Command::Remove(id, reply)).await
    }

    pub async fn add_account(&self, wallet: WalletId) -> Result<Vec<Account>, EngineError> {
        self.ask(|reply| Command::AddAccount(wallet, reply)).await
    }

    pub async fn remove_account(&self, wallet: WalletId, index: u32) -> Result<(), EngineError> {
        self.ask(|reply| Command::RemoveAccount(wallet, index, reply))
            .await
    }

    pub async fn set_ton_contract(
        &self,
        wallet: WalletId,
        contract: TonContract,
    ) -> Result<(), EngineError> {
        self.ask(|reply| Command::Choose(wallet, Choice::Ton(contract), reply))
            .await
    }

    pub async fn set_solana_path(
        &self,
        wallet: WalletId,
        path: SolanaPath,
    ) -> Result<(), EngineError> {
        self.ask(|reply| Command::Choose(wallet, Choice::Solana(path), reply))
            .await
    }

    pub async fn set_bitcoin_type(
        &self,
        wallet: WalletId,
        kind: BitcoinType,
    ) -> Result<(), EngineError> {
        self.ask(|reply| Command::Choose(wallet, Choice::Bitcoin(kind), reply))
            .await
    }

    pub async fn receive_address(
        &self,
        network: NetworkId,
        address: Address,
    ) -> Result<Address, EngineError> {
        self.ask(|reply| Command::ReceiveAddress(network, address, reply))
            .await
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

    pub async fn check_password(&self, password: SecretString) -> Result<(), EngineError> {
        self.ask(|reply| Command::CheckPassword(password, reply))
            .await
    }

    pub async fn rename_wallet(&self, id: WalletId, name: String) -> Result<(), EngineError> {
        self.ask(|reply| Command::RenameWallet(id, name, reply))
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

    pub async fn replace(
        &self,
        network: NetworkId,
        id: String,
        how: Replace,
    ) -> Result<Draft, EngineError> {
        self.ask(|reply| Command::Replace(network, id, how, reply))
            .await
    }

    pub async fn prepare_swap(&self, request: SwapRequest) -> Result<SwapDraft, EngineError> {
        self.ask(|reply| Command::PrepareSwap(request, reply)).await
    }

    pub async fn choose_swap(&self, id: SwapId, route: usize) -> Result<SwapReview, EngineError> {
        self.ask(|reply| Command::ChooseSwap(id, route, reply))
            .await
    }

    pub async fn swaps(&self) -> Result<Vec<SwapRecord>, EngineError> {
        self.ask(Command::Swaps).await
    }

    pub async fn confirm_order(
        &self,
        draft: DraftId,
        password: SecretString,
    ) -> Result<SwapRecord, EngineError> {
        self.ask(|reply| Command::ConfirmOrder(draft, password, reply))
            .await
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

    pub async fn outgoing(&self) -> Result<Vec<Outgoing>, EngineError> {
        self.ask(Command::Outgoing).await
    }

    pub async fn activity(&self) -> Result<Vec<Activity>, EngineError> {
        self.ask(Command::Activity).await
    }

    pub async fn contacts(&self) -> Result<Vec<Contact>, EngineError> {
        self.ask(Command::Contacts).await
    }

    pub async fn save_contact(&self, contact: Contact) -> Result<(), EngineError> {
        self.ask(|reply| Command::SaveContact(contact, reply)).await
    }

    pub async fn remove_contact(
        &self,
        network: NetworkId,
        address: Address,
    ) -> Result<bool, EngineError> {
        self.ask(|reply| Command::RemoveContact(network, address, reply))
            .await
    }

    pub async fn cautions(&self, address: Address) -> Result<Vec<Caution>, EngineError> {
        self.ask(|reply| Command::Cautions(address, reply)).await
    }

    pub async fn tokens(&self) -> Result<Vec<CustomToken>, EngineError> {
        self.ask(Command::Tokens).await
    }

    pub async fn add_token(&self, asset: AssetId) -> Result<CustomToken, EngineError> {
        self.ask(|reply| Command::AddToken(asset, reply)).await
    }

    pub async fn remove_token(&self, asset: AssetId) -> Result<bool, EngineError> {
        self.ask(|reply| Command::RemoveToken(asset, reply)).await
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
    registry: Arc<Registry>,
    chains: Arc<Vec<AnyChain>>,
    custom: Arc<Vec<CustomToken>>,
    settings: Settings,
    market: Option<Market>,
    prices: watch::Receiver<Snapshot>,
    saved: Snapshot,
    saved_at: Option<Instant>,
    unsaved: bool,
    inbox: mpsc::Receiver<Command>,
    events: broadcast::Sender<Event>,
    state: watch::Sender<State>,
    portfolio: watch::Sender<Portfolio>,
    notices_tx: mpsc::Sender<Notice>,
    notices: mpsc::Receiver<Notice>,
    accounts: Vec<Account>,
    uncounted: Vec<WalletId>,
    held: Vec<Held>,
    health: Vec<NetworkHealth>,
    generation: u64,
    refreshing: Option<JoinHandle<()>>,
    scanning: BTreeMap<NetworkId, JoinHandle<()>>,
    loaded: BTreeSet<NetworkId>,
    scanned: BTreeSet<NetworkId>,
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
    outgoing: Vec<Outgoing>,
    trusted: Vec<TrustedBlock>,
    broadcasting: BTreeSet<String>,
    tracking: Option<JoinHandle<()>>,
    next_track: Option<Instant>,
    draft_ttl: Duration,
    track_every: Duration,
    swappers: Option<Arc<Swappers>>,
    swap_drafts: BTreeMap<SwapId, PreparedSwap>,
    next_swap: u64,
    swaps: Vec<SwapRecord>,
    polling: Option<JoinHandle<()>>,
    next_poll: Option<Instant>,
}

struct TonCheck {
    w5: Address,
    v4r2: Address,
    ton_only: bool,
}

fn ton_check(
    keyring: &Keyring,
    chains: &[AnyChain],
    id: WalletId,
) -> Result<Option<TonCheck>, EngineError> {
    let Some(chain) = chains::ton_mainnet(chains) else {
        return Ok(None);
    };
    let wallets = keyring.wallets()?;
    let Some(wallet) = wallets
        .iter()
        .find(|wallet| wallet.id == id && wallet.ton.is_some())
    else {
        return Ok(None);
    };
    let seed = keyring.seed(id)?;
    Ok(Some(TonCheck {
        w5: chain.derive_as(seed, 0, TonContract::W5)?,
        v4r2: chain.derive_as(seed, 0, TonContract::V4R2)?,
        ton_only: wallet.kind != WalletKind::Bip39,
    }))
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
            let next_poll = self.next_poll;
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
                () = at(next_poll) => self.start_polling(),
                Some(notice) = self.notices.recv() => self.notice(notice).await,
                Ok(()) = self.prices.changed(), if has_market => {
                    self.remember_prices();
                    self.publish_portfolio();
                }
            }
        };
        self.keep_prices();
        drop(self);
        if let Some(reply) = shutdown {
            let _ = reply.send(Ok(()));
        }
    }

    async fn handle(&mut self, command: Command) {
        match command {
            Command::Create(password, reply) => {
                let result = self
                    .keyring(move |keyring| {
                        keyring.create(password.expose_secret().as_bytes())?;
                        keyring.store_key()
                    })
                    .await;
                self.opened(result, reply).await;
            }
            Command::Unlock(password, reply) => {
                let result = self
                    .keyring(move |keyring| {
                        keyring.unlock(password.expose_secret().as_bytes())?;
                        keyring.store_key()
                    })
                    .await;
                self.opened(result, reply).await;
            }
            Command::Lock(reply) => {
                self.lock();
                let _ = reply.send(Ok(()));
            }
            Command::Refresh => self.start_refresh(),
            Command::PrepareSend(request, reply) => self.prepare_send(&request, reply),
            Command::Replace(network, id, how, reply) => {
                self.replace_send(&network, &id, how, reply);
            }
            Command::ConfirmOrder(id, password, reply) => {
                self.confirm_order(id, password, reply).await;
            }
            Command::ConfirmSend(id, password, reply) => {
                self.confirm_send(id, password, reply).await;
            }
            Command::PrepareSwap(request, reply) => self.prepare_swap(&request, reply),
            Command::ChooseSwap(id, route, reply) => self.choose_swap(id, route, reply),
            Command::Swaps(reply) => answer(reply, self.swaps()),
            Command::History(reply) => answer(reply, self.history()),
            Command::Incoming(reply) => answer(reply, self.incoming()),
            Command::Outgoing(reply) => answer(reply, self.outgoing()),
            Command::Activity(reply) => answer(reply, self.activities()),
            Command::Contacts(reply) => answer(reply, self.contacts()),
            Command::SaveContact(contact, reply) => answer(reply, self.save_contact(&contact)),
            Command::RemoveContact(network, address, reply) => {
                answer(reply, self.remove_contact(&network, &address));
            }
            Command::Cautions(address, reply) => answer(reply, self.cautions(&address)),
            Command::Tokens(reply) => answer(reply, self.tokens()),
            Command::AddToken(asset, reply) => self.add_token(asset, reply),
            Command::RemoveToken(asset, reply) => answer(reply, self.remove_token(&asset)),
            Command::ReceiveAddress(network, address, reply) => {
                self.receive_address(&network, address, reply);
            }
            Command::Touch | Command::Shutdown(_) => {}
            command @ (Command::Wallets(_)
            | Command::Accounts(_)
            | Command::Generate(..)
            | Command::Import(..)
            | Command::ImportKey(..)
            | Command::Watch(..)
            | Command::SetCounted(..)
            | Command::Remove(..)
            | Command::AddAccount(..)
            | Command::RemoveAccount(..)
            | Command::Choose(..)
            | Command::Reveal(..)
            | Command::ChangePassword(..)
            | Command::CheckPassword(..)
            | Command::RenameWallet(..)) => self.handle_wallets(command).await,
            Command::Settings(reply) => answer(reply, Ok(self.settings.clone())),
            Command::SetSettings(settings, reply) => answer(reply, self.set_settings(settings)),
        }
    }

    async fn handle_wallets(&mut self, command: Command) {
        match command {
            Command::Wallets(reply) => {
                let result = self.wallets().await;
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
            Command::Generate(words, passphrase, reply) => answer(
                reply,
                self.changed(move |keyring| keyring.generate(words, passphrase.expose_secret()))
                    .await,
            ),
            Command::Import(phrase, passphrase, kind, reply) => {
                self.import(phrase, passphrase, kind, reply).await;
            }
            Command::ImportKey(key, family, reply) => {
                let chains = Arc::clone(&self.chains);
                let added = self
                    .keyring(move |keyring| {
                        let id = keyring.import_key(key.expose_secret(), family)?;
                        Ok::<_, EngineError>((id, ton_check(keyring, &chains, id)?))
                    })
                    .await;
                self.imported(added, reply).await;
            }
            Command::Watch(address, counted, reply) => {
                answer(reply, self.watch(address, counted).await);
            }
            Command::SetCounted(id, counted, reply) => answer(
                reply,
                self.changed(move |keyring| keyring.set_counted(id, counted))
                    .await,
            ),
            Command::Remove(id, reply) => {
                let result = self.keyring(move |keyring| keyring.remove(id)).await;
                if result.is_ok() {
                    let _ = self.store.forget_account_indexes(id.value());
                    let _ = self.store.forget_wallet_name(id.value());
                }
                self.after_change(result.is_ok()).await;
                let _ = reply.send(result);
            }
            Command::AddAccount(wallet, reply) => {
                let result = self.add_account(wallet).await;
                let _ = reply.send(result);
            }
            Command::RemoveAccount(wallet, index, reply) => {
                let result = self.remove_account(wallet, index).await;
                let _ = reply.send(result);
            }
            Command::Choose(id, choice, reply) => answer(
                reply,
                self.changed(move |keyring| choice.apply(keyring, id)).await,
            ),
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
            Command::CheckPassword(password, reply) => {
                let result = self
                    .keyring(move |keyring| {
                        keyring.check_password(password.expose_secret().as_bytes())
                    })
                    .await;
                let _ = reply.send(result);
            }
            Command::RenameWallet(id, name, reply) => {
                let result = self.rename_wallet(id, &name).await;
                self.after_change(result.is_ok()).await;
                let _ = reply.send(result);
            }
            _ => {}
        }
    }

    async fn wallets(&mut self) -> Result<Vec<WalletInfo>, EngineError> {
        let mut wallets = self.keyring(|keyring| keyring.wallets()).await?;
        for wallet in &mut wallets {
            wallet.name = self.store.wallet_name(wallet.id.value())?;
        }
        Ok(wallets)
    }

    async fn rename_wallet(&mut self, id: WalletId, name: &str) -> Result<(), EngineError> {
        if !self.wallets().await?.iter().any(|wallet| wallet.id == id) {
            return Err(KeyringError::UnknownWallet.into());
        }
        self.store.save_wallet_name(id.value(), name)?;
        Ok(())
    }

    fn set_settings(&mut self, settings: Settings) -> Result<(), EngineError> {
        self.store.save_settings(&settings)?;
        let testnets_changed = settings.show_testnets != self.settings.show_testnets;
        self.settings = settings.clone();
        self.switch_prices();
        let _ = self.events.send(Event::SettingsChanged(settings));
        self.activity();
        self.refresh_prices();
        self.publish_portfolio();
        if testnets_changed {
            self.restart_refresh();
        }
        Ok(())
    }

    async fn notice(&mut self, notice: Notice) {
        match notice {
            Notice::Balances(generation, update) if generation == self.generation => {
                self.balances_arrived(*update);
            }
            Notice::History(generation, update) if generation == self.generation => {
                self.history_arrived(*update);
            }
            Notice::Refreshed(generation) if generation == self.generation => self.refreshed(),
            Notice::Quoted(quoted) => self.quoted(*quoted),
            Notice::Delivered(delivered) => self.delivered(*delivered),
            Notice::Tracked(updates) => self.tracked(updates),
            Notice::SwapQuoted(quoted) => self.swap_quoted(*quoted),
            Notice::SwapOrdered(ordered) => self.swap_ordered(*ordered),
            Notice::Polled(updates) => self.polled(updates),
            Notice::TokenRead(read) => self.token_read(*read),
            Notice::Detected(session, found) if session == self.session && !found.is_empty() => {
                let _ = self
                    .changed(move |keyring| {
                        found
                            .into_iter()
                            .try_for_each(|(id, choice)| choice.apply(keyring, id))
                    })
                    .await;
            }
            Notice::Balances(..)
            | Notice::History(..)
            | Notice::Refreshed(_)
            | Notice::Detected(..) => {}
        }
    }

    fn receive_address(&self, network: &NetworkId, address: Address, reply: Reply<Address>) {
        let found = self.ensure_unlocked().and_then(|()| {
            let chain = self
                .chain_index(network)
                .ok_or(EngineError::UnknownNetwork)?;
            let family = self.chains[chain].family();
            if !self
                .accounts
                .iter()
                .any(|account| account.address == address && account.serves(network, family))
            {
                return Err(ChainError::Invalid(
                    "this address is not one of this wallet's accounts",
                )
                .into());
            }
            Ok(chain)
        });
        let chain = match found {
            Ok(chain) => chain,
            Err(err) => {
                let _ = reply.send(Err(err));
                return;
            }
        };
        let chains = Arc::clone(&self.chains);
        tokio::spawn(async move {
            let result = chains[chain].receive_address(&address).await;
            let _ = reply.send(result.map_err(EngineError::from));
        });
    }

    async fn detect_paths(&mut self) {
        let chains = Arc::clone(&self.chains);
        if chains::solana_mainnet(&chains).is_none() && chains::bitcoin_mainnet(&chains).is_none() {
            return;
        }
        let shared = Arc::clone(&chains);
        let candidates = self
            .keyring(move |keyring| {
                let mut candidates = Vec::new();
                if chains::solana_mainnet(&shared).is_some() {
                    let [standard, short] = SolanaPath::ALL;
                    for id in keyring.undetected(ChainFamily::Solana)? {
                        let seed = keyring.seed(id)?;
                        candidates.push((
                            id,
                            Choices::Solana(
                                bahaa_solana::derive(seed, 0, standard)?,
                                bahaa_solana::derive(seed, 0, short)?,
                            ),
                        ));
                    }
                }
                if let Some(chain) = chains::bitcoin_mainnet(&shared) {
                    let [segwit, taproot] = BitcoinType::ALL;
                    for id in keyring.undetected(ChainFamily::Bitcoin)? {
                        let seed = keyring.seed(id)?;
                        candidates.push((
                            id,
                            Choices::Bitcoin(
                                chain.first(seed, 0, segwit)?,
                                chain.first(seed, 0, taproot)?,
                            ),
                        ));
                    }
                }
                Ok::<_, EngineError>(candidates)
            })
            .await;
        let Some(candidates) = candidates.ok().filter(|candidates| !candidates.is_empty()) else {
            return;
        };
        let (session, notices) = (self.session, self.notices_tx.clone());
        tokio::spawn(async move {
            let mut found = Vec::new();
            for (id, choices) in candidates {
                let choice = match (
                    choices,
                    chains::solana_mainnet(&chains),
                    chains::bitcoin_mainnet(&chains),
                ) {
                    (Choices::Solana(standard, short), Some(chain), _) => {
                        match join(chain.last_used(&standard), chain.last_used(&short)).await {
                            (Ok(standard), Ok(short)) => {
                                Some(Choice::Solana(keyring::most_recent(
                                    (SolanaPath::Bip44Change, standard),
                                    (SolanaPath::Bip44, short),
                                )))
                            }
                            _ => None,
                        }
                    }
                    (Choices::Bitcoin(segwit, taproot), _, Some(chain)) => {
                        match join(chain.last_used(&segwit), chain.last_used(&taproot)).await {
                            (Ok(segwit), Ok(taproot)) => {
                                Some(Choice::Bitcoin(keyring::most_recent(
                                    (BitcoinType::NativeSegwit, segwit),
                                    (BitcoinType::Taproot, taproot),
                                )))
                            }
                            _ => None,
                        }
                    }
                    _ => None,
                };
                found.extend(choice.map(|choice| (id, choice)));
            }
            let _ = notices.send(Notice::Detected(session, found)).await;
        });
    }

    async fn keyring<T: Send + 'static, E: Into<EngineError> + Send + 'static>(
        &mut self,
        work: impl FnOnce(&mut Keyring) -> Result<T, E> + Send + 'static,
    ) -> Result<T, EngineError> {
        let mut keyring = self.keyring.take().ok_or(EngineError::Stopped)?;
        let (keyring, result) = tokio::task::spawn_blocking(move || {
            let result = std::panic::catch_unwind(AssertUnwindSafe(|| work(&mut keyring)));
            if result.is_err() {
                keyring.lock();
            }
            (keyring, result)
        })
        .await
        .map_err(|_| EngineError::Stopped)?;
        self.keyring = Some(keyring);
        if let Ok(result) = result {
            result.map_err(Into::into)
        } else {
            self.lock();
            Err(KeyringError::Locked.into())
        }
    }

    fn unlocked(&self) -> bool {
        self.keyring.as_ref().is_some_and(Keyring::is_unlocked)
    }

    fn owner(&self, address: &Address) -> Option<WalletId> {
        self.accounts
            .iter()
            .find(|account| account.address == *address)
            .map(|account| account.wallet)
    }

    fn ensure_unlocked(&self) -> Result<(), EngineError> {
        if self.unlocked() {
            Ok(())
        } else {
            Err(KeyringError::Locked.into())
        }
    }

    async fn opened(&mut self, result: Result<StoreKey, EngineError>, reply: Reply<()>) {
        let result = result.and_then(|key| self.open_store(key));
        match result {
            Ok(()) => self.after_unlock().await,
            Err(_) => self.lock(),
        }
        let _ = reply.send(result);
    }

    fn open_store(&mut self, key: StoreKey) -> Result<(), EngineError> {
        match self.store.unlock(key) {
            Err(StoreError::OtherVault) => {
                let keyring = self.keyring.as_ref().ok_or(EngineError::Stopped)?;
                self.store.adopt(keyring.store_key()?)?;
            }
            opened => opened?,
        }
        let custom = self.store.tokens()?;
        let mut history = self.store.sent()?;
        history.sort_by_key(|tx| std::cmp::Reverse(tx.created_at));
        let scans = self.store.scans()?;
        let mut received = self.store.received()?;
        incoming::sort(&mut received);
        let mut outgoing = self.store.outgoing()?;
        incoming::sort(&mut outgoing);
        let mut swaps = self.store.swaps()?;
        swaps.sort_by_key(|swap| std::cmp::Reverse(swap.created_at));
        chains::know(&self.chains, &custom);
        self.custom = Arc::new(custom);
        self.history = history;
        self.scans = scans;
        self.received = received;
        self.outgoing = outgoing;
        self.swaps = swaps;
        self.saved = prices::saved(&self.store, self.settings.fiat.price_base());
        self.saved_at = None;
        self.unsaved = false;
        self.remember_prices();
        Ok(())
    }

    fn forget_private(&mut self) {
        self.store.lock();
        self.custom = Arc::new(Vec::new());
        self.history.clear();
        self.scans.clear();
        self.received.clear();
        self.outgoing.clear();
        self.swaps.clear();
        self.saved = prices::empty(self.settings.fiat.price_base());
        self.saved_at = None;
        self.unsaved = false;
    }

    async fn after_unlock(&mut self) {
        self.set_state(State::Unlocked);
        self.activity();
        self.derive_accounts().await;
        self.start_tracking();
        self.start_polling();
        self.detect_paths().await;
    }

    async fn import(
        &mut self,
        phrase: SecretString,
        passphrase: SecretString,
        kind: Option<PhraseKind>,
        reply: Reply<WalletId>,
    ) {
        let chains = Arc::clone(&self.chains);
        let added = self
            .keyring(move |keyring| {
                let (phrase, passphrase) = (phrase.expose_secret(), passphrase.expose_secret());
                let id = match kind {
                    Some(kind) => keyring.import_as(phrase, passphrase, kind),
                    None => keyring.import(phrase, passphrase),
                }?;
                Ok::<_, EngineError>((id, ton_check(keyring, &chains, id)?))
            })
            .await;
        self.imported(added, reply).await;
    }

    async fn imported(
        &mut self,
        added: Result<(WalletId, Option<TonCheck>), EngineError>,
        reply: Reply<WalletId>,
    ) {
        let result = match added {
            Ok((id, Some(check))) => self.settle_contract(id, check).await,
            Ok((id, None)) => Ok(id),
            Err(err) => Err(err),
        };
        self.after_change(result.is_ok()).await;
        if result.is_ok() {
            self.detect_paths().await;
        }
        let _ = reply.send(result);
    }

    async fn settle_contract(
        &mut self,
        id: WalletId,
        check: TonCheck,
    ) -> Result<WalletId, EngineError> {
        let chains = Arc::clone(&self.chains);
        let used = match chains::ton_mainnet(&chains) {
            Some(chain) => {
                match join(chain.last_used(&check.w5), chain.last_used(&check.v4r2)).await {
                    (Ok(w5), Ok(v4r2)) => Ok(keyring::most_recent(
                        (TonContract::W5, w5),
                        (TonContract::V4R2, v4r2),
                    )),
                    (Err(err), _) | (_, Err(err)) => Err(err),
                }
            }
            None => Ok(TonContract::W5),
        };
        match used {
            Ok(TonContract::W5) => Ok(id),
            Ok(contract) => {
                self.keyring(move |keyring| keyring.set_ton_contract(id, contract))
                    .await?;
                Ok(id)
            }
            Err(_) if !check.ton_only => Ok(id),
            Err(err) => {
                let _ = self.keyring(move |keyring| keyring.remove(id)).await;
                Err(EngineError::Chain(err))
            }
        }
    }

    async fn watch(&mut self, address: Address, counted: bool) -> Result<WalletId, EngineError> {
        if self.owner(&address).is_some() {
            return Err(KeyringError::Duplicate.into());
        }
        self.changed(move |keyring| keyring.watch(address, counted))
            .await
    }

    async fn changed<T: Send + 'static>(
        &mut self,
        work: impl FnOnce(&mut Keyring) -> Result<T, KeyringError> + Send + 'static,
    ) -> Result<T, EngineError> {
        let result = self.keyring(work).await;
        self.after_change(result.is_ok()).await;
        result
    }

    async fn after_change(&mut self, ok: bool) {
        if !ok {
            return;
        }
        if let Ok(wallets) = self.wallets().await {
            let _ = self.events.send(Event::WalletsChanged(wallets));
        }
        self.derive_accounts().await;
    }

    async fn derive_accounts(&mut self) {
        let chains = Arc::clone(&self.chains);
        let listed = self
            .keyring
            .as_ref()
            .map_or_else(|| Ok(Vec::new()), Keyring::wallets)
            .unwrap_or_default();
        let indexes: Vec<(WalletId, Vec<u32>)> = listed
            .iter()
            .map(|wallet| {
                let kept = match wallet.kind {
                    WalletKind::Ton | WalletKind::Key(_) | WalletKind::Watch(_) => vec![0],
                    WalletKind::Bip39 => self
                        .store
                        .account_indexes(wallet.id.value())
                        .unwrap_or_else(|_| vec![0]),
                };
                (wallet.id, kept)
            })
            .collect();
        let derived = self
            .keyring(move |keyring| {
                let mut accounts = Vec::new();
                for wallet in keyring.wallets()? {
                    let watched = keyring.watched(wallet.id)?;
                    let kept = indexes
                        .iter()
                        .find(|(id, _)| *id == wallet.id)
                        .map_or_else(|| vec![0], |(_, kept)| kept.clone());
                    for index in kept {
                        let mut derived = Vec::with_capacity(chains.len());
                        for chain in chains.iter().filter(|chain| chain.holds(wallet.kind)) {
                            let address = match watched {
                                Some(address) => address,
                                None => chain.derive(keyring.seed(wallet.id)?, index, &wallet)?,
                            };
                            derived.push((chain.family(), chain.network().id.clone(), address));
                        }
                        accounts.extend(chains::accounts(wallet.id, index, &derived));
                    }
                }
                Ok::<_, EngineError>(accounts)
            })
            .await;
        if let Ok(accounts) = derived {
            if accounts != self.accounts {
                self.loaded.clear();
                self.scanned.clear();
            }
            self.held.retain(|held| {
                accounts
                    .iter()
                    .any(|account| account.wallet == held.wallet && account.address == held.address)
            });
            self.accounts = accounts;
            self.uncounted = listed
                .iter()
                .filter(|wallet| !wallet.counted)
                .map(|wallet| wallet.id)
                .collect();
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
        let testnets = self.settings.show_testnets;
        let active: Vec<NetworkId> = self
            .chains
            .iter()
            .map(AnyChain::network)
            .filter(|network| testnets || !network.testnet)
            .map(|network| network.id.clone())
            .collect();
        self.held
            .retain(|held| active.contains(held.asset.network()));
        self.health
            .retain(|health| active.contains(&health.network));
        for network in &active {
            if !self.health.iter().any(|health| health.network == *network) {
                self.health.push(NetworkHealth::waiting(network.clone()));
            }
        }
        self.publish_portfolio();
        let chains = Arc::clone(&self.chains);
        let accounts = self.accounts.clone();
        let custom = Arc::clone(&self.custom);
        let generation = self.generation;
        let notices = self.notices_tx.clone();
        self.refreshing = Some(tokio::spawn(async move {
            let reads = chains
                .iter()
                .filter(|chain| active.contains(&chain.network().id))
                .map(|chain| {
                    let (accounts, custom, notices) = (&accounts, &custom, &notices);
                    async move {
                        let read = chains::balances(chain, accounts, custom).await;
                        let _ = notices
                            .send(Notice::Balances(generation, Box::new(read)))
                            .await;
                    }
                });
            join_all(reads).await;
            let _ = notices.send(Notice::Refreshed(generation)).await;
        }));
    }

    fn start_history(&mut self, network: &NetworkId) {
        if self.scanning.contains_key(network) {
            return;
        }
        let Some(index) = self.chain_index(network) else {
            return;
        };
        let chains = Arc::clone(&self.chains);
        let accounts = self.accounts.clone();
        let custom = Arc::clone(&self.custom);
        let cursors: Vec<Scan> = self
            .scans
            .iter()
            .filter(|scan| scan.network == *network)
            .cloned()
            .collect();
        let generation = self.generation;
        let notices = self.notices_tx.clone();
        let task = tokio::spawn(async move {
            let read = chains::history(&chains[index], &accounts, &custom, &cursors).await;
            let _ = notices
                .send(Notice::History(generation, Box::new(read)))
                .await;
        });
        self.scanning.insert(network.clone(), task);
    }

    fn stop_refresh(&mut self) {
        if let Some(task) = self.refreshing.take() {
            task.abort();
        }
        for (_, task) in std::mem::take(&mut self.scanning) {
            task.abort();
        }
        self.generation = self.generation.wrapping_add(1);
        self.next_refresh = None;
    }

    fn restart_refresh(&mut self) {
        self.stop_refresh();
        self.start_refresh();
    }

    fn balances_arrived(&mut self, update: NetworkBalances) {
        let network = update.network;
        let failed = update.problem.is_some();
        let accounts = &self.accounts;
        let kept: Vec<Held> = self
            .held
            .iter()
            .filter(|old| {
                old.asset.network() != &network
                    || (failed
                        && accounts.iter().any(|account| {
                            account.wallet == old.wallet && account.address == old.address
                        })
                        && !update
                            .held
                            .iter()
                            .any(|new| new.address == old.address && new.asset == old.asset))
            })
            .cloned()
            .collect();
        self.held = kept;
        self.held.extend(update.held);
        if !self.health.iter().any(|health| health.network == network) {
            self.health.push(NetworkHealth::waiting(network.clone()));
        }
        if let Some(health) = self
            .health
            .iter_mut()
            .find(|health| health.network == network)
        {
            health.problem = update.problem;
            health.checked_at = update.checked_at;
            if !failed {
                health.updated_at = Some(update.checked_at);
            }
        }
        self.loaded.insert(network.clone());
        self.refresh_prices();
        self.publish_portfolio();
        self.start_history(&network);
    }

    fn history_arrived(&mut self, update: NetworkHistory) {
        self.scanning.remove(&update.network);
        self.record_scans(update.scans);
        if let Some(health) = self
            .health
            .iter_mut()
            .find(|health| health.network == update.network)
        {
            health.history_problem = update.problem;
        }
        self.scanned.insert(update.network);
        self.record_trusted();
        self.publish_portfolio();
    }

    fn refreshed(&mut self) {
        self.refreshing = None;
        self.record_trusted();
        self.next_refresh = Some(Instant::now() + self.refresh_every);
    }

    fn record_trusted(&mut self) {
        for block in chains::trusted(&self.chains) {
            if !self.trusted.contains(&block) && self.store.save_trusted(&block).is_ok() {
                self.trusted.retain(|known| known.network != block.network);
                self.trusted.push(block);
            }
        }
    }

    fn lock(&mut self) {
        if let Some(keyring) = self.keyring.as_mut() {
            keyring.lock();
        }
        for chain in self.chains.iter() {
            chain.forget();
        }
        self.deadline = None;
        self.stop_refresh();
        self.stop_tracking();
        self.stop_polling();
        self.drafts.clear();
        self.session = self.session.wrapping_add(1);
        self.accounts.clear();
        self.uncounted.clear();
        self.held.clear();
        self.health.clear();
        self.loaded.clear();
        self.scanned.clear();
        self.keep_prices();
        self.forget_private();
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
        let snapshot = self.quotes();
        let health = self
            .health
            .iter()
            .map(|health| NetworkHealth {
                loading: !self.loaded.contains(&health.network),
                history_loading: !self.scanned.contains(&health.network),
                ..health.clone()
            })
            .collect();
        let portfolio = portfolio::build(
            &self.registry,
            &self.custom,
            &self.held,
            &self.uncounted,
            health,
            &snapshot,
            self.settings.fiat,
        );
        self.portfolio.send_if_modified(|current| {
            let changed = *current != portfolio;
            if changed {
                *current = portfolio;
            }
            changed
        });
    }

    fn refresh_prices(&self) {
        let Some(market) = &self.market else {
            return;
        };
        let wanted = if self.unlocked() {
            let received = self
                .received
                .iter()
                .filter(|entry| self.owner(&entry.to).is_some())
                .map(|entry| &entry.asset);
            portfolio::wanted(&self.registry, &self.custom, &self.held, received)
        } else {
            BTreeSet::new()
        };
        market.want(Request {
            wanted,
            base: self.settings.fiat.price_base().to_owned(),
        });
    }
}
