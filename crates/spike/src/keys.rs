use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use alloy_signer_local::coins_bip39::{English, Entropy, Mnemonic};
use alloy_signer_local::{MnemonicBuilder, PrivateKeySigner};
use anyhow::{Context, Result};
use ed25519_dalek::SigningKey;
use hmac::{Hmac, Mac};
use sha2::Sha512;
use zeroize::{Zeroize, Zeroizing};

const EVM_PATH: &str = "m/44'/60'/0'/0/0";
const TRON_PATH: &str = "m/44'/195'/0'/0";
const TON_PATH: [u32; 3] = [44, 607, 0];
const HARDENED: u32 = 0x8000_0000;

pub fn location() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home)
        .join(".bahaa-spike")
        .join("testnet-mnemonic"))
}

pub fn create() -> Result<PathBuf> {
    let path = location()?;
    let dir = path.parent().context("mnemonic path has no parent")?;
    private_dir(dir)?;
    let mut entropy = [0u8; 16];
    getrandom::fill(&mut entropy)?;
    let mnemonic = Mnemonic::<English>::new_from_entropy(Entropy::from_slice(entropy)?);
    entropy.zeroize();
    let phrase = Zeroizing::new(mnemonic.to_phrase());
    let mut file = private_file(&path)?;
    file.write_all(phrase.as_bytes())?;
    file.write_all(b"\n")?;
    Ok(path)
}

pub fn load() -> Result<Zeroizing<String>> {
    let path = location()?;
    let text = Zeroizing::new(fs::read_to_string(&path).with_context(|| {
        format!(
            "no testnet mnemonic at {}, run `bahaa-spike new` first",
            path.display()
        )
    })?);
    Ok(Zeroizing::new(text.trim().to_owned()))
}

pub fn evm(phrase: &str) -> Result<PrivateKeySigner> {
    Ok(MnemonicBuilder::<English>::default()
        .phrase(phrase)
        .derivation_path(EVM_PATH)?
        .build()?)
}

pub fn tron(phrase: &str, index: u32) -> Result<PrivateKeySigner> {
    Ok(MnemonicBuilder::<English>::default()
        .phrase(phrase)
        .derivation_path(format!("{TRON_PATH}/{index}"))?
        .build()?)
}

pub fn ton(phrase: &str) -> Result<SigningKey> {
    let seed = Zeroizing::new(Mnemonic::<English>::new_from_phrase(phrase)?.to_seed(None)?);
    let secret = slip10(&seed[..], &TON_PATH)?;
    Ok(SigningKey::from_bytes(&secret))
}

fn slip10(seed: &[u8], path: &[u32]) -> Result<Zeroizing<[u8; 32]>> {
    let mut node = hmac512(b"ed25519 seed", &[seed])?;
    for index in path {
        let hardened = (index | HARDENED).to_be_bytes();
        node = hmac512(&node[32..], &[&[0], &node[..32], &hardened])?;
    }
    let mut secret = Zeroizing::new([0u8; 32]);
    secret.copy_from_slice(&node[..32]);
    Ok(secret)
}

fn hmac512(key: &[u8], parts: &[&[u8]]) -> Result<Zeroizing<[u8; 64]>> {
    let mut mac = Hmac::<Sha512>::new_from_slice(key)?;
    for part in parts {
        mac.update(part);
    }
    let mut out = Zeroizing::new([0u8; 64]);
    out.copy_from_slice(&mac.finalize().into_bytes());
    Ok(out)
}

#[cfg(unix)]
fn private_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    Ok(())
}

#[cfg(not(unix))]
fn private_dir(dir: &Path) -> Result<()> {
    DirBuilder::new().recursive(true).create(dir)?;
    Ok(())
}

#[cfg(unix)]
fn private_file(path: &Path) -> Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("{} already exists or cannot be created", path.display()))
}

#[cfg(not(unix))]
fn private_file(path: &Path) -> Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("{} already exists or cannot be created", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_the_known_first_account() {
        let signer = evm("test test test test test test test test test test test junk").unwrap();
        assert_eq!(
            signer.address().to_string(),
            "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"
        );
    }

    #[test]
    fn slip10_matches_the_published_ed25519_vectors() {
        let seed = alloy_primitives::hex::decode("000102030405060708090a0b0c0d0e0f").unwrap();
        let master = slip10(&seed, &[]).unwrap();
        assert_eq!(
            alloy_primitives::hex::encode(master.as_slice()),
            "2b4be7f19ee27bbf30c667b642d5f4aa69fd169872f8fc3059c08ebae2eb19e7"
        );
        assert_eq!(
            alloy_primitives::hex::encode(
                SigningKey::from_bytes(&master).verifying_key().to_bytes()
            ),
            "a4b2856bfec510abab89753fac1ac0e1112364e7d250545963f135f2a33188ed"
        );
        let deep = slip10(&seed, &[0, 1, 2, 2, 1_000_000_000]).unwrap();
        assert_eq!(
            alloy_primitives::hex::encode(deep.as_slice()),
            "8f94d394a8e8fd6b1bc2f3f49f5c47e385281d5c17e65324b0f62483e37e8793"
        );
        assert_eq!(
            alloy_primitives::hex::encode(SigningKey::from_bytes(&deep).verifying_key().to_bytes()),
            "3c24da049451555d51a7014a37337aa4e12d41e485abccfa46b47dfb2af54b7a"
        );
    }
}
