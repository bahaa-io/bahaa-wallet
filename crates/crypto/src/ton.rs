use std::fmt;

use hmac::{Hmac, Mac};
use pbkdf2::pbkdf2_hmac;
use secrecy::{ExposeSecret, ExposeSecretMut, SecretBox, SecretString};
use sha2::Sha512;
use thiserror::Error;
use zeroize::Zeroizing;

use crate::Seed;
use crate::mnemonic::{SeedKind, WORDS, seed_of};

pub const TON_WORDS: usize = 24;
const KEY_ROUNDS: u32 = 100_000;
const CHECK_ROUNDS: u32 = KEY_ROUNDS / 256;
const CHECK_SALT: &[u8] = b"TON seed version";
const KEY_SALT: &[u8] = b"TON default seed";
const KEY_LEN: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum TonPhraseError {
    #[error("a TON recovery phrase has 24 words, this one has {0}")]
    WordCount(usize),
    #[error("word {0} is not in the TON word list")]
    UnknownWord(usize),
    #[error("these words are not a TON recovery phrase, check the words and their order")]
    NotTon,
}

pub struct TonMnemonic {
    indices: SecretBox<[u16]>,
}

impl TonMnemonic {
    pub fn parse(phrase: &str) -> Result<Self, TonPhraseError> {
        let given = phrase.split_whitespace().count();
        if given != TON_WORDS {
            return Err(TonPhraseError::WordCount(given));
        }
        let mut indices = SecretBox::new(vec![0u16; TON_WORDS].into_boxed_slice());
        for (position, word) in phrase.split_whitespace().enumerate() {
            let word = Zeroizing::new(word.to_ascii_lowercase());
            let index = WORDS
                .binary_search(&word.as_str())
                .map_err(|_| TonPhraseError::UnknownWord(position + 1))?;
            indices.expose_secret_mut()[position] =
                u16::try_from(index).map_err(|_| TonPhraseError::UnknownWord(position + 1))?;
        }
        Self::from_indices(indices.expose_secret())
    }

    pub fn from_indices(indices: &[u16]) -> Result<Self, TonPhraseError> {
        if indices.len() != TON_WORDS {
            return Err(TonPhraseError::WordCount(indices.len()));
        }
        if let Some(position) = indices
            .iter()
            .position(|index| usize::from(*index) >= WORDS.len())
        {
            return Err(TonPhraseError::UnknownWord(position + 1));
        }
        let mnemonic = Self {
            indices: SecretBox::new(indices.to_vec().into_boxed_slice()),
        };
        let mut check = Zeroizing::new([0u8; 64]);
        pbkdf2_hmac::<Sha512>(&*mnemonic.entropy()?, CHECK_SALT, CHECK_ROUNDS, &mut *check);
        if check[0] != 0 {
            return Err(TonPhraseError::NotTon);
        }
        Ok(mnemonic)
    }

    pub fn expose_indices(&self) -> &[u16] {
        self.indices.expose_secret()
    }

    pub fn phrase(&self) -> SecretString {
        let words: Vec<&str> = self
            .indices
            .expose_secret()
            .iter()
            .map(|index| WORDS[usize::from(*index)])
            .collect();
        SecretString::from(words.join(" "))
    }

    pub fn to_seed(&self) -> Result<Seed, TonPhraseError> {
        let mut derived = Zeroizing::new([0u8; 64]);
        pbkdf2_hmac::<Sha512>(&*self.entropy()?, KEY_SALT, KEY_ROUNDS, &mut *derived);
        Ok(seed_of(SeedKind::Ton, &derived[..KEY_LEN]))
    }

    fn entropy(&self) -> Result<Zeroizing<[u8; 64]>, TonPhraseError> {
        let phrase = self.phrase();
        let mac = Hmac::<Sha512>::new_from_slice(phrase.expose_secret().as_bytes())
            .map_err(|_| TonPhraseError::NotTon)?;
        Ok(Zeroizing::new(mac.finalize().into_bytes().into()))
    }
}

