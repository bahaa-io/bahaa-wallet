mod activity;
mod contacts;
mod networks;
mod onboarding;
mod portfolio;
mod receive;
mod send;
mod settings;
mod tokens;
mod unlock;
mod wallets;

pub use onboarding::completions;

use bahaa_engine::{
    AssetId, ChainFamily, NetworkId, SecretString, SentStatus, Settings, TonContract, Updates,
    WalletId, WalletInfo, WalletKind, WordCount,
};

use crate::action::{Action, Field, Pick, Tab};
use crate::call::Outcome;
use crate::core::{Ctx, Reply};
use crate::text;
use crate::view::{
    Button, Cell, Choice, Group, Input, Item, Level, Line, Note, Page, PageId, Request, Secret,
    Tone, Value,
};

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
    }
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
    if ctx.data.wallets.len() < 2 {
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
    }
}

pub(crate) fn revealed(phrase: &SecretString) -> Secret {
    use bahaa_engine::ExposeSecret;
    Secret::new(phrase.expose_secret())
}

pub(crate) fn memo_allowed(asset: &AssetId, family: ChainFamily) -> bool {
    asset.contract().is_none() || family == ChainFamily::Ton
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
