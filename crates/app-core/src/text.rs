use bahaa_engine::swap::{self, Charged, CostKind, Custody, Method, Refusal, SwapStatus};
use bahaa_engine::{
    Caution, ChainFamily, EngineError, FeeItem, RateUnit, Replace, SentStatus, Status, TonContract,
};

use crate::action::{Channel, Direction, Tab};
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
pub(crate) const CREATE: &str = "Create";
pub(crate) const IMPORT: &str = "Import";
pub(crate) const WORDS_12: &str = "12 words";
pub(crate) const WORDS_24: &str = "24 words";
pub(crate) const WATCH: &str = "Watch";
pub(crate) const WATCH_TITLE: &str = "Watch an address";
pub(crate) const WATCH_TEXT: &str =
    "Follow the balances and activity of any address. A watch-only wallet can't send.";
pub(crate) const WATCH_ONLY: &str = "Watch only";
pub(crate) const WATCH_ADDED: &str = "Watching the address";
pub(crate) const COUNT_IN_TOTAL: &str = "Count in my total";
pub(crate) const IMPORT_KEY: &str = "Import a private key";
pub(crate) const IMPORT_KEY_TEXT: &str = "Paste a private key exported from another wallet. One key is one account, on EVM networks, Tron, TON, Solana or Bitcoin.";
pub(crate) const PRIVATE_KEY: &str = "Private key";
pub(crate) const USE_FOR: &str = "Use it for";
pub(crate) const EVM_NETWORKS: &str = "EVM networks";
pub(crate) const TRON: &str = "Tron";
pub(crate) const TON: &str = "TON";
pub(crate) const SOLANA: &str = "Solana";
pub(crate) const BITCOIN: &str = "Bitcoin";
pub(crate) const KEY_OK: &str = "A valid private key";
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
pub(crate) const PORTFOLIO_COLUMNS: [&str; 5] = ["Asset", "Network", "Price", "Amount", "Value"];
pub(crate) const HOLDING_COLUMNS: [&str; 5] = ["Wallet", "Account", "Address", "Amount", "Value"];
pub(crate) const WALLETS: &str = "Wallets";
pub(crate) const ALL_WALLETS: &str = "All";
pub(crate) const SHOW: &str = "Show";
pub(crate) const EMPTY_BALANCES: &str = "Empty balances";
pub(crate) const NO_ASSETS: &str = "Nothing here yet. Receive coins to see them.";
pub(crate) const NETWORK: &str = "Network";
pub(crate) const PRICE: &str = "Price";
pub(crate) const ASSET: &str = "Asset";
pub(crate) const PRICES_OFFLINE: &str = "Prices offline";
pub(crate) const OTHER: &str = "Other";
pub(crate) const CHECKING: &str = "Checking networks…";
pub(crate) const CONNECTED: &str = "Connected";

pub(crate) const NETWORK_STATUS: &str = "Network status";
pub(crate) const NETWORK_STATUS_TEXT: &str = "Balances and history come straight from public servers of each network. Prices come from api.bahaa.io.";
pub(crate) const NETWORK_COLUMNS: [&str; 4] = ["Network", "Balances", "Updated", "History"];
pub(crate) const PRICES: &str = "Prices";
pub(crate) const REACHABLE: &str = "Reachable";
pub(crate) const UNREACHABLE: &str = "Can't reach";
pub(crate) const NOT_CHECKED: &str = "Checking";
pub(crate) const HISTORY_OK: &str = "Readable";
pub(crate) const HISTORY_FAILED: &str = "Can't read now";
pub(crate) const REFRESH_NOW: &str = "Refresh now";

pub(crate) const ACTIVITY: &str = "Activity";
pub(crate) const ACTIVITY_COLUMNS: [&str; 5] = ["When", "Amount", "Network", "With", "Status"];
pub(crate) const NO_ACTIVITY: &str = "No transfers yet";
pub(crate) const NO_MATCHES: &str = "No transfers match these filters";
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
pub(crate) const WAITING: &str = "Waiting for";
pub(crate) const PENDING_NOTE: &str = "Taking long? Speed up sends it again with a higher fee, and Cancel keeps the money in your wallet. Both show the new fee and the expected wait before you confirm anything.";
pub(crate) const EXPECTED_WAIT: &str = "Expected wait";
pub(crate) const EXTRA_COST: &str = "Extra cost";
pub(crate) const WAS_GOING_TO: &str = "Was going to";
pub(crate) const SPEED_UP_NOTE: &str = "This sends the same transfer again with a higher fee. Only one of the two can go through, so you pay one fee, not two.";
pub(crate) const CANCEL_NOTE: &str = "This replaces your pending transfer with one that keeps the money in your wallet. Only one of the two can go through: if the original confirms first, the cancel fails and costs nothing.";
pub(crate) const ESTIMATE_NOTE: &str =
    "Waits are estimates from what the network asks for right now, not promises.";
