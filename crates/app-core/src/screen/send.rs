use std::cmp::Ordering;

use bahaa_engine::{
    Address, Amount, AssetId, Caution, ChainFamily, Contact, Draft, EngineError, Holding, MAX_MEMO,
    NetworkId, Replace, SendRequest, Spend, WalletId, is_plain_text,
};

use super::activity::Transfer;
use super::{
    Screen, asset_cell, button, cell, choice, group, hidden, input, line, network_cell,
    network_line, note, status_tone, toned_line,
};
use crate::action::{Action, Field, Go, Pick, Tab, Toggle};
use crate::call::{Call, Outcome};
use crate::core::{Ctx, Nav, Reply};
use crate::links::{self, Payment};
use crate::view::{Input, Level, Note, Page, PageId, Row, Secret, Table, Tone};
use crate::{format, parse, text};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Source {
    pub wallet: WalletId,
    pub address: Address,
    pub asset: AssetId,
}

enum Step {
    Choose,
    Compose,
    Review {
        draft: Draft,
        at: u64,
    },
    Confirm {
        draft: Draft,
        at: u64,
        password: Secret,
    },
    Sent {
        network: NetworkId,
        id: String,
    },
}

pub(crate) struct Send {
    source: Option<Source>,
    picked: bool,
    to_text: String,
    to: Option<Address>,
    to_problem: Option<String>,
    cautions: Vec<Caution>,
    amount_text: String,
    max: bool,
    memo: String,
    replacing: Option<(NetworkId, String, Replace)>,
    step: Step,
    error: Option<String>,
}

impl Send {
    pub(crate) fn new(source: Option<Source>) -> Self {
        let step = if source.is_some() {
            Step::Compose
        } else {
            Step::Choose
        };
        Self {
            source,
            picked: false,
            to_text: String::new(),
            to: None,
            to_problem: None,
            cautions: Vec::new(),
            amount_text: String::new(),
            max: false,
            memo: String::new(),
            replacing: None,
            step,
            error: None,
        }
    }

    pub(crate) fn replacing(network: NetworkId, id: String, how: Replace) -> (Self, Call) {
        let call = Call::Replace(network.clone(), id.clone(), how);
        let screen = Self {
            replacing: Some((network, id, how)),
            step: Step::Compose,
            ..Self::new(None)
        };
        (screen, call)
    }

    fn sources<'a>(ctx: &'a Ctx<'_>) -> Vec<&'a Holding> {
        let mut held: Vec<&Holding> = ctx
            .data
            .portfolio
            .holdings
            .iter()
            .filter(|holding| !holding.amount.is_zero())
            .collect();
        held.sort_by(|a, b| {
            b.value
                .partial_cmp(&a.value)
                .unwrap_or(Ordering::Equal)
                .then_with(|| a.symbol.cmp(&b.symbol))
        });
        held
    }

    fn held<'a>(&self, ctx: &'a Ctx<'_>) -> Option<&'a Holding> {
        let source = self.source.as_ref()?;
        ctx.data.portfolio.holdings.iter().find(|holding| {
            holding.wallet == source.wallet
                && holding.address == source.address
                && holding.asset == source.asset
        })
    }

    fn family(&self, ctx: &Ctx<'_>) -> Option<ChainFamily> {
        let source = self.source.as_ref()?;
        ctx.network(source.asset.network())
            .map(bahaa_engine::Network::family)
    }

    fn memo_allowed(&self, ctx: &Ctx<'_>) -> bool {
        match (&self.source, self.family(ctx)) {
            (Some(source), Some(family)) => super::memo_allowed(&source.asset, family),
            _ => false,
        }
    }

    fn contacts<'a>(&self, ctx: &'a Ctx<'_>) -> Vec<&'a Contact> {
        let Some(source) = &self.source else {
            return Vec::new();
        };
        if self.to.is_some() {
            return Vec::new();
        }
        let wanted = self.to_text.trim().to_lowercase();
        ctx.data
            .contacts
            .iter()
            .filter(|contact| contact.network == *source.asset.network())
            .filter(|contact| wanted.is_empty() || contact.name.to_lowercase().contains(&wanted))
            .collect()
    }

    fn set_to(&mut self, ctx: &Ctx<'_>) -> Reply {
        self.to = None;
        self.to_problem = None;
        self.cautions.clear();
        self.error = None;
        if self.to_text.trim().is_empty() {
            return Reply::default();
        }
        if let Some(read) = links::read(&self.to_text, ctx.registry) {
            return match read {
                Ok(payment) => self.pay(payment, ctx),
                Err(problem) => {
                    self.to_problem = Some(problem);
                    Reply::default()
                }
            };
        }
        let Some(family) = self.family(ctx) else {
            return Reply::default();
        };
        match parse::address(&self.to_text, family) {
            Ok(address) => self.recipient(address, ctx),
            Err(problem) => {
                self.to_problem = Some(problem);
                Reply::default()
            }
        }
    }

