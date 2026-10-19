use bahaa_engine::{Caution, EngineError, FeeItem, Replace, SentStatus, TonContract};

use crate::action::Tab;
use crate::core::Ctx;
use crate::format;

pub(crate) const NO_VALUE: &str = "—";
pub(crate) const UNKNOWN: &str = "?";
pub(crate) const JUST_NOW: &str = "just now";
pub(crate) const LOADING: &str = "Loading…";
pub(crate) const STARTING: &str = "Opening your wallet…";

pub(crate) const WELCOME: &str = "Welcome to Bahaa Wallet";
pub(crate) const WELCOME_TEXT: &str =
    "Your keys stay on this device. Create a new wallet or import one you already have.";
pub(crate) const ADD_WALLET: &str = "Add a wallet";
pub(crate) const CREATE_12: &str = "Create a wallet (12 words)";
pub(crate) const CREATE_24: &str = "Create a wallet (24 words)";
pub(crate) const IMPORT: &str = "Import a wallet";
pub(crate) const SET_PASSWORD: &str = "Set a password";
pub(crate) const SET_PASSWORD_TEXT: &str =
    "This password unlocks the wallet on this device. It can't be recovered, so keep it safe.";
pub(crate) const NEW_PASSWORD: &str = "New password";
pub(crate) const CONFIRM_PASSWORD: &str = "Type it again";
pub(crate) const CURRENT_PASSWORD: &str = "Current password";
pub(crate) const PASSWORD: &str = "Password";
pub(crate) const PASSWORD_SHORT: &str = "Use at least 8 characters";
pub(crate) const PASSWORD_MISMATCH: &str = "The two passwords don't match";
pub(crate) const PASSWORD_WEAK: &str = "Short passwords are easier to guess, 12 or more is better";
pub(crate) const PASSWORD_CHANGED: &str = "Password changed";
pub(crate) const ENTER_PASSWORD: &str = "Enter your password";
pub(crate) const YOUR_PHRASE: &str = "Your recovery phrase";
pub(crate) const YOUR_PHRASE_TEXT: &str = "Write these words down, in order, and keep them offline. Anyone with them can take your funds; without them a lost device means lost funds.";
pub(crate) const WROTE_IT_DOWN: &str = "I wrote it down";
pub(crate) const PHRASE_FIRST: &str = "Back up your recovery phrase first";
pub(crate) const CHECK_PHRASE: &str = "Check your phrase";
pub(crate) const CHECK_PHRASE_TEXT: &str = "Type these words from your recovery phrase.";
pub(crate) const CHECK: &str = "Check";
pub(crate) const BACKED_UP: &str = "Your recovery phrase is backed up";
pub(crate) const IMPORT_TITLE: &str = "Import a wallet";
pub(crate) const IMPORT_TEXT: &str =
    "Type your recovery phrase, words separated by spaces. Add the passphrase only if you set one.";
pub(crate) const PHRASE: &str = "Recovery phrase";
pub(crate) const PASSPHRASE: &str = "Passphrase (optional)";
pub(crate) const LOOKS_TON: &str = "A TON wallet phrase";
pub(crate) const AMBIGUOUS: &str =
    "These words work as a multi-chain phrase and as a TON wallet phrase";
pub(crate) const WHICH_APP: &str = "Which kind of wallet made them?";
pub(crate) const KIND_MULTI_CHAIN: &str = "Multi-chain wallet";
pub(crate) const KIND_TON: &str = "TON wallet";
pub(crate) const CHOOSE_KIND: &str = "Choose which kind of wallet made these words";
pub(crate) const IMPORTED: &str = "Wallet imported";
pub(crate) const CONTINUE: &str = "Continue";
pub(crate) const UNLOCK: &str = "Unlock your wallet";
pub(crate) const UNLOCK_BUTTON: &str = "Unlock";

pub(crate) const PORTFOLIO: &str = "Portfolio";
pub(crate) const PORTFOLIO_COLUMNS: [&str; 4] = ["Asset", "Network", "Amount", "Value"];
pub(crate) const HOLDING_COLUMNS: [&str; 5] = ["Wallet", "Account", "Address", "Amount", "Value"];
pub(crate) const WALLETS: &str = "Wallets";
pub(crate) const ALL_WALLETS: &str = "All";
pub(crate) const SHOW: &str = "Show";
pub(crate) const EMPTY_BALANCES: &str = "Empty balances";
pub(crate) const NO_ASSETS: &str = "Nothing here yet. Receive coins to see them.";
pub(crate) const NETWORK: &str = "Network";
pub(crate) const PRICE: &str = "Price";
pub(crate) const ASSET: &str = "Asset";
pub(crate) const PRICES_OFFLINE: &str = "Prices are unavailable right now";

