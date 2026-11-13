use bahaa_chain::{ChainError, memo_text};
use ed25519_dalek::VerifyingKey;
use sha2::{Digest, Sha256};

use crate::bubblegum::{Compressed, Leaf, MAX_DEPTH, V2};
use crate::metaplex::Reader;
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
pub(crate) const RULES: Key = id("auth9SigNpDKz4sJJ1DfCTuZrZNSAgh9sFD3rboVmgg");
pub(crate) const INSTRUCTIONS: Key = id("Sysvar1nstructions1111111111111111111111111");
pub(crate) const WRAPPED: Key = id("So11111111111111111111111111111111111111112");
pub(crate) const BUBBLEGUM: Key = id("BGUMAp9Gq7iTEuizy4pqaxsTyUCBK68MDfK752saRPUY");
pub(crate) const SPL_COMPRESSION: Key = id("cmtDvXumGCrqC1Age74AVPhSRVXJMd8PJS91L8KbNCK");
pub(crate) const MPL_COMPRESSION: Key = id("mcmt6YrQEMKw8Mw43FmpRLmf7BqRnFMKmAcbxE3xkAW");
const SPL_NOOP: Key = id("noopb9bkMVfRPU8AsbpTUg8AQkHtKwMYZiFUjNRtMmV");
const MPL_NOOP: Key = id("mnoopTCrg4p8ry25e4bcWA9XZjbNjMTfgYVGGEdRsf3");

const TRANSFER: u32 = 2;
const CORE_TRANSFER: [u8; 2] = [14, 0];
const PROGRAMMABLE_TRANSFER: [u8; 2] = [49, 0];
const COMPRESSED_TRANSFER: [u8; 8] = [163, 52, 200, 231, 140, 3, 69, 186];
const COMPRESSED_TRANSFER_V2: [u8; 8] = [119, 40, 6, 235, 234, 221, 248, 49];
const V1_ACCOUNTS: usize = 8;
const V2_ACCOUNTS: usize = 11;
const TRANSFER_CHECKED: u8 = 12;
const TRANSFER_FEE: u8 = 26;
const WITH_FEE: u8 = 1;
const CREATE_IDEMPOTENT: u8 = 1;
const UNIT_LIMIT: u8 = 2;
const UNIT_PRICE: u8 = 3;