    fn pay(&mut self, payment: Payment, ctx: &Ctx<'_>) -> Reply {
        let network = payment.network.map(|network| network.id).or_else(|| {
            self.source
                .as_ref()
                .map(|source| source.asset.network().clone())
        });
        let Some(network) = network else {
            self.to_problem = Some(text::PICK_SOURCE.to_owned());
            return Reply::default();
        };
        let asset = match payment.token {
            Some(token) => AssetId::Token(network, token),
            None => AssetId::Native(network),
        };
        if self.source.as_ref().map(|source| &source.asset) != Some(&asset) {
            let Some(holding) = Self::sources(ctx)
                .into_iter()
                .find(|holding| holding.asset == asset)
            else {
                self.to_problem = Some(text::no_funds(&ctx.symbol(&asset)));
                return Reply::default();
            };
            self.source = Some(source_of(holding));
        }
        if let (Some(units), Some(coin)) = (payment.units, ctx.coin(&asset))
            && let Ok(amount) = Amount::new(units, coin.decimals)
        {
            self.amount_text = amount.to_string();
            self.max = false;
        }
        if let Some(memo) = payment.memo {
            self.memo = memo;
        }
        self.recipient(payment.to, ctx)
    }

    fn recipient(&mut self, address: Address, ctx: &Ctx<'_>) -> Reply {
        if let Some(source) = &self.source {
            self.to_text = ctx.shown(&address, source.asset.network());
        }
        self.to = Some(address);
        Reply::call(Call::Cautions(address))
    }

    fn request(&self, ctx: &Ctx<'_>) -> Result<SendRequest, String> {
        let source = self.source.as_ref().ok_or(text::PICK_SOURCE)?;
        let to = match (self.to, self.family(ctx)) {
            (Some(to), _) => to,
            (None, _) if self.to_text.trim().is_empty() => {
                return Err(text::ENTER_RECIPIENT.to_owned());
            }
            (None, Some(family)) => parse::address(&self.to_text, family)?,
            (None, None) => return Err(text::PICK_SOURCE.to_owned()),
        };
        let account = ctx
            .account_index(source.wallet, &source.address)
            .ok_or(text::UNKNOWN_ACCOUNT)?;
        let amount = if self.max {
            Spend::Max
        } else {
            let decimals = ctx.coin(&source.asset).ok_or(text::UNKNOWN_ASSET)?.decimals;
            Spend::Exact(parse::amount(&self.amount_text, decimals)?)
        };
        let memo = match self.memo.trim() {
            "" => None,
            _ if !self.memo_allowed(ctx) => return Err(text::MEMO_NOT_HERE.to_owned()),
            memo if !is_plain_text(memo, MAX_MEMO) => return Err(text::MEMO_INVALID.to_owned()),
            memo => Some(memo.to_owned()),
        };
        Ok(SendRequest {
            wallet: source.wallet,
            account,
            asset: source.asset.clone(),
            to,
            amount,
            memo,
        })
    }

    fn quote(&self, ctx: &Ctx<'_>) -> Result<Call, String> {
        match &self.replacing {
            Some((network, id, how)) => Ok(Call::Replace(network.clone(), id.clone(), *how)),
            None => self.request(ctx).map(Call::Prepare),
        }
    }

    fn requote(&mut self, ctx: &Ctx<'_>) -> Reply {
        match self.quote(ctx) {
            Ok(call) => {
                self.error = None;
                Reply::call(call)
            }
            Err(problem) => {
                self.error = Some(problem);
                Reply::default()
            }
        }
    }

    fn choose(&mut self, position: usize, ctx: &Ctx<'_>) -> Reply {
        let Some(holding) = Self::sources(ctx).into_iter().nth(position) else {
            return Reply::default();
        };
        self.source = Some(source_of(holding));
        self.picked = true;
        self.step = Step::Compose;
        self.set_to(ctx)
    }

    fn to_hint(&self, ctx: &Ctx<'_>) -> Option<Note> {
        if let Some(problem) = &self.to_problem {
            return Some(note(Level::Error, problem.clone()));
        }
        let (to, network) = (self.to?, self.source.as_ref()?.asset.network());
        let known = match (ctx.contact(network, &to), ctx.owner(&to)) {
            (Some(contact), _) => text::known_address(&contact.name),
            (None, Some(wallet)) => text::own_address(&ctx.owner_label(wallet, &to)),
            (None, None) => text::valid_address(&ctx.network_name(network)),
        };
        Some(note(Level::Success, known))
    }

