use bahaa_engine::connect::{Content, Meaning, Reading};
use bahaa_engine::{
    Address, Amount, AppAsk, AppId, Approved, AskId, AskKind, AskReview, ConnectedApp, DataReview,
    EngineError, Invite, KeyringError, NetworkId, NftId, TxReview, hex, to_plain_text,
};
use ruint::aliases::U256;

use super::{
    Screen, button, cell, choice, group, hidden, input, line, network_line, note, toned_line,
};
use crate::action::{Action, Field, Go, Pick};
use crate::call::{Call, Outcome};
use crate::core::{Ctx, Nav, Reply};
use crate::view::{Item, Level, Line, Page, PageId, Request, Row, Secret, Table, Tone};
use crate::{format, text};

const MAX_COMMENT: usize = 80;
const MAX_TEXT: usize = 2_000;
const MAX_LINES: usize = 40;
const MAX_SCHEMA: usize = 200;
const SHOWN_BYTES: usize = 64;
const MANY_MESSAGES: usize = 4;
const MOST_TENTHS: u8 = 9;

pub(crate) struct Connect {
    step: Joining,
    error: Option<String>,
}

enum Joining {
    Link(String),
    Invite {
        invite: Box<Invite>,
        account: usize,
        password: Secret,
    },
}

impl Connect {
    pub(crate) fn new() -> Self {
        Self {
            step: Joining::Link(String::new()),
            error: None,
        }
    }

    fn invite_page(
        &self,
        invite: &Invite,
        account: usize,
        password: &Secret,
        ctx: &Ctx<'_>,
    ) -> Page {
        let several = invite
            .accounts
            .iter()
            .any(|candidate| candidate.network != invite.accounts[0].network);
        let choices = invite
            .accounts
            .iter()
            .enumerate()
            .map(|(position, candidate)| {
                let mut parts = vec![
                    ctx.owner_label(candidate.wallet, &candidate.address),
                    format::short(&ctx.shown(&candidate.address, &candidate.network)),
                ];
                if several {
                    parts.push(ctx.network_name(&candidate.network));
                }
                choice(
                    parts.join(" · "),
                    Action::Pick(Pick::AppAccount(position)),
                    position == account,
                )
            })
            .collect();
        let mut notes = vec![
            note(Level::Warning, text::not_proven(&invite.app.domain)),
            note(Level::Info, text::CONNECT_NOTE),
        ];
        if !invite.unsupported.is_empty() {
            notes.push(note(Level::Info, text::not_offered(&invite.unsupported)));
        }
        Page {
            id: PageId::Invite,
            title: text::connect_title(&invite.app.name),
            lines: vec![
                line(text::WEBSITE, invite.app.domain.clone()),
                line(
                    text::ASKS_FOR,
                    if invite.proof {
                        text::ADDRESS_AND_PROOF
                    } else {
                        text::YOUR_ADDRESS
                    },
                ),
            ],
            form: vec![
                group(text::ACCOUNT, choices).into(),
                hidden(Field::Password, text::PASSWORD, password).into(),
            ],
            notes,
            error: self.error.clone(),
            buttons: vec![
                button(text::CONNECT, Action::Submit),
                button(text::DECLINE, Action::Pick(Pick::Decline)),
            ],
            ..Page::default()
        }
    }
}

impl Screen for Connect {
    fn act(&mut self, action: &Action, _ctx: &Ctx<'_>) -> Reply {
        match (&mut self.step, action) {
            (Joining::Link(typed), Action::Type(Field::Link, value)) => {
                typed.clone_from(value);
                self.error = None;
            }
            (Joining::Link(typed), Action::Submit) => {
                if typed.trim().is_empty() {
                    self.error = Some(text::PASTE_LINK.to_owned());
                } else {
                    return Reply::call(Call::ConnectApp(typed.trim().to_owned()));
                }
            }
            (
                Joining::Invite {
                    invite, account, ..
                },
                Action::Pick(Pick::AppAccount(position)),
            ) if *position < invite.accounts.len() => {
                *account = *position;
            }
            (Joining::Invite { password, .. }, Action::Type(Field::Password, value)) => {
                *password = Secret::new(value);
            }
            (
                Joining::Invite {
                    invite,
                    account,
                    password,
                },
                Action::Submit,
            ) => {
                return Reply::call(Call::ApproveApp(
                    invite.id,
                    invite.accounts[*account].clone(),
                    super::secret(password),
                ));
            }
            (Joining::Invite { invite, .. }, Action::Pick(Pick::Decline)) => {
                return declined(Call::RejectApp(invite.id));
            }
            _ => {}
        }
        Reply::default()
    }

