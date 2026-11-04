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
    Swap,
    Receive,
    Contacts,
    Settings,
}

impl Tab {
    pub const ALL: [Self; 7] = [
        Self::Portfolio,
        Self::Activity,
        Self::Send,
        Self::Swap,
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
    Watch,
    Kind(Kind),
    Family(Family),
    Wallet(Option<usize>),
    Direction(Direction),
    Source(usize),
    Buy(usize),
    Route(usize),
    Requote,
    Contact(usize),
    Network(usize),
    Target(usize),
    Currency(Currency),
    Channel(Channel),
    CheckUpdates,
    AutoLock(u16),
    Contract(Contract),
    Path(Path),
    AddressType(AddressType),
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
    Counted,
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
    Ton,
    Solana,
    Bitcoin,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Path {
    Bip44Change,
    Bip44,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddressType {
    NativeSegwit,
    Taproot,
}
