use std::collections::{BTreeMap, BTreeSet};

use bahaa_chain::ChainError;
use bahaa_config::Feature;
use bahaa_types::{Address, Collection, Lock, Nft, NftKind, Trust};
use futures_util::future::try_join;
use futures_util::stream::{self, StreamExt, TryStreamExt};
use serde_json::{Value, json};

use super::{MAX_KEYS, core_collection};
use crate::bubblegum::{self, Args, Compressed, Leaf, MAX_DEPTH, Tree, V2};
use crate::chain::SolanaChain;
use crate::programs::{Moved, Shape, asset_id};
use crate::rpc;
use crate::tx::{self, Key};

const PROOFS: usize = 100;
const READS: usize = 8;
const MAX_CHANGES: usize = 1 << 20;
const STAND_IN: Key = [255; 32];
const V1_NFT: &str = "V1_NFT";
const V2_NFT: &str = "MplBubblegumV2";
const NON_FUNGIBLE: u8 = 0;
const STANDARDS: [&str; 4] = [
    "NonFungible",
    "FungibleAsset",
    "Fungible",
    "NonFungibleEdition",
];
const USES: [&str; 3] = ["Burn", "Multiple", "Single"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Claim {
    pub id: Key,
    leaf: Leaf,
    args: Args,
}

struct Proof {
    root: Key,
    nodes: Vec<Key>,
    node: u64,
    tree: Key,
}

impl SolanaChain {
    pub(super) async fn compressed_nfts(
        &self,
        owner: &Key,
        claims: Vec<Claim>,
    ) -> Result<Vec<Nft>, ChainError> {
        if claims.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<Key> = claims.iter().map(|claim| claim.id).collect();
        let proofs = self.proofs(&ids).await?;
        let mut wanted: BTreeMap<Key, BTreeSet<Key>> = BTreeMap::new();
        for proof in proofs.values() {
            wanted.entry(proof.tree).or_default().insert(proof.root);
        }
        let trees = self.trees(wanted.keys().copied().collect()).await?;
        let roots = self.roots(&trees, &wanted).await?;
        let held: Vec<(Claim, Compressed, bool)> = claims
            .into_iter()
            .filter_map(|claim| {
                let proof = proofs.get(&claim.id)?;
                let moving = proven(
                    &claim,
                    owner,
                    proof,
                    trees.get(&proof.tree)?,
                    roots.get(&proof.tree)?,
                )?;
                let described = described(&claim);
                Some((claim, moving, described))
            })
            .collect();
        let mut records = BTreeSet::new();
        let mut cores = BTreeSet::new();
        for (claim, moving, described) in &held {
            match &moving.leaf.v2 {
                Some(v2) => cores.extend(v2.collection),
                None if *described => records.extend(verified(&claim.args)),
                None => {}
            }
        }
        let (records, cores) = try_join(self.record_names(records), self.core_names(cores)).await?;
        held.into_iter()
            .map(|(claim, moving, described)| {
                let (collection, trust) = match &moving.leaf.v2 {
                    Some(v2) => core_collection(v2.collection, &cores),
                    None => (
                        verified(&claim.args)
                            .filter(|_| described)
                            .map(|key| Collection {
                                address: Some(Address::Solana(key)),
                                name: records.get(&key).cloned().flatten(),
                            }),
                        Trust::Unknown,
                    ),
                };
                let (name, uri) = if described {
                    (claim.args.name.as_str(), claim.args.uri.as_str())
                } else {
                    ("", "")
                };
                Ok(Nft {
                    collection,
                    trust: if described { trust } else { Trust::Flagged },
                    lock: moving
                        .leaf
                        .v2
                        .and_then(|v2| v2.lock())
                        .or_else(|| (!fits(owner, &moving)).then_some(Lock::Unsupported)),
                    ..self.nft(claim.id, NftKind::SolanaCompressed, name, uri)?
                })
            })
            .collect()
    }

    pub(super) async fn compressed_move(&self, owner: &Key, id: &Key) -> Result<Moved, ChainError> {
        let not_held = || ChainError::Invalid("this wallet doesn't hold that NFT");
        let asked = |method: &'static str, params: Value| async move {
            match self
                .rpc
                .call_with(Some(Feature::Nfts), method, params)
                .await
            {
                Err(ChainError::Rejected(_)) => Ok(Value::Null),
                other => other,
            }
        };
        let (asset, proof) = try_join(
            asked(
                "getAsset",
                json!({"id": rpc::text(id), "options": {"showUnverifiedCollections": true}}),
            ),
            asked("getAssetProof", json!({"id": rpc::text(id)})),
        )
        .await?;
        let claim = claim(&asset)
            .filter(|claim| claim.id == *id)
            .ok_or_else(not_held)?;
        let proof = proof_of(&proof).ok_or_else(not_held)?;
        let tree = *self
            .trees(vec![proof.tree])
            .await?
            .get(&proof.tree)
            .ok_or_else(not_held)?;
        let roots = self
            .recent(&proof.tree, &tree, &BTreeSet::from([proof.root]))
            .await?;
        let moving = proven(&claim, owner, &proof, &tree, &roots).ok_or_else(not_held)?;
        match moving.leaf.v2.and_then(|v2| v2.lock()) {
            Some(Lock::Soulbound) => {
                return Err(ChainError::Invalid(
                    "this NFT is bound to its owner and can't be sent",
                ));
            }
            Some(_) => return Err(ChainError::Invalid("this NFT is frozen and can't be sent")),
            None => {}
        }
        if !fits(owner, &moving) {
            return Err(ChainError::Invalid(
                "this NFT needs a bigger transaction than this wallet can send",
            ));
        }
        Ok(Moved::Compressed(Box::new(moving)))
    }

    async fn proofs(&self, ids: &[Key]) -> Result<BTreeMap<Key, Proof>, ChainError> {
        let mut found = BTreeMap::new();
        for chunk in ids.chunks(PROOFS) {
            let asked: Vec<String> = chunk.iter().map(rpc::text).collect();
            let reply = self
                .rpc
                .call_with(
                    Some(Feature::Nfts),
                    "getAssetProofBatch",
                    json!({"ids": asked}),
                )
                .await?;
            let listed = reply.as_object().ok_or_else(ChainError::malformed)?;
            found.extend(
                chunk
                    .iter()
                    .zip(&asked)
                    .filter_map(|(id, text)| Some((*id, proof_of(listed.get(text)?)?))),
            );
        }
        Ok(found)
    }

    async fn trees(&self, keys: Vec<Key>) -> Result<BTreeMap<Key, Tree>, ChainError> {
        let mut found = BTreeMap::new();
        for chunk in keys.chunks(MAX_KEYS) {
            let accounts = self.accounts(chunk, Some(bubblegum::HEAD)).await?;
            found.extend(chunk.iter().zip(accounts).filter_map(|(key, account)| {
                let account = account?;
                Some((
                    *key,
                    Tree::read(&account.owner, &account.data, account.space)?,
                ))
            }));
        }
        Ok(found)
    }

    async fn roots(
        &self,
        trees: &BTreeMap<Key, Tree>,
        wanted: &BTreeMap<Key, BTreeSet<Key>>,
    ) -> Result<BTreeMap<Key, Vec<Key>>, ChainError> {
        let reads: Vec<_> = wanted
            .iter()
            .filter_map(|(key, roots)| {
                let tree = trees.get(key)?;
                Some(async move { Ok((*key, self.recent(key, tree, roots).await?)) })
            })
            .collect();
        stream::iter(reads)
            .buffer_unordered(READS)
            .try_collect()
            .await
    }

    async fn recent(
        &self,
        key: &Key,
        tree: &Tree,
        wanted: &BTreeSet<Key>,
    ) -> Result<Vec<Key>, ChainError> {
        let at = tree.root_at().ok_or_else(ChainError::overflow)?;
        let Some(current) = self.slice(key, at, 32).await? else {
            return Ok(Vec::new());
        };
        let current: Key = current.try_into().map_err(|_| ChainError::malformed())?;
        if wanted.iter().all(|root| *root == current) {
            return Ok(vec![current]);
        }
        match tree.changes() {
            Some((offset, length)) if length <= MAX_CHANGES => Ok(self
                .slice(key, offset, length)
                .await?
                .map(|changes| tree.recent(&changes))
                .unwrap_or_default()),
            _ => Ok(vec![current]),
        }
    }
}

