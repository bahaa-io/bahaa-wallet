use bahaa_engine::{Address, AssetId, Collectible, Mark, NftId, NftKind, Trust, WalletId};

use super::send::{Send, Source};
use super::{Screen, button, cell, choice, group, line, network_cell, network_line, note};
use crate::action::{Action, Go, Marking, Pick, Showing, Toggle};
use crate::call::{Call, Outcome};
use crate::core::{Ctx, Nav, Reply};
use crate::view::{Cell, Item, Level, Line, Note, Page, PageId, Request, Row, Table, Tone};
use crate::{format, text};

pub(crate) fn nft_name(item: &Collectible) -> String {
    item.label().unwrap_or_else(|| text::UNNAMED_NFT.to_owned())
}

pub(crate) fn view_group(nfts: bool) -> Item {
    group(
        text::VIEW,
        vec![
            choice(
                text::COINS,
                Action::Pick(Pick::Showing(Showing::Coins)),
                !nfts,
            ),
            choice(text::NFTS, Action::Pick(Pick::Showing(Showing::Nfts)), nfts),
        ],
    )
    .into()
}

pub(crate) fn listed<'a>(
    ctx: &'a Ctx<'_>,
    filter: Option<WalletId>,
    hidden: bool,
) -> Vec<&'a Collectible> {
    let mut items: Vec<&Collectible> = ctx
        .data
        .collectibles
        .items
        .iter()
        .filter(|item| item.shown != hidden)
        .filter(|item| {
            filter.map_or_else(|| ctx.counted(item.wallet), |wallet| item.wallet == wallet)
        })
        .collect();
    items.sort_by_cached_key(|item| {
        let collection = collection_name(ctx, item).map(|name| natural(&name));
        (collection.is_none(), collection, natural(&nft_name(item)))
    });
    items
}

fn natural(text: &str) -> Vec<(bool, usize, String)> {
    let mut parts = Vec::new();
    let mut rest = text;
    while let Some(first) = rest.chars().next() {
        let digits = first.is_ascii_digit();
        let end = rest
            .find(|c: char| c.is_ascii_digit() != digits)
            .unwrap_or(rest.len());
        let (part, tail) = rest.split_at(end);
        parts.push(if digits {
            let number = part.trim_start_matches('0');
            (true, number.len(), number.to_owned())
        } else {
            (false, 0, part.to_lowercase())
        });
        rest = tail;
    }
    parts
}

pub(crate) fn page(
    ctx: &Ctx<'_>,
    filter: Option<WalletId>,
    hidden: bool,
    mut form: Vec<Item>,
) -> Page {
    let items = listed(ctx, filter, hidden);
    let rows = items
        .iter()
        .enumerate()
        .map(|(position, item)| Row {
            cells: vec![
                cell(nft_name(item), Tone::Plain),
                cell(collection_name(ctx, item).unwrap_or_default(), Tone::Muted),
                network_cell(ctx, item.nft.asset.network()),
                row_note(item, hidden),
            ],
            open: Some(Action::Go(Go::Nft(position))),
            warn: false,
        })
        .collect();
    form.push(
        group(
            text::SHOW,
            vec![choice(
                text::hidden_choice(listed(ctx, filter, true).len()),
                Action::Toggle(Toggle::HiddenNfts),
                hidden,
            )],
        )
        .into(),
    );
    Page {
        id: PageId::Nfts,
        title: text::PORTFOLIO.to_owned(),
        headline: Some(text::nft_count(items.len())),
        form,
        table: Some(Table {
            columns: text::NFT_COLUMNS.map(str::to_owned).to_vec(),
            rows,
            empty: if hidden {
                text::NO_HIDDEN_NFTS
            } else {
                text::NO_NFTS
            }
            .to_owned(),
        }),
        notes: notes(ctx, hidden),
        ..Page::default()
    }
}

