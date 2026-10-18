use tonlib_core::cell::{ArcCell, BagOfCells, Cell, CellParser};

use crate::proof::{Found, label, lookup};

pub(crate) const PARAMS: [i32; 7] = [18, 19, 20, 21, 24, 25, 43];
const FEE_PARAMS: [i32; 3] = [18, 21, 25];
pub(crate) const TRANSFER_GAS: u64 = 4_939;
const STORAGE_TAG: u8 = 0xcc;
const FLAT_TAG: u8 = 0xd1;
const GAS_TAG: u8 = 0xdd;
const GAS_EXT_TAG: u8 = 0xde;
const FORWARD_TAG: u8 = 0xea;
const SHIFT: u128 = 1 << 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Settings {
    pub bit_price_ps: u64,
    pub cell_price_ps: u64,
    pub flat_gas_limit: u64,
    pub flat_gas_price: u64,
    pub gas_price: u64,
    pub lump_price: u64,
    pub bit_price: u64,
    pub cell_price: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Config {
    params: Vec<(i32, ArcCell)>,
}

impl Config {
    pub(crate) fn new(cells: Vec<ArcCell>) -> Self {
        Self {
            params: PARAMS.into_iter().zip(cells).collect(),
        }
    }

    pub(crate) fn settings(&self, now: u32) -> Option<Settings> {
        let cells = FEE_PARAMS
            .iter()
            .map(|id| self.get(*id).cloned())
            .collect::<Option<Vec<_>>>()?;
        Settings::read(&cells, now)
    }

    pub(crate) fn encoded(&self) -> Option<Vec<(i32, Vec<u8>)>> {
        self.params
            .iter()
            .map(|(id, cell)| {
                let boc = BagOfCells::from_root(Cell::clone(cell))
                    .serialize(false)
                    .ok()?;
                Some((*id, boc))
            })
            .collect()
    }

    fn get(&self, id: i32) -> Option<&ArcCell> {
        self.params
            .iter()
            .find(|(known, _)| *known == id)
            .map(|(_, cell)| cell)
    }
}

impl Settings {
    pub(crate) fn read(params: &[ArcCell], now: u32) -> Option<Self> {
        let [storage, gas, forward] = params else {
            return None;
        };
        let (bit_price_ps, cell_price_ps) = storage_prices(storage, now)?;
        let (flat_gas_limit, flat_gas_price, gas_price) = gas_prices(gas)?;
        let (lump_price, bit_price, cell_price) = forward_prices(forward)?;
        Some(Self {
            bit_price_ps,
            cell_price_ps,
            flat_gas_limit,
            flat_gas_price,
            gas_price,
            lump_price,
            bit_price,
            cell_price,
        })
    }

    pub(crate) fn gas_fee(&self, gas: u64) -> Option<u64> {
        if gas <= self.flat_gas_limit {
            return Some(self.flat_gas_price);
        }
        let extra = u128::from(gas - self.flat_gas_limit) * u128::from(self.gas_price);
        let extra = u64::try_from(extra.div_ceil(SHIFT)).ok()?;
        self.flat_gas_price.checked_add(extra)
    }

    pub(crate) fn forward_fee(&self, (bits, cells): (u64, u64)) -> Option<u64> {
        let size = u128::from(bits) * u128::from(self.bit_price)
            + u128::from(cells) * u128::from(self.cell_price);
        self.lump_price
            .checked_add(u64::try_from(size.div_ceil(SHIFT)).ok()?)
    }

    pub(crate) fn storage_fee(&self, (bits, cells): (u64, u64), seconds: u64) -> Option<u64> {
        let rate = u128::from(bits) * u128::from(self.bit_price_ps)
            + u128::from(cells) * u128::from(self.cell_price_ps);
        u64::try_from((rate * u128::from(seconds)).div_ceil(SHIFT)).ok()
    }
}

pub(crate) fn size_without_root(root: &Cell) -> (u64, u64) {
    let mut seen = std::collections::BTreeSet::new();
    let mut pending: Vec<ArcCell> = root.references().to_vec();
    let (mut bits, mut cells) = (0u64, 0u64);
    while let Some(cell) = pending.pop() {
        if !seen.insert(cell.cell_hash()) {
            continue;
        }
        bits += u64::try_from(cell.bit_len()).unwrap_or(u64::MAX);
        cells += 1;
        pending.extend(cell.references().iter().cloned());
    }
    (bits, cells)
}

fn ordinary(cell: &ArcCell) -> Option<&ArcCell> {
    (!cell.is_exotic()).then_some(cell)
}

fn storage_prices(root: &ArcCell, now: u32) -> Option<(u64, u64)> {
    let mut best: Option<(u32, u64, u64)> = None;
    for leaf in leaves(root, 32)? {
        let mut parser = leaf.0.parser();
        parser.skip_bits(leaf.1).ok()?;
        if parser.load_u8(8).ok()? != STORAGE_TAG {
            return None;
        }
        let since = parser.load_u32(32).ok()?;
        let bit = parser.load_u64(64).ok()?;
        let cell = parser.load_u64(64).ok()?;
        if since <= now && best.is_none_or(|(known, ..)| since >= known) {
            best = Some((since, bit, cell));
        }
    }
    best.map(|(_, bit, cell)| (bit, cell))
}

fn leaves(root: &ArcCell, bits: usize) -> Option<Vec<(ArcCell, usize)>> {
    let mut found = Vec::new();
    let mut pending = vec![(root.clone(), bits)];
    while let Some((cell, remaining)) = pending.pop() {
        ordinary(&cell)?;
        let mut parser = cell.parser();
        let length = label(&mut parser, remaining)?.len();
        if length == remaining {
            found.push((cell.clone(), cell.bit_len() - parser.remaining_bits()));
            continue;
        }
        let rest = remaining - length - 1;
        pending.push((cell.reference(0).ok()?.clone(), rest));
        pending.push((cell.reference(1).ok()?.clone(), rest));
        if found.len() + pending.len() > 64 {
            return None;
        }
    }
    Some(found)
}

fn gas_prices(cell: &ArcCell) -> Option<(u64, u64, u64)> {
    let mut parser = ordinary(cell)?.parser();
    let (mut flat_limit, mut flat_price) = (0, 0);
    let mut tag = parser.load_u8(8).ok()?;
    if tag == FLAT_TAG {
        flat_limit = parser.load_u64(64).ok()?;
        flat_price = parser.load_u64(64).ok()?;
        tag = parser.load_u8(8).ok()?;
    }
    if !matches!(tag, GAS_TAG | GAS_EXT_TAG) {
        return None;
    }
    Some((flat_limit, flat_price, parser.load_u64(64).ok()?))
}

fn forward_prices(cell: &ArcCell) -> Option<(u64, u64, u64)> {
    let mut parser: CellParser<'_> = ordinary(cell)?.parser();
    if parser.load_u8(8).ok()? != FORWARD_TAG {
        return None;
    }
    Some((
        parser.load_u64(64).ok()?,
        parser.load_u64(64).ok()?,
        parser.load_u64(64).ok()?,
    ))
}

pub(crate) fn param(dict: &ArcCell, id: i32) -> Option<ArcCell> {
    let Found::Leaf(leaf, _) = lookup(dict, &id.to_be_bytes(), 32)? else {
        return None;
    };
    let value = leaf.reference(0).ok()?.clone();
    ordinary(&value).cloned()
}

#[cfg(test)]
mod tests {
    use tonlib_core::cell::CellBuilder;

    use super::*;

    pub(crate) fn mainnet() -> Settings {
        Settings {
            bit_price_ps: 0,
            cell_price_ps: 135,
            flat_gas_limit: 100,
            flat_gas_price: 6_667,
            gas_price: 4_369_067,
            lump_price: 66_667,
            bit_price: 4_369_067,
            cell_price: 436_906_667,
        }
    }

    #[test]
    fn fees_match_what_mainnet_charged_a_v5r1_transfer() {
        let settings = mainnet();
        assert_eq!(settings.gas_fee(TRANSFER_GAS), Some(329_268));
        assert_eq!(settings.gas_fee(50), Some(6_667));
        assert_eq!(settings.forward_fee((0, 0)), Some(66_667));
        assert_eq!(settings.storage_fee((1_000, 10), 65_536), Some(1_350));
    }

    fn storage_entry(since: u32, cell_price: u64) -> CellBuilder {
        let mut entry = CellBuilder::new();
        entry
            .store_u8(8, STORAGE_TAG)
            .unwrap()
            .store_u32(32, since)
            .unwrap();
        entry
            .store_u64(64, 1)
            .unwrap()
            .store_u64(64, cell_price)
            .unwrap();
        entry
            .store_u64(64, 1_000)
            .unwrap()
            .store_u64(64, 500_000)
            .unwrap();
        entry
    }

    #[test]
    fn reads_the_three_fee_params() {
        let leaf = |key_bit: bool, entry: CellBuilder| {
            let mut cell = CellBuilder::new();
            cell.store_u8(2, 0b11)
                .unwrap()
                .store_bit(key_bit)
                .unwrap()
                .store_u8(5, 31)
                .unwrap();
            cell.store_cell(&{ entry }.build().unwrap()).unwrap();
            cell.build().unwrap().to_arc()
        };
        let mut root = CellBuilder::new();
        root.store_u8(2, 0b00).unwrap();
        root.store_reference(&leaf(false, storage_entry(0, 500)))
            .unwrap();
        root.store_reference(&leaf(true, storage_entry(1_777_500_000, 135)))
            .unwrap();
        let storage = root.build().unwrap().to_arc();
        let mut gas = CellBuilder::new();
        gas.store_u8(8, FLAT_TAG)
            .unwrap()
            .store_u64(64, 100)
            .unwrap()
            .store_u64(64, 6_667)
            .unwrap();
        gas.store_u8(8, GAS_EXT_TAG)
            .unwrap()
            .store_u64(64, 4_369_067)
            .unwrap();
        for _ in 0..6 {
            gas.store_u64(64, 1).unwrap();
        }
        let gas = gas.build().unwrap().to_arc();
        let mut forward = CellBuilder::new();
        forward
            .store_u8(8, FORWARD_TAG)
            .unwrap()
            .store_u64(64, 66_667)
            .unwrap();
        forward
            .store_u64(64, 4_369_067)
            .unwrap()
            .store_u64(64, 436_906_667)
            .unwrap();
        forward
            .store_u32(32, 98_304)
            .unwrap()
            .store_u16(16, 21_845)
            .unwrap()
            .store_u16(16, 21_845)
            .unwrap();
        let forward = forward.build().unwrap().to_arc();
        let params = [storage, gas, forward];
        let mut expected = mainnet();
        expected.bit_price_ps = 1;
        assert_eq!(Settings::read(&params, 1_792_000_000), Some(expected));
        let older = Settings::read(&params, 1_700_000_000).unwrap();
        assert_eq!(older.cell_price_ps, 500);
        assert_eq!(Settings::read(&params[..2], 1_792_000_000), None);
        let mut swapped = CellBuilder::new();
        swapped.store_u8(2, 0b00).unwrap();
        swapped
            .store_reference(&leaf(false, storage_entry(1_777_500_000, 135)))
            .unwrap();
        swapped
            .store_reference(&leaf(true, storage_entry(0, 500)))
            .unwrap();
        let swapped = [
            swapped.build().unwrap().to_arc(),
            params[1].clone(),
            params[2].clone(),
        ];
        assert_eq!(
            Settings::read(&swapped, 1_792_000_000)
                .unwrap()
                .cell_price_ps,
            135
        );
        let mut future = CellBuilder::new();
        future.store_u8(2, 0b00).unwrap();
        future
            .store_reference(&leaf(false, storage_entry(1_777_500_000, 135)))
            .unwrap();
        future
            .store_reference(&leaf(true, storage_entry(1_900_000_000, 9)))
            .unwrap();
        let future = [
            future.build().unwrap().to_arc(),
            params[1].clone(),
            params[2].clone(),
        ];
        assert_eq!(
            Settings::read(&future, 1_792_000_000)
                .unwrap()
                .cell_price_ps,
            135
        );
    }

    #[test]
    fn message_size_counts_each_cell_below_the_root_once() {
        let mut shared = CellBuilder::new();
        shared.store_u8(8, 7).unwrap();
        let shared = shared.build().unwrap().to_arc();
        let mut middle = CellBuilder::new();
        middle
            .store_u16(16, 1)
            .unwrap()
            .store_reference(&shared)
            .unwrap();
        let middle = middle.build().unwrap().to_arc();
        let mut root = CellBuilder::new();
        root.store_u32(32, 1).unwrap();
        root.store_reference(&middle)
            .unwrap()
            .store_reference(&shared)
            .unwrap();
        assert_eq!(size_without_root(&root.build().unwrap()), (24, 2));
        assert_eq!(
            size_without_root(&CellBuilder::new().build().unwrap()),
            (0, 0)
        );
    }
}
