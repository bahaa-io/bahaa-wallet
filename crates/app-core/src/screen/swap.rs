use bahaa_engine::swap::{Custody, Method, Ranked, Route, Step as CallStep, WARN_BPS};
use bahaa_engine::{
    Amount, AssetId, EngineError, NetworkId, Spend, SwapDraft, SwapRequest, SwapReview,
};
use ruint::aliases::U256;

use super::activity::Transfer;
use super::send::{Source, source_of};
use super::{
    Screen, asset_cell, button, cell, choice, group, hidden, input, line, network_cell, note,
    swap_details, swap_tone, toned_line,
};
use crate::action::{Action, Field, Go, Pick, Tab, Toggle};
use crate::call::{Call, Outcome};
use crate::core::{Ctx, Nav, Reply};
use crate::view::{Input, Level, Line, Note, Page, PageId, Row, Secret, Table, Tone};
use crate::{format, parse, text};

const WHOLE_BPS: u32 = 10_000;

#[derive(Default)]
enum Step {
    #[default]
    Choose,
    Target,
    Compose,
    Review {
        review: Box<SwapReview>,
        at: u64,
    },
    Confirm {
        review: Box<SwapReview>,
        at: u64,
        password: Secret,
    },
    Started {
        network: NetworkId,
        id: String,
    },
}

#[derive(Default)]
pub(crate) struct Swap {
    source: Option<Source>,
    buy: Option<AssetId>,
    amount_text: String,
    max: bool,
    routes: Option<(SwapDraft, u64)>,
    step: Step,
    error: Option<String>,
}

impl Swap {
    fn targets(&self, ctx: &Ctx<'_>) -> Vec<AssetId> {
        let Some(source) = &self.source else {
            return Vec::new();
        };
        let testnet = ctx
            .network(source.asset.network())
            .is_some_and(|network| network.testnet);
        let mut targets: Vec<AssetId> = ctx
            .networks()
            .into_iter()
            .filter(|network| network.testnet == testnet)
            .filter(|network| {
                ctx.data.accounts.iter().any(|account| {
                    account.wallet == source.wallet && account.serves(&network.id, network.family())
                })
            })
            .flat_map(|network| {
                let custom = ctx
                    .data
                    .tokens
                    .iter()
                    .filter(|token| token.asset.network() == &network.id)
                    .map(|token| token.asset.clone());
                network
                    .assets()
                    .map(|(asset, _)| asset)
                    .chain(custom)
                    .collect::<Vec<_>>()
            })
            .filter(|asset| asset != &source.asset)
            .collect();
        targets.sort_by_key(|asset| asset.network() != source.asset.network());
        targets
    }

    fn fresh(&self, now: u64) -> Option<&SwapDraft> {
        self.routes
            .as_ref()
            .filter(|(draft, at)| now < at + draft.valid_for.as_secs())
            .map(|(draft, _)| draft)
    }

    fn request(&self, ctx: &Ctx<'_>) -> Result<SwapRequest, String> {
        let source = self.source.as_ref().ok_or(text::PICK_SWAP_SOURCE)?;
        let buy = self.buy.clone().ok_or(text::PICK_SWAP_TARGET)?;
        let account = ctx
            .account_index(source.wallet, &source.address)
            .ok_or(text::UNKNOWN_ACCOUNT)?;
        let amount = if self.max {
            Spend::Max
        } else {
            let decimals = ctx.coin(&source.asset).ok_or(text::UNKNOWN_ASSET)?.decimals;
            Spend::Exact(parse::amount(&self.amount_text, decimals)?)
        };
        Ok(SwapRequest {
            wallet: source.wallet,
            account,
            sell: source.asset.clone(),
            buy,
            amount,
        })
    }

    fn find(&mut self, ctx: &Ctx<'_>) -> Reply {
        self.routes = None;
        match self.request(ctx) {
            Ok(request) => {
                self.error = None;
                Reply::call(Call::PrepareSwap(request))
            }
            Err(problem) => {
                self.error = Some(problem);
                Reply::default()
            }
        }
    }

    fn pick_route(&mut self, position: usize, ctx: &Ctx<'_>) -> Reply {
        let Some(draft) = self.fresh(ctx.now) else {
            self.error = Some(text::ROUTES_OLD.to_owned());
            return Reply::default();
        };
        match draft.routes.get(position) {
            Some(route) if route.refused.is_none() => {
                let call = Call::ChooseSwap(draft.id, position);
                self.error = None;
                Reply::call(call)
            }
            _ => Reply::default(),
        }
    }

