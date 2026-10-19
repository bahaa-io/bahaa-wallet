use bahaa_engine::{TonContract, WalletId, WalletKind};

use super::{Screen, button, choice, group, hidden, input, line, wallet_kind};
use crate::action::{Action, Contract, Field, Go, Pick, Tab};
use crate::call::{Call, Outcome};
use crate::core::{Ctx, Nav, Reply};
use crate::view::{Level, Page, PageId, Row, Secret, Table};
use crate::{format, text};

pub(crate) struct Wallet {
    id: WalletId,
    name: String,
    error: Option<String>,
}

impl Wallet {
    pub(crate) fn new(id: WalletId, ctx: &Ctx<'_>) -> Self {
        Self {
            id,
            name: ctx.wallet_name(id),
            error: None,
        }
    }
}

impl Screen for Wallet {
    fn act(&mut self, action: &Action, _ctx: &Ctx<'_>) -> Reply {
        match action {
            Action::Type(Field::Name, value) => self.name.clone_from(value),
            Action::Submit => {
                return Reply::call(Call::Rename(self.id, self.name.trim().to_owned()));
            }
            Action::Pick(Pick::AddAccount) => return Reply::call(Call::AddAccount(self.id)),
            Action::Pick(Pick::RemoveAccount(index)) => {
                return Reply::call(Call::RemoveAccount(self.id, *index));
            }
            Action::Pick(Pick::Contract(contract)) => {
                let contract = match contract {
                    Contract::W5 => TonContract::W5,
                    Contract::V4R2 => TonContract::V4R2,
                };
                return Reply::call(Call::SetTonContract(self.id, contract));
            }
            Action::Go(Go::Reveal) => {
                return Reply::nav(Nav::Push(Box::new(Reveal::new(self.id))));
            }
            Action::Go(Go::RemoveWallet) => {
                return Reply::nav(Nav::Push(Box::new(Remove::new(self.id))));
            }
            _ => {}
        }
        Reply::default()
    }

    fn done(&mut self, outcome: Outcome, _ctx: &Ctx<'_>) -> Reply {
        match outcome {
            Outcome::Done(Ok(())) => {
                self.error = None;
                Reply::note(Level::Success, text::SAVED)
            }
            Outcome::Done(Err(err)) => {
                self.error = Some(text::error(&err));
                Reply::default()
            }
            _ => Reply::default(),
        }
    }

    fn page(&self, ctx: &Ctx<'_>) -> Page {
        let Some(wallet) = ctx.data.wallets.iter().find(|wallet| wallet.id == self.id) else {
            return Page {
                id: PageId::Wallet,
                title: text::WALLET.to_owned(),
                text: Some(text::LOADING.to_owned()),
                ..Page::default()
            };
        };
        let accounts: Vec<_> = ctx
            .data
            .accounts
            .iter()
            .filter(|account| {
                account.wallet == self.id
                    && account.network.as_ref().is_none_or(|network| {
                        ctx.networks().iter().any(|shown| shown.id == *network)
                    })
            })
            .collect();
        let mut indexes: Vec<u32> = accounts.iter().map(|account| account.index).collect();
        indexes.sort_unstable();
        indexes.dedup();
        let rows = accounts
            .iter()
            .map(|account| Row {
                cells: vec![
                    text::account(account.index),
                    account.family.as_str().to_owned(),
                    format::short(&account.network.as_ref().map_or_else(
                        || account.address.to_string(),
                        |network| ctx.shown(&account.address, network),
                    )),
                ],
                open: None,
                warn: false,
            })
            .collect();
        let mut buttons = vec![button(text::SAVE_NAME, Action::Submit)];
        let mut groups = Vec::new();
        match wallet.kind {
            WalletKind::Bip39 => {
                buttons.push(button(text::ADD_ACCOUNT, Action::Pick(Pick::AddAccount)));
                if indexes.len() > 1 {
                    buttons.extend(indexes.iter().map(|index| {
                        button(
                            text::remove_account(*index),
                            Action::Pick(Pick::RemoveAccount(*index)),
                        )
                    }));
                }
            }
            WalletKind::Ton(contract) => groups.push(group(
                text::TON_CONTRACT,
                vec![
                    choice(
                        text::W5,
                        Action::Pick(Pick::Contract(Contract::W5)),
                        contract == TonContract::W5,
                    ),
                    choice(
                        text::V4R2,
                        Action::Pick(Pick::Contract(Contract::V4R2)),
                        contract == TonContract::V4R2,
                    ),
                ],
            )),
        }
        buttons.push(button(text::SHOW_PHRASE, Action::Go(Go::Reveal)));
        buttons.push(button(text::REMOVE_WALLET, Action::Go(Go::RemoveWallet)));
        Page {
            id: PageId::Wallet,
            title: ctx.wallet_name(self.id),
            lines: vec![line(text::KIND, wallet_kind(wallet))],
            inputs: vec![input(Field::Name, text::NAME, &self.name)],
            groups,
            table: Some(Table {
                columns: text::ACCOUNT_COLUMNS.map(str::to_owned).to_vec(),
                rows,
                empty: text::LOADING.to_owned(),
            }),
            error: self.error.clone(),
            buttons,
            ..Page::default()
        }
    }
}