pub(crate) const FEW_SECONDS: &str = "a few seconds";
pub(crate) const SLOW: &str = "slow, others pay a higher tip";
pub(crate) const STUCK: &str = "stuck at today's fees";
pub(crate) const SECOND: &str = "second";
pub(crate) const MINUTE: &str = "minute";
pub(crate) const HOUR: &str = "hour";
pub(crate) const DAY: &str = "day";

pub(crate) const SEND: &str = "Send";
pub(crate) const SWAP: &str = "Swap";
pub(crate) const PICK_SWAP_SOURCE: &str = "Pick what to swap.";
pub(crate) const NOTHING_TO_SWAP: &str = "There is nothing to swap yet. Receive some coins first.";
pub(crate) const PICK_SWAP_TARGET: &str =
    "Pick what you want to receive. It arrives in this wallet.";
pub(crate) const NO_TARGETS: &str = "This wallet has nothing else to swap to.";
pub(crate) const TARGET_COLUMNS: [&str; 2] = ["Asset", "Network"];
pub(crate) const ROUTE_COLUMNS: [&str; 6] = [
    "Provider",
    "You receive",
    "Value",
    "All costs",
    "Wait",
    "Held by",
];
pub(crate) const FIND_ROUTES: &str = "Find routes";
pub(crate) const REVIEW_BEST: &str = "Review the best route";
pub(crate) const NEW_ROUTES: &str = "Find new routes";
pub(crate) const OTHER_ROUTES: &str = "Other routes";
pub(crate) const NO_ROUTES: &str = "No provider can swap these assets right now.";
pub(crate) const ROUTES_OLD: &str =
    "These prices are older than 30 seconds. Find new routes before you continue.";
pub(crate) const ROUTES_PICK: &str =
    "Routes are ranked by what you receive after every cost. Pick any of them.";
pub(crate) const PROVIDER: &str = "Provider";
pub(crate) const YOU_SEND: &str = "You send";
pub(crate) const YOU_RECEIVE: &str = "You receive";
pub(crate) const RATE: &str = "Rate";
pub(crate) const DEPOSIT_TO: &str = "Deposit to";
pub(crate) const SWAP_CONTRACT: &str = "Swap contract";
pub(crate) const APPROVAL: &str = "Approval";
pub(crate) const TRANSACTIONS: &str = "Transactions";
pub(crate) const APPROVAL_TITLE: &str = "Approval";
pub(crate) const ORDER_TITLE: &str = "Swap order";
pub(crate) const VIEW_ORDER: &str = "View order";
pub(crate) const ORDER_NOTE: &str = "A signed order is not a transaction: nothing leaves your wallet until a solver fills it, and an order that expires takes nothing.";
pub(crate) const ORDER: &str = "Order";
pub(crate) const SIGNED_ORDER: &str = "you sign an order that solvers fill at your minimum or better within 30 minutes, no network fee for the swap itself";
pub(crate) const ALLOWED: &str = "Allowed";
pub(crate) const APPROVED: &str = "Approved";
pub(crate) const SWAP_TO: &str = "To";
pub(crate) const SENDING_DEPOSIT: &str = "Sending your deposit…";
pub(crate) const VIEW_DEPOSIT: &str = "View the deposit";
pub(crate) const NO_PRICE: &str = "There is no market price to compare this route with.";
pub(crate) const HELD_NOBODY: &str = "nobody";
pub(crate) const HELD_CONTRACT: &str = "a contract";
pub(crate) const HELD_PROVIDER: &str = "the provider";
pub(crate) const HELD_ISSUER: &str = "the issuer";
pub(crate) const SWAP_BEST: &str = "best";
pub(crate) const REFUSED_ROUTE: &str = "refused";
pub(crate) const SWAP_NOW: &str = "Swap now";
pub(crate) const SWAP_ALL: &str = "Swap everything";
pub(crate) const SLOW_SHORT: &str = "slow";
pub(crate) const STUCK_SHORT: &str = "stuck";
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
pub(crate) const NOTHING_TO_SEND: &str =
    "Nothing to send yet. Receive coins first, then send them from here.";