    fn submit(&mut self, ctx: &Ctx<'_>) -> Reply {
        match std::mem::take(&mut self.step) {
            step @ (Step::Choose | Step::Target) => {
                self.step = step;
                Reply::default()
            }
            Step::Compose => {
                self.step = Step::Compose;
                let best = self.fresh(ctx.now).and_then(|draft| {
                    draft
                        .routes
                        .iter()
                        .position(|route| route.refused.is_none())
                });
                match best {
                    Some(position) => self.pick_route(position, ctx),
                    None => self.find(ctx),
                }
            }
            Step::Review { review, at } if left(&review, at, ctx.now) == 0 => {
                self.step = Step::Compose;
                self.find(ctx)
            }
            Step::Review { review, at } => {
                self.step = Step::Confirm {
                    review,
                    at,
                    password: Secret::default(),
                };
                Reply::default()
            }
            Step::Confirm {
                review,
                at,
                password,
            } => {
                let call = if review.order.typed.is_some() {
                    Call::ConfirmOrder(review.draft.id, super::secret(&password))
                } else {
                    Call::Confirm(review.draft.id, super::secret(&password))
                };
                self.step = Step::Confirm {
                    review,
                    at,
                    password,
                };
                Reply::call(call)
            }
            Step::Started { network, id } => {
                let transfer = if ctx.sent(&network, &id).is_some() {
                    Transfer::sent(network.clone(), id.clone())
                } else {
                    Transfer::order(network.clone(), id.clone())
                };
                self.step = Step::Started { network, id };
                Reply::nav(Nav::Push(Box::new(transfer)))
            }
        }
    }

    fn side(&self, ctx: &Ctx<'_>, asset: &AssetId) -> String {
        let owner = self
            .source
            .as_ref()
            .map(|source| ctx.wallet_name(source.wallet))
            .unwrap_or_default();
        text::side(
            &ctx.symbol(asset),
            &ctx.network_name(asset.network()),
            &owner,
        )
    }

    fn target_page(&self, ctx: &Ctx<'_>) -> Page {
        let rows = self
            .targets(ctx)
            .iter()
            .enumerate()
            .map(|(position, asset)| Row {
                cells: vec![asset_cell(ctx, asset), network_cell(ctx, asset.network())],
                open: Some(Action::Pick(Pick::Buy(position))),
                warn: false,
            })
            .collect();
        let lines = self
            .source
            .as_ref()
            .map(|source| line(text::FROM, self.side(ctx, &source.asset)))
            .into_iter()
            .collect();
        Page {
            id: PageId::SwapTo,
            title: text::SWAP.to_owned(),
            text: Some(text::PICK_SWAP_TARGET.to_owned()),
            lines,
            table: Some(Table {
                columns: text::TARGET_COLUMNS.map(str::to_owned).to_vec(),
                rows,
                empty: text::NO_TARGETS.to_owned(),
            }),
            ..Page::default()
        }
    }

    fn compose_page(&self, ctx: &Ctx<'_>) -> Page {
        let (Some(source), Some(buy)) = (&self.source, &self.buy) else {
            return self.target_page(ctx);
        };
        let held = super::holding(ctx, source);
        let mut lines = vec![line(text::FROM, self.side(ctx, &source.asset))];
        if let Some(holding) = held {
            lines.push(line(
                text::BALANCE,
                text::with_value(
                    &ctx.amount(holding.amount, &holding.symbol),
                    &ctx.value(holding.value),
                ),
            ));
        }
        lines.push(line(text::SWAP_TO, self.side(ctx, buy)));
        let hint = if self.max {
            Some(note(
                Level::Info,
                text::everything(source.asset.contract().is_none(), self.routes.is_some()),
            ))
        } else {
            held.and_then(|holding| super::amount_hint(ctx, holding, &self.amount_text))
        };
        let form = vec![
            Input {
                hint,
                ..input(Field::Amount, text::AMOUNT, &self.amount_text)
            }
            .into(),
            group(
                "",
                vec![choice(
                    text::SWAP_ALL,
                    Action::Toggle(Toggle::Max),
                    self.max,
                )],
            )
            .into(),
        ];
        let fresh = self.fresh(ctx.now);
        let (table, notes) = self
            .routes
            .as_ref()
            .map(|(draft, _)| routes_part(ctx, draft, buy, fresh.is_some()))
            .unzip();
        let table = table.filter(|table| !table.rows.is_empty());
        let usable =
            fresh.is_some_and(|draft| draft.routes.iter().any(|route| route.refused.is_none()));
        let buttons = if usable {
            vec![
                button(text::REVIEW_BEST, Action::Submit),
                button(text::NEW_ROUTES, Action::Pick(Pick::Requote)),
            ]
        } else {
            vec![button(text::FIND_ROUTES, Action::Submit)]
        };
        Page {
            id: PageId::Swap,
            title: text::SWAP.to_owned(),
            lines,
            form,
            table,
            notes: notes.unwrap_or_default(),
            error: self.error.clone(),
            buttons,
            ..Page::default()
        }
    }

