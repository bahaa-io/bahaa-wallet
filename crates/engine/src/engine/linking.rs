use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bahaa_chain::{ChainError, FeeQuote, Link, NameRecord, Signed, check_call};
use bahaa_config::Network;
use bahaa_crypto::{ExposeSecret, SecretString};
use bahaa_store::{SentKind, SentTx};
use bahaa_types::{Address, Amount, AssetId, ChainFamily, NetworkId};
use tokio::time::Instant;

use super::connect::{batch_sent, signed_batch};
use super::names::{PAUSED_ENDING, SNS_ENDING, TON_ENDINGS};
use super::send::call_record;
use super::{EngineError, Notice, Reply, Worker};
use crate::chains::{self, AnyCall, AnyChain, LinkPlan, Linked};
use crate::{KeyringError, WalletId};

const MAX_LINKS: usize = 8;
const NOT_REGISTERED: &str = "this name isn't registered";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LinkId(u64);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkRequest {
    pub wallet: WalletId,
    pub account: u32,
    pub network: NetworkId,
    pub name: String,
    pub link: Link,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkDraft {
    pub id: LinkId,
    pub network: NetworkId,
    pub from: Address,
    pub record: NameRecord,
    pub link: Link,
    pub attach: Option<Amount>,
    pub returned: bool,
    pub fee: FeeQuote,
    pub calls: usize,
    pub valid_for: Duration,
}

struct Prepared {
    draft: LinkDraft,
    wallet: WalletId,
    chain: usize,
    plan: LinkPlan,
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
    link: Link,
    result: Result<Linked, ChainError>,
    reply: Reply<LinkDraft>,
}

enum Signing {
    Ton(Box<(Signed, bahaa_ton::DecodedBatch)>),
    Evm(Vec<(Signed, bahaa_chain::DecodedCall, Amount)>),
}

impl Worker {
    pub(super) fn name_record(&self, asset: &AssetId, reply: Reply<NameRecord>) {
        let AssetId::Nft(network, id) = asset else {
            let _ = reply.send(Err(ChainError::Invalid("only NFT names can be read").into()));
            return;
        };
        let Some(chain) = self.chain_index(network) else {
            let _ = reply.send(Err(EngineError::UnknownNetwork));
            return;
        };
        let item = *id.address();
        let chains = Arc::clone(&self.chains);
        tokio::spawn(async move {
            let result = chains[chain].name_record(&item).await;
            let _ = reply.send(result.map_err(EngineError::from));
        });
    }

    pub(super) fn find_name(&self, name: &str, reply: Reply<NameRecord>) {
        let homes = match self.name_homes(name) {
            Ok(homes) => homes,
            Err(err) => {
                let _ = reply.send(Err(err));
                return;
            }
        };
        let (chains, covered, name) = (Arc::clone(&self.chains), self.covered(), name.to_owned());
        tokio::spawn(async move {
            let _ = reply.send(found(&chains, &homes, &name, &covered).await);
        });
    }

    pub(super) fn prepare_link(&self, request: &LinkRequest, reply: Reply<LinkDraft>) {
        let (chain, from) = match self.spending(request.wallet, request.account, &request.network) {
            Ok(found) => found,
            Err(err) => {
                let _ = reply.send(Err(err));
                return;
            }
        };
        let chains = Arc::clone(&self.chains);
        let notices = self.notices_tx.clone();
        let covered = self.covered();
        let (session, wallet, account, link) =
            (self.session, request.wallet, request.account, request.link);
        let name = request.name.clone();
        tokio::spawn(async move {
            let result = chains[chain]
                .prepare_link((account, &from), &name, link, &covered)
                .await;
            let planned = Planned {
                session,
                wallet,
                chain,
                from,
                link,
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
        let linked = match planned.result {
            Ok(linked) => linked,
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
        let calls = match &linked.plan {
            LinkPlan::Ton(_) => 1,
            LinkPlan::Evm(calls) => calls.len(),
        };
        let draft = LinkDraft {
            id,
            network: self.chains[planned.chain].network().id.clone(),
            from: planned.from,
            record: linked.record,
            link: planned.link,
            attach: linked.attach,
            returned: linked.returned,
            fee: linked.fee,
            calls,
            valid_for: self.draft_ttl,
        };
        self.naming.drafts.insert(
            id,
            Prepared {
                draft: draft.clone(),
                wallet: planned.wallet,
                chain: planned.chain,
                plan: linked.plan,
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
            Ok((chain, batch)) => self.deliver(chain, batch, reply, None),
            Err(err) => {
                let _ = reply.send(Err(err));
            }
        }
    }

    async fn signed_link(
        &mut self,
        id: LinkId,
        password: SecretString,
    ) -> Result<(usize, Vec<(SentTx, Signed)>), EngineError> {
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
        let (wallet, chain, plan) = (prepared.wallet, prepared.chain, prepared.plan.clone());
        let chains = Arc::clone(&self.chains);
        let signing = self
            .keyring(move |keyring| {
                keyring.check_password(password.expose_secret().as_bytes())?;
                let seed = keyring.seed(wallet)?;
                let chain = &chains[chain];
                match &plan {
                    LinkPlan::Ton(batch) => {
                        let ton = chain.ton().ok_or(EngineError::UnknownNetwork)?;
                        let signed = signed_batch(ton, batch, seed)?;
                        Ok(Signing::Ton(Box::new(signed)))
                    }
                    LinkPlan::Evm(calls) => calls
                        .iter()
                        .map(|call| {
                            let call = AnyCall::Evm(call.clone());
                            let signed = chain.sign_call(&call, seed)?;
                            let decoded = chain.decode_call(&signed)?;
                            check_call(call.planned(), &decoded)?;
                            Ok((signed, decoded, call.fee_max()))
                        })
                        .collect::<Result<Vec<_>, EngineError>>()
                        .map(Signing::Evm),
                }
            })
            .await?;
        let prepared = self
            .naming
            .drafts
            .remove(&id)
            .ok_or(EngineError::UnknownDraft)?;
        let batch = self.link_records(&prepared, signing)?;
        for (record, _) in &batch {
            self.save(record.clone())?;
        }
        Ok((chain, batch))
    }

    fn link_records(
        &self,
        prepared: &Prepared,
        signing: Signing,
    ) -> Result<Vec<(SentTx, Signed)>, EngineError> {
        let draft = &prepared.draft;
        let name = draft.record.name.clone();
        let batch = match (signing, &prepared.plan) {
            (Signing::Ton(signed), LinkPlan::Ton(batch)) => {
                let (signed, decoded) = *signed;
                let tx = batch_sent(
                    &draft.network,
                    (SentKind::Name, name),
                    batch.readings(),
                    draft.fee.max(),
                    (&signed, &decoded),
                )?;
                vec![(tx, signed)]
            }
            (Signing::Evm(calls), LinkPlan::Evm(_)) => calls
                .into_iter()
                .map(|(signed, decoded, fee_max)| {
                    let moved = (
                        SentKind::Name,
                        decoded.call.to,
                        AssetId::Native(draft.network.clone()),
                        decoded.call.value,
                    );
                    let tx = SentTx {
                        private: false,
                        name: Some(name.clone()),
                        ..call_record(&draft.network, (&signed, decoded), moved, fee_max)
                    };
                    (tx, signed)
                })
                .collect(),
            _ => return Err(ChainError::Mismatch("the network").into()),
        };
        if batch
            .iter()
            .any(|(tx, _)| self.reused(&tx.network, &tx.from, tx.sequence))
        {
            return Err(EngineError::StillSending);
        }
        Ok(batch)
    }

    fn name_homes(&self, name: &str) -> Result<Vec<usize>, EngineError> {
        let lower = name.trim().to_lowercase();
        if TON_ENDINGS.iter().any(|ending| lower.ends_with(ending)) {
            return self
                .chains
                .iter()
                .position(|chain| chain.family() == ChainFamily::Ton && !chain.network().testnet)
                .map(|ton| vec![ton])
                .ok_or(EngineError::UnknownNetwork);
        }
        if lower.contains('@') || lower.ends_with(SNS_ENDING) || lower.ends_with(PAUSED_ENDING) {
            return Err(ChainError::Unsupported("changing this kind of name yet").into());
        }
        let homes: Vec<usize> = bahaa_evm::name_homes(&lower)
            .iter()
            .filter_map(|id| chains::evm_chain(&self.chains, *id))
            .collect();
        if homes.is_empty() {
            return Err(EngineError::UnknownNetwork);
        }
        Ok(homes)
    }

    fn covered(&self) -> Vec<Network> {
        self.chains
            .iter()
            .map(AnyChain::network)
            .filter(|network| network.family() == ChainFamily::Evm && !network.testnet)
            .cloned()
            .collect()
    }
}

async fn found(
    chains: &[AnyChain],
    homes: &[usize],
    name: &str,
    covered: &[Network],
) -> Result<NameRecord, EngineError> {
    for home in homes {
        if let Some(record) = chains[*home].find_name(name, covered).await? {
            return Ok(record);
        }
    }
    Err(ChainError::Invalid(NOT_REGISTERED).into())
}