fn notes(ctx: &Ctx<'_>, hidden: bool) -> Vec<Note> {
    let health = &ctx.data.collectibles.health;
    let names = |keep: &dyn Fn(&bahaa_engine::NftHealth) -> bool| -> Vec<String> {
        health
            .iter()
            .filter(|health| keep(health))
            .map(|health| ctx.network_name(&health.network))
            .collect()
    };
    let mut notes = Vec::new();
    let loading = names(&|health| health.loading);
    if !loading.is_empty() {
        notes.push(note(Level::Info, text::nfts_loading(&loading)));
    }
    let failed = names(&|health| !health.loading && health.problem.is_some());
    if !failed.is_empty() {
        notes.push(note(Level::Warning, text::nfts_problem(&failed)));
    }
    let capped = names(&|health| health.capped);
    if !capped.is_empty() {
        notes.push(note(Level::Info, text::nfts_capped(&capped)));
    }
    if hidden {
        notes.push(note(Level::Info, text::HIDDEN_NFTS_NOTE));
    }
    notes
}

fn row_note(item: &Collectible, hidden: bool) -> Cell {
    if hidden {
        return match (item.mark, &item.fake, item.nft.trust) {
            (Some(Mark::Hidden), ..) => cell(text::BY_YOU, Tone::Muted),
            (_, Some(_), _) => cell(text::LOOKS_FAKE, Tone::Warning),
            (_, None, Trust::Flagged) => cell(text::FLAGGED, Tone::Bad),
            _ => cell(text::UNKNOWN_COLLECTION, Tone::Muted),
        };
    }
    match item.nft.lock {
        Some(lock) => cell(text::lock_short(lock), Tone::Warning),
        None if item.nft.kind == NftKind::Erc1155 => {
            cell(text::held_count(&item.nft.amount.to_string()), Tone::Muted)
        }
        None => cell("", Tone::Plain),
    }
}

fn collection_name(ctx: &Ctx<'_>, item: &Collectible) -> Option<String> {
    let collection = item.nft.collection.as_ref()?;
    collection.name.clone().or_else(|| {
        collection
            .address
            .map(|address| format::short(&ctx.shown(&address, item.nft.asset.network())))
    })
}

fn listing(item: &Collectible) -> String {
    match (item.mark, item.shown, &item.fake, item.nft.trust) {
        (Some(Mark::Shown), ..) => text::SHOWN_BY_YOU.to_owned(),
        (Some(Mark::Hidden), ..) => text::HIDDEN_BY_YOU.to_owned(),
        (None, true, ..) => text::SHOWN_KNOWN.to_owned(),
        (None, false, Some(fake), _) => text::fake_reason(fake),
        (None, false, None, Trust::Flagged) => text::HIDDEN_FLAGGED.to_owned(),
        (None, false, None, _) => text::HIDDEN_UNKNOWN.to_owned(),
    }
}

pub(crate) struct Detail {
    wallet: WalletId,
    address: Address,
    asset: AssetId,
    marking: Option<Marking>,
}

impl Detail {
    pub(crate) fn open(item: &Collectible) -> Reply {
        let detail = Self {
            wallet: item.wallet,
            address: item.address,
            asset: item.nft.asset.clone(),
            marking: None,
        };
        Reply {
            call: Some(Call::Picture(detail.asset.clone())),
            ..Reply::nav(Nav::Push(Box::new(detail)))
        }
    }

    fn item<'a>(&self, ctx: &'a Ctx<'_>) -> Option<&'a Collectible> {
        ctx.collectible(self.wallet, &self.address, &self.asset)
    }

    fn sendable(&self, ctx: &Ctx<'_>) -> bool {
        self.item(ctx)
            .is_some_and(|item| item.nft.lock.is_none() && !ctx.watch_only(self.wallet))
    }

    fn id_text(&self, ctx: &Ctx<'_>) -> Option<String> {
        let nft = self.asset.nft()?;
        Some(ctx.shown(nft.address(), self.asset.network()))
    }
}