pub(crate) const ENTER_RECIPIENT: &str = "Enter who to send to";
pub(crate) const UNKNOWN_ACCOUNT: &str = "This account is not in the wallet anymore";
pub(crate) const UNKNOWN_ASSET: &str = "This asset is not known to the wallet";
pub(crate) const MEMO_NOT_HERE: &str = "This network takes no memo for this asset";
pub(crate) const MEMO_INVALID: &str = "A memo is up to 120 characters of plain text";
pub(crate) const AMOUNT_COMMA: &str = "Use a dot for decimals, without thousands separators";
pub(crate) const AMOUNT_ZERO: &str = "Send more than zero";

pub(crate) const RECEIVE: &str = "Receive";
pub(crate) const PICK_TARGET: &str = "Choose where to receive";
pub(crate) const RECEIVE_COLUMNS: [&str; 2] = ["Network", "Address"];
pub(crate) const BALANCE: &str = "Balance";
pub(crate) const SEND_COLUMNS: [&str; 4] = ["Asset", "Network", "Amount", "Value"];
pub(crate) const SPAM: &str = "Spam";
pub(crate) const HIDE: &str = "Hide";
pub(crate) const QUOTE_GONE: &str = "This quote has expired. Get a new one to send.";
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
pub(crate) const SECURITY: &str = "Security";
pub(crate) const APP: &str = "App";

pub(crate) const WALLET: &str = "Wallet";
pub(crate) const KIND: &str = "Type";
pub(crate) const ACCOUNT_COLUMNS: [&str; 3] = ["Account", "Family", "Address"];
pub(crate) const SAVE_NAME: &str = "Save name";
pub(crate) const SAVED: &str = "Saved";
pub(crate) const ADD_ACCOUNT: &str = "Add account";
pub(crate) const TON_CONTRACT: &str = "TON contract";
pub(crate) const W5: &str = "W5 (current)";
pub(crate) const V4R2: &str = "V4R2 (older)";
pub(crate) const SOLANA_PATH: &str = "Solana path";
pub(crate) const PATH_BIP44_CHANGE: &str = "Phantom, Solflare";
pub(crate) const PATH_BIP44: &str = "Trust Wallet";
pub(crate) const BITCOIN_TYPE: &str = "Bitcoin addresses";
pub(crate) const NATIVE_SEGWIT: &str = "Native SegWit (bc1q)";
pub(crate) const TAPROOT: &str = "Taproot (bc1p)";
pub(crate) const SHOW_PHRASE: &str = "Show recovery phrase";
pub(crate) const SHOW_KEY: &str = "Show private key";
pub(crate) const PRIVACY: &str = "Privacy";
pub(crate) const HIDE_BALANCES: &str = "Hide balances";
pub(crate) const HIDDEN: &str = "••••";
pub(crate) const UPDATES: &str = "Updates";
pub(crate) const CHECK_UPDATES: &str = "Check for updates";
pub(crate) const DEVELOPER: &str = "Developer";
pub(crate) const DEVELOPER_TEXT: &str =
    "For people who build or test. These options show more detail and never touch your keys.";
pub(crate) const DEVELOPER_MODE: &str = "Developer mode";
pub(crate) const FEE_DETAILS: &str = "Fee details when sending";
pub(crate) const DATA_FOLDER: &str = "Data folder";
pub(crate) const REMOVE_WALLET: &str = "Remove wallet";
pub(crate) const REVEAL_TEXT: &str =
    "Anyone who sees this can take your funds. Make sure no one is watching.";
pub(crate) const REMOVE_WATCH_TEXT: &str =
    "The wallet leaves this device. It only watched an address, so you can add it again any time.";
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
pub(crate) const LINK_UNKNOWN_TOKEN: &str = "This payment link asks for a token you have not added";
pub(crate) const LINK_UNKNOWN_NETWORK: &str =
    "This payment link is for a network the wallet does not have";

pub(crate) fn direction(direction: Direction) -> &'static str {
    match direction {
        Direction::All => ALL_WALLETS,
        Direction::Received => RECEIVED,
        Direction::Sent => SENT_TITLE,
    }
}

