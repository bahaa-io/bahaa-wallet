use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bahaa_chain::{ChainError, FeeQuote};
use bahaa_crypto::{ExposeSecret, SecretString};
use bahaa_store::{SentKind, SentTx};
use bahaa_ton::{Link, Linking, NameRecord, TonBatch};
use bahaa_types::{Address, Amount, AssetId, NetworkId};
use tokio::time::Instant;

use super::connect::{batch_sent, signed_batch};
use super::{EngineError, Notice, Reply, Worker};
use crate::{KeyringError, WalletId};

const MAX_LINKS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LinkId(u64);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkRequest {
    pub wallet: WalletId,
    pub account: u32,
    pub asset: AssetId,
    pub link: Link,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkDraft {
    pub id: LinkId,
    pub network: NetworkId,
    pub from: Address,
    pub record: NameRecord,
    pub link: Link,
    pub attach: Amount,
    pub returned: bool,
    pub fee: FeeQuote,
    pub valid_for: Duration,
}

struct Prepared {
    draft: LinkDraft,
    wallet: WalletId,
    chain: usize,
    batch: TonBatch,
    expires: Instant,
}

#[derive(Default)]
pub(super) struct Naming {
    drafts: BTreeMap<LinkId, Prepared>,
    next: u64,
}

impl Naming {
    pub(super) fn forget(&mut self) {
        self.drafts.clear();
    }
}

pub(super) struct Planned {
    session: u64,
    wallet: WalletId,
    chain: usize,
    from: Address,
    result: Result<Linking, ChainError>,
    reply: Reply<LinkDraft>,
}

impl Worker {
    pub(super) fn name_record(&self, asset: &AssetId, reply: Reply<NameRecord>) {
        let (chain, item) = match self.name_item(asset) {
            Ok(found) => found,
            Err(err) => {
                let _ = reply.send(Err(err));
                return;
            }
        };
        let chains = Arc::clone(&self.chains);
        tokio::spawn(async move {
            let result = chains[chain].name_record(&item).await;
            let _ = reply.send(result.map_err(EngineError::from));
        });
    }

    pub(super) fn prepare_link(&self, request: &LinkRequest, reply: Reply<LinkDraft>) {
        let target = self.name_item(&request.asset).and_then(|(chain, item)| {
            let (spending, from) =
                self.spending(request.wallet, request.account, request.asset.network())?;
            if spending == chain {
                Ok((chain, item, from))
            } else {
                Err(EngineError::UnknownNetwork)
            }
        });
        let (chain, item, from) = match target {
            Ok(found) => found,
            Err(err) => {
                let _ = reply.send(Err(err));
                return;
            }
        };
        let chains = Arc::clone(&self.chains);
        let notices = self.notices_tx.clone();
        let (session, wallet, account, link) =
            (self.session, request.wallet, request.account, request.link);
        tokio::spawn(async move {
            let result = chains[chain]
                .prepare_link(account, &from, &item, link)
                .await;
            let planned = Planned {
                session,
                wallet,
                chain,
                from,
                result,
                reply,
            };
            let _ = notices.send(Notice::LinkPlanned(Box::new(planned))).await;
        });
    }

    pub(super) fn link_planned(&mut self, planned: Planned) {
        if planned.session != self.session || !self.unlocked() {
            let _ = planned.reply.send(Err(KeyringError::Locked.into()));
            return;
        }
        let linking = match planned.result {
            Ok(linking) => linking,
            Err(err) => {
                let _ = planned.reply.send(Err(err.into()));
                return;
            }
        };
        let now = Instant::now();
        self.naming.drafts.retain(|_, kept| kept.expires > now);
        while self.naming.drafts.len() >= MAX_LINKS {
            self.naming.drafts.pop_first();
        }
        self.naming.next += 1;
        let id = LinkId(self.naming.next);
        let draft = LinkDraft {
            id,
            network: self.chains[planned.chain].network().id.clone(),
            from: planned.from,
            record: linking.record,
            link: linking.link,
            attach: linking.attach,
            returned: linking.returned,
            fee: linking.fee,
            valid_for: self.draft_ttl,
        };
        self.naming.drafts.insert(
            id,
            Prepared {
                draft: draft.clone(),
                wallet: planned.wallet,
                chain: planned.chain,
                batch: linking.batch,
                expires: now + self.draft_ttl,
            },
        );
        let _ = planned.reply.send(Ok(draft));
    }

    pub(super) async fn confirm_link(
        &mut self,
        id: LinkId,
        password: SecretString,
        reply: Reply<SentTx>,
    ) {
        match self.signed_link(id, password).await {
            Ok((chain, tx, signed)) => self.deliver(chain, vec![(tx, signed)], reply, None),
            Err(err) => {
                let _ = reply.send(Err(err));
            }
        }
    }

    async fn signed_link(
        &mut self,
        id: LinkId,
        password: SecretString,
    ) -> Result<(usize, SentTx, bahaa_chain::Signed), EngineError> {
        self.ensure_unlocked()?;
        let prepared = self
            .naming
            .drafts
            .get(&id)
            .ok_or(EngineError::UnknownDraft)?;
        if prepared.expires <= Instant::now() {
            self.naming.drafts.remove(&id);
            return Err(EngineError::DraftExpired);
        }
        let (wallet, chain, batch) = (prepared.wallet, prepared.chain, prepared.batch.clone());
        let chains = Arc::clone(&self.chains);
        let (signed, decoded) = self
            .keyring(move |keyring| {
                keyring.check_password(password.expose_secret().as_bytes())?;
                let seed = keyring.seed(wallet)?;
                let ton = chains[chain].ton().ok_or(EngineError::UnknownNetwork)?;
                signed_batch(ton, &batch, seed)
            })
            .await?;
        let prepared = self
            .naming
            .drafts
            .remove(&id)
            .ok_or(EngineError::UnknownDraft)?;
        let draft = &prepared.draft;
        let tx = batch_sent(
            &draft.network,
            (SentKind::Name, draft.record.name.clone()),
            prepared.batch.readings(),
            draft.fee.max(),
            (&signed, &decoded),
        )?;
        if self.reused(&tx.network, &tx.from, tx.sequence) {
            return Err(EngineError::StillSending);
        }
        self.save(tx.clone())?;
        Ok((chain, tx, signed))
    }

    fn name_item(&self, asset: &AssetId) -> Result<(usize, Address), EngineError> {
        let AssetId::Nft(network, id) = asset else {
            return Err(ChainError::Invalid("only NFT names can be linked").into());
        };
        let chain = self
            .chain_index(network)
            .ok_or(EngineError::UnknownNetwork)?;
        Ok((chain, *id.address()))
    }
}