    fn amount_hint(&self, ctx: &Ctx<'_>) -> Option<Note> {
        if self.max || self.amount_text.trim().is_empty() {
            return None;
        }
        let holding = self.held(ctx)?;
        let decimals = ctx.coin(&holding.asset)?.decimals;
        let amount = match parse::amount(&self.amount_text, decimals) {
            Ok(amount) => amount,
            Err(problem) => return Some(note(Level::Error, problem)),
        };
        if amount.partial_cmp(&holding.amount) == Some(Ordering::Greater) {
            let balance = ctx.amount(holding.amount, &holding.symbol);
            return Some(note(Level::Error, text::too_much(&balance)));
        }
        let value = holding
            .price
            .and_then(|price| price.value_of(amount, ctx.fiat().places()).ok())?;
        Some(note(
            Level::Info,
            text::approx(&format::fiat(Some(value), ctx.fiat())),
        ))
    }

    fn compose(&mut self, action: &Action, ctx: &Ctx<'_>) -> Reply {
        match action {
            Action::Type(Field::To, value) => {
                self.to_text.clone_from(value);
                return self.set_to(ctx);
            }
            Action::Pick(Pick::Contact(position)) => {
                if let Some(contact) = self.contacts(ctx).get(*position) {
                    let address = contact.address;
                    return self.recipient(address, ctx);
                }
            }
            Action::Type(Field::Amount, value) => {
                self.amount_text.clone_from(value);
                self.max = false;
            }
            Action::Toggle(Toggle::Max) => self.max = !self.max,
            Action::Type(Field::Memo, value) => self.memo.clone_from(value),
            _ => {}
        }
        Reply::default()
    }

    fn choose_page(ctx: &Ctx<'_>) -> Page {
        let held = Self::sources(ctx);
        if held.is_empty() {
            return Page {
                id: PageId::SendFrom,
                title: text::SEND.to_owned(),
                text: Some(text::NOTHING_TO_SEND.to_owned()),
                buttons: vec![button(text::RECEIVE, Action::Go(Go::Tab(Tab::Receive)))],
                ..Page::default()
            };
        }
        let owners: Vec<String> = held
            .iter()
            .map(|holding| ctx.owner_label(holding.wallet, &holding.address))
            .collect();
        let several = super::several(&owners);
        let mut columns: Vec<String> = text::SEND_COLUMNS.map(str::to_owned).to_vec();
        if several {
            columns.push(text::WALLET.to_owned());
        }
        let rows = held
            .iter()
            .zip(owners)
            .enumerate()
            .map(|(position, (holding, owner))| {
                let mut cells = vec![
                    asset_cell(ctx, &holding.asset),
                    network_cell(ctx, holding.asset.network()),
                    ctx.amount(holding.amount, &holding.symbol).into(),
                    ctx.value(holding.value).into(),
                ];
                if several {
                    cells.push(cell(owner, Tone::Muted));
                }
                Row {
                    cells,
                    open: Some(Action::Pick(Pick::Source(position))),
                    warn: false,
                }
            })
            .collect();
        Page {
            id: PageId::SendFrom,
            title: text::SEND.to_owned(),
            text: Some(text::PICK_SOURCE.to_owned()),
            table: Some(Table {
                columns,
                rows,
                empty: text::NOTHING_TO_SEND.to_owned(),
            }),
            ..Page::default()
        }
    }