impl Screen for Detail {
    fn act(&mut self, action: &Action, ctx: &Ctx<'_>) -> Reply {
        match action {
            Action::Go(Go::SendAsset) if self.sendable(ctx) => {
                Reply::nav(Nav::Push(Box::new(Send::new(Some(Source {
                    wallet: self.wallet,
                    address: self.address,
                    asset: self.asset.clone(),
                })))))
            }
            Action::Pick(Pick::Mark(marking)) => {
                self.marking = Some(*marking);
                let mark = match marking {
                    Marking::Show => Some(Mark::Shown),
                    Marking::Hide => Some(Mark::Hidden),
                    Marking::Default => None,
                };
                Reply::call(Call::MarkNft(self.asset.clone(), mark))
            }
            Action::Copy => self
                .id_text(ctx)
                .map_or_else(Reply::default, |id| Reply::request(Request::Copy(id))),
            Action::Explore => match (self.id_text(ctx), ctx.network(self.asset.network())) {
                (Some(id), Some(network)) => {
                    Reply::request(Request::Open(network.explorer.address_url(&id)))
                }
                _ => Reply::default(),
            },
            _ => Reply::default(),
        }
    }

    fn done(&mut self, outcome: Outcome, _ctx: &Ctx<'_>) -> Reply {
        let marking = self.marking.take();
        match (outcome, marking) {
            (Outcome::Done(Ok(())), Some(marking)) => {
                let said = match marking {
                    Marking::Show => text::SHOWN_NOW,
                    Marking::Hide => text::HIDDEN_NOW,
                    Marking::Default => text::DEFAULT_NOW,
                };
                Reply {
                    call: Some(Call::Picture(self.asset.clone())),
                    ..Reply::note(Level::Success, said)
                }
            }
            (Outcome::Done(Err(err)), _) => Reply::note(Level::Error, text::error(&err)),
            _ => Reply::default(),
        }
    }

    fn page(&self, ctx: &Ctx<'_>) -> Page {
        let Some(item) = self.item(ctx) else {
            return Page {
                id: PageId::Nft,
                title: text::NFT.to_owned(),
                text: Some(text::NFT_GONE.to_owned()),
                ..Page::default()
            };
        };
        let network = item.nft.asset.network();
        let mut lines: Vec<Line> = Vec::new();
        if let Some(name) = collection_name(ctx, item) {
            lines.push(line(text::COLLECTION, name));
        }
        lines.push(network_line(ctx, network));
        lines.push(line(
            text::WALLET,
            text::owner(
                &ctx.owner_label(item.wallet, &item.address),
                &format::short(&ctx.shown(&item.address, network)),
            ),
        ));
        lines.push(line(text::KIND, text::nft_kind(item.nft.kind)));
        if let Some(token) = item.nft.asset.nft().and_then(NftId::token) {
            lines.push(line(text::TOKEN_NUMBER, token.to_string()));
        }
        if item.nft.kind == NftKind::Erc1155 {
            lines.push(line(text::HELD, item.nft.amount.to_string()));
        }
        lines.push(line(text::LISTED, listing(item)));
        if let Some(lock) = item.nft.lock {
            lines.push(line(text::SEND_LOCK, text::lock_reason(lock)));
        }
        let mut buttons = Vec::new();
        if self.sendable(ctx) {
            buttons.push(button(text::SEND, Action::Go(Go::SendAsset)));
        }
        buttons.push(if item.shown {
            button(text::HIDE_IT, Action::Pick(Pick::Mark(Marking::Hide)))
        } else {
            button(text::SHOW_IT, Action::Pick(Pick::Mark(Marking::Show)))
        });
        if item.mark.is_some() {
            buttons.push(button(
                text::USE_DEFAULT,
                Action::Pick(Pick::Mark(Marking::Default)),
            ));
        }
        buttons.push(button(text::COPY_NFT_ID, Action::Copy));
        buttons.push(button(text::OPEN_EXPLORER, Action::Explore));
        Page {
            id: PageId::Nft,
            title: nft_name(item),
            text: item
                .details
                .as_ref()
                .and_then(|details| details.about.clone()),
            lines,
            buttons,
            picture: item.picture.clone(),
            ..Page::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::natural;

    #[test]
    fn numbers_in_names_sort_by_value() {
        let mut names = vec![
            "Lad #218",
            "lad #10",
            "Lad #1273",
            "Lad #0099",
            "Lad",
            "Box 2b",
        ];
        names.sort_by_key(|name| natural(name));
        assert_eq!(
            names,
            [
                "Box 2b",
                "Lad",
                "lad #10",
                "Lad #0099",
                "Lad #218",
                "Lad #1273"
            ]
        );
    }
}
