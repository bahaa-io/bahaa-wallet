#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Type(Field, String),
    Submit,
    Back,
    Go(Go),
    Pick(Pick),
    Toggle(Toggle),
    Copy,
    Explore,
    Dismiss(u64),
    Lock,
    Refresh,
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Field {
    Password,
    NewPassword,
    Confirm,
    Phrase,
    Passphrase,
    Word(usize),
    Key,
    To,
    Amount,
    Memo,
    Name,
    Address,
    Contract,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Portfolio,
    Activity,
    Send,
    Receive,
    Contacts,
    Settings,
}

impl Tab {
    pub const ALL: [Self; 6] = [
        Self::Portfolio,
        Self::Activity,
        Self::Send,
        Self::Receive,
        Self::Contacts,
        Self::Settings,
    ];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Go {
    Tab(Tab),
    Asset(usize),
    Transfer(usize),
    Contact(usize),
    NewContact,
    Wallet(usize),
    AddWallet,
    Tokens,
    NewToken,
    ChangePassword,
    Reveal,
    RemoveWallet,
    SendAsset,
    ReceiveAsset,
    Networks,
    Developer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    Create(Words),
    Import,
    ImportKey,
    Kind(Kind),
    Family(Family),
    Wallet(Option<usize>),
    Direction(Direction),
    Source(usize),
    Contact(usize),
    Network(usize),
    Target(usize),
    Currency(Currency),
    Channel(Channel),
    CheckUpdates,
    AutoLock(u16),
    Contract(Contract),
    AddAccount,
    RemoveAccount(u32),
    RemoveToken(usize),
    SpeedUp,
    Cancel,
    Delete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Toggle {
    Max,
    HideEmpty,
    HideSpam,
    HideBalances,
    Developer,
    Testnets,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Direction {
    #[default]
    All,
    Received,
    Sent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Words {
    Twelve,
    TwentyFour,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Family {
    #[default]
    Evm,
    Tron,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    MultiChain,
    Ton,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Currency {
    Usd,
    Eur,
    Irt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Off,
    Stable,
    Beta,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Contract {
    W5,
    V4R2,
}