    fn compose_page(&self, ctx: &Ctx<'_>) -> Page {
        if let Some((_, _, how)) = &self.replacing {
            return Page {
                id: PageId::Compose,
                title: text::replace_title(*how).to_owned(),
                text: Some(text::QUOTING.to_owned()),
                error: self.error.clone(),
                buttons: vec![button(text::TRY_AGAIN, Action::Submit)],
                ..Page::default()
            };
        }
        let Some(source) = &self.source else {
            return Self::choose_page(ctx);
        };
        let network = source.asset.network();
        let mut lines = vec![
            line(
                text::FROM,
                text::owner(
                    &ctx.owner_label(source.wallet, &source.address),
                    &format::short(&ctx.shown(&source.address, network)),
                ),
            ),
            network_line(ctx, network),
        ];
        if let Some(holding) = self.held(ctx) {
            lines.push(line(
                text::BALANCE,
                text::with_value(
                    &ctx.amount(holding.amount, &holding.symbol),
                    &ctx.value(holding.value),
                ),
            ));
        }
        let contacts: Vec<_> = self
            .contacts(ctx)
            .into_iter()
            .enumerate()
            .map(|(position, contact)| {
                choice(&contact.name, Action::Pick(Pick::Contact(position)), false)
            })
            .collect();
        let mut form = vec![
            Input {
                hint: self.to_hint(ctx),
                ..input(Field::To, text::TO, &self.to_text)
            }
            .into(),
        ];
        if !contacts.is_empty() {
            form.push(group(text::CONTACTS, contacts).into());
        }
        form.extend([
            Input {
                hint: self.amount_hint(ctx),
                ..input(Field::Amount, text::AMOUNT, &self.amount_text)
            }
            .into(),
            group(
                "",
                vec![choice(
                    text::SEND_ALL,
                    Action::Toggle(Toggle::Max),
                    self.max,
                )],
            )
            .into(),
        ]);
        if self.memo_allowed(ctx) {
            form.push(input(Field::Memo, text::MEMO, &self.memo).into());
        }
        Page {
            id: PageId::Compose,
            title: text::send_title(&ctx.symbol(&source.asset)),
            lines,
            form,
            notes: self
                .cautions
                .iter()
                .map(|caution| note(Level::Warning, text::caution(caution, ctx)))
                .collect(),
            error: self.error.clone(),
            buttons: vec![button(text::REVIEW, Action::Submit)],
            ..Page::default()
        }
    }

    fn review_page(&self, draft: &Draft, at: u64, ctx: &Ctx<'_>) -> Page {
        let transfer = &draft.transfer;
        let network = transfer.asset.network();
        let fee_symbol = ctx.native_symbol(network);
        let fee_value = ctx
            .price(&AssetId::Native(network.clone()))
            .and_then(|price| {
                price
                    .value_of(draft.fee.expected(), ctx.fiat().places())
                    .ok()
            });
        let mut recipient = format::grouped(&ctx.shown(&transfer.to, network));
        if let Some(contact) = ctx.contact(network, &transfer.to) {
            recipient = text::with_name(&recipient, &contact.name);
        }
        let mut lines = vec![line(text::TO, recipient), network_line(ctx, network)];
        if let Some(memo) = &transfer.memo {
            lines.push(line(text::MEMO, memo.clone()));
        }
        lines.push(line(
            text::FEE,
            text::with_value(
                &format::exact(draft.fee.expected(), &fee_symbol),
                &format::fiat(fee_value, ctx.fiat()),
            ),
        ));
        let left = remaining(draft, at, ctx.now);
        if ctx.data.settings.developer_mode {
            let from = format::short(&ctx.shown(&transfer.from, network));
            let from = match ctx.owner(&transfer.from) {
                Some(wallet) => text::with_name(&from, &ctx.wallet_name(wallet)),
                None => from,
            };
            lines.insert(0, line(text::FROM, from));
            lines.extend(draft.fee.lines().iter().map(|fee| {
                line(
                    &text::fee_item(&fee.item),
                    format::exact(fee.amount, &fee_symbol),
                )
            }));
            lines.push(line(
                text::MOST_FEE,
                format::exact(draft.fee.max(), &fee_symbol),
            ));
            lines.push(line(text::QUOTE, text::quote_left(left)));
        }
        let mut notes: Vec<Note> = draft
            .cautions
            .iter()
            .map(|caution| note(Level::Warning, text::caution(caution, ctx)))
            .collect();
        if left == 0 {
            notes.push(note(Level::Info, text::QUOTE_GONE));
        }
        Page {
            id: PageId::Review,
            title: text::REVIEW.to_owned(),
            headline: Some(format::exact(transfer.amount, &ctx.symbol(&transfer.asset))),
            lines,
            notes,
            error: self.error.clone(),
            buttons: vec![button(
                if left == 0 {
                    text::NEW_QUOTE
                } else {
                    text::CONFIRM
                },
                Action::Submit,
            )],
            ..Page::default()
        }
    }

    fn submit(&mut self, ctx: &Ctx<'_>) -> Reply {
        match std::mem::replace(&mut self.step, Step::Compose) {
            Step::Choose => {
                self.step = Step::Choose;
                Reply::default()
            }
            Step::Compose => self.requote(ctx),
            Step::Review { draft, at } if remaining(&draft, at, ctx.now) == 0 => {
                self.step = Step::Review { draft, at };
                self.requote(ctx)
            }
            Step::Review { draft, at } => {
                self.step = Step::Confirm {
                    draft,
                    at,
                    password: Secret::default(),
                };
                Reply::default()
            }
            Step::Confirm {
                draft,
                at,
                password,
            } => {
                let call = Call::Confirm(draft.id, super::secret(&password));
                self.step = Step::Confirm {
                    draft,
                    at,
                    password,
                };
                Reply::call(call)
            }
            Step::Sent { network, id } => {
                let transfer = Transfer::sent(network.clone(), id.clone());
                self.step = Step::Sent { network, id };
                Reply::nav(Nav::Push(Box::new(transfer)))
            }
        }
    }
}

