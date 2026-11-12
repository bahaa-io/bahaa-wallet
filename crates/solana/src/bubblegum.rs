use bahaa_types::Lock;
use sha3::{Digest, Keccak256};

use crate::programs::{MPL_COMPRESSION, SPL_COMPRESSION};
use crate::tx::Key;

pub(crate) const HEAD: usize = 80;
pub(crate) const MAX_DEPTH: usize = 30;
const HEADER: u64 = 56;
const COUNTERS: u64 = 24;
const TREE_ACCOUNT: u8 = 1;
const HEADER_V1: u8 = 0;
const MAX_BUFFER: u64 = 2_048;
const NODE: u64 = 32;
const ENTRY: u64 = NODE + 8;
const FROZEN: u8 = 0b011;
const BOUND: u8 = 0b100;
const ORIGINAL_TOKEN: u8 = 0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct V2 {
    pub collection: Option<Key>,
    pub asset_data_hash: Key,
    pub flags: u8,
}

impl V2 {
    pub(crate) fn lock(&self) -> Option<Lock> {
        if self.flags & BOUND != 0 {
            Some(Lock::Soulbound)
        } else if self.flags & FROZEN != 0 {
            Some(Lock::Frozen)
        } else {
            None
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Leaf {
    pub tree: Key,
    pub owner: Key,
    pub delegate: Key,
    pub nonce: u64,
    pub index: u32,
    pub data_hash: Key,
    pub creator_hash: Key,
    pub v2: Option<V2>,
}

impl Leaf {
    pub(crate) fn hash(&self, id: &Key) -> Key {
        let version = [if self.v2.is_some() { 2 } else { 1 }];
        let nonce = self.nonce.to_le_bytes();
        let mut parts: Vec<&[u8]> = vec![
            &version,
            id,
            &self.owner,
            &self.delegate,
            &nonce,
            &self.data_hash,
            &self.creator_hash,
        ];
        let collection;
        if let Some(v2) = &self.v2 {
            collection = keccak(&[&v2.collection.unwrap_or_default()]);
            parts.extend([
                collection.as_slice(),
                &v2.asset_data_hash,
                std::slice::from_ref(&v2.flags),
            ]);
        }
        keccak(&parts)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Compressed {
    pub leaf: Leaf,
    pub root: Key,
    pub proof: Vec<Key>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Args {
    pub name: String,
    pub symbol: String,
    pub uri: String,
    pub fee: u16,
    pub primary: bool,
    pub mutable: bool,
    pub edition: Option<u8>,
    pub standard: Option<u8>,
    pub collection: Option<(bool, Key)>,
    pub uses: Option<(u8, u64, u64)>,
    pub creators: Vec<(Key, bool, u8)>,
}

impl Args {
    pub(crate) fn hash(&self, v2: bool) -> Key {
        let mut out = Vec::new();
        for text in [&self.name, &self.symbol, &self.uri] {
            out.extend(u32::try_from(text.len()).unwrap_or(u32::MAX).to_le_bytes());
            out.extend(text.as_bytes());
        }
        out.extend(self.fee.to_le_bytes());
        out.extend([u8::from(self.primary), u8::from(self.mutable)]);
        if v2 {
            optional(&mut out, self.standard.map(|standard| vec![standard]));
            self.creators_into(&mut out);
            optional(&mut out, self.collection.map(|(_, key)| key.to_vec()));
        } else {
            optional(&mut out, self.edition.map(|edition| vec![edition]));
            optional(&mut out, self.standard.map(|standard| vec![standard]));
            optional(
                &mut out,
                self.collection.map(|(verified, key)| {
                    let mut collection = vec![u8::from(verified)];
                    collection.extend(key);
                    collection
                }),
            );
            optional(
                &mut out,
                self.uses.map(|(method, remaining, total)| {
                    let mut uses = vec![method];
                    uses.extend(remaining.to_le_bytes());
                    uses.extend(total.to_le_bytes());
                    uses
                }),
            );
            out.push(ORIGINAL_TOKEN);
            self.creators_into(&mut out);
        }
        keccak(&[&keccak(&[&out]), &self.fee.to_le_bytes()])
    }

    fn creators_into(&self, out: &mut Vec<u8>) {
        out.extend(
            u32::try_from(self.creators.len())
                .unwrap_or(u32::MAX)
                .to_le_bytes(),
        );
        for (address, verified, share) in &self.creators {
            out.extend(address);
            out.extend([u8::from(*verified), *share]);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Tree {
    pub v2: bool,
    pub depth: u32,
    pub canopy: u32,
    buffer: u64,
    active: u64,
    filled: u64,
}

impl Tree {
    pub(crate) fn read(owner: &Key, head: &[u8], space: u64) -> Option<Self> {
        let v2 = match *owner {
            SPL_COMPRESSION => false,
            MPL_COMPRESSION => true,
            _ => return None,
        };
        let head = head.get(..HEAD)?;
        if head[0] != TREE_ACCOUNT || head[1] != HEADER_V1 {
            return None;
        }
        let word = |at: usize| Some(u32::from_le_bytes(head.get(at..at + 4)?.try_into().ok()?));
        let long = |at: usize| Some(u64::from_le_bytes(head.get(at..at + 8)?.try_into().ok()?));
        let (buffer, depth) = (u64::from(word(2)?), word(6)?);
        let (active, filled) = (long(64)?, long(72)?);
        if !(1..=MAX_DEPTH).contains(&usize::try_from(depth).ok()?)
            || !buffer.is_power_of_two()
            || buffer > MAX_BUFFER
            || active >= buffer
            || filled == 0
            || filled > buffer
        {
            return None;
        }
        let mut tree = Self {
            v2,
            depth,
            canopy: 0,
            buffer,
            active,
            filled,
        };
        let used = HEADER + COUNTERS + (buffer + 1) * tree.stride();
        let cached = space.checked_sub(used)?;
        let nodes = (cached % NODE == 0).then_some(cached / NODE + 2)?;
        tree.canopy = nodes
            .is_power_of_two()
            .then(|| nodes.trailing_zeros() - 1)
            .filter(|canopy| *canopy <= depth)?;
        Some(tree)
    }

    fn stride(&self) -> u64 {
        ENTRY + NODE * u64::from(self.depth)
    }

    pub(crate) fn root_at(&self) -> Option<usize> {
        usize::try_from(HEADER + COUNTERS + self.active * self.stride()).ok()
    }

    pub(crate) fn changes(&self) -> Option<(usize, usize)> {
        Some((HEAD, usize::try_from(self.buffer * self.stride()).ok()?))
    }

    pub(crate) fn recent(&self, changes: &[u8]) -> Vec<Key> {
        let stride = self.stride();
        (0..self.filled)
            .filter_map(|back| {
                let slot = self.active.wrapping_sub(back) & (self.buffer - 1);
                let at = usize::try_from(slot * stride).ok()?;
                changes.get(at..at + 32)?.try_into().ok()
            })
            .collect()
    }

    pub(crate) fn needed(&self) -> usize {
        usize::try_from(self.depth - self.canopy).unwrap_or(MAX_DEPTH)
    }
}

pub(crate) fn root(leaf: Key, proof: &[Key], index: u32) -> Key {
    proof
        .iter()
        .enumerate()
        .fold(leaf, |node, (depth, sibling)| {
            let right = u32::try_from(depth)
                .ok()
                .and_then(|depth| index.checked_shr(depth))
                .is_some_and(|bits| bits & 1 == 1);
            if right {
                keccak(&[sibling, &node])
            } else {
                keccak(&[&node, sibling])
            }
        })
}

fn optional(out: &mut Vec<u8>, value: Option<Vec<u8>>) {
    match value {
        Some(value) => {
            out.push(1);
            out.extend(value);
        }
        None => out.push(0),
    }
}

fn keccak(parts: &[&[u8]]) -> Key {
    let mut hash = Keccak256::new();
    for part in parts {
        hash.update(part);
    }
    hash.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(depth: u32, buffer: u32, active: u64, filled: u64) -> Vec<u8> {
        let mut head = vec![TREE_ACCOUNT, HEADER_V1];
        head.extend(buffer.to_le_bytes());
        head.extend(depth.to_le_bytes());
        head.resize(64, 0);
        head.extend(active.to_le_bytes());
        head.extend(filled.to_le_bytes());
        head
    }

    fn space(depth: u64, buffer: u64, canopy: u32) -> u64 {
        let stride = 40 + 32 * depth;
        80 + (buffer + 1) * stride + 32 * ((2u64 << canopy) - 2)
    }

    #[test]
    fn a_tree_header_gives_its_depth_and_the_canopy_from_its_size() {
        for canopy in [0, 1, 5, 14] {
            let tree =
                Tree::read(&SPL_COMPRESSION, &head(14, 64, 3, 9), space(14, 64, canopy)).unwrap();
            assert_eq!((tree.v2, tree.depth, tree.canopy), (false, 14, canopy));
            assert_eq!(tree.needed(), 14 - usize::try_from(canopy).unwrap());
            assert_eq!(tree.root_at(), Some(80 + 3 * 488));
            assert_eq!(tree.changes(), Some((80, 64 * 488)));
        }
        let tree = Tree::read(
            &MPL_COMPRESSION,
            &head(20, 2_048, 0, 1),
            space(20, 2_048, 0),
        );
        assert!(tree.unwrap().v2);
    }

    #[test]
    fn anything_but_a_compression_tree_is_refused() {
        let fine = head(14, 64, 3, 9);
        let size = space(14, 64, 2);
        assert!(Tree::read(&SPL_COMPRESSION, &fine, size).is_some());
        assert!(Tree::read(&[7; 32], &fine, size).is_none());
        assert!(Tree::read(&SPL_COMPRESSION, &fine[..79], size).is_none());
        for bad in [size - 32, size + 1, size + 32, 100] {
            assert!(Tree::read(&SPL_COMPRESSION, &fine, bad).is_none(), "{bad}");
        }
        for (edit, at) in [(0, 0), (1, 1)] {
            let mut broken = fine.clone();
            broken[at] = edit + 2;
            assert!(Tree::read(&SPL_COMPRESSION, &broken, size).is_none());
        }
        for (depth, buffer, active, filled) in [
            (0, 64, 0, 1),
            (31, 64, 0, 1),
            (14, 48, 0, 1),
            (14, 4_096, 0, 1),
            (14, 64, 64, 1),
            (14, 64, 0, 0),
            (14, 64, 0, 65),
        ] {
            let header = head(depth, buffer, active, filled);
            let size = space(u64::from(depth), u64::from(buffer), 0);
            assert!(
                Tree::read(&SPL_COMPRESSION, &header, size).is_none(),
                "{depth} {buffer} {active} {filled}"
            );
        }
        assert!(Tree::read(&SPL_COMPRESSION, &head(3, 8, 0, 1), space(3, 8, 4)).is_none());
    }

    #[test]
    fn recent_roots_are_read_newest_first_around_the_ring() {
        let tree = Tree::read(&SPL_COMPRESSION, &head(3, 8, 1, 3), space(3, 8, 0)).unwrap();
        let stride = 40 + 32 * 3;
        let mut changes = vec![0; 8 * stride];
        for slot in 0..8 {
            changes[slot * stride] = u8::try_from(slot).unwrap() + 1;
        }
        let firsts: Vec<u8> = tree.recent(&changes).iter().map(|root| root[0]).collect();
        assert_eq!(firsts, [2, 1, 8]);
        assert_eq!(tree.recent(&changes[..stride]).len(), 1);
    }

    #[test]
    fn version_2_flags_lock_frozen_and_bound_items() {
        let with = |flags| {
            V2 {
                collection: None,
                asset_data_hash: [0; 32],
                flags,
            }
            .lock()
        };
        assert_eq!(with(0), None);
        assert_eq!(with(0b1000), None);
        assert_eq!(with(0b001), Some(Lock::Frozen));
        assert_eq!(with(0b010), Some(Lock::Frozen));
        assert_eq!(with(0b111), Some(Lock::Soulbound));
    }

    #[test]
    fn proofs_climb_left_or_right_by_the_index_bits() {
        let (leaf, left, right) = ([1; 32], [2; 32], [3; 32]);
        let up = keccak(&[&keccak(&[&left, &leaf]), &right]);
        assert_eq!(root(leaf, &[left, right], 0b01), up);
        assert_ne!(root(leaf, &[left, right], 0b00), up);
    }
}