    fn review_page(&self, review: &SwapReview, at: u64, ctx: &Ctx<'_>) -> Page {
        let ranked = &review.route;
        let route = &ranked.route;
        let (sold, bought) = (ctx.symbol(&route.sell), ctx.symbol(&route.buy));
        let fiat = |amount: Option<Amount>| format::fiat(amount, ctx.fiat());
        let value = ranked.value.as_ref();
        let owner = self
            .source
            .as_ref()
            .map(|source| ctx.wallet_name(source.wallet))
            .unwrap_or_default();
        let mut lines = vec![
            line(text::PROVIDER, text::custody(route.method, route.provider)),
            line(
                text::YOU_SEND,
                text::sending(
                    &format::exact(route.amount, &sold),
                    &fiat(value.map(|value| value.sold)),
                    &owner,
                ),
            ),
            line(
                text::YOU_RECEIVE,
                text::receiving(
                    &format::exact(route.expected, &bought),
                    &fiat(value.map(|value| value.received)),
                    &format::exact(route.minimum, &bought),
                    &text::on_network(&owner, &ctx.network_name(route.buy.network())),
                ),
            ),
        ];
        lines.extend(rate_line(ctx, ranked));
        lines.extend(cost_lines(ctx, route));
        let contract = review.order.contract.is_some();
        let pooled = contract && route.custody() == Custody::Nobody;
        let signed = review.order.typed.is_some();
        if let Some(wait) = route.wait {
            let wait = format::wait(wait);
            lines.push(line(
                text::EXPECTED_WAIT,
                if pooled || signed {
                    wait
                } else {
                    text::after_deposit(&wait)
                },
            ));
        }
        let network = route.sell.network();
        if signed {
            lines.push(line(text::ORDER, text::SIGNED_ORDER.to_owned()));
        } else {
            lines.push(line(
                if contract && route.custody() != Custody::Provider {
                    text::SWAP_CONTRACT
                } else {
                    text::DEPOSIT_TO
                },
                format::grouped(&ctx.shown(&review.order.deposit, network)),
            ));
        }
        lines.extend(approval_lines(review, &sold));
        if let Some(memo) = &review.draft.transfer.memo {
            lines.push(line(text::MEMO, memo.clone()));
        }
        let destination = ctx.network_name(route.buy.network());
        let mut notes = review_notes(review, (&sold, &bought), &destination);
        let gone = left(review, at, ctx.now) == 0;
        if gone {
            notes.push(note(Level::Info, text::QUOTE_GONE));
        }
        Page {
            id: PageId::SwapReview,
            title: text::swap_title(
                &format::exact(route.amount, &sold),
                &bought,
                &ctx.network_name(route.buy.network()),
            ),
            lines,
            notes,
            error: self.error.clone(),
            buttons: vec![
                button(
                    if gone { text::NEW_QUOTE } else { text::CONFIRM },
                    Action::Submit,
                ),
                button(text::OTHER_ROUTES, Action::Back),
            ],
            ..Page::default()
        }
    }

    fn started_page(network: &NetworkId, id: &str, ctx: &Ctx<'_>) -> Page {
        let swap = ctx.swap(network, id);
        let mut lines = vec![swap.map_or_else(
            || toned_line(text::STATUS, text::SENDING_DEPOSIT, Tone::Waiting),
            |swap| {
                toned_line(
                    text::STATUS,
                    text::swap_status(&swap.status),
                    swap_tone(&swap.status),
                )
            },
        )];
        if let Some(swap) = swap {
            lines.extend(swap_details(ctx, swap));
        }
        Page {
            id: PageId::Swapping,
            title: text::SWAP.to_owned(),
            headline: swap.map(|swap| {
                text::swap_headline(
                    &format::exact(swap.amount, &ctx.symbol(&swap.sell)),
                    &ctx.symbol(&swap.buy),
                )
            }),
            lines,
            buttons: vec![
                button(
                    if ctx.sent(network, id).is_some() {
                        text::VIEW_DEPOSIT
                    } else {
                        text::VIEW_ORDER
                    },
                    Action::Submit,
                ),
                button(text::DONE, Action::Go(Go::Tab(Tab::Portfolio))),
            ],
            ..Page::default()
        }
    }
}

