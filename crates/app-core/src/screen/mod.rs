mod activity;
mod contacts;
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
    AssetId, ChainFamily, NetworkId, SecretString, WalletInfo, WalletKind, WordCount,
};

use crate::action::{Action, Field, Pick, Tab};
use crate::call::Outcome;
use crate::core::{Ctx, Reply};
use crate::text;
use crate::view::{
    Button, Choice, Group, Input, Item, Level, Line, Note, Page, PageId, Secret, Value,
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

pub(crate) fn unlock() -> Box<dyn Screen> {
    Box::new(unlock::Unlock::default())
}

pub(crate) fn tab(tab: Tab) -> Box<dyn Screen> {
    match tab {
        Tab::Portfolio => Box::new(portfolio::Portfolio::default()),
        Tab::Activity => Box::new(activity::List),
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
    Choice {
        label: label.into(),
        action,
        selected,
    }
}

pub(crate) fn group(label: &str, choices: Vec<Choice>) -> Group {
    Group {
        label: label.to_owned(),
        choices,
    }
}

pub(crate) fn line(label: &str, value: impl Into<String>) -> Line {
    Line {
        label: label.to_owned(),
        value: value.into(),
    }
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
    let hint = (length > 0 && length < GOOD_PASSWORD).then(|| text::PASSWORD_WEAK.to_owned());
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
    let words = match wallet.words {
        WordCount::Twelve => 12,
        WordCount::Fifteen => 15,
        WordCount::Eighteen => 18,
        WordCount::TwentyOne => 21,
        WordCount::TwentyFour => 24,
    };
    match wallet.kind {
        WalletKind::Bip39 => text::multi_chain(words, wallet.passphrase),
        WalletKind::Ton(contract) => text::ton_wallet(contract),
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
            choice(
                &network.name,
                Action::Pick(Pick::Network(position)),
                selected == Some(&network.id),
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