pub(crate) const ACTIVITY: &str = "Activity";
pub(crate) const ACTIVITY_COLUMNS: [&str; 5] = ["When", "Amount", "Network", "With", "Status"];
pub(crate) const NO_ACTIVITY: &str = "No transfers yet";
pub(crate) const RECEIVED: &str = "Received";
pub(crate) const ELSEWHERE: &str = "Sent from another app";
pub(crate) const TRANSFER: &str = "Transfer";
pub(crate) const SENT_TITLE: &str = "Sent";
pub(crate) const RECEIVED_TITLE: &str = "Received";
pub(crate) const ELSEWHERE_TITLE: &str = "Sent from another app";
pub(crate) const STATUS: &str = "Status";
pub(crate) const FROM: &str = "From";
pub(crate) const TO: &str = "To";
pub(crate) const FEE: &str = "Fee";
pub(crate) const MOST_FEE: &str = "Most it can cost";
pub(crate) const MEMO: &str = "Memo";
pub(crate) const TIME: &str = "Time";
pub(crate) const TRANSACTION: &str = "Transaction";
pub(crate) const COPY_ID: &str = "Copy transaction id";
pub(crate) const OPEN_EXPLORER: &str = "Open in explorer";
pub(crate) const SPEED_UP: &str = "Speed up";
pub(crate) const CANCEL_SEND: &str = "Cancel this send";

pub(crate) const SEND: &str = "Send";
pub(crate) const AMOUNT: &str = "Amount";
pub(crate) const SEND_ALL: &str = "Send everything";
pub(crate) const REVIEW: &str = "Review";
pub(crate) const QUOTE: &str = "Quote";
pub(crate) const CONFIRM: &str = "Confirm";
pub(crate) const NEW_QUOTE: &str = "Get a new quote";
pub(crate) const CONFIRM_TITLE: &str = "Confirm with your password";
pub(crate) const SEND_NOW: &str = "Send";
pub(crate) const SENDING: &str = "Sending…";
pub(crate) const VIEW_TRANSFER: &str = "View transfer";
pub(crate) const DONE: &str = "Done";
pub(crate) const QUOTING: &str = "Getting a fee quote…";
pub(crate) const TRY_AGAIN: &str = "Try again";
pub(crate) const QUOTE_EXPIRED: &str = "The fee quote expired, review the new one";
pub(crate) const PICK_SOURCE: &str = "Choose what to send";
pub(crate) const ENTER_RECIPIENT: &str = "Enter who to send to";
pub(crate) const UNKNOWN_ACCOUNT: &str = "This account is not in the wallet anymore";
pub(crate) const UNKNOWN_ASSET: &str = "This asset is not known to the wallet";
pub(crate) const MEMO_NOT_HERE: &str = "This network takes no memo for this asset";
pub(crate) const MEMO_INVALID: &str = "A memo is up to 120 characters of plain text";
pub(crate) const AMOUNT_COMMA: &str = "Use a dot for decimals, without thousands separators";
pub(crate) const AMOUNT_ZERO: &str = "Send more than zero";

pub(crate) const RECEIVE: &str = "Receive";
pub(crate) const RECEIVE_TO: &str = "Receive to";
pub(crate) const PICK_TARGET: &str = "Choose where to receive";
pub(crate) const AMOUNT_OPTIONAL: &str = "Amount (optional)";
pub(crate) const MEMO_OPTIONAL: &str = "Memo (optional)";
pub(crate) const COPY_ADDRESS: &str = "Copy address";
pub(crate) const COPIED: &str = "Copied, the clipboard clears in 30 seconds";

pub(crate) const CONTACTS: &str = "Contacts";
pub(crate) const CONTACT_COLUMNS: [&str; 3] = ["Name", "Network", "Address"];
pub(crate) const NO_CONTACTS: &str = "No contacts yet";
pub(crate) const ADD_CONTACT: &str = "Add contact";
pub(crate) const NEW_CONTACT: &str = "New contact";
pub(crate) const EDIT_CONTACT: &str = "Edit contact";
pub(crate) const NAME: &str = "Name";
pub(crate) const ADDRESS: &str = "Address";
pub(crate) const PICK_NETWORK: &str = "Choose a network";
pub(crate) const SAVE: &str = "Save";
pub(crate) const DELETE: &str = "Delete";
pub(crate) const CONTACT_SAVED: &str = "Contact saved";
pub(crate) const CONTACT_REMOVED: &str = "Contact deleted";

