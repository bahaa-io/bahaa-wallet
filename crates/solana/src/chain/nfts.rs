use std::collections::BTreeSet;
use std::sync::PoisonError;

use bahaa_chain::{ChainError, Found};
use bahaa_config::Feature;
use bahaa_types::{
    Address, Amount, AssetId, Collection, Lock, MAX_NFT_LINK, MAX_NFT_TEXT, MAX_NFTS, Nft, NftId,
    NftKind, Trust, is_plain_text, to_plain_text,
};
use futures_util::future::try_join;
use ruint::aliases::U256;
use serde_json::{Value, json};

use super::{SolanaChain, bytes, held};
use crate::metaplex;
use crate::programs::{Moved, TOKEN, metadata_account, token_account, token_record};
use crate::rpc::{self, Account};
use crate::tx::Key;

const PAGE: usize = 1_000;
const MAX_PAGES: usize = 3;
const MAX_KEYS: usize = 100;
const MINT_SIZE: usize = 82;
const FROZEN: u8 = 2;
const TOKEN_INTERFACES: [&str; 5] = [
    "V1_NFT",
    "V1_PRINT",
    "LEGACY_NFT",
    "V2_NFT",
    "ProgrammableNFT",
];
const CORE_INTERFACE: &str = "MplCoreAsset";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Listed {
    Token(Key),
    Core(Key),
}

impl SolanaChain {
    pub async fn nfts(&self, owner: &Address) -> Result<Found, ChainError> {
        let owner = bytes(owner)?;
        let (listed, capped) = self.listing(&owner).await?;
        let mints: Vec<Key> = listed
            .iter()
            .filter_map(|listed| match listed {
                Listed::Token(mint) => Some(*mint),
                Listed::Core(_) => None,
            })
            .collect();
        let assets: Vec<Key> = listed
            .iter()
            .filter_map(|listed| match listed {
                Listed::Core(asset) => Some(*asset),
                Listed::Token(_) => None,
            })
            .collect();
        let mut items = self.token_nfts(&owner, &mints).await?;
        items.extend(self.core_nfts(&owner, &assets).await?);
        Ok(Found { items, capped })
    }

    pub(super) async fn nft_move(&self, owner: &Key, id: &Key) -> Result<Moved, ChainError> {
        let not_held = ChainError::Invalid("this wallet doesn't hold that NFT");
        let source = token_account(owner, id)?;
        let found = self
            .accounts(
                &[
                    *id,
                    metadata_account(id)?,
                    source,
                    token_record(id, &source)?,
                ],
                None,
            )
            .await?;
        let [item, record, account, state] = found.as_slice() else {
            return Err(ChainError::malformed());
        };
        if let Some(core) = metaplex::core(item.as_ref()) {
            if core.owner != *owner {
                return Err(not_held);
            }
            return Ok(Moved::Core {
                asset: *id,
                collection: core.collection,
            });
        }
        if !one_of_one(item.as_ref()) || held(account.as_ref(), owner, id)? != 1 {
            return Err(not_held);
        }
        let record = metaplex::metadata(record.as_ref(), id)
            .ok_or(ChainError::Invalid("this is not an NFT the wallet knows"))?;
        match record.standard {
            Some(4 | 5) => {
                return match metaplex::token_lock(state.as_ref()) {
                    None => Ok(Moved::Programmable {
                        mint: *id,
                        rules: record.rules,
                    }),
                    Some(Lock::Listed) => Err(ChainError::Invalid(
                        "this NFT is listed for sale, cancel the listing to send it",
                    )),
                    Some(_) => Err(ChainError::Invalid(
                        "this NFT is locked, for example while staked, and can't be sent",
                    )),
                };
            }
            None | Some(0 | 3) => {}
            Some(_) => return Err(ChainError::Invalid("this is a token, not an NFT")),
        }
        if frozen(account.as_ref()) {
            return Err(ChainError::Invalid("this NFT is frozen and can't be sent"));
        }
        self.nft_mints
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(*id);
        Ok(Moved::Token {
            mint: *id,
            decimals: 0,
        })
    }

