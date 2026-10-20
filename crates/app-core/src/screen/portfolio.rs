use std::cmp::Ordering;

use bahaa_engine::{Amount, AssetId, Holding, WalletId};
use ruint::aliases::U256;

use super::receive::Receive;
use super::send::{Send, Source};
use super::{
    Screen, asset_cell, button, cell, choice, group, line, network_cell, network_line,
    picked_wallet, wallet_group,
};
use crate::action::{Action, Go, Pick, Toggle};
use crate::core::{Ctx, Nav, Reply};
use crate::view::{Item, Page, PageId, Row, Share, Table, Tone};
use crate::{format, text};

const SHARES: usize = 5;
const PERMILLE: u16 = 1000;

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
            Action::Pick(Pick::Wallet(position)) => self.filter = picked_wallet(ctx, *position),
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
        let rows = summaries
            .iter()
            .enumerate()
            .map(|(position, summary)| Row {
                cells: vec![
                    asset_cell(ctx, &summary.asset),
                    network_cell(ctx, summary.asset.network()),
                    cell(
                        format::price(ctx.price(&summary.asset), ctx.fiat()),
                        Tone::Muted,
                    ),
                    ctx.amount(summary.amount, &ctx.symbol(&summary.asset))
                        .into(),
                    ctx.value(summary.value).into(),
                ],
                open: Some(Action::Go(Go::Asset(position))),
                warn: false,
            })
            .collect();
        let mut form: Vec<Item> = wallet_group(ctx, self.filter)
            .into_iter()
            .map(Item::from)
            .collect();
        form.push(
            group(
                text::SHOW,
                vec![choice(
                    text::EMPTY_BALANCES,
                    Action::Toggle(Toggle::HideEmpty),
                    self.show_empty,
                )],
            )
            .into(),
        );
        Page {
            id: PageId::Portfolio,
            title: text::PORTFOLIO.to_owned(),
            headline: Some(ctx.value(total)),
            shares: total
                .filter(|_| !ctx.data.settings.hide_balances)
                .map_or_else(Vec::new, |total| shares(ctx, &summaries, total)),
            form,
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
                Reply::nav(Nav::Push(Box::new(Receive::for_holding(holding))))
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
                let amount = ctx.amount(holding.amount, &symbol);
                let amount = match holding.staked {
                    Some(staked) => text::with_staked(&amount, &ctx.amount(staked, &symbol)),
                    None => amount,
                };
                Row {
                    cells: vec![
                        ctx.wallet_name(holding.wallet).into(),
                        cell(account_label(ctx, holding), Tone::Muted),
                        cell(
                            format::short(&ctx.shown(&holding.address, self.asset.network())),
                            Tone::Muted,
                        ),
                        amount.into(),
                        ctx.value(holding.value).into(),
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
            headline: total.map(|total| ctx.exact(total, &symbol)),
            lines: vec![
                network_line(ctx, self.asset.network()),
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

fn shares(ctx: &Ctx<'_>, summaries: &[Summary], total: Amount) -> Vec<Share> {
    let mut parts: Vec<(String, Tone, Amount)> = Vec::new();
    for summary in summaries {
        let Some(value) = summary.value.filter(|value| !value.is_zero()) else {
            continue;
        };
        let symbol = ctx.symbol(&summary.asset);
        match parts.iter_mut().find(|(label, ..)| *label == symbol) {
            Some((_, _, sum)) => *sum = sum.checked_add(value).unwrap_or(*sum),
            None => parts.push((symbol, ctx.coin_tone(&summary.asset), value)),
        }
    }
    parts.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(Ordering::Equal));
    if parts.len() > SHARES {
        let rest = parts.split_off(SHARES - 1);
        if let Some(sum) = rest
            .into_iter()
            .map(|(.., value)| value)
            .reduce(|sum, value| sum.checked_add(value).unwrap_or(sum))
        {
            parts.push((text::OTHER.to_owned(), Tone::Muted, sum));
        }
    }
    parts
        .into_iter()
        .map(|(label, tone, value)| Share {
            label,
            permille: permille(value, total),
            tone,
        })
        .collect()
}

fn permille(value: Amount, total: Amount) -> u16 {
    if total.is_zero() {
        return 0;
    }
    let scaled = value.units().saturating_mul(U256::from(PERMILLE)) / total.units();
    u16::try_from(scaled).map_or(PERMILLE, |permille| permille.min(PERMILLE))
}

fn sum(total: Option<Amount>, value: Option<Amount>) -> Option<Amount> {
    total?.checked_add(value?).ok()
}

fn account_label(ctx: &Ctx<'_>, holding: &Holding) -> String {
    ctx.account_index(holding.wallet, &holding.address)
        .map_or_else(String::new, text::account)
}

#[cfg(test)]
mod tests {
    use bahaa_engine::Amount;

    use super::permille;

    fn usd(text: &str) -> Amount {
        Amount::parse(text, 2).unwrap()
    }

    #[test]
    fn shares_are_parts_of_a_thousand() {
        assert_eq!(permille(usd("1"), usd("3")), 333);
        assert_eq!(permille(usd("2.5"), usd("2.5")), 1000);
        assert_eq!(permille(usd("0"), usd("7")), 0);
        assert_eq!(permille(usd("7"), usd("0")), 0);
    }
}