fn routes_part(ctx: &Ctx<'_>, draft: &SwapDraft, buy: &AssetId, fresh: bool) -> (Table, Vec<Note>) {
    let symbol = ctx.symbol(buy);
    let best = draft
        .routes
        .iter()
        .position(|route| route.refused.is_none());
    let table = Table {
        columns: text::ROUTE_COLUMNS.map(str::to_owned).to_vec(),
        rows: draft
            .routes
            .iter()
            .enumerate()
            .map(|(position, ranked)| {
                route_row(ctx, position, ranked, best == Some(position), &symbol)
            })
            .collect(),
        empty: text::NO_ROUTES.to_owned(),
    };
    let mut notes = Vec::new();
    if !draft.routes.is_empty() {
        notes.push(note(
            Level::Info,
            if fresh {
                text::ROUTES_PICK
            } else {
                text::ROUTES_OLD
            },
        ));
    }
    notes.extend(draft.unavailable.iter().map(|missing| {
        note(
            Level::Info,
            text::unavailable(missing.provider, &missing.reason),
        )
    }));
    (table, notes)
}

fn review_notes(review: &SwapReview, (sold, bought): (&str, &str), destination: &str) -> Vec<Note> {
    let ranked = &review.route;
    let route = &ranked.route;
    let pooled = review.order.contract.is_some() && route.custody() == Custody::Nobody;
    let mut notes: Vec<Note> = ranked
        .cautions
        .iter()
        .map(|caution| {
            note(
                Level::Warning,
                text::route_caution(*caution, format::percent),
            )
        })
        .collect();
    if review.checked {
        let sent = format::exact(route.amount, sold);
        notes.push(note(
            Level::Info,
            if route.sell.network() == route.buy.network() {
                text::checked_note(&sent, &format::exact(route.minimum, bought))
            } else {
                text::checked_deposit(&sent)
            },
        ));
    }
    notes.push(note(
        Level::Info,
        if review.order.typed.is_some() {
            text::ORDER_NOTE.to_owned()
        } else if route.method == Method::Issuer {
            text::issuer_note(route.provider, sold, destination)
        } else if pooled {
            text::pool_note(sold)
        } else {
            text::refund_note(route.provider, sold)
        },
    ));
    notes
}

fn approval_lines(review: &SwapReview, sold: &str) -> Vec<Line> {
    let signed = review.order.typed.is_some();
    if review.steps.len() < 2 && !signed {
        return Vec::new();
    }
    let approved = review.steps.iter().find_map(|step| match step {
        CallStep::Approve { amount, .. } if !amount.is_zero() => Some(*amount),
        CallStep::Approve { .. } | CallStep::Swap => None,
    });
    let mut lines: Vec<Line> = approved
        .map(|amount| {
            let amount = format::exact(amount, sold);
            line(
                text::APPROVAL,
                if signed {
                    text::order_approval(&amount)
                } else {
                    text::approval_line(&amount)
                },
            )
        })
        .into_iter()
        .collect();
    lines.push(line(
        text::TRANSACTIONS,
        if signed {
            text::order_transactions(review.steps.len().saturating_sub(1))
        } else {
            text::transactions(review.steps.len())
        },
    ));
    lines
}

fn rate_line(ctx: &Ctx<'_>, ranked: &Ranked) -> Option<Line> {
    let route = &ranked.route;
    let market = ranked.value.as_ref().map(|value| {
        let (sold, received) = (value.sold.units(), value.received.units());
        let below = received < sold;
        let difference = if below {
            share(sold - received, sold)
        } else {
            share(received - sold, sold)
        };
        text::market(below, &format::percent(difference))
    });
    Some(line(
        text::RATE,
        text::rate(
            &ctx.symbol(&route.sell),
            &format::amount(rate(route)?, &ctx.symbol(&route.buy)),
            market.as_deref(),
        ),
    ))
}

