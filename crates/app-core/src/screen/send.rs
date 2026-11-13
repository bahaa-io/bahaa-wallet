use bahaa_engine::{
    Address, Amount, AssetId, Caution, ChainFamily, Contact, Draft, EngineError, FeeQuote, Holding,
    MAX_MEMO, NetworkId, Replace, Resolved, SendRequest, Speed, Spend, WalletId, is_plain_text,
};

use super::activity::Transfer;
use super::{
    Screen, button, choice, group, hidden, input, kept_aside, line, native_worth, network_line,
    note, speed, status_tone, toned_line,
};
use crate::action::{Action, Field, Go, Pick, Tab, Toggle};
use crate::call::{Call, Outcome};
use crate::core::{Ctx, Nav, Reply};
use crate::links::{self, Payment};
use crate::view::{Input, Level, Line, Note, Page, PageId, Secret, Tone};
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
    Custom {
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
    name: Option<Resolved>,
    looking: bool,
    cautions: Vec<Caution>,
    amount_text: String,
    max: bool,
    memo: String,
    replacing: Option<(NetworkId, String, Replace)>,
    speed: Speed,
    custom: String,
    step: Step,
    error: Option<String>,
    search: String,
}

impl Send {
    pub(crate) fn new(source: Option<Source>) -> Self {
        let step = if source.is_some() {
            Step::Compose
        } else {
            Step::Choose
        };
        let one = source.as_ref().is_some_and(|source| source.asset.is_nft());
        Self {
            source,
            picked: false,
            to_text: String::new(),
            to: None,
            to_problem: None,
            name: None,
            looking: false,
            cautions: Vec::new(),
            amount_text: if one { "1".to_owned() } else { String::new() },
            max: false,
            memo: String::new(),
            replacing: None,
            speed: Speed::Normal,
            custom: String::new(),
            step,
            error: None,
            search: String::new(),
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

    fn held<'a>(&self, ctx: &'a Ctx<'_>) -> Option<&'a Holding> {
        super::holding(ctx, self.source.as_ref()?)
    }

    fn nft_count(&self, ctx: &Ctx<'_>) -> Option<Amount> {
        let source = self.source.as_ref()?;
        ctx.collectible(source.wallet, &source.address, &source.asset)
            .map(|item| item.nft.amount)
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
        self.name = None;
        self.looking = false;
        self.cautions.clear();
        self.error = None;
        if self.to_text.trim().is_empty() {
            return Reply::default();
        }
        if let Some(read) = links::read(&self.to_text, ctx.registry, |asset| {
            ctx.coin(asset).map(|coin| coin.decimals)
        }) {
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
        match (parse::address(&self.to_text, family), &self.source) {
            (Ok(address), _) => self.recipient(address, ctx),
            (Err(_), Some(source)) if parse::looks_like_name(&self.to_text) => {
                self.looking = true;
                let network = source.asset.network().clone();
                Reply::call(Call::ResolveName(self.to_text.trim().to_owned(), network))
            }
            (Err(problem), _) => {
                self.to_problem = Some(problem);
                Reply::default()
            }
        }
    }

    fn resolved(&mut self, result: Result<Resolved, EngineError>) -> Reply {
        self.looking = false;
        match (&mut self.step, result) {
            (
                Step::Confirm {
                    draft, password, ..
                },
                Ok(found),
            ) if found.address == draft.transfer.to => {
                Reply::call(Call::Confirm(draft.id, super::secret(password)))
            }
            (Step::Confirm { password, .. }, Ok(_)) => {
                *password = Secret::default();
                self.error = Some(text::NAME_MOVED.to_owned());
                Reply::default()
            }
            (Step::Confirm { .. }, Err(err)) => {
                self.error = Some(text::error(&err));
                Reply::default()
            }
            (Step::Compose, Ok(found)) => {
                let address = found.address;
                self.to = Some(address);
                self.name = Some(found);
                Reply::call(Call::Cautions(address))
            }
            (Step::Compose, Err(err)) => {
                self.to_problem = Some(text::error(&err));
                Reply::default()
            }
            _ => Reply::default(),
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
            let Some(holding) = super::spendable(ctx)
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
            (None, _) if self.looking => return Err(text::STILL_LOOKING.to_owned()),
            (None, _) if let Some(problem) = &self.to_problem => return Err(problem.clone()),
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
            let decimals = if source.asset.is_nft() {
                0
            } else {
                ctx.coin(&source.asset).ok_or(text::UNKNOWN_ASSET)?.decimals
            };
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
            speed: self.speed,
        })
    }

    fn quote(&self, ctx: &Ctx<'_>) -> Result<Call, String> {
        match &self.replacing {
            Some((network, id, how)) => Ok(Call::Replace(network.clone(), id.clone(), *how)),
            None => self
                .request(ctx)
                .map(|request| Call::Prepare(Box::new(request))),
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

    fn pick_speed(&mut self, position: usize, ctx: &Ctx<'_>) -> Reply {
        if self.replacing.is_some() {
            return Reply::default();
        }
        if position == speed::CUSTOM {
            if let Step::Review { draft, at } = &self.step {
                self.step = Step::Custom {
                    draft: draft.clone(),
                    at: *at,
                };
                self.error = None;
            }
            return Reply::default();
        }
        match speed::chosen(position) {
            Some(chosen) if chosen != self.speed => {
                self.speed = chosen;
                self.requote(ctx)
            }
            _ => Reply::default(),
        }
    }

    fn choose(&mut self, position: usize, ctx: &Ctx<'_>) -> Reply {
        let Some(holding) = super::spendable(ctx).into_iter().nth(position) else {
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
        if self.looking {
            return Some(note(Level::Info, text::LOOKING_UP));
        }
        let network = self.source.as_ref()?.asset.network();
        if self.to_text.trim().is_empty() {
            let mainnet = ctx.network(network).filter(|network| !network.testnet)?;
            return Some(note(Level::Info, text::to_help(mainnet.family())));
        }
        let to = self.to?;
        if let Some(found) = &self.name {
            let shown = format::short(&ctx.shown(&to, network));
            return Some(note(
                Level::Success,
                text::name_points(found.service, &shown),
            ));
        }
        let known = match (ctx.contact(network, &to), ctx.owner(&to)) {
            (Some(contact), _) => text::known_address(&contact.name),
            (None, Some(wallet)) => text::own_address(&ctx.owner_label(wallet, &to)),
            (None, None) => text::valid_address(&ctx.network_name(network)),
        };
        Some(note(Level::Success, known))
    }

    fn amount_hint(&self, ctx: &Ctx<'_>) -> Option<Note> {
        if self.max {
            let coin = matches!(self.source.as_ref()?.asset, AssetId::Native(_));
            return Some(note(Level::Info, text::everything(coin, false)));
        }
        super::amount_hint(ctx, self.held(ctx)?, &self.amount_text)
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
            Action::Toggle(Toggle::Max) => {
                self.max = !self.max;
                let all = self
                    .held(ctx)
                    .map(|holding| holding.amount)
                    .or_else(|| self.nft_count(ctx));
                if let Some(all) = all.filter(|_| self.max) {
                    self.amount_text = all.to_string();
                }
            }
            Action::Type(Field::Memo, value) => self.memo.clone_from(value),
            _ => {}
        }
        Reply::default()
    }

    fn choose_page(&self, ctx: &Ctx<'_>) -> Page {
        super::holdings_page(
            ctx,
            PageId::SendFrom,
            (text::SEND, text::PICK_SOURCE, text::NOTHING_TO_SEND),
            &self.search,
        )
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
            return self.choose_page(ctx);
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
        let several = self
            .nft_count(ctx)
            .filter(|count| count.units() > ruint::aliases::U256::from(1u8));
        if let Some(count) = several {
            lines.push(line(text::HELD, count.to_string()));
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
        if !source.asset.is_nft() || several.is_some() {
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
        }
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
        let how = self.replacing.as_ref().map(|(_, _, how)| *how);
        let original = self
            .replacing
            .as_ref()
            .and_then(|(network, id, _)| ctx.sent(network, id));
        let named = |address: &Address| {
            let shown = format::grouped(&ctx.shown(address, network));
            match ctx.contact(network, address) {
                Some(contact) => text::with_name(&shown, &contact.name),
                None => shown,
            }
        };
        let (headline, mut lines) = match (how, original) {
            (Some(Replace::Cancel), Some(original)) => (
                format::exact(original.amount, &ctx.symbol(&original.asset)),
                vec![line(text::WAS_GOING_TO, named(&original.to))],
            ),
            _ => (
                format::exact(transfer.amount, &ctx.symbol(&transfer.asset)),
                vec![line(text::TO, named(&transfer.to))],
            ),
        };
        if let Some(found) = self.name.as_ref().filter(|_| how.is_none()) {
            lines.push(line(text::NAME, text::name_via(&found.name, found.service)));
        }
        lines.push(network_line(ctx, network));
        if let Some(memo) = &transfer.memo {
            lines.push(line(text::MEMO, memo.clone()));
        }
        let withheld = draft.fee.withheld();
        if let Some(fee) = withheld {
            let symbol = ctx.symbol(&transfer.asset);
            lines.push(line(text::TOKEN_FEE, format::exact(fee, &symbol)));
            if let Ok(arrives) = transfer.amount.checked_sub(fee) {
                lines.push(line(text::ARRIVES, format::exact(arrives, &symbol)));
            }
        }
        lines.extend(fee_lines(&draft.fee, network, ctx));
        let speeds = if how.is_none() {
            speed::table(&draft.fee, self.speed, network, ctx)
        } else {
            None
        };
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
            .chain(kept_aside(&draft.transfer.from))
            .chain(withheld.map(|_| note(Level::Info, text::WITHHELD_NOTE)))
            .collect();
        if let Some(how) = how {
            notes.push(note(Level::Info, text::replace_note(how)));
            notes.push(note(Level::Info, text::ESTIMATE_NOTE));
        } else {
            notes.extend(speed::notes(&draft.fee, self.speed, network, ctx));
        }
        if left == 0 {
            notes.push(note(Level::Info, text::QUOTE_GONE));
        }
        Page {
            id: PageId::Review,
            title: how.map_or(text::REVIEW, text::replace_title).to_owned(),
            headline: Some(headline),
            lines,
            table: speeds,
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
            Step::Custom { draft, at } => {
                let picked = speed::custom_speed(&draft.fee, &self.custom);
                self.step = Step::Custom { draft, at };
                match picked {
                    Ok(chosen) => {
                        self.speed = chosen;
                        self.requote(ctx)
                    }
                    Err(problem) => {
                        self.error = Some(problem);
                        Reply::default()
                    }
                }
            }
            Step::Confirm {
                draft,
                at,
                password,
            } => {
                let call = match &self.name {
                    Some(found) => {
                        self.looking = true;
                        let network = draft.transfer.asset.network().clone();
                        Call::ResolveName(found.name.clone(), network)
                    }
                    None => Call::Confirm(draft.id, super::secret(&password)),
                };
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

pub(crate) fn source_of(holding: &Holding) -> Source {
    Source {
        wallet: holding.wallet,
        address: holding.address,
        asset: holding.asset.clone(),
    }
}

fn fee_lines(fee: &FeeQuote, network: &NetworkId, ctx: &Ctx<'_>) -> Vec<Line> {
    let symbol = ctx.native_symbol(network);
    let valued = |amount: Amount| {
        text::with_value(
            &format::exact(amount, &symbol),
            &format::fiat(native_worth(ctx, network, amount), ctx.fiat()),
        )
    };
    let (pace, expected) = (fee.pace(), fee.expected());
    let mut lines = Vec::new();
    if let Some(before) = fee.replaced() {
        lines.push(line(
            text::FEE,
            text::changed(&format::exact(before.fee, &symbol), &valued(expected)),
        ));
        if let Ok(extra) = expected.checked_sub(before.fee)
            && !extra.is_zero()
        {
            lines.push(line(text::EXTRA_COST, valued(extra)));
        }
        if let (Some(old), Some(new)) = (before.pace.rate, pace.rate) {
            lines.push(line(
                text::rate_label(new.unit),
                text::changed(&format::rate(old), &format::rate(new)),
            ));
        }
        match (before.pace.wait, pace.wait) {
            (Some(old), Some(new)) => lines.push(line(
                text::EXPECTED_WAIT,
                text::changed(&format::wait(old), &format::wait(new)),
            )),
            (None, Some(new)) => lines.push(line(text::EXPECTED_WAIT, format::wait(new))),
            (_, None) => {}
        }
    } else {
        lines.push(line(text::FEE, valued(expected)));
        if let Some(wait) = pace.wait {
            lines.push(line(text::EXPECTED_WAIT, format::wait(wait)));
        }
        if let Some(rate) = pace.rate
            && ctx.data.settings.developer_mode
        {
            lines.push(line(text::rate_label(rate.unit), format::rate(rate)));
        }
    }
    lines
}

fn remaining(draft: &Draft, at: u64, now: u64) -> u64 {
    (at + draft.valid_for.as_secs()).saturating_sub(now)
}

impl Screen for Send {
    fn act(&mut self, action: &Action, ctx: &Ctx<'_>) -> Reply {
        match (&mut self.step, action) {
            (Step::Choose, Action::Pick(Pick::Source(position))) => self.choose(*position, ctx),
            (Step::Choose, Action::Type(Field::Search, value)) => {
                self.search.clone_from(value);
                Reply::default()
            }
            (Step::Review { .. }, Action::Pick(Pick::Speed(position))) => {
                self.pick_speed(*position, ctx)
            }
            (Step::Custom { .. }, Action::Type(Field::FeeRate, value)) => {
                self.custom.clone_from(value);
                Reply::default()
            }
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
            Outcome::Resolved(result) => return self.resolved(result),
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
            Step::Confirm { draft, at, .. } | Step::Custom { draft, at } => {
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
            Step::Choose => self.choose_page(ctx),
            Step::Compose => self.compose_page(ctx),
            Step::Review { draft, at } => self.review_page(draft, *at, ctx),
            Step::Custom { draft, .. } => {
                speed::custom_page(&draft.fee, &self.custom, self.error.clone())
            }
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

#[cfg(test)]
mod tests {
    use bahaa_engine::{
        Amount, FeeItem, FeeLine, FeeQuote, FeeRate, Pace, RateUnit, Registry, Replaced, Wait,
    };

    use super::fee_lines;
    use crate::core::{Ctx, Data, Host};

    fn btc(text: &str) -> Amount {
        Amount::parse(text, 8).unwrap()
    }

    fn sats(text: &str) -> FeeRate {
        FeeRate {
            amount: Amount::parse(text, 3).unwrap(),
            unit: RateUnit::SatPerVbyte,
        }
    }

    fn shown(quote: &FeeQuote, data: &Data) -> Vec<(String, String)> {
        let registry = Registry::builtin().unwrap();
        let host = Host {
            folder: String::new(),
            updates: false,
        };
        let ctx = Ctx {
            registry: &registry,
            data,
            host: &host,
            now: 0,
        };
        fee_lines(quote, &"bitcoin".parse().unwrap(), &ctx)
            .into_iter()
            .map(|line| (line.label, line.value))
            .collect()
    }

    fn quote(fee: &str) -> FeeQuote {
        let lines = vec![FeeLine {
            item: FeeItem::Size { vbytes: 141 },
            amount: btc(fee),
        }];
        FeeQuote::new(btc(fee), btc(fee), lines)
            .unwrap()
            .paced(Pace {
                rate: Some(sats("3")),
                wait: Some(Wait::About(1_200)),
            })
    }

    #[test]
    fn a_speed_up_compares_the_fee_rate_and_wait_with_the_pending_transfer() {
        let replacing = quote("0.00000423").replacing(Replaced {
            fee: btc("0.00000282"),
            pace: Pace {
                rate: Some(sats("2")),
                wait: Some(Wait::About(3_600)),
            },
        });
        let pairs = [
            ("Fee", "0.00000282 BTC → 0.00000423 BTC ≈ —"),
            ("Extra cost", "0.00000141 BTC ≈ —"),
            ("Fee rate", "2 sat/vB → 3 sat/vB"),
            ("Expected wait", "about 1 hour → about 20 minutes"),
        ];
        assert_eq!(
            shown(&replacing, &Data::default()),
            pairs.map(|(label, value)| (label.to_owned(), value.to_owned()))
        );
    }

    #[test]
    fn a_send_shows_its_wait_and_only_developers_see_the_rate() {
        let mut data = Data::default();
        let plain = shown(&quote("0.00000423"), &data);
        assert_eq!(
            plain[1],
            ("Expected wait".to_owned(), "about 20 minutes".to_owned())
        );
        assert_eq!(plain.len(), 2);
        data.settings.developer_mode = true;
        let detailed = shown(&quote("0.00000423"), &data);
        assert_eq!(detailed[2], ("Fee rate".to_owned(), "3 sat/vB".to_owned()));
    }
}
