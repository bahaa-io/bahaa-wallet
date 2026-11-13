use std::collections::{BTreeMap, BTreeSet};
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

use super::{SolanaChain, bytes};
use crate::metaplex;
use crate::programs::{Moved, TOKEN, metadata_account, token_account, token_record};
use crate::rpc::{self, Account};
use crate::token::held;
use crate::tx::Key;

mod compressed;

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

enum Listed {
    Token(Key),
    Core(Key),
    Compressed(Box<compressed::Claim>),
}

impl Listed {
    fn id(&self) -> Key {
        match self {
            Self::Token(id) | Self::Core(id) => *id,
            Self::Compressed(claim) => claim.id,
        }
    }
}

impl SolanaChain {
    pub async fn nfts(&self, owner: &Address) -> Result<Found, ChainError> {
        let owner = bytes(owner)?;
        let (listed, capped) = self.listing(&owner).await?;
        let (mut mints, mut assets, mut claims) = (Vec::new(), Vec::new(), Vec::new());
        for item in listed {
            match item {
                Listed::Token(mint) => mints.push(mint),
                Listed::Core(asset) => assets.push(asset),
                Listed::Compressed(claim) => claims.push(*claim),
            }
        }
        let mut items = self.token_nfts(&owner, &mints).await?;
        items.extend(self.core_nfts(&owner, &assets).await?);
        items.extend(self.compressed_nfts(&owner, claims).await?);
        Ok(Found { items, capped })
    }

    pub(super) async fn nft_move(&self, owner: &Key, id: &Key) -> Result<Moved, ChainError> {
        let not_held = ChainError::Invalid("this wallet doesn't hold that NFT");
        let source = token_account(owner, &TOKEN, id)?;
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
        if item.is_none() {
            return self.compressed_move(owner, id).await;
        }
        if let Some(core) = metaplex::core(item.as_ref()) {
            if core.owner != *owner {
                return Err(not_held);
            }
            return Ok(Moved::Core {
                asset: *id,
                collection: core.collection,
            });
        }
        if !one_of_one(item.as_ref()) || held(account.as_ref(), &TOKEN, owner, id)? != 1 {
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
            program: TOKEN,
            fee: None,
        })
    }

    async fn listing(&self, owner: &Key) -> Result<(Vec<Listed>, bool), ChainError> {
        let mut listed = Vec::new();
        let mut seen = BTreeSet::new();
        let mut capped = true;
        for page in 1..=MAX_PAGES {
            let params = json!({
                "ownerAddress": rpc::text(owner),
                "page": page,
                "limit": PAGE,
                "options": {"showUnverifiedCollections": true}
            });
            let reply = self
                .rpc
                .call_with(Some(Feature::Nfts), "getAssetsByOwner", params)
                .await?;
            let (found, full) = page_of(&reply).ok_or_else(ChainError::malformed)?;
            listed.extend(found.into_iter().filter(|item| seen.insert(item.id())));
            if !full {
                capped = false;
                break;
            }
        }
        listed.sort_by_key(|item| matches!(item, Listed::Compressed(_)));
        capped |= listed.len() > MAX_NFTS;
        listed.truncate(MAX_NFTS);
        Ok((listed, capped))
    }

    async fn record_names(
        &self,
        collections: BTreeSet<Key>,
    ) -> Result<BTreeMap<Key, Option<String>>, ChainError> {
        let keys: Vec<Key> = collections.into_iter().collect();
        let records = keys
            .iter()
            .map(metadata_account)
            .collect::<Result<Vec<_>, _>>()?;
        let found = self.all_accounts(&records).await?;
        Ok(keys
            .into_iter()
            .zip(found)
            .map(|(key, record)| {
                let name =
                    metaplex::metadata(record.as_ref(), &key).and_then(|record| text(&record.name));
                (key, name)
            })
            .collect())
    }

    async fn core_names(
        &self,
        collections: BTreeSet<Key>,
    ) -> Result<BTreeMap<Key, String>, ChainError> {
        let keys: Vec<Key> = collections.into_iter().collect();
        let found = self.all_accounts(&keys).await?;
        Ok(keys
            .into_iter()
            .zip(found)
            .filter_map(|(key, account)| Some((key, metaplex::core_collection(account.as_ref())?)))
            .collect())
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
            keys.extend([
                *mint,
                token_account(owner, &TOKEN, mint)?,
                metadata_account(mint)?,
            ]);
        }
        let found = self.all_accounts(&keys).await?;
        let mut held_nfts = Vec::new();
        for (mint, accounts) in mints.iter().zip(found.chunks(3)) {
            let [mint_account, account, record] = accounts else {
                return Err(ChainError::malformed());
            };
            if !one_of_one(mint_account.as_ref())
                || held(account.as_ref(), &TOKEN, owner, mint)? != 1
            {
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
        let collections: BTreeSet<Key> = held_nfts
            .iter()
            .filter_map(|(.., record)| {
                record
                    .collection
                    .filter(|(verified, _)| *verified)
                    .map(|(_, key)| key)
            })
            .collect();
        let programmable: Vec<usize> = (0..held_nfts.len())
            .filter(|index| held_nfts[*index].1 == NftKind::SolanaProgrammable)
            .collect();
        let states = programmable
            .iter()
            .map(|index| {
                let mint = &held_nfts[*index].0;
                token_record(mint, &token_account(owner, &TOKEN, mint)?)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let (named, states) =
            try_join(self.record_names(collections), self.all_accounts(&states)).await?;
        for (index, state) in programmable.into_iter().zip(&states) {
            held_nfts[index].2 = metaplex::token_lock(state.as_ref());
        }
        held_nfts
            .into_iter()
            .map(|(mint, kind, lock, record)| {
                let collection =
                    record
                        .collection
                        .filter(|(verified, _)| *verified)
                        .map(|(_, key)| Collection {
                            address: Some(Address::Solana(key)),
                            name: named.get(&key).cloned().flatten(),
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
        let collections: BTreeSet<Key> = held_assets
            .iter()
            .filter_map(|(_, core)| core.collection)
            .collect();
        let named = self.core_names(collections).await?;
        held_assets
            .into_iter()
            .map(|(id, core)| {
                let (collection, trust) = core_collection(core.collection, &named);
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

fn core_collection(
    collection: Option<Key>,
    named: &BTreeMap<Key, String>,
) -> (Option<Collection>, Trust) {
    let Some(key) = collection else {
        return (None, Trust::Unknown);
    };
    match named.get(&key) {
        Some(name) => (
            Some(Collection {
                address: Some(Address::Solana(key)),
                name: text(name),
            }),
            Trust::Unknown,
        ),
        None => (None, Trust::Flagged),
    }
}

fn page_of(reply: &Value) -> Option<(Vec<Listed>, bool)> {
    let items = reply["items"].as_array()?;
    let mut listed = Vec::new();
    for item in items {
        let id = rpc::key(&item["id"]).ok()?;
        if item["burnt"].as_bool() == Some(true) {
            continue;
        }
        if item["compression"]["compressed"].as_bool() == Some(true) {
            listed.extend(compressed::claim(item).map(|claim| Listed::Compressed(Box::new(claim))));
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
    compressed::fuzz(value);
}
