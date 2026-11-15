use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bahaa_chain::{ChainError, FeeQuote, MAX_MEMO, Signed, now};
use bahaa_config::ChainKind;
use bahaa_connect::{
    AppRequest, Bridge, Bridges, Call, Code, ConnectError, ConnectItem, ConnectRequest, Connector,
    Content, Device, Incoming, Invitation, JettonItem, Manifest, Message, NftItem, Outgoing,
    SessionKey, Transaction, TransferItem, acknowledged, address_item, answer, connected,
    data_hash, disconnected, failure, proof_hash, proof_item, read_link, read_request, refused,
    schema_crc, sent, signed_data, signed_message, unsupported_item,
};
use bahaa_crypto::{ExposeSecret, SecretString};
use bahaa_store::{Connection, SentKind, SentStatus, SentTx};
use bahaa_ton::{
    DecodedBatch, Handover, Identity, Meaning, Outbound, Reading, Requested, TonBatch, Wanted,
    cell_data_hash, max_messages,
};
use bahaa_types::{
    Address, Amount, AssetId, ChainFamily, NetworkId, NftId, TonContract, hex, is_plain_text,
};
use futures_util::future::join;
use ruint::aliases::U256;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep};
use url::Url;
use zeroize::Zeroizing;

use super::{EngineError, Notice, Reply, Worker};
use crate::chains::{self, AnyChain};
use crate::{KeyringError, WalletId, WalletKind};

const MAX_APPS: usize = 32;
const MAX_INVITES: usize = 4;
const MAX_ASKS: usize = 16;
const INVITE_LIFE: Duration = Duration::from_secs(300);
const ASK_LIFE: u64 = 300;
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

enum Work {
    Data(Content, String),
    Relayed(Box<TonBatch>),
    Send(Box<TonBatch>),
}

enum Done {
    Data(u64, [u8; 64]),
    Internal(Vec<u8>),
    Sent(Signed, DecodedBatch),
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

