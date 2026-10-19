use std::cmp::Ordering;

use bahaa_engine::{Amount, AssetId, Holding, WalletId};

use super::receive::Receive;
use super::send::{Send, Source};
use super::{Screen, button, choice, group, line};
use crate::action::{Action, Go, Pick, Toggle};
use crate::core::{Ctx, Nav, Reply};
use crate::view::{Page, PageId, Row, Table};
use crate::{format, text};

#[derive(Default)]
pub(crate) struct Portfolio {
    filter: Option<WalletId>,
    show_empty: bool,
}

struct Summary {
    asset: AssetId,
    amount: Amount,
    value: Option<Amount>,
}

impl Portfolio {
    fn summaries(&self, ctx: &Ctx<'_>) -> Vec<Summary> {
        let mut list: Vec<Summary> = Vec::new();
        for holding in held(ctx, self.filter) {
            match list
                .iter_mut()
                .find(|summary| summary.asset == holding.asset)
            {
                Some(summary) => {
                    summary.amount = summary
                        .amount
                        .checked_add(holding.amount)
                        .unwrap_or(summary.amount);
                    summary.value = sum(summary.value, holding.value);
                }
                None => list.push(Summary {
                    asset: holding.asset.clone(),
                    amount: holding.amount,
                    value: holding.value,
                }),
            }
        }
        list.retain(|summary| self.show_empty || !summary.amount.is_zero());
        list.sort_by(|a, b| {
            b.value
                .partial_cmp(&a.value)
                .unwrap_or(Ordering::Equal)
                .then_with(|| ctx.symbol(&a.asset).cmp(&ctx.symbol(&b.asset)))
        });
        list
    }
}

impl Screen for Portfolio {
    fn act(&mut self, action: &Action, ctx: &Ctx<'_>) -> Reply {
        match action {
            Action::Pick(Pick::Wallet(position)) => {
                self.filter = position
                    .and_then(|position| ctx.data.wallets.get(position))
                    .map(|wallet| wallet.id);
            }
            Action::Toggle(Toggle::HideEmpty) => self.show_empty = !self.show_empty,
            Action::Go(Go::Asset(row)) => {
                if let Some(summary) = self.summaries(ctx).into_iter().nth(*row) {
                    return Reply::nav(Nav::Push(Box::new(Asset {
                        asset: summary.asset,
                        filter: self.filter,
                    })));
                }
            }
            _ => {}
        }
        Reply::default()
    }

    fn page(&self, ctx: &Ctx<'_>) -> Page {
        let summaries = self.summaries(ctx);
        let total = summaries
            .iter()
            .filter_map(|summary| summary.value)
            .reduce(|total, value| total.checked_add(value).unwrap_or(total));
        let mut wallets = vec![choice(
            text::ALL_WALLETS,
            Action::Pick(Pick::Wallet(None)),
            self.filter.is_none(),
        )];
        wallets.extend(
            ctx.data
                .wallets
                .iter()
                .enumerate()
                .map(|(position, wallet)| {
                    choice(
                        ctx.wallet_name(wallet.id),
                        Action::Pick(Pick::Wallet(Some(position))),
                        self.filter == Some(wallet.id),
                    )
                }),
        );
        let rows = summaries
            .iter()
            .enumerate()
            .map(|(position, summary)| Row {
                cells: vec![
                    ctx.symbol(&summary.asset),
                    ctx.network_name(summary.asset.network()),
                    format::amount(summary.amount, &ctx.symbol(&summary.asset)),
                    format::fiat(summary.value, ctx.fiat()),
                ],
                open: Some(Action::Go(Go::Asset(position))),
                warn: false,
            })
            .collect();
        Page {
            id: PageId::Portfolio,
            title: text::PORTFOLIO.to_owned(),
            headline: Some(format::fiat(total, ctx.fiat())),
            groups: vec![
                group(text::WALLETS, wallets),
                group(
                    text::SHOW,
                    vec![choice(
                        text::EMPTY_BALANCES,
                        Action::Toggle(Toggle::HideEmpty),
                        self.show_empty,
                    )],
                ),
            ],
            table: Some(Table {
                columns: text::PORTFOLIO_COLUMNS.map(str::to_owned).to_vec(),
                rows,
                empty: text::NO_ASSETS.to_owned(),
            }),
            ..Page::default()
        }
    }
}

