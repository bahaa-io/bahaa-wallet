use bahaa_crypto::{ExposeSecret, KEY_BYTES, Mnemonic, SecretString, TON_WORDS, TonMnemonic};
use bahaa_types::{Address, ChainFamily, SolanaPath, TonContract};
use zeroize::Zeroizing;

use crate::KeyringError;

const VERSION: u8 = 8;
const FIRST_VERSION: u8 = 1;
const CONTRACT_VERSION: u8 = 3;
const SOLANA_PATH_VERSION: u8 = 8;
const MAX_WALLETS: usize = 64;
const MAX_PASSPHRASE: usize = 1024;
const BIP39: u8 = 0;
const TON: u8 = 1;
const KEY: u8 = 2;
const WATCH: u8 = 3;
const FAMILY_EVM: u8 = 0;
const FAMILY_TRON: u8 = 1;
const FAMILY_TON: u8 = 2;
const FAMILY_SOLANA: u8 = 3;
const W5: u8 = 0;
const V4R2: u8 = 1;
const UNKNOWN_PATH: u8 = 0;
const BIP44_CHANGE: u8 = 1;
const BIP44: u8 = 2;

pub enum Phrase {
    Bip39 {
        mnemonic: Mnemonic,
        passphrase: SecretString,
        contract: TonContract,
        solana: Option<SolanaPath>,
    },
    Ton {
        mnemonic: TonMnemonic,
        contract: TonContract,
    },
    Key {
        key: Zeroizing<[u8; KEY_BYTES]>,
        family: ChainFamily,
    },
    TonKey {
        key: Zeroizing<[u8; KEY_BYTES]>,
        contract: TonContract,
    },
    Watch {
        address: Address,
        counted: bool,
    },
}

impl Phrase {
    pub fn contract(&self) -> Option<TonContract> {
        match self {
            Self::Bip39 { contract, .. }
            | Self::Ton { contract, .. }
            | Self::TonKey { contract, .. } => Some(*contract),
            Self::Key { .. } | Self::Watch { .. } => None,
        }
    }

    pub fn contract_mut(&mut self) -> Option<&mut TonContract> {
        match self {
            Self::Bip39 { contract, .. }
            | Self::Ton { contract, .. }
            | Self::TonKey { contract, .. } => Some(contract),
            Self::Key { .. } | Self::Watch { .. } => None,
        }
    }

    pub fn solana_path(&self) -> Option<SolanaPath> {
        match self {
            Self::Bip39 { solana, .. } => Some(solana.unwrap_or_default()),
            _ => None,
        }
    }

    pub fn undetected(&self) -> bool {
        matches!(self, Self::Bip39 { solana: None, .. })
    }

    pub fn solana_mut(&mut self) -> Option<&mut Option<SolanaPath>> {
        match self {
            Self::Bip39 { solana, .. } => Some(solana),
            _ => None,
        }
    }

    pub fn watched(&self) -> Option<Address> {
        match self {
            Self::Watch { address, .. } => Some(*address),
            _ => None,
        }
    }

    pub fn counted(&self) -> bool {
        match self {
            Self::Watch { counted, .. } => *counted,
            _ => true,
        }
    }

