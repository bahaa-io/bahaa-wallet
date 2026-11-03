use std::sync::Arc;
use std::time::Duration;

use bahaa_chain::{ChainError, ContractSwap, FeeQuote, MAX_MEMO, Step, Transfer};
use bahaa_store::{SentStatus, SentTx, SwapRecord, SwapStatus};
use bahaa_swap::{
    Across, Charged, Comment, Cost, CostKind, Cow, Jupiter, KyberSwap, OneClick, Order, Pools,
    Progress, Provider, Providers, QUOTE_LIFE, Ranked, Relay, Request, Route, SwapError, Velora,
    rank,
};
use bahaa_types::{Address, Amount, AssetId, BitcoinScript, NetworkId, is_plain_text};
use futures_util::future::join_all;
use tokio::time::Instant;

mod cow;

use super::send::{NewDraft, Planned, Spend};
use super::{Draft, EngineError, Event, Notice, Reply, Worker};
use crate::chains::{AnyCall, AnyChain};
use crate::portfolio;
use crate::{KeyringError, WalletId};

const MAX_SWAP_DRAFTS: usize = 8;
const STAND_IN: [u8; 32] = [
    0xe5, 0x5e, 0x0d, 0x12, 0x9a, 0x10, 0xa5, 0x9f, 0x99, 0xac, 0xd7, 0x44, 0xcb, 0xa4, 0x9a, 0x46,
    0x72, 0x8b, 0xc5, 0xc0, 0x15, 0xcd, 0x52, 0xd2, 0xea, 0xa5, 0xe5, 0x26, 0xd2, 0xb1, 0x09, 0x35,
];
const DEPOSIT_WINDOW: u64 = 30 * 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SwapId(u64);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwapRequest {
    pub wallet: WalletId,
    pub account: u32,
    pub sell: AssetId,
    pub buy: AssetId,
    pub amount: Spend,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unavailable {
    pub provider: &'static str,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwapDraft {
    pub id: SwapId,
    pub amount: Amount,
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
    pub steps: Vec<Step>,
    pub checked: bool,
}

pub(crate) struct Swappers {
    oneclick: OneClick,
    relay: Relay,
    pools: Pools,
    kyber: KyberSwap,
    velora: Velora,
    across: Across,
    cow: Cow,
    jupiter: Jupiter,
}

impl Swappers {
    pub(crate) fn new(providers: &Providers) -> Result<Self, EngineError> {
        let client = bahaa_chain::http::client()?;
        Ok(Self {
            oneclick: OneClick::new(&providers.oneclick, client.clone())?,
            relay: Relay::new(&providers.relay, client.clone())?,
            pools: Pools::new(&providers.pools)?,
            kyber: KyberSwap::new(&providers.kyberswap, client.clone())?,
            velora: Velora::new(&providers.velora, client.clone())?,
            across: Across::new(&providers.across, client.clone())?,
            cow: Cow::new(&providers.cow, client.clone())?,
            jupiter: Jupiter::new(&providers.jupiter, client)?,
        })
    }

    fn covering(&self, chain: &AnyChain, request: &Request) -> Vec<Provider> {
        let mut providers = vec![Provider::OneClick, Provider::Relay];
        if chain.evm().is_some() {
            providers.extend(
                self.pools
                    .provider(request.sell.network())
                    .filter(|_| self.pools.covers(request)),
            );
            let covered = [
                (Provider::KyberSwap, self.kyber.covers(request)),
                (Provider::Velora, self.velora.covers(request)),
                (Provider::Across, self.across.covers(request)),
                (Provider::Cow, self.cow.covers(request)),
            ];
            providers.extend(
                covered
                    .into_iter()
                    .filter_map(|(provider, covers)| covers.then_some(provider)),
            );
        }
        if chain.solana().is_some() && self.jupiter.covers(request) {
            providers.push(Provider::Jupiter);
        }
        providers
    }

    async fn quotes(
        &self,
        chain: &AnyChain,
        request: &Request,
        now: u64,
    ) -> (Vec<Quote>, Vec<Unavailable>) {
        let asks = self
            .covering(chain, request)
            .into_iter()
            .map(|provider| async move {
                let answer = match provider {
                    Provider::OneClick if !self.oneclick.covers(request) => {
                        Err(SwapError::Unsupported)
                    }
                    Provider::OneClick => self.oneclick.quote(request, false, now).await,
                    _ => self
                        .fresh(provider, chain, request, now)
                        .await
                        .map(|(route, order)| (route, Some(order))),
                };
                (provider, answer)
            });
        let answers = join_all(asks).await;
        let mut quotes = Vec::new();
        let mut unavailable = Vec::new();
        for (provider, answer) in answers {
            match answer {
                Ok((route, order)) => quotes.push((provider, route, order)),
                Err(err) => unavailable.push(Unavailable {
                    provider: provider.name(),
                    reason: err.to_string(),
                }),
            }
        }
        (quotes, unavailable)
    }

    async fn order(
        &self,
        picked: &Picked,
        chain: &AnyChain,
        now: u64,
    ) -> Result<(Route, Order), SwapError> {
        if let Some(order) = picked.order.clone().filter(|order| order.expires_at > now) {
            return Ok((picked.route.route.clone(), order));
        }
        self.fresh(picked.provider, chain, &picked.request, now)
            .await
    }

    async fn fresh(
        &self,
        provider: Provider,
        chain: &AnyChain,
        request: &Request,
        now: u64,
    ) -> Result<(Route, Order), SwapError> {
        match provider {
            Provider::OneClick => {
                let (route, order) = self.oneclick.quote(request, true, now).await?;
                let order =
                    order.ok_or(SwapError::Untrusted("the order has no deposit address"))?;
                Ok((route, order))
            }
            Provider::Relay => self.relay.quote(request, now).await,
            Provider::Uniswap | Provider::PancakeSwap => {
                let evm = chain.evm().ok_or(SwapError::Unsupported)?;
                self.pools.quote(evm, request, now).await
            }
            Provider::KyberSwap => {
                let evm = chain.evm().ok_or(SwapError::Unsupported)?;
                self.kyber.quote(evm, request, now).await
            }
            Provider::Across => {
                let evm = chain.evm().ok_or(SwapError::Unsupported)?;
                self.across.quote(evm, request, now).await
            }
            Provider::Velora => {
                let evm = chain.evm().ok_or(SwapError::Unsupported)?;
                self.velora.quote(evm, request, now).await
            }
            Provider::Cow => self.cow.quote(request, now).await,
            Provider::Jupiter => {
                let solana = chain.solana().ok_or(SwapError::Unsupported)?;
                self.jupiter.quote(solana, request, now).await
            }
        }
    }

    async fn status(
        &self,
        provider: Provider,
        network: &NetworkId,
        reference: &str,
    ) -> Result<Progress, SwapError> {
        match provider {
            Provider::OneClick => self.oneclick.status(reference).await,
            Provider::Relay => self.relay.status(reference).await,
            Provider::Across => self.across.status(network, reference).await,
            Provider::Cow => self.cow.status(network, reference).await,
            Provider::Uniswap
            | Provider::PancakeSwap
            | Provider::KyberSwap
            | Provider::Velora
            | Provider::Jupiter => Err(SwapError::Unsupported),
        }
    }
}

type Quote = (Provider, Route, Option<Order>);

pub(super) struct PreparedSwap {
    wallet: WalletId,
    account: u32,
    chain: usize,
    max: bool,
    request: Request,
    routes: Vec<(Provider, Ranked, Option<Order>)>,
    expires: Instant,
}

#[derive(Clone)]
struct Picked {
    wallet: WalletId,
    account: u32,
    chain: usize,
    max: bool,
    request: Request,
    provider: Provider,
    route: Ranked,
    order: Option<Order>,
}

pub(super) struct SwapLink {
    request: Request,
    order: Order,
}

pub(super) use cow::record as order_record;

struct Found {
    request: Request,
    quotes: Vec<Quote>,
    unavailable: Vec<Unavailable>,
    fee: Option<FeeQuote>,
}

pub(super) struct SwapQuoted {
    session: u64,
    wallet: WalletId,
    account: u32,
    chain: usize,
    max: bool,
    found: Result<Found, EngineError>,
    reply: Reply<SwapDraft>,
}

pub(super) struct SwapOrdered {
    session: u64,
    wallet: WalletId,
    chain: usize,
    request: Request,
    result: Result<Ordered, EngineError>,
    reply: Reply<SwapReview>,
}

struct Ordered {
    route: Route,
    order: Order,
    plan: Planned,
    transfer: Transfer,
    fee: FeeQuote,
    steps: Vec<Step>,
    checked: bool,
}

struct Priced {
    route: Route,
    order: Order,
    plans: Vec<AnyCall>,
    fee: FeeQuote,
    swap: ContractSwap,
}

fn contract_swap(account: u32, request: &Request, order: &Order) -> Option<ContractSwap> {
    let contract = order.contract.as_ref()?;
    let here = request.buy.network() == request.sell.network();
    Some(ContractSwap {
        account,
        from: request.from,
        call: contract.call.clone(),
        spender: contract.spender,
        sell: request.sell.clone(),
        amount: order.amount,
        buy: here.then(|| (request.buy.clone(), order.minimum)),
        ours: contract.ours,
    })
}

fn lowered(amount: Amount, needed: Amount, available: Amount) -> Result<Amount, ChainError> {
    needed
        .checked_sub(available)
        .and_then(|short| amount.checked_sub(short))
        .ok()
        .filter(|smaller| !smaller.is_zero())
        .ok_or(ChainError::Invalid(
            "there is not enough left to swap after the network fee",
        ))
}

#[allow(clippy::too_many_arguments)]
async fn priced(
    chain: &AnyChain,
    swappers: &Swappers,
    provider: Provider,
    account: u32,
    max: bool,
    request: &mut Request,
    (route, order): (Route, Order),
    now: u64,
) -> Result<Priced, EngineError> {
    let untrusted = || SwapError::Untrusted("the route has no contract call");
    let swap = contract_swap(account, request, &order).ok_or_else(untrusted)?;
    match chain.prepare_swap(&swap).await {
        Ok((plans, fee)) => Ok(Priced {
            route,
            order,
            plans,
            fee,
            swap,
        }),
        Err(ChainError::Insufficient {
            needed, available, ..
        }) if max
            && request.sell.contract().is_none()
            && needed.decimals() == order.amount.decimals() =>
        {
            request.amount = lowered(order.amount, needed, available)?;
            let (route, order) = swappers.fresh(provider, chain, request, now).await?;
            let swap = contract_swap(account, request, &order).ok_or_else(untrusted)?;
            let (plans, fee) = chain.prepare_swap(&swap).await?;
            Ok(Priced {
                route,
                order,
                plans,
                fee,
                swap,
            })
        }
        Err(err) => Err(err.into()),
    }
}

fn network_fee(route: &mut Route, fee: &FeeQuote) {
    route
        .costs
        .retain(|cost| cost.kind != CostKind::NetworkFee || cost.charged == Charged::InQuote);
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

fn stand_in(like: &Address) -> Address {
    let mut short = [0u8; 20];
    short.copy_from_slice(&STAND_IN[..20]);
    match like {
        Address::Evm(_) => Address::Evm(short),
        Address::Tron(_) => Address::Tron(short),
        Address::Ton { workchain, .. } => Address::Ton {
            workchain: *workchain,
            hash: STAND_IN,
        },
        Address::Solana(_) => Address::Solana(STAND_IN),
        Address::Bitcoin { testnet, .. } => Address::Bitcoin {
            script: BitcoinScript::WitnessKeyHash(short),
            testnet: *testnet,
        },
    }
}

async fn most(
    chain: &AnyChain,
    account: u32,
    request: &Request,
    to: Address,
    memo: Option<String>,
) -> Result<Amount, ChainError> {
    let everything = Transfer {
        account,
        from: request.from,
        to,
        asset: request.sell.clone(),
        amount: Amount::default(),
        max: true,
        memo,
    };
    let (_, planned, _) = chain.prepare(&everything).await?;
    let exact = Transfer {
        amount: planned.amount,
        max: false,
        ..everything
    };
    match chain.prepare(&exact).await {
        Ok(_) => Ok(planned.amount),
        Err(ChainError::Insufficient {
            needed, available, ..
        }) if request.sell.contract().is_none()
            && needed.decimals() == planned.amount.decimals() =>
        {
            let smaller = lowered(planned.amount, needed, available)?;
            chain
                .prepare(&Transfer {
                    amount: smaller,
                    ..exact
                })
                .await?;
            Ok(smaller)
        }
        Err(err) => Err(err),
    }
}

async fn checked(
    chain: &AnyChain,
    swappers: &Swappers,
    account: u32,
    max: bool,
    asked: &Request,
    quotes: Vec<Quote>,
    now: u64,
) -> (Vec<Quote>, Vec<Unavailable>) {
    let checks = quotes.into_iter().map(|(provider, route, order)| {
        let mut request = asked.clone();
        async move {
            let order = match order {
                Some(order) if order.typed.is_some() => {
                    return Box::pin(cow::listed(
                        chain,
                        swappers,
                        account,
                        &request,
                        (provider, route, order),
                    ))
                    .await;
                }
                Some(order) if order.contract.is_some() => order,
                deposit => return Ok((provider, route, deposit)),
            };
            let priced = Box::pin(priced(
                chain,
                swappers,
                provider,
                account,
                max,
                &mut request,
                (route, order),
                now,
            ))
            .await
            .map_err(|err| Unavailable {
                provider: provider.name(),
                reason: err.to_string(),
            })?;
            let mut route = priced.route;
            network_fee(&mut route, &priced.fee);
            Ok((provider, route, Some(priced.order)))
        }
    });
    let mut kept = Vec::new();
    let mut unavailable = Vec::new();
    for check in join_all(checks).await {
        match check {
            Ok(quote) => kept.push(quote),
            Err(missing) => unavailable.push(missing),
        }
    }
    (kept, unavailable)
}

async fn call_order(
    chain: &AnyChain,
    swappers: &Swappers,
    picked: &Picked,
    request: &mut Request,
    (route, order): (Route, Order),
    now: u64,
) -> Result<Ordered, EngineError> {
    request.amount = order.amount;
    let priced = Box::pin(priced(
        chain,
        swappers,
        picked.provider,
        picked.account,
        picked.max,
        request,
        (route, order),
        now,
    ))
    .await?;
    let transfer = Transfer {
        account: picked.account,
        from: request.from,
        to: priced.order.deposit,
        asset: request.sell.clone(),
        amount: priced.order.amount,
        max: false,
        memo: None,
    };
    Ok(Ordered {
        steps: priced
            .plans
            .iter()
            .map(|plan| plan.planned().step.clone())
            .collect(),
        checked: priced.plans.iter().all(AnyCall::checked),
        route: priced.route,
        order: priced.order,
        plan: Planned::Calls {
            calls: priced.plans,
            swap: Box::new(priced.swap),
        },
        transfer,
        fee: priced.fee,
    })
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
        let (chain, mut asked) = match self.swap_target(request) {
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
        let max = request.amount == Spend::Max;
        tokio::spawn(async move {
            let found = async {
                if max {
                    asked.amount = Box::pin(most(
                        &chains[chain],
                        account,
                        &asked,
                        stand_in(&asked.from),
                        None,
                    ))
                    .await?;
                }
                let now = bahaa_chain::now();
                let (quotes, mut unavailable) = swappers.quotes(&chains[chain], &asked, now).await;
                let (quotes, more) =
                    checked(&chains[chain], &swappers, account, max, &asked, quotes, now).await;
                unavailable.extend(more);
                let target = quotes
                    .iter()
                    .filter_map(|(_, _, order)| order.as_ref())
                    .find(|order| order.contract.is_none())
                    .map_or_else(|| stand_in(&asked.from), |order| order.deposit);
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
                Ok::<_, EngineError>(Found {
                    request: asked,
                    quotes,
                    unavailable,
                    fee,
                })
            }
            .await;
            let quoted = SwapQuoted {
                session,
                wallet,
                account,
                chain,
                max,
                found,
                reply,
            };
            let _ = notices.send(Notice::SwapQuoted(Box::new(quoted))).await;
        });
    }

    pub(super) fn swap_quoted(&mut self, arrived: SwapQuoted) {
        let SwapQuoted {
            session,
            wallet,
            account,
            chain,
            max,
            found,
            reply,
        } = arrived;
        if session != self.session || !self.unlocked() {
            let _ = reply.send(Err(KeyringError::Locked.into()));
            return;
        }
        let Found {
            request,
            quotes,
            unavailable,
            fee,
        } = match found {
            Ok(found) => found,
            Err(err) => {
                let _ = reply.send(Err(err));
                return;
            }
        };
        let quotes: Vec<Quote> = quotes
            .into_iter()
            .map(|(provider, mut route, order)| {
                let deposit = order.as_ref().is_none_or(|order| order.contract.is_none());
                if let Some(fee) = fee.as_ref().filter(|_| deposit) {
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
            amount: request.amount,
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
                max,
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
            let mut request = picked.request.clone();
            let result = async {
                let chain = &chains[picked.chain];
                let now = bahaa_chain::now();
                let (mut route, mut order) = swappers.order(&picked, chain, now).await?;
                if order.typed.is_some() {
                    return Box::pin(cow::ordered(
                        chain,
                        &swappers,
                        &picked,
                        &mut request,
                        (route, order),
                    ))
                    .await;
                }
                if order.contract.is_some() {
                    return Box::pin(call_order(
                        chain,
                        &swappers,
                        &picked,
                        &mut request,
                        (route, order),
                        now,
                    ))
                    .await;
                }
                if picked.max {
                    let exact = Box::pin(most(
                        chain,
                        picked.account,
                        &request,
                        order.deposit,
                        memo(&order)?,
                    ))
                    .await?;
                    if exact != order.amount {
                        request.amount = exact;
                        let again = Picked {
                            request: request.clone(),
                            order: None,
                            ..picked.clone()
                        };
                        (route, order) = swappers.order(&again, chain, bahaa_chain::now()).await?;
                    }
                }
                if order.amount != request.amount {
                    return Err(SwapError::Untrusted("the order is for another amount").into());
                }
                let transfer = Transfer {
                    account: picked.account,
                    from: request.from,
                    to: order.deposit,
                    asset: request.sell.clone(),
                    amount: order.amount,
                    max: false,
                    memo: memo(&order)?,
                };
                let (plan, transfer, fee) = chain.prepare(&transfer).await?;
                Ok::<_, EngineError>(Ordered {
                    route,
                    order,
                    plan: Planned::Transfer(Box::new(plan)),
                    transfer,
                    fee,
                    steps: Vec::new(),
                    checked: false,
                })
            }
            .await;
            let ordered = SwapOrdered {
                session,
                wallet: picked.wallet,
                chain: picked.chain,
                request,
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
        let Ordered {
            mut route,
            order,
            plan,
            transfer,
            fee,
            steps,
            checked,
        } = match result {
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
                steps,
                checked,
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
            reference: if order.provider.by_transaction() {
                sent.id.clone()
            } else {
                order.reference.clone()
            },
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
            order: None,
            approval: None,
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

    pub(super) fn settle_swap(&mut self, sent: &SentTx) {
        let Some(record) = self.swaps.iter().find(|swap| {
            swap.network == sent.network && swap.id == sent.id && swap.status.is_open()
        }) else {
            return;
        };
        let on_chain = Provider::from_key(&record.provider).is_some_and(Provider::on_chain);
        let status = match &sent.status {
            SentStatus::Confirmed if on_chain => SwapStatus::Done,
            SentStatus::Failed(reason) => SwapStatus::Failed(reason.clone()),
            SentStatus::Dropped => SwapStatus::Failed("the network dropped the swap".to_owned()),
            SentStatus::Confirmed | SentStatus::Sending | SentStatus::Pending => return,
        };
        let record = SwapRecord {
            status,
            updated_at: bahaa_chain::now(),
            ..record.clone()
        };
        let _ = self.save_swap(record);
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
        let approvals: Vec<Option<SentStatus>> = open
            .iter()
            .map(|swap| {
                let id = swap.approval.as_ref()?;
                self.history
                    .iter()
                    .find(|tx| tx.network == swap.network && tx.id == *id)
                    .map(|tx| tx.status.clone())
                    .or(Some(SentStatus::Sending))
            })
            .collect();
        let notices = self.notices_tx.clone();
        self.polling = Some(tokio::spawn(async move {
            let jobs = open.into_iter().zip(approvals).map(|(swap, approval)| {
                let swappers = Arc::clone(&swappers);
                async move {
                    let provider =
                        Provider::from_key(&swap.provider).filter(|key| !key.on_chain())?;
                    if let Some(body) = swap.order.clone() {
                        return cow::placed(&swappers, swap, &body, approval).await;
                    }
                    let status = stored(
                        swappers
                            .status(provider, &swap.network, &swap.reference)
                            .await
                            .ok()?,
                    );
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
        let open = self.swaps.iter().any(|swap| {
            swap.status.is_open()
                && Provider::from_key(&swap.provider).is_some_and(|provider| !provider.on_chain())
        });
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

    pub(super) fn save_swap(&mut self, record: SwapRecord) -> Result<(), EngineError> {
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
            max: prepared.max,
            request: prepared.request.clone(),
            provider: *provider,
            route: route.clone(),
            order: order.clone(),
        })
    }

    fn swap_target(&self, request: &SwapRequest) -> Result<(usize, Request), EngineError> {
        let (chain, from) =
            self.spending(request.wallet, request.account, request.sell.network())?;
        if request.sell == request.buy {
            return Err(ChainError::Invalid("pick two different assets").into());
        }
        let sold = portfolio::coin(&self.registry, &self.custom, &request.sell)
            .ok_or(ChainError::Invalid("this wallet doesn't track that asset"))?;
        let bought = portfolio::coin(&self.registry, &self.custom, &request.buy)
            .ok_or(ChainError::Invalid("this wallet doesn't track that asset"))?;
        let amount = match request.amount {
            Spend::Exact(amount) if !amount.is_zero() && amount.decimals() == sold.decimals => {
                amount
            }
            Spend::Max => Amount::zero(sold.decimals).map_err(|_| ChainError::overflow())?,
            Spend::Exact(_) => {
                return Err(ChainError::Invalid("enter an amount to swap").into());
            }
        };
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
                amount,
                buy: request.buy.clone(),
                buy_decimals: bought.decimals,
                native_decimals: self.chains[chain].network().native.decimals,
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

    use super::{memo, stand_in};

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
            contract: None,
            typed: None,
        }
    }

    #[test]
    fn the_fee_estimate_goes_to_an_unused_address_of_the_same_kind() {
        let solana = Address::parse(
            ChainFamily::Solana,
            "9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM",
        )
        .unwrap();
        let bitcoin = Address::parse(
            ChainFamily::Bitcoin,
            "tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx",
        )
        .unwrap();
        for like in [solana, bitcoin] {
            let other = stand_in(&like);
            assert_ne!(other, like);
            assert_eq!(other.family(), like.family());
        }
        assert!(matches!(
            stand_in(&bitcoin),
            Address::Bitcoin { testnet: true, .. }
        ));
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