pub(crate) const SETTINGS: &str = "Settings";
pub(crate) const VERSION: &str = "Version";
pub(crate) const CURRENCY: &str = "Currency";
pub(crate) const AUTO_LOCK: &str = "Lock after";
pub(crate) const NETWORKS: &str = "Networks";
pub(crate) const SHOW_TESTNETS: &str = "Show test networks";
pub(crate) const WALLET_COLUMNS: [&str; 2] = ["Wallet", "Type"];
pub(crate) const NO_WALLETS: &str = "No wallets yet";
pub(crate) const TOKENS: &str = "Tokens";
pub(crate) const CHANGE_PASSWORD: &str = "Change password";
pub(crate) const LOCK_NOW: &str = "Lock now";

pub(crate) const WALLET: &str = "Wallet";
pub(crate) const KIND: &str = "Type";
pub(crate) const ACCOUNT_COLUMNS: [&str; 3] = ["Account", "Family", "Address"];
pub(crate) const SAVE_NAME: &str = "Save name";
pub(crate) const SAVED: &str = "Saved";
pub(crate) const ADD_ACCOUNT: &str = "Add account";
pub(crate) const TON_CONTRACT: &str = "Wallet contract";
pub(crate) const W5: &str = "W5 (current)";
pub(crate) const V4R2: &str = "V4R2 (older)";
pub(crate) const SHOW_PHRASE: &str = "Show recovery phrase";
pub(crate) const REMOVE_WALLET: &str = "Remove wallet";
pub(crate) const REVEAL_TEXT: &str =
    "Anyone who sees these words can take your funds. Make sure no one is watching.";
pub(crate) const REMOVE_TEXT: &str = "The wallet leaves this device. You can only bring it back with its recovery phrase, so make sure you have it.";
pub(crate) const WALLET_REMOVED: &str = "Wallet removed";

pub(crate) const ADD_TOKEN: &str = "Add token";
pub(crate) const ADD_TOKEN_TEXT: &str =
    "Paste the token's contract address. The wallet reads its name and decimals from the network.";
pub(crate) const CONTRACT: &str = "Contract address";
pub(crate) const ADD: &str = "Add";
pub(crate) const TOKEN_COLUMNS: [&str; 4] = ["Symbol", "Name", "Network", "Contract"];
pub(crate) const NO_TOKENS: &str = "No added tokens";
pub(crate) const TOKEN_REMOVED: &str = "Token removed";

pub(crate) const LINK_INVALID: &str = "This payment link is broken";
pub(crate) const LINK_UNSUPPORTED: &str = "This kind of payment link is not supported";
pub(crate) const LINK_UNKNOWN_NETWORK: &str =
    "This payment link is for a network the wallet does not have";

pub(crate) fn tab(tab: Tab) -> &'static str {
    match tab {
        Tab::Portfolio => PORTFOLIO,
        Tab::Activity => ACTIVITY,
        Tab::Send => SEND,
        Tab::Receive => RECEIVE,
        Tab::Contacts => CONTACTS,
        Tab::Settings => SETTINGS,
    }
}

pub(crate) fn sentence(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

pub(crate) fn error(err: &EngineError) -> String {
    sentence(&err.to_string())
}

pub(crate) fn minutes_ago(minutes: u64) -> String {
    format!("{minutes} min ago")
}

pub(crate) fn hours_ago(hours: u64) -> String {
    format!("{hours} h ago")
}

pub(crate) fn minutes(minutes: u16) -> String {
    format!("{minutes} min")
}

pub(crate) fn wallet(position: usize) -> String {
    format!("Wallet {position}")
}

pub(crate) fn account(index: u32) -> String {
    format!("Account {}", u64::from(index) + 1)
}

pub(crate) fn multi_chain(words: u8, passphrase: bool) -> String {
    if passphrase {
        format!("Multi-chain, {words} words, with passphrase")
    } else {
        format!("Multi-chain, {words} words")
    }
}

pub(crate) fn ton_wallet(contract: TonContract) -> String {
    match contract {
        TonContract::W5 => "TON wallet (W5)".to_owned(),
        TonContract::V4R2 => "TON wallet (V4R2)".to_owned(),
    }
}

pub(crate) fn looks_multi_chain(words: usize) -> String {
    format!("A multi-chain recovery phrase, {words} words")
}

pub(crate) fn word_number(position: usize) -> String {
    format!("Word #{position}")
}

pub(crate) fn word_mismatch(position: usize) -> String {
    format!("Word #{position} doesn't match, check your note")
}

pub(crate) fn coin_title(name: &str, symbol: &str) -> String {
    format!("{name} ({symbol})")
}

pub(crate) fn with_name(value: &str, name: &str) -> String {
    format!("{value} ({name})")
}

pub(crate) fn with_staked(amount: &str, staked: &str) -> String {
    format!("{amount} + {staked} staked")
}

pub(crate) fn with_value(amount: &str, value: &str) -> String {
    format!("{amount} ≈ {value}")
}

pub(crate) fn at_most(fee: &str) -> String {
    format!("at most {fee}")
}

pub(crate) fn source(amount: &str, network: &str, wallet: &str) -> String {
    format!("{amount} · {network} · {wallet}")
}

pub(crate) fn target(wallet: &str, account: &str, network: &str) -> String {
    format!("{wallet} · {account} · {network}")
}

pub(crate) fn no_funds(symbol: &str) -> String {
    format!("No account here holds {symbol}")
}

pub(crate) fn quote_left(seconds: u64) -> String {
    if seconds == 0 {
        "expired".to_owned()
    } else {
        format!("valid for {seconds} s")
    }
}

pub(crate) fn replace_title(how: Replace) -> &'static str {
    match how {
        Replace::SpeedUp => "Speed up the send",
        Replace::Cancel => "Cancel the send",
    }
}

