use std::fmt::Write;
use std::sync::Arc;
use std::time::Duration;

use bahaa_chain::{ChainError, FeeQuote, Signed, Transfer, TxId, TxStatus, check_signed};
use bahaa_crypto::{ExposeSecret, SecretString};
use bahaa_store::{SentStatus, SentTx};
use bahaa_types::{Address, Amount, AssetId, NetworkId, to_plain_text};
use futures_util::future::join_all;
use tokio::time::Instant;

use super::{EngineError, Event, Notice, Reply, Worker};
use crate::chains::{self, AnyChain, AnyPlan};
use crate::{KeyringError, WalletId};

const MAX_DRAFTS: usize = 8;
const DROP_AFTER_SECS: u64 = 600;
const MAX_REASON: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DraftId(u64);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SendRequest {
    pub wallet: WalletId,
    pub asset: AssetId,
    pub to: Address,
    pub amount: Amount,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Draft {
    pub id: DraftId,
    pub transfer: Transfer,
    pub fee: FeeQuote,
    pub valid_for: Duration,
}

pub(super) struct Prepared {
    wallet: WalletId,
    chain: usize,
    transfer: Transfer,
    fee: FeeQuote,
    plan: AnyPlan,
    expires: Instant,
}

pub(super) struct Quoted {
    session: u64,
    wallet: WalletId,
    chain: usize,
    transfer: Transfer,
    result: Result<(AnyPlan, FeeQuote), ChainError>,
    reply: Reply<Draft>,
}

pub(super) struct Delivered {
    record: SentTx,
    delivery: Delivery,
    reply: Reply<SentTx>,
}

enum Delivery {
    Accepted,
    Refused(String),
    Unknown,
}

impl Worker {
    pub(super) fn prepare_send(&mut self, request: &SendRequest, reply: Reply<Draft>) {
        let (chain, transfer) = match self.send_target(request) {
            Ok(target) => target,
            Err(err) => {
                let _ = reply.send(Err(err));
                return;
            }
        };
        let chains = Arc::clone(&self.chains);
        let notices = self.notices_tx.clone();
        let session = self.session;
        let wallet = request.wallet;
        tokio::spawn(async move {
            let result = chains[chain].prepare(&transfer).await;
            let quoted = Quoted {
                session,
                wallet,
                chain,
                transfer,
                result,
                reply,
            };
            let _ = notices.send(Notice::Quoted(Box::new(quoted))).await;
        });
    }

    fn send_target(&self, request: &SendRequest) -> Result<(usize, Transfer), EngineError> {
        if !self.unlocked() {
            return Err(KeyringError::Locked.into());
        }
        let chain = self
            .chain_index(request.asset.network())
            .ok_or(ChainError::Unsupported("sending in this version"))?;
        let family = self.chains[chain].family();
        let from = self
            .accounts
            .iter()
            .find(|account| account.wallet == request.wallet && account.family == family)
            .ok_or(KeyringError::UnknownWallet)?
            .address;
        if request.asset.contract() == Some(&request.to) {
            return Err(ChainError::Invalid(
                "that is the token's own contract address, tokens sent there are lost",
            )
            .into());
        }
        Ok((
            chain,
            Transfer {
                account: 0,
                from,
                to: request.to,
                asset: request.asset.clone(),
                amount: request.amount,
            },
        ))
    }

    pub(super) fn quoted(&mut self, quoted: Quoted) {
        let Quoted {
            session,
            wallet,
            chain,
            transfer,
            result,
            reply,
        } = quoted;
        if session != self.session || !self.unlocked() {
            let _ = reply.send(Err(KeyringError::Locked.into()));
            return;
        }
        let (plan, fee) = match result {
            Ok(quote) => quote,
            Err(err) => {
                let _ = reply.send(Err(err.into()));
                return;
            }
        };
        let now = Instant::now();
        self.drafts.retain(|_, draft| draft.expires > now);
        while self.drafts.len() >= MAX_DRAFTS {
            self.drafts.pop_first();
        }
        self.next_draft = self.next_draft.wrapping_add(1);
        let id = DraftId(self.next_draft);
        let prepared = Prepared {
            wallet,
            chain,
            transfer: transfer.clone(),
            fee: fee.clone(),
            plan,
            expires: now + self.draft_ttl,
        };
        self.drafts.insert(id, prepared);
        let _ = reply.send(Ok(Draft {
            id,
            transfer,
            fee,
            valid_for: self.draft_ttl,
        }));
    }

    pub(super) async fn confirm_send(
        &mut self,
        id: DraftId,
        password: SecretString,
        reply: Reply<SentTx>,
    ) {
        match self.sign_draft(id, password).await {
            Ok((chain, record, signed)) => self.deliver(chain, record, signed, reply),
            Err(err) => {
                let _ = reply.send(Err(err));
            }
        }
    }

    async fn sign_draft(
        &mut self,
        id: DraftId,
        password: SecretString,
    ) -> Result<(usize, SentTx, Signed), EngineError> {
        let draft = self.drafts.remove(&id).ok_or(EngineError::UnknownDraft)?;
        if Instant::now() >= draft.expires {
            return Err(EngineError::DraftExpired);
        }
        let chains = Arc::clone(&self.chains);
        let (wallet, chain) = (draft.wallet, draft.chain);
        let (plan, transfer) = (draft.plan.clone(), draft.transfer.clone());
        let signed = self
            .keyring(move |keyring| {
                keyring.check_password(password.expose_secret().as_bytes())?;
                let signed = chains[chain].sign(&plan, keyring.seed(wallet)?)?;
                let decoded = chains[chain].decode(&signed)?;
                check_signed(&transfer, &decoded)?;
                Ok::<_, EngineError>((signed, decoded))
            })
            .await;
        let (signed, decoded) = match signed {
            Err(EngineError::Keyring(KeyringError::WrongPassword)) => {
                self.drafts.insert(id, draft);
                return Err(KeyringError::WrongPassword.into());
            }
            other => other?,
        };
        let network = self.chains[chain].network().id.clone();
        let reused = decoded.sequence.is_some()
            && self.history.iter().any(|tx| {
                tx.status.is_open()
                    && tx.network == network
                    && tx.from == decoded.from
                    && tx.sequence == decoded.sequence
            });
        if reused {
            return Err(EngineError::StillSending);
        }
        let record = SentTx {
            id: signed.id.to_string(),
            network,
            from: decoded.from,
            to: decoded.to,
            asset: decoded.asset,
            amount: decoded.amount,
            sequence: decoded.sequence,
            fee_max: draft.fee.max(),
            fee_paid: None,
            block: None,
            created_at: chains::now(),
            status: SentStatus::Sending,
            raw: Some(to_hex(&signed.bytes)),
        };
        self.save(record.clone())?;
        Ok((chain, record, signed))
    }

    fn deliver(&mut self, chain: usize, record: SentTx, signed: Signed, reply: Reply<SentTx>) {
        self.broadcasting.insert(record.id.clone());
        let chains = Arc::clone(&self.chains);
        let notices = self.notices_tx.clone();
        tokio::spawn(async move {
            let delivery = deliver(&chains[chain], &signed).await;
            let delivered = Delivered {
                record,
                delivery,
                reply,
            };
            let _ = notices.send(Notice::Delivered(Box::new(delivered))).await;
        });
    }

    pub(super) fn delivered(&mut self, delivered: Delivered) {
        let Delivered {
            mut record,
            delivery,
            reply,
        } = delivered;
        self.broadcasting.remove(&record.id);
        let refused = match delivery {
            Delivery::Accepted => {
                record.status = SentStatus::Pending;
                None
            }
            Delivery::Refused(reason) => {
                record.status = SentStatus::Failed(reason.clone());
                record.raw = None;
                Some(reason)
            }
            Delivery::Unknown => None,
        };
        let saved = self.save(record.clone());
        let _ = reply.send(match (refused, saved) {
            (Some(reason), _) => Err(ChainError::Rejected(reason).into()),
            (None, Err(err)) => Err(err),
            (None, Ok(())) => Ok(record),
        });
        self.schedule_tracking();
    }

    pub(super) fn history(&self) -> Result<Vec<SentTx>, EngineError> {
        if self.unlocked() {
            Ok(self.history.clone())
        } else {
            Err(KeyringError::Locked.into())
        }
    }

    pub(super) fn start_tracking(&mut self) {
        self.next_track = None;
        if !self.unlocked() || self.tracking.is_some() {
            return;
        }
        let open = self.trackable();
        if open.is_empty() {
            return;
        }
        let chains = Arc::clone(&self.chains);
        let notices = self.notices_tx.clone();
        self.tracking = Some(tokio::spawn(async move {
            let now = chains::now();
            let jobs = open
                .iter()
                .map(|(chain, tx)| track(&chains[*chain], tx, now));
            let updates = join_all(jobs).await.into_iter().flatten().collect();
            let _ = notices.send(Notice::Tracked(updates)).await;
        }));
    }

    pub(super) fn tracked(&mut self, updates: Vec<SentTx>) {
        self.tracking = None;
        let mut settled = false;
        for update in updates {
            let still_open = self
                .history
                .iter()
                .find(|tx| tx.network == update.network && tx.id == update.id)
                .is_some_and(|tx| tx.status.is_open());
            if still_open && !self.broadcasting.contains(&update.id) {
                settled |= !update.status.is_open();
                let _ = self.save(update);
            }
        }
        if settled {
            self.restart_refresh();
        }
        self.schedule_tracking();
    }

    pub(super) fn schedule_tracking(&mut self) {
        let waiting = self.tracking.is_none() && self.next_track.is_none();
        if self.unlocked() && waiting && !self.trackable().is_empty() {
            self.next_track = Some(Instant::now() + self.track_every);
        }
    }

    pub(super) fn stop_tracking(&mut self) {
        if let Some(task) = self.tracking.take() {
            task.abort();
        }
        self.next_track = None;
    }

    fn trackable(&self) -> Vec<(usize, SentTx)> {
        self.history
            .iter()
            .filter(|tx| tx.status.is_open() && !self.broadcasting.contains(&tx.id))
            .filter_map(|tx| Some((self.chain_index(&tx.network)?, tx.clone())))
            .collect()
    }

    fn chain_index(&self, network: &NetworkId) -> Option<usize> {
        self.chains
            .iter()
            .position(|chain| &chain.network().id == network)
    }

    fn save(&mut self, record: SentTx) -> Result<(), EngineError> {
        self.store.save_sent(&record)?;
        match self
            .history
            .iter_mut()
            .find(|tx| tx.network == record.network && tx.id == record.id)
        {
            Some(slot) => *slot = record.clone(),
            None => self.history.insert(0, record.clone()),
        }
        let _ = self.events.send(Event::TxUpdated(Box::new(record)));
        Ok(())
    }
}

async fn deliver(chain: &AnyChain, signed: &Signed) -> Delivery {
    match chain.broadcast(signed).await {
        Ok(_) => Delivery::Accepted,
        Err(ChainError::Rejected(reason)) => match chain.status(&signed.id).await {
            Ok(TxStatus::Pending | TxStatus::Confirmed { .. }) => Delivery::Accepted,
            Ok(_) => Delivery::Refused(failure(&reason)),
            Err(_) => Delivery::Unknown,
        },
        Err(_) => Delivery::Unknown,
    }
}

async fn track(chain: &AnyChain, tx: &SentTx, now: u64) -> Option<SentTx> {
    let id = TxId::new(tx.id.clone()).ok()?;
    let status = chain.status(&id).await.ok()?;
    match step(tx, status, now) {
        Step::Keep => None,
        Step::Update(next) => Some(*next),
        Step::Rebroadcast(bytes) => {
            chain.broadcast(&Signed { id, bytes }).await.ok()?;
            (tx.status != SentStatus::Pending).then(|| SentTx {
                status: SentStatus::Pending,
                ..tx.clone()
            })
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Step {
    Keep,
    Update(Box<SentTx>),
    Rebroadcast(Vec<u8>),
}

fn step(tx: &SentTx, status: TxStatus, now: u64) -> Step {
    let mut next = tx.clone();
    match status {
        TxStatus::Confirmed { block, fee } => {
            next.status = SentStatus::Confirmed;
            next.block = block;
            next.fee_paid = fee;
            next.raw = None;
        }
        TxStatus::Failed(reason) => {
            next.status = SentStatus::Failed(failure(&reason));
            next.raw = None;
        }
        TxStatus::Pending => next.status = SentStatus::Pending,
        TxStatus::NotFound if now.saturating_sub(tx.created_at) >= DROP_AFTER_SECS => {
            next.status = SentStatus::Dropped;
            next.raw = None;
        }
        TxStatus::NotFound => {
            return match tx.raw.as_deref().and_then(from_hex) {
                Some(bytes) => Step::Rebroadcast(bytes),
                None => Step::Keep,
            };
        }
    }
    if next == *tx {
        Step::Keep
    } else {
        Step::Update(Box::new(next))
    }
}

fn failure(reason: &str) -> String {
    let reason = to_plain_text(reason, MAX_REASON);
    if reason.is_empty() {
        "the network gave no reason".to_owned()
    } else {
        reason
    }
}

fn to_hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(text.get(at..at + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(status: SentStatus, created_at: u64) -> SentTx {
        let address = Address::Evm([1; 20]);
        SentTx {
            id: format!("0x{}", "ab".repeat(32)),
            network: "bsc".parse().unwrap(),
            from: address,
            to: address,
            asset: "bsc".parse().unwrap(),
            amount: Amount::parse("1", 18).unwrap(),
            sequence: Some(0),
            fee_max: Amount::parse("0.001", 18).unwrap(),
            fee_paid: None,
            block: None,
            created_at,
            status,
            raw: Some("02f801".to_owned()),
        }
    }

    #[test]
    fn a_confirmation_settles_the_record_and_forgets_the_bytes() {
        let fee = Amount::parse("0.000021", 18).unwrap();
        let confirmed = TxStatus::Confirmed {
            block: Some(9),
            fee: Some(fee),
        };
        let Step::Update(next) = step(&open(SentStatus::Pending, 100), confirmed, 120) else {
            panic!("expected an update");
        };
        assert_eq!(next.status, SentStatus::Confirmed);
        assert_eq!(
            (next.block, next.fee_paid, next.raw),
            (Some(9), Some(fee), None)
        );
    }

    #[test]
    fn failures_are_recorded_as_plain_text() {
        let reverted = TxStatus::Failed("reverted\u{202E}\n".to_owned());
        let Step::Update(next) = step(&open(SentStatus::Pending, 100), reverted, 120) else {
            panic!("expected an update");
        };
        assert_eq!(next.status, SentStatus::Failed("reverted".to_owned()));
        assert_eq!(next.raw, None);
        assert_eq!(failure("\u{200B}"), "the network gave no reason");
    }

    #[test]
    fn a_node_that_lost_the_transaction_gets_it_again_until_it_is_too_old() {
        let sending = open(SentStatus::Sending, 1_000);
        assert_eq!(
            step(&sending, TxStatus::NotFound, 1_000 + DROP_AFTER_SECS - 1),
            Step::Rebroadcast(vec![0x02, 0xf8, 0x01])
        );
        let Step::Update(dropped) = step(&sending, TxStatus::NotFound, 1_000 + DROP_AFTER_SECS)
        else {
            panic!("expected an update");
        };
        assert_eq!((dropped.status, dropped.raw), (SentStatus::Dropped, None));
    }

    #[test]
    fn pending_moves_forward_only_once() {
        let Step::Update(next) = step(&open(SentStatus::Sending, 1), TxStatus::Pending, 2) else {
            panic!("expected an update");
        };
        assert_eq!(next.status, SentStatus::Pending);
        assert_eq!(step(&next, TxStatus::Pending, 3), Step::Keep);
    }

    #[test]
    fn signed_bytes_survive_the_hex_round_trip() {
        let bytes = vec![0x00, 0x02, 0xf8, 0xff, 0x7a];
        assert_eq!(to_hex(&bytes), "0002f8ff7a");
        assert_eq!(from_hex(&to_hex(&bytes)), Some(bytes));
        for bad in ["0", "zz", "0x02", "02f"] {
            assert_eq!(from_hex(bad), None, "{bad}");
        }
        assert_eq!(from_hex(""), Some(Vec::new()));
    }
}
