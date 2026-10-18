use tonlib_core::cell::{ArcCell, BagOfCells, Cell, CellParser};

use crate::tl::BlockId;

const BLOCK_TAG: u32 = 0x11ef_55aa;
const STATE_TAG: u32 = 0x9023_afe2;
const MC_STATE_EXTRA_TAG: u16 = 0xcc26;
const MERKLE_PROOF: u8 = 3;
const MERKLE_UPDATE: u8 = 4;
const MAX_SHARD_DEPTH: u32 = 60;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Found {
    Leaf(ArcCell, usize),
    Absent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Proven {
    Present {
        account: [u8; 32],
        last: ([u8; 32], u64),
    },
    Absent,
}

pub(crate) fn account(
    masterchain: &BlockId,
    shard_block: &BlockId,
    shard_proof: &[u8],
    proof: &[u8],
    workchain: i8,
    hash: &[u8; 32],
) -> Option<Proven> {
    if workchain == -1 {
        if shard_block != masterchain {
            return None;
        }
    } else {
        if shard_block.workchain != i32::from(workchain) {
            return None;
        }
        let (block, state) = roots(shard_proof)?;
        let block = opened(&block, &masterchain.root_hash)?;
        let state = opened(&state, &state_hash(&block)?)?;
        shard_listed(&state, workchain, hash, shard_block)?;
    }
    let (block, state) = roots(proof)?;
    let block = opened(&block, &shard_block.root_hash)?;
    let state = opened(&state, &state_hash(&block)?)?;
    account_in(&state, hash)
}

pub(crate) fn config(
    masterchain: &BlockId,
    state_proof: &[u8],
    config_proof: &[u8],
    params: &[i32],
) -> Option<Vec<ArcCell>> {
    let block = opened(&single(state_proof)?, &masterchain.root_hash)?;
    let state = opened(&single(config_proof)?, &state_hash(&block)?)?;
    let mut parser = shard_state(&state)?;
    if !parser.load_bit().ok()? {
        return None;
    }
    let extra = child(&state, 3)?;
    let mut parser = extra.parser();
    if parser.load_u16(16).ok()? != MC_STATE_EXTRA_TAG {
        return None;
    }
    let shards = parser.load_bit().ok()?;
    let dict = child(&extra, usize::from(shards))?;
    params
        .iter()
        .map(|id| crate::fees::param(&dict, *id))
        .collect()
}

fn single(boc: &[u8]) -> Option<ArcCell> {
    BagOfCells::parse(boc).ok()?.single_root().ok()
}

fn roots(boc: &[u8]) -> Option<(ArcCell, ArcCell)> {
    let bag = BagOfCells::parse(boc).ok()?;
    match bag.roots.as_slice() {
        [block, state] => Some((block.clone(), state.clone())),
        _ => None,
    }
}

fn opened(proof: &ArcCell, expected: &[u8; 32]) -> Option<ArcCell> {
    let data = proof.data();
    if !proof.is_exotic() || data.first() != Some(&MERKLE_PROOF) || data.get(1..33)? != expected {
        return None;
    }
    let inner = proof.reference(0).ok()?.clone();
    (!inner.is_exotic()).then_some(inner)
}

fn state_hash(block: &Cell) -> Option<[u8; 32]> {
    if block.parser().load_u32(32).ok()? != BLOCK_TAG {
        return None;
    }
    let update = block.reference(2).ok()?;
    let data = update.data();
    if !update.is_exotic() || data.first() != Some(&MERKLE_UPDATE) {
        return None;
    }
    data.get(33..65)?.try_into().ok()
}

fn child(cell: &Cell, index: usize) -> Option<ArcCell> {
    let child = cell.reference(index).ok()?.clone();
    (!child.is_exotic()).then_some(child)
}

fn shard_state(state: &Cell) -> Option<CellParser<'_>> {
    let mut parser = state.parser();
    if parser.load_u32(32).ok()? != STATE_TAG {
        return None;
    }
    parser.skip_bits(32 + 104 + 192 + 1).ok()?;
    Some(parser)
}

