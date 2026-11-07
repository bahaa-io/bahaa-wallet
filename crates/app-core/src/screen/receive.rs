use bahaa_engine::{Address, AssetId, Holding, MAX_MEMO, NetworkId, WalletId, is_plain_text};

use super::{Screen, button, cell, input, line, network_cell, network_line, note, toned_line};
use crate::action::{Action, Field, Pick};
use crate::call::{Call, Outcome};
use crate::core::{Ctx, Nav, Reply};
use crate::search::Facts;
use crate::view::{Level, Page, PageId, Request, Row, Table, Tone};
use crate::{format, links, parse, text};

#[derive(Clone, Debug, PartialEq, Eq)]
struct Target {
    wallet: WalletId,
    address: Address,
    network: NetworkId,
}

impl Target {
    fn lookup(&self) -> Call {
        Call::ReceiveAddress(self.network.clone(), self.address)
    }
}

#[derive(Default)]
pub(crate) struct Receive {
    target: Option<Target>,
    shown: Option<Address>,
    picked: bool,
    asset: Option<AssetId>,
    amount: String,
    memo: String,
    search: String,
}

impl Receive {
    pub(crate) fn for_holding(holding: &Holding) -> Reply {
        let target = Target {
            wallet: holding.wallet,
            address: holding.address,
            network: holding.asset.network().clone(),
        };
        let call = target.lookup();
        let screen = Self {
            target: Some(target),
            asset: Some(holding.asset.clone()),
            ..Self::default()
        };
        Reply {
            call: Some(call),
            ..Reply::nav(Nav::Push(Box::new(screen)))
        }
    }

    fn address(&self, target: &Target) -> Address {
        self.shown.unwrap_or(target.address)
    }

    fn targets(ctx: &Ctx<'_>) -> Vec<Target> {
        let networks = ctx.networks();
        ctx.data
            .wallets
            .iter()
            .flat_map(|wallet| {
                ctx.data
                    .accounts
                    .iter()
                    .filter(move |account| account.wallet == wallet.id)
            })
            .flat_map(|account| {
                networks
                    .iter()
                    .filter(move |network| account.serves(&network.id, network.family()))
                    .map(move |network| Target {
                        wallet: account.wallet,
                        address: account.address,
                        network: network.id.clone(),
                    })
            })
            .collect()
    }

    fn asset(&self, target: &Target) -> AssetId {
        self.asset
            .clone()
            .filter(|asset| asset.network() == &target.network)
            .unwrap_or_else(|| AssetId::Native(target.network.clone()))
    }

    fn owners(ctx: &Ctx<'_>, targets: &[Target]) -> Vec<String> {
        targets
            .iter()
            .map(|target| ctx.owner_label(target.wallet, &target.address))
            .collect()
    }

    fn list_page(&self, ctx: &Ctx<'_>) -> Page {
        let targets = Self::targets(ctx);
        let owners = Self::owners(ctx, &targets);
        let several = super::several(&owners);
        let mut columns: Vec<String> = text::RECEIVE_COLUMNS.map(str::to_owned).to_vec();
        if several {
            columns.push(text::WALLET.to_owned());
        }
        let total = targets.len();
        let kept = super::searched(
            ctx,
            &self.search,
            targets.iter().zip(owners),
            |(target, owner)| {
                let mut facts = Facts::default();
                facts
                    .network(ctx, &target.network)
                    .address(ctx, &target.address, &target.network)
                    .word(owner);
                for asset in ctx.assets(&target.network) {
                    facts.asset(ctx, &asset);
                }
                facts
            },
        );
        let shown = kept.len();
        let rows = kept
            .into_iter()
            .map(|(position, (target, owner))| {
                let mut cells = vec![
                    network_cell(ctx, &target.network),
                    format::short(&ctx.shown(&target.address, &target.network)).into(),
                ];
                if several {
                    cells.push(cell(owner, Tone::Muted));
                }
                Row {
                    cells,
                    open: Some(Action::Pick(Pick::Target(position))),
                    warn: false,
                }
            })
            .collect();
        Page {
            id: PageId::ReceiveTo,
            title: text::RECEIVE.to_owned(),
            text: Some(text::PICK_TARGET.to_owned()),
            form: vec![super::search_box(
                &self.search,
                text::SEARCH_RECEIVE,
                shown,
                total,
            )],
            table: Some(Table {
                columns,
                rows,
                empty: super::search_empty(&self.search, text::LOADING),
            }),
            ..Page::default()
        }
    }

    fn address_page(&self, target: &Target, ctx: &Ctx<'_>) -> Page {
        let Some(network) = ctx.network(&target.network) else {
            return self.list_page(ctx);
        };
        let asset = self.asset(target);
        let memo_allowed = super::memo_allowed(&asset, network.family());
        let amount = ctx
            .coin(&asset)
            .and_then(|coin| parse::amount(&self.amount, coin.decimals).ok());
        let memo =
            (memo_allowed && is_plain_text(self.memo.trim(), MAX_MEMO)).then(|| self.memo.trim());
        let mut lines = vec![network_line(ctx, &network.id)];
        if super::several(&Self::owners(ctx, &Self::targets(ctx))) {
            lines.push(line(
                text::WALLET,
                ctx.owner_label(target.wallet, &target.address),
            ));
        }
        if asset.contract().is_some() {
            lines.push(toned_line(
                text::ASSET,
                ctx.symbol(&asset),
                ctx.coin_tone(&asset),
            ));
        }
        let mut form = vec![input(Field::Amount, text::AMOUNT_OPTIONAL, &self.amount).into()];
        if memo_allowed {
            form.push(input(Field::Memo, text::MEMO_OPTIONAL, &self.memo).into());
        }
        let address = self.address(target);
        Page {
            id: PageId::Receive,
            title: text::receive_title(&network.name),
            headline: Some(format::grouped(&ctx.shown(&address, &target.network))),
            lines,
            form,
            notes: vec![note(Level::Info, text::receive_note(&network.name))],
            qr: Some(links::request(network, &address, &asset, amount, memo)),
            buttons: vec![button(text::COPY_ADDRESS, Action::Copy)],
            ..Page::default()
        }
    }
}

impl Screen for Receive {
    fn act(&mut self, action: &Action, ctx: &Ctx<'_>) -> Reply {
        match action {
            Action::Pick(Pick::Target(position)) if self.target.is_none() => {
                self.target = Self::targets(ctx).into_iter().nth(*position);
                self.picked = self.target.is_some();
                if let Some(target) = &self.target {
                    return Reply::call(target.lookup());
                }
            }
            Action::Type(Field::Search, value) if self.target.is_none() => {
                self.search.clone_from(value);
            }
            Action::Type(Field::Amount, value) => self.amount.clone_from(value),
            Action::Type(Field::Memo, value) => self.memo.clone_from(value),
            Action::Copy => {
                if let Some(target) = &self.target {
                    return Reply::request(Request::Copy(
                        ctx.shown(&self.address(target), &target.network),
                    ));
                }
            }
            _ => {}
        }
        Reply::default()
    }

    fn done(&mut self, outcome: Outcome, _ctx: &Ctx<'_>) -> Reply {
        if let Outcome::Receiving(Ok(address)) = outcome {
            self.shown = Some(address);
        }
        Reply::default()
    }

    fn back(&mut self) -> Option<Reply> {
        if !self.picked {
            return None;
        }
        *self = Self::default();
        Some(Reply::default())
    }

    fn page(&self, ctx: &Ctx<'_>) -> Page {
        match &self.target {
            Some(target) => self.address_page(target, ctx),
            None => self.list_page(ctx),
        }
    }
}