pub(crate) fn prices(status: Status) -> &'static str {
    match status {
        Status::Idle => "Not needed yet",
        Status::Live => "Live",
        Status::Polling => "Updating every 30 seconds",
        Status::Offline => "Offline",
    }
}

pub(crate) fn network_problem(network: &str, problem: &str) -> String {
    format!("{network}: {problem}")
}

pub(crate) fn history_problem(network: &str, problem: &str) -> String {
    format!("{network} history: {problem}")
}

pub(crate) fn tab(tab: Tab) -> &'static str {
    match tab {
        Tab::Portfolio => PORTFOLIO,
        Tab::Activity => ACTIVITY,
        Tab::Send => SEND,
        Tab::Swap => SWAP,
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

pub(crate) fn left_out(count: usize) -> String {
    if count == 1 {
        "1 watch-only wallet is left out of this total. Pick it above to see it.".to_owned()
    } else {
        format!("{count} watch-only wallets are left out of this total. Pick one above to see it.")
    }
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

pub(crate) fn changed(before: &str, after: &str) -> String {
    format!("{before} → {after}")
}

pub(crate) fn duration(count: u64, unit: &str) -> String {
    if count == 1 {
        format!("1 {unit}")
    } else {
        format!("{count} {unit}s")
    }
}

pub(crate) fn about(span: &str) -> String {
    format!("about {span}")
}

pub(crate) fn more_than(span: &str) -> String {
    format!("more than {span}")
}

pub(crate) fn rate_label(unit: RateUnit) -> &'static str {
    match unit {
        RateUnit::SatPerVbyte => "Fee rate",
        RateUnit::Gwei => "Fee limit",
    }
}

pub(crate) fn rate_unit(unit: RateUnit) -> &'static str {
    match unit {
        RateUnit::SatPerVbyte => "sat/vB",
        RateUnit::Gwei => "gwei",
    }
}

pub(crate) fn at_most(fee: &str) -> String {
    format!("at most {fee}")
}

pub(crate) fn owner(wallet: &str, account: &str) -> String {
    format!("{wallet} · {account}")
}

pub(crate) fn send_title(symbol: &str) -> String {
    format!("Send {symbol}")
}

pub(crate) fn receive_title(network: &str) -> String {
    format!("Receive on {network}")
}

pub(crate) fn valid_address(network: &str) -> String {
    format!("A valid {network} address")
}

pub(crate) fn known_address(name: &str) -> String {
    format!("{name}, from your contacts")
}

pub(crate) fn own_address(wallet: &str) -> String {
    format!("Your own {wallet}")
}

pub(crate) fn approx(value: &str) -> String {
    format!("≈ {value}")
}

pub(crate) fn too_much(balance: &str) -> String {
    format!("More than you have ({balance})")
}

pub(crate) fn spam_hidden(count: usize) -> String {
    match count {
        1 => "1 flagged transfer is hidden".to_owned(),
        many => format!("{many} flagged transfers are hidden"),
    }
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

pub(crate) fn side(asset: &str, network: &str, owner: &str) -> String {
    format!("{asset} · {network} · {owner}")
}

pub(crate) fn on_network(owner: &str, network: &str) -> String {
    format!("{owner} on {network}")
}

pub(crate) fn everything(coin: bool, exact: bool) -> &'static str {
    match (coin, exact) {
        (false, _) => "Your whole balance",
        (true, true) => "Everything after the network fee",
        (true, false) => "Your whole balance, less the network fee",
    }
}

pub(crate) fn at_least(expected: &str, minimum: &str) -> String {
    format!("{expected} · at least {minimum}")
}

pub(crate) fn best(provider: &str) -> String {
    format!("{provider} · {SWAP_BEST}")
}

pub(crate) fn unavailable(provider: &str, reason: &str) -> String {
    format!("{provider}: {}", sentence(reason))
}

pub(crate) fn held_by(method: Method) -> &'static str {
    match (method, method.custody()) {
        (Method::Issuer, _) => HELD_ISSUER,
        (_, Custody::Nobody) => HELD_NOBODY,
        (_, Custody::Escrow) => HELD_CONTRACT,
        (_, Custody::Provider) => HELD_PROVIDER,
    }
}

