use std::fmt;
use std::sync::LazyLock;

use pbkdf2::pbkdf2_hmac;
use secrecy::{ExposeSecret, ExposeSecretMut, SecretBox, SecretString};
use sha2::{Digest, Sha256, Sha512};
use thiserror::Error;
use unicode_normalization::UnicodeNormalization;
use zeroize::Zeroizing;

const SALT_PREFIX: &str = "mnemonic";
const SEED_ROUNDS: u32 = 2048;
const SEED_LEN: usize = 64;
const BITS_PER_WORD: usize = 11;

static WORDS: LazyLock<Vec<&'static str>> =
    LazyLock::new(|| include_str!("english.txt").lines().collect());

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WordCount {
    Twelve,
    Fifteen,
    Eighteen,
    TwentyOne,
    TwentyFour,
}

impl WordCount {
    pub fn words(self) -> usize {
        match self {
            Self::Twelve => 12,
            Self::Fifteen => 15,
            Self::Eighteen => 18,
            Self::TwentyOne => 21,
            Self::TwentyFour => 24,
        }
    }

    fn entropy_len(self) -> usize {
        self.words() * 4 / 3
    }

    fn from_words(words: usize) -> Option<Self> {
        [
            Self::Twelve,
            Self::Fifteen,
            Self::Eighteen,
            Self::TwentyOne,
            Self::TwentyFour,
        ]
        .into_iter()
        .find(|count| count.words() == words)
    }

    fn from_entropy_len(len: usize) -> Option<Self> {
        Self::from_words(len * 3 / 4).filter(|count| count.entropy_len() == len)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum MnemonicError {
    #[error("a recovery phrase has 12, 15, 18, 21 or 24 words, this one has {0}")]
    WordCount(usize),
    #[error("word {0} is not in the BIP39 English word list")]
    UnknownWord(usize),
    #[error("the phrase checksum does not match, check the words and their order")]
    Checksum,
    #[error("entropy must be 16, 20, 24, 28 or 32 bytes")]
    EntropyLength,
    #[error("the system random number generator failed")]
    Randomness,
}

pub struct Mnemonic {
    entropy: SecretBox<[u8]>,
}

pub struct Seed(SecretBox<[u8]>);

impl Mnemonic {
    pub fn generate(count: WordCount) -> Result<Self, MnemonicError> {
        let mut entropy = secret_bytes(count.entropy_len());
        getrandom::fill(entropy.expose_secret_mut()).map_err(|_| MnemonicError::Randomness)?;
        Ok(Self { entropy })
    }

    pub fn from_entropy(entropy: &[u8]) -> Result<Self, MnemonicError> {
        WordCount::from_entropy_len(entropy.len()).ok_or(MnemonicError::EntropyLength)?;
        let mut secret = secret_bytes(entropy.len());
        secret.expose_secret_mut().copy_from_slice(entropy);
        Ok(Self { entropy: secret })
    }

    pub fn parse(phrase: &str) -> Result<Self, MnemonicError> {
        let given = phrase.split_whitespace().count();
        let count = WordCount::from_words(given).ok_or(MnemonicError::WordCount(given))?;
        let mut indices = Zeroizing::new(Vec::with_capacity(count.words()));
        for (position, word) in phrase.split_whitespace().enumerate() {
            let word = Zeroizing::new(word.to_ascii_lowercase());
            let index = WORDS
                .binary_search(&word.as_str())
                .map_err(|_| MnemonicError::UnknownWord(position + 1))?;
            indices
                .push(u16::try_from(index).map_err(|_| MnemonicError::UnknownWord(position + 1))?);
        }
        let bits = pack(&indices);
        let (entropy, rest) = bits.split_at(count.entropy_len());
        let checksum_bits = count.entropy_len() / 4;
        let expected = Sha256::digest(entropy)[0] >> (8 - checksum_bits);
        if rest.first().map(|byte| byte >> (8 - checksum_bits)) != Some(expected) {
            return Err(MnemonicError::Checksum);
        }
        Self::from_entropy(entropy)
    }

    pub fn word_count(&self) -> WordCount {
        WordCount::from_entropy_len(self.entropy.expose_secret().len())
            .unwrap_or(WordCount::TwentyFour)
    }

    pub fn phrase(&self) -> SecretString {
        let indices = unpack(self.entropy.expose_secret());
        let word = |index: &u16| WORDS[usize::from(*index)];
        let len = indices
            .iter()
            .map(|index| word(index).len() + 1)
            .sum::<usize>()
            - 1;
        let mut phrase = String::with_capacity(len);
        for index in indices.iter() {
            if !phrase.is_empty() {
                phrase.push(' ');
            }
            phrase.push_str(word(index));
        }
        SecretString::from(phrase)
    }

    pub fn to_seed(&self, passphrase: &str) -> Seed {
        let phrase = self.phrase();
        let normalized = passphrase.nfkd().map(char::len_utf8).sum::<usize>();
        let mut salt = Zeroizing::new(String::with_capacity(SALT_PREFIX.len() + normalized));
        salt.push_str(SALT_PREFIX);
        salt.extend(passphrase.nfkd());
        let mut seed = secret_bytes(SEED_LEN);
        pbkdf2_hmac::<Sha512>(
            phrase.expose_secret().as_bytes(),
            salt.as_bytes(),
            SEED_ROUNDS,
            seed.expose_secret_mut(),
        );
        Seed(seed)
    }
}

impl Seed {
    pub fn expose(&self) -> &[u8] {
        self.0.expose_secret()
    }
}

impl fmt::Debug for Mnemonic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Mnemonic([redacted], {} words)",
            self.word_count().words()
        )
    }
}