    pub(super) fn bridged(&mut self, incoming: Incoming) {
        let opened = self
            .links
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, session)| session.app == incoming.from)
            .find_map(|(index, session)| {
                let plain = session.key.open(&incoming.from, &incoming.sealed).ok()?;
                Some((index, plain))
            });
        let Some((index, plain)) = opened else {
            return;
        };
        let read = read_request(&plain);
        let id = match &read {
            Ok(request) => Some(request.id),
            Err(bad) => bad.id,
        };
        let record = &mut self.links.sessions[index].record;
        if let Some(id) = id {
            if record.request.is_some_and(|last| id <= last) {
                return;
            }
            record.request = Some(id);
        }
        if incoming.event.is_some() {
            record.cursor.clone_from(&incoming.event);
        }
        let _ = self.store.save_connection(record);
        let trace = incoming.trace;
        match read {
            Err(bad) => {
                if let Some(id) = bad.id {
                    self.tell_session(index, failure(id, bad.code, bad.reason), trace);
                }
            }
            Ok(AppRequest {
                id,
                call: Call::Disconnect,
            }) => {
                self.tell_session(index, acknowledged(id), trace);
                let ended = self.links.sessions[index].record.id.clone();
                self.end_session(&ended, false);
                self.listen();
                self.publish_links();
            }
            Ok(AppRequest { id, call }) => match self.admit(index, &call) {
                Ok(expires_at) => {
                    let ask = Ask {
                        id: AskId(self.links.bump()),
                        session: self.links.sessions[index].record.id.clone(),
                        request: id,
                        call,
                        trace,
                        arrived_at: now(),
                        expires_at,
                        plan: None,
                    };
                    self.links.asks.push(ask);
                    self.schedule_expiry();
                    self.publish_links();
                }
                Err((code, reason)) => self.tell_session(index, failure(id, code, reason), trace),
            },
        }
    }

    fn admit(&self, index: usize, call: &Call) -> Result<u64, (Code, &'static str)> {
        let record = &self.links.sessions[index].record;
        let bad = |reason| (Code::BadRequest, reason);
        let (network, from, valid_until) = match call {
            Call::SendTransaction(tx) | Call::SignMessage(tx) => {
                (tx.network.as_ref(), tx.from, tx.valid_until)
            }
            Call::SignData(data) => (data.network.as_ref(), data.from, None),
            Call::Disconnect => return Err(bad("this request can't be queued")),
        };
        let ours = self
            .chain_index(&record.network)
            .and_then(|chain| global_id(&self.chains[chain].network().kind));
        if network.is_some_and(|wanted| Some(wanted) != ours.as_ref()) {
            return Err(bad("this request is for another network"));
        }
        if from.is_some_and(|from| from != record.address) {
            return Err(bad("this request is for another account"));
        }
        let (wallet, _) = self
            .app_account(record)
            .ok_or(bad("this account is no longer in the wallet"))?;
        if matches!(call, Call::SignMessage(_)) && self.contract_of(wallet) != Some(TonContract::W5)
        {
            return Err((
                Code::NotSupported,
                "this wallet can't sign messages for relayers",
            ));
        }
        if self.links.asks.len() >= MAX_ASKS {
            return Err((Code::Unknown, "too many requests are waiting in the wallet"));
        }
        let now = now();
        match valid_until {
            Some(until) if until <= now => Err(bad("this request has expired")),
            Some(until) => Ok(until.min(now + ASK_LIFE)),
            None => Ok(now + ASK_LIFE),
        }
    }

    fn app_account(&self, record: &Connection) -> Option<(WalletId, u32)> {
        self.accounts
            .iter()
            .find(|account| {
                account.address == record.address
                    && account.serves(&record.network, ChainFamily::Ton)
            })
            .map(|account| (account.wallet, account.index))
    }

    fn contract_of(&self, wallet: WalletId) -> Option<TonContract> {
        self.keyring
            .as_ref()?
            .wallets()
            .ok()?
            .into_iter()
            .find(|info| info.id == wallet)?
            .ton
    }

    pub(super) fn review_ask(&mut self, id: AskId, reply: Reply<AskReview>) {
        let target = self.ensure_unlocked().and_then(|()| self.ask_target(id));
        let (ask, record, (_, index), chain) = match target {
            Ok(target) => target,
            Err(err) => {
                let _ = reply.send(Err(err));
                return;
            }
        };
        let ask = &self.links.asks[ask];
        let tx = match &ask.call {
            Call::SignData(data) => {
                let review = DataReview {
                    from: record.address,
                    network: record.network,
                    domain: record.domain,
                    content: data.content.clone(),
                };
                let _ = reply.send(Ok(AskReview::Data(Box::new(review))));
                return;
            }
            Call::SendTransaction(tx) | Call::SignMessage(tx) => tx.clone(),
            Call::Disconnect => {
                let _ = reply.send(Err(EngineError::UnknownRequest));
                return;
            }
        };
        let relayed = matches!(ask.call, Call::SignMessage(_));
        let (until, from) = (ask.expires_at, record.address);
        let chains = Arc::clone(&self.chains);
        let (session, notices) = (self.session, self.notices_tx.clone());
        tokio::spawn(async move {
            let result =
                chains::shielded(rehearse(&chains[chain], index, from, &tx, until, relayed)).await;
            let rehearsed = Rehearsed {
                session,
                ask: id,
                result,
                reply,
            };
            let _ = notices.send(Notice::Rehearsed(Box::new(rehearsed))).await;
        });
    }

    fn ask_target(
        &self,
        id: AskId,
    ) -> Result<(usize, Connection, (WalletId, u32), usize), EngineError> {
        let ask = self
            .links
            .asks
            .iter()
            .position(|ask| ask.id == id)
            .ok_or(EngineError::UnknownRequest)?;
        if self.links.asks[ask].expires_at <= now() {
            return Err(EngineError::RequestExpired);
        }
        let record = self
            .links
            .position(&self.links.asks[ask].session)
            .map(|session| self.links.sessions[session].record.clone())
            .ok_or(EngineError::UnknownApp)?;
        let account = self
            .app_account(&record)
            .ok_or(KeyringError::UnknownWallet)?;
        let chain = self
            .chain_index(&record.network)
            .ok_or(EngineError::UnknownNetwork)?;
        Ok((ask, record, account, chain))
    }

    pub(super) fn rehearsed(&mut self, rehearsed: Rehearsed) {
        let Rehearsed {
            session,
            ask,
            result,
            reply,
        } = rehearsed;
        if session != self.session || !self.unlocked() {
            let _ = reply.send(Err(KeyringError::Locked.into()));
            return;
        }
        let draft_ttl = self.draft_ttl;
        let Some(slot) = self.links.asks.iter_mut().find(|slot| slot.id == ask) else {
            let _ = reply.send(Err(EngineError::UnknownRequest));
            return;
        };
        let network = self
            .links
            .sessions
            .iter()
            .find(|session| session.record.id == slot.session)
            .map(|session| session.record.network.clone());
        let ((batch, fee), network) = match (result, network) {
            (Ok(planned), Some(network)) => (planned, network),
            (Err(err), _) => {
                let _ = reply.send(Err(err.into()));
                return;
            }
            (Ok(_), None) => {
                let _ = reply.send(Err(EngineError::UnknownApp));
                return;
            }
        };
        let life = draft_ttl.min(Duration::from_secs(slot.expires_at.saturating_sub(now())));
        let review = TxReview {
            from: *batch.from(),
            network,
            readings: batch.readings().to_vec(),
            fee: fee.clone(),
            valid_for: life,
        };
        slot.plan = Some(Reviewed {
            batch: Box::new(batch),
            fee,
            expires: Instant::now() + life,
        });
        let _ = reply.send(Ok(AskReview::Transaction(Box::new(review))));
    }

    pub(super) async fn approve_ask(
        &mut self,
        id: AskId,
        password: SecretString,
        reply: Reply<Approved>,
    ) {
        let (ask, record, done, chain) = match self.sign_ask(id, password).await {
            Ok(signed) => signed,
            Err(err) => {
                let _ = reply.send(Err(err));
                return;
            }
        };
        let Some(index) = self.links.position(&ask.session) else {
            let _ = reply.send(Err(EngineError::UnknownApp));
            return;
        };
        match done {
            Done::Data(timestamp, signature) => {
                let Call::SignData(data) = &ask.call else {
                    let _ = reply.send(Err(EngineError::UnknownRequest));
                    return;
                };
                let signed = signed_data(
                    &signature,
                    &record.address,
                    timestamp,
                    &record.domain,
                    &data.payload,
                );
                self.tell_session(index, answer(ask.request, &signed), ask.trace);
                let _ = reply.send(Ok(Approved::Signed));
            }
            Done::Internal(internal) => {
                self.tell_session(index, signed_message(ask.request, &internal), ask.trace);
                let _ = reply.send(Ok(Approved::Signed));
            }
            Done::Sent(signed, decoded) => {
                self.send_for_app(chain, (index, &ask), &record, (signed, &decoded), reply);
            }
        }
        self.publish_links();
        self.schedule_expiry();
    }

    fn send_for_app(
        &mut self,
        chain: usize,
        (index, ask): (usize, &Ask),
        record: &Connection,
        (signed, decoded): (Signed, &DecodedBatch),
        reply: Reply<Approved>,
    ) {
        let built = ask
            .plan
            .as_ref()
            .and_then(|plan| Some((plan.batch.readings(), plan.fee.as_ref()?.max())))
            .ok_or(EngineError::NotReviewed)
            .and_then(|(readings, fee_max)| app_sent(record, readings, fee_max, &signed, decoded));
        let tx = match built {
            Ok(tx) if self.reused(&tx.network, &tx.from, tx.sequence) => {
                Err(EngineError::StillSending)
            }
            other => other,
        };
        let tx = match tx.and_then(|tx| self.save(tx.clone()).map(|()| tx)) {
            Ok(tx) => tx,
            Err(err) => {
                let refusal = failure(ask.request, Code::Unknown, "the wallet couldn't send it");
                self.tell_session(index, refusal, ask.trace.clone());
                let _ = reply.send(Err(err));
                return;
            }
        };
        let session = &self.links.sessions[index];
        let (bridge, key, app) = (
            session.bridge.clone(),
            Arc::clone(&session.key),
            session.app,
        );
        let (request, trace, bytes) = (ask.request, ask.trace.clone(), signed.bytes.clone());
        let (delivered, outcome) = oneshot::channel();
        self.deliver(chain, vec![(tx, signed)], delivered, None);
        tokio::spawn(async move {
            match outcome.await {
                Ok(Ok(tx)) => {
                    let _ = post(
                        &bridge,
                        &key,
                        &app,
                        &sent(request, &bytes),
                        trace.as_deref(),
                    )
                    .await;
                    let _ = reply.send(Ok(Approved::Sent(Box::new(tx))));
                }
                Ok(Err(err)) => {
                    let refusal = failure(request, Code::Unknown, "the network refused it");
                    let _ = post(&bridge, &key, &app, &refusal, trace.as_deref()).await;
                    let _ = reply.send(Err(err));
                }
                Err(_) => {
                    let _ = reply.send(Err(EngineError::Stopped));
                }
            }
        });
    }

    async fn sign_ask(
        &mut self,
        id: AskId,
        password: SecretString,
    ) -> Result<(Ask, Connection, Done, usize), EngineError> {
        self.ensure_unlocked()?;
        let (position, record, (wallet, index), chain) = match self.ask_target(id) {
            Err(EngineError::RequestExpired) => {
                self.expire_links();
                return Err(EngineError::RequestExpired);
            }
            other => other?,
        };
        let ask = &self.links.asks[position];
        let work = match &ask.call {
            Call::SignData(data) => Work::Data(data.content.clone(), record.domain.clone()),
            Call::SendTransaction(_) | Call::SignMessage(_) => {
                let plan = ask
                    .plan
                    .as_ref()
                    .filter(|plan| plan.expires > Instant::now())
                    .ok_or(EngineError::NotReviewed)?;
                let batch = plan.batch.clone();
                if matches!(ask.call, Call::SignMessage(_)) {
                    Work::Relayed(batch)
                } else {
                    Work::Send(batch)
                }
            }
            Call::Disconnect => return Err(EngineError::UnknownRequest),
        };
        let chains = Arc::clone(&self.chains);
        let address = record.address;
        let done = self
            .keyring(move |keyring| {
                keyring.check_password(password.expose_secret().as_bytes())?;
                let seed = keyring.seed(wallet)?;
                let ton = chains[chain].ton().ok_or(EngineError::UnknownNetwork)?;
                match work {
                    Work::Data(content, domain) => {
                        let timestamp = now();
                        let hash = match &content {
                            Content::Cell { schema, cell } => cell_data_hash(
                                schema_crc(schema),
                                timestamp,
                                &address,
                                &domain,
                                cell,
                            ),
                            other => data_hash(&address, &domain, timestamp, other),
                        }
                        .ok_or(ChainError::Invalid("this data can't be signed"))?;
                        let signature = ton.sign_hash(seed, index, &address, &hash)?;
                        Ok::<_, EngineError>(Done::Data(timestamp, signature))
                    }
                    Work::Relayed(batch) => Ok(Done::Internal(ton.sign_internal(&batch, seed)?)),
                    Work::Send(batch) => {
                        let signed = ton.sign_batch(&batch, seed)?;
                        let decoded = ton.decode_batch(&signed)?;
                        if decoded.from != *batch.from() || decoded.messages != batch.messages() {
                            return Err(ChainError::Mismatch("the signed request").into());
                        }
                        Ok(Done::Sent(signed, decoded))
                    }
                }
            })
            .await?;
        let ask = self.links.asks.remove(position);
        Ok((ask, record, done, chain))
    }

    pub(super) fn reject_ask(&mut self, id: AskId) -> Result<(), EngineError> {
        self.ensure_unlocked()?;
        let position = self
            .links
            .asks
            .iter()
            .position(|ask| ask.id == id)
            .ok_or(EngineError::UnknownRequest)?;
        let ask = self.links.asks.remove(position);
        self.refuse(&ask, "the user declined the request");
        self.publish_links();
        self.schedule_expiry();
        Ok(())
    }

    pub(super) fn expire_links(&mut self) {
        self.links.next_expiry = None;
        let (seconds, moment) = (now(), Instant::now());
        let (expired, kept): (Vec<Ask>, Vec<Ask>) = std::mem::take(&mut self.links.asks)
            .into_iter()
            .partition(|ask| ask.expires_at <= seconds);
        self.links.asks = kept;
        for ask in &expired {
            self.refuse(ask, "the request expired before it was approved");
        }
        self.links
            .invites
            .retain(|_, invited| invited.expires > moment);
        if !expired.is_empty() {
            self.publish_links();
        }
        self.schedule_expiry();
    }

    fn schedule_expiry(&mut self) {
        let (seconds, moment) = (now(), Instant::now());
        let asks = self
            .links
            .asks
            .iter()
            .map(|ask| moment + Duration::from_secs(ask.expires_at.saturating_sub(seconds)));
        let invites = self.links.invites.values().map(|invited| invited.expires);
        self.links.next_expiry = asks.chain(invites).min();
    }

    fn refuse(&self, ask: &Ask, reason: &str) {
        if let Some(index) = self.links.position(&ask.session) {
            self.tell_session(
                index,
                failure(ask.request, Code::Declined, reason),
                ask.trace.clone(),
            );
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

async fn rehearse(
    chain: &AnyChain,
    account: u32,
    from: Address,
    tx: &Transaction,
    until: u64,
    relayed: bool,
) -> Result<(TonBatch, Option<FeeQuote>), ChainError> {
    let ton = chain
        .ton()
        .ok_or(ChainError::Unsupported("apps on this network"))?;
    let messages = match &tx.outgoing {
        Outgoing::Messages(messages) => messages.iter().map(outbound).collect(),
        Outgoing::Items(items) => {
            let wanted: Vec<Wanted> = items.iter().map(wanted).collect();
            ton.build_items(account, &from, &wanted).await?
        }
    };
    let requested = Requested {
        account,
        from,
        messages,
        valid_until: Some(until),
    };
    if relayed {
        Ok((ton.prepare_relayed(&requested).await?, None))
    } else {
        let (batch, fee) = ton.prepare_batch(&requested).await?;
        Ok((batch, Some(fee)))
    }
}

fn outbound(message: &Message) -> Outbound {
    Outbound {
        to: message.to,
        bounce: message.bounce,
        nano: U256::from(message.amount),
        body: message.payload.clone(),
        init: message.state_init.clone(),
    }
}

fn wanted(item: &TransferItem) -> Wanted {
    match item {
        TransferItem::Ton(message) => Wanted::Coins(outbound(message)),
        TransferItem::Jetton(JettonItem {
            master,
            destination,
            amount,
            attach,
            query_id,
            response,
            custom_payload,
            forward_amount,
            forward_payload,
        }) => Wanted::Token(Handover {
            contract: *master,
            to: *destination,
            units: U256::from(*amount),
            attach: attach.map(U256::from),
            query_id: *query_id,
            response: *response,
            custom: custom_payload.clone(),
            forward: forward_amount.map(U256::from),
            payload: forward_payload.clone(),
        }),
        TransferItem::Nft(NftItem {
            item,
            new_owner,
            attach,
            query_id,
            response,
            custom_payload,
            forward_amount,
            forward_payload,
        }) => Wanted::Nft(Handover {
            contract: *item,
            to: *new_owner,
            units: U256::from(1u8),
            attach: attach.map(U256::from),
            query_id: *query_id,
            response: *response,
            custom: custom_payload.clone(),
            forward: forward_amount.map(U256::from),
            payload: forward_payload.clone(),
        }),
    }
}

fn app_sent(
    record: &Connection,
    readings: &[Reading],
    fee_max: Amount,
    signed: &Signed,
    decoded: &DecodedBatch,
) -> Result<SentTx, EngineError> {
    let network = record.network.clone();
    let total = readings
        .iter()
        .try_fold(None::<Amount>, |sum, reading| match sum {
            Some(sum) => sum.checked_add(reading.amount).map(Some),
            None => Ok(Some(reading.amount)),
        })
        .map_err(|_| ChainError::overflow())?
        .ok_or(ChainError::Invalid("the request has no messages"))?;
    let (asset, amount, to, memo) = match readings {
        [
            Reading {
                meaning: Meaning::Token { master, amount, to },
                ..
            },
        ] => (AssetId::Token(network.clone(), *master), *amount, *to, None),
        [
            Reading {
                meaning: Meaning::Nft { item, to },
                ..
            },
        ] => (
            AssetId::Nft(
                network.clone(),
                Box::new(NftId::new(*item, None).map_err(|_| ChainError::malformed())?),
            ),
            Amount::parse("1", 0).map_err(|_| ChainError::malformed())?,
            *to,
            None,
        ),
        [first, rest @ ..] => {
            let memo = match (&first.meaning, rest) {
                (
                    Meaning::Coins {
                        comment: Some(text),
                    },
                    [],
                ) if is_plain_text(text, MAX_MEMO) => Some(text.clone()),
                _ => None,
            };
            (AssetId::Native(network.clone()), total, first.to, memo)
        }
        [] => return Err(ChainError::Invalid("the request has no messages").into()),
    };
    Ok(SentTx {
        id: signed.id.to_string(),
        network,
        from: decoded.from,
        to,
        asset,
        amount,
        sequence: Some(decoded.sequence),
        fee_max,
        fee_paid: None,
        block: None,
        created_at: now(),
        expires_at: Some(decoded.expires_at),
        status: SentStatus::Sending,
        raw: Some(hex::encode(&signed.bytes)),
        transaction: None,
        memo,
        replaces: None,
        kind: SentKind::App,
        private: false,
        name: Some(record.name.clone()),
    })
}

#[cfg(test)]
mod tests {
    use bahaa_chain::TxId;

    use super::*;

    fn party(fill: u8) -> Address {
        Address::Ton {
            workchain: 0,
            hash: [fill; 32],
        }
    }

    fn reading(meaning: Meaning, nano: &str) -> Reading {
        Reading {
            to: party(2),
            bounce: true,
            amount: Amount::parse(nano, 9).unwrap(),
            meaning,
            deploys: false,
        }
    }

    fn record() -> Connection {
        Connection {
            id: "ab".repeat(32),
            secret: "cd".repeat(32),
            app: "ef".repeat(32),
            bridge: "https://connect.ton.org/bridge".to_owned(),
            url: "https://app.example.com".to_owned(),
            domain: "app.example.com".to_owned(),
            name: "Example".to_owned(),
            icon: None,
            network: "ton".parse().unwrap(),
            address: party(1),
            event: 1,
            request: Some(4),
            cursor: None,
            created_at: 1,
        }
    }

    fn sent(readings: &[Reading]) -> SentTx {
        let signed = Signed {
            id: TxId::new("12".repeat(32)).unwrap(),
            bytes: vec![0xb5, 0xee],
        };
        let decoded = DecodedBatch {
            from: party(1),
            messages: Vec::new(),
            sequence: 7,
            expires_at: 1_800_000_000,
        };
        let fee = Amount::parse("0.002", 9).unwrap();
        app_sent(&record(), readings, fee, &signed, &decoded).unwrap()
    }

    #[test]
    fn an_apps_send_is_recorded_by_what_it_moves() {
        let master = party(9);
        let token = sent(&[reading(
            Meaning::Token {
                master,
                amount: Amount::parse("1.5", 6).unwrap(),
                to: party(3),
            },
            "0.05",
        )]);
        assert_eq!(token.asset, AssetId::Token("ton".parse().unwrap(), master));
        assert_eq!(
            (token.amount.to_string(), token.to),
            ("1.5".to_owned(), party(3))
        );
        assert_eq!(
            (token.kind, token.name.as_deref()),
            (SentKind::App, Some("Example"))
        );
        assert_eq!(
            (token.sequence, token.expires_at),
            (Some(7), Some(1_800_000_000))
        );
        assert_eq!(token.raw.as_deref(), Some("b5ee"));
        let mixed = sent(&[
            reading(
                Meaning::Coins {
                    comment: Some("hi".to_owned()),
                },
                "0.1",
            ),
            reading(Meaning::Call { op: Some(1) }, "0.25"),
        ]);
        assert_eq!(mixed.asset, AssetId::Native("ton".parse().unwrap()));
        assert_eq!(
            (mixed.amount.to_string(), mixed.to, mixed.memo),
            ("0.35".to_owned(), party(2), None)
        );
        let paid = sent(&[reading(
            Meaning::Coins {
                comment: Some("hi".to_owned()),
            },
            "0.1",
        )]);
        assert_eq!(paid.memo.as_deref(), Some("hi"));
    }

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