pub(crate) fn custody(method: Method, provider: &str) -> String {
    if method == Method::Issuer {
        return format!("{provider} · the coin's issuer burns it here and mints it there");
    }
    match method.custody() {
        Custody::Nobody => {
            format!("{provider} · the swap happens in one transaction, nobody holds your money")
        }
        Custody::Escrow => {
            format!("{provider} · its contract holds your money for a few seconds")
        }
        Custody::Provider => format!(
            "{provider} · it holds your money until it pays out, and refunds you if it fails"
        ),
    }
}

pub(crate) fn sending(amount: &str, value: &str, from: &str) -> String {
    format!("{amount} ≈ {value} from {from}")
}

pub(crate) fn receiving(expected: &str, value: &str, minimum: &str, to: &str) -> String {
    format!("{expected} ≈ {value} · at least {minimum}, to {to}")
}

pub(crate) fn rate(sold: &str, bought: &str, market: Option<&str>) -> String {
    match market {
        Some(market) => format!("1 {sold} = {bought} · {market}"),
        None => format!("1 {sold} = {bought}"),
    }
}

pub(crate) fn market(below: bool, percent: &str) -> String {
    if below {
        format!("{percent} below market value")
    } else {
        format!("{percent} above market value")
    }
}

pub(crate) fn cost_kind(kind: CostKind) -> &'static str {
    match kind {
        CostKind::NetworkFee => "Network fee",
        CostKind::ProviderFee => "Provider fee",
        CostKind::PoolFee => "Pool fee",
    }
}

pub(crate) fn charged(amount: &str, charged: Charged) -> String {
    match charged {
        Charged::Extra => format!("{amount}, paid from your balance"),
        Charged::InQuote => format!("{amount}, included in the rate"),
    }
}

pub(crate) fn after_deposit(wait: &str) -> String {
    format!("{wait}, after your deposit confirms")
}

pub(crate) fn costs(value: &str, percent: &str) -> String {
    format!("{value} · {percent}")
}

pub(crate) fn swap_title(amount: &str, bought: &str, network: &str) -> String {
    format!("Swap {amount} to {bought} on {network}")
}

pub(crate) fn swap_headline(sold: &str, bought: &str) -> String {
    format!("{sold} → {bought}")
}

pub(crate) fn route_caution(caution: swap::Caution, percent: impl Fn(u32) -> String) -> String {
    match caution {
        swap::Caution::BelowMarket(bps) => format!(
            "You get {} less than the market value of what you send.",
            percent(bps)
        ),
        swap::Caution::CostlyFees(bps) => {
            format!("Network fees are {} of what you send.", percent(bps))
        }
        swap::Caution::Unpriced => NO_PRICE.to_owned(),
    }
}

pub(crate) fn refusal(refusal: Refusal, percent: impl Fn(u32) -> String) -> String {
    match refusal {
        Refusal::BelowMarket(bps) => format!("{REFUSED_ROUTE}: {} below market", percent(bps)),
        Refusal::Inconsistent => format!("{REFUSED_ROUTE}: the quote doesn't add up"),
    }
}

pub(crate) fn approval_line(amount: &str) -> String {
    format!("lets the swap contract take exactly {amount}, all of it used by this swap")
}

pub(crate) fn order_approval(amount: &str) -> String {
    format!("lets CoW Swap take exactly {amount} when a solver fills your order")
}

pub(crate) fn order_transactions(approvals: usize) -> String {
    match approvals {
        0 => "none, you only sign the order".to_owned(),
        1 => "1: an approval, then you sign the order".to_owned(),
        count => format!("{count}: clears an older approval, approves, then you sign the order"),
    }
}

pub(crate) fn transactions(count: usize) -> String {
    match count {
        1 => "1: the swap".to_owned(),
        2 => "2: an approval, then the swap".to_owned(),
        _ => format!("{count}: clears an older approval, approves, then swaps"),
    }
}

pub(crate) fn checked_note(sold: &str, minimum: &str) -> String {
    format!(
        "Checked before signing: exactly {sold} leaves this wallet, at least {minimum} arrives, and nothing else moves."
    )
}

pub(crate) fn checked_deposit(sold: &str) -> String {
    format!("Checked before signing: exactly {sold} leaves this wallet and nothing else moves.")
}

pub(crate) fn pool_note(symbol: &str) -> String {
    format!(
        "If the price drops below the minimum before the swap confirms, it fails and your {symbol} stays in your wallet."
    )
}