pub(super) fn claim(item: &Value) -> Option<Claim> {
    let key = |value: &Value| rpc::key(value).ok();
    let zipped = &item["compression"];
    if zipped["compressed"].as_bool() != Some(true) {
        return None;
    }
    let owner = key(&item["ownership"]["owner"])?;
    let delegate = match &item["ownership"]["delegate"] {
        Value::Null => owner,
        value => key(value)?,
    };
    let collection = match item["grouping"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|group| group["group_key"] == "collection")
    {
        Some(group) => Some((
            group["verified"].as_bool().unwrap_or(true),
            key(&group["group_value"])?,
        )),
        None => None,
    };
    let v2 = match item["interface"].as_str()? {
        V1_NFT => None,
        V2_NFT => Some(V2 {
            collection: collection.map(|(_, key)| key),
            asset_data_hash: key(&zipped["asset_data_hash"])?,
            flags: u8::try_from(zipped["flags"].as_u64()?).ok()?,
        }),
        _ => return None,
    };
    Some(Claim {
        id: key(&item["id"])?,
        leaf: Leaf {
            tree: key(&zipped["tree"])?,
            owner,
            delegate,
            nonce: zipped["leaf_id"].as_u64()?,
            index: 0,
            data_hash: key(&zipped["data_hash"])?,
            creator_hash: key(&zipped["creator_hash"])?,
            v2,
        },
        args: args(item, collection)?,
    })
}