    fn done(&mut self, outcome: Outcome, _ctx: &Ctx<'_>) -> Reply {
        match outcome {
            Outcome::Invited(Ok(invite)) => {
                self.error = None;
                self.step = Joining::Invite {
                    invite,
                    account: 0,
                    password: Secret::default(),
                };
            }
            Outcome::Invited(Err(err)) => self.error = Some(text::error(&err)),
            Outcome::Connected(Ok(app)) => {
                return Reply::nav(Nav::Back)
                    .with_note(Level::Success, text::connected_to(&app.app.name));
            }
            Outcome::Connected(Err(EngineError::Keyring(KeyringError::WrongPassword))) => {
                if let Joining::Invite { password, .. } = &mut self.step {
                    *password = Secret::default();
                }
                self.error = Some(text::error(&KeyringError::WrongPassword.into()));
            }
            Outcome::Connected(Err(EngineError::RequestExpired | EngineError::UnknownRequest)) => {
                self.step = Joining::Link(String::new());
                self.error = Some(text::INVITE_GONE.to_owned());
            }
            Outcome::Connected(Err(err)) => {
                self.step = Joining::Link(String::new());
                self.error = Some(text::error(&err));
            }
            _ => {}
        }
        Reply::default()
    }

    fn back(&mut self) -> Option<Reply> {
        match &self.step {
            Joining::Link(_) => None,
            Joining::Invite { invite, .. } => Some(declined(Call::RejectApp(invite.id))),
        }
    }

    fn page(&self, ctx: &Ctx<'_>) -> Page {
        match &self.step {
            Joining::Link(typed) => Page {
                id: PageId::ConnectApp,
                title: text::CONNECT_APP.to_owned(),
                text: Some(text::CONNECT_APP_TEXT.to_owned()),
                form: vec![input(Field::Link, text::LINK, typed).into()],
                error: self.error.clone(),
                buttons: vec![button(text::CONTINUE, Action::Submit)],
                ..Page::default()
            },
            Joining::Invite {
                invite,
                account,
                password,
            } => self.invite_page(invite, *account, password, ctx),
        }
    }
}

fn declined(call: Call) -> Reply {
    Reply {
        nav: Some(Nav::Back),
        call: Some(call),
        ..Reply::default()
    }
    .with_note(Level::Info, text::DECLINED)
}

fn account_label(ctx: &Ctx<'_>, address: &Address, network: &NetworkId) -> String {
    let shown = format::short(&ctx.shown(address, network));
    match ctx.owner(address) {
        Some(wallet) => text::with_name(&shown, &ctx.owner_label(wallet, address)),
        None => shown,
    }
}

pub(crate) struct Apps;

impl Screen for Apps {
    fn act(&mut self, action: &Action, ctx: &Ctx<'_>) -> Reply {
        match action {
            Action::Go(Go::App(row)) => ctx
                .data
                .connections
                .apps
                .get(*row)
                .map_or_else(Reply::default, |app| {
                    Reply::nav(Nav::Push(Box::new(AppPage::new(app))))
                }),
            _ => Reply::default(),
        }
    }

