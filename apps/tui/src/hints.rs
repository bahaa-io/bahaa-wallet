use std::fmt::Display;

use bahaa_app_core::Action;

pub(crate) const BRAND_MARK: &str = "◆";
pub(crate) const BRAND_NAME: &str = "Bahaa";
pub(crate) const BRAND_WALLET: &str = "Wallet";
pub(crate) const LOGO: [&str; 3] = [
    "█▀▄ ▄▀▄ █ █ ▄▀▄ ▄▀▄",
    "█▀▄ █▀█ █▀█ █▀█ █▀█",
    "▀▀  ▀ ▀ ▀ ▀ ▀ ▀ ▀ ▀",
];
pub(crate) const LOGO_WORD: &str = "w a l l e t";
pub(crate) const ABOUT: &str = "A non-custodial wallet for EVM networks, Tron and TON";
pub(crate) const DATA_HELP: &str =
    "Folder for the wallet's files (default: the system's app data folder)";
pub(crate) const NO_DATA_DIR: &str =
    "No app data folder was found on this system. Pass one with --data.";
pub(crate) const TOO_SMALL: &str = "Make the window larger: at least 80 × 24";
pub(crate) const NO_CLIPBOARD: &str = "The clipboard isn't available here";
pub(crate) const NO_BROWSER: &str = "The browser couldn't be opened";
pub(crate) const WORKING: &str = "Working…";
pub(crate) const WORDS_HIDDEN: &str = "Press s to show the words. Make sure no one is watching.";
pub(crate) const QR_TITLE: &str = "Scan to get the address";
pub(crate) const QR_TOO_BIG: &str = "Make the window larger to show the QR code";
pub(crate) const KEYS_TOO_BIG: &str = "Make the window larger to see all keys";
pub(crate) const TAB_KEYS: [&str; 9] = ["1", "2", "3", "4", "5", "6", "7", "8", "9"];
pub(crate) const MORE_ABOVE: &str = "▲";
pub(crate) const MORE_BELOW: &str = "▼";

pub(crate) const MENU_TITLE: &str = "Commands";
pub(crate) const MENU_PROMPT: &str = "Type to search";
pub(crate) const MENU_EMPTY: &str = "Nothing matches";
pub(crate) const KEYS_TITLE: &str = "Keys";
pub(crate) const KIND_ACTION: &str = "action";
pub(crate) const KIND_OPTION: &str = "option";
pub(crate) const KIND_OPEN: &str = "open";
pub(crate) const KIND_TAB: &str = "tab";
pub(crate) const KIND_APP: &str = "app";
pub(crate) const STATUS: &str = "Network status";
pub(crate) const SHOW_QR: &str = "Show the QR code";
pub(crate) const SHOW_WORDS: &str = "Show or hide the words";
pub(crate) const SHOW_KEYS: &str = "Show all keys";
pub(crate) const REFRESH: &str = "Refresh balances";
pub(crate) const LOCK_WALLET: &str = "Lock the wallet";
pub(crate) const GO_BACK: &str = "Go back";
pub(crate) const QUIT_APP: &str = "Quit";

pub(crate) const UPDATE_TITLE: &str = "Update available";
pub(crate) const UPDATES_TITLE: &str = "Updates";
pub(crate) const WHATS_NEW: &str = "What's new";
pub(crate) const NO_NOTES: &str = "This release has no notes.";
pub(crate) const UPDATE_NOW: &str = "Update now";
pub(crate) const LATER: &str = "Later";
pub(crate) const DONE: &str = "OK";
pub(crate) const UPDATE_UNREACHABLE: &str = "GitHub can't be reached right now";
pub(crate) const UPDATE_UNREADABLE: &str = "GitHub sent a reply this app can't read";
pub(crate) const UPDATE_TOO_BIG: &str = "the download is larger than any release should be";
pub(crate) const UPDATE_UNSIGNED: &str = "the release isn't signed with the Bahaa release key";
pub(crate) const UPDATE_MISSING: &str = "the release has no build for this computer";
pub(crate) const UPDATE_DAMAGED: &str = "the download doesn't match its signed checksum";
pub(crate) const UPDATE_REPLACE: &str =
    "the app file couldn't be replaced, check that its folder is writable";

pub(crate) const CLICK_TO_TYPE: &str = "Click to type here";
pub(crate) const DISMISS: &str = "Click to dismiss";
pub(crate) const OPEN_STATUS: &str = "Click for network status";

pub(crate) type Hint = (&'static str, &'static str);