impl fmt::Debug for Seed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Seed([redacted])")
    }
}

fn secret_bytes(len: usize) -> SecretBox<[u8]> {
    SecretBox::new(vec![0u8; len].into_boxed_slice())
}

fn pack(indices: &[u16]) -> Zeroizing<Vec<u8>> {
    let total = indices.len() * BITS_PER_WORD;
    let mut bytes = Zeroizing::new(vec![0u8; total.div_ceil(8)]);
    for (word, index) in indices.iter().enumerate() {
        for offset in 0..BITS_PER_WORD {
            if (index >> (BITS_PER_WORD - 1 - offset)) & 1 == 1 {
                let position = word * BITS_PER_WORD + offset;
                bytes[position / 8] |= 0x80 >> (position % 8);
            }
        }
    }
    bytes
}

fn unpack(entropy: &[u8]) -> Zeroizing<Vec<u16>> {
    let checksum = Sha256::digest(entropy)[0];
    let total = entropy.len() * 8 + entropy.len() / 4;
    let bit = |position: usize| {
        let byte = entropy.get(position / 8).copied().unwrap_or(checksum);
        u16::from((byte >> (7 - position % 8)) & 1)
    };
    Zeroizing::new(
        (0..total / BITS_PER_WORD)
            .map(|word| {
                (0..BITS_PER_WORD).fold(0u16, |index, offset| {
                    (index << 1) | bit(word * BITS_PER_WORD + offset)
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_list_is_the_official_one() {
        assert_eq!(WORDS.len(), 2048);
        assert!(WORDS.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!((WORDS[0], WORDS[2047]), ("abandon", "zoo"));
    }

    #[test]
    fn generated_phrases_round_trip() {
        for count in [WordCount::Twelve, WordCount::TwentyFour] {
            let mnemonic = Mnemonic::generate(count).unwrap();
            assert_eq!(mnemonic.word_count(), count);
            let phrase = mnemonic.phrase();
            assert_eq!(phrase.expose_secret().split(' ').count(), count.words());
            let again = Mnemonic::parse(phrase.expose_secret()).unwrap();
            assert_eq!(
                again.entropy.expose_secret(),
                mnemonic.entropy.expose_secret()
            );
        }
    }

    #[test]
    fn generated_phrases_differ() {
        let a = Mnemonic::generate(WordCount::Twelve).unwrap();
        let b = Mnemonic::generate(WordCount::Twelve).unwrap();
        assert_ne!(a.entropy.expose_secret(), b.entropy.expose_secret());
    }

    #[test]
    fn accepts_messy_but_correct_input() {
        let tidy = Mnemonic::parse("abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about").unwrap();
        let messy = Mnemonic::parse("  ABANDON abandon\tabandon\nabandon abandon  abandon abandon abandon abandon abandon abandon About ").unwrap();
        assert_eq!(tidy.entropy.expose_secret(), messy.entropy.expose_secret());
    }

    #[test]
    fn explains_bad_phrases_without_echoing_them() {
        let eleven = "abandon ".repeat(11);
        assert_eq!(
            Mnemonic::parse(&eleven).unwrap_err(),
            MnemonicError::WordCount(11)
        );
        let unknown = "abandon abandon abandon secretword abandon abandon abandon abandon abandon abandon abandon about";
        let err = Mnemonic::parse(unknown).unwrap_err();
        assert_eq!(err, MnemonicError::UnknownWord(4));
        assert!(!err.to_string().contains("secretword"));
        let lookalike = "abandon abandon abandon abаndon abandon abandon abandon abandon abandon abandon abandon about";
        assert_eq!(
            Mnemonic::parse(lookalike).unwrap_err(),
            MnemonicError::UnknownWord(4)
        );
        let swapped = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about abandon";
        assert_eq!(
            Mnemonic::parse(swapped).unwrap_err(),
            MnemonicError::Checksum
        );
    }

    #[test]
    fn rejects_bad_entropy_lengths() {
        for len in [0, 15, 17, 33, 64] {
            assert_eq!(
                Mnemonic::from_entropy(&vec![0; len]).unwrap_err(),
                MnemonicError::EntropyLength
            );
        }
    }

    #[test]
    fn debug_output_never_shows_secrets() {
        let mnemonic = Mnemonic::parse(
            "legal winner thank year wave sausage worth useful legal winner thank yellow",
        )
        .unwrap();
        let seed = mnemonic.to_seed("");
        let shown = format!("{mnemonic:?} {seed:?} {:?}", mnemonic.phrase());
        assert!(!shown.contains("legal"));
        assert!(!shown.contains("winner"));
        assert_eq!(
            shown.matches("redacted").count() + shown.matches("REDACTED").count(),
            3
        );
    }

    #[test]
    fn passphrases_are_normalized_before_use() {
        let mnemonic = Mnemonic::from_entropy(&[7; 16]).unwrap();
        let composed = mnemonic.to_seed("caf\u{e9}");
        let decomposed = mnemonic.to_seed("cafe\u{301}");
        assert_eq!(composed.expose(), decomposed.expose());
        assert_ne!(composed.expose(), mnemonic.to_seed("cafe").expose());
    }
}