fn cost_lines(ctx: &Ctx<'_>, route: &Route) -> Vec<Line> {
    route
        .costs
        .iter()
        .map(|cost| {
            let worth = ctx
                .price(&cost.asset)
                .and_then(|price| price.value_of(cost.amount, ctx.fiat().places()).ok());
            line(
                text::cost_kind(cost.kind),
                text::charged(
                    &text::with_value(
                        &format::exact(cost.amount, &ctx.symbol(&cost.asset)),
                        &format::fiat(worth, ctx.fiat()),
                    ),
                    cost.charged,
                ),
            )
        })
        .collect()
}

fn route_row(ctx: &Ctx<'_>, position: usize, ranked: &Ranked, best: bool, symbol: &str) -> Row {
    let route = &ranked.route;
    let refused = ranked.refused.is_some();
    let (name, tone) = match (refused, best) {
        (true, _) => (route.provider.to_owned(), Tone::Bad),
        (false, true) => (text::best(route.provider), Tone::Good),
        (false, false) => (route.provider.to_owned(), Tone::Plain),
    };
    let fiat = |amount: Option<Amount>| format::fiat(amount, ctx.fiat());
    let costs = match (ranked.refused, &ranked.value) {
        (Some(refusal), _) => cell(text::refusal(refusal, format::percent), Tone::Bad),
        (None, Some(value)) => {
            let lost = value.sold.units().saturating_sub(value.net.units());
            let bps = share(lost, value.sold.units());
            let worth = Amount::new(lost, value.sold.decimals()).ok();
            cell(
                text::costs(&fiat(worth), &format::percent(bps)),
                if bps > WARN_BPS {
                    Tone::Warning
                } else {
                    Tone::Plain
                },
            )
        }
        (None, None) => cell(text::NO_VALUE, Tone::Muted),
    };
    Row {
        cells: vec![
            cell(name, tone),
            format::exact(route.expected, symbol).into(),
            fiat(ranked.value.as_ref().map(|value| value.received)).into(),
            costs,
            cell(format::short_wait(route.wait), Tone::Muted),
            cell(text::held_by(route.method), Tone::Muted),
        ],
        open: (!refused).then_some(Action::Pick(Pick::Route(position))),
        warn: refused,
    }
}

fn rate(route: &Route) -> Option<Amount> {
    let scale = U256::from(10u8).checked_pow(U256::from(route.amount.decimals()))?;
    let units = route
        .expected
        .units()
        .checked_mul(scale)?
        .checked_div(route.amount.units())?;
    Amount::new(units, route.expected.decimals()).ok()
}

fn share(part: U256, whole: U256) -> u32 {
    if whole.is_zero() {
        return 0;
    }
    let bps = part.saturating_mul(U256::from(WHOLE_BPS)) / whole;
    u32::try_from(bps).unwrap_or(u32::MAX)
}

fn left(review: &SwapReview, at: u64, now: u64) -> u64 {
    (at + review.draft.valid_for.as_secs()).saturating_sub(now)
}

impl Screen for Swap {
    fn act(&mut self, action: &Action, ctx: &Ctx<'_>) -> Reply {
        match (&mut self.step, action) {
            (Step::Choose, Action::Pick(Pick::Source(position))) => {
                if let Some(holding) = super::spendable(ctx).into_iter().nth(*position) {
                    self.source = Some(source_of(holding));
                    self.buy = None;
                    self.routes = None;
                    self.step = Step::Target;
                }
                Reply::default()
            }
            (Step::Target, Action::Pick(Pick::Buy(position))) => {
                if let Some(asset) = self.targets(ctx).into_iter().nth(*position) {
                    self.buy = Some(asset);
                    self.routes = None;
                    self.step = Step::Compose;
                }
                Reply::default()
            }
            (_, Action::Submit) => self.submit(ctx),
            (Step::Compose, Action::Type(Field::Amount, value)) => {
                self.amount_text.clone_from(value);
                self.max = false;
                self.routes = None;
                self.error = None;
                Reply::default()
            }
            (Step::Compose, Action::Toggle(Toggle::Max)) => {
                self.max = !self.max;
                if let Some(holding) = self
                    .source
                    .as_ref()
                    .and_then(|source| super::holding(ctx, source))
                    .filter(|_| self.max)
                {
                    self.amount_text = holding.amount.to_string();
                }
                self.routes = None;
                self.error = None;
                Reply::default()
            }
            (Step::Compose, Action::Pick(Pick::Route(position))) => self.pick_route(*position, ctx),
            (Step::Compose, Action::Pick(Pick::Requote)) => self.find(ctx),
            (Step::Confirm { password, .. }, Action::Type(Field::Password, value)) => {
                *password = Secret::new(value);
                Reply::default()
            }
            _ => Reply::default(),
        }
    }