struct Asset {
    asset: AssetId,
    filter: Option<WalletId>,
}

impl Asset {
    fn holdings<'a>(&self, ctx: &'a Ctx<'_>) -> Vec<&'a Holding> {
        held(ctx, self.filter)
            .filter(|holding| holding.asset == self.asset)
            .collect()
    }
}

impl Screen for Asset {
    fn act(&mut self, action: &Action, ctx: &Ctx<'_>) -> Reply {
        let first = self
            .holdings(ctx)
            .into_iter()
            .max_by(|a, b| a.amount.partial_cmp(&b.amount).unwrap_or(Ordering::Equal));
        match (action, first) {
            (Action::Go(Go::SendAsset), Some(holding)) => {
                let source = Source {
                    wallet: holding.wallet,
                    address: holding.address,
                    asset: holding.asset.clone(),
                };
                Reply::nav(Nav::Push(Box::new(Send::new(Some(source)))))
            }
            (Action::Go(Go::ReceiveAsset), Some(holding)) => {
                Reply::nav(Nav::Push(Box::new(Receive::for_holding(holding, ctx))))
            }
            _ => Reply::default(),
        }
    }

    fn page(&self, ctx: &Ctx<'_>) -> Page {
        let symbol = ctx.symbol(&self.asset);
        let holdings = self.holdings(ctx);
        let total = holdings
            .iter()
            .map(|holding| holding.amount)
            .reduce(|total, amount| total.checked_add(amount).unwrap_or(total));
        let price_at = holdings.iter().find_map(|holding| holding.price_at);
        let price = match price_at {
            Some(at) => format!(
                "{} · {}",
                format::price(ctx.price(&self.asset), ctx.fiat()),
                format::ago(at, ctx.now)
            ),
            None => format::price(None, ctx.fiat()),
        };
        let rows = holdings
            .iter()
            .map(|holding| {
                let amount = format::amount(holding.amount, &symbol);
                let amount = match holding.staked {
                    Some(staked) => text::with_staked(&amount, &format::amount(staked, &symbol)),
                    None => amount,
                };
                Row {
                    cells: vec![
                        ctx.wallet_name(holding.wallet),
                        account_label(ctx, holding),
                        format::short(&ctx.shown(&holding.address, self.asset.network())),
                        amount,
                        format::fiat(holding.value, ctx.fiat()),
                    ],
                    open: None,
                    warn: false,
                }
            })
            .collect();
        Page {
            id: PageId::Asset,
            title: ctx.coin(&self.asset).map_or_else(
                || symbol.clone(),
                |coin| text::coin_title(&coin.name, &coin.symbol),
            ),
            headline: total.map(|total| format::exact(total, &symbol)),
            lines: vec![
                line(text::NETWORK, ctx.network_name(self.asset.network())),
                line(text::PRICE, price),
            ],
            table: Some(Table {
                columns: text::HOLDING_COLUMNS.map(str::to_owned).to_vec(),
                rows,
                empty: text::NO_ASSETS.to_owned(),
            }),
            buttons: vec![
                button(text::SEND, Action::Go(Go::SendAsset)),
                button(text::RECEIVE, Action::Go(Go::ReceiveAsset)),
            ],
            ..Page::default()
        }
    }
}

fn held<'a>(ctx: &'a Ctx<'_>, filter: Option<WalletId>) -> impl Iterator<Item = &'a Holding> {
    ctx.data
        .portfolio
        .holdings
        .iter()
        .filter(move |holding| filter.is_none_or(|wallet| holding.wallet == wallet))
}

fn sum(total: Option<Amount>, value: Option<Amount>) -> Option<Amount> {
    total?.checked_add(value?).ok()
}

fn account_label(ctx: &Ctx<'_>, holding: &Holding) -> String {
    ctx.account_index(holding.wallet, &holding.address)
        .map_or_else(String::new, text::account)
}