struct Reveal {
    id: WalletId,
    password: Secret,
    words: Option<Secret>,
    error: Option<String>,
}

impl Reveal {
    fn new(id: WalletId) -> Self {
        Self {
            id,
            password: Secret::default(),
            words: None,
            error: None,
        }
    }
}

impl Screen for Reveal {
    fn act(&mut self, action: &Action, _ctx: &Ctx<'_>) -> Reply {
        match action {
            Action::Type(Field::Password, value) => self.password = Secret::new(value),
            Action::Submit if self.words.is_none() => {
                return Reply::call(Call::Reveal(self.id, super::secret(&self.password)));
            }
            Action::Submit => return Reply::nav(Nav::Back),
            _ => {}
        }
        Reply::default()
    }

    fn done(&mut self, outcome: Outcome, _ctx: &Ctx<'_>) -> Reply {
        self.password = Secret::default();
        match outcome {
            Outcome::Revealed(Ok(phrase)) => {
                self.words = Some(super::revealed(&phrase));
                self.error = None;
            }
            Outcome::Revealed(Err(err)) => self.error = Some(text::error(&err)),
            _ => {}
        }
        Reply::default()
    }

    fn page(&self, ctx: &Ctx<'_>) -> Page {
        let mut page = Page {
            id: PageId::Reveal,
            title: text::reveal_title(&ctx.wallet_name(self.id)),
            text: Some(text::REVEAL_TEXT.to_owned()),
            error: self.error.clone(),
            ..Page::default()
        };
        if let Some(words) = &self.words {
            page.secret = Some(words.clone());
            page.buttons = vec![button(text::DONE, Action::Submit)];
        } else {
            page.inputs = vec![hidden(Field::Password, text::PASSWORD, &self.password)];
            page.buttons = vec![button(text::SHOW_PHRASE, Action::Submit)];
        }
        page
    }
}

struct Remove {
    id: WalletId,
    password: Secret,
    checked: bool,
    error: Option<String>,
}

impl Remove {
    fn new(id: WalletId) -> Self {
        Self {
            id,
            password: Secret::default(),
            checked: false,
            error: None,
        }
    }
}

impl Screen for Remove {
    fn act(&mut self, action: &Action, _ctx: &Ctx<'_>) -> Reply {
        match action {
            Action::Type(Field::Password, value) => self.password = Secret::new(value),
            Action::Submit => {
                return Reply::call(Call::CheckPassword(super::secret(&self.password)));
            }
            _ => {}
        }
        Reply::default()
    }

    fn done(&mut self, outcome: Outcome, _ctx: &Ctx<'_>) -> Reply {
        match outcome {
            Outcome::Done(Ok(())) if !self.checked => {
                self.checked = true;
                self.password = Secret::default();
                Reply::call(Call::Remove(self.id))
            }
            Outcome::Done(Ok(())) => {
                Reply::nav(Nav::Tab(Tab::Settings)).with_note(Level::Success, text::WALLET_REMOVED)
            }
            Outcome::Done(Err(err)) => {
                self.checked = false;
                self.error = Some(text::error(&err));
                Reply::default()
            }
            _ => Reply::default(),
        }
    }

    fn page(&self, ctx: &Ctx<'_>) -> Page {
        Page {
            id: PageId::RemoveWallet,
            title: text::remove_title(&ctx.wallet_name(self.id)),
            text: Some(text::REMOVE_TEXT.to_owned()),
            inputs: vec![hidden(Field::Password, text::PASSWORD, &self.password)],
            error: self.error.clone(),
            buttons: vec![button(text::REMOVE_WALLET, Action::Submit)],
            ..Page::default()
        }
    }
}
