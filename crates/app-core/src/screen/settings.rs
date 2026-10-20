use bahaa_engine::{Fiat, Updates};

use super::onboarding::Welcome;
use super::tokens;
use super::wallets::Wallet;
use super::{
    Screen, button, cell, choice, group, hidden, line, password_inputs, password_problem,
    wallet_kind,
};
use crate::action::{Action, Channel, Currency, Field, Go, Pick, Toggle};
use crate::call::{Call, Outcome};
use crate::core::{Ctx, Nav, Reply};
use crate::text;
use crate::view::{Item, Level, Page, PageId, Request, Row, Secret, Table, Tone};

const AUTO_LOCK: [u16; 5] = [1, 5, 10, 30, 60];

pub(crate) struct Settings;

impl Screen for Settings {
    fn act(&mut self, action: &Action, ctx: &Ctx<'_>) -> Reply {
        let current = &ctx.data.settings;
        match action {
            Action::Pick(Pick::Currency(currency)) => save(bahaa_engine::Settings {
                fiat: fiat(*currency),
                ..current.clone()
            }),
            Action::Pick(Pick::AutoLock(minutes)) => save(bahaa_engine::Settings {
                lock_after_minutes: *minutes,
                ..current.clone()
            }),
            Action::Toggle(Toggle::HideBalances) => save(bahaa_engine::Settings {
                hide_balances: !current.hide_balances,
                ..current.clone()
            }),
            Action::Pick(Pick::Channel(channel)) => {
                let settings = bahaa_engine::Settings {
                    updates: updates(*channel),
                    ..current.clone()
                };
                Reply {
                    request: super::update_check(&settings, true),
                    ..save(settings)
                }
            }
            Action::Pick(Pick::CheckUpdates) => Reply::request(Request::CheckUpdates {
                beta: current.updates == Updates::Beta,
                asked: true,
            }),
            Action::Go(Go::Wallet(row)) => ctx
                .data
                .wallets
                .get(*row)
                .map_or_else(Reply::default, |wallet| {
                    Reply::nav(Nav::Push(Box::new(Wallet::new(wallet.id, ctx))))
                }),
            Action::Go(Go::AddWallet) => Reply::nav(Nav::Push(Box::new(Welcome::new(true)))),
            Action::Go(Go::Tokens) => Reply::nav(Nav::Push(Box::new(tokens::List))),
            Action::Go(Go::Developer) => Reply::nav(Nav::Push(Box::new(Developer))),
            Action::Go(Go::ChangePassword) => {
                Reply::nav(Nav::Push(Box::new(ChangePassword::default())))
            }
            _ => Reply::default(),
        }
    }

    fn done(&mut self, outcome: Outcome, _ctx: &Ctx<'_>) -> Reply {
        failed(outcome)
    }

    fn page(&self, ctx: &Ctx<'_>) -> Page {
        let settings = &ctx.data.settings;
        let currencies = [Currency::Usd, Currency::Eur, Currency::Irt]
            .into_iter()
            .map(|currency| {
                choice(
                    fiat(currency).code(),
                    Action::Pick(Pick::Currency(currency)),
                    settings.fiat == fiat(currency),
                )
            })
            .collect();
        let locks = AUTO_LOCK
            .into_iter()
            .map(|minutes| {
                choice(
                    text::minutes(minutes),
                    Action::Pick(Pick::AutoLock(minutes)),
                    settings.lock_after_minutes == minutes,
                )
            })
            .collect();
        let rows = ctx
            .data
            .wallets
            .iter()
            .enumerate()
            .map(|(position, wallet)| Row {
                cells: vec![
                    ctx.wallet_name(wallet.id).into(),
                    cell(wallet_kind(wallet), Tone::Muted),
                ],
                open: Some(Action::Go(Go::Wallet(position))),
                warn: false,
            })
            .collect();
        let mut form: Vec<Item> = vec![
            group(text::CURRENCY, currencies).into(),
            group(text::AUTO_LOCK, locks).into(),
            toggle(
                text::PRIVACY,
                text::HIDE_BALANCES,
                Toggle::HideBalances,
                settings.hide_balances,
            ),
        ];
        let mut buttons = vec![
            button(text::ADD_WALLET, Action::Go(Go::AddWallet)),
            button(text::TOKENS, Action::Go(Go::Tokens)),
            button(text::NETWORK_STATUS, Action::Go(Go::Networks)),
        ];
        if ctx.host.updates {
            let channels = [Channel::Off, Channel::Stable, Channel::Beta]
                .into_iter()
                .map(|channel| {
                    choice(
                        text::channel(channel),
                        Action::Pick(Pick::Channel(channel)),
                        settings.updates == updates(channel),
                    )
                })
                .collect();
            form.push(group(text::UPDATES, channels).into());
            buttons.push(button(
                text::CHECK_UPDATES,
                Action::Pick(Pick::CheckUpdates),
            ));
        }
        buttons.extend([
            button(text::DEVELOPER, Action::Go(Go::Developer)),
            button(text::CHANGE_PASSWORD, Action::Go(Go::ChangePassword)),
            button(text::LOCK_NOW, Action::Lock),
        ]);
        Page {
            id: PageId::Settings,
            title: text::SETTINGS.to_owned(),
            form,
            table: Some(Table {
                columns: text::WALLET_COLUMNS.map(str::to_owned).to_vec(),
                rows,
                empty: text::NO_WALLETS.to_owned(),
            }),
            buttons,
            ..Page::default()
        }
    }
}