    fn page(&self, ctx: &Ctx<'_>) -> Page {
        let connections = &ctx.data.connections;
        let rows = connections
            .apps
            .iter()
            .enumerate()
            .map(|(position, app)| Row {
                cells: vec![
                    app.app.name.clone().into(),
                    cell(app.app.domain.clone(), Tone::Muted),
                    account_label(ctx, &app.address, &app.network).into(),
                    cell(format::ago(app.connected_at, ctx.now), Tone::Muted),
                ],
                open: Some(Action::Go(Go::App(position))),
                warn: false,
            })
            .collect();
        let mut buttons = vec![button(text::CONNECT_APP, Action::Go(Go::ConnectApp))];
        if !connections.asks.is_empty() {
            buttons.push(button(text::REQUESTS, Action::Go(Go::Requests)));
        }
        Page {
            id: PageId::Apps,
            title: text::CONNECTED_APPS.to_owned(),
            table: Some(Table {
                columns: text::APP_COLUMNS.map(str::to_owned).to_vec(),
                rows,
                empty: text::NO_APPS.to_owned(),
            }),
            buttons,
            ..Page::default()
        }
    }
}

struct AppPage {
    id: AppId,
    name: String,
}

impl AppPage {
    fn new(app: &ConnectedApp) -> Self {
        Self {
            id: app.id.clone(),
            name: app.app.name.clone(),
        }
    }

    fn app<'a>(&self, ctx: &'a Ctx<'_>) -> Option<&'a ConnectedApp> {
        ctx.data
            .connections
            .apps
            .iter()
            .find(|app| app.id == self.id)
    }
}

impl Screen for AppPage {
    fn act(&mut self, action: &Action, ctx: &Ctx<'_>) -> Reply {
        match (action, self.app(ctx)) {
            (Action::Pick(Pick::Disconnect), Some(_)) => {
                Reply::call(Call::DisconnectApp(self.id.clone()))
            }
            (Action::Explore, Some(app)) if app.app.url.starts_with("https://") => {
                Reply::request(Request::Open(app.app.url.clone()))
            }
            _ => Reply::default(),
        }
    }

    fn done(&mut self, outcome: Outcome, _ctx: &Ctx<'_>) -> Reply {
        match outcome {
            Outcome::Done(Ok(())) => {
                Reply::nav(Nav::Back).with_note(Level::Success, text::disconnected_from(&self.name))
            }
            Outcome::Done(Err(err)) => Reply::note(Level::Error, text::error(&err)),
            _ => Reply::default(),
        }
    }

    fn page(&self, ctx: &Ctx<'_>) -> Page {
        let Some(app) = self.app(ctx) else {
            return Page {
                id: PageId::App,
                title: self.name.clone(),
                text: Some(text::APP_GONE.to_owned()),
                ..Page::default()
            };
        };
        let mut buttons = vec![button(text::DISCONNECT, Action::Pick(Pick::Disconnect))];
        if app.app.url.starts_with("https://") {
            buttons.push(button(text::OPEN_WEBSITE, Action::Explore));
        }
        Page {
            id: PageId::App,
            title: app.app.name.clone(),
            lines: vec![
                line(text::WEBSITE, app.app.domain.clone()),
                line(
                    text::ACCOUNT,
                    account_label(ctx, &app.address, &app.network),
                ),
                network_line(ctx, &app.network),
                line(text::CONNECTED_AT, format::when(app.connected_at)),
            ],
            buttons,
            ..Page::default()
        }
    }
}

pub(crate) fn requests(ctx: &Ctx<'_>) -> Reply {
    match ctx.data.connections.asks.as_slice() {
        [ask] => open(ask),
        _ => Reply::nav(Nav::Push(Box::new(Requests))),
    }
}

fn open(ask: &AppAsk) -> Reply {
    Reply {
        nav: Some(Nav::Push(Box::new(RequestPage::new(ask)))),
        call: Some(Call::ReviewRequest(ask.id)),
        ..Reply::default()
    }
}

struct Requests;

impl Screen for Requests {
    fn act(&mut self, action: &Action, ctx: &Ctx<'_>) -> Reply {
        match action {
            Action::Go(Go::Request(row)) => ctx
                .data
                .connections
                .asks
                .get(*row)
                .map_or_else(Reply::default, open),
            _ => Reply::default(),
        }
    }