pub(crate) fn approved_for(party: &str) -> String {
    format!("Approval for {party}")
}

pub(crate) fn exactly(amount: &str) -> String {
    format!("exactly {amount}")
}

pub(crate) fn issuer_note(provider: &str, symbol: &str, network: &str) -> String {
    format!(
        "{provider} burns your {symbol} here and mints it to your account on {network}. A burn can't be undone, so nothing is refunded."
    )
}

pub(crate) fn refund_note(provider: &str, symbol: &str) -> String {
    format!("If {provider} can't finish the swap, it sends your {symbol} back to this wallet.")
}

pub(crate) fn swap_status(status: &SwapStatus) -> String {
    match status {
        SwapStatus::Waiting => "Waiting for your deposit".to_owned(),
        SwapStatus::Received => "Deposit received".to_owned(),
        SwapStatus::Swapping => "Swapping".to_owned(),
        SwapStatus::Done => "Done".to_owned(),
        SwapStatus::Refunded => "Refunded".to_owned(),
        SwapStatus::Failed(reason) => format!("Failed: {reason}"),
    }
}

pub(crate) fn swap_done(sold: &str, bought: &str) -> String {
    format!("Swap done: {sold} to {bought}")
}

pub(crate) fn swap_refunded(sold: &str) -> String {
    format!("Swap refunded: {sold} is on its way back")
}

pub(crate) fn swap_failed(sold: &str) -> String {
    format!("The swap of {sold} failed")
}

pub(crate) fn swapped_to(bought: &str, network: &str) -> String {
    format!("Swap to {bought} on {network}")
}

pub(crate) fn replace_note(how: Replace) -> &'static str {
    match how {
        Replace::SpeedUp => SPEED_UP_NOTE,
        Replace::Cancel => CANCEL_NOTE,
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

fn named(networks: &[String]) -> String {
    match networks {
        [one] => one.clone(),
        [one, two] => format!("{one} and {two}"),
        many => format!("{} networks", many.len()),
    }
}

pub(crate) fn unreachable(networks: &[String]) -> String {
    format!("Can't reach {}", named(networks))
}

pub(crate) fn loading(networks: &[String]) -> String {
    format!("Loading {}…", named(networks))
}

pub(crate) fn total_pending(networks: &[String]) -> String {
    format!(
        "Still loading {}. The total isn't final yet.",
        named(networks)
    )
}

pub(crate) fn history_pending(networks: &[String]) -> String {
    format!("Still reading the history of {}.", named(networks))
}

pub(crate) fn hidden_amount(symbol: &str) -> String {
    format!("{HIDDEN} {symbol}")
}

pub(crate) fn channel(channel: Channel) -> &'static str {
    match channel {
        Channel::Off => "Off",
        Channel::Stable => "Stable",
        Channel::Beta => "Beta",
    }
}

fn family(family: ChainFamily) -> &'static str {
    match family {
        ChainFamily::Evm => EVM_NETWORKS,
        ChainFamily::Tron => TRON,
        ChainFamily::Ton => TON,
        ChainFamily::Solana => SOLANA,
        ChainFamily::Bitcoin => BITCOIN,
    }
}

pub(crate) fn not_an_address(families: &[ChainFamily]) -> String {
    let names: Vec<&str> = families.iter().map(|of| family(*of)).collect();
    let listed = match names.split_last() {
        Some((last, [])) => (*last).to_owned(),
        Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
        None => String::new(),
    };
    format!("This is not an address on {listed}")
}

pub(crate) fn key_wallet(of: ChainFamily) -> String {
    format!("{PRIVATE_KEY}, {}", family(of))
}

pub(crate) fn watch_wallet(of: ChainFamily) -> String {
    format!("{WATCH_ONLY}, {}", family(of))
}

pub(crate) fn watch_hint(of: ChainFamily) -> String {
    format!("An address on {}", family(of))
}

pub(crate) fn key_title(wallet: &str) -> String {
    format!("Private key of {wallet}")
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
        FeeItem::Signature => "Signature fee".to_owned(),
        FeeItem::Priority { units } => format!("Priority fee ({units} compute units)"),
        FeeItem::Size { vbytes } => format!("Size ({vbytes} vbytes)"),
        FeeItem::Leftover => "Change too small to keep".to_owned(),
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
