use bahaa_engine::{Activity, ActivityKind, ChainFamily, Movement, NetworkId, Replace};

use super::send::Send;
use super::{Screen, button, line, note};
use crate::action::{Action, Go, Pick};
use crate::core::{Ctx, Nav, Reply};
use crate::view::{Level, Line, Page, PageId, Request, Row, Table};
use crate::{format, text};

pub(crate) struct List;

impl Screen for List {
    fn act(&mut self, action: &Action, ctx: &Ctx<'_>) -> Reply {
        match action {
            Action::Go(Go::Transfer(row)) => ctx
                .data
                .activity
                .get(*row)
                .map_or_else(Reply::default, |activity| {
                    Reply::nav(Nav::Push(Box::new(Transfer { key: key(activity) })))
                }),
            _ => Reply::default(),
        }
    }

    fn page(&self, ctx: &Ctx<'_>) -> Page {
        let rows = ctx
            .data
            .activity
            .iter()
            .enumerate()
            .map(|(position, activity)| {
                let (sign, moved, party, status, warn) = match &activity.kind {
                    ActivityKind::Sent(tx) => (
                        '−',
                        (tx.amount, &tx.asset),
                        ctx.party(&tx.to, &tx.network),
                        text::status(&tx.status),
                        false,
                    ),
                    ActivityKind::SentElsewhere(moved) => (
                        '−',
                        (moved.amount, &moved.asset),
                        ctx.party(&moved.to, &moved.network),
                        text::ELSEWHERE.to_owned(),
                        false,
                    ),
                    ActivityKind::Received(arrival) => (
                        '+',
                        (arrival.received.amount, &arrival.received.asset),
                        ctx.party(&arrival.received.from, &arrival.received.network),
                        text::RECEIVED.to_owned(),
                        !arrival.cautions.is_empty(),
                    ),
                };
                let (amount, asset) = moved;
                Row {
                    cells: vec![
                        format::ago(activity.time, ctx.now),
                        format!("{sign}{}", format::amount(amount, &ctx.symbol(asset))),
                        ctx.network_name(asset.network()),
                        party,
                        status,
                    ],
                    open: Some(Action::Go(Go::Transfer(position))),
                    warn,
                }
            })
            .collect();
        Page {
            id: PageId::Activity,
            title: text::ACTIVITY.to_owned(),
            table: Some(Table {
                columns: text::ACTIVITY_COLUMNS.map(str::to_owned).to_vec(),
                rows,
                empty: text::NO_ACTIVITY.to_owned(),
            }),
            ..Page::default()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Key {
    Sent(NetworkId, String),
    Moved(NetworkId, String, u64, bool),
}

fn key(activity: &Activity) -> Key {
    match &activity.kind {
        ActivityKind::Sent(tx) => Key::Sent(tx.network.clone(), tx.id.clone()),
        ActivityKind::SentElsewhere(moved) => {
            Key::Moved(moved.network.clone(), moved.id.clone(), moved.index, false)
        }
        ActivityKind::Received(arrival) => {
            let moved = &arrival.received;
            Key::Moved(moved.network.clone(), moved.id.clone(), moved.index, true)
        }
    }
}

pub(crate) struct Transfer {
    key: Key,
}

impl Transfer {
    pub(crate) fn sent(network: NetworkId, id: String) -> Self {
        Self {
            key: Key::Sent(network, id),
        }
    }

    fn find<'a>(&self, ctx: &'a Ctx<'_>) -> Option<&'a Activity> {
        ctx.data
            .activity
            .iter()
            .find(|activity| key(activity) == self.key)
    }

    fn id(&self) -> (&NetworkId, &str) {
        match &self.key {
            Key::Sent(network, id) | Key::Moved(network, id, ..) => (network, id),
        }
    }
}

impl Screen for Transfer {
    fn act(&mut self, action: &Action, ctx: &Ctx<'_>) -> Reply {
        let (network, id) = self.id();
        match action {
            Action::Copy => Reply::request(Request::Copy(id.to_owned())),
            Action::Explore => ctx.network(network).map_or_else(Reply::default, |network| {
                Reply::request(Request::Open(network.explorer.tx_url(id)))
            }),
            Action::Pick(Pick::SpeedUp) => replace(network, id, Replace::SpeedUp),
            Action::Pick(Pick::Cancel) => replace(network, id, Replace::Cancel),
            _ => Reply::default(),
        }
    }

    fn page(&self, ctx: &Ctx<'_>) -> Page {
        let (network, id) = self.id();
        let Some(activity) = self.find(ctx) else {
            return Page {
                id: PageId::Transfer,
                title: text::TRANSFER.to_owned(),
                text: Some(text::LOADING.to_owned()),
                ..Page::default()
            };
        };
        let mut buttons = vec![
            button(text::COPY_ID, Action::Copy),
            button(text::OPEN_EXPLORER, Action::Explore),
        ];
        let (title, headline, mut lines, notes) = match &activity.kind {
            ActivityKind::Sent(tx) => {
                let replaceable = tx.status.is_open()
                    && ctx
                        .network(&tx.network)
                        .is_some_and(|network| network.family() == ChainFamily::Evm);
                if replaceable {
                    buttons.push(button(text::SPEED_UP, Action::Pick(Pick::SpeedUp)));
                    buttons.push(button(text::CANCEL_SEND, Action::Pick(Pick::Cancel)));
                }
                let symbol = ctx.native_symbol(&tx.network);
                let fee = tx.fee_paid.map_or_else(
                    || text::at_most(&format::exact(tx.fee_max, &symbol)),
                    |paid| format::exact(paid, &symbol),
                );
                let mut lines = vec![
                    line(text::STATUS, text::status(&tx.status)),
                    line(text::FROM, ctx.party(&tx.from, &tx.network)),
                    line(text::TO, ctx.shown(&tx.to, &tx.network)),
                    line(text::FEE, fee),
                ];
                if let Some(memo) = &tx.memo {
                    lines.push(line(text::MEMO, memo.clone()));
                }
                (
                    text::SENT_TITLE,
                    format::exact(tx.amount, &ctx.symbol(&tx.asset)),
                    lines,
                    Vec::new(),
                )
            }
            ActivityKind::SentElsewhere(moved) => (
                text::ELSEWHERE_TITLE,
                format::exact(moved.amount, &ctx.symbol(&moved.asset)),
                movement_lines(ctx, moved),
                Vec::new(),
            ),
            ActivityKind::Received(arrival) => (
                text::RECEIVED_TITLE,
                format::exact(
                    arrival.received.amount,
                    &ctx.symbol(&arrival.received.asset),
                ),
                movement_lines(ctx, &arrival.received),
                arrival
                    .cautions
                    .iter()
                    .map(|caution| note(Level::Warning, text::caution(caution, ctx)))
                    .collect(),
            ),
        };
        lines.push(line(text::NETWORK, ctx.network_name(network)));
        lines.push(line(text::TIME, format::when(activity.time)));
        lines.push(line(text::TRANSACTION, id.to_owned()));
        Page {
            id: PageId::Transfer,
            title: title.to_owned(),
            headline: Some(headline),
            lines,
            notes,
            buttons,
            ..Page::default()
        }
    }
}

fn replace(network: &NetworkId, id: &str, how: Replace) -> Reply {
    let (screen, call) = Send::replacing(network.clone(), id.to_owned(), how);
    Reply {
        call: Some(call),
        ..Reply::nav(Nav::Push(Box::new(screen)))
    }
}

fn movement_lines(ctx: &Ctx<'_>, moved: &Movement) -> Vec<Line> {
    let mut lines = vec![
        line(text::FROM, ctx.party(&moved.from, &moved.network)),
        line(text::TO, ctx.party(&moved.to, &moved.network)),
    ];
    if let Some(memo) = &moved.memo {
        lines.push(line(text::MEMO, memo.clone()));
    }
    lines
}
