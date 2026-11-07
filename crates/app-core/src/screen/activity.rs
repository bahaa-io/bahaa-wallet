use bahaa_engine::{
    Activity, ActivityKind, ChainFamily, Movement, NetworkId, Replace, SentKind, SentTx, WalletId,
};

use super::send::Send;
use super::{
    Screen, button, cell, choice, group, line, network_cell, network_line, note, picked_wallet,
    search_box, search_empty, status_tone, swap_details, swap_tone, toned_line, wallet_group,
};
use crate::action::{Action, Direction, Field, Go, Pick, Toggle};
use crate::core::{Ctx, Nav, Reply};
use crate::search::{Facts, Query};
use crate::view::{Button, Item, Level, Line, Note, Page, PageId, Request, Row, Table, Tone};
use crate::{format, text};

#[derive(Default)]
pub(crate) struct List {
    filter: Option<WalletId>,
    direction: Direction,
    show_spam: bool,
    search: String,
}

impl List {
    fn matches(&self, activity: &Activity) -> bool {
        let direction = match activity.kind {
            ActivityKind::Received(_) => Direction::Received,
            ActivityKind::Sent(_) | ActivityKind::SentElsewhere(_) | ActivityKind::Order(_) => {
                Direction::Sent
            }
        };
        self.filter.is_none_or(|wallet| activity.wallet == wallet)
            && (self.direction == Direction::All || self.direction == direction)
    }

    fn hidden(&self, activity: &Activity) -> bool {
        !self.show_spam
            && matches!(&activity.kind, ActivityKind::Received(arrival) if !arrival.cautions.is_empty())
    }
}

impl Screen for List {
    fn act(&mut self, action: &Action, ctx: &Ctx<'_>) -> Reply {
        match action {
            Action::Pick(Pick::Wallet(position)) => {
                self.filter = picked_wallet(ctx, *position);
                Reply::default()
            }
            Action::Pick(Pick::Direction(direction)) => {
                self.direction = *direction;
                Reply::default()
            }
            Action::Toggle(Toggle::HideSpam) => {
                self.show_spam = !self.show_spam;
                Reply::default()
            }
            Action::Type(Field::Search, value) => {
                self.search.clone_from(value);
                Reply::default()
            }
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
        let query = Query::new(ctx, &self.search);
        let listed: Vec<(usize, &Activity)> = ctx
            .data
            .activity
            .iter()
            .enumerate()
            .filter(|(_, activity)| self.matches(activity) && !self.hidden(activity))
            .collect();
        let total = listed.len();
        let rows: Vec<Row> = listed
            .into_iter()
            .filter(|(_, activity)| query.is_empty() || query.matches(&facts(ctx, activity)))
            .map(|(position, activity)| row(ctx, position, activity))
            .collect();
        let shown = rows.len();
        let hidden = ctx
            .data
            .activity
            .iter()
            .filter(|activity| self.matches(activity) && self.hidden(activity))
            .count();
        let empty = if ctx.data.activity.is_empty() {
            text::NO_ACTIVITY.to_owned()
        } else {
            search_empty(&self.search, text::NO_MATCHES)
        };
        let mut form = vec![search_box(
            &self.search,
            text::SEARCH_ACTIVITY,
            shown,
            total,
        )];
        form.extend(self.form(ctx));
        Page {
            id: PageId::Activity,
            title: text::ACTIVITY.to_owned(),
            form,
            table: Some(Table {
                columns: text::ACTIVITY_COLUMNS.map(str::to_owned).to_vec(),
                rows,
                empty,
            }),
            notes: Self::notes(ctx, hidden),
            ..Page::default()
        }
    }
}

impl List {
    fn notes(ctx: &Ctx<'_>, hidden: usize) -> Vec<Note> {
        let mut notes = Vec::new();
        let reading = ctx.pending(true);
        if !reading.is_empty() {
            notes.push(note(Level::Info, text::history_pending(&reading)));
        }
        if hidden > 0 {
            notes.push(note(Level::Info, text::spam_hidden(hidden)));
        }
        notes
    }

