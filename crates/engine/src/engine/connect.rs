use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bahaa_chain::{ChainError, FeeQuote, now};
use bahaa_config::ChainKind;
use bahaa_connect::{
    Bridge, Bridges, Call, Code, ConnectError, ConnectItem, ConnectRequest, Connector, Content,
    Device, Invitation, Manifest, SessionKey, address_item, connected, disconnected, proof_hash,
    proof_item, read_link, refused, unsupported_item,
};
use bahaa_crypto::{ExposeSecret, SecretString};
use bahaa_store::{Connection, SentTx};
use bahaa_ton::{Identity, Reading, TonBatch, max_messages};
use bahaa_types::{Address, ChainFamily, NetworkId, TonContract, hex};
use futures_util::future::join;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep};
use url::Url;
use zeroize::Zeroizing;

use super::{EngineError, Notice, Reply, Worker};
use crate::chains::AnyChain;
use crate::{KeyringError, WalletId, WalletKind};

mod asks;

const MAX_APPS: usize = 32;
const MAX_INVITES: usize = 4;
const INVITE_LIFE: Duration = Duration::from_secs(300);
const RECONNECT: Duration = Duration::from_secs(1);
const LONGEST_WAIT: Duration = Duration::from_secs(60);
const SEND_TRIES: u32 = 3;
const CONNECT_EVENT: u64 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct InviteId(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AskId(u64);

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AppId(String);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppInfo {
    pub name: String,
    pub domain: String,
    pub url: String,
    pub icon: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppAccount {
    pub wallet: WalletId,
    pub index: u32,
    pub network: NetworkId,
    pub address: Address,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invite {
    pub id: InviteId,
    pub app: AppInfo,
    pub accounts: Vec<AppAccount>,
    pub proof: bool,
    pub unsupported: Vec<String>,
    pub valid_for: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectedApp {
    pub id: AppId,
    pub app: AppInfo,
    pub network: NetworkId,
    pub address: Address,
    pub connected_at: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AskKind {
    Send,
    SignMessage,
    SignData,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppAsk {
    pub id: AskId,
    pub app: AppId,
    pub name: String,
    pub domain: String,
    pub kind: AskKind,
    pub arrived_at: u64,
    pub expires_at: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Connections {
    pub apps: Vec<ConnectedApp>,
    pub asks: Vec<AppAsk>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AskReview {
    Transaction(Box<TxReview>),
    Data(Box<DataReview>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxReview {
    pub from: Address,
    pub network: NetworkId,
    pub readings: Vec<Reading>,
    pub fee: Option<FeeQuote>,
    pub valid_for: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataReview {
    pub from: Address,
    pub network: NetworkId,
    pub domain: String,
    pub content: Content,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Approved {
    Sent(Box<SentTx>),
    Signed,
}

pub(super) struct Links {
    connector: Option<Connector>,
    sessions: Vec<Session>,
    invites: BTreeMap<InviteId, Invited>,
    asks: Vec<Ask>,
    listening: BTreeMap<String, Listener>,
    next: u64,
    out: watch::Sender<Connections>,
    pub(super) next_expiry: Option<Instant>,
}

struct Listener {
    ids: Vec<[u8; 32]>,
    task: JoinHandle<()>,
}

struct Heard {
    bridge: Bridge,
    ids: Vec<[u8; 32]>,
    cursor: Option<String>,
}

struct Session {
    record: Connection,
    key: Arc<SessionKey>,
    app: [u8; 32],
    bridge: Bridge,
}

struct Invited {
    invitation: Invitation,
    manifest: Manifest,
    bridge: Url,
    key: Arc<SessionKey>,
    accounts: Vec<AppAccount>,
    expires: Instant,
}

struct Ask {
    id: AskId,
    session: String,
    request: u64,
    call: Call,
    trace: Option<String>,
    arrived_at: u64,
    expires_at: u64,
    plan: Option<Reviewed>,
}

struct Reviewed {
    batch: Box<TonBatch>,
    fee: Option<FeeQuote>,
    expires: Instant,
}

pub(super) struct Met {
    session: u64,
    invitation: Invitation,
    accounts: Vec<AppAccount>,
    manifest: Result<Manifest, ConnectError>,
    bridge: Url,
    reply: Reply<Invite>,
}

pub(super) struct Linked {
    session: u64,
    linking: Linking,
    result: Result<(), ConnectError>,
    reply: Reply<ConnectedApp>,
}

struct Linking {
    record: Connection,
    key: Arc<SessionKey>,
    app: [u8; 32],
    bridge: Bridge,
    message: Vec<u8>,
    trace: Option<String>,
}

struct SignedIn {
    network: String,
    contract: TonContract,
    identity: Identity,
    proof: Option<(u64, String, [u8; 64])>,
}

pub(super) struct Rehearsed {
    session: u64,
    ask: AskId,
    result: Result<(TonBatch, Option<FeeQuote>), ChainError>,
    reply: Reply<AskReview>,
}

impl Links {
    pub(super) fn new(
        bridges: Option<Bridges>,
    ) -> Result<(Self, watch::Receiver<Connections>), EngineError> {
        let connector = bridges.map(Connector::new).transpose()?;
        let (out, connections) = watch::channel(Connections::default());
        Ok((
            Self {
                connector,
                sessions: Vec::new(),
                invites: BTreeMap::new(),
                asks: Vec::new(),
                listening: BTreeMap::new(),
                next: 0,
                out,
                next_expiry: None,
            },
            connections,
        ))
    }

    pub(super) fn load(&mut self, records: Vec<Connection>) {
        self.sessions = records.into_iter().filter_map(session).collect();
        self.sessions
            .sort_by_key(|session| session.record.created_at);
    }

    fn bump(&mut self) -> u64 {
        self.next = self.next.wrapping_add(1);
        self.next
    }

    fn position(&self, id: &str) -> Option<usize> {
        self.sessions
            .iter()
            .position(|session| session.record.id == id)
    }
}

fn session(record: Connection) -> Option<Session> {
    let mut secret = Zeroizing::new([0u8; 32]);
    hex::decode_into(&record.secret, secret.as_mut_slice())?;
    let key = SessionKey::from_secret(&secret);
    let app = hex::decode_array::<32>(&record.app)?;
    let bridge = Bridge::new(Url::parse(&record.bridge).ok()?).ok()?;
    (hex::encode(&key.id()) == record.id).then(|| Session {
        record,
        key: Arc::new(key),
        app,
        bridge,
    })
}

impl Worker {
    pub(super) fn connect_app(&mut self, link: &str, reply: Reply<Invite>) {
        let started = self.ensure_unlocked().and_then(|()| {
            let connector = self
                .links
                .connector
                .clone()
                .ok_or(ChainError::Unsupported("connecting apps in this setup"))?;
            let invitation = read_link(link)?;
            let accounts = self.app_accounts(&invitation.request)?;
            Ok((connector, invitation, accounts))
        });
        let (connector, invitation, accounts) = match started {
            Ok(started) => started,
            Err(err) => {
                let _ = reply.send(Err(err));
                return;
            }
        };
        let (session, notices) = (self.session, self.notices_tx.clone());
        tokio::spawn(async move {
            let (manifest, bridge) = join(
                connector.manifest(&invitation),
                connector.bridge(&invitation),
            )
            .await;
            let met = Met {
                session,
                invitation,
                accounts,
                manifest,
                bridge,
                reply,
            };
            let _ = notices.send(Notice::Met(Box::new(met))).await;
        });
    }

    fn app_accounts(&self, request: &ConnectRequest) -> Result<Vec<AppAccount>, EngineError> {
        let wanted = request.items.iter().find_map(|item| match item {
            ConnectItem::Address { network } => network.clone(),
            ConnectItem::Proof { .. } | ConnectItem::Other(_) => None,
        });
        let known: Vec<(&NetworkId, bool)> = self
            .chains
            .iter()
            .map(AnyChain::network)
            .filter(|network| {
                global_id(&network.kind)
                    .is_some_and(|id| wanted.as_ref().is_none_or(|wanted| *wanted == id))
            })
            .map(|network| (&network.id, network.testnet))
            .collect();
        let networks: Vec<&NetworkId> = known
            .iter()
            .filter(|(_, testnet)| self.settings.show_testnets || !testnet)
            .map(|(id, _)| *id)
            .collect();
        if networks.is_empty() {
            return Err(ChainError::Invalid(if known.is_empty() {
                "this app wants a TON network this wallet doesn't have"
            } else {
                "this app wants the TON test network, turn on test networks first"
            })
            .into());
        }
        let keyring = self.keyring.as_ref().ok_or(KeyringError::Locked)?;
        let spending: Vec<WalletId> = keyring
            .wallets()?
            .into_iter()
            .filter(|wallet| !matches!(wallet.kind, WalletKind::Watch(_)) && wallet.ton.is_some())
            .map(|wallet| wallet.id)
            .collect();
        let accounts: Vec<AppAccount> = networks
            .into_iter()
            .flat_map(|network| {
                self.accounts
                    .iter()
                    .filter(|account| {
                        spending.contains(&account.wallet)
                            && account.serves(network, ChainFamily::Ton)
                    })
                    .map(|account| AppAccount {
                        wallet: account.wallet,
                        index: account.index,
                        network: network.clone(),
                        address: account.address,
                    })
            })
            .collect();
        if accounts.is_empty() {
            return Err(
                ChainError::Invalid("add a TON wallet that can send before connecting").into(),
            );
        }
        Ok(accounts)
    }

    pub(super) fn met(&mut self, met: Met) {
        let Met {
            session,
            invitation,
            accounts,
            manifest,
            bridge,
            reply,
        } = met;
        if session != self.session || !self.unlocked() {
            let _ = reply.send(Err(KeyringError::Locked.into()));
            return;
        }
        let made = manifest.and_then(|manifest| Ok((manifest, SessionKey::generate()?)));
        let (manifest, key) = match made {
            Ok(made) => made,
            Err(err) => {
                let code = if err == ConnectError::NoManifest {
                    Code::ManifestNotFound
                } else {
                    Code::ManifestContent
                };
                if let (Ok(key), Ok(bridge)) = (SessionKey::generate(), Bridge::new(bridge)) {
                    tell(
                        bridge,
                        Arc::new(key),
                        invitation.app,
                        refused(CONNECT_EVENT, code, &err.to_string()),
                        invitation.trace.clone(),
                    );
                }
                let _ = reply.send(Err(err.into()));
                return;
            }
        };
        let now = Instant::now();
        self.links
            .invites
            .retain(|_, invited| invited.expires > now);
        while self.links.invites.len() >= MAX_INVITES {
            self.links.invites.pop_first();
        }
        let id = InviteId(self.links.bump());
        let invite = Invite {
            id,
            app: info(&manifest),
            accounts: accounts.clone(),
            proof: proof_of(&invitation.request).is_some(),
            unsupported: invitation
                .request
                .items
                .iter()
                .filter_map(|item| match item {
                    ConnectItem::Other(name) => Some(name.clone()),
                    ConnectItem::Address { .. } | ConnectItem::Proof { .. } => None,
                })
                .collect(),
            valid_for: INVITE_LIFE,
        };
        self.links.invites.insert(
            id,
            Invited {
                invitation,
                manifest,
                bridge,
                key: Arc::new(key),
                accounts,
                expires: now + INVITE_LIFE,
            },
        );
        self.schedule_expiry();
        let _ = reply.send(Ok(invite));
    }

    pub(super) async fn approve_app(
        &mut self,
        id: InviteId,
        account: AppAccount,
        password: SecretString,
        reply: Reply<ConnectedApp>,
    ) {
        let linking = match self.link_app(id, account, password).await {
            Ok(linking) => linking,
            Err(err) => {
                let _ = reply.send(Err(err));
                return;
            }
        };
        let (session, notices) = (self.session, self.notices_tx.clone());
        tokio::spawn(async move {
            let result = post(
                &linking.bridge,
                &linking.key,
                &linking.app,
                &linking.message,
                linking.trace.as_deref(),
            )
            .await;
            let linked = Linked {
                session,
                linking,
                result,
                reply,
            };
            let _ = notices.send(Notice::Linked(Box::new(linked))).await;
        });
    }

    async fn link_app(
        &mut self,
        id: InviteId,
        account: AppAccount,
        password: SecretString,
    ) -> Result<Linking, EngineError> {
        self.ensure_unlocked()?;
        let invited = self
            .links
            .invites
            .remove(&id)
            .ok_or(EngineError::UnknownRequest)?;
        if Instant::now() >= invited.expires {
            return Err(EngineError::RequestExpired);
        }
        let checked = if !invited.accounts.contains(&account) {
            Err(ChainError::Invalid("this account can't be given to this app").into())
        } else if self.links.sessions.len() >= MAX_APPS {
            Err(ChainError::Invalid("too many apps are connected, disconnect one first").into())
        } else {
            self.chain_index(&account.network)
                .ok_or(EngineError::UnknownNetwork)
        };
        let chain = match checked {
            Ok(chain) => chain,
            Err(err) => {
                self.links.invites.insert(id, invited);
                return Err(err);
            }
        };
        let proof = proof_of(&invited.invitation.request);
        let domain = invited.manifest.domain.clone();
        match self
            .sign_in(
                chain,
                &account,
                proof.map(|payload| (domain, payload)),
                password,
            )
            .await
        {
            Ok(signed) => linking(invited, account, &signed),
            Err(err) => {
                if err == EngineError::Keyring(KeyringError::WrongPassword) {
                    self.links.invites.insert(id, invited);
                }
                Err(err)
            }
        }
    }

    async fn sign_in(
        &mut self,
        chain: usize,
        account: &AppAccount,
        proof: Option<(String, String)>,
        password: SecretString,
    ) -> Result<SignedIn, EngineError> {
        let network =
            global_id(&self.chains[chain].network().kind).ok_or(EngineError::UnknownNetwork)?;
        let chains = Arc::clone(&self.chains);
        let (wallet, index, address) = (account.wallet, account.index, account.address);
        self.keyring(move |keyring| {
            keyring.check_password(password.expose_secret().as_bytes())?;
            let contract = keyring
                .wallets()?
                .into_iter()
                .find(|info| info.id == wallet)
                .and_then(|info| info.ton)
                .ok_or(KeyringError::UnknownWallet)?;
            let seed = keyring.seed(wallet)?;
            let ton = chains[chain].ton().ok_or(EngineError::UnknownNetwork)?;
            let identity = ton.identity(seed, index, &address)?;
            let proof = match proof {
                Some((domain, payload)) => {
                    let timestamp = now();
                    let hash = proof_hash(&address, &domain, timestamp, &payload)
                        .ok_or_else(ChainError::malformed)?;
                    let signature = ton.sign_hash(seed, index, &address, &hash)?;
                    Some((timestamp, payload, signature))
                }
                None => None,
            };
            Ok::<_, EngineError>(SignedIn {
                network,
                contract,
                identity,
                proof,
            })
        })
        .await
    }

    pub(super) fn linked(&mut self, linked: Linked) {
        let Linked {
            session,
            linking,
            result,
            reply,
        } = linked;
        if session != self.session || !self.unlocked() {
            let _ = reply.send(Err(KeyringError::Locked.into()));
            return;
        }
        if let Err(err) = result {
            let _ = reply.send(Err(err.into()));
            return;
        }
        let Linking {
            record,
            key,
            app,
            bridge,
            ..
        } = linking;
        let stale: Vec<String> = self
            .links
            .sessions
            .iter()
            .filter(|session| {
                session.record.domain == record.domain
                    && session.record.address == record.address
                    && session.record.network == record.network
            })
            .map(|session| session.record.id.clone())
            .collect();
        for id in stale {
            self.end_session(&id, true);
        }
        if let Err(err) = self.store.save_connection(&record) {
            let _ = reply.send(Err(err.into()));
            return;
        }
        let shown = connected_app(&record);
        self.links.sessions.push(Session {
            record,
            key,
            app,
            bridge,
        });
        self.listen();
        self.publish_links();
        let _ = reply.send(Ok(shown));
    }

    pub(super) fn reject_app(&mut self, id: InviteId) -> Result<(), EngineError> {
        self.ensure_unlocked()?;
        let invited = self
            .links
            .invites
            .remove(&id)
            .ok_or(EngineError::UnknownRequest)?;
        if let Ok(bridge) = Bridge::new(invited.bridge) {
            tell(
                bridge,
                invited.key,
                invited.invitation.app,
                refused(
                    CONNECT_EVENT,
                    Code::Declined,
                    "the user declined the connection",
                ),
                invited.invitation.trace,
            );
        }
        self.schedule_expiry();
        Ok(())
    }

    pub(super) fn disconnect_app(&mut self, id: &AppId) -> Result<(), EngineError> {
        self.ensure_unlocked()?;
        if self.links.position(&id.0).is_none() {
            return Err(EngineError::UnknownApp);
        }
        self.end_session(&id.0, true);
        self.listen();
        self.publish_links();
        Ok(())
    }

    fn end_session(&mut self, id: &str, tell_app: bool) {
        let Some(index) = self.links.position(id) else {
            return;
        };
        let mut session = self.links.sessions.remove(index);
        if tell_app {
            session.record.event = session.record.event.saturating_add(1);
            tell(
                session.bridge,
                session.key,
                session.app,
                disconnected(session.record.event),
                None,
            );
        }
        let _ = self.store.forget_connection(id);
        self.links.asks.retain(|ask| ask.session != id);
        self.schedule_expiry();
    }

    pub(super) fn prune_links(&mut self) {
        let gone: Vec<String> = self
            .links
            .sessions
            .iter()
            .filter(|session| self.app_account(&session.record).is_none())
            .map(|session| session.record.id.clone())
            .collect();
        if gone.is_empty() {
            return;
        }
        for id in gone {
            self.end_session(&id, true);
        }
        self.listen();
        self.publish_links();
    }

    pub(super) fn listen(&mut self) {
        let mut wanted: BTreeMap<String, Heard> = BTreeMap::new();
        for session in &self.links.sessions {
            let heard = wanted
                .entry(session.record.bridge.clone())
                .or_insert_with(|| Heard {
                    bridge: session.bridge.clone(),
                    ids: Vec::new(),
                    cursor: None,
                });
            heard.ids.push(session.key.id());
            heard.cursor = latest(heard.cursor.take(), session.record.cursor.clone());
        }
        self.links.listening.retain(|url, listener| {
            let keep = wanted
                .get(url)
                .is_some_and(|heard| heard.ids == listener.ids);
            if !keep {
                listener.task.abort();
            }
            keep
        });
        for (
            url,
            Heard {
                bridge,
                ids,
                cursor,
            },
        ) in wanted
        {
            if self.links.listening.contains_key(&url) {
                continue;
            }
            let task = tokio::spawn(listening(
                bridge,
                ids.clone(),
                cursor,
                self.notices_tx.clone(),
                self.session,
            ));
            self.links.listening.insert(url, Listener { ids, task });
        }
    }

    fn tell_session(&self, index: usize, message: Vec<u8>, trace: Option<String>) {
        let session = &self.links.sessions[index];
        tell(
            session.bridge.clone(),
            Arc::clone(&session.key),
            session.app,
            message,
            trace,
        );
    }

    pub(super) fn forget_links(&mut self) {
        for ask in std::mem::take(&mut self.links.asks) {
            self.refuse(&ask, "the wallet was locked");
        }
        for (_, listener) in std::mem::take(&mut self.links.listening) {
            listener.task.abort();
        }
        self.links.sessions.clear();
        self.links.invites.clear();
        self.links.next_expiry = None;
        self.publish_links();
    }

    pub(super) fn publish_links(&self) {
        let apps = self
            .links
            .sessions
            .iter()
            .map(|session| connected_app(&session.record))
            .collect();
        let asks = self
            .links
            .asks
            .iter()
            .filter_map(|ask| {
                let record = &self.links.sessions[self.links.position(&ask.session)?].record;
                Some(AppAsk {
                    id: ask.id,
                    app: AppId(record.id.clone()),
                    name: record.name.clone(),
                    domain: record.domain.clone(),
                    kind: match ask.call {
                        Call::SignMessage(_) => AskKind::SignMessage,
                        Call::SignData(_) => AskKind::SignData,
                        Call::SendTransaction(_) | Call::Disconnect => AskKind::Send,
                    },
                    arrived_at: ask.arrived_at,
                    expires_at: ask.expires_at,
                })
            })
            .collect();
        let connections = Connections { apps, asks };
        self.links.out.send_if_modified(|current| {
            let changed = *current != connections;
            if changed {
                *current = connections;
            }
            changed
        });
    }
}

fn global_id(kind: &ChainKind) -> Option<String> {
    match kind {
        ChainKind::Ton { global_id, .. } => Some(global_id.to_string()),
        _ => None,
    }
}

fn linking(
    invited: Invited,
    account: AppAccount,
    signed: &SignedIn,
) -> Result<Linking, EngineError> {
    let Invited {
        invitation,
        manifest,
        bridge,
        key,
        ..
    } = invited;
    let items: Vec<_> = invitation
        .request
        .items
        .iter()
        .map(|item| match (item, &signed.proof) {
            (ConnectItem::Address { .. }, _) => address_item(
                &account.address,
                &signed.network,
                &signed.identity.public_key,
                &signed.identity.state_init,
            ),
            (ConnectItem::Proof { .. }, Some((timestamp, payload, signature))) => {
                proof_item(*timestamp, &manifest.domain, signature, payload)
            }
            (ConnectItem::Proof { .. }, None) => unsupported_item("ton_proof"),
            (ConnectItem::Other(name), _) => unsupported_item(name),
        })
        .collect();
    let device = Device {
        version: env!("CARGO_PKG_VERSION"),
        max_messages: max_messages(signed.contract),
        sign_message: signed.contract == TonContract::W5,
    };
    let AppInfo {
        name,
        domain,
        url,
        icon,
    } = info(&manifest);
    let record = Connection {
        id: hex::encode(&key.id()),
        secret: hex::encode(key.secret().as_slice()),
        app: hex::encode(&invitation.app),
        bridge: bridge.to_string(),
        url,
        domain,
        name,
        icon,
        network: account.network,
        address: account.address,
        event: CONNECT_EVENT,
        request: None,
        cursor: None,
        created_at: now(),
    };
    Ok(Linking {
        record,
        key,
        app: invitation.app,
        bridge: Bridge::new(bridge)?,
        message: connected(CONNECT_EVENT, &items, &device),
        trace: invitation.trace,
    })
}

fn info(manifest: &Manifest) -> AppInfo {
    AppInfo {
        name: manifest.name.clone(),
        domain: manifest.domain.clone(),
        url: manifest.url.to_string(),
        icon: manifest.icon.as_ref().map(Url::to_string),
    }
}

fn connected_app(record: &Connection) -> ConnectedApp {
    ConnectedApp {
        id: AppId(record.id.clone()),
        app: AppInfo {
            name: record.name.clone(),
            domain: record.domain.clone(),
            url: record.url.clone(),
            icon: record.icon.clone(),
        },
        network: record.network.clone(),
        address: record.address,
        connected_at: record.created_at,
    }
}

fn proof_of(request: &ConnectRequest) -> Option<String> {
    request.items.iter().find_map(|item| match item {
        ConnectItem::Proof { payload } => Some(payload.clone()),
        ConnectItem::Address { .. } | ConnectItem::Other(_) => None,
    })
}

fn latest(known: Option<String>, other: Option<String>) -> Option<String> {
    match (known, other) {
        (Some(known), Some(other)) => match (known.parse::<u64>(), other.parse::<u64>()) {
            (Ok(a), Ok(b)) if a >= b => Some(known),
            _ => Some(other),
        },
        (known, other) => known.or(other),
    }
}

fn tell(
    bridge: Bridge,
    key: Arc<SessionKey>,
    to: [u8; 32],
    message: Vec<u8>,
    trace: Option<String>,
) {
    tokio::spawn(async move {
        let _ = post(&bridge, &key, &to, &message, trace.as_deref()).await;
    });
}

async fn post(
    bridge: &Bridge,
    key: &SessionKey,
    to: &[u8; 32],
    message: &[u8],
    trace: Option<&str>,
) -> Result<(), ConnectError> {
    let mut tries = 1;
    loop {
        match bridge.send(key, to, message, None, trace).await {
            Err(ConnectError::Network(_)) if tries < SEND_TRIES => {
                tries += 1;
                sleep(RECONNECT).await;
            }
            done => return done,
        }
    }
}

async fn listening(
    bridge: Bridge,
    ids: Vec<[u8; 32]>,
    mut cursor: Option<String>,
    notices: mpsc::Sender<Notice>,
    session: u64,
) {
    let mut wait = RECONNECT;
    loop {
        let (sink, mut found) = mpsc::channel(8);
        let reading = async {
            let sink = sink;
            bridge.listen(&ids, cursor.as_deref(), &sink).await
        };
        let forwarding = async {
            let mut last = None;
            while let Some(incoming) = found.recv().await {
                if incoming.event.is_some() {
                    last.clone_from(&incoming.event);
                }
                let notice = Notice::Bridged(session, Box::new(incoming));
                if notices.send(notice).await.is_err() {
                    break;
                }
            }
            last
        };
        let (result, last) = join(reading, forwarding).await;
        cursor = last.or(cursor);
        if notices.is_closed() {
            return;
        }
        if result.is_ok() {
            wait = RECONNECT;
            sleep(RECONNECT).await;
        } else {
            sleep(wait).await;
            wait = (wait * 2).min(LONGEST_WAIT);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_newest_bridge_event_is_kept() {
        assert_eq!(
            latest(Some("9".to_owned()), Some("12".to_owned())).as_deref(),
            Some("12")
        );
        assert_eq!(
            latest(Some("12".to_owned()), Some("9".to_owned())).as_deref(),
            Some("12")
        );
        assert_eq!(latest(None, Some("3".to_owned())).as_deref(), Some("3"));
        assert_eq!(latest(Some("3".to_owned()), None).as_deref(), Some("3"));
    }
}