    fn page(&self, ctx: &Ctx<'_>) -> Page {
        let rows = ctx
            .data
            .connections
            .asks
            .iter()
            .enumerate()
            .map(|(position, ask)| Row {
                cells: vec![
                    ask.name.clone().into(),
                    text::ask_kind(ask.kind).to_owned().into(),
                    cell(
                        format::short_span(ask.expires_at.saturating_sub(ctx.now)),
                        Tone::Waiting,
                    ),
                ],
                open: Some(Action::Go(Go::Request(position))),
                warn: false,
            })
            .collect();
        Page {
            id: PageId::Requests,
            title: text::REQUESTS.to_owned(),
            table: Some(Table {
                columns: text::REQUEST_COLUMNS.map(str::to_owned).to_vec(),
                rows,
                empty: text::NO_REQUESTS.to_owned(),
            }),
            ..Page::default()
        }
    }
}

struct RequestPage {
    id: AskId,
    about: About,
    step: Asked,
    error: Option<String>,
    approving: bool,
}

struct About {
    name: String,
    domain: String,
    kind: AskKind,
    expires_at: u64,
}

impl About {
    fn header(&self, ctx: &Ctx<'_>, from: &Address, network: &NetworkId) -> Vec<Line> {
        vec![
            line(text::WEBSITE, self.domain.clone()),
            line(text::ACCOUNT, account_label(ctx, from, network)),
            network_line(ctx, network),
            toned_line(
                text::TIME_LEFT,
                format::short_span(self.expires_at.saturating_sub(ctx.now)),
                Tone::Waiting,
            ),
        ]
    }

    fn transaction_page(&self, review: &TxReview, at: u64, ctx: &Ctx<'_>) -> Page {
        let network = &review.network;
        let native = ctx.native_symbol(network);
        let total = total(&review.readings);
        let mut lines = self.header(ctx, &review.from, network);
        let mut notes = vec![note(Level::Warning, text::not_proven(&self.domain))];
        if let Some(fee) = &review.fee {
            lines.push(line(
                text::FEE,
                text::with_value(
                    &format::exact(fee.expected(), &native),
                    &format::fiat(
                        super::native_worth(ctx, network, fee.expected()),
                        ctx.fiat(),
                    ),
                ),
            ));
            if let Some(wait) = fee.pace().wait {
                lines.push(line(text::EXPECTED_WAIT, format::wait(wait)));
            }
            if most_of(
                ctx,
                review,
                total.and_then(|total| total.checked_add(fee.max()).ok()),
            ) {
                notes.push(note(Level::Warning, text::most_balance(&native)));
            }
        } else {
            lines.push(line(text::FEE, text::PAID_BY_RELAYER));
            notes.push(note(Level::Info, text::RELAYER_NOTE));
        }
        if review.readings.iter().any(|reading| reading.deploys) {
            notes.push(note(Level::Warning, text::DEPLOYS_NOTE));
        }
        if review
            .readings
            .iter()
            .any(|reading| unreadable(&reading.meaning))
        {
            notes.push(note(Level::Warning, text::CALLS_NOTE));
        }
        if review.readings.len() > MANY_MESSAGES {
            notes.push(note(
                Level::Info,
                text::many_messages(review.readings.len()),
            ));
        }
        let stale = at + review.valid_for.as_secs() <= ctx.now;
        if stale {
            notes.push(note(Level::Info, text::REVIEW_OLD));
        }
        let rows = review
            .readings
            .iter()
            .map(|reading| message_row(ctx, network, &native, reading))
            .collect();
        Page {
            headline: Some(headline(ctx, network, &native, &review.readings, total)),
            lines,
            table: Some(Table {
                columns: text::MESSAGE_COLUMNS.map(str::to_owned).to_vec(),
                rows,
                empty: String::new(),
            }),
            notes,
            buttons: vec![button(
                if stale {
                    text::CHECK_AGAIN
                } else {
                    text::APPROVE
                },
                Action::Submit,
            )],
            ..Page::default()
        }
    }

