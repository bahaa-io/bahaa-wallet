use bahaa_engine::{BitcoinType, SolanaPath, TonContract, WalletId, WalletInfo, WalletKind};

use super::{
    Screen, button, cell, choice, counted_toggle, group, hidden, input, line, wallet_kind,
};
use crate::action::{Action, AddressType, Contract, Field, Go, Path, Pick, Tab, Toggle};
use crate::call::{Call, Outcome};
use crate::core::{Ctx, Nav, Reply};
use crate::view::{Item, Level, Page, PageId, Row, Secret, Table, Tone};
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
    fn act(&mut self, action: &Action, ctx: &Ctx<'_>) -> Reply {
        match action {
            Action::Toggle(Toggle::Counted) => {
                return Reply::call(Call::SetCounted(self.id, !ctx.counted(self.id)));
            }
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
            Action::Pick(Pick::Path(path)) => {
                let path = match path {
                    Path::Bip44Change => SolanaPath::Bip44Change,
                    Path::Bip44 => SolanaPath::Bip44,
                };
                return Reply::call(Call::SetSolanaPath(self.id, path));
            }
            Action::Pick(Pick::AddressType(kind)) => {
                let kind = match kind {
                    AddressType::NativeSegwit => BitcoinType::NativeSegwit,
                    AddressType::Taproot => BitcoinType::Taproot,
                };
                return Reply::call(Call::SetBitcoinType(self.id, kind));
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
                    text::account(account.index).into(),
                    cell(account.family.as_str(), Tone::Muted),
                    cell(
                        format::short(&account.network.as_ref().map_or_else(
                            || account.address.to_string(),
                            |network| ctx.shown(&account.address, network),
                        )),
                        Tone::Muted,
                    ),
                ],
                open: None,
                warn: false,
            })
            .collect();
        let mut buttons = vec![button(text::SAVE_NAME, Action::Submit)];
        let mut form = vec![input(Field::Name, text::NAME, &self.name).into()];
        form.extend(choices(wallet));
        if wallet.kind == WalletKind::Bip39 {
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
        if matches!(wallet.kind, WalletKind::Watch(_)) {
            form.push(counted_toggle(wallet.counted));
        }
        if let Some(label) = reveal_label(wallet.kind) {
            buttons.push(button(label, Action::Go(Go::Reveal)));
        }
        buttons.push(button(text::REMOVE_WALLET, Action::Go(Go::RemoveWallet)));
        Page {
            id: PageId::Wallet,
            title: ctx.wallet_name(self.id),
            lines: vec![line(text::KIND, wallet_kind(wallet))],
            form,
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
        let kind = ctx
            .data
            .wallets
            .iter()
            .find(|wallet| wallet.id == self.id)
            .map_or(WalletKind::Bip39, |wallet| wallet.kind);
        let name = ctx.wallet_name(self.id);
        let title = match kind {
            WalletKind::Key(_) => text::key_title(&name),
            WalletKind::Bip39 | WalletKind::Ton | WalletKind::Watch(_) => text::reveal_title(&name),
        };
        let mut page = Page {
            id: PageId::Reveal,
            title,
            text: Some(text::REVEAL_TEXT.to_owned()),
            error: self.error.clone(),
            ..Page::default()
        };
        if let Some(words) = &self.words {
            page.secret = Some(words.clone());
            page.buttons = vec![button(text::DONE, Action::Submit)];
        } else {
            page.form = vec![hidden(Field::Password, text::PASSWORD, &self.password).into()];
            page.buttons = reveal_label(kind)
                .map(|label| button(label, Action::Submit))
                .into_iter()
                .collect();
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
            text: Some(
                if ctx.watch_only(self.id) {
                    text::REMOVE_WATCH_TEXT
                } else {
                    text::REMOVE_TEXT
                }
                .to_owned(),
            ),
            form: vec![hidden(Field::Password, text::PASSWORD, &self.password).into()],
            error: self.error.clone(),
            buttons: vec![button(text::REMOVE_WALLET, Action::Submit)],
            ..Page::default()
        }
    }
}

fn choices(wallet: &WalletInfo) -> Vec<Item> {
    let pair = |label: &str, options: [(&str, Pick, bool); 2]| -> Item {
        let choices = options
            .into_iter()
            .map(|(text, pick, on)| choice(text, Action::Pick(pick), on))
            .collect();
        group(label, choices).into()
    };
    let mut items = Vec::new();
    if let Some(contract) = wallet.ton {
        items.push(pair(
            text::TON_CONTRACT,
            [
                (
                    text::W5,
                    Pick::Contract(Contract::W5),
                    contract == TonContract::W5,
                ),
                (
                    text::V4R2,
                    Pick::Contract(Contract::V4R2),
                    contract == TonContract::V4R2,
                ),
            ],
        ));
    }
    if let Some(path) = wallet.solana {
        items.push(pair(
            text::SOLANA_PATH,
            [
                (
                    text::PATH_BIP44_CHANGE,
                    Pick::Path(Path::Bip44Change),
                    path == SolanaPath::Bip44Change,
                ),
                (
                    text::PATH_BIP44,
                    Pick::Path(Path::Bip44),
                    path == SolanaPath::Bip44,
                ),
            ],
        ));
    }
    if let Some(kind) = wallet.bitcoin {
        items.push(pair(
            text::BITCOIN_TYPE,
            [
                (
                    text::NATIVE_SEGWIT,
                    Pick::AddressType(AddressType::NativeSegwit),
                    kind == BitcoinType::NativeSegwit,
                ),
                (
                    text::TAPROOT,
                    Pick::AddressType(AddressType::Taproot),
                    kind == BitcoinType::Taproot,
                ),
            ],
        ));
    }
    items
}

fn reveal_label(kind: WalletKind) -> Option<&'static str> {
    match kind {
        WalletKind::Key(_) => Some(text::SHOW_KEY),
        WalletKind::Bip39 | WalletKind::Ton => Some(text::SHOW_PHRASE),
        WalletKind::Watch(_) => None,
    }
}
