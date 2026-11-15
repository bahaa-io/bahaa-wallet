use std::sync::Arc;
use std::time::Duration;

use bahaa_chain::{ChainError, FeeQuote, MAX_MEMO, Signed, now};
use bahaa_connect::{
    AppRequest, Call, Code, Content, Incoming, JettonItem, Message, NftItem, Outgoing, Transaction,
    TransferItem, acknowledged, answer, data_hash, failure, read_request, schema_crc, sent,
    signed_data, signed_message,
};
use bahaa_crypto::{ExposeSecret, SecretString};
use bahaa_store::{Connection, SentKind, SentStatus, SentTx};
use bahaa_ton::{
    DecodedBatch, Handover, Meaning, Outbound, Reading, Requested, TonBatch, Wanted, cell_data_hash,
};
use bahaa_types::{Address, Amount, AssetId, ChainFamily, NftId, TonContract, hex, is_plain_text};
use ruint::aliases::U256;
use tokio::sync::oneshot;
use tokio::time::Instant;

use super::{
    Approved, Ask, AskId, AskReview, DataReview, Rehearsed, Reviewed, TxReview, global_id, post,
};
use crate::chains::{self, AnyChain};
use crate::engine::{EngineError, Notice, Reply, Worker};
use crate::{KeyringError, WalletId};

const MAX_ASKS: usize = 16;
const ASK_LIFE: u64 = 300;

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

impl Worker {
    pub(in crate::engine) fn bridged(&mut self, incoming: Incoming) {
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

    pub(super) fn app_account(&self, record: &Connection) -> Option<(WalletId, u32)> {
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

    pub(in crate::engine) fn review_ask(&mut self, id: AskId, reply: Reply<AskReview>) {
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

    pub(in crate::engine) fn rehearsed(&mut self, rehearsed: Rehearsed) {
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

    pub(in crate::engine) async fn approve_ask(
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

    pub(in crate::engine) fn reject_ask(&mut self, id: AskId) -> Result<(), EngineError> {
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

    pub(in crate::engine) fn expire_links(&mut self) {
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

    pub(super) fn schedule_expiry(&mut self) {
        let (seconds, moment) = (now(), Instant::now());
        let asks = self
            .links
            .asks
            .iter()
            .map(|ask| moment + Duration::from_secs(ask.expires_at.saturating_sub(seconds)));
        let invites = self.links.invites.values().map(|invited| invited.expires);
        self.links.next_expiry = asks.chain(invites).min();
    }

    pub(super) fn refuse(&self, ask: &Ask, reason: &str) {
        if let Some(index) = self.links.position(&ask.session) {
            self.tell_session(
                index,
                failure(ask.request, Code::Declined, reason),
                ask.trace.clone(),
            );
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
}
