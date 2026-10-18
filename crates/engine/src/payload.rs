use bahaa_crypto::{ExposeSecret, Mnemonic, SecretString, TON_WORDS, TonMnemonic};
use bahaa_types::TonContract;
use zeroize::Zeroizing;

use crate::KeyringError;

const VERSION: u8 = 2;
const FIRST_VERSION: u8 = 1;
const MAX_WALLETS: usize = 64;
const MAX_PASSPHRASE: usize = 1024;
const BIP39: u8 = 0;
const TON: u8 = 1;
const W5: u8 = 0;
const V4R2: u8 = 1;

pub enum Phrase {
    Bip39 {
        mnemonic: Mnemonic,
        passphrase: SecretString,
    },
    Ton {
        mnemonic: TonMnemonic,
        contract: TonContract,
    },
}

pub struct Entry {
    pub id: u32,
    pub phrase: Phrase,
}

pub struct Contents {
    pub next_id: u32,
    pub entries: Vec<Entry>,
}

pub fn encode(contents: &Contents) -> Result<Zeroizing<Vec<u8>>, KeyringError> {
    if contents.entries.len() > MAX_WALLETS {
        return Err(KeyringError::TooManyWallets);
    }
    let mut out = Zeroizing::new(Vec::new());
    out.push(VERSION);
    out.extend_from_slice(&contents.next_id.to_be_bytes());
    let count = u16::try_from(contents.entries.len()).map_err(|_| KeyringError::TooManyWallets)?;
    out.extend_from_slice(&count.to_be_bytes());
    for entry in &contents.entries {
        out.extend_from_slice(&entry.id.to_be_bytes());
        match &entry.phrase {
            Phrase::Bip39 {
                mnemonic,
                passphrase,
            } => {
                out.push(BIP39);
                bip39(&mut out, mnemonic, passphrase)?;
            }
            Phrase::Ton { mnemonic, contract } => {
                out.push(TON);
                for index in mnemonic.expose_indices() {
                    out.extend_from_slice(&index.to_be_bytes());
                }
                out.push(match contract {
                    TonContract::W5 => W5,
                    TonContract::V4R2 => V4R2,
                });
            }
        }
    }
    Ok(out)
}

fn bip39(
    out: &mut Vec<u8>,
    mnemonic: &Mnemonic,
    passphrase: &SecretString,
) -> Result<(), KeyringError> {
    let entropy = mnemonic.expose_entropy();
    let passphrase = passphrase.expose_secret().as_bytes();
    out.push(u8::try_from(entropy.len()).map_err(|_| KeyringError::Damaged)?);
    out.extend_from_slice(entropy);
    let length = u16::try_from(passphrase.len()).map_err(|_| KeyringError::PassphraseTooLong)?;
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(passphrase);
    Ok(())
}

pub fn decode(bytes: &[u8]) -> Result<Contents, KeyringError> {
    let mut reader = Reader(bytes);
    let version = reader.take(1)?[0];
    if version > VERSION {
        return Err(KeyringError::Newer);
    }
    if version < FIRST_VERSION {
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
        let kind = if version == FIRST_VERSION {
            BIP39
        } else {
            reader.take(1)?[0]
        };
        let phrase = match kind {
            BIP39 => read_bip39(&mut reader)?,
            TON => read_ton(&mut reader)?,
            _ => return Err(KeyringError::Damaged),
        };
        if id >= next_id || entries.iter().any(|entry: &Entry| entry.id == id) {
            return Err(KeyringError::Damaged);
        }
        entries.push(Entry { id, phrase });
    }
    if !reader.0.is_empty() {
        return Err(KeyringError::Damaged);
    }
    Ok(Contents { next_id, entries })
}

fn read_bip39(reader: &mut Reader<'_>) -> Result<Phrase, KeyringError> {
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
    Ok(Phrase::Bip39 {
        mnemonic,
        passphrase: SecretString::from(owned),
    })
}

