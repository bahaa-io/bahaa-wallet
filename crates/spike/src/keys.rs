use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use alloy_signer_local::coins_bip39::{English, Entropy, Mnemonic};
use alloy_signer_local::{MnemonicBuilder, PrivateKeySigner};
use anyhow::{Context, Result};
use zeroize::{Zeroize, Zeroizing};

const EVM_PATH: &str = "m/44'/60'/0'/0/0";
const TRON_PATH: &str = "m/44'/195'/0'/0";

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
}