fn shard_listed(
    masterchain_state: &Cell,
    workchain: i8,
    hash: &[u8; 32],
    shard_block: &BlockId,
) -> Option<()> {
    let mut parser = shard_state(masterchain_state)?;
    if !parser.load_bit().ok()? {
        return None;
    }
    let extra = child(masterchain_state, 3)?;
    let mut parser = extra.parser();
    if parser.load_u16(16).ok()? != MC_STATE_EXTRA_TAG || !parser.load_bit().ok()? {
        return None;
    }
    let key = i32::from(workchain).to_be_bytes();
    let Found::Leaf(leaf, _) = lookup(&child(&extra, 0)?, &key, 32)? else {
        return None;
    };
    let (shard, descr) = bin_tree_leaf(child(&leaf, 0)?, hash)?;
    let mut parser = descr.parser();
    parser.skip_bits(1).ok()?;
    if !matches!(parser.load_u8(4).ok()?, 0xa | 0xb) {
        return None;
    }
    let seqno = parser.load_u32(32).ok()?;
    parser.skip_bits(32 + 64 + 64).ok()?;
    let root_hash = parser.load_bits(256).ok()?;
    let file_hash = parser.load_bits(256).ok()?;
    let listed = u64::from_be_bytes(shard_block.shard.to_be_bytes()) == shard
        && u32::from_be_bytes(shard_block.seqno.to_be_bytes()) == seqno
        && root_hash == shard_block.root_hash
        && file_hash == shard_block.file_hash;
    listed.then_some(())
}

fn bin_tree_leaf(root: ArcCell, hash: &[u8; 32]) -> Option<(u64, ArcCell)> {
    let mut cell = root;
    let mut prefix = 0u64;
    for depth in 0..=MAX_SHARD_DEPTH {
        if !cell.parser().load_bit().ok()? {
            return Some((prefix | 1 << (63 - depth), cell));
        }
        let right = bit(hash, usize::try_from(depth).ok()?);
        if right {
            prefix |= 1 << (63 - depth);
        }
        cell = child(&cell, usize::from(right))?;
    }
    None
}

fn account_in(shard_state_cell: &Cell, hash: &[u8; 32]) -> Option<Proven> {
    shard_state(shard_state_cell)?;
    let accounts = child(shard_state_cell, 1)?;
    if !accounts.parser().load_bit().ok()? {
        return Some(Proven::Absent);
    }
    let Found::Leaf(leaf, label) = lookup(&child(&accounts, 0)?, hash, 256)? else {
        return Some(Proven::Absent);
    };
    let mut parser = leaf.parser();
    parser.skip_bits(label).ok()?;
    parser.load_u8(5).ok()?;
    let length = usize::from(parser.load_u8(4).ok()?);
    parser.skip_bits(length * 8).ok()?;
    let account_ref = usize::from(parser.load_bit().ok()?);
    let account = leaf.reference(account_ref).ok()?;
    let last_hash = parser.load_bits(256).ok()?.try_into().ok()?;
    let last_lt = parser.load_u64(64).ok()?;
    Some(Proven::Present {
        account: account.get_hash(0).as_slice().try_into().ok()?,
        last: (last_hash, last_lt),
    })
}

pub(crate) fn lookup(root: &ArcCell, key: &[u8], bits: usize) -> Option<Found> {
    let mut cell = root.clone();
    let mut done = 0;
    loop {
        if cell.is_exotic() {
            return None;
        }
        let mut parser = cell.parser();
        let label = label(&mut parser, bits - done)?;
        if label
            .iter()
            .enumerate()
            .any(|(at, value)| bit(key, done + at) != *value)
        {
            return Some(Found::Absent);
        }
        done += label.len();
        if done == bits {
            let used = cell.bit_len() - parser.remaining_bits();
            return Some(Found::Leaf(cell, used));
        }
        let right = bit(key, done);
        done += 1;
        cell = cell.reference(usize::from(right)).ok()?.clone();
    }
}