    fn form(&self, ctx: &Ctx<'_>) -> Vec<Item> {
        let directions = [Direction::All, Direction::Received, Direction::Sent]
            .into_iter()
            .map(|direction| {
                choice(
                    text::direction(direction),
                    Action::Pick(Pick::Direction(direction)),
                    self.direction == direction,
                )
            })
            .collect();
        let spam = vec![choice(
            text::HIDE,
            Action::Toggle(Toggle::HideSpam),
            !self.show_spam,
        )];
        wallet_group(ctx, self.filter)
            .into_iter()
            .map(Item::from)
            .chain([
                group(text::SHOW, directions).into(),
                group(text::SPAM, spam).into(),
            ])
            .collect()
    }
}

fn facts(ctx: &Ctx<'_>, activity: &Activity) -> Facts {
    let mut facts = Facts::default();
    match &activity.kind {
        ActivityKind::Sent(tx) => {
            facts
                .amount(tx.amount)
                .asset(ctx, &tx.asset)
                .address(ctx, &tx.from, &tx.network)
                .address(ctx, &tx.to, &tx.network)
                .id(&tx.id)
                .word(&text::status(&tx.status))
                .wallet(ctx, activity.wallet, &tx.from);
            if let Some(swap) = ctx.swap(&tx.network, &tx.id) {
                facts
                    .asset(ctx, &swap.buy)
                    .word(&text::swap_status(&swap.status));
            }
        }
        ActivityKind::SentElsewhere(moved) => {
            facts
                .moved(ctx, moved)
                .word(text::ELSEWHERE)
                .wallet(ctx, activity.wallet, &moved.from);
        }
        ActivityKind::Received(arrival) => {
            facts
                .moved(ctx, &arrival.received)
                .word(text::RECEIVED)
                .wallet(ctx, activity.wallet, &arrival.received.to);
        }
        ActivityKind::Order(swap) => {
            facts
                .amount(swap.amount)
                .asset(ctx, &swap.sell)
                .asset(ctx, &swap.buy)
                .address(ctx, &swap.from, &swap.network)
                .address(ctx, &swap.to, swap.buy.network())
                .id(&swap.id)
                .id(&swap.reference)
                .word(&swap.provider)
                .word(&text::swap_status(&swap.status))
                .wallet(ctx, activity.wallet, &swap.from);
        }
    }
    facts
}

fn row(ctx: &Ctx<'_>, position: usize, activity: &Activity) -> Row {
    let (sign, moved, party, status, warn) = match &activity.kind {
        ActivityKind::Sent(tx) if tx.kind == SentKind::Approval => (
            "",
            (tx.amount, &tx.asset),
            text::approved_for(&ctx.party(&tx.to, &tx.network)),
            cell(text::status(&tx.status), status_tone(&tx.status)),
            false,
        ),
        ActivityKind::Sent(tx) => match ctx.swap(&tx.network, &tx.id) {
            Some(swap) => (
                "−",
                (tx.amount, &tx.asset),
                text::swapped_to(
                    &ctx.symbol(&swap.buy),
                    &ctx.network_name(swap.buy.network()),
                ),
                cell(text::swap_status(&swap.status), swap_tone(&swap.status)),
                false,
            ),
            None => (
                "−",
                (tx.amount, &tx.asset),
                ctx.party(&tx.to, &tx.network),
                cell(text::status(&tx.status), status_tone(&tx.status)),
                false,
            ),
        },
        ActivityKind::SentElsewhere(moved) => (
            "−",
            (moved.amount, &moved.asset),
            ctx.party(&moved.to, &moved.network),
            cell(text::ELSEWHERE, Tone::Muted),
            false,
        ),
        ActivityKind::Order(swap) => (
            "−",
            (swap.amount, &swap.sell),
            text::swapped_to(
                &ctx.symbol(&swap.buy),
                &ctx.network_name(swap.buy.network()),
            ),
            cell(text::swap_status(&swap.status), swap_tone(&swap.status)),
            false,
        ),
        ActivityKind::Received(arrival) => {
            let warn = !arrival.cautions.is_empty();
            (
                "+",
                (arrival.received.amount, &arrival.received.asset),
                ctx.party(&arrival.received.from, &arrival.received.network),
                cell(
                    text::RECEIVED,
                    if warn { Tone::Warning } else { Tone::Good },
                ),
                warn,
            )
        }
    };
    let (amount, asset) = moved;
    let amount_tone = match sign {
        "+" => Tone::Good,
        "" => Tone::Muted,
        _ => Tone::Plain,
    };
    Row {
        cells: vec![
            cell(format::ago(activity.time, ctx.now), Tone::Muted),
            cell(
                format!("{sign}{}", ctx.amount(amount, &ctx.symbol(asset))),
                amount_tone,
            ),
            network_cell(ctx, asset.network()),
            party.into(),
            status,
        ],
        open: Some(Action::Go(Go::Transfer(position))),
        warn,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Key {
    Sent(NetworkId, String),
    Moved(NetworkId, String, u64, bool),
    Order(NetworkId, String),
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
        ActivityKind::Order(swap) => Key::Order(swap.network.clone(), swap.id.clone()),
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

    pub(crate) fn order(network: NetworkId, id: String) -> Self {
        Self {
            key: Key::Order(network, id),
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
            Key::Sent(network, id) | Key::Moved(network, id, ..) | Key::Order(network, id) => {
                (network, id)
            }
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
            ActivityKind::Sent(tx) => sent_page(ctx, tx, &mut buttons),
            ActivityKind::SentElsewhere(moved) => (
                text::ELSEWHERE_TITLE,
                ctx.exact(moved.amount, &ctx.symbol(&moved.asset)),
                movement_lines(ctx, moved),
                Vec::new(),
            ),
            ActivityKind::Order(swap) => {
                buttons.retain(|button| button.action == Action::Copy);
                let mut lines = vec![toned_line(
                    text::STATUS,
                    text::swap_status(&swap.status),
                    swap_tone(&swap.status),
                )];
                lines.extend(swap_details(ctx, swap));
                (
                    text::ORDER_TITLE,
                    ctx.exact(swap.amount, &ctx.symbol(&swap.sell)),
                    lines,
                    vec![note(Level::Info, text::ORDER_NOTE)],
                )
            }
            ActivityKind::Received(arrival) => (
                text::RECEIVED_TITLE,
                ctx.exact(
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
        lines.push(network_line(ctx, network));
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

fn sent_page(
    ctx: &Ctx<'_>,
    tx: &SentTx,
    buttons: &mut Vec<Button>,
) -> (&'static str, String, Vec<Line>, Vec<Note>) {
    let approval = tx.kind == SentKind::Approval;
    let replaceable = tx.status.is_open()
        && tx.kind == SentKind::Transfer
        && ctx.network(&tx.network).is_some_and(|network| {
            matches!(network.family(), ChainFamily::Evm | ChainFamily::Bitcoin)
        });
    if replaceable {
        buttons.push(button(text::SPEED_UP, Action::Pick(Pick::SpeedUp)));
        buttons.push(button(text::CANCEL_SEND, Action::Pick(Pick::Cancel)));
    }
    let symbol = ctx.native_symbol(&tx.network);
    let fee = tx.fee_paid.map_or_else(
        || text::at_most(&format::exact(tx.fee_max, &symbol)),
        |paid| format::exact(paid, &symbol),
    );
    let mut lines = vec![toned_line(
        text::STATUS,
        text::status(&tx.status),
        status_tone(&tx.status),
    )];
    if tx.status.is_open() {
        lines.push(line(
            text::WAITING,
            format::span(ctx.now.saturating_sub(tx.created_at)),
        ));
    }
    lines.extend([
        line(text::FROM, ctx.party(&tx.from, &tx.network)),
        line(
            if approval { text::ALLOWED } else { text::TO },
            ctx.shown(&tx.to, &tx.network),
        ),
    ]);
    if approval {
        lines.push(line(
            text::APPROVED,
            text::exactly(&ctx.exact(tx.amount, &ctx.symbol(&tx.asset))),
        ));
    }
    lines.push(line(text::FEE, fee));
    if let Some(memo) = &tx.memo {
        lines.push(line(text::MEMO, memo.clone()));
    }
    if let Some(swap) = ctx.swap(&tx.network, &tx.id) {
        lines.push(toned_line(
            text::SWAP,
            text::swap_status(&swap.status),
            swap_tone(&swap.status),
        ));
        lines.extend(swap_details(ctx, swap));
    }
    let notes = if replaceable {
        vec![note(Level::Info, text::PENDING_NOTE)]
    } else {
        Vec::new()
    };
    (
        if approval {
            text::APPROVAL_TITLE
        } else {
            text::SENT_TITLE
        },
        ctx.exact(tx.amount, &ctx.symbol(&tx.asset)),
        lines,
        notes,
    )
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