pub(crate) fn receive_note(network: &str) -> String {
    format!("Only send {network} assets to this address.")
}

pub(crate) fn received(amount: &str, network: &str) -> String {
    format!("Received {amount} on {network}")
}

pub(crate) fn confirmed(amount: &str) -> String {
    format!("{amount} sent and confirmed")
}

pub(crate) fn failed(amount: &str, reason: &str) -> String {
    format!("Sending {amount} failed: {reason}")
}

pub(crate) fn dropped(amount: &str) -> String {
    format!("Sending {amount} was dropped by the network")
}

pub(crate) fn unreachable(networks: &[String]) -> String {
    format!("Can't reach {} right now", networks.join(", "))
}

pub(crate) fn reveal_title(wallet: &str) -> String {
    format!("Recovery phrase of {wallet}")
}

pub(crate) fn remove_title(wallet: &str) -> String {
    format!("Remove {wallet}?")
}

pub(crate) fn remove_account(index: u32) -> String {
    format!("Remove {}", account(index))
}

pub(crate) fn remove_token(symbol: &str) -> String {
    format!("Remove {symbol}")
}

pub(crate) fn token_added(symbol: &str) -> String {
    format!("Added {symbol}")
}

pub(crate) fn status(status: &SentStatus) -> String {
    match status {
        SentStatus::Sending => SENDING.to_owned(),
        SentStatus::Pending => "Waiting to confirm".to_owned(),
        SentStatus::Confirmed => "Confirmed".to_owned(),
        SentStatus::Failed(reason) => format!("Failed: {reason}"),
        SentStatus::Dropped => "Dropped".to_owned(),
    }
}

pub(crate) fn fee_item(item: &FeeItem) -> String {
    match item {
        FeeItem::Gas { units, .. } => format!("Gas ({units} units)"),
        FeeItem::L1Data => "Ethereum data fee".to_owned(),
        FeeItem::Operator => "Operator fee".to_owned(),
        FeeItem::Bandwidth { bytes } => format!("Bandwidth ({bytes} bytes)"),
        FeeItem::Energy { units } => format!("Energy ({units} units)"),
        FeeItem::Activation => "New account activation".to_owned(),
        FeeItem::Compute { gas } => format!("Compute ({gas} gas)"),
        FeeItem::Storage => "Storage".to_owned(),
        FeeItem::Forwarding => "Forwarding".to_owned(),
        FeeItem::Contracts => "Token contracts".to_owned(),
        FeeItem::Memo => "Memo".to_owned(),
    }
}

pub(crate) fn caution(caution: &Caution, ctx: &Ctx<'_>) -> String {
    match caution {
        Caution::LooksLike(known) => {
            let known = ctx
                .data
                .contacts
                .iter()
                .find(|contact| contact.address == *known)
                .map_or_else(|| format::short(&known.to_string()), |contact| contact.name.clone());
            format!("This only looks like {known}: it is a different address. Check every character.")
        }
        Caution::Dust => {
            "A tiny amount like this is often spam, sent to plant a look-alike address in your history."
                .to_owned()
        }
        Caution::DustSender => {
            "This address sent you a tiny amount before. It may be a scam look-alike.".to_owned()
        }
    }
}