impl fmt::Debug for TonMnemonic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TonMnemonic([redacted])")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Mnemonic, ton_key};

    const PHRASE: &str = "dose ice enrich trigger test dove century still betray gas diet dune use other base gym mad law immense village world example praise game";

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().fold(String::new(), |mut out, byte| {
            use std::fmt::Write as _;
            let _ = write!(out, "{byte:02x}");
            out
        })
    }

    #[test]
    fn the_reference_phrase_gives_the_reference_key() {
        let mnemonic = TonMnemonic::parse(PHRASE).unwrap();
        let seed = mnemonic.to_seed().unwrap();
        assert_eq!(seed.kind(), SeedKind::Ton);
        let key = ton_key(&seed).unwrap();
        assert_eq!(
            hex(&key.to_bytes()),
            "119dcf2840a3d56521d260b2f125eedc0d4f3795b9e627269a4b5a6dca8257bd"
        );
        assert_eq!(
            hex(&key.verifying_key().to_bytes()),
            "c04ad1885c127fe863abb00752fa844e6439bb04f264d70de7cea580b32637ab"
        );
        assert_eq!(mnemonic.phrase().expose_secret(), PHRASE);
        let messy = format!("  {}  ", PHRASE.to_uppercase().replace(' ', "\n "));
        assert_eq!(
            TonMnemonic::parse(&messy).unwrap().expose_indices(),
            mnemonic.expose_indices()
        );
    }

    #[test]
    fn only_real_ton_phrases_are_accepted() {
        let words: Vec<&str> = PHRASE.split(' ').collect();
        let swapped = [&words[1..2], &words[..1], &words[2..]].concat().join(" ");
        assert_eq!(
            TonMnemonic::parse(&swapped).unwrap_err(),
            TonPhraseError::NotTon
        );
        assert_eq!(
            TonMnemonic::parse(&words[..12].join(" ")).unwrap_err(),
            TonPhraseError::WordCount(12)
        );
        let typo = PHRASE.replacen("dose", "doze", 1);
        assert_eq!(
            TonMnemonic::parse(&typo).unwrap_err(),
            TonPhraseError::UnknownWord(1)
        );
        let bip39 = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon art";
        assert!(Mnemonic::parse(bip39).is_ok());
        assert_eq!(
            TonMnemonic::parse(bip39).unwrap_err(),
            TonPhraseError::NotTon
        );
        assert_eq!(
            TonMnemonic::from_indices(&[2048; TON_WORDS]).unwrap_err(),
            TonPhraseError::UnknownWord(1)
        );
        let round = TonMnemonic::from_indices(TonMnemonic::parse(PHRASE).unwrap().expose_indices());
        assert!(round.is_ok());
        assert!(!format!("{:?}", round.unwrap()).contains("dose"));
    }

    #[test]
    fn each_kind_of_seed_only_opens_its_own_keys() {
        let ton = TonMnemonic::parse(PHRASE).unwrap().to_seed().unwrap();
        let path: crate::DerivationPath = "m/44'/607'/0'".parse().unwrap();
        assert_eq!(
            crate::ed25519(&ton, &path).unwrap_err(),
            crate::DeriveError::TonOnly
        );
        let evm_path: crate::DerivationPath = "m/44'/60'/0'/0/0".parse().unwrap();
        assert_eq!(
            crate::secp256k1(&ton, &evm_path).unwrap_err(),
            crate::DeriveError::TonOnly
        );
        let bip39 = Mnemonic::parse("test test test test test test test test test test test junk")
            .unwrap()
            .to_seed("");
        assert_eq!(bip39.kind(), SeedKind::Bip39);
        assert_eq!(ton_key(&bip39).unwrap_err(), crate::DeriveError::NotTon);
        assert!(crate::ed25519(&bip39, &path).is_ok());
    }
}