    async fn listing(&self, owner: &Key) -> Result<(Vec<Listed>, bool), ChainError> {
        let mut listed = Vec::new();
        let mut seen = BTreeSet::new();
        for page in 1..=MAX_PAGES {
            let params = json!({"ownerAddress": rpc::text(owner), "page": page, "limit": PAGE});
            let reply = self
                .rpc
                .call_with(Some(Feature::Nfts), "getAssetsByOwner", params)
                .await?;
            let (found, full) = page_of(&reply).ok_or_else(ChainError::malformed)?;
            listed.extend(found.into_iter().filter(|item| seen.insert(*item)));
            if !full {
                let capped = listed.len() > MAX_NFTS;
                listed.truncate(MAX_NFTS);
                return Ok((listed, capped));
            }
        }
        listed.truncate(MAX_NFTS);
        Ok((listed, true))
    }

    async fn all_accounts(&self, keys: &[Key]) -> Result<Vec<Option<Account>>, ChainError> {
        let mut found = Vec::with_capacity(keys.len());
        for chunk in keys.chunks(MAX_KEYS) {
            found.extend(self.accounts(chunk, None).await?);
        }
        Ok(found)
    }

    async fn token_nfts(&self, owner: &Key, mints: &[Key]) -> Result<Vec<Nft>, ChainError> {
        let mut keys = Vec::with_capacity(mints.len() * 3);
        for mint in mints {
            keys.extend([*mint, token_account(owner, mint)?, metadata_account(mint)?]);
        }
        let found = self.all_accounts(&keys).await?;
        let mut held_nfts = Vec::new();
        for (mint, accounts) in mints.iter().zip(found.chunks(3)) {
            let [mint_account, account, record] = accounts else {
                return Err(ChainError::malformed());
            };
            if !one_of_one(mint_account.as_ref()) || held(account.as_ref(), owner, mint)? != 1 {
                continue;
            }
            let Some(record) = metaplex::metadata(record.as_ref(), mint) else {
                continue;
            };
            let kind = match record.standard {
                None | Some(0 | 3) => NftKind::SolanaToken,
                Some(4 | 5) => NftKind::SolanaProgrammable,
                Some(_) => continue,
            };
            let lock =
                (kind == NftKind::SolanaToken && frozen(account.as_ref())).then_some(Lock::Frozen);
            held_nfts.push((*mint, kind, lock, record));
        }
        let mut collections: Vec<Key> = held_nfts
            .iter()
            .filter_map(|(.., record)| {
                record
                    .collection
                    .filter(|(verified, _)| *verified)
                    .map(|(_, key)| key)
            })
            .collect();
        collections.sort_unstable();
        collections.dedup();
        let records = collections
            .iter()
            .map(metadata_account)
            .collect::<Result<Vec<_>, _>>()?;
        let programmable: Vec<usize> = (0..held_nfts.len())
            .filter(|index| held_nfts[*index].1 == NftKind::SolanaProgrammable)
            .collect();
        let states = programmable
            .iter()
            .map(|index| {
                let mint = &held_nfts[*index].0;
                token_record(mint, &token_account(owner, mint)?)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let (named, states) =
            try_join(self.all_accounts(&records), self.all_accounts(&states)).await?;
        for (index, state) in programmable.into_iter().zip(&states) {
            held_nfts[index].2 = metaplex::token_lock(state.as_ref());
        }
        let name_of = |key: &Key| {
            let index = collections.binary_search(key).ok()?;
            metaplex::metadata(named.get(index)?.as_ref(), key)
                .and_then(|record| text(&record.name))
        };
        held_nfts
            .into_iter()
            .map(|(mint, kind, lock, record)| {
                let collection =
                    record
                        .collection
                        .filter(|(verified, _)| *verified)
                        .map(|(_, key)| Collection {
                            address: Some(Address::Solana(key)),
                            name: name_of(&key),
                        });
                Ok(Nft {
                    collection,
                    lock,
                    ..self.nft(mint, kind, &record.name, &record.uri)?
                })
            })
            .collect()
    }

    async fn core_nfts(&self, owner: &Key, assets: &[Key]) -> Result<Vec<Nft>, ChainError> {
        let found = self.all_accounts(assets).await?;
        let held_assets: Vec<(Key, metaplex::Core)> = assets
            .iter()
            .zip(&found)
            .filter_map(|(id, account)| metaplex::core(account.as_ref()).map(|core| (*id, core)))
            .filter(|(_, core)| core.owner == *owner)
            .collect();
        let mut collections: Vec<Key> = held_assets
            .iter()
            .filter_map(|(_, core)| core.collection)
            .collect();
        collections.sort_unstable();
        collections.dedup();
        let named = self.all_accounts(&collections).await?;
        held_assets
            .into_iter()
            .map(|(id, core)| {
                let (collection, trust) = match core.collection {
                    None => (None, Trust::Unknown),
                    Some(key) => {
                        let index = collections
                            .binary_search(&key)
                            .map_err(|_| ChainError::malformed())?;
                        match metaplex::core_collection(named.get(index).and_then(Option::as_ref)) {
                            Some(name) => (
                                Some(Collection {
                                    address: Some(Address::Solana(key)),
                                    name: text(&name),
                                }),
                                Trust::Unknown,
                            ),
                            None => (None, Trust::Flagged),
                        }
                    }
                };
                Ok(Nft {
                    collection,
                    trust,
                    ..self.nft(id, NftKind::SolanaCore, &core.name, &core.uri)?
                })
            })
            .collect()
    }

    fn nft(&self, id: Key, kind: NftKind, name: &str, uri: &str) -> Result<Nft, ChainError> {
        Ok(Nft {
            asset: AssetId::Nft(
                self.network.id.clone(),
                Box::new(
                    NftId::new(Address::Solana(id), None).map_err(|_| ChainError::malformed())?,
                ),
            ),
            kind,
            amount: Amount::new(U256::from(1u8), 0).map_err(|_| ChainError::overflow())?,
            collection: None,
            name: text(name),
            link: (!uri.is_empty() && is_plain_text(uri, MAX_NFT_LINK)).then(|| uri.to_owned()),
            trust: Trust::Unknown,
            lock: None,
        })
    }
}

fn page_of(reply: &Value) -> Option<(Vec<Listed>, bool)> {
    let items = reply["items"].as_array()?;
    let mut listed = Vec::new();
    for item in items {
        let id = rpc::key(&item["id"]).ok()?;
        let compressed = item["compression"]["compressed"].as_bool().unwrap_or(false);
        if compressed || item["burnt"].as_bool() == Some(true) {
            continue;
        }
        let interface = item["interface"].as_str()?;
        if interface == CORE_INTERFACE {
            listed.push(Listed::Core(id));
        } else if TOKEN_INTERFACES.contains(&interface)
            && rpc::key(&item["token_info"]["token_program"]).ok() == Some(TOKEN)
        {
            listed.push(Listed::Token(id));
        }
    }
    Some((listed, items.len() >= PAGE))
}

fn one_of_one(mint: Option<&Account>) -> bool {
    mint.filter(|account| account.owner == TOKEN && account.data.len() >= MINT_SIZE)
        .is_some_and(|account| {
            account.data[36..44] == 1u64.to_le_bytes()
                && account.data[44] == 0
                && account.data[45] == 1
        })
}

fn frozen(account: Option<&Account>) -> bool {
    account.is_some_and(|account| account.data.get(108) == Some(&FROZEN))
}

fn text(raw: &str) -> Option<String> {
    let clean = to_plain_text(raw, MAX_NFT_TEXT);
    (!clean.is_empty()).then_some(clean)
}

#[cfg(fuzzing)]
pub(crate) fn fuzz(value: &Value) {
    let _ = page_of(value);
}
