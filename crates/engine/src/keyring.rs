use std::fmt;
use std::fs::DirBuilder;
use std::path::{Path, PathBuf};

use bahaa_crypto::{
    ExposeSecret, Mnemonic, MnemonicError, SecretString, Seed, TonMnemonic, TonPhraseError,
    WordCount,
};
use bahaa_types::TonContract;
use bahaa_vault::{KdfParams, Vault, VaultError};
use thiserror::Error;

use crate::payload::{self, Contents, Entry, Phrase};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WalletId(u32);

impl WalletId {
    pub fn value(self) -> u32 {
        self.0
    }
}

#[cfg(test)]
pub(crate) fn test_wallet(id: u32) -> WalletId {
    WalletId(id)
}

impl fmt::Display for WalletId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WalletInfo {
    pub id: WalletId,
    pub words: WordCount,
    pub passphrase: bool,
    pub kind: WalletKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WalletKind {
    Bip39,
    Ton(TonContract),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhraseKind {
    Bip39,
    Ton,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NewPhrase {
    #[default]
    Twelve,
    TwentyFour,
}

impl NewPhrase {
    fn words(self) -> WordCount {
        match self {
            Self::Twelve => WordCount::Twelve,
            Self::TwentyFour => WordCount::TwentyFour,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum KeyringError {
    #[error("no wallet has been set up on this device yet")]
    NoVault,
    #[error("a wallet vault already exists here")]
    AlreadyExists,
    #[error("the wallet is locked")]
    Locked,
    #[error("wrong password")]
    WrongPassword,
    #[error("this wallet is already in the vault")]
    Duplicate,
    #[error("there is no such wallet")]
    UnknownWallet,
    #[error("the vault holds at most 64 wallets")]
    TooManyWallets,
    #[error("the passphrase is too long")]
    PassphraseTooLong,
    #[error("the vault contents are damaged")]
    Damaged,
    #[error("this vault was written by a newer Bahaa Wallet, update the app")]
    Newer,
    #[error(
        "these words work both as a TON phrase and as a multi-chain phrase, choose which app made them"
    )]
    Ambiguous,
    #[error("TON recovery phrases with a password are not supported yet")]
    TonPassphrase,
    #[error(transparent)]
    TonPhrase(#[from] TonPhraseError),
    #[error(transparent)]
    Vault(VaultError),
    #[error(transparent)]
    Mnemonic(#[from] MnemonicError),
}

impl From<VaultError> for KeyringError {
    fn from(err: VaultError) -> Self {
        match err {
            VaultError::WrongPassword => Self::WrongPassword,
            VaultError::Io(std::io::ErrorKind::NotFound) => Self::NoVault,
            other => Self::Vault(other),
        }
    }
}

pub struct Keyring {
    path: PathBuf,
    params: KdfParams,
    session: Option<Session>,
}

struct Session {
    vault: Vault,
    contents: Contents,
    seeds: Vec<Seed>,
}

impl Keyring {
    pub fn new(path: impl Into<PathBuf>, params: KdfParams) -> Self {
        Self {
            path: path.into(),
            params,
            session: None,
        }
    }

    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    pub fn is_unlocked(&self) -> bool {
        self.session.is_some()
    }

    pub fn create(&mut self, password: &[u8]) -> Result<(), KeyringError> {
        if self.exists() {
            return Err(KeyringError::AlreadyExists);
        }
        if let Some(parent) = self
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            private_dir(parent)?;
        }
        let session = Session {
            vault: Vault::create(password, self.params)?,
            contents: Contents {
                next_id: 1,
                entries: Vec::new(),
            },
            seeds: Vec::new(),
        };
        persist(&self.path, &session)?;
        self.session = Some(session);
        Ok(())
    }

    pub fn unlock(&mut self, password: &[u8]) -> Result<(), KeyringError> {
        let file = bahaa_vault::read(&self.path)?;
        let (vault, payload) = Vault::open(&file, password)?;
        let contents = payload::decode(payload.expose_secret())?;
        let seeds = contents
            .entries
            .iter()
            .map(|entry| seed(&entry.phrase))
            .collect::<Result<_, _>>()?;
        self.session = Some(Session {
            vault,
            contents,
            seeds,
        });
        Ok(())
    }

    pub fn lock(&mut self) {
        self.session = None;
    }

    pub fn wallets(&self) -> Result<Vec<WalletInfo>, KeyringError> {
        Ok(self
            .session()?
            .contents
            .entries
            .iter()
            .map(|entry| match &entry.phrase {
                Phrase::Bip39 {
                    mnemonic,
                    passphrase,
                } => WalletInfo {
                    id: WalletId(entry.id),
                    words: mnemonic.word_count(),
                    passphrase: !passphrase.expose_secret().is_empty(),
                    kind: WalletKind::Bip39,
                },
                Phrase::Ton { contract, .. } => WalletInfo {
                    id: WalletId(entry.id),
                    words: WordCount::TwentyFour,
                    passphrase: false,
                    kind: WalletKind::Ton(*contract),
                },
            })
            .collect())
    }

    pub fn generate(
        &mut self,
        words: NewPhrase,
        passphrase: &str,
    ) -> Result<(WalletId, SecretString), KeyringError> {
        self.session()?;
        let mnemonic = Mnemonic::generate(words.words())?;
        let phrase = mnemonic.phrase();
        let id = self.add(bip39(mnemonic, passphrase)?)?;
        Ok((id, phrase))
    }

    pub fn import(&mut self, phrase: &str, passphrase: &str) -> Result<WalletId, KeyringError> {
        self.session()?;
        let multi = Mnemonic::parse(phrase);
        let ton = passphrase
            .is_empty()
            .then(|| TonMnemonic::parse(phrase).ok())
            .flatten();
        match (multi, ton) {
            (Ok(_), Some(_)) => Err(KeyringError::Ambiguous),
            (Ok(mnemonic), None) => self.add(bip39(mnemonic, passphrase)?),
            (Err(_), Some(mnemonic)) => self.add(Phrase::Ton {
                mnemonic,
                contract: TonContract::W5,
            }),
            (Err(err), None) => Err(err.into()),
        }
    }

    pub fn import_as(
        &mut self,
        phrase: &str,
        passphrase: &str,
        kind: PhraseKind,
    ) -> Result<WalletId, KeyringError> {
        self.session()?;
        match kind {
            PhraseKind::Bip39 => self.add(bip39(Mnemonic::parse(phrase)?, passphrase)?),
            PhraseKind::Ton if !passphrase.is_empty() => Err(KeyringError::TonPassphrase),
            PhraseKind::Ton => self.add(Phrase::Ton {
                mnemonic: TonMnemonic::parse(phrase)?,
                contract: TonContract::W5,
            }),
        }
    }

    pub fn remove(&mut self, id: WalletId) -> Result<(), KeyringError> {
        let path = self.path.clone();
        let session = self.session_mut()?;
        let index = session.index(id)?;
        let entry = session.contents.entries.remove(index);
        let seed = session.seeds.remove(index);
        if let Err(err) = persist(&path, session) {
            session.contents.entries.insert(index, entry);
            session.seeds.insert(index, seed);
            return Err(err);
        }
        Ok(())
    }

    pub fn seed(&self, id: WalletId) -> Result<&Seed, KeyringError> {
        let session = self.session()?;
        Ok(&session.seeds[session.index(id)?])
    }

    pub fn check_password(&self, password: &[u8]) -> Result<(), KeyringError> {
        self.session()?;
        self.verify(password)
    }

    pub fn reveal(&self, id: WalletId, password: &[u8]) -> Result<SecretString, KeyringError> {
        let session = self.session()?;
        let index = session.index(id)?;
        self.verify(password)?;
        Ok(match &session.contents.entries[index].phrase {
            Phrase::Bip39 { mnemonic, .. } => mnemonic.phrase(),
            Phrase::Ton { mnemonic, .. } => mnemonic.phrase(),
        })
    }

    pub fn change_password(&mut self, old: &[u8], new: &[u8]) -> Result<(), KeyringError> {
        self.session()?;
        self.verify(old)?;
        let path = self.path.clone();
        let params = self.params;
        let session = self.session_mut()?;
        let previous = session.vault.params();
        session.vault.change_password(new, params)?;
        if let Err(err) = persist(&path, session) {
            session.vault.change_password(old, previous)?;
            return Err(err);
        }
        Ok(())
    }

    fn add(&mut self, phrase: Phrase) -> Result<WalletId, KeyringError> {
        let path = self.path.clone();
        let session = self.session_mut()?;
        let seed = seed(&phrase)?;
        if session
            .seeds
            .iter()
            .any(|known| known.kind() == seed.kind() && known.expose() == seed.expose())
        {
            return Err(KeyringError::Duplicate);
        }
        let id = session.contents.next_id;
        let next = id.checked_add(1).ok_or(KeyringError::TooManyWallets)?;
        session.contents.entries.push(Entry { id, phrase });
        session.seeds.push(seed);
        session.contents.next_id = next;
        if let Err(err) = persist(&path, session) {
            session.contents.entries.pop();
            session.seeds.pop();
            session.contents.next_id = id;
            return Err(err);
        }
        Ok(WalletId(id))
    }

    fn verify(&self, password: &[u8]) -> Result<(), KeyringError> {
        let file = bahaa_vault::read(&self.path)?;
        Vault::open(&file, password)?;
        Ok(())
    }

    fn session(&self) -> Result<&Session, KeyringError> {
        self.session.as_ref().ok_or(KeyringError::Locked)
    }

    fn session_mut(&mut self) -> Result<&mut Session, KeyringError> {
        self.session.as_mut().ok_or(KeyringError::Locked)
    }
}

impl Session {
    fn index(&self, id: WalletId) -> Result<usize, KeyringError> {
        self.contents
            .entries
            .iter()
            .position(|entry| entry.id == id.0)
            .ok_or(KeyringError::UnknownWallet)
    }
}

impl fmt::Debug for Keyring {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.session {
            Some(session) => write!(
                f,
                "Keyring(unlocked, {} wallets, [redacted])",
                session.contents.entries.len()
            ),
            None => f.write_str("Keyring(locked)"),
        }
    }
}

fn bip39(mnemonic: Mnemonic, passphrase: &str) -> Result<Phrase, KeyringError> {
    payload::check_passphrase(passphrase)?;
    let mut owned = String::with_capacity(passphrase.len());
    owned.push_str(passphrase);
    Ok(Phrase::Bip39 {
        mnemonic,
        passphrase: SecretString::from(owned),
    })
}

fn seed(phrase: &Phrase) -> Result<Seed, KeyringError> {
    match phrase {
        Phrase::Bip39 {
            mnemonic,
            passphrase,
        } => Ok(mnemonic.to_seed(passphrase.expose_secret())),
        Phrase::Ton { mnemonic, .. } => mnemonic.to_seed().map_err(|_| KeyringError::Damaged),
    }
}

fn persist(path: &Path, session: &Session) -> Result<(), KeyringError> {
    let payload = payload::encode(&session.contents)?;
    let file = session.vault.seal(&payload)?;
    bahaa_vault::write_atomic(path, &file)?;
    Ok(())
}

pub(crate) fn private_dir(dir: &Path) -> Result<(), KeyringError> {
    let mut builder = DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(dir)
        .map_err(|err| KeyringError::Vault(VaultError::Io(err.kind())))
}
