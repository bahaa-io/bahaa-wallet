use bahaa_engine::{Address, AssetId, Holding, MAX_MEMO, NetworkId, WalletId, is_plain_text};

use super::{Screen, button, choice, group, input, line, note};
use crate::action::{Action, Field, Pick};
use crate::core::{Ctx, Reply};
use crate::view::{Level, Page, PageId, Request};
use crate::{links, parse, text};

#[derive(Clone, Debug, PartialEq, Eq)]
struct Target {
    wallet: WalletId,
    index: u32,
    address: Address,
    network: NetworkId,
}

#[derive(Default)]
pub(crate) struct Receive {
    target: Option<Target>,
    asset: Option<AssetId>,
    amount: String,
    memo: String,
}

impl Receive {
    pub(crate) fn for_holding(holding: &Holding, ctx: &Ctx<'_>) -> Self {
        Self {
            target: ctx
                .account_index(holding.wallet, &holding.address)
                .map(|index| Target {
                    wallet: holding.wallet,
                    index,
                    address: holding.address,
                    network: holding.asset.network().clone(),
                }),
            asset: Some(holding.asset.clone()),
            ..Self::default()
        }
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
                        index: account.index,
                        address: account.address,
                        network: network.id.clone(),
                    })
            })
            .collect()
    }

    fn asset(&self) -> Option<AssetId> {
        let target = self.target.as_ref()?;
        Some(
            self.asset
                .clone()
                .filter(|asset| asset.network() == &target.network)
                .unwrap_or_else(|| AssetId::Native(target.network.clone())),
        )
    }
}

impl Screen for Receive {
    fn act(&mut self, action: &Action, ctx: &Ctx<'_>) -> Reply {
        match action {
            Action::Pick(Pick::Target(position)) => {
                self.target = Self::targets(ctx).into_iter().nth(*position);
            }
            Action::Type(Field::Amount, value) => self.amount.clone_from(value),
            Action::Type(Field::Memo, value) => self.memo.clone_from(value),
            Action::Copy => {
                if let Some(target) = &self.target {
                    return Reply::request(Request::Copy(
                        ctx.shown(&target.address, &target.network),
                    ));
                }
            }
            _ => {}
        }
        Reply::default()
    }

    fn page(&self, ctx: &Ctx<'_>) -> Page {
        let targets = Self::targets(ctx);
        let single = targets
            .windows(2)
            .all(|pair| (pair[0].wallet, pair[0].index) == (pair[1].wallet, pair[1].index));
        let targets = targets
            .into_iter()
            .enumerate()
            .map(|(position, target)| {
                let network = ctx.network_name(&target.network);
                let label = if single {
                    network
                } else {
                    text::target(
                        &ctx.wallet_name(target.wallet),
                        &text::account(target.index),
                        &network,
                    )
                };
                choice(
                    label,
                    Action::Pick(Pick::Target(position)),
                    self.target.as_ref() == Some(&target),
                )
            })
            .collect();
        let mut page = Page {
            id: PageId::Receive,
            title: text::RECEIVE.to_owned(),
            form: vec![group(text::RECEIVE_TO, targets).into()],
            ..Page::default()
        };
        let (Some(target), Some(asset)) = (&self.target, self.asset()) else {
            page.text = Some(text::PICK_TARGET.to_owned());
            return page;
        };
        let Some(network) = ctx.network(&target.network) else {
            return page;
        };
        let memo_allowed = super::memo_allowed(&asset, network.family());
        let units = ctx
            .coin(&asset)
            .and_then(|coin| parse::amount(&self.amount, coin.decimals).ok())
            .map(|amount| amount.units());
        let memo =
            (memo_allowed && is_plain_text(self.memo.trim(), MAX_MEMO)).then(|| self.memo.trim());
        page.headline = Some(ctx.shown(&target.address, &target.network));
        page.lines = vec![
            line(text::NETWORK, network.name.clone()),
            line(text::ASSET, ctx.symbol(&asset)),
        ];
        page.form
            .push(input(Field::Amount, text::AMOUNT_OPTIONAL, &self.amount).into());
        if memo_allowed {
            page.form
                .push(input(Field::Memo, text::MEMO_OPTIONAL, &self.memo).into());
        }
        page.notes = vec![note(Level::Info, text::receive_note(&network.name))];
        page.qr = Some(links::request(
            network,
            &target.address,
            &asset,
            units,
            memo,
        ));
        page.buttons = vec![button(text::COPY_ADDRESS, Action::Copy)];
        page
    }
}
