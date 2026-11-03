use std::sync::Arc;
use std::time::Duration;

use bahaa_chain::{ChainError, FeeQuote, MAX_MEMO, Transfer};
use bahaa_store::{SentTx, SwapRecord, SwapStatus};
use bahaa_swap::{
    Charged, Comment, Cost, CostKind, OneClick, Order, Progress, Provider, Providers, QUOTE_LIFE,
    Ranked, Relay, Request, Route, SwapError, rank,
};
use bahaa_types::{Address, Amount, AssetId, is_plain_text};
use futures_util::future::{join, join_all};
use tokio::time::Instant;

use super::send::NewDraft;
use super::{Draft, EngineError, Event, Notice, Reply, Worker};
use crate::chains::AnyPlan;
use crate::portfolio;
use crate::{KeyringError, WalletId};

const MAX_SWAP_DRAFTS: usize = 8;
const DEPOSIT_WINDOW: u64 = 30 * 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SwapId(u64);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwapRequest {
    pub wallet: WalletId,
    pub account: u32,
    pub sell: AssetId,
    pub buy: AssetId,
    pub amount: Amount,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unavailable {
    pub provider: &'static str,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwapDraft {
    pub id: SwapId,
    pub routes: Vec<Ranked>,
    pub unavailable: Vec<Unavailable>,
    pub fee: Option<FeeQuote>,
    pub valid_for: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwapReview {
    pub draft: Draft,
    pub route: Ranked,
    pub order: Order,
}

pub(crate) struct Swappers {
    oneclick: OneClick,
    relay: Relay,
}

impl Swappers {
    pub(crate) fn new(providers: &Providers) -> Result<Self, EngineError> {
        let client = bahaa_chain::http::client()?;
        Ok(Self {
            oneclick: OneClick::new(&providers.oneclick, client.clone())?,
            relay: Relay::new(&providers.relay, client),
        })
    }

    async fn quotes(&self, request: &Request, now: u64) -> (Vec<Quote>, Vec<Unavailable>) {
        let oneclick = async {
            self.oneclick.covers(request).then_some(())?;
            Some(self.oneclick.quote(request, false, now).await)
        };
        let relay = async {
            self.relay.covers(request).then_some(())?;
            Some(
                self.relay
                    .quote(request, now)
                    .await
                    .map(|(route, order)| (route, Some(order))),
            )
        };
        let (oneclick, relay) = join(oneclick, relay).await;
        let mut quotes = Vec::new();
        let mut unavailable = Vec::new();
        for (provider, answer) in [(Provider::OneClick, oneclick), (Provider::Relay, relay)] {
            match answer {
                Some(Ok((route, order))) => quotes.push((provider, route, order)),
                Some(Err(err)) => unavailable.push(Unavailable {
                    provider: provider.name(),
                    reason: err.to_string(),
                }),
                None => {}
            }
        }
        (quotes, unavailable)
    }

    async fn order(&self, picked: &Picked, now: u64) -> Result<(Route, Order), SwapError> {
        if let Some(order) = picked.order.clone().filter(|order| order.expires_at > now) {
            return Ok((picked.route.route.clone(), order));
        }
        match picked.provider {
            Provider::OneClick => {
                let (route, order) = self.oneclick.quote(&picked.request, true, now).await?;
                let order =
                    order.ok_or(SwapError::Untrusted("the order has no deposit address"))?;
                Ok((route, order))
            }
            Provider::Relay => self.relay.quote(&picked.request, now).await,
        }
    }

    async fn status(&self, provider: Provider, reference: &str) -> Result<Progress, SwapError> {
        match provider {
            Provider::OneClick => self.oneclick.status(reference).await,
            Provider::Relay => self.relay.status(reference).await,
        }
    }
}

type Quote = (Provider, Route, Option<Order>);

pub(super) struct PreparedSwap {
    wallet: WalletId,
    account: u32,
    chain: usize,
    request: Request,
    routes: Vec<(Provider, Ranked, Option<Order>)>,
    expires: Instant,
}

struct Picked {
    wallet: WalletId,
    account: u32,
    chain: usize,
    request: Request,
    provider: Provider,
    route: Ranked,
    order: Option<Order>,
}

pub(super) struct SwapLink {
    request: Request,
    order: Order,
}

pub(super) struct SwapQuoted {
    session: u64,
    wallet: WalletId,
    account: u32,
    chain: usize,
    request: Request,
    quotes: Vec<Quote>,
    unavailable: Vec<Unavailable>,
    fee: Option<FeeQuote>,
    reply: Reply<SwapDraft>,
}

pub(super) struct SwapOrdered {
    session: u64,
    wallet: WalletId,
    chain: usize,
    request: Request,
    result: Result<(Route, Order, AnyPlan, Transfer, FeeQuote), EngineError>,
    reply: Reply<SwapReview>,
}

fn network_fee(route: &mut Route, fee: &FeeQuote) {
    route.costs.retain(|cost| cost.kind != CostKind::NetworkFee);
    route.costs.push(Cost {
        kind: CostKind::NetworkFee,
        asset: AssetId::Native(route.sell.network().clone()),
        amount: fee.expected(),
        charged: Charged::Extra,
    });
}

fn memo(order: &Order) -> Result<Option<String>, EngineError> {
    let text = match &order.comment {
        None => return Ok(None),
        Some(Comment::Text(text)) => Some(text.clone()),
        Some(Comment::TonBody(body)) => bahaa_ton::read_comment(body),
    };
    text.filter(|text| is_plain_text(text, MAX_MEMO))
        .map(Some)
        .ok_or(EngineError::Swap(SwapError::Untrusted(
            "the provider's deposit comment is not plain text",
        )))
}

fn stored(progress: Progress) -> SwapStatus {
    match progress {
        Progress::Waiting => SwapStatus::Waiting,
        Progress::Received => SwapStatus::Received,
        Progress::Swapping => SwapStatus::Swapping,
        Progress::Done => SwapStatus::Done,
        Progress::Refunded => SwapStatus::Refunded,
        Progress::Failed(reason) => SwapStatus::Failed(reason),
    }
}

impl Worker {
    pub(super) fn prepare_swap(&mut self, request: &SwapRequest, reply: Reply<SwapDraft>) {
        let (chain, asked) = match self.swap_target(request) {
            Ok(target) => target,
            Err(err) => {
                let _ = reply.send(Err(err));
                return;
            }
        };
        let Some(swappers) = self.swappers.clone() else {
            let _ = reply.send(Err(SwapError::Unsupported.into()));
            return;
        };
        let chains = Arc::clone(&self.chains);
        let notices = self.notices_tx.clone();
        let (session, wallet, account) = (self.session, request.wallet, request.account);
        tokio::spawn(async move {
            let now = bahaa_chain::now();
            let (quotes, unavailable) = swappers.quotes(&asked, now).await;
            let target = quotes
                .iter()
                .find_map(|(_, _, order)| order.as_ref().map(|order| order.deposit))
                .unwrap_or(asked.from);
            let estimate = Transfer {
                account,
                from: asked.from,
                to: target,
                asset: asked.sell.clone(),
                amount: asked.amount,
                max: false,
                memo: None,
            };
            let fee = chains[chain]
                .prepare(&estimate)
                .await
                .ok()
                .map(|(_, _, fee)| fee);
            let found = SwapQuoted {
                session,
                wallet,
                account,
                chain,
                request: asked,
                quotes,
                unavailable,
                fee,
                reply,
            };
            let _ = notices.send(Notice::SwapQuoted(Box::new(found))).await;
        });
    }

    pub(super) fn swap_quoted(&mut self, arrived: SwapQuoted) {
        let SwapQuoted {
            session,
            wallet,
            account,
            chain,
            request,
            quotes,
            unavailable,
            fee,
            reply,
        } = arrived;
        if session != self.session || !self.unlocked() {
            let _ = reply.send(Err(KeyringError::Locked.into()));
            return;
        }
        let quotes: Vec<Quote> = quotes
            .into_iter()
            .map(|(provider, mut route, order)| {
                if let Some(fee) = &fee {
                    network_fee(&mut route, fee);
                }
                (provider, route, order)
            })
            .collect();
        let ranked = self.ranked(quotes.iter().map(|(_, route, _)| route.clone()).collect());
        let routes = ranked
            .into_iter()
            .filter_map(|ranked| {
                let (provider, _, order) =
                    quotes.iter().find(|(_, route, _)| *route == ranked.route)?;
                Some((*provider, ranked, order.clone()))
            })
            .collect::<Vec<_>>();
        let now = Instant::now();
        let life = Duration::from_secs(QUOTE_LIFE);
        self.swap_drafts.retain(|_, draft| draft.expires > now);
        while self.swap_drafts.len() >= MAX_SWAP_DRAFTS {
            self.swap_drafts.pop_first();
        }
        self.next_swap = self.next_swap.wrapping_add(1);
        let id = SwapId(self.next_swap);
        let draft = SwapDraft {
            id,
            routes: routes.iter().map(|(_, ranked, _)| ranked.clone()).collect(),
            unavailable,
            fee,
            valid_for: life,
        };
        self.swap_drafts.insert(
            id,
            PreparedSwap {
                wallet,
                account,
                chain,
                request,
                routes,
                expires: now + life,
            },
        );
        let _ = reply.send(Ok(draft));
    }

    pub(super) fn choose_swap(&mut self, id: SwapId, index: usize, reply: Reply<SwapReview>) {
        let picked = match self.picked(id, index) {
            Ok(picked) => picked,
            Err(err) => {
                let _ = reply.send(Err(err));
                return;
            }
        };
        let Some(swappers) = self.swappers.clone() else {
            let _ = reply.send(Err(SwapError::Unsupported.into()));
            return;
        };
        let chains = Arc::clone(&self.chains);
        let notices = self.notices_tx.clone();
        let session = self.session;
        tokio::spawn(async move {
            let result = async {
                let (route, order) = swappers.order(&picked, bahaa_chain::now()).await?;
                if order.amount != picked.request.amount {
                    return Err(SwapError::Untrusted("the order is for another amount").into());
                }
                let transfer = Transfer {
                    account: picked.account,
                    from: picked.request.from,
                    to: order.deposit,
                    asset: picked.request.sell.clone(),
                    amount: order.amount,
                    max: false,
                    memo: memo(&order)?,
                };
                let (plan, transfer, fee) = chains[picked.chain].prepare(&transfer).await?;
                Ok::<_, EngineError>((route, order, plan, transfer, fee))
            }
            .await;
            let ordered = SwapOrdered {
                session,
                wallet: picked.wallet,
                chain: picked.chain,
                request: picked.request,
                result,
                reply,
            };
            let _ = notices.send(Notice::SwapOrdered(Box::new(ordered))).await;
        });
    }

    pub(super) fn swap_ordered(&mut self, ordered: SwapOrdered) {
        let SwapOrdered {
            session,
            wallet,
            chain,
            request,
            result,
            reply,
        } = ordered;
        if session != self.session || !self.unlocked() {
            let _ = reply.send(Err(KeyringError::Locked.into()));
            return;
        }
        let (mut route, order, plan, transfer, fee) = match result {
            Ok(ordered) => ordered,
            Err(err) => {
                let _ = reply.send(Err(err));
                return;
            }
        };
        network_fee(&mut route, &fee);
        let left = order.expires_at.saturating_sub(bahaa_chain::now());
        let draft = self.add_draft(NewDraft {
            wallet,
            chain,
            plan,
            transfer,
            fee,
            replaces: None,
            swap: Some(SwapLink {
                request,
                order: order.clone(),
            }),
            until: Some(Instant::now() + Duration::from_secs(left)),
        });
        let mut ranked = self.ranked(vec![route]);
        let _ = reply.send(match ranked.pop() {
            Some(route) if route.refused.is_none() => Ok(SwapReview {
                draft,
                route,
                order,
            }),
            _ => Err(SwapError::Untrusted("the provider's final order is far below market").into()),
        });
    }

    pub(super) fn record_swap(&mut self, sent: &SentTx, link: SwapLink) {
        let SwapLink { request, order } = link;
        let now = bahaa_chain::now();
        let record = SwapRecord {
            id: sent.id.clone(),
            network: sent.network.clone(),
            provider: order.provider.key().to_owned(),
            reference: order.reference.clone(),
            from: request.from,
            to: request.to,
            deposit: order.deposit,
            sell: request.sell,
            amount: order.amount,
            buy: request.buy,
            expected: order.expected,
            minimum: order.minimum,
            created_at: now,
            updated_at: now,
            status: SwapStatus::Waiting,
        };
        if self.save_swap(record).is_err() {
            return;
        }
        if order.provider == Provider::OneClick
            && let Some(swappers) = self.swappers.clone()
        {
            let id = sent.id.clone();
            tokio::spawn(async move {
                let _ = swappers.oneclick.submit(&order.reference, &id).await;
            });
        }
        self.schedule_polling();
    }

    pub(super) fn swaps(&self) -> Result<Vec<SwapRecord>, EngineError> {
        self.ensure_unlocked()?;
        Ok(self.swaps.clone())
    }

    pub(super) fn start_polling(&mut self) {
        self.next_poll = None;
        if !self.unlocked() || self.polling.is_some() {
            return;
        }
        let open: Vec<SwapRecord> = self
            .swaps
            .iter()
            .filter(|swap| swap.status.is_open())
            .cloned()
            .collect();
        let Some(swappers) = self.swappers.clone().filter(|_| !open.is_empty()) else {
            return;
        };
        let notices = self.notices_tx.clone();
        self.polling = Some(tokio::spawn(async move {
            let jobs = open.into_iter().map(|swap| {
                let swappers = Arc::clone(&swappers);
                async move {
                    let provider = Provider::from_key(&swap.provider)?;
                    let status = stored(swappers.status(provider, &swap.reference).await.ok()?);
                    (status != swap.status).then(|| SwapRecord {
                        status,
                        updated_at: bahaa_chain::now(),
                        ..swap
                    })
                }
            });
            let updates = join_all(jobs).await.into_iter().flatten().collect();
            let _ = notices.send(Notice::Polled(updates)).await;
        }));
    }

    pub(super) fn polled(&mut self, updates: Vec<SwapRecord>) {
        self.polling = None;
        for update in updates {
            let open = self.swaps.iter().any(|swap| {
                swap.network == update.network && swap.id == update.id && swap.status.is_open()
            });
            if open {
                let _ = self.save_swap(update);
            }
        }
        self.schedule_polling();
    }

    pub(super) fn schedule_polling(&mut self) {
        let idle = self.polling.is_none() && self.next_poll.is_none();
        let open = self.swaps.iter().any(|swap| swap.status.is_open());
        if self.unlocked() && idle && open && self.swappers.is_some() {
            self.next_poll = Some(Instant::now() + self.track_every);
        }
    }

    pub(super) fn stop_polling(&mut self) {
        if let Some(task) = self.polling.take() {
            task.abort();
        }
        self.next_poll = None;
        self.swap_drafts.clear();
    }

    fn save_swap(&mut self, record: SwapRecord) -> Result<(), EngineError> {
        self.store.save_swap(&record)?;
        match self
            .swaps
            .iter_mut()
            .find(|swap| swap.network == record.network && swap.id == record.id)
        {
            Some(slot) => *slot = record.clone(),
            None => self.swaps.insert(0, record.clone()),
        }
        let _ = self.events.send(Event::SwapUpdated(Box::new(record)));
        Ok(())
    }

    fn ranked(&self, routes: Vec<Route>) -> Vec<Ranked> {
        let snapshot = self.quotes();
        let fiat = self.settings.fiat;
        rank(
            routes,
            |asset| portfolio::price(&self.registry, &self.custom, &snapshot, fiat, asset),
            fiat.places(),
        )
    }

    fn picked(&self, id: SwapId, index: usize) -> Result<Picked, EngineError> {
        self.ensure_unlocked()?;
        let prepared = self.swap_drafts.get(&id).ok_or(EngineError::UnknownDraft)?;
        if Instant::now() >= prepared.expires {
            return Err(EngineError::DraftExpired);
        }
        let (provider, route, order) = prepared
            .routes
            .get(index)
            .ok_or(ChainError::Invalid("there is no such route"))?;
        if route.refused.is_some() {
            return Err(SwapError::Untrusted("this route was refused").into());
        }
        Ok(Picked {
            wallet: prepared.wallet,
            account: prepared.account,
            chain: prepared.chain,
            request: prepared.request.clone(),
            provider: *provider,
            route: route.clone(),
            order: order.clone(),
        })
    }

    fn swap_target(&self, request: &SwapRequest) -> Result<(usize, Request), EngineError> {
        let (chain, from) =
            self.spending(request.wallet, request.account, request.sell.network())?;
        if request.sell == request.buy || request.amount.is_zero() {
            return Err(ChainError::Invalid("pick two different assets and an amount").into());
        }
        let sold = portfolio::coin(&self.registry, &self.custom, &request.sell)
            .ok_or(ChainError::Invalid("this wallet doesn't track that asset"))?;
        let bought = portfolio::coin(&self.registry, &self.custom, &request.buy)
            .ok_or(ChainError::Invalid("this wallet doesn't track that asset"))?;
        if sold.decimals != request.amount.decimals() {
            return Err(ChainError::Invalid("the amount has the wrong number of decimals").into());
        }
        let destination = self
            .chain_index(request.buy.network())
            .ok_or(ChainError::Unsupported("receiving on that network"))?;
        let family = self.chains[destination].family();
        let mut theirs: Vec<(u32, Address)> = self
            .accounts
            .iter()
            .filter(|account| {
                account.wallet == request.wallet && account.serves(request.buy.network(), family)
            })
            .map(|account| (account.index, account.address))
            .collect();
        theirs.sort_by_key(|(index, _)| (*index != request.account, *index));
        let to = theirs
            .first()
            .map(|(_, address)| *address)
            .ok_or(ChainError::Invalid(
                "this wallet has no account on the network you want to receive on",
            ))?;
        Ok((
            chain,
            Request {
                sell: request.sell.clone(),
                amount: request.amount,
                buy: request.buy.clone(),
                buy_decimals: bought.decimals,
                from,
                to,
                deadline: bahaa_chain::now().saturating_add(DEPOSIT_WINDOW),
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use bahaa_swap::{Comment, Order, Provider};
    use bahaa_types::{Address, Amount, ChainFamily, hex};

    use super::memo;

    const RELAY_BODY: &str = "b5ee9c7241010101004800008c0000000030786463383838323163313265313165346231346431383835633565346131366563633435656264393733616238666239363030346434366534336432336266623544d48afe";

    fn order(comment: Option<Comment>) -> Order {
        let amount = Amount::parse("10", 9).unwrap();
        Order {
            provider: Provider::Relay,
            reference: "x".to_owned(),
            deposit: Address::parse(
                ChainFamily::Ton,
                "EQCrdGsDTqA2t6xRR4N6V4J705F7w_VQbUdHnofsh-8lVIPs",
            )
            .unwrap(),
            comment,
            amount,
            expected: amount,
            minimum: amount,
            expires_at: 0,
        }
    }

    #[test]
    fn a_deposit_comment_becomes_the_memo_of_our_own_transfer() {
        let body = hex::decode(RELAY_BODY).unwrap();
        assert_eq!(
            memo(&order(Some(Comment::TonBody(body))))
                .unwrap()
                .as_deref(),
            Some("0xdc88821c12e11e4b14d1885c5e4a16ecc45ebd973ab8fb96004d46e43d23bfb5")
        );
        assert_eq!(memo(&order(None)).unwrap(), None);
        assert!(memo(&order(Some(Comment::TonBody(vec![1, 2, 3])))).is_err());
        assert!(memo(&order(Some(Comment::Text("line\nbreak".to_owned())))).is_err());
    }
}