fn args(item: &Value, collection: Option<(bool, Key)>) -> Option<Args> {
    let text = |value: &Value| match value {
        Value::Null => Some(String::new()),
        Value::String(text) => Some(text.clone()),
        _ => None,
    };
    let named = |value: &Value, names: &[&str]| {
        names
            .iter()
            .position(|name| value.as_str() == Some(*name))
            .and_then(|index| u8::try_from(index).ok())
    };
    let small = |value: &Value| value.as_u64().and_then(|value| u8::try_from(value).ok());
    let metadata = &item["content"]["metadata"];
    let edition = match &item["supply"]["edition_nonce"] {
        Value::Null => None,
        value => Some(small(value)?),
    };
    let standard = match &metadata["token_standard"] {
        Value::Null => None,
        value => Some(named(value, &STANDARDS)?),
    };
    let uses = match &item["uses"] {
        Value::Null => None,
        uses => Some((
            named(&uses["use_method"], &USES)?,
            uses["remaining"].as_u64()?,
            uses["total"].as_u64()?,
        )),
    };
    let creators = item["creators"]
        .as_array()?
        .iter()
        .map(|creator| {
            Some((
                rpc::key(&creator["address"]).ok()?,
                creator["verified"].as_bool()?,
                small(&creator["share"])?,
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    Some(Args {
        name: text(&metadata["name"])?,
        symbol: text(&metadata["symbol"])?,
        uri: text(&item["content"]["json_uri"])?,
        fee: u16::try_from(item["royalty"]["basis_points"].as_u64()?).ok()?,
        primary: item["royalty"]["primary_sale_happened"].as_bool()?,
        mutable: item["mutable"].as_bool()?,
        edition,
        standard,
        collection,
        uses,
        creators,
    })
}

fn proof_of(value: &Value) -> Option<Proof> {
    let key = |value: &Value| rpc::key(value).ok();
    let nodes = value["proof"]
        .as_array()
        .filter(|nodes| nodes.len() <= MAX_DEPTH)?
        .iter()
        .map(key)
        .collect::<Option<Vec<_>>>()?;
    Some(Proof {
        root: key(&value["root"])?,
        nodes,
        node: value["node_index"].as_u64()?,
        tree: key(&value["tree_id"])?,
    })
}

fn proven(
    claim: &Claim,
    owner: &Key,
    proof: &Proof,
    tree: &Tree,
    roots: &[Key],
) -> Option<Compressed> {
    if claim.leaf.owner != *owner
        || proof.tree != claim.leaf.tree
        || tree.v2 != claim.leaf.v2.is_some()
        || proof.nodes.len() != usize::try_from(tree.depth).ok()?
        || !roots.contains(&proof.root)
        || asset_id(&proof.tree, claim.leaf.nonce).ok()? != claim.id
    {
        return None;
    }
    let width = 1u64 << tree.depth;
    let index = proof
        .node
        .checked_sub(width)
        .filter(|index| *index < width)
        .and_then(|index| u32::try_from(index).ok())?;
    let leaf = Leaf {
        index,
        ..claim.leaf.clone()
    };
    (bubblegum::root(leaf.hash(&claim.id), &proof.nodes, index) == proof.root).then_some(())?;
    Some(Compressed {
        leaf,
        root: proof.root,
        proof: proof.nodes.get(..tree.needed())?.to_vec(),
    })
}

fn described(claim: &Claim) -> bool {
    let v2 = claim.leaf.v2.is_some();
    claim.args.hash(v2) == claim.leaf.data_hash
        || (claim.args.standard == Some(NON_FUNGIBLE)
            && Args {
                standard: None,
                ..claim.args.clone()
            }
            .hash(v2)
                == claim.leaf.data_hash)
}

fn verified(args: &Args) -> Option<Key> {
    args.collection
        .filter(|(verified, _)| *verified)
        .map(|(_, key)| key)
}

fn fits(owner: &Key, moving: &Compressed) -> bool {
    let shape = Shape {
        from: *owner,
        to: STAND_IN,
        moved: Moved::Compressed(Box::new(moving.clone())),
        units: 1,
        memo: None,
        limit: 0,
        price: 0,
    };
    shape
        .instructions()
        .and_then(|list| tx::compile(*owner, STAND_IN, &list))
        .is_ok()
}

#[cfg(fuzzing)]
pub(super) fn fuzz(value: &Value) {
    if let Some(claim) = claim(value) {
        let _ = described(&claim);
    }
    if let Some(proof) = proof_of(value) {
        let index = u32::try_from(proof.node).unwrap_or(u32::MAX);
        let _ = bubblegum::root(proof.root, &proof.nodes, index);
    }
}

#[cfg(test)]
mod tests {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;

    use super::*;

    fn fixture() -> Value {
        serde_json::from_str(include_str!("../../../tests/fixtures/compressed.json")).unwrap()
    }

    fn tree(fixture: &Value, key: &str) -> (Tree, Key) {
        let tree = &fixture["trees"][key];
        let head = STANDARD.decode(tree["head"].as_str().unwrap()).unwrap();
        let root = STANDARD.decode(tree["root"].as_str().unwrap()).unwrap();
        (
            Tree::read(
                &rpc::key(&tree["owner"]).unwrap(),
                &head,
                tree["space"].as_u64().unwrap(),
            )
            .unwrap(),
            root.try_into().unwrap(),
        )
    }

    #[test]
    fn listed_leaves_rebuild_the_indexed_leaf_and_reach_the_root_on_chain() {
        let fixture = fixture();
        let owner = rpc::key(&fixture["owner"]).unwrap();
        let mut summary = Vec::new();
        for item in fixture["assets"]["items"].as_array().unwrap() {
            let claim = claim(item).unwrap();
            let id = rpc::text(&claim.id);
            let proof = proof_of(&fixture["proofs"][&id]).unwrap();
            let (tree, root) = tree(&fixture, &rpc::text(&proof.tree));
            let moving = proven(&claim, &owner, &proof, &tree, &[root]).unwrap();
            assert_eq!(
                moving.leaf.hash(&claim.id),
                rpc::key(&item["compression"]["asset_hash"]).unwrap(),
                "{id}"
            );
            assert!(proven(&claim, &owner, &proof, &tree, &[[0; 32]]).is_none());
            assert!(proven(&claim, &[3; 32], &proof, &tree, &[root]).is_none());
            summary.push((
                moving.leaf.v2.is_some(),
                moving.proof.len(),
                described(&claim),
                fits(&owner, &moving),
            ));
        }
        assert_eq!(
            summary,
            [
                (false, 7, true, true),
                (true, 6, true, true),
                (false, 24, true, false),
                (false, 7, true, true),
                (false, 14, true, true),
                (false, 24, false, false),
            ]
        );
    }

    #[test]
    fn a_leaf_with_any_other_field_misses_the_root() {
        let fixture = fixture();
        let owner = rpc::key(&fixture["owner"]).unwrap();
        for item in fixture["assets"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .take(2)
        {
            let good = claim(item).unwrap();
            let proof = proof_of(&fixture["proofs"][&rpc::text(&good.id)]).unwrap();
            let (tree, root) = tree(&fixture, &rpc::text(&proof.tree));
            let mut edits: Vec<fn(&mut Claim)> = vec![
                |claim| claim.leaf.delegate = [9; 32],
                |claim| claim.leaf.nonce += 1,
                |claim| claim.leaf.data_hash[0] ^= 1,
                |claim| claim.leaf.creator_hash[0] ^= 1,
                |claim| claim.id = [9; 32],
                |claim| claim.leaf.tree = [9; 32],
            ];
            if good.leaf.v2.is_some() {
                edits.push(|claim| {
                    if let Some(v2) = &mut claim.leaf.v2 {
                        v2.flags = 1;
                    }
                });
                edits.push(|claim| {
                    if let Some(v2) = &mut claim.leaf.v2 {
                        v2.collection = None;
                    }
                });
            }
            for edit in edits {
                let mut claim = good.clone();
                edit(&mut claim);
                assert!(proven(&claim, &owner, &proof, &tree, &[root]).is_none());
            }
        }
    }

    #[test]
    fn only_the_shown_texts_and_collection_hash_to_the_leaf() {
        let fixture = fixture();
        let good = claim(&fixture["assets"]["items"][0]).unwrap();
        assert!(described(&good));
        let mut renamed = good.clone();
        renamed.args.name.push('!');
        assert!(!described(&renamed));
        let mut moved = good.clone();
        moved.args.collection = Some((true, [9; 32]));
        assert!(!described(&moved));
        let mut unverified = good;
        unverified.args.collection = unverified.args.collection.map(|(_, key)| (false, key));
        assert!(!described(&unverified));
    }
}
