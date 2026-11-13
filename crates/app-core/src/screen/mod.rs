mod activity;
mod contacts;
mod networks;
mod nfts;
mod onboarding;
mod portfolio;
mod receive;
mod send;
mod settings;
mod speed;
mod swap;
mod tokens;
mod unlock;
mod wallets;

pub(crate) use nfts::nft_name;
pub use onboarding::completions;

use std::cmp::Ordering;

use bahaa_engine::swap::{Provider, SwapRecord, SwapStatus};
use bahaa_engine::{
    Address, Amount, AssetId, BitcoinScript, ChainFamily, Holding, NetworkId, SecretString,
    SentStatus, Settings, TonContract, Updates, WalletId, WalletInfo, WalletKind, WordCount,
};

use crate::action::{Action, Field, Go, Pick, Tab, Toggle};
use crate::call::Outcome;
use crate::core::{Ctx, Reply};
use crate::search::{Facts, Query};
use crate::view::{
    Button, Cell, Choice, Group, Input, Item, Level, Line, Note, Page, PageId, Request, Row,
    Secret, Table, Tone, Value,
};
use crate::{format, parse, text};
use send::Source;

const MIN_PASSWORD: usize = 8;
const GOOD_PASSWORD: usize = 12;

pub(crate) trait Screen: Send {
    fn act(&mut self, action: &Action, ctx: &Ctx<'_>) -> Reply;

    fn done(&mut self, _outcome: Outcome, _ctx: &Ctx<'_>) -> Reply {
        Reply::default()
    }

    fn back(&mut self) -> Option<Reply> {
        None
    }

    fn page(&self, ctx: &Ctx<'_>) -> Page;
}

pub(crate) fn starting() -> Box<dyn Screen> {
    Box::new(Starting)
}

pub(crate) fn welcome(adding: bool) -> Box<dyn Screen> {
    Box::new(onboarding::Welcome::new(adding))
}

pub(crate) fn networks() -> Box<dyn Screen> {
    Box::new(networks::Networks)
}

pub(crate) fn unlock() -> Box<dyn Screen> {
    Box::new(unlock::Unlock::default())
}

pub(crate) fn tab(tab: Tab) -> Box<dyn Screen> {
    match tab {
        Tab::Portfolio => Box::new(portfolio::Portfolio::default()),
        Tab::Activity => Box::new(activity::List::default()),
        Tab::Send => Box::new(send::Send::new(None)),
        Tab::Swap => Box::new(swap::Swap::default()),
        Tab::Receive => Box::new(receive::Receive::default()),
        Tab::Contacts => Box::new(contacts::List),
        Tab::Settings => Box::new(settings::Settings),
    }
}

struct Starting;

impl Screen for Starting {
    fn act(&mut self, _action: &Action, _ctx: &Ctx<'_>) -> Reply {
        Reply::default()
    }

    fn page(&self, _ctx: &Ctx<'_>) -> Page {
        Page {
            id: PageId::Starting,
            title: text::STARTING.to_owned(),
            ..Page::default()
        }
    }
}

pub(crate) fn button(label: impl Into<String>, action: Action) -> Button {
    Button {
        label: label.into(),
        action,
        section: None,
    }
}

pub(crate) fn sections(list: impl IntoIterator<Item = (&'static str, Vec<Button>)>) -> Vec<Button> {
    list.into_iter()
        .flat_map(|(label, buttons)| {
            buttons.into_iter().map(move |button| Button {
                section: Some(label.to_owned()),
                ..button
            })
        })
        .collect()
}

pub(crate) fn toggle(label: &str, choice_label: &str, toggle: Toggle, on: bool) -> Item {
    group(
        label,
        vec![choice(choice_label, Action::Toggle(toggle), on)],
    )
    .into()
}

pub(crate) fn counted_toggle(on: bool) -> Item {
    toggle(text::PORTFOLIO, text::COUNT_IN_TOTAL, Toggle::Counted, on)
}

pub(crate) fn choice(label: impl Into<String>, action: Action, selected: bool) -> Choice {
    toned_choice(label, action, selected, Tone::Plain)
}

pub(crate) fn toned_choice(
    label: impl Into<String>,
    action: Action,
    selected: bool,
    tone: Tone,
) -> Choice {
    Choice {
        label: label.into(),
        action,
        selected,
        tone,
    }
}

pub(crate) fn group(label: &str, choices: Vec<Choice>) -> Group {
    Group {
        label: label.to_owned(),
        choices,
    }
}

pub(crate) fn line(label: &str, value: impl Into<String>) -> Line {
    toned_line(label, value, Tone::Plain)
}

pub(crate) fn toned_line(label: &str, value: impl Into<String>, tone: Tone) -> Line {
    Line {
        label: label.to_owned(),
        value: value.into(),
        tone,
    }
}

pub(crate) fn network_line(ctx: &Ctx<'_>, network: &NetworkId) -> Line {
    toned_line(
        text::NETWORK,
        ctx.network_name(network),
        ctx.network_tone(network),
    )
}

pub(crate) fn swap_details(ctx: &Ctx<'_>, swap: &SwapRecord) -> Vec<Line> {
    let bought = ctx.symbol(&swap.buy);
    Provider::from_key(&swap.provider)
        .map(|provider| line(text::PROVIDER, provider.name()))
        .into_iter()
        .chain([line(
            text::YOU_RECEIVE,
            text::at_least(
                &format::exact(swap.expected, &bought),
                &format::exact(swap.minimum, &bought),
            ),
        )])
        .collect()
}

pub(crate) fn replaceable(ctx: &Ctx<'_>, network: &NetworkId) -> bool {
    ctx.network(network)
        .is_some_and(|network| matches!(network.family(), ChainFamily::Evm | ChainFamily::Bitcoin))
}

pub(crate) fn native_worth(ctx: &Ctx<'_>, network: &NetworkId, amount: Amount) -> Option<Amount> {
    ctx.price(&AssetId::Native(network.clone()))
        .and_then(|price| price.value_of(amount, ctx.fiat().places()).ok())
}

pub(crate) fn kept_aside(from: &Address) -> Option<Note> {
    matches!(
        from,
        Address::Bitcoin {
            script: BitcoinScript::Taproot(_),
            ..
        }
    )
    .then(|| note(Level::Info, text::KEPT_ASIDE))
}

pub(crate) fn silence(ctx: &Ctx<'_>, swap: &SwapRecord) -> Option<Note> {
    (swap.status == SwapStatus::Unanswered).then(|| {
        note(
            Level::Warning,
            text::unanswered_note(
                text::provider_of(swap),
                &ctx.symbol(&swap.buy),
                &ctx.network_name(swap.buy.network()),
            ),
        )
    })
}

pub(crate) fn swap_tone(status: &SwapStatus) -> Tone {
    match status {
        SwapStatus::Waiting | SwapStatus::Received | SwapStatus::Swapping => Tone::Waiting,
        SwapStatus::Done => Tone::Good,
        SwapStatus::Refunded | SwapStatus::Unanswered => Tone::Warning,
        SwapStatus::Failed(_) => Tone::Bad,
    }
}

pub(crate) fn status_tone(status: &SentStatus) -> Tone {
    match status {
        SentStatus::Sending | SentStatus::Pending => Tone::Waiting,
        SentStatus::Confirmed => Tone::Good,
        SentStatus::Failed(_) => Tone::Bad,
        SentStatus::Dropped => Tone::Warning,
    }
}

pub(crate) fn cell(text: impl Into<String>, tone: Tone) -> Cell {
    Cell {
        text: text.into(),
        tone,
    }
}

pub(crate) fn network_cell(ctx: &Ctx<'_>, network: &NetworkId) -> Cell {
    cell(ctx.network_name(network), ctx.network_tone(network))
}

pub(crate) fn asset_cell(ctx: &Ctx<'_>, asset: &AssetId) -> Cell {
    cell(ctx.symbol(asset), ctx.coin_tone(asset))
}

pub(crate) fn wallet_group(ctx: &Ctx<'_>, filter: Option<WalletId>) -> Option<Group> {
    let wallets = &ctx.data.wallets;
    if wallets.len() < 2 && wallets.iter().all(|wallet| wallet.counted) {
        return None;
    }
    let all = choice(
        text::ALL_WALLETS,
        Action::Pick(Pick::Wallet(None)),
        filter.is_none(),
    );
    let wallets = ctx
        .data
        .wallets
        .iter()
        .enumerate()
        .map(|(position, wallet)| {
            choice(
                ctx.wallet_name(wallet.id),
                Action::Pick(Pick::Wallet(Some(position))),
                filter == Some(wallet.id),
            )
        });
    Some(group(
        text::WALLETS,
        std::iter::once(all).chain(wallets).collect(),
    ))
}

pub(crate) fn update_check(settings: &Settings, asked: bool) -> Option<Request> {
    match settings.updates {
        Updates::Off => None,
        Updates::Stable => Some(Request::CheckUpdates { beta: false, asked }),
        Updates::Beta => Some(Request::CheckUpdates { beta: true, asked }),
    }
}

pub(crate) fn spendable<'a>(ctx: &'a Ctx<'_>) -> Vec<&'a Holding> {
    let mut held: Vec<&Holding> = ctx
        .data
        .portfolio
        .holdings
        .iter()
        .filter(|holding| !holding.amount.is_zero() && !ctx.watch_only(holding.wallet))
        .collect();
    held.sort_by(|a, b| {
        b.value
            .partial_cmp(&a.value)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.symbol.cmp(&b.symbol))
    });
    held
}

pub(crate) fn holding<'a>(ctx: &'a Ctx<'_>, source: &Source) -> Option<&'a Holding> {
    ctx.data.portfolio.holdings.iter().find(|holding| {
        holding.wallet == source.wallet
            && holding.address == source.address
            && holding.asset == source.asset
    })
}

pub(crate) fn search_box(typed: &str, about: &str, shown: usize, total: usize) -> Item {
    let hint = if typed.trim().is_empty() {
        about.to_owned()
    } else {
        text::found(shown, total)
    };
    Input {
        hint: Some(note(Level::Info, hint)),
        ..input(Field::Search, text::SEARCH, typed)
    }
    .into()
}

pub(crate) fn searched<T>(
    ctx: &Ctx<'_>,
    typed: &str,
    items: impl IntoIterator<Item = T>,
    facts: impl Fn(&T) -> Facts,
) -> Vec<(usize, T)> {
    let query = Query::new(ctx, typed);
    items
        .into_iter()
        .enumerate()
        .filter(|(_, item)| query.is_empty() || query.matches(&facts(item)))
        .collect()
}

pub(crate) fn search_empty(typed: &str, empty: &str) -> String {
    if typed.trim().is_empty() {
        empty.to_owned()
    } else {
        text::nothing_matches(typed.trim())
    }
}

pub(crate) fn holdings_page(
    ctx: &Ctx<'_>,
    id: PageId,
    (title, intro, empty): (&str, &str, &str),
    typed: &str,
) -> Page {
    let held = spendable(ctx);
    if held.is_empty() {
        return Page {
            id,
            title: title.to_owned(),
            text: Some(empty.to_owned()),
            buttons: vec![button(text::RECEIVE, Action::Go(Go::Tab(Tab::Receive)))],
            ..Page::default()
        };
    }
    let owners: Vec<String> = held
        .iter()
        .map(|holding| ctx.owner_label(holding.wallet, &holding.address))
        .collect();
    let several = several(&owners);
    let mut columns: Vec<String> = text::SEND_COLUMNS.map(str::to_owned).to_vec();
    if several {
        columns.push(text::WALLET.to_owned());
    }
    let total = held.len();
    let kept = searched(ctx, typed, held.iter().zip(owners), |(holding, owner)| {
        let mut facts = Facts::default();
        facts.asset(ctx, &holding.asset).word(owner);
        facts
    });
    let shown = kept.len();
    let rows = kept
        .into_iter()
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
        id,
        title: title.to_owned(),
        text: Some(intro.to_owned()),
        form: vec![search_box(typed, text::SEARCH_HOLDINGS, shown, total)],
        table: Some(Table {
            columns,
            rows,
            empty: search_empty(typed, empty),
        }),
        ..Page::default()
    }
}

pub(crate) fn amount_hint(ctx: &Ctx<'_>, holding: &Holding, typed: &str) -> Option<Note> {
    if typed.trim().is_empty() {
        return None;
    }
    let decimals = ctx.coin(&holding.asset)?.decimals;
    let amount = match parse::amount(typed, decimals) {
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

pub(crate) fn several(owners: &[String]) -> bool {
    owners.iter().any(|owner| owner != &owners[0])
}

pub(crate) fn picked_wallet(ctx: &Ctx<'_>, position: Option<usize>) -> Option<WalletId> {
    position
        .and_then(|position| ctx.data.wallets.get(position))
        .map(|wallet| wallet.id)
}

pub(crate) fn note(level: Level, text: impl Into<String>) -> Note {
    Note {
        level,
        text: text.into(),
    }
}

pub(crate) fn input(field: Field, label: &str, value: &str) -> Input {
    Input {
        field,
        label: label.to_owned(),
        value: Value::Text(value.to_owned()),
        hint: None,
    }
}

pub(crate) fn hidden(field: Field, label: &str, value: &Secret) -> Input {
    Input {
        field,
        label: label.to_owned(),
        value: Value::Hidden(value.clone()),
        hint: None,
    }
}

pub(crate) fn secret(text: &Secret) -> SecretString {
    SecretString::from(text.expose())
}

pub(crate) fn password_problem(password: &Secret, confirm: &Secret) -> Option<&'static str> {
    if password.expose().chars().count() < MIN_PASSWORD {
        Some(text::PASSWORD_SHORT)
    } else if password != confirm {
        Some(text::PASSWORD_MISMATCH)
    } else {
        None
    }
}

pub(crate) fn password_inputs(password: &Secret, confirm: &Secret) -> Vec<Item> {
    let length = password.expose().chars().count();
    let hint =
        (length > 0 && length < GOOD_PASSWORD).then(|| note(Level::Warning, text::PASSWORD_WEAK));
    vec![
        Input {
            hint,
            ..hidden(Field::NewPassword, text::NEW_PASSWORD, password)
        }
        .into(),
        hidden(Field::Confirm, text::CONFIRM_PASSWORD, confirm).into(),
    ]
}

pub(crate) fn wallet_kind(wallet: &WalletInfo) -> String {
    let words = |count| match count {
        WordCount::Twelve => 12,
        WordCount::Fifteen => 15,
        WordCount::Eighteen => 18,
        WordCount::TwentyOne => 21,
        WordCount::TwentyFour => 24,
    };
    match wallet.kind {
        WalletKind::Bip39 => text::multi_chain(wallet.words.map_or(0, words), wallet.passphrase),
        WalletKind::Ton => text::ton_wallet(wallet.ton.unwrap_or(TonContract::W5)),
        WalletKind::Key(family) => text::key_wallet(family),
        WalletKind::Watch(family) => text::watch_wallet(family),
    }
}

pub(crate) fn revealed(phrase: &SecretString) -> Secret {
    use bahaa_engine::ExposeSecret;
    Secret::new(phrase.expose_secret())
}

pub(crate) fn memo_allowed(asset: &AssetId, family: ChainFamily) -> bool {
    match family {
        ChainFamily::Ton | ChainFamily::Solana => true,
        ChainFamily::Evm | ChainFamily::Tron => matches!(asset, AssetId::Native(_)),
        ChainFamily::Bitcoin => false,
    }
}

pub(crate) fn network_group(ctx: &Ctx<'_>, selected: Option<&NetworkId>) -> Group {
    let networks = ctx
        .networks()
        .iter()
        .enumerate()
        .map(|(position, network)| {
            toned_choice(
                &network.name,
                Action::Pick(Pick::Network(position)),
                selected == Some(&network.id),
                Tone::Brand(network.color),
            )
        })
        .collect();
    group(text::NETWORK, networks)
}

pub(crate) fn picked_network(ctx: &Ctx<'_>, position: usize) -> Option<NetworkId> {
    ctx.networks()
        .get(position)
        .map(|network| network.id.clone())
}

#[cfg(test)]
mod tests {
    use bahaa_engine::{Address, ChainFamily};

    use super::kept_aside;

    #[test]
    fn only_taproot_senders_are_told_that_small_coins_stay() {
        let address = |text: &str| Address::parse(ChainFamily::Bitcoin, text).unwrap();
        assert!(
            kept_aside(&address(
                "bc1p5cyxnuxmeuwuvkwfem96lqzszd02n6xdcjrs20cac6yqjjwudpxqkedrcr"
            ))
            .is_some()
        );
        assert!(kept_aside(&address("bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu")).is_none());
    }
}