fn source_of(holding: &Holding) -> Source {
    Source {
        wallet: holding.wallet,
        address: holding.address,
        asset: holding.asset.clone(),
    }
}

fn remaining(draft: &Draft, at: u64, now: u64) -> u64 {
    (at + draft.valid_for.as_secs()).saturating_sub(now)
}

impl Screen for Send {
    fn act(&mut self, action: &Action, ctx: &Ctx<'_>) -> Reply {
        match (&mut self.step, action) {
            (Step::Choose, Action::Pick(Pick::Source(position))) => self.choose(*position, ctx),
            (_, Action::Submit) => self.submit(ctx),
            (Step::Confirm { password, .. }, Action::Type(Field::Password, value)) => {
                *password = Secret::new(value);
                Reply::default()
            }
            (Step::Compose, _) => self.compose(action, ctx),
            _ => Reply::default(),
        }
    }

    fn done(&mut self, outcome: Outcome, ctx: &Ctx<'_>) -> Reply {
        match outcome {
            Outcome::Cautions(Ok(cautions)) => self.cautions = cautions,
            Outcome::Drafted(Ok(draft)) => {
                self.error = None;
                self.step = Step::Review { draft, at: ctx.now };
            }
            Outcome::Sent(Ok(tx)) => {
                self.step = Step::Sent {
                    network: tx.network,
                    id: tx.id,
                };
            }
            Outcome::Sent(Err(EngineError::DraftExpired | EngineError::UnknownDraft)) => {
                if let Step::Confirm { draft, .. } = &self.step {
                    self.step = Step::Review {
                        draft: draft.clone(),
                        at: 0,
                    };
                }
                self.error = Some(text::QUOTE_EXPIRED.to_owned());
            }
            Outcome::Sent(Err(err)) => {
                if let Step::Confirm { password, .. } = &mut self.step {
                    *password = Secret::default();
                }
                self.error = Some(text::error(&err));
            }
            Outcome::Drafted(Err(err)) => self.error = Some(text::error(&err)),
            _ => {}
        }
        Reply::default()
    }

    fn back(&mut self) -> Option<Reply> {
        match &self.step {
            Step::Compose if self.picked => {
                self.step = Step::Choose;
                Some(Reply::default())
            }
            Step::Choose | Step::Compose => None,
            Step::Review { .. } if self.replacing.is_some() => None,
            Step::Review { .. } => {
                self.step = Step::Compose;
                Some(Reply::default())
            }
            Step::Confirm { draft, at, .. } => {
                self.step = Step::Review {
                    draft: draft.clone(),
                    at: *at,
                };
                Some(Reply::default())
            }
            Step::Sent { .. } => Some(Reply::nav(Nav::Tab(Tab::Send))),
        }
    }

    fn page(&self, ctx: &Ctx<'_>) -> Page {
        match &self.step {
            Step::Choose => Self::choose_page(ctx),
            Step::Compose => self.compose_page(ctx),
            Step::Review { draft, at } => self.review_page(draft, *at, ctx),
            Step::Confirm { password, .. } => Page {
                id: PageId::Confirm,
                title: text::CONFIRM_TITLE.to_owned(),
                form: vec![hidden(Field::Password, text::PASSWORD, password).into()],
                error: self.error.clone(),
                buttons: vec![button(text::SEND_NOW, Action::Submit)],
                ..Page::default()
            },
            Step::Sent { network, id } => {
                let sent = ctx.sent(network, id);
                Page {
                    id: PageId::Sent,
                    title: text::SENT_TITLE.to_owned(),
                    headline: sent.map(|tx| format::exact(tx.amount, &ctx.symbol(&tx.asset))),
                    lines: vec![
                        sent.map_or_else(
                            || toned_line(text::STATUS, text::SENDING, Tone::Waiting),
                            |tx| {
                                toned_line(
                                    text::STATUS,
                                    text::status(&tx.status),
                                    status_tone(&tx.status),
                                )
                            },
                        ),
                        network_line(ctx, network),
                    ],
                    buttons: vec![
                        button(text::VIEW_TRANSFER, Action::Submit),
                        button(text::DONE, Action::Go(Go::Tab(Tab::Portfolio))),
                    ],
                    ..Page::default()
                }
            }
        }
    }
}
