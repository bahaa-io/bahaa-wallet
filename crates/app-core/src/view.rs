use std::fmt;
use std::sync::Arc;

use zeroize::Zeroizing;

use crate::action::{Action, Field};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct View {
    pub page: Page,
    pub tabs: Vec<Choice>,
    pub banner: Option<String>,
    pub toasts: Vec<Toast>,
    pub busy: bool,
    pub handled: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Page {
    pub id: PageId,
    pub title: String,
    pub text: Option<String>,
    pub headline: Option<String>,
    pub lines: Vec<Line>,
    pub form: Vec<Item>,
    pub table: Option<Table>,
    pub notes: Vec<Note>,
    pub error: Option<String>,
    pub buttons: Vec<Button>,
    pub qr: Option<String>,
    pub secret: Option<Secret>,
}

impl Page {
    pub fn inputs(&self) -> impl Iterator<Item = &Input> {
        self.form.iter().filter_map(|item| match item {
            Item::Input(input) => Some(input),
            Item::Group(_) => None,
        })
    }

    pub fn groups(&self) -> impl Iterator<Item = &Group> {
        self.form.iter().filter_map(|item| match item {
            Item::Group(group) => Some(group),
            Item::Input(_) => None,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PageId {
    #[default]
    Starting,
    Welcome,
    NewPassword,
    Phrase,
    Verify,
    Import,
    Unlock,
    Portfolio,
    Asset,
    Activity,
    Transfer,
    Compose,
    Review,
    Confirm,
    Sent,
    Receive,
    Contacts,
    Contact,
    Settings,
    Wallet,
    Reveal,
    RemoveWallet,
    Tokens,
    NewToken,
    ChangePassword,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub label: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    Group(Group),
    Input(Input),
}

impl From<Group> for Item {
    fn from(group: Group) -> Self {
        Self::Group(group)
    }
}

impl From<Input> for Item {
    fn from(input: Input) -> Self {
        Self::Input(input)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub label: String,
    pub choices: Vec<Choice>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    pub label: String,
    pub action: Action,
    pub selected: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Input {
    pub field: Field,
    pub label: String,
    pub value: Value,
    pub hint: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Text(String),
    Hidden(Secret),
}

impl Value {
    pub fn text(&self) -> &str {
        match self {
            Self::Text(text) => text,
            Self::Hidden(secret) => secret.expose(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Table {
    pub columns: Vec<String>,
    pub rows: Vec<Row>,
    pub empty: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub cells: Vec<String>,
    pub open: Option<Action>,
    pub warn: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub level: Level,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Button {
    pub label: String,
    pub action: Action,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toast {
    pub id: u64,
    pub level: Level,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    Copy(String),
    ClearClipboard,
    Open(String),
    Quit,
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secret(Arc<Zeroizing<String>>);

impl Secret {
    pub(crate) fn new(text: &str) -> Self {
        Self(Arc::new(Zeroizing::new(text.to_owned())))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn words(&self) -> impl Iterator<Item = &str> {
        self.0.split_whitespace()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([redacted])")
    }
}