struct Developer;

impl Screen for Developer {
    fn act(&mut self, action: &Action, ctx: &Ctx<'_>) -> Reply {
        let current = &ctx.data.settings;
        match action {
            Action::Toggle(Toggle::Developer) => save(bahaa_engine::Settings {
                developer_mode: !current.developer_mode,
                ..current.clone()
            }),
            Action::Toggle(Toggle::Testnets) => save(bahaa_engine::Settings {
                show_testnets: !current.show_testnets,
                ..current.clone()
            }),
            _ => Reply::default(),
        }
    }

    fn done(&mut self, outcome: Outcome, _ctx: &Ctx<'_>) -> Reply {
        failed(outcome)
    }

    fn page(&self, ctx: &Ctx<'_>) -> Page {
        let settings = &ctx.data.settings;
        Page {
            id: PageId::Developer,
            title: text::DEVELOPER.to_owned(),
            text: Some(text::DEVELOPER_TEXT.to_owned()),
            lines: vec![
                line(text::VERSION, env!("CARGO_PKG_VERSION")),
                line(text::DATA_FOLDER, ctx.host.folder.clone()),
            ],
            form: vec![
                toggle(
                    text::DEVELOPER_MODE,
                    text::FEE_DETAILS,
                    Toggle::Developer,
                    settings.developer_mode,
                ),
                toggle(
                    text::NETWORKS,
                    text::SHOW_TESTNETS,
                    Toggle::Testnets,
                    settings.show_testnets,
                ),
            ],
            buttons: vec![button(text::NETWORK_STATUS, Action::Go(Go::Networks))],
            ..Page::default()
        }
    }
}

fn save(settings: bahaa_engine::Settings) -> Reply {
    Reply::call(Call::SetSettings(settings))
}

fn failed(outcome: Outcome) -> Reply {
    match outcome {
        Outcome::Done(Err(err)) => Reply::note(Level::Error, text::error(&err)),
        _ => Reply::default(),
    }
}

fn toggle(label: &str, choice_label: &str, toggle: Toggle, on: bool) -> Item {
    group(
        label,
        vec![choice(choice_label, Action::Toggle(toggle), on)],
    )
    .into()
}

fn fiat(currency: Currency) -> Fiat {
    match currency {
        Currency::Usd => Fiat::Usd,
        Currency::Eur => Fiat::Eur,
        Currency::Irt => Fiat::Irt,
    }
}

fn updates(channel: Channel) -> Updates {
    match channel {
        Channel::Off => Updates::Off,
        Channel::Stable => Updates::Stable,
        Channel::Beta => Updates::Beta,
    }
}

#[derive(Default)]
struct ChangePassword {
    old: Secret,
    new: Secret,
    confirm: Secret,
    error: Option<String>,
}

impl Screen for ChangePassword {
    fn act(&mut self, action: &Action, _ctx: &Ctx<'_>) -> Reply {
        match action {
            Action::Type(Field::Password, value) => self.old = Secret::new(value),
            Action::Type(Field::NewPassword, value) => self.new = Secret::new(value),
            Action::Type(Field::Confirm, value) => self.confirm = Secret::new(value),
            Action::Submit => match password_problem(&self.new, &self.confirm) {
                Some(problem) => self.error = Some(problem.to_owned()),
                None => {
                    return Reply::call(Call::ChangePassword(
                        super::secret(&self.old),
                        super::secret(&self.new),
                    ));
                }
            },
            _ => {}
        }
        Reply::default()
    }

    fn done(&mut self, outcome: Outcome, _ctx: &Ctx<'_>) -> Reply {
        match outcome {
            Outcome::Done(Ok(())) => {
                Reply::nav(Nav::Back).with_note(Level::Success, text::PASSWORD_CHANGED)
            }
            Outcome::Done(Err(err)) => {
                self.error = Some(text::error(&err));
                Reply::default()
            }
            _ => Reply::default(),
        }
    }

    fn page(&self, _ctx: &Ctx<'_>) -> Page {
        let mut form = vec![hidden(Field::Password, text::CURRENT_PASSWORD, &self.old).into()];
        form.extend(password_inputs(&self.new, &self.confirm));
        Page {
            id: PageId::ChangePassword,
            title: text::CHANGE_PASSWORD.to_owned(),
            form,
            error: self.error.clone(),
            buttons: vec![button(text::SAVE, Action::Submit)],
            ..Page::default()
        }
    }
}