    fn done(&mut self, outcome: Outcome, ctx: &Ctx<'_>) -> Reply {
        match outcome {
            Outcome::Routes(Ok(draft)) => {
                self.error = draft.routes.is_empty().then(|| text::NO_ROUTES.to_owned());
                if self.max {
                    self.amount_text = draft.amount.to_string();
                }
                self.routes = Some((draft, ctx.now));
            }
            Outcome::Reviewed(Ok(review)) => {
                self.error = None;
                self.step = Step::Review {
                    review,
                    at: ctx.now,
                };
            }
            Outcome::Sent(Ok(tx)) => {
                self.routes = None;
                self.step = Step::Started {
                    network: tx.network,
                    id: tx.id,
                };
            }
            Outcome::Ordered(Ok(swap)) => {
                self.routes = None;
                self.step = Step::Started {
                    network: swap.network,
                    id: swap.id,
                };
            }
            Outcome::Sent(Err(EngineError::DraftExpired | EngineError::UnknownDraft))
            | Outcome::Ordered(Err(EngineError::DraftExpired | EngineError::UnknownDraft)) => {
                self.routes = None;
                self.step = Step::Compose;
                self.error = Some(text::QUOTE_EXPIRED.to_owned());
            }
            Outcome::Sent(Err(err)) | Outcome::Ordered(Err(err)) => {
                if let Step::Confirm { password, .. } = &mut self.step {
                    *password = Secret::default();
                }
                self.error = Some(text::error(&err));
            }
            Outcome::Routes(Err(err)) | Outcome::Reviewed(Err(err)) => {
                self.error = Some(text::error(&err));
            }
            _ => {}
        }
        Reply::default()
    }

    fn back(&mut self) -> Option<Reply> {
        self.error = None;
        self.step = match std::mem::take(&mut self.step) {
            Step::Choose => return None,
            Step::Target => Step::Choose,
            Step::Compose => Step::Target,
            Step::Review { .. } => Step::Compose,
            Step::Confirm { review, at, .. } => Step::Review { review, at },
            Step::Started { .. } => return Some(Reply::nav(Nav::Tab(Tab::Swap))),
        };
        Some(Reply::default())
    }

    fn page(&self, ctx: &Ctx<'_>) -> Page {
        match &self.step {
            Step::Choose => super::holdings_page(
                ctx,
                PageId::SwapFrom,
                text::SWAP,
                text::PICK_SWAP_SOURCE,
                text::NOTHING_TO_SWAP,
            ),
            Step::Target => self.target_page(ctx),
            Step::Compose => self.compose_page(ctx),
            Step::Review { review, at } => self.review_page(review, *at, ctx),
            Step::Confirm { password, .. } => Page {
                id: PageId::Confirm,
                title: text::CONFIRM_TITLE.to_owned(),
                form: vec![hidden(Field::Password, text::PASSWORD, password).into()],
                error: self.error.clone(),
                buttons: vec![button(text::SWAP_NOW, Action::Submit)],
                ..Page::default()
            },
            Step::Started { network, id } => Self::started_page(network, id, ctx),
        }
    }
}

#[cfg(test)]
mod tests {
    use bahaa_engine::Amount;
    use bahaa_engine::swap::{Method, Route};
    use ruint::aliases::U256;

    use super::{rate, share};

    #[test]
    fn the_rate_is_what_one_unit_of_the_sold_coin_buys() {
        let route = Route {
            provider: "Relay",
            method: Method::Deposit,
            sell: "bsc".parse().unwrap(),
            amount: Amount::parse("0.5", 18).unwrap(),
            buy: "base".parse().unwrap(),
            expected: Amount::parse("0.13342", 18).unwrap(),
            minimum: Amount::parse("0.13309", 18).unwrap(),
            costs: Vec::new(),
            wait: None,
        };
        assert_eq!(rate(&route).unwrap().to_string(), "0.26684");
        assert_eq!(share(U256::from(134u16), U256::from(34_921u32)), 38);
        assert_eq!(share(U256::from(1u8), U256::ZERO), 0);
    }
}