    fn data_page(&self, review: &DataReview, ctx: &Ctx<'_>) -> Page {
        let mut lines = self.header(ctx, &review.from, &review.network);
        let shown = match &review.content {
            Content::Text(written) => {
                lines.push(line(text::KIND, text::TEXT_KIND));
                written
                    .lines()
                    .take(MAX_LINES)
                    .map(|line| to_plain_text(line, MAX_TEXT))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            Content::Binary(bytes) => {
                lines.push(line(text::KIND, text::BINARY_KIND));
                lines.push(line(text::SIZE, text::bytes(bytes.len())));
                hex_preview(bytes)
            }
            Content::Cell { schema, cell } => {
                lines.push(line(text::KIND, text::CELL_KIND));
                lines.push(line(text::SCHEMA, to_plain_text(schema, MAX_SCHEMA)));
                lines.push(line(text::SIZE, text::bytes(cell.len())));
                hex_preview(cell)
            }
        };
        Page {
            text: Some(shown),
            lines,
            notes: vec![
                note(Level::Warning, text::not_proven(&review.domain)),
                note(Level::Info, text::SIGN_DATA_NOTE),
            ],
            buttons: vec![button(text::APPROVE, Action::Submit)],
            ..Page::default()
        }
    }
}

enum Asked {
    Checking,
    Review(Box<AskReview>, u64),
    Approve(Box<AskReview>, u64, Secret),
}

impl RequestPage {
    fn new(ask: &AppAsk) -> Self {
        Self {
            id: ask.id,
            about: About {
                name: ask.name.clone(),
                domain: ask.domain.clone(),
                kind: ask.kind,
                expires_at: ask.expires_at,
            },
            step: Asked::Checking,
            error: None,
            approving: false,
        }
    }

    fn waiting(&self, ctx: &Ctx<'_>) -> bool {
        self.approving
            || ctx
                .data
                .connections
                .asks
                .iter()
                .any(|ask| ask.id == self.id)
    }

    fn base(&self, ctx: &Ctx<'_>) -> Page {
        let mut buttons = Vec::new();
        if self.waiting(ctx) {
            buttons.push(button(text::DECLINE, Action::Pick(Pick::Decline)));
        }
        Page {
            id: PageId::Request,
            title: text::ask_title(&self.about.name, self.about.kind),
            error: self.error.clone(),
            buttons,
            ..Page::default()
        }
    }

    fn review_page(&self, review: &AskReview, at: u64, ctx: &Ctx<'_>) -> Page {
        let base = self.base(ctx);
        let mut page = match review {
            AskReview::Transaction(review) => self.about.transaction_page(review, at, ctx),
            AskReview::Data(review) => self.about.data_page(review, ctx),
        };
        page.buttons.extend(base.buttons);
        Page {
            id: base.id,
            title: base.title,
            error: base.error,
            ..page
        }
    }

    fn submit(&mut self, ctx: &Ctx<'_>) -> Reply {
        let id = self.id;
        match std::mem::replace(&mut self.step, Asked::Checking) {
            Asked::Checking => Reply::call(Call::ReviewRequest(id)),
            Asked::Review(review, at) if stale(&review, at, ctx.now) => {
                self.step = Asked::Review(review, at);
                Reply::call(Call::ReviewRequest(id))
            }
            Asked::Review(review, at) => {
                self.step = Asked::Approve(review, at, Secret::default());
                self.error = None;
                Reply::default()
            }
            Asked::Approve(review, at, password) => {
                let call = Call::ApproveRequest(id, super::secret(&password));
                self.step = Asked::Approve(review, at, password);
                self.approving = true;
                Reply::call(call)
            }
        }
    }
}

impl Screen for RequestPage {
    fn act(&mut self, action: &Action, ctx: &Ctx<'_>) -> Reply {
        match action {
            Action::Pick(Pick::Decline) if self.waiting(ctx) => {
                declined(Call::RejectRequest(self.id))
            }
            Action::Type(Field::Password, value) => {
                if let Asked::Approve(_, _, password) = &mut self.step {
                    *password = Secret::new(value);
                }
                Reply::default()
            }
            Action::Submit => self.submit(ctx),
            _ => Reply::default(),
        }
    }

