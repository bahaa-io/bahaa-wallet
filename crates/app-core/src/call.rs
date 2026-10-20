use bahaa_engine::{
    Account, Activity, Address, AssetId, Caution, ChainFamily, Contact, CustomToken, Draft,
    DraftId, Engine, EngineError, NetworkId, NewPhrase, PhraseKind, Replace, SecretString,
    SendRequest, SentTx, Settings, TonContract, WalletId, WalletInfo,
};

pub(crate) enum Call {
    Create(SecretString),
    Unlock(SecretString),
    Lock,
    Refresh,
    Generate(NewPhrase),
    Import(SecretString, SecretString, Option<PhraseKind>),
    ImportKey(SecretString, ChainFamily),
    Rename(WalletId, String),
    Remove(WalletId),
    CheckPassword(SecretString),
    AddAccount(WalletId),
    RemoveAccount(WalletId, u32),
    SetTonContract(WalletId, TonContract),
    Reveal(WalletId, SecretString),
    ChangePassword(SecretString, SecretString),
    SetSettings(Settings),
    Prepare(SendRequest),
    Replace(NetworkId, String, Replace),
    Confirm(DraftId, SecretString),
    Cautions(Address),
    SaveContact(Contact),
    RemoveContact(NetworkId, Address),
    AddToken(AssetId),
    RemoveToken(AssetId),
}

pub(crate) enum Outcome {
    Done(Result<(), EngineError>),
    Generated(Result<(WalletId, SecretString), EngineError>),
    Imported(Result<WalletId, EngineError>),
    Revealed(Result<SecretString, EngineError>),
    Drafted(Result<Draft, EngineError>),
    Sent(Result<SentTx, EngineError>),
    Cautions(Result<Vec<Caution>, EngineError>),
    Token(Result<CustomToken, EngineError>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Load {
    Wallets,
    Accounts,
    Activity,
    Contacts,
    Tokens,
    Settings,
}

impl Load {
    pub(crate) const ALL: [Self; 6] = [
        Self::Wallets,
        Self::Accounts,
        Self::Activity,
        Self::Contacts,
        Self::Tokens,
        Self::Settings,
    ];
}

pub(crate) enum Loaded {
    Wallets(Vec<WalletInfo>),
    Accounts(Vec<Account>),
    Activity(Vec<Activity>),
    Contacts(Vec<Contact>),
    Tokens(Vec<CustomToken>),
    Settings(Settings),
}

pub(crate) async fn run(engine: &Engine, call: Call) -> Outcome {
    match call {
        Call::Create(password) => done(engine.create(password).await),
        Call::Unlock(password) => done(engine.unlock(password).await),
        Call::Lock => done(engine.lock().await),
        Call::Refresh => {
            engine.refresh();
            Outcome::Done(Ok(()))
        }
        Call::Generate(words) => {
            let passphrase = SecretString::from(String::new());
            Outcome::Generated(engine.generate(words, passphrase).await)
        }
        Call::Import(phrase, passphrase, kind) => Outcome::Imported(match kind {
            Some(kind) => engine.import_as(phrase, passphrase, kind).await,
            None => engine.import(phrase, passphrase).await,
        }),
        Call::ImportKey(key, family) => Outcome::Imported(engine.import_key(key, family).await),
        Call::Rename(wallet, name) => done(engine.rename_wallet(wallet, name).await),
        Call::Remove(wallet) => done(engine.remove(wallet).await),
        Call::CheckPassword(password) => done(engine.check_password(password).await),
        Call::AddAccount(wallet) => done(engine.add_account(wallet).await),
        Call::RemoveAccount(wallet, index) => done(engine.remove_account(wallet, index).await),
        Call::SetTonContract(wallet, contract) => {
            done(engine.set_ton_contract(wallet, contract).await)
        }
        Call::Reveal(wallet, password) => Outcome::Revealed(engine.reveal(wallet, password).await),
        Call::ChangePassword(old, new) => done(engine.change_password(old, new).await),
        Call::SetSettings(settings) => done(engine.set_settings(settings).await),
        Call::Prepare(request) => Outcome::Drafted(engine.prepare_send(request).await),
        Call::Replace(network, id, how) => Outcome::Drafted(engine.replace(network, id, how).await),
        Call::Confirm(draft, password) => Outcome::Sent(engine.confirm_send(draft, password).await),
        Call::Cautions(address) => Outcome::Cautions(engine.cautions(address).await),
        Call::SaveContact(contact) => done(engine.save_contact(contact).await),
        Call::RemoveContact(network, address) => {
            done(engine.remove_contact(network, address).await)
        }
        Call::AddToken(asset) => Outcome::Token(engine.add_token(asset).await),
        Call::RemoveToken(asset) => done(engine.remove_token(asset).await),
    }
}

fn done<T>(result: Result<T, EngineError>) -> Outcome {
    Outcome::Done(result.map(drop))
}

pub(crate) async fn load(engine: &Engine, load: Load) -> Option<Loaded> {
    match load {
        Load::Wallets => engine.wallets().await.ok().map(Loaded::Wallets),
        Load::Accounts => engine.accounts().await.ok().map(Loaded::Accounts),
        Load::Activity => engine.activity().await.ok().map(Loaded::Activity),
        Load::Contacts => engine.contacts().await.ok().map(Loaded::Contacts),
        Load::Tokens => engine.tokens().await.ok().map(Loaded::Tokens),
        Load::Settings => engine.settings().await.ok().map(Loaded::Settings),
    }
}