const fn id(text: &str) -> Key {
    bs58::decode(text.as_bytes()).into_array_const_unwrap()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Moved {
    Coin,
    Token {
        mint: Key,
        decimals: u8,
        program: Key,
        fee: Option<u64>,
    },
    Core {
        asset: Key,
        collection: Option<Key>,
    },
    Programmable {
        mint: Key,
        rules: Option<Key>,
    },
    Compressed(Box<Compressed>),
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
        match &self.moved {
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
            Moved::Token {
                mint,
                decimals,
                program,
                fee,
            } => {
                let source = token_account(&self.from, program, mint)?;
                let destination = token_account(&self.to, program, mint)?;
                list.push(Instruction {
                    program: ASSOCIATED,
                    accounts: vec![
                        meta(self.from, true, true),
                        meta(destination, false, true),
                        meta(self.to, false, false),
                        meta(*mint, false, false),
                        meta(SYSTEM, false, false),
                        meta(*program, false, false),
                    ],
                    data: vec![CREATE_IDEMPOTENT],
                });
                list.extend(memo);
                let mut data = match fee {
                    None => vec![TRANSFER_CHECKED],
                    Some(_) => vec![TRANSFER_FEE, WITH_FEE],
                };
                data.extend_from_slice(&self.units.to_le_bytes());
                data.push(*decimals);
                if let Some(fee) = fee {
                    data.extend_from_slice(&fee.to_le_bytes());
                }
                list.push(Instruction {
                    program: *program,
                    accounts: vec![
                        meta(source, false, true),
                        meta(*mint, false, false),
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
                        meta(*asset, false, true),
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
            Moved::Programmable { mint, rules } => {
                list.extend(memo);
                list.push(self.programmable(*mint, *rules)?);
            }
            Moved::Compressed(moving) => {
                list.extend(memo);
                list.push(self.compressed(moving)?);
            }
        }
        Ok(list)
    }

    fn compressed(&self, moving: &Compressed) -> Result<Instruction, ChainError> {
        let leaf = &moving.leaf;
        let config = tree_config(&leaf.tree)?;
        let mut data = Vec::with_capacity(152);
        data.extend(match leaf.v2 {
            None => COMPRESSED_TRANSFER,
            Some(_) => COMPRESSED_TRANSFER_V2,
        });
        data.extend(moving.root);
        data.extend(leaf.data_hash);
        data.extend(leaf.creator_hash);
        let mut accounts = match &leaf.v2 {
            None => vec![
                meta(config, false, false),
                meta(self.from, true, false),
                meta(leaf.delegate, false, false),
                meta(self.to, false, false),
                meta(leaf.tree, false, true),
                meta(SPL_NOOP, false, false),
                meta(SPL_COMPRESSION, false, false),
                meta(SYSTEM, false, false),
            ],
            Some(v2) => {
                data.push(1);
                data.extend(v2.asset_data_hash);
                match v2.flags {
                    0 => data.push(0),
                    flags => data.extend([1, flags]),
                }
                vec![
                    meta(config, false, true),
                    meta(self.from, true, true),
                    meta(BUBBLEGUM, false, false),
                    meta(self.from, false, false),
                    meta(leaf.delegate, false, false),
                    meta(self.to, false, false),
                    meta(leaf.tree, false, true),
                    meta(v2.collection.unwrap_or(BUBBLEGUM), false, false),
                    meta(MPL_NOOP, false, false),
                    meta(MPL_COMPRESSION, false, false),
                    meta(SYSTEM, false, false),
                ]
            }
        };
        data.extend(leaf.nonce.to_le_bytes());
        data.extend(leaf.index.to_le_bytes());
        accounts.extend(moving.proof.iter().map(|node| meta(*node, false, false)));
        Ok(Instruction {
            program: BUBBLEGUM,
            accounts,
            data,
        })
    }

    fn programmable(&self, mint: Key, rules: Option<Key>) -> Result<Instruction, ChainError> {
        let source = token_account(&self.from, &TOKEN, &mint)?;
        let destination = token_account(&self.to, &TOKEN, &mint)?;
        let mut data = PROGRAMMABLE_TRANSFER.to_vec();
        data.extend_from_slice(&self.units.to_le_bytes());
        data.push(0);
        Ok(Instruction {
            program: METADATA,
            accounts: vec![
                meta(source, false, true),
                meta(self.from, false, false),
                meta(destination, false, true),
                meta(self.to, false, false),
                meta(mint, false, false),
                meta(metadata_account(&mint)?, false, true),
                meta(edition_account(&mint)?, false, false),
                meta(token_record(&mint, &source)?, false, true),
                meta(token_record(&mint, &destination)?, false, true),
                meta(self.from, true, false),
                meta(self.from, true, true),
                meta(SYSTEM, false, false),
                meta(INSTRUCTIONS, false, false),
                meta(TOKEN, false, false),
                meta(ASSOCIATED, false, false),
                meta(rules.map_or(METADATA, |_| RULES), false, false),
                meta(rules.unwrap_or(METADATA), false, false),
            ],
            data,
        })
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
        let account = |index: usize| {
            last.accounts
                .get(index)
                .map(|meta| meta.key)
                .ok_or_else(unknown)
        };
        let (to, moved, units) = match last.program {
            SYSTEM => (account(1)?, Moved::Coin, units(last.data.get(4..12))?),
            CORE => (
                account(4)?,
                Moved::Core {
                    asset: account(0)?,
                    collection: Some(account(1)?).filter(|key| *key != CORE),
                },
                1,
            ),
            METADATA => (
                account(3)?,
                Moved::Programmable {
                    mint: account(4)?,
                    rules: Some(account(16)?).filter(|key| *key != METADATA),
                },
                units(last.data.get(2..10))?,
            ),
            BUBBLEGUM => {
                let (to, moving) = read_compressed(last).ok_or_else(unknown)?;
                (to, Moved::Compressed(Box::new(moving)), 1)
            }
            TOKEN | TOKEN_2022 => read_token(instructions, last).ok_or_else(unknown)?,
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

pub(crate) fn token_account(owner: &Key, program: &Key, mint: &Key) -> Result<Key, ChainError> {
    derived(&[owner, program, mint], &ASSOCIATED)
}

pub(crate) fn metadata_account(mint: &Key) -> Result<Key, ChainError> {
    derived(&[b"metadata", &METADATA, mint], &METADATA)
}

pub(crate) fn edition_account(mint: &Key) -> Result<Key, ChainError> {
    derived(&[b"metadata", &METADATA, mint, b"edition"], &METADATA)
}

pub(crate) fn tree_config(tree: &Key) -> Result<Key, ChainError> {
    derived(&[tree], &BUBBLEGUM)
}

pub(crate) fn asset_id(tree: &Key, nonce: u64) -> Result<Key, ChainError> {
    derived(&[b"asset", tree, &nonce.to_le_bytes()], &BUBBLEGUM)
}

pub(crate) fn token_record(mint: &Key, account: &Key) -> Result<Key, ChainError> {
    derived(
        &[b"metadata", &METADATA, mint, b"token_record", account],
        &METADATA,
    )
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

fn read_token(instructions: &[Instruction], last: &Instruction) -> Option<(Key, Moved, u64)> {
    let owner = instructions
        .iter()
        .find(|instruction| instruction.program == ASSOCIATED)?
        .accounts
        .get(2)?
        .key;
    let word = |bytes: Option<&[u8]>| Some(u64::from_le_bytes(bytes?.try_into().ok()?));
    let (rest, fee) = match last.data.as_slice() {
        [TRANSFER_CHECKED, rest @ ..] => (rest, None),
        [TRANSFER_FEE, WITH_FEE, rest @ ..] if last.program == TOKEN_2022 => {
            (rest, Some(word(rest.get(9..17))?))
        }
        _ => return None,
    };
    Some((
        owner,
        Moved::Token {
            mint: last.accounts.get(1)?.key,
            decimals: *rest.get(8)?,
            program: last.program,
            fee,
        },
        word(rest.get(..8))?,
    ))
}

fn read_compressed(instruction: &Instruction) -> Option<(Key, Compressed)> {
    let mut read = Reader(&instruction.data);
    let v2 = match read.take(8)? {
        head if head == COMPRESSED_TRANSFER => false,
        head if head == COMPRESSED_TRANSFER_V2 => true,
        _ => return None,
    };
    let (root, data_hash, creator_hash) = (read.key()?, read.key()?, read.key()?);
    let extra = if v2 {
        if !read.flag()? {
            return None;
        }
        let asset_data_hash = read.key()?;
        let flags = if read.flag()? { read.byte()? } else { 0 };
        Some((asset_data_hash, flags))
    } else {
        None
    };
    let nonce = u64::from_le_bytes(read.take(8)?.try_into().ok()?);
    let index = u32::from_le_bytes(read.take(4)?.try_into().ok()?);
    let keys: Vec<Key> = instruction.accounts.iter().map(|meta| meta.key).collect();
    let fixed = if v2 { V2_ACCOUNTS } else { V1_ACCOUNTS };
    let proof = keys.get(fixed..).filter(|proof| proof.len() <= MAX_DEPTH)?;
    let at = |v1: usize, v2: usize| keys.get(if extra.is_some() { v2 } else { v1 }).copied();
    let leaf = Leaf {
        tree: at(4, 6)?,
        owner: at(1, 3)?,
        delegate: at(2, 4)?,
        nonce,
        index,
        data_hash,
        creator_hash,
        v2: extra.map(|(asset_data_hash, flags)| V2 {
            collection: keys.get(7).copied().filter(|key| *key != BUBBLEGUM),
            asset_data_hash,
            flags,
        }),
    };
    Some((
        at(3, 5)?,
        Compressed {
            leaf,
            root,
            proof: proof.to_vec(),
        },
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

#[cfg(fuzzing)]
pub(crate) fn fuzz(data: &[u8]) {
    if let Ok(parsed) = tx::parse(data) {
        let _ = Shape::read(&parsed);
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
            token_account(&owner, &TOKEN, &mint).unwrap(),
            key("EDNd1ycsydWYwVmrYZvqYazFqwk1QjBgAUKFjBoz1jKP")
        );
        let owner = key("68dzdXkni9BrAwU1asAwurMEdQhXUJq6MNY8niDAny8t");
        let mint = key("7atgF8KQo4wJrD5ATGX7t1V2zVvykPJbFfNeVf1icFv1");
        assert_eq!(
            token_account(&owner, &TOKEN_2022, &mint).unwrap(),
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
        program: TOKEN,
        fee: None,
    };

    const TOKEN_2022_MOVE: Moved = Moved::Token {
        mint: [7; 32],
        decimals: 6,
        program: TOKEN_2022,
        fee: None,
    };

    const FEE_MOVE: Moved = Moved::Token {
        mint: [7; 32],
        decimals: 6,
        program: TOKEN_2022,
        fee: Some(40_350),
    };

    fn core(collection: Option<Key>) -> Moved {
        Moved::Core {
            asset: [8; 32],
            collection,
        }
    }

    fn programmable(rules: Option<Key>) -> Moved {
        Moved::Programmable {
            mint: [10; 32],
            rules,
        }
    }

    fn compressed(v2: Option<V2>, delegate: Key, proof: usize) -> Moved {
        Moved::Compressed(Box::new(Compressed {
            leaf: Leaf {
                tree: [12; 32],
                owner: [1; 32],
                delegate,
                nonce: 9_399,
                index: 9_398,
                data_hash: [13; 32],
                creator_hash: [14; 32],
                v2,
            },
            root: [15; 32],
            proof: (0..proof)
                .map(|node| [u8::try_from(node).unwrap() + 100; 32])
                .collect(),
        }))
    }

    fn v2(collection: Option<Key>, flags: u8) -> V2 {
        V2 {
            collection,
            asset_data_hash: [16; 32],
            flags,
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
            shape(TOKEN_2022_MOVE, None),
            shape(FEE_MOVE, Some("invoice 13")),
            Shape {
                units: 1,
                ..shape(core(None), None)
            },
            Shape {
                units: 1,
                ..shape(core(Some([9; 32])), Some("gift"))
            },
            Shape {
                units: 1,
                ..shape(programmable(None), None)
            },
            Shape {
                units: 1,
                ..shape(programmable(Some([11; 32])), Some("gift"))
            },
            Shape {
                units: 1,
                ..shape(compressed(None, [1; 32], 7), None)
            },
            Shape {
                units: 1,
                ..shape(compressed(None, [17; 32], 14), Some("gift"))
            },
            Shape {
                units: 1,
                ..shape(compressed(Some(v2(Some([18; 32]), 0)), [1; 32], 6), None)
            },
            Shape {
                units: 1,
                ..shape(compressed(Some(v2(None, 8)), [17; 32], 0), Some("gift"))
            },
        ] {
            let read = parsed(shape.from, shape.instructions().unwrap());
            assert_eq!(Shape::read(&read).unwrap(), shape);
        }
    }

    #[test]
    fn compressed_transfers_match_real_ones_of_both_versions() {
        use base64::Engine;
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/bubblegum.json")).unwrap();
        for (version, owner, fixed) in [("v1", 1, V1_ACCOUNTS), ("v2", 3, V2_ACCOUNTS)] {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(fixture[version]["transaction"].as_str().unwrap())
                .unwrap();
            let wire = tx::read(&bytes).unwrap();
            let real = wire
                .instructions
                .iter()
                .find(|instruction| wire.keys[usize::from(instruction.program)] == BUBBLEGUM)
                .unwrap();
            let [signers, signed_readonly, readonly] = wire.header.map(usize::from);
            let flags = |index: usize| {
                let signer = index < signers;
                let writable = if signer {
                    index < signers - signed_readonly
                } else {
                    index < wire.keys.len() - readonly
                };
                (signer, writable)
            };
            let instruction = Instruction {
                program: BUBBLEGUM,
                accounts: real
                    .accounts
                    .iter()
                    .map(|index| {
                        let index = usize::from(*index);
                        let (signer, writable) = flags(index);
                        meta(wire.keys[index], signer, writable)
                    })
                    .collect(),
                data: real.data.clone(),
            };
            let (to, moving) = read_compressed(&instruction).unwrap();
            assert_eq!(moving.leaf.owner, instruction.accounts[owner].key);
            assert_eq!(moving.proof.len(), instruction.accounts.len() - fixed);
            assert_eq!(moving.leaf.v2.is_some(), version == "v2");
            let shape = Shape {
                from: moving.leaf.owner,
                to,
                moved: Moved::Compressed(Box::new(moving)),
                units: 1,
                memo: None,
                limit: 1,
                price: 0,
            };
            let ours = shape.instructions().unwrap().pop().unwrap();
            assert_eq!(ours.data, real.data, "{version}");
            assert_eq!(ours.accounts.len(), instruction.accounts.len(), "{version}");
            for (index, (built, seen)) in
                ours.accounts.iter().zip(&instruction.accounts).enumerate()
            {
                assert_eq!(built.key, seen.key, "{version} account {index}");
                assert!(
                    !built.writable || seen.writable,
                    "{version} account {index}"
                );
                assert!(!built.signer || seen.signer, "{version} account {index}");
            }
            let Moved::Compressed(moving) = &shape.moved else {
                unreachable!()
            };
            assert_eq!(
                ours.accounts[0].key,
                tree_config(&moving.leaf.tree).unwrap()
            );
        }
    }

    #[test]
    fn a_programmable_transfer_names_the_accounts_of_a_real_one() {
        use base64::Engine;
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/pnft.json")).unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(fixture["transaction"].as_str().unwrap())
            .unwrap();
        let wire = tx::read(&bytes).unwrap();
        let real = wire
            .instructions
            .iter()
            .find(|instruction| wire.keys[usize::from(instruction.program)] == METADATA)
            .unwrap();
        let keys: Vec<Key> = real
            .accounts
            .iter()
            .map(|index| wire.keys[usize::from(*index)])
            .collect();
        let shape = Shape {
            from: keys[1],
            to: keys[3],
            moved: Moved::Programmable {
                mint: keys[4],
                rules: Some(keys[16]),
            },
            units: 1,
            memo: None,
            limit: 1,
            price: 0,
        };
        let ours = shape.instructions().unwrap().pop().unwrap();
        assert_eq!(ours.data, real.data);
        assert_eq!(ours.accounts.len(), keys.len());
        for (index, (built, real)) in ours.accounts.iter().zip(&keys).enumerate() {
            if index == 10 {
                assert_eq!(built.key, shape.from);
            } else {
                assert_eq!(built.key, *real, "account {index}");
            }
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