    fn done(&mut self, outcome: Outcome, ctx: &Ctx<'_>) -> Reply {
        self.approving = false;
        match outcome {
            Outcome::Asked(Ok(review)) => {
                self.error = None;
                self.step = Asked::Review(review, ctx.now);
            }
            Outcome::Approved(Ok(approved)) => {
                let sent = matches!(approved, Approved::Sent(_));
                return Reply::nav(Nav::Back)
                    .with_note(Level::Success, text::answered(&self.about.name, sent));
            }
            Outcome::Approved(Err(EngineError::NotReviewed)) => {
                self.step = Asked::Checking;
                self.error = Some(text::REVIEW_OLD.to_owned());
                return Reply::call(Call::ReviewRequest(self.id));
            }
            Outcome::Approved(Err(err)) => {
                if let Asked::Approve(_, _, password) = &mut self.step {
                    *password = Secret::default();
                }
                self.error = Some(text::error(&err));
            }
            Outcome::Asked(Err(err)) | Outcome::Done(Err(err)) => {
                self.error = Some(text::error(&err));
            }
            _ => {}
        }
        Reply::default()
    }

    fn back(&mut self) -> Option<Reply> {
        match std::mem::replace(&mut self.step, Asked::Checking) {
            Asked::Approve(review, at, _) => {
                self.step = Asked::Review(review, at);
                self.error = None;
                Some(Reply::default())
            }
            other => {
                self.step = other;
                None
            }
        }
    }

