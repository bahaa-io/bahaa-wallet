use bahaa_chain::{ChainError, memo_text};
use ed25519_dalek::VerifyingKey;
use sha2::{Digest, Sha256};

use crate::tx::{self, Instruction, Key, Meta, Parsed};

pub(crate) const SYSTEM: Key = [0; 32];
pub(crate) const TOKEN: Key = id("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
pub(crate) const TOKEN_2022: Key = id("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");
pub(crate) const ASSOCIATED: Key = id("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
pub(crate) const MEMO: Key = id("MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr");
pub(crate) const MEMO_V1: Key = id("Memo1UhkJRfHyvLMcVucJwxXeuD728EqVDDwQDxFMNo");
pub(crate) const BUDGET: Key = id("ComputeBudget111111111111111111111111111111");
pub(crate) const METADATA: Key = id("metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s");
pub(crate) const CORE: Key = id("CoREENxT6tW1HoK8ypY1SxRMZTcVPm7R94rH4PZNhX7d");
pub(crate) const WRAPPED: Key = id("So11111111111111111111111111111111111111112");

const TRANSFER: u32 = 2;
const CORE_TRANSFER: [u8; 2] = [14, 0];
const TRANSFER_CHECKED: u8 = 12;
const CREATE_IDEMPOTENT: u8 = 1;
const UNIT_LIMIT: u8 = 2;
const UNIT_PRICE: u8 = 3;

const fn id(text: &str) -> Key {
    bs58::decode(text.as_bytes()).into_array_const_unwrap()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Moved {
    Coin,
    Token { mint: Key, decimals: u8 },
    Core { asset: Key, collection: Option<Key> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Shape {
    pub from: Key,
    pub to: Key,
    pub moved: Moved,
    pub units: u64,
    pub memo: Option<String>,
    pub limit: u32,
    pub price: u64,
}

impl Shape {
    pub(crate) fn instructions(&self) -> Result<Vec<Instruction>, ChainError> {
        let mut list = vec![
            budget(UNIT_LIMIT, &self.limit.to_le_bytes()),
            budget(UNIT_PRICE, &self.price.to_le_bytes()),
        ];
        let memo = self.memo.as_ref().map(|text| Instruction {
            program: MEMO,
            accounts: Vec::new(),
            data: text.as_bytes().to_vec(),
        });
        match self.moved {
            Moved::Coin => {
                list.extend(memo);
                let mut data = TRANSFER.to_le_bytes().to_vec();
                data.extend_from_slice(&self.units.to_le_bytes());
                list.push(Instruction {
                    program: SYSTEM,
                    accounts: vec![meta(self.from, true, true), meta(self.to, false, true)],
                    data,
                });
            }
            Moved::Token { mint, decimals } => {
                let source = token_account(&self.from, &mint)?;
                let destination = token_account(&self.to, &mint)?;
                list.push(Instruction {
                    program: ASSOCIATED,
                    accounts: vec![
                        meta(self.from, true, true),
                        meta(destination, false, true),
                        meta(self.to, false, false),
                        meta(mint, false, false),
                        meta(SYSTEM, false, false),
                        meta(TOKEN, false, false),
                    ],
                    data: vec![CREATE_IDEMPOTENT],
                });
                list.extend(memo);
                let mut data = vec![TRANSFER_CHECKED];
                data.extend_from_slice(&self.units.to_le_bytes());
                data.push(decimals);
                list.push(Instruction {
                    program: TOKEN,
                    accounts: vec![
                        meta(source, false, true),
                        meta(mint, false, false),
                        meta(destination, false, true),
                        meta(self.from, true, false),
                    ],
                    data,
                });
            }
            Moved::Core { asset, collection } => {
                list.extend(memo);
                list.push(Instruction {
                    program: CORE,
                    accounts: vec![
                        meta(asset, false, true),
                        meta(collection.unwrap_or(CORE), false, false),
                        meta(self.from, true, true),
                        meta(CORE, false, false),
                        meta(self.to, false, false),
                        meta(SYSTEM, false, false),
                        meta(CORE, false, false),
                    ],
                    data: CORE_TRANSFER.to_vec(),
                });
            }
        }
        Ok(list)
    }

    pub(crate) fn read(parsed: &Parsed) -> Result<Self, ChainError> {
        let unknown = || ChainError::Mismatch("the kind of transfer");
        let instructions = parsed.instructions.as_slice();
        let [limit, price, .., last] = instructions else {
            return Err(unknown());
        };
        let word = |instruction: &Instruction, tag: u8| -> Option<Vec<u8>> {
            let (first, rest) = instruction.data.split_first()?;
            (instruction.program == BUDGET && *first == tag).then(|| rest.to_vec())
        };
        let limit = word(limit, UNIT_LIMIT)
            .and_then(|bytes| bytes.try_into().ok())
            .map(u32::from_le_bytes)
            .ok_or_else(unknown)?;
        let price = word(price, UNIT_PRICE)
            .and_then(|bytes| bytes.try_into().ok())
            .map(u64::from_le_bytes)
            .ok_or_else(unknown)?;
        let units = |bytes: Option<&[u8]>| {
            bytes
                .and_then(|bytes| bytes.try_into().ok())
                .map(u64::from_le_bytes)
                .ok_or_else(unknown)
        };
        let (to, moved, units) = match last.program {
            SYSTEM => (
                last.accounts.get(1).ok_or_else(unknown)?.key,
                Moved::Coin,
                units(last.data.get(4..12))?,
            ),
            CORE => {
                let account = |index: usize| {
                    last.accounts
                        .get(index)
                        .map(|meta| meta.key)
                        .ok_or_else(unknown)
                };
                let collection = Some(account(1)?).filter(|key| *key != CORE);
                (
                    account(4)?,
                    Moved::Core {
                        asset: account(0)?,
                        collection,
                    },
                    1,
                )
            }
            TOKEN => {
                let owner = instructions
                    .iter()
                    .find(|instruction| instruction.program == ASSOCIATED)
                    .and_then(|create| create.accounts.get(2))
                    .ok_or_else(unknown)?
                    .key;
                let mint = last.accounts.get(1).ok_or_else(unknown)?.key;
                let decimals = *last.data.get(9).ok_or_else(unknown)?;
                (
                    owner,
                    Moved::Token { mint, decimals },
                    units(last.data.get(1..9))?,
                )
            }
            _ => return Err(unknown()),
        };
        let memo = match instructions
            .iter()
            .find(|instruction| instruction.program == MEMO)
        {
            Some(instruction) => Some(
                std::str::from_utf8(&instruction.data)
                    .ok()
                    .filter(|text| memo_text(text.as_bytes()).as_deref() == Some(text))
                    .ok_or(ChainError::Mismatch("the memo"))?
                    .to_owned(),
            ),
            None => None,
        };
        let shape = Self {
            from: parsed.payer,
            to,
            moved,
            units,
            memo,
            limit,
            price,
        };
        if tx::compile(parsed.payer, parsed.blockhash, &shape.instructions()?)? != parsed.message {
            return Err(unknown());
        }
        Ok(shape)
    }
}

pub(crate) fn token_account(owner: &Key, mint: &Key) -> Result<Key, ChainError> {
    derived(&[owner, &TOKEN, mint], &ASSOCIATED)
}

pub(crate) fn metadata_account(mint: &Key) -> Result<Key, ChainError> {
    derived(&[b"metadata", &METADATA, mint], &METADATA)
}

fn derived(seeds: &[&[u8]], program: &Key) -> Result<Key, ChainError> {
    (0..=u8::MAX)
        .rev()
        .find_map(|bump| {
            let mut hash = Sha256::new();
            for seed in seeds {
                hash.update(seed);
            }
            hash.update([bump]);
            hash.update(program);
            hash.update(b"ProgramDerivedAddress");
            let key: Key = hash.finalize().into();
            VerifyingKey::from_bytes(&key).is_err().then_some(key)
        })
        .ok_or(ChainError::Invalid(
            "no account address exists for this token",
        ))
}

fn budget(tag: u8, value: &[u8]) -> Instruction {
    let mut data = vec![tag];
    data.extend_from_slice(value);
    Instruction {
        program: BUDGET,
        accounts: Vec::new(),
        data,
    }
}

fn meta(key: Key, signer: bool, writable: bool) -> Meta {
    Meta {
        key,
        signer,
        writable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(text: &str) -> Key {
        bs58::decode(text).into_vec().unwrap().try_into().unwrap()
    }

    #[test]
    fn token_accounts_match_the_ones_other_wallets_derive() {
        let owner = key("B1iGmDJdvmxyUiYM8UEo2Uw2D58EmUrw4KyLYMmrhf8V");
        let mint = key("SRMuApVNdxXokk5GT7XD5cUUgXMBCoAz2LHeuAoKWRt");
        assert_eq!(
            token_account(&owner, &mint).unwrap(),
            key("EDNd1ycsydWYwVmrYZvqYazFqwk1QjBgAUKFjBoz1jKP")
        );
        let owner = key("68dzdXkni9BrAwU1asAwurMEdQhXUJq6MNY8niDAny8t");
        let mint = key("7atgF8KQo4wJrD5ATGX7t1V2zVvykPJbFfNeVf1icFv1");
        assert_eq!(
            derived(&[&owner, &TOKEN_2022, &mint], &ASSOCIATED).unwrap(),
            key("3PaFQnebQMHBgthRScup2B932cMxA1GBP7m9roCkomHq")
        );
    }

    #[test]
    fn program_ids_are_the_well_known_ones() {
        assert_eq!(SYSTEM, key("11111111111111111111111111111111"));
        assert_eq!(TOKEN[..4], [0x06, 0xdd, 0xf6, 0xe1]);
        assert_ne!(TOKEN, TOKEN_2022);
    }

    fn shape(moved: Moved, memo: Option<&str>) -> Shape {
        Shape {
            from: [1; 32],
            to: [2; 32],
            moved,
            units: 1_500_000,
            memo: memo.map(str::to_owned),
            limit: 6_000,
            price: 25_000,
        }
    }

    const TOKEN_MOVE: Moved = Moved::Token {
        mint: [7; 32],
        decimals: 6,
    };

    fn core(collection: Option<Key>) -> Moved {
        Moved::Core {
            asset: [8; 32],
            collection,
        }
    }

    fn parsed(payer: Key, instructions: Vec<Instruction>) -> Parsed {
        Parsed {
            signature: [0; 64],
            payer,
            blockhash: [5; 32],
            message: tx::compile(payer, [5; 32], &instructions).unwrap(),
            instructions,
        }
    }

    #[test]
    fn every_shape_reads_back_from_its_instructions() {
        for shape in [
            shape(Moved::Coin, None),
            shape(Moved::Coin, Some("deposit 4417 🙂")),
            shape(TOKEN_MOVE, None),
            shape(TOKEN_MOVE, Some("invoice 12")),
            Shape {
                units: 1,
                ..shape(core(None), None)
            },
            Shape {
                units: 1,
                ..shape(core(Some([9; 32])), Some("gift"))
            },
        ] {
            let read = parsed(shape.from, shape.instructions().unwrap());
            assert_eq!(Shape::read(&read).unwrap(), shape);
        }
    }

    #[test]
    fn anything_beyond_the_shape_is_refused() {
        let native = shape(Moved::Coin, Some("a"));
        let mut extra = native.instructions().unwrap();
        extra.insert(2, extra[2].clone());
        assert!(Shape::read(&parsed(native.from, extra)).is_err());
        let mut other = native.instructions().unwrap();
        other[3].data[0] = 3;
        assert!(Shape::read(&parsed(native.from, other)).is_err());
        let mut hidden = native.instructions().unwrap();
        hidden[2].data = "a\u{202e}".as_bytes().to_vec();
        assert_eq!(
            Shape::read(&parsed(native.from, hidden)).unwrap_err(),
            ChainError::Mismatch("the memo")
        );
        let token = shape(TOKEN_MOVE, None);
        let mut stranger = token.instructions().unwrap();
        stranger[2].accounts[1].key = [4; 32];
        assert!(Shape::read(&parsed(token.from, stranger)).is_err());
        let short = token.instructions().unwrap()[..2].to_vec();
        assert!(Shape::read(&parsed(token.from, short)).is_err());
        let mut moved = parsed(token.from, token.instructions().unwrap());
        moved.blockhash = [6; 32];
        assert!(Shape::read(&moved).is_err());
    }
}