pub(crate) const SUBMIT: Hint = ("Enter", "submit");
pub(crate) const OPEN: Hint = ("Enter", "open");
pub(crate) const PRESS: Hint = ("Enter", "select");
pub(crate) const RUN: Hint = ("Enter", "run");
pub(crate) const PICK: Hint = ("Space", "pick");
pub(crate) const MOVE: Hint = ("↑↓", "move");
pub(crate) const CHOOSE: Hint = ("←→", "choose");
pub(crate) const NEXT: Hint = ("Tab", "next");
pub(crate) const COMPLETE: Hint = ("Tab", "complete");
pub(crate) const BACK: Hint = ("Esc", "back");
pub(crate) const SHOW: Hint = ("Ctrl-R", "show/hide");
pub(crate) const WORDS: Hint = ("s", "show/hide words");
pub(crate) const COPY: Hint = ("y", "copy");
pub(crate) const QR: Hint = ("v", "QR code");
pub(crate) const CLOSE: Hint = ("Esc", "close");
pub(crate) const UPDATE: Hint = ("Enter", "update");
pub(crate) const NOT_NOW: Hint = ("Esc", "later");
pub(crate) const SCROLL: Hint = ("↑↓", "scroll");
pub(crate) const OK: Hint = ("Enter", "close");
pub(crate) const EXPLORE: Hint = ("o", "explorer");
pub(crate) const MENU: Hint = ("Ctrl-K", "menu");
pub(crate) const KEYS: Hint = ("?", "keys");
pub(crate) const TABS: Hint = ("1-6", "tabs");
pub(crate) const LOCK: Hint = ("Ctrl-L", "lock");
pub(crate) const QUIT: Hint = ("q", "quit");
pub(crate) const FORCE_QUIT: Hint = ("Ctrl-C", "quit");

pub(crate) const KEY_GROUPS: [(&str, &[Hint]); 4] = [
    (
        "Move around",
        &[
            ("Tab", "next field"),
            ("Shift-Tab", "previous field"),
            ("↑↓ ←→", "move inside"),
            ("Enter", "select or open"),
            ("Esc", "back"),
            ("1-6", "switch tabs"),
            ("Alt-1-6", "tabs while typing"),
            ("PgUp PgDn", "scroll"),
        ],
    ),
    (
        "Do things",
        &[
            ("Ctrl-K  :", "command menu"),
            ("y", "copy"),
            ("o", "open in the explorer"),
            ("v", "QR code"),
            ("s", "show or hide words"),
            ("r", "refresh balances"),
            ("Ctrl-L", "lock"),
            ("q  Ctrl-C", "quit"),
        ],
    ),
    (
        "While typing",
        &[
            ("Tab", "complete a word"),
            ("Ctrl-R", "show or hide a secret"),
            ("Ctrl-U", "clear the field"),
            ("Ctrl-W", "delete a word"),
        ],
    ),
    (
        "Mouse",
        &[
            ("Click", "select, open or press"),
            ("Right click", "command menu"),
            ("Wheel", "scroll"),
        ],
    ),
];

pub(crate) fn shortcut(action: &Action) -> Option<&'static str> {
    match action {
        Action::Submit => Some("Enter"),
        Action::Back => Some("Esc"),
        Action::Copy => Some("y"),
        Action::Explore => Some("o"),
        Action::Refresh => Some("r"),
        Action::Lock => Some("Ctrl-L"),
        Action::Quit => Some("q"),
        _ => None,
    }
}

pub(crate) fn go_to(tab: &str) -> String {
    format!("Go to {tab}")
}

pub(crate) fn pick(choice: &str) -> String {
    format!("Pick {choice}")
}

pub(crate) fn open(row: &str) -> String {
    format!("Open {row}")
}

pub(crate) fn key_for(key: &str) -> String {
    format!("Shortcut: {key}")
}

pub(crate) fn offer(new: impl Display, current: impl Display) -> String {
    format!("Bahaa Wallet {new} is ready. You have {current}.")
}

pub(crate) fn updating(version: impl Display) -> String {
    format!("Downloading {version} and checking its signature…")
}

pub(crate) fn updated(version: impl Display) -> String {
    format!("Updated to {version}. Quit and open Bahaa Wallet again to use it.")
}

pub(crate) fn latest(current: impl Display) -> String {
    format!("You have the latest version ({current}).")
}

pub(crate) fn update_failed(reason: &str) -> String {
    format!("Couldn't update: {reason}.")
}

pub(crate) fn cannot_start(err: impl Display) -> String {
    format!("Bahaa Wallet couldn't start: {err}")
}
