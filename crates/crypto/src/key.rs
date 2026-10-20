use bahaa_types::hex;
use k256::SecretKey;
use thiserror::Error;
use zeroize::Zeroizing;

use crate::mnemonic::seed_of;
use crate::{Seed, SeedKind};

pub const KEY_BYTES: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum KeyError {
    #[error("a private key is 64 hex characters")]
    Length,
    #[error("a private key only has the characters 0-9 and a-f")]
    NotHex,
    #[error("this is not a valid private key")]
    Invalid,
}

pub fn private_key(text: &str) -> Result<Zeroizing<[u8; KEY_BYTES]>, KeyError> {
    let text = text.trim();
    let digits = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .unwrap_or(text);
    if digits.len() != 2 * KEY_BYTES {
        return Err(KeyError::Length);
    }
    let mut key = Zeroizing::new([0u8; KEY_BYTES]);
    hex::decode_into(digits, &mut *key).ok_or(KeyError::NotHex)?;
    SecretKey::from_slice(&*key).map_err(|_| KeyError::Invalid)?;
    Ok(key)
}

pub fn key_seed(key: &[u8; KEY_BYTES]) -> Result<Seed, KeyError> {
    SecretKey::from_slice(key).map_err(|_| KeyError::Invalid)?;
    Ok(seed_of(SeedKind::Key, key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DerivationPath, secp256k1, secp256k1_address};

    const HARDHAT_KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

    #[test]
    fn a_private_key_reads_with_or_without_0x_and_stands_for_one_account() {
        let key = private_key(HARDHAT_KEY).unwrap();
        assert_eq!(
            *key,
            *private_key(&HARDHAT_KEY[2..].to_uppercase()).unwrap()
        );
        let seed = key_seed(&key).unwrap();
        assert_eq!(seed.kind(), SeedKind::Key);
        let path: DerivationPath = "m/44'/60'/0'/0/0".parse().unwrap();
        let address = secp256k1_address(&secp256k1(&seed, &path).unwrap().public_key());
        assert_eq!(
            hex::encode(&address),
            "f39fd6e51aad88f6f4ce6ab8827279cfffb92266"
        );
        assert!(crate::ed25519(&seed, &"m/44'/607'/0'".parse().unwrap()).is_err());
    }

    #[test]
    fn bad_private_keys_say_why() {
        assert_eq!(private_key("abc").unwrap_err(), KeyError::Length);
        assert_eq!(private_key(&"g".repeat(64)).unwrap_err(), KeyError::NotHex);
        assert_eq!(private_key(&"0".repeat(64)).unwrap_err(), KeyError::Invalid);
        assert_eq!(private_key(&"f".repeat(64)).unwrap_err(), KeyError::Invalid);
    }
}
