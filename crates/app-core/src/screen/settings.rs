use bahaa_engine::Fiat;
use zeroize::Zeroizing;

use super::onboarding::Welcome;
use super::tokens;
use super::wallets::Wallet;
use super::{
    Screen, button, choice, group, hidden, kept, line, password_inputs, password_problem,
    wallet_kind,
};
use crate::action::{Action, Currency, Field, Go, Pick, Toggle};
use crate::call::{Call, Outcome};
use crate::core::{Ctx, Nav, Reply};
use crate::text;
use crate::view::{Level, Page, PageId, Row, Table};

const AUTO_LOCK: [u16; 5] = [1, 5, 10, 30, 60];

pub(crate) struct Settings;

impl Screen for Settings {
    fn act(&mut self, action: &Action, ctx: &Ctx<'_>) -> Reply {
        let current = &ctx.data.settings;
        let save = |settings| Reply::call(Call::SetSettings(settings));
        match action {
            Action::Pick(Pick::Currency(currency)) => save(bahaa_engine::Settings {
                fiat: fiat(*currency),
                ..current.clone()
            }),
            Action::Pick(Pick::AutoLock(minutes)) => save(bahaa_engine::Settings {
                lock_after_minutes: *minutes,
                ..current.clone()
            }),
            Action::Toggle(Toggle::Testnets) => save(bahaa_engine::Settings {
                show_testnets: !current.show_testnets,
                ..current.clone()
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
            Action::Go(Go::ChangePassword) => {
                Reply::nav(Nav::Push(Box::new(ChangePassword::default())))
            }
            _ => Reply::default(),
        }
    }

    fn done(&mut self, outcome: Outcome, _ctx: &Ctx<'_>) -> Reply {
        match outcome {
            Outcome::Done(Err(err)) => Reply::note(Level::Error, text::error(&err)),
            _ => Reply::default(),
        }
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
                cells: vec![ctx.wallet_name(wallet.id), wallet_kind(wallet)],
                open: Some(Action::Go(Go::Wallet(position))),
                warn: false,
            })
            .collect();
        Page {
            id: PageId::Settings,
            title: text::SETTINGS.to_owned(),
            lines: vec![line(text::VERSION, env!("CARGO_PKG_VERSION"))],
            groups: vec![
                group(text::CURRENCY, currencies),
                group(text::AUTO_LOCK, locks),
                group(
                    text::NETWORKS,
                    vec![choice(
                        text::SHOW_TESTNETS,
                        Action::Toggle(Toggle::Testnets),
                        settings.show_testnets,
                    )],
                ),
            ],
            table: Some(Table {
                columns: text::WALLET_COLUMNS.map(str::to_owned).to_vec(),
                rows,
                empty: text::NO_WALLETS.to_owned(),
            }),
            buttons: vec![
                button(text::ADD_WALLET, Action::Go(Go::AddWallet)),
                button(text::TOKENS, Action::Go(Go::Tokens)),
                button(text::CHANGE_PASSWORD, Action::Go(Go::ChangePassword)),
                button(text::LOCK_NOW, Action::Lock),
            ],
            ..Page::default()
        }
    }
}

fn fiat(currency: Currency) -> Fiat {
    match currency {
        Currency::Usd => Fiat::Usd,
        Currency::Eur => Fiat::Eur,
        Currency::Irt => Fiat::Irt,
    }
}

#[derive(Default)]
struct ChangePassword {
    old: Zeroizing<String>,
    new: Zeroizing<String>,
    confirm: Zeroizing<String>,
    error: Option<String>,
}

impl Screen for ChangePassword {
    fn act(&mut self, action: &Action, _ctx: &Ctx<'_>) -> Reply {
        match action {
            Action::Type(Field::Password, value) => self.old = kept(value),
            Action::Type(Field::NewPassword, value) => self.new = kept(value),
            Action::Type(Field::Confirm, value) => self.confirm = kept(value),
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
        let mut inputs = vec![hidden(Field::Password, text::CURRENT_PASSWORD)];
        inputs.extend(password_inputs(&self.new));
        Page {
            id: PageId::ChangePassword,
            title: text::CHANGE_PASSWORD.to_owned(),
            inputs,
            error: self.error.clone(),
            buttons: vec![button(text::SAVE, Action::Submit)],
            ..Page::default()
        }
    }
}