    fn page(&self, ctx: &Ctx<'_>) -> Page {
        if !self.waiting(ctx) {
            return Page {
                id: PageId::Request,
                title: text::ask_title(&self.about.name, self.about.kind),
                text: Some(text::REQUEST_GONE.to_owned()),
                ..Page::default()
            };
        }
        match &self.step {
            Asked::Checking => {
                let mut page = self.base(ctx);
                page.text = Some(text::CHECKING_REQUEST.to_owned());
                if self.error.is_some() {
                    page.buttons
                        .insert(0, button(text::CHECK_AGAIN, Action::Submit));
                }
                page
            }
            Asked::Review(review, at) => self.review_page(review, *at, ctx),
            Asked::Approve(_, _, password) => Page {
                id: PageId::Approve,
                title: text::APPROVE_TITLE.to_owned(),
                form: vec![Item::from(hidden(
                    Field::Password,
                    text::PASSWORD,
                    password,
                ))],
                error: self.error.clone(),
                buttons: vec![button(
                    if self.about.kind == AskKind::Send {
                        text::SEND_NOW
                    } else {
                        text::SIGN_NOW
                    },
                    Action::Submit,
                )],
                ..Page::default()
            },
        }
    }
}

fn stale(review: &AskReview, at: u64, now: u64) -> bool {
    match review {
        AskReview::Transaction(review) => super::expired(at, review.valid_for, now),
        AskReview::Data(_) => false,
    }
}

fn total(readings: &[Reading]) -> Option<Amount> {
    let (first, rest) = readings.split_first()?;
    rest.iter().try_fold(first.amount, |sum, reading| {
        sum.checked_add(reading.amount).ok()
    })
}

fn headline(
    ctx: &Ctx<'_>,
    network: &NetworkId,
    native: &str,
    readings: &[Reading],
    total: Option<Amount>,
) -> String {
    match readings {
        [
            Reading {
                meaning: Meaning::Token { master, amount, .. },
                ..
            },
        ] => token_amount(ctx, network, master, *amount),
        _ => total.map_or_else(
            || text::NO_VALUE.to_owned(),
            |total| format::exact(total, native),
        ),
    }
}

fn token_amount(ctx: &Ctx<'_>, network: &NetworkId, master: &Address, amount: Amount) -> String {
    match ctx.coin(&bahaa_engine::AssetId::Token(network.clone(), *master)) {
        Some(coin) => format::exact(amount, &coin.symbol),
        None => text::of_token(
            &amount.to_string(),
            &format::short(&ctx.shown(master, network)),
        ),
    }
}

fn unreadable(meaning: &Meaning) -> bool {
    matches!(
        meaning,
        Meaning::Call { .. } | Meaning::ForeignToken | Meaning::ForeignNft
    )
}

fn message_row(ctx: &Ctx<'_>, network: &NetworkId, native: &str, reading: &Reading) -> Row {
    let party = |to: &Address| ctx.party(to, network);
    let what = match &reading.meaning {
        Meaning::Coins { comment: None } => text::SENDS_COINS.to_owned(),
        Meaning::Coins {
            comment: Some(comment),
        } => text::comment(&to_plain_text(comment, MAX_COMMENT)),
        Meaning::Token { master, amount, to } => {
            text::moves(&token_amount(ctx, network, master, *amount), &party(to))
        }
        Meaning::Nft { item, to } => {
            let name = NftId::new(*item, None).map_or_else(
                |_| text::NFT.to_owned(),
                |id| ctx.symbol(&bahaa_engine::AssetId::Nft(network.clone(), Box::new(id))),
            );
            text::moves(&name, &party(to))
        }
        Meaning::ForeignToken => text::UNKNOWN_TOKEN.to_owned(),
        Meaning::ForeignNft => text::UNKNOWN_NFT.to_owned(),
        Meaning::Record { wallet: Some(to) } => text::points_name(&party(to)),
        Meaning::Record { wallet: None } => text::UNLINKS_NAME.to_owned(),
        Meaning::Call { op } => op.map_or_else(|| text::CONTRACT_CALL.to_owned(), text::call_op),
    };
    let warn = reading.deploys
        || unreadable(&reading.meaning)
        || matches!(reading.meaning, Meaning::Record { .. });
    let what = if reading.deploys {
        text::and_deploys(&what)
    } else {
        what
    };
    Row {
        cells: vec![
            party(&reading.to).into(),
            format::exact(reading.amount, native).into(),
            cell(what, if warn { Tone::Warning } else { Tone::Plain }),
        ],
        open: None,
        warn,
    }
}

fn most_of(ctx: &Ctx<'_>, review: &TxReview, spent: Option<Amount>) -> bool {
    let native = bahaa_engine::AssetId::Native(review.network.clone());
    let balance = ctx
        .data
        .portfolio
        .holdings
        .iter()
        .find(|holding| holding.address == review.from && holding.asset == native)
        .map(|holding| holding.amount);
    match (spent, balance) {
        (Some(spent), Some(balance)) => mostly(spent, balance),
        _ => false,
    }
}

fn mostly(spent: Amount, balance: Amount) -> bool {
    spent.units().saturating_mul(U256::from(10u8))
        >= balance.units().saturating_mul(U256::from(MOST_TENTHS))
}

fn hex_preview(bytes: &[u8]) -> String {
    let shown = &bytes[..bytes.len().min(SHOWN_BYTES)];
    let mut preview = format::grouped(&hex::encode(shown));
    if bytes.len() > SHOWN_BYTES {
        preview.push_str(" …");
    }
    preview
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bahaa_engine::{FeeItem, FeeLine, FeeQuote, Pace, Registry, Wait};

    use super::*;
    use crate::core::{Data, Host};

    fn party(fill: u8) -> Address {
        Address::Ton {
            workchain: 0,
            hash: [fill; 32],
        }
    }

    fn gram(text: &str) -> Amount {
        Amount::parse(text, 9).unwrap()
    }

    fn about(kind: AskKind) -> About {
        About {
            name: "Demo App".to_owned(),
            domain: "demo.example.org".to_owned(),
            kind,
            expires_at: 1_000 + 240,
        }
    }

    fn review(fee: Option<FeeQuote>, registry: &Registry) -> TxReview {
        let network: NetworkId = "ton".parse().unwrap();
        let usdt = registry.get(&network).unwrap().tokens[0].contract;
        TxReview {
            from: party(1),
            network,
            readings: vec![
                Reading {
                    to: party(2),
                    bounce: false,
                    amount: gram("0.1"),
                    meaning: Meaning::Coins {
                        comment: Some("for the\u{202e} app".to_owned()),
                    },
                    deploys: false,
                },
                Reading {
                    to: party(3),
                    bounce: true,
                    amount: gram("0.05"),
                    meaning: Meaning::Token {
                        master: usdt,
                        amount: Amount::parse("1.5", 6).unwrap(),
                        to: party(4),
                    },
                    deploys: false,
                },
                Reading {
                    to: party(5),
                    bounce: true,
                    amount: gram("0.2"),
                    meaning: Meaning::Call {
                        op: Some(0x1234_5678),
                    },
                    deploys: true,
                },
            ],
            fee,
            valid_for: Duration::from_secs(120),
        }
    }

    fn quote() -> FeeQuote {
        let lines = vec![FeeLine {
            item: FeeItem::Forwarding,
            amount: gram("0.001"),
        }];
        FeeQuote::new(gram("0.002"), gram("0.001"), lines)
            .unwrap()
            .paced(Pace {
                rate: None,
                wait: Some(Wait::About(20)),
            })
    }

    fn drawn(about: &About, review: &TxReview, now: u64) -> Page {
        let registry = Registry::builtin().unwrap();
        let data = Data::default();
        let host = Host {
            folder: String::new(),
            updates: false,
        };
        let ctx = Ctx {
            registry: &registry,
            data: &data,
            host: &host,
            now,
        };
        about.transaction_page(review, 1_000, &ctx)
    }

    #[test]
    fn a_send_shows_every_message_its_fee_and_its_risks() {
        let registry = Registry::builtin().unwrap();
        let page = drawn(
            &about(AskKind::Send),
            &review(Some(quote()), &registry),
            1_010,
        );
        assert_eq!(page.headline.as_deref(), Some("0.35 GRAM"));
        let lines: Vec<(&str, &str)> = page
            .lines
            .iter()
            .map(|line| (line.label.as_str(), line.value.as_str()))
            .collect();
        assert!(lines.contains(&("Website", "demo.example.org")));
        assert!(lines.contains(&("Time left", "3 min")));
        assert!(
            lines
                .iter()
                .any(|(label, value)| *label == "Fee" && value.starts_with("0.001 GRAM"))
        );
        let what: Vec<&str> = page
            .table
            .as_ref()
            .unwrap()
            .rows
            .iter()
            .map(|row| row.cells[2].text.as_str())
            .collect();
        assert_eq!(what[0], "Comment “for the app”");
        assert!(what[1].starts_with("Sends 1.5 USDT to "), "{}", what[1]);
        assert_eq!(what[2], "Contract call 0x12345678, creates a contract");
        assert!(page.table.as_ref().unwrap().rows[2].warn);
        let notes: Vec<&str> = page.notes.iter().map(|note| note.text.as_str()).collect();
        assert!(notes[0].contains("demo.example.org"));
        assert!(notes.contains(&text::DEPLOYS_NOTE));
        assert!(notes.contains(&text::CALLS_NOTE));
        assert_eq!(page.buttons[0].label, text::APPROVE);
        let old = drawn(
            &about(AskKind::Send),
            &review(Some(quote()), &registry),
            1_200,
        );
        assert_eq!(old.buttons[0].label, text::CHECK_AGAIN);
        assert!(old.notes.iter().any(|note| note.text == text::REVIEW_OLD));
    }

    #[test]
    fn a_relayed_message_says_who_pays_the_fee() {
        let registry = Registry::builtin().unwrap();
        let page = drawn(
            &about(AskKind::SignMessage),
            &review(None, &registry),
            1_010,
        );
        assert!(
            page.lines
                .iter()
                .any(|line| line.label == "Fee" && line.value == text::PAID_BY_RELAYER)
        );
        assert!(
            page.notes
                .iter()
                .any(|note| note.text == text::RELAYER_NOTE)
        );
    }

    #[test]
    fn most_of_the_balance_starts_at_ninety_percent() {
        assert!(mostly(gram("0.9"), gram("1")));
        assert!(!mostly(gram("0.89"), gram("1")));
        assert_eq!(hex_preview(&[1, 2, 3]), "0102 03");
        assert!(hex_preview(&[0; 70]).ends_with(" …"));
    }
}