pub(crate) fn label(parser: &mut CellParser<'_>, max: usize) -> Option<Vec<bool>> {
    let width = usize::try_from(max.bit_width()).ok()?;
    if !parser.load_bit().ok()? {
        let mut length = 0;
        while parser.load_bit().ok()? {
            length += 1;
            if length > max {
                return None;
            }
        }
        return (0..length).map(|_| parser.load_bit().ok()).collect();
    }
    if !parser.load_bit().ok()? {
        let length = usize::try_from(parser.load_u32(width).ok()?).ok()?;
        if length > max {
            return None;
        }
        return (0..length).map(|_| parser.load_bit().ok()).collect();
    }
    let value = parser.load_bit().ok()?;
    let length = usize::try_from(parser.load_u32(width).ok()?).ok()?;
    (length <= max).then(|| vec![value; length])
}

fn bit(key: &[u8], at: usize) -> bool {
    key.get(at / 8)
        .is_some_and(|byte| byte >> (7 - at % 8) & 1 == 1)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tonlib_core::cell::CellBuilder;

    use super::*;

    fn leaf_with(label: &[bool], value: u8) -> ArcCell {
        let mut cell = CellBuilder::new();
        cell.store_bit(false).unwrap();
        for _ in label {
            cell.store_bit(true).unwrap();
        }
        cell.store_bit(false).unwrap();
        for bit in label {
            cell.store_bit(*bit).unwrap();
        }
        cell.store_u8(8, value).unwrap();
        cell.build().unwrap().to_arc()
    }

    #[test]
    fn labels_read_in_all_three_forms() {
        let mut short = CellBuilder::new();
        short
            .store_bit(false)
            .unwrap()
            .store_u8(3, 0b110)
            .unwrap()
            .store_u8(2, 0b10)
            .unwrap();
        let short = short.build().unwrap();
        assert_eq!(label(&mut short.parser(), 8), Some(vec![true, false]));
        let mut long = CellBuilder::new();
        long.store_u8(2, 0b10)
            .unwrap()
            .store_u8(4, 3)
            .unwrap()
            .store_u8(3, 0b101)
            .unwrap();
        let long = long.build().unwrap();
        assert_eq!(label(&mut long.parser(), 8), Some(vec![true, false, true]));
        let mut same = CellBuilder::new();
        same.store_u8(2, 0b11)
            .unwrap()
            .store_bit(true)
            .unwrap()
            .store_u32(9, 256)
            .unwrap();
        let same = same.build().unwrap();
        assert_eq!(label(&mut same.parser(), 256), Some(vec![true; 256]));
        let mut too_long = CellBuilder::new();
        too_long
            .store_u8(2, 0b11)
            .unwrap()
            .store_bit(true)
            .unwrap()
            .store_u8(4, 9)
            .unwrap();
        let too_long = too_long.build().unwrap();
        assert_eq!(label(&mut too_long.parser(), 8), None);
    }

    #[test]
    fn lookups_follow_the_key_and_prove_absence() {
        let left = leaf_with(&[false, true], 7);
        let right = leaf_with(&[true, true], 9);
        let mut fork = CellBuilder::new();
        fork.store_bit(false)
            .unwrap()
            .store_bit(true)
            .unwrap()
            .store_bit(false)
            .unwrap()
            .store_bit(true)
            .unwrap();
        fork.store_reference(&left)
            .unwrap()
            .store_reference(&right)
            .unwrap();
        let root = fork.build().unwrap().to_arc();
        let Found::Leaf(found, used) = lookup(&root, &[0b1001_0000], 4).unwrap() else {
            panic!("not found");
        };
        let mut parser = found.parser();
        parser.skip_bits(used).unwrap();
        assert_eq!(parser.load_u8(8).unwrap(), 7);
        let Found::Leaf(found, used) = lookup(&root, &[0b1111_0000], 4).unwrap() else {
            panic!("not found");
        };
        let mut parser = found.parser();
        parser.skip_bits(used).unwrap();
        assert_eq!(parser.load_u8(8).unwrap(), 9);
        assert_eq!(lookup(&root, &[0b0001_0000], 4), Some(Found::Absent));
        assert_eq!(lookup(&root, &[0b1000_0000], 4), Some(Found::Absent));
    }

    fn descr(seqno: u32, root: u8) -> ArcCell {
        let mut cell = CellBuilder::new();
        cell.store_bit(false).unwrap().store_u8(4, 0xb).unwrap();
        cell.store_u32(32, seqno).unwrap().store_u32(32, 1).unwrap();
        cell.store_u64(64, 2).unwrap().store_u64(64, 3).unwrap();
        cell.store_bits(256, &[root; 32]).unwrap();
        cell.store_bits(256, &[root ^ 0xff; 32]).unwrap();
        cell.build().unwrap().to_arc()
    }

    fn masterchain_state() -> Cell {
        let mut tree = CellBuilder::new();
        tree.store_bit(true).unwrap();
        tree.store_reference(&descr(10, 0x0a)).unwrap();
        tree.store_reference(&descr(20, 0x14)).unwrap();
        let tree = tree.build().unwrap().to_arc();
        let mut entry = CellBuilder::new();
        entry
            .store_u8(2, 0b11)
            .unwrap()
            .store_bit(false)
            .unwrap()
            .store_u8(6, 32)
            .unwrap();
        entry.store_reference(&tree).unwrap();
        let entry = entry.build().unwrap().to_arc();
        let mut extra = CellBuilder::new();
        extra
            .store_u16(16, MC_STATE_EXTRA_TAG)
            .unwrap()
            .store_bit(true)
            .unwrap();
        extra.store_reference(&entry).unwrap();
        let extra = extra.build().unwrap().to_arc();
        let empty = CellBuilder::new().build().unwrap().to_arc();
        let mut state = CellBuilder::new();
        state.store_u32(32, STATE_TAG).unwrap();
        state.store_bits(32 + 104 + 192 + 1, &[0; 42]).unwrap();
        state.store_bit(true).unwrap();
        for _ in 0..3 {
            state.store_reference(&empty).unwrap();
        }
        state.store_reference(&extra).unwrap();
        state.build().unwrap()
    }

    #[test]
    fn a_shard_block_counts_only_when_the_masterchain_lists_it_for_this_account() {
        let state = masterchain_state();
        let block = |shard: u64, seqno: i32, root: u8, file: u8| BlockId {
            workchain: 0,
            shard: i64::from_be_bytes(shard.to_be_bytes()),
            seqno,
            root_hash: [root; 32],
            file_hash: [file; 32],
        };
        let (high, low) = ([0xf0; 32], [0x0f; 32]);
        let (left, right) = (0x4000_0000_0000_0000, 0xc000_0000_0000_0000);
        let listed = |workchain: i8, hash: &[u8; 32], shard: &BlockId| {
            shard_listed(&state, workchain, hash, shard)
        };
        assert_eq!(listed(0, &high, &block(right, 20, 0x14, 0xeb)), Some(()));
        assert_eq!(listed(0, &low, &block(left, 10, 0x0a, 0xf5)), Some(()));
        for wrong in [
            block(left, 10, 0x0a, 0xf5),
            block(right, 21, 0x14, 0xeb),
            block(right, 20, 0x15, 0xeb),
            block(right, 20, 0x14, 0xec),
        ] {
            assert_eq!(listed(0, &high, &wrong), None, "{wrong:?}");
        }
        assert_eq!(listed(-1, &high, &block(right, 20, 0x14, 0xeb)), None);
    }

    fn empty() -> ArcCell {
        CellBuilder::new().build().unwrap().to_arc()
    }

    fn hash_of(cell: &ArcCell) -> [u8; 32] {
        cell.get_hash(0).as_slice().try_into().unwrap()
    }

    fn merkle_proof(child: &ArcCell) -> ArcCell {
        let mut cell = CellBuilder::new();
        cell.set_cell_is_exotic(true);
        cell.store_u8(8, MERKLE_PROOF).unwrap();
        cell.store_slice(child.get_hash(0).as_slice()).unwrap();
        cell.store_u16(16, child.get_depth(0)).unwrap();
        cell.store_reference(child).unwrap();
        cell.build().unwrap().to_arc()
    }

    fn block_over(state: &ArcCell) -> ArcCell {
        let old = empty();
        let mut update = CellBuilder::new();
        update.set_cell_is_exotic(true);
        update.store_u8(8, MERKLE_UPDATE).unwrap();
        update.store_slice(old.get_hash(0).as_slice()).unwrap();
        update.store_slice(state.get_hash(0).as_slice()).unwrap();
        update.store_u16(16, old.get_depth(0)).unwrap();
        update.store_u16(16, state.get_depth(0)).unwrap();
        update
            .store_reference(&old)
            .unwrap()
            .store_reference(state)
            .unwrap();
        let update = update.build().unwrap().to_arc();
        let mut block = CellBuilder::new();
        block.store_u32(32, BLOCK_TAG).unwrap();
        block
            .store_reference(&empty())
            .unwrap()
            .store_reference(&empty())
            .unwrap();
        block.store_reference(&update).unwrap();
        block.build().unwrap().to_arc()
    }

    fn state_with(accounts: &ArcCell, custom: Option<&ArcCell>) -> ArcCell {
        let mut state = CellBuilder::new();
        state.store_u32(32, STATE_TAG).unwrap();
        state.store_bits(32 + 104 + 192 + 1, &[0; 42]).unwrap();
        state.store_bit(custom.is_some()).unwrap();
        state
            .store_reference(&empty())
            .unwrap()
            .store_reference(accounts)
            .unwrap();
        state.store_reference(&empty()).unwrap();
        if let Some(custom) = custom {
            state.store_reference(custom).unwrap();
        }
        state.build().unwrap().to_arc()
    }

    fn accounts_holding(key: &[u8; 32], account: &ArcCell) -> ArcCell {
        let mut leaf = CellBuilder::new();
        leaf.store_u8(2, 0b10).unwrap().store_u16(9, 256).unwrap();
        leaf.store_slice(key).unwrap();
        leaf.store_u8(5, 0)
            .unwrap()
            .store_u8(4, 0)
            .unwrap()
            .store_bit(false)
            .unwrap();
        leaf.store_reference(account).unwrap();
        leaf.store_slice(&[0x77; 32])
            .unwrap()
            .store_u64(64, 4_242)
            .unwrap();
        let leaf = leaf.build().unwrap().to_arc();
        let mut accounts = CellBuilder::new();
        accounts
            .store_bit(true)
            .unwrap()
            .store_reference(&leaf)
            .unwrap();
        accounts
            .store_u8(5, 0)
            .unwrap()
            .store_u8(4, 0)
            .unwrap()
            .store_bit(false)
            .unwrap();
        accounts.build().unwrap().to_arc()
    }

    fn no_accounts() -> ArcCell {
        let mut accounts = CellBuilder::new();
        accounts.store_bit(false).unwrap();
        accounts
            .store_u8(5, 0)
            .unwrap()
            .store_u8(4, 0)
            .unwrap()
            .store_bit(false)
            .unwrap();
        accounts.build().unwrap().to_arc()
    }

    fn listing(seqno: u32, root: &[u8; 32], file: &[u8; 32]) -> ArcCell {
        let mut descr = CellBuilder::new();
        descr.store_bit(false).unwrap().store_u8(4, 0xa).unwrap();
        descr
            .store_u32(32, seqno)
            .unwrap()
            .store_u32(32, 0)
            .unwrap();
        descr.store_u64(64, 0).unwrap().store_u64(64, 0).unwrap();
        descr.store_slice(root).unwrap().store_slice(file).unwrap();
        let descr = descr.build().unwrap().to_arc();
        let mut entry = CellBuilder::new();
        entry
            .store_u8(2, 0b11)
            .unwrap()
            .store_bit(false)
            .unwrap()
            .store_u8(6, 32)
            .unwrap();
        entry.store_reference(&descr).unwrap();
        let entry = entry.build().unwrap().to_arc();
        let mut extra = CellBuilder::new();
        extra
            .store_u16(16, MC_STATE_EXTRA_TAG)
            .unwrap()
            .store_bit(true)
            .unwrap();
        extra.store_reference(&entry).unwrap();
        extra.build().unwrap().to_arc()
    }

    struct Chain {
        masterchain: BlockId,
        shard: BlockId,
        shard_proof: Vec<u8>,
        proof: Vec<u8>,
    }

    fn chain(accounts: &ArcCell, listed_root: Option<[u8; 32]>) -> Chain {
        let shard_state = state_with(accounts, None);
        let shard_block = block_over(&shard_state);
        let shard = BlockId {
            workchain: 0,
            shard: i64::MIN,
            seqno: 7,
            root_hash: hash_of(&shard_block),
            file_hash: [9; 32],
        };
        let listed = listing(7, &listed_root.unwrap_or(shard.root_hash), &shard.file_hash);
        let mc_state = state_with(&no_accounts(), Some(&listed));
        let mc_block = block_over(&mc_state);
        let masterchain = BlockId {
            workchain: -1,
            shard: i64::MIN,
            seqno: 1,
            root_hash: hash_of(&mc_block),
            file_hash: [8; 32],
        };
        let bag = |first: &ArcCell, second: &ArcCell| {
            BagOfCells::new(&[merkle_proof(first), merkle_proof(second)])
                .serialize(false)
                .unwrap()
        };
        Chain {
            masterchain,
            shard,
            shard_proof: bag(&mc_block, &mc_state),
            proof: bag(&shard_block, &shard_state),
        }
    }

    fn proven(chain: &Chain, key: &[u8; 32]) -> Option<Proven> {
        account(
            &chain.masterchain,
            &chain.shard,
            &chain.shard_proof,
            &chain.proof,
            0,
            key,
        )
    }

    #[test]
    fn a_whole_proof_chain_leads_from_the_masterchain_to_the_account() {
        let mut data = CellBuilder::new();
        data.store_u32(32, 0xdead_beef).unwrap();
        let held = data.build().unwrap().to_arc();
        let ours = [0x3c; 32];
        let good = chain(&accounts_holding(&ours, &held), None);
        assert_eq!(
            proven(&good, &ours),
            Some(Proven::Present {
                account: hash_of(&held),
                last: ([0x77; 32], 4_242)
            })
        );
        assert_eq!(proven(&good, &[0xc3; 32]), Some(Proven::Absent));
        let unlisted = chain(&accounts_holding(&ours, &held), Some([0x55; 32]));
        assert_eq!(proven(&unlisted, &ours), None);
        let nobody = chain(&no_accounts(), None);
        assert_eq!(proven(&nobody, &ours), Some(Proven::Absent));
        let mut moved = chain(&accounts_holding(&ours, &held), None);
        moved.masterchain.root_hash[0] ^= 1;
        assert_eq!(proven(&moved, &ours), None);
        let mut swapped = chain(&accounts_holding(&ours, &held), None);
        std::mem::swap(&mut swapped.shard_proof, &mut swapped.proof);
        assert_eq!(proven(&swapped, &ours), None);
    }

    #[test]
    fn shard_ids_carry_their_prefix_and_end_bit() {
        let leaf = |marker: u8| {
            let mut cell = CellBuilder::new();
            cell.store_bit(false).unwrap().store_u8(8, marker).unwrap();
            cell.build().unwrap().to_arc()
        };
        let mut fork = CellBuilder::new();
        fork.store_bit(true)
            .unwrap()
            .store_reference(&leaf(1))
            .unwrap()
            .store_reference(&leaf(2))
            .unwrap();
        let root = fork.build().unwrap().to_arc();
        let (shard, _) = bin_tree_leaf(Arc::clone(&root), &[0x00; 32]).unwrap();
        assert_eq!(shard, 0x4000_0000_0000_0000);
        let (shard, found) = bin_tree_leaf(root, &[0xff; 32]).unwrap();
        assert_eq!(shard, 0xc000_0000_0000_0000);
        let mut parser = found.parser();
        parser.skip_bits(1).unwrap();
        assert_eq!(parser.load_u8(8).unwrap(), 2);
        assert_eq!(
            bin_tree_leaf(leaf(3), &[0; 32]).unwrap().0,
            0x8000_0000_0000_0000
        );
    }
}
