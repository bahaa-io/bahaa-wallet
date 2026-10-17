use bahaa_crypto::{ExposeSecret, Mnemonic, SecretString};
use zeroize::Zeroizing;

use crate::KeyringError;

const VERSION: u8 = 1;
const MAX_WALLETS: usize = 64;
const MAX_PASSPHRASE: usize = 1024;

pub struct Entry {
    pub id: u32,
    pub mnemonic: Mnemonic,
    pub passphrase: SecretString,
}

pub struct Contents {
    pub next_id: u32,
    pub entries: Vec<Entry>,
}

pub fn encode(contents: &Contents) -> Result<Zeroizing<Vec<u8>>, KeyringError> {
    if contents.entries.len() > MAX_WALLETS {
        return Err(KeyringError::TooManyWallets);
    }
    let size = 1
        + 4
        + 2
        + contents
            .entries
            .iter()
            .map(|entry| {
                4 + 1
                    + entry.mnemonic.expose_entropy().len()
                    + 2
                    + entry.passphrase.expose_secret().len()
            })
            .sum::<usize>();
    let mut out = Zeroizing::new(Vec::with_capacity(size));
    out.push(VERSION);
    out.extend_from_slice(&contents.next_id.to_be_bytes());
    let count = u16::try_from(contents.entries.len()).map_err(|_| KeyringError::TooManyWallets)?;
    out.extend_from_slice(&count.to_be_bytes());
    for entry in &contents.entries {
        let entropy = entry.mnemonic.expose_entropy();
        let passphrase = entry.passphrase.expose_secret().as_bytes();
        out.extend_from_slice(&entry.id.to_be_bytes());
        out.push(u8::try_from(entropy.len()).map_err(|_| KeyringError::Damaged)?);
        out.extend_from_slice(entropy);
        let length =
            u16::try_from(passphrase.len()).map_err(|_| KeyringError::PassphraseTooLong)?;
        out.extend_from_slice(&length.to_be_bytes());
        out.extend_from_slice(passphrase);
    }
    Ok(out)
}

pub fn decode(bytes: &[u8]) -> Result<Contents, KeyringError> {
    let mut reader = Reader(bytes);
    if reader.take(1)? != [VERSION] {
        return Err(KeyringError::Damaged);
    }
    let next_id = u32::from_be_bytes(reader.array()?);
    let count = usize::from(u16::from_be_bytes(reader.array()?));
    if count > MAX_WALLETS {
        return Err(KeyringError::Damaged);
    }
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        let id = u32::from_be_bytes(reader.array()?);
        let entropy_len = usize::from(reader.take(1)?[0]);
        let mnemonic =
            Mnemonic::from_entropy(reader.take(entropy_len)?).map_err(|_| KeyringError::Damaged)?;
        let passphrase_len = usize::from(u16::from_be_bytes(reader.array()?));
        if passphrase_len > MAX_PASSPHRASE {
            return Err(KeyringError::Damaged);
        }
        let raw = reader.take(passphrase_len)?;
        let text = std::str::from_utf8(raw).map_err(|_| KeyringError::Damaged)?;
        let mut owned = String::with_capacity(text.len());
        owned.push_str(text);
        if id >= next_id || entries.iter().any(|entry: &Entry| entry.id == id) {
            return Err(KeyringError::Damaged);
        }
        entries.push(Entry {
            id,
            mnemonic,
            passphrase: SecretString::from(owned),
        });
    }
    if !reader.0.is_empty() {
        return Err(KeyringError::Damaged);
    }
    Ok(Contents { next_id, entries })
}

pub fn check_passphrase(passphrase: &str) -> Result<(), KeyringError> {
    if passphrase.len() > MAX_PASSPHRASE {
        return Err(KeyringError::PassphraseTooLong);
    }
    Ok(())
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], KeyringError> {
        let (head, rest) = self.0.split_at_checked(len).ok_or(KeyringError::Damaged)?;
        self.0 = rest;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], KeyringError> {
        self.take(N)?.try_into().map_err(|_| KeyringError::Damaged)
    }
}
