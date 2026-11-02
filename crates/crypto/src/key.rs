use bahaa_types::hex;
use ed25519_dalek::SigningKey;
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
    #[error("a TON private key is 64 or 128 hex characters")]
    TonLength,
    #[error("a private key only has the characters 0-9 and a-f")]
    NotHex,
    #[error("this is not a valid private key")]
    Invalid,
}

pub fn private_key(text: &str) -> Result<Zeroizing<[u8; KEY_BYTES]>, KeyError> {
    let digits = digits(text);
    if digits.len() != 2 * KEY_BYTES {
        return Err(KeyError::Length);
    }
    let key = decode::<KEY_BYTES>(digits)?;
    SecretKey::from_slice(&*key).map_err(|_| KeyError::Invalid)?;
    Ok(key)
}

pub fn key_seed(key: &[u8; KEY_BYTES]) -> Result<Seed, KeyError> {
    SecretKey::from_slice(key).map_err(|_| KeyError::Invalid)?;
    Ok(seed_of(SeedKind::Key, key))
}

pub fn ton_private_key(text: &str) -> Result<Zeroizing<[u8; KEY_BYTES]>, KeyError> {
    let digits = digits(text);
    if digits.len() == 2 * KEY_BYTES {
        return decode::<KEY_BYTES>(digits);
    }
    if digits.len() != 4 * KEY_BYTES {
        return Err(KeyError::TonLength);
    }
    let pair = decode::<{ 2 * KEY_BYTES }>(digits)?;
    let mut key = Zeroizing::new([0u8; KEY_BYTES]);
    key.copy_from_slice(&pair[..KEY_BYTES]);
    if SigningKey::from_bytes(&key).verifying_key().as_bytes()[..] != pair[KEY_BYTES..] {
        return Err(KeyError::Invalid);
    }
    Ok(key)
}

pub fn ton_key_seed(key: &[u8; KEY_BYTES]) -> Seed {
    seed_of(SeedKind::Ton, key)
}

fn digits(text: &str) -> &str {
    let text = text.trim();
    text.strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .unwrap_or(text)
}

fn decode<const N: usize>(digits: &str) -> Result<Zeroizing<[u8; N]>, KeyError> {
    let mut bytes = Zeroizing::new([0u8; N]);
    hex::decode_into(digits, &mut *bytes).ok_or(KeyError::NotHex)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DerivationPath, secp256k1, secp256k1_address, ton_key};

    const HARDHAT_KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
    const RFC8032_KEY: &str = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";
    const RFC8032_PUBLIC: &str = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";

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

    #[test]
    fn a_ton_key_reads_alone_or_followed_by_its_public_key() {
        let key = ton_private_key(RFC8032_KEY).unwrap();
        let pair = format!("0x{RFC8032_KEY}{}", RFC8032_PUBLIC.to_uppercase());
        assert_eq!(*key, *ton_private_key(&pair).unwrap());
        let seed = ton_key_seed(&key);
        assert_eq!(seed.kind(), SeedKind::Ton);
        let public = ton_key(&seed).unwrap().verifying_key().to_bytes();
        assert_eq!(hex::encode(&public), RFC8032_PUBLIC);
        assert!(secp256k1(&seed, &"m/44'/60'/0'/0/0".parse().unwrap()).is_err());
    }

    #[test]
    fn bad_ton_keys_say_why() {
        assert_eq!(ton_private_key("abc").unwrap_err(), KeyError::TonLength);
        assert_eq!(
            ton_private_key(&"g".repeat(128)).unwrap_err(),
            KeyError::NotHex
        );
        let other = format!("{RFC8032_KEY}{}", "1".repeat(64));
        assert_eq!(ton_private_key(&other).unwrap_err(), KeyError::Invalid);
    }
}