    pub fn counted_mut(&mut self) -> Option<&mut bool> {
        match self {
            Self::Watch { counted, .. } => Some(counted),
            _ => None,
        }
    }
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
                ..
            } => {
                out.push(BIP39);
                bip39(&mut out, mnemonic, passphrase)?;
            }
            Phrase::Ton { mnemonic, .. } => {
                out.push(TON);
                for index in mnemonic.expose_indices() {
                    out.extend_from_slice(&index.to_be_bytes());
                }
            }
            Phrase::Key { key, family } => {
                out.push(KEY);
                out.push(match family {
                    ChainFamily::Evm => FAMILY_EVM,
                    ChainFamily::Tron => FAMILY_TRON,
                    ChainFamily::Solana => FAMILY_SOLANA,
                    ChainFamily::Ton => return Err(KeyringError::Damaged),
                });
                out.extend_from_slice(&**key);
            }
            Phrase::TonKey { key, .. } => {
                out.push(KEY);
                out.push(FAMILY_TON);
                out.extend_from_slice(&**key);
            }
            Phrase::Watch { address, counted } => {
                out.push(WATCH);
                match address {
                    Address::Evm(bytes) => {
                        out.push(FAMILY_EVM);
                        out.extend_from_slice(bytes);
                    }
                    Address::Tron(bytes) => {
                        out.push(FAMILY_TRON);
                        out.extend_from_slice(bytes);
                    }
                    Address::Ton { workchain, hash } => {
                        out.push(FAMILY_TON);
                        out.extend_from_slice(&workchain.to_be_bytes());
                        out.extend_from_slice(hash);
                    }
                    Address::Solana(bytes) => {
                        out.push(FAMILY_SOLANA);
                        out.extend_from_slice(bytes);
                    }
                }
                out.push(u8::from(*counted));
            }
        }
        if let Some(contract) = entry.phrase.contract() {
            out.push(match contract {
                TonContract::W5 => W5,
                TonContract::V4R2 => V4R2,
            });
        }
        if let Phrase::Bip39 { solana, .. } = &entry.phrase {
            out.push(match solana {
                None => UNKNOWN_PATH,
                Some(SolanaPath::Bip44Change) => BIP44_CHANGE,
                Some(SolanaPath::Bip44) => BIP44,
            });
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
            BIP39 => read_bip39(&mut reader, version)?,
            TON => read_ton(&mut reader)?,
            KEY => read_key(&mut reader)?,
            WATCH => read_watch(&mut reader)?,
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

fn read_bip39(reader: &mut Reader<'_>, version: u8) -> Result<Phrase, KeyringError> {
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
    let contract = if version >= CONTRACT_VERSION {
        read_contract(reader)?
    } else {
        TonContract::W5
    };
    let solana = if version >= SOLANA_PATH_VERSION {
        match reader.take(1)?[0] {
            UNKNOWN_PATH => None,
            BIP44_CHANGE => Some(SolanaPath::Bip44Change),
            BIP44 => Some(SolanaPath::Bip44),
            _ => return Err(KeyringError::Damaged),
        }
    } else {
        None
    };
    Ok(Phrase::Bip39 {
        mnemonic,
        passphrase: SecretString::from(owned),
        contract,
        solana,
    })
}

fn read_ton(reader: &mut Reader<'_>) -> Result<Phrase, KeyringError> {
    let mut indices = Zeroizing::new([0u16; TON_WORDS]);
    for index in indices.iter_mut() {
        *index = u16::from_be_bytes(reader.array()?);
    }
    let mnemonic = TonMnemonic::from_indices(&*indices).map_err(|_| KeyringError::Damaged)?;
    Ok(Phrase::Ton {
        mnemonic,
        contract: read_contract(reader)?,
    })
}

fn read_key(reader: &mut Reader<'_>) -> Result<Phrase, KeyringError> {
    let family = reader.take(1)?[0];
    let mut key = Zeroizing::new([0u8; KEY_BYTES]);
    key.copy_from_slice(reader.take(KEY_BYTES)?);
    Ok(match family {
        FAMILY_EVM => Phrase::Key {
            key,
            family: ChainFamily::Evm,
        },
        FAMILY_TRON => Phrase::Key {
            key,
            family: ChainFamily::Tron,
        },
        FAMILY_TON => Phrase::TonKey {
            key,
            contract: read_contract(reader)?,
        },
        FAMILY_SOLANA => Phrase::Key {
            key,
            family: ChainFamily::Solana,
        },
        _ => return Err(KeyringError::Damaged),
    })
}

fn read_watch(reader: &mut Reader<'_>) -> Result<Phrase, KeyringError> {
    let address = match reader.take(1)?[0] {
        FAMILY_EVM => Address::Evm(reader.array()?),
        FAMILY_TRON => Address::Tron(reader.array()?),
        FAMILY_TON => Address::Ton {
            workchain: i8::from_be_bytes(reader.array()?),
            hash: reader.array()?,
        },
        FAMILY_SOLANA => Address::Solana(reader.array()?),
        _ => return Err(KeyringError::Damaged),
    };
    let counted = match reader.take(1)?[0] {
        0 => false,
        1 => true,
        _ => return Err(KeyringError::Damaged),
    };
    Ok(Phrase::Watch { address, counted })
}

fn read_contract(reader: &mut Reader<'_>) -> Result<TonContract, KeyringError> {
    match reader.take(1)?[0] {
        W5 => Ok(TonContract::W5),
        V4R2 => Ok(TonContract::V4R2),
        _ => Err(KeyringError::Damaged),
    }
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

    fn multi(contract: TonContract, solana: Option<SolanaPath>) -> Entry {
        Entry {
            id: 1,
            phrase: Phrase::Bip39 {
                mnemonic: Mnemonic::from_entropy(&[7; 16]).unwrap(),
                passphrase: SecretString::from("extra".to_owned()),
                contract,
                solana,
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
            contract,
            solana,
        } = &contents.entries[0].phrase
        else {
            panic!("an old vault holds multi-chain phrases");
        };
        assert_eq!(mnemonic.expose_entropy(), [7; 16]);
        assert_eq!(passphrase.expose_secret(), "pw");
        assert_eq!(*contract, TonContract::W5);
        assert_eq!(*solana, None);
    }

    #[test]
    fn a_second_version_vault_keeps_w5_for_multi_chain_phrases() {
        let mut old = vec![2];
        old.extend_from_slice(&5u32.to_be_bytes());
        old.extend_from_slice(&1u16.to_be_bytes());
        old.extend_from_slice(&3u32.to_be_bytes());
        old.push(BIP39);
        old.push(16);
        old.extend_from_slice(&[7; 16]);
        old.extend_from_slice(&0u16.to_be_bytes());
        let contents = decode(&old).unwrap();
        assert_eq!(contents.entries[0].phrase.contract(), Some(TonContract::W5));
    }

    #[test]
    fn private_keys_round_trip_with_their_family() {
        for family in [ChainFamily::Evm, ChainFamily::Tron, ChainFamily::Solana] {
            let contents = Contents {
                next_id: 4,
                entries: vec![Entry {
                    id: 3,
                    phrase: Phrase::Key {
                        key: Zeroizing::new([9; KEY_BYTES]),
                        family,
                    },
                }],
            };
            let decoded = decode(&encode(&contents).unwrap()).unwrap();
            assert!(matches!(
                &decoded.entries[0].phrase,
                Phrase::Key { key, family: read } if **key == [9; KEY_BYTES] && *read == family
            ));
            assert_eq!(decoded.entries[0].phrase.contract(), None);
        }
    }

    #[test]
    fn ton_keys_round_trip_with_their_contract() {
        for contract in [TonContract::W5, TonContract::V4R2] {
            let contents = Contents {
                next_id: 2,
                entries: vec![Entry {
                    id: 1,
                    phrase: Phrase::TonKey {
                        key: Zeroizing::new([5; KEY_BYTES]),
                        contract,
                    },
                }],
            };
            let encoded = encode(&contents).unwrap();
            let decoded = decode(&encoded).unwrap();
            assert!(matches!(
                &decoded.entries[0].phrase,
                Phrase::TonKey { key, contract: read } if **key == [5; KEY_BYTES] && *read == contract
            ));
            let mut unknown = encoded.to_vec();
            unknown[1 + 4 + 2 + 4 + 1] = 4;
            assert!(matches!(decode(&unknown), Err(KeyringError::Damaged)));
        }
    }

    #[test]
    fn watched_addresses_round_trip_with_their_switch() {
        let addresses = [
            Address::Evm([1; 20]),
            Address::Tron([2; 20]),
            Address::Ton {
                workchain: -1,
                hash: [3; 32],
            },
            Address::Solana([4; 32]),
        ];
        for (address, counted) in addresses.into_iter().zip([true, false, true, false]) {
            let contents = Contents {
                next_id: 2,
                entries: vec![Entry {
                    id: 1,
                    phrase: Phrase::Watch { address, counted },
                }],
            };
            let encoded = encode(&contents).unwrap();
            let decoded = decode(&encoded).unwrap();
            assert!(matches!(
                &decoded.entries[0].phrase,
                Phrase::Watch { address: read, counted: kept } if *read == address && *kept == counted
            ));
            assert_eq!(decoded.entries[0].phrase.contract(), None);
            let mut switch = encoded.to_vec();
            let last = switch.len() - 1;
            switch[last] = 2;
            assert!(matches!(decode(&switch), Err(KeyringError::Damaged)));
            let mut family = encoded.to_vec();
            family[1 + 4 + 2 + 4 + 1] = 4;
            assert!(matches!(decode(&family), Err(KeyringError::Damaged)));
        }
    }

    #[test]
    fn both_kinds_of_phrase_round_trip() {
        let paths = [None, Some(SolanaPath::Bip44Change), Some(SolanaPath::Bip44)];
        for (contract, solana) in [TonContract::W5, TonContract::V4R2, TonContract::W5]
            .into_iter()
            .zip(paths)
        {
            let contents = Contents {
                next_id: 3,
                entries: vec![multi(contract, solana), ton(contract)],
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
                Phrase::Bip39 { passphrase, contract: read, solana: path, .. }
                    if passphrase.expose_secret() == "extra" && *read == contract && *path == solana
            ));
            assert_eq!(decoded.entries[1].phrase.solana_path(), None);
            assert_eq!(decoded.entries[0].phrase.undetected(), solana.is_none());
            let mut unknown = encode(&contents).unwrap().to_vec();
            unknown[1 + 4 + 2 + 4 + 1 + 1 + 16 + 2 + 5 + 1] = 3;
            assert!(matches!(decode(&unknown), Err(KeyringError::Damaged)));
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
