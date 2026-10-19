use tonlib_core::cell::{ArcCell, BagOfCells, Cell, CellParser};

use crate::proof::{Found, label, lookup};

pub(crate) const PARAMS: [i32; 13] = [0, 5, 8, 12, 18, 19, 20, 21, 24, 25, 31, 43, 45];
const REQUIRED: [i32; 10] = [0, 8, 12, 18, 19, 20, 21, 24, 25, 31];
pub(crate) const STORAGE_PRICES: i32 = 18;
const FEE_PARAMS: [i32; 2] = [21, 25];
pub(crate) const TRANSFER_GAS: u64 = 4_939;
const STORAGE_TAG: u8 = 0xcc;
const FLAT_TAG: u8 = 0xd1;
const GAS_TAG: u8 = 0xdd;
const GAS_EXT_TAG: u8 = 0xde;
const FORWARD_TAG: u8 = 0xea;
const SHIFT: u128 = 1 << 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Settings {
    pub flat_gas_limit: u64,
    pub flat_gas_price: u64,
    pub gas_price: u64,
    pub lump_price: u64,
    pub bit_price: u64,
    pub cell_price: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Storage {
    periods: Vec<Period>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Period {
    since: u32,
    bit: u64,
    cell: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Param {
    Present(ArcCell),
    Absent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Config {
    params: Vec<(i32, ArcCell)>,
}

impl Config {
    pub(crate) fn new(found: Vec<Param>) -> Option<Self> {
        let params: Vec<(i32, ArcCell)> = PARAMS
            .into_iter()
            .zip(found)
            .filter_map(|(id, param)| match param {
                Param::Present(cell) => Some((id, cell)),
                Param::Absent => None,
            })
            .collect();
        REQUIRED
            .iter()
            .all(|id| params.iter().any(|(known, _)| known == id))
            .then_some(Self { params })
    }

    pub(crate) fn settings(&self) -> Option<Settings> {
        let cells = FEE_PARAMS
            .iter()
            .map(|id| self.get(*id).cloned())
            .collect::<Option<Vec<_>>>()?;
        Settings::read(&cells)
    }

    pub(crate) fn storage(&self) -> Option<Storage> {
        Storage::read(self.get(STORAGE_PRICES)?)
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
    pub(crate) fn read(params: &[ArcCell]) -> Option<Self> {
        let [gas, forward] = params else {
            return None;
        };
        let (flat_gas_limit, flat_gas_price, gas_price) = gas_prices(gas)?;
        let (lump_price, bit_price, cell_price) = forward_prices(forward)?;
        Some(Self {
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
}

impl Storage {
    fn read(root: &ArcCell) -> Option<Self> {
        let mut periods = Vec::new();
        for leaf in leaves(root, 32)? {
            let mut parser = leaf.0.parser();
            parser.skip_bits(leaf.1).ok()?;
            if parser.load_u8(8).ok()? != STORAGE_TAG {
                return None;
            }
            periods.push(Period {
                since: parser.load_u32(32).ok()?,
                bit: parser.load_u64(64).ok()?,
                cell: parser.load_u64(64).ok()?,
            });
        }
        periods.sort_by_key(|period| period.since);
        (!periods.is_empty()).then_some(Self { periods })
    }

    pub(crate) fn fee(&self, (bits, cells): (u64, u64), last_paid: u32, now: u32) -> Option<u64> {
        let first = self.periods.first()?;
        if last_paid == 0 || now <= last_paid || now <= first.since {
            return Some(0);
        }
        let mut upto = last_paid.max(first.since);
        let mut total: u128 = 0;
        for (at, period) in self.periods.iter().enumerate() {
            let until = self
                .periods
                .get(at + 1)
                .map_or(now, |next| next.since.min(now));
            if upto < until {
                let rate = u128::from(bits) * u128::from(period.bit)
                    + u128::from(cells) * u128::from(period.cell);
                total = total.checked_add(rate.checked_mul(u128::from(until - upto))?)?;
                upto = until;
            }
        }
        u64::try_from(total.div_ceil(SHIFT)).ok()
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

pub(crate) fn leaves(root: &ArcCell, bits: usize) -> Option<Vec<(ArcCell, usize)>> {
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

pub(crate) fn param(dict: &ArcCell, id: i32) -> Option<Param> {
    let Found::Leaf(leaf, _) = lookup(dict, &id.to_be_bytes(), 32)? else {
        return Some(Param::Absent);
    };
    let value = leaf.reference(0).ok()?.clone();
    ordinary(&value).cloned().map(Param::Present)
}

#[cfg(test)]
mod tests {
    use tonlib_core::cell::CellBuilder;

    use super::*;

    pub(crate) fn mainnet() -> Settings {
        Settings {
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
        let storage = Storage {
            periods: vec![Period {
                since: 0,
                bit: 0,
                cell: 135,
            }],
        };
        assert_eq!(
            storage.fee((1_000, 10), 1_000_000, 1_000_000 + 65_536),
            Some(1_350)
        );
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

    fn leaf(key_bit: bool, entry: CellBuilder) -> ArcCell {
        let mut cell = CellBuilder::new();
        cell.store_u8(2, 0b11)
            .unwrap()
            .store_bit(key_bit)
            .unwrap()
            .store_u8(5, 31)
            .unwrap();
        cell.store_cell(&{ entry }.build().unwrap()).unwrap();
        cell.build().unwrap().to_arc()
    }

    fn prices(first: CellBuilder, second: CellBuilder) -> ArcCell {
        let mut root = CellBuilder::new();
        root.store_u8(2, 0b00).unwrap();
        root.store_reference(&leaf(false, first)).unwrap();
        root.store_reference(&leaf(true, second)).unwrap();
        root.build().unwrap().to_arc()
    }

    #[test]
    fn reads_the_gas_and_forwarding_params() {
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
        assert_eq!(
            Settings::read(&[gas.clone(), forward.clone()]),
            Some(mainnet())
        );
        assert_eq!(Settings::read(std::slice::from_ref(&gas)), None);
        assert_eq!(Settings::read(&[forward, gas]), None);
    }

    #[test]
    fn storage_is_charged_at_each_periods_own_price() {
        let change = 1_777_500_000;
        let storage =
            Storage::read(&prices(storage_entry(change, 135), storage_entry(0, 500))).unwrap();
        assert_eq!(
            storage
                .periods
                .iter()
                .map(|period| period.since)
                .collect::<Vec<_>>(),
            [0, change]
        );
        let size = (1_000, 10);
        assert_eq!(storage.fee(size, change - 1_000, change + 3_000), Some(200));
        assert_eq!(storage.fee(size, change + 1_000, change + 4_000), Some(108));
        assert_eq!(storage.fee(size, change - 4_000, change), Some(367));
        assert_eq!(storage.fee(size, 0, change), Some(0));
        assert_eq!(storage.fee(size, change, change), Some(0));
        assert_eq!(storage.fee(size, change + 5, change), Some(0));
        let future = Storage::read(&prices(
            storage_entry(change, 135),
            storage_entry(1_900_000_000, 9),
        ))
        .unwrap();
        assert_eq!(future.fee(size, change - 9, change), Some(0));
        assert_eq!(
            future.fee(size, 1_792_000_000, 1_792_000_000 + 65_536),
            Some(2_350)
        );
        let mut broken = CellBuilder::new();
        broken.store_u8(8, FLAT_TAG).unwrap();
        assert_eq!(Storage::read(&prices(storage_entry(0, 500), broken)), None);
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
