use bahaa_types::hex;
use ed25519_dalek::SigningKey;
use k256::SecretKey;
use thiserror::Error;
use zeroize::Zeroizing;

use crate::mnemonic::seed_of;
use crate::{Seed, SeedKind};

pub const KEY_BYTES: usize = 32;
const SOLANA_MAX_CHARS: usize = 88;
const WIF_MAX_CHARS: usize = 52;
const WIF_MAINNET: u8 = 0x80;
const WIF_TESTNET: u8 = 0xef;
const WIF_COMPRESSED: u8 = 0x01;
const WIF_BYTES: usize = KEY_BYTES + 2;
const WIF_UNCOMPRESSED_BYTES: usize = KEY_BYTES + 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum KeyError {
    #[error("a private key is 64 hex characters")]
    Length,
    #[error("a TON private key is 64 or 128 hex characters")]
    TonLength,
    #[error(
        "a Solana private key is the base58 text wallets export, or a key file's list of 64 numbers"
    )]
    SolanaFormat,
    #[error("a Bitcoin private key is the WIF text wallets export, starting with K, L or c")]
    BitcoinFormat,
    #[error("this Bitcoin key is in the old uncompressed form, which this wallet does not use")]
    Uncompressed,
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

pub fn secp256k1_seed(key: &[u8; KEY_BYTES]) -> Result<Seed, KeyError> {
    SecretKey::from_slice(key).map_err(|_| KeyError::Invalid)?;
    Ok(seed_of(SeedKind::Secp256k1, key))
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

pub fn ed25519_seed(key: &[u8; KEY_BYTES]) -> Seed {
    seed_of(SeedKind::Ed25519, key)
}

pub fn solana_private_key(text: &str) -> Result<Zeroizing<[u8; KEY_BYTES]>, KeyError> {
    let text = text.trim();
    let mut pair = Zeroizing::new([0u8; 2 * KEY_BYTES]);
    let read = match text
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
    {
        Some(list) => {
            let mut parts = list.split(',');
            for slot in pair.iter_mut() {
                *slot = parts
                    .next()
                    .and_then(|part| part.trim().parse().ok())
                    .ok_or(KeyError::SolanaFormat)?;
            }
            parts.next().is_none()
        }
        None => {
            text.len() <= SOLANA_MAX_CHARS
                && bs58::decode(text).onto(&mut pair[..]).ok() == Some(pair.len())
        }
    };
    if !read {
        return Err(KeyError::SolanaFormat);
    }
    let mut key = Zeroizing::new([0u8; KEY_BYTES]);
    key.copy_from_slice(&pair[..KEY_BYTES]);
    if SigningKey::from_bytes(&key).verifying_key().as_bytes()[..] != pair[KEY_BYTES..] {
        return Err(KeyError::Invalid);
    }
    Ok(key)
}

pub fn bitcoin_private_key(text: &str) -> Result<Zeroizing<[u8; KEY_BYTES]>, KeyError> {
    let text = text.trim();
    if text.len() > WIF_MAX_CHARS {
        return Err(KeyError::BitcoinFormat);
    }
    let mut raw = Zeroizing::new([0u8; WIF_BYTES + 4]);
    let read = bs58::decode(text)
        .with_check(None)
        .onto(&mut raw[..])
        .map_err(|_| KeyError::BitcoinFormat)?;
    if !matches!(raw[0], WIF_MAINNET | WIF_TESTNET) {
        return Err(KeyError::BitcoinFormat);
    }
    match read {
        WIF_BYTES if raw[KEY_BYTES + 1] == WIF_COMPRESSED => {}
        WIF_UNCOMPRESSED_BYTES => return Err(KeyError::Uncompressed),
        _ => return Err(KeyError::BitcoinFormat),
    }
    let mut key = Zeroizing::new([0u8; KEY_BYTES]);
    key.copy_from_slice(&raw[1..=KEY_BYTES]);
    SecretKey::from_slice(&*key).map_err(|_| KeyError::Invalid)?;
    Ok(key)
}

pub fn bitcoin_wif(key: &[u8; KEY_BYTES]) -> Zeroizing<String> {
    let mut raw = Zeroizing::new([0u8; WIF_BYTES]);
    raw[0] = WIF_MAINNET;
    raw[1..=KEY_BYTES].copy_from_slice(key);
    raw[KEY_BYTES + 1] = WIF_COMPRESSED;
    Zeroizing::new(bs58::encode(&raw[..]).with_check().into_string())
}

pub fn solana_keypair(key: &[u8; KEY_BYTES]) -> Zeroizing<String> {
    let mut pair = Zeroizing::new([0u8; 2 * KEY_BYTES]);
    pair[..KEY_BYTES].copy_from_slice(key);
    pair[KEY_BYTES..].copy_from_slice(SigningKey::from_bytes(key).verifying_key().as_bytes());
    Zeroizing::new(bs58::encode(&pair[..]).into_string())
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
    use crate::{DerivationPath, ed25519, secp256k1, secp256k1_address};

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
        let seed = secp256k1_seed(&key).unwrap();
        assert_eq!(seed.kind(), SeedKind::Secp256k1);
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
        let seed = ed25519_seed(&key);
        assert_eq!(seed.kind(), SeedKind::Ed25519);
        let public = ed25519(&seed, &"m/44'/607'/0'".parse().unwrap())
            .unwrap()
            .verifying_key()
            .to_bytes();
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

    #[test]
    fn a_solana_key_reads_as_wallets_export_it_and_as_a_key_file() {
        let secret: [u8; KEY_BYTES] = hex::decode_array(RFC8032_KEY).unwrap();
        let exported = solana_keypair(&secret);
        let mut pair = hex::decode(RFC8032_KEY).unwrap();
        pair.extend(hex::decode(RFC8032_PUBLIC).unwrap());
        assert_eq!(*exported, bs58::encode(&pair).into_string());
        assert_eq!(
            *solana_private_key(&format!(" {} ", *exported)).unwrap(),
            secret
        );
        let numbers: Vec<String> = pair.iter().map(ToString::to_string).collect();
        let file = format!("[{}]", numbers.join(", "));
        assert_eq!(*solana_private_key(&file).unwrap(), secret);
        let seed = ed25519_seed(&secret);
        let public = ed25519(&seed, &"m/44'/501'/0'/0'".parse().unwrap())
            .unwrap()
            .verifying_key()
            .to_bytes();
        assert_eq!(hex::encode(&public), RFC8032_PUBLIC);
    }

    #[test]
    fn bad_solana_keys_say_why() {
        let mut pair = hex::decode(RFC8032_KEY).unwrap();
        pair.extend(hex::decode(RFC8032_PUBLIC).unwrap());
        let numbers: Vec<String> = pair.iter().map(ToString::to_string).collect();
        let short = format!("[{}]", numbers[..63].join(","));
        let long = format!("[{},1]", numbers.join(","));
        let wide = format!("[{},256]", numbers[..63].join(","));
        let half = bs58::encode(&pair[..KEY_BYTES]).into_string();
        for bad in [
            "",
            "[]",
            short.as_str(),
            long.as_str(),
            wide.as_str(),
            half.as_str(),
            RFC8032_KEY,
            &"z".repeat(500),
        ] {
            assert_eq!(
                solana_private_key(bad).unwrap_err(),
                KeyError::SolanaFormat,
                "{bad}"
            );
        }
        pair[KEY_BYTES] ^= 1;
        assert_eq!(
            solana_private_key(&bs58::encode(&pair).into_string()).unwrap_err(),
            KeyError::Invalid
        );
    }

    const KEY: &str = "0c28fca386c7a227600b2fe50b7cae11ec86d3bf1fbe471be89827e19d72aa1d";

    #[test]
    fn wif_keys_round_trip_the_bitcoin_wiki_example() {
        let key: [u8; KEY_BYTES] = hex::decode_array(KEY).unwrap();
        let wif = bitcoin_wif(&key);
        assert_eq!(
            wif.as_str(),
            "KwdMAjGmerYanjeui5SHS7JkmpZvVipYvB2LJGU1ZxJwYvP98617"
        );
        assert_eq!(*bitcoin_private_key(&wif).unwrap(), key);
        assert_eq!(
            *bitcoin_private_key(" cMzLdeGd5vEqxB8B6VFQoRopQ3sLAAvEzDAoQgvX54xwofSWj1fx\n")
                .unwrap(),
            key
        );
    }

    #[test]
    fn other_wif_forms_are_refused_with_a_reason() {
        assert_eq!(
            bitcoin_private_key("5HueCGU8rMjxEXxiPuD5BDku4MkFqeZyd4dZ1jvhTVqvbTLvyTJ"),
            Err(KeyError::Uncompressed)
        );
        for text in [
            "",
            "KwdMAjGmerYanjeui5SHS7JkmpZvVipYvB2LJGU1ZxJwYvP98618",
            "1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa",
            KEY,
            &"K".repeat(60),
        ] {
            assert_eq!(
                bitcoin_private_key(text),
                Err(KeyError::BitcoinFormat),
                "{text}"
            );
        }
    }
}