fn read_ton(reader: &mut Reader<'_>) -> Result<Phrase, KeyringError> {
    let mut indices = Zeroizing::new([0u16; TON_WORDS]);
    for index in indices.iter_mut() {
        *index = u16::from_be_bytes(reader.array()?);
    }
    let mnemonic = TonMnemonic::from_indices(&*indices).map_err(|_| KeyringError::Damaged)?;
    let contract = match reader.take(1)?[0] {
        W5 => TonContract::W5,
        V4R2 => TonContract::V4R2,
        _ => return Err(KeyringError::Damaged),
    };
    Ok(Phrase::Ton { mnemonic, contract })
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

#[cfg(test)]
mod tests {
    use super::*;

    const TON: &str = "dose ice enrich trigger test dove century still betray gas diet dune use other base gym mad law immense village world example praise game";

    fn ton(contract: TonContract) -> Entry {
        Entry {
            id: 2,
            phrase: Phrase::Ton {
                mnemonic: TonMnemonic::parse(TON).unwrap(),
                contract,
            },
        }
    }

    fn multi() -> Entry {
        Entry {
            id: 1,
            phrase: Phrase::Bip39 {
                mnemonic: Mnemonic::from_entropy(&[7; 16]).unwrap(),
                passphrase: SecretString::from("extra".to_owned()),
            },
        }
    }

    #[test]
    fn a_first_version_vault_still_opens() {
        let mut old = vec![FIRST_VERSION];
        old.extend_from_slice(&5u32.to_be_bytes());
        old.extend_from_slice(&1u16.to_be_bytes());
        old.extend_from_slice(&3u32.to_be_bytes());
        old.push(16);
        old.extend_from_slice(&[7; 16]);
        old.extend_from_slice(&2u16.to_be_bytes());
        old.extend_from_slice(b"pw");
        let contents = decode(&old).unwrap();
        assert_eq!((contents.next_id, contents.entries.len()), (5, 1));
        let Phrase::Bip39 {
            mnemonic,
            passphrase,
        } = &contents.entries[0].phrase
        else {
            panic!("an old vault holds multi-chain phrases");
        };
        assert_eq!(mnemonic.expose_entropy(), [7; 16]);
        assert_eq!(passphrase.expose_secret(), "pw");
    }

    #[test]
    fn both_kinds_of_phrase_round_trip() {
        for contract in [TonContract::W5, TonContract::V4R2] {
            let contents = Contents {
                next_id: 3,
                entries: vec![multi(), ton(contract)],
            };
            let decoded = decode(&encode(&contents).unwrap()).unwrap();
            assert_eq!(decoded.next_id, 3);
            let Phrase::Ton {
                mnemonic,
                contract: read,
            } = &decoded.entries[1].phrase
            else {
                panic!("the TON phrase came back as another kind");
            };
            assert_eq!(*read, contract);
            assert_eq!(mnemonic.phrase().expose_secret(), TON);
            assert!(matches!(
                &decoded.entries[0].phrase,
                Phrase::Bip39 { passphrase, .. } if passphrase.expose_secret() == "extra"
            ));
        }
    }

    #[test]
    fn newer_or_damaged_contents_are_refused() {
        let contents = Contents {
            next_id: 3,
            entries: vec![ton(TonContract::W5)],
        };
        let good = encode(&contents).unwrap();
        let mut newer = good.to_vec();
        newer[0] = VERSION + 1;
        assert!(matches!(decode(&newer), Err(KeyringError::Newer)));
        let kind_at = 1 + 4 + 2 + 4;
        let mut unknown = good.to_vec();
        unknown[kind_at] = 9;
        assert!(matches!(decode(&unknown), Err(KeyringError::Damaged)));
        let mut contract = good.to_vec();
        let last = contract.len() - 1;
        contract[last] = 7;
        assert!(matches!(decode(&contract), Err(KeyringError::Damaged)));
        let mut words = good.to_vec();
        words.swap(kind_at + 1, kind_at + 3);
        words.swap(kind_at + 2, kind_at + 4);
        assert!(matches!(decode(&words), Err(KeyringError::Damaged)));
        assert!(matches!(
            decode(&good[..good.len() - 1]),
            Err(KeyringError::Damaged)
        ));
        let mut longer = good.to_vec();
        longer.push(0);
        assert!(matches!(decode(&longer), Err(KeyringError::Damaged)));
    }
}
