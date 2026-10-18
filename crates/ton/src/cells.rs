use std::sync::Arc;

use bahaa_types::Address;
use ruint::aliases::U256;
use tonlib_core::cell::{ArcCell, BagOfCells, Cell, CellBuilder, CellParser, TonCellError};
use tonlib_core::{TonAddress, TonHash};

use crate::http::Item;

const MAX_DEPTH: u32 = 64;
const ADDRESS_BITS: u32 = 267;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    Missing,
    Uninit,
    Frozen,
    Active {
        code: [u8; 32],
        data: Option<ArcCell>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Account {
    pub balance: U256,
    pub bits: u64,
    pub cells: u64,
    pub last_paid: u32,
    pub due: U256,
    pub status: Status,
    pub last: Option<([u8; 32], u64)>,
}

impl Account {
    fn missing() -> Self {
        Self {
            balance: U256::ZERO,
            bits: 0,
            cells: 0,
            last_paid: 0,
            due: U256::ZERO,
            status: Status::Missing,
            last: None,
        }
    }
}

pub(crate) fn account(boc: &[u8], owner: &Address) -> Option<Account> {
    if boc.is_empty() {
        return Some(Account::missing());
    }
    let root = BagOfCells::parse(boc).ok()?.single_root().ok()?;
    let mut parser = root.parser();
    if !parser.load_bit().ok()? {
        return Some(Account::missing());
    }
    let address = parser.load_address().ok()?;
    if from_ton(&address)? != *owner {
        return None;
    }
    let cells = u64::try_from(var_uint(&mut parser, 3)?).ok()?;
    let bits = u64::try_from(var_uint(&mut parser, 3)?).ok()?;
    match parser.load_u8(3).ok()? {
        0 => {}
        1 => parser.skip_bits(256).ok()?,
        _ => return None,
    }
    let last_paid = parser.load_u32(32).ok()?;
    let due = if parser.load_bit().ok()? {
        var_uint(&mut parser, 4)?
    } else {
        U256::ZERO
    };
    parser.skip_bits(64).ok()?;
    let balance = var_uint(&mut parser, 4)?;
    if parser.load_bit().ok()? {
        parser.next_reference().ok()?;
    }
    let status = if parser.load_bit().ok()? {
        state_init(&mut parser)?
    } else {
        let frozen = parser.load_bit().ok()?;
        if frozen {
            Status::Frozen
        } else {
            Status::Uninit
        }
    };
    Some(Account {
        balance,
        bits,
        cells,
        last_paid,
        due,
        status,
        last: None,
    })
}

fn state_init(parser: &mut CellParser<'_>) -> Option<Status> {
    if parser.load_bit().ok()? {
        parser.skip_bits(5).ok()?;
    }
    if parser.load_bit().ok()? {
        parser.skip_bits(2).ok()?;
    }
    let code = if parser.load_bit().ok()? {
        parser.next_reference().ok()?
    } else {
        return None;
    };
    let data = if parser.load_bit().ok()? {
        Some(parser.next_reference().ok()?)
    } else {
        None
    };
    Some(Status::Active {
        code: code.cell_hash().as_slice().try_into().ok()?,
        data,
    })
}

pub(crate) fn var_uint(parser: &mut CellParser<'_>, length_bits: usize) -> Option<U256> {
    let length = usize::from(parser.load_u8(length_bits).ok()?);
    if length == 0 {
        return Some(U256::ZERO);
    }
    let bytes = parser.load_bytes(length).ok()?;
    U256::try_from_be_slice(&bytes)
}

pub(crate) fn no_arguments() -> Result<Vec<u8>, TonCellError> {
    let mut root = CellBuilder::new();
    root.store_u32(24, 0)?;
    BagOfCells::from_root(root.build()?).serialize(false)
}

pub(crate) fn address_argument(address: &Address) -> Result<Vec<u8>, TonCellError> {
    let Address::Ton { workchain, hash } = address else {
        return Err(TonCellError::InvalidCellData(
            "not a TON address".to_owned(),
        ));
    };
    let mut slice = CellBuilder::new();
    slice.store_address(&TonAddress::new(
        i32::from(*workchain),
        TonHash::from(*hash),
    ))?;
    let slice = slice.build()?.to_arc();
    let nil = CellBuilder::new().build()?.to_arc();
    let mut root = CellBuilder::new();
    root.store_u32(24, 1)?
        .store_u8(8, 4)?
        .store_u16(10, 0)?
        .store_u16(10, 267)?
        .store_u8(3, 0)?
        .store_u8(3, 0)?
        .store_reference(&nil)?
        .store_reference(&slice)?;
    BagOfCells::from_root(root.build()?).serialize(false)
}

pub(crate) fn read_stack(boc: &[u8]) -> Option<Vec<Item>> {
    let root = BagOfCells::parse(boc).ok()?.single_root().ok()?;
    let mut parser = root.parser();
    let depth = parser.load_u32(24).ok()?;
    if depth > MAX_DEPTH {
        return None;
    }
    let mut items = Vec::new();
    let mut cell: ArcCell = Arc::clone(&root);
    let mut skip = 24;
    for _ in 0..depth {
        let mut parser = cell.parser();
        parser.skip_bits(skip).ok()?;
        let rest = parser.next_reference().ok()?;
        items.push(value(&mut parser)?);
        parser.ensure_empty().ok()?;
        cell = rest;
        skip = 0;
    }
    let mut end = cell.parser();
    end.skip_bits(skip).ok()?;
    end.ensure_empty().ok()?;
    items.reverse();
    Some(items)
}

fn value(parser: &mut CellParser<'_>) -> Option<Item> {
    match parser.load_u8(8).ok()? {
        1 => {
            let number = parser.load_i64(64).ok()?;
            u64::try_from(number)
                .ok()
                .map(|number| Item::Num(U256::from(number)))
        }
        2 => {
            if parser.load_u8(7).ok()? != 0 || parser.load_bit().ok()? {
                return None;
            }
            let bytes = parser.load_bits(256).ok()?;
            Some(Item::Num(U256::try_from_be_slice(&bytes)?))
        }
        3 => boc_of(&parser.next_reference().ok()?).map(Item::Cell),
        4 => {
            let cell = parser.next_reference().ok()?;
            let start = usize::from(parser.load_u16(10).ok()?);
            let end = usize::from(parser.load_u16(10).ok()?);
            let first = usize::from(parser.load_u8(3).ok()?);
            let last = usize::from(parser.load_u8(3).ok()?);
            slice(&cell, start, end, first, last)
                .and_then(|cell| boc_of(&cell.to_arc()))
                .map(Item::Cell)
        }
        _ => None,
    }
}

fn slice(cell: &Cell, start: usize, end: usize, first: usize, last: usize) -> Option<Cell> {
    if start > end || end > cell.bit_len() || first > last || last > cell.references().len() {
        return None;
    }
    if u32::try_from(end - start).ok()? > ADDRESS_BITS * 4 {
        return None;
    }
    let mut parser = cell.parser();
    parser.skip_bits(start).ok()?;
    let bits = parser.load_bits(end - start).ok()?;
    let mut builder = CellBuilder::new();
    builder.store_bits(end - start, &bits).ok()?;
    builder
        .store_references(&cell.references()[first..last])
        .ok()?;
    builder.build().ok()
}

fn boc_of(cell: &ArcCell) -> Option<Vec<u8>> {
    BagOfCells::from_root(Cell::clone(cell))
        .serialize(false)
        .ok()
}

fn from_ton(address: &TonAddress) -> Option<Address> {
    Some(Address::Ton {
        workchain: i8::try_from(address.workchain).ok()?,
        hash: address.hash_part.as_slice().try_into().ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wallet::read_address;

    fn owner() -> Address {
        Address::Ton {
            workchain: 0,
            hash: [0xab; 32],
        }
    }

    fn account_cell(extra: u8, due: bool, balance: u64) -> Vec<u8> {
        let mut builder = CellBuilder::new();
        builder
            .store_bit(true)
            .unwrap()
            .store_address(&TonAddress::new(0, TonHash::from([0xab; 32])))
            .unwrap()
            .store_u8(3, 1)
            .unwrap()
            .store_u8(8, 5)
            .unwrap()
            .store_u8(3, 2)
            .unwrap()
            .store_u16(16, 900)
            .unwrap()
            .store_u8(3, extra)
            .unwrap();
        if extra == 1 {
            builder.store_bits(256, &[7; 32]).unwrap();
        }
        builder
            .store_u32(32, 1_700_000_000)
            .unwrap()
            .store_bit(due)
            .unwrap();
        if due {
            builder.store_u8(4, 1).unwrap().store_u8(8, 3).unwrap();
        }
        builder.store_u64(64, 42).unwrap();
        let length = balance
            .to_be_bytes()
            .iter()
            .position(|byte| *byte != 0)
            .map_or(0, |zeros| 8 - zeros);
        builder.store_u8(4, u8::try_from(length).unwrap()).unwrap();
        builder
            .store_bits(length * 8, &balance.to_be_bytes()[8 - length..])
            .unwrap();
        builder.store_bit(false).unwrap();
        builder.store_u8(2, 0).unwrap();
        BagOfCells::from_root(builder.build().unwrap())
            .serialize(false)
            .unwrap()
    }

    fn active(code: &Cell, data: &Cell) -> Vec<u8> {
        let mut builder = CellBuilder::new();
        builder.store_bit(true).unwrap();
        builder
            .store_address(&TonAddress::new(0, TonHash::from([0xab; 32])))
            .unwrap();
        builder.store_u8(3, 1).unwrap().store_u8(8, 21).unwrap();
        builder
            .store_u8(3, 2)
            .unwrap()
            .store_u16(16, 5_120)
            .unwrap();
        builder
            .store_u8(3, 0)
            .unwrap()
            .store_u32(32, 1_790_000_000)
            .unwrap();
        builder
            .store_bit(true)
            .unwrap()
            .store_u8(4, 1)
            .unwrap()
            .store_u8(8, 9)
            .unwrap();
        builder
            .store_u64(64, 42)
            .unwrap()
            .store_u8(4, 1)
            .unwrap()
            .store_u8(8, 77)
            .unwrap();
        builder.store_bit(false).unwrap();
        builder.store_bit(true).unwrap();
        builder.store_bit(false).unwrap().store_bit(false).unwrap();
        builder
            .store_bit(true)
            .unwrap()
            .store_child(code.clone())
            .unwrap();
        builder
            .store_bit(true)
            .unwrap()
            .store_child(data.clone())
            .unwrap();
        builder.store_bit(false).unwrap();
        BagOfCells::from_root(builder.build().unwrap())
            .serialize(false)
            .unwrap()
    }

    #[test]
    fn reads_the_state_and_storage_of_a_deployed_wallet() {
        let mut code = CellBuilder::new();
        code.store_u32(32, 0xc0de).unwrap();
        let code = code.build().unwrap();
        let mut data = CellBuilder::new();
        data.store_u32(32, 7).unwrap();
        let data = data.build().unwrap();
        let read = account(&active(&code, &data), &owner()).unwrap();
        assert_eq!(
            (
                read.balance,
                read.bits,
                read.cells,
                read.last_paid,
                read.due
            ),
            (U256::from(77u8), 5_120, 21, 1_790_000_000, U256::from(9u8))
        );
        let Status::Active {
            code: code_hash,
            data: Some(stored),
        } = read.status
        else {
            panic!("{:?}", read.status);
        };
        assert_eq!(
            code_hash,
            <[u8; 32]>::try_from(code.cell_hash().as_slice()).unwrap()
        );
        assert_eq!(stored.cell_hash(), data.cell_hash());
        assert_eq!(
            account(&account_cell(0, false, 5), &owner())
                .unwrap()
                .status,
            Status::Uninit
        );
        assert_eq!(account(&[], &owner()).unwrap().status, Status::Missing);
    }

    fn balance_of(boc: &[u8], owner: &Address) -> Option<U256> {
        account(boc, owner).map(|account| account.balance)
    }

    #[test]
    fn reads_the_balance_of_both_storage_layouts() {
        assert_eq!(
            balance_of(&account_cell(0, false, 2_500_000_000), &owner()),
            Some(U256::from(2_500_000_000u64))
        );
        assert_eq!(
            balance_of(&account_cell(1, true, 7), &owner()),
            Some(U256::from(7u8))
        );
        assert_eq!(
            balance_of(&account_cell(0, false, 0), &owner()),
            Some(U256::ZERO)
        );
        assert_eq!(balance_of(&account_cell(2, false, 7), &owner()), None);
        let stranger = Address::Ton {
            workchain: 0,
            hash: [1; 32],
        };
        assert_eq!(balance_of(&account_cell(0, false, 7), &stranger), None);
        assert_eq!(balance_of(&[], &owner()), Some(U256::ZERO));
        let mut none = CellBuilder::new();
        none.store_bit(false).unwrap();
        let none = BagOfCells::from_root(none.build().unwrap())
            .serialize(false)
            .unwrap();
        assert_eq!(balance_of(&none, &owner()), Some(U256::ZERO));
        assert_eq!(balance_of(&[1, 2, 3], &owner()), None);
    }

    fn stack(values: Vec<(u8, CellBuilder)>) -> Vec<u8> {
        let mut rest = CellBuilder::new().build().unwrap().to_arc();
        let depth = values.len();
        for (at, (_, mut value)) in values.into_iter().enumerate() {
            let mut cell = CellBuilder::new();
            if at + 1 == depth {
                cell.store_u32(24, u32::try_from(depth).unwrap()).unwrap();
            }
            cell.store_reference(&rest).unwrap();
            let built = value.build().unwrap();
            cell.store_cell(&built).unwrap();
            rest = cell.build().unwrap().to_arc();
        }
        BagOfCells::from_root(Cell::clone(&rest))
            .serialize(false)
            .unwrap()
    }

    fn tiny(number: i64) -> (u8, CellBuilder) {
        let mut value = CellBuilder::new();
        value.store_u8(8, 1).unwrap().store_i64(64, number).unwrap();
        (1, value)
    }

    fn big(number: u64) -> (u8, CellBuilder) {
        let mut value = CellBuilder::new();
        value
            .store_u8(8, 2)
            .unwrap()
            .store_u8(7, 0)
            .unwrap()
            .store_bit(false)
            .unwrap();
        let mut bits = [0u8; 32];
        bits[24..].copy_from_slice(&number.to_be_bytes());
        value.store_bits(256, &bits).unwrap();
        (2, value)
    }

    fn cell(address: [u8; 32]) -> (u8, CellBuilder) {
        let mut inner = CellBuilder::new();
        inner
            .store_address(&TonAddress::new(0, TonHash::from(address)))
            .unwrap();
        let mut value = CellBuilder::new();
        value
            .store_u8(8, 3)
            .unwrap()
            .store_child(inner.build().unwrap())
            .unwrap();
        (3, value)
    }

    #[test]
    fn reads_get_method_results_bottom_first() {
        let boc = stack(vec![
            big(94_631_240_538_000),
            cell([0xab; 32]),
            cell([0xcd; 32]),
            tiny(5),
        ]);
        let items = read_stack(&boc).unwrap();
        assert_eq!(items.len(), 4);
        assert_eq!(items[0], Item::Num(U256::from(94_631_240_538_000u64)));
        let Item::Cell(holder) = &items[1] else {
            panic!("{items:?}");
        };
        assert_eq!(read_address(holder), Some(owner()));
        assert_eq!(items[3], Item::Num(U256::from(5u8)));
        assert_eq!(read_stack(&stack(vec![tiny(-1)])), None);
        assert_eq!(read_stack(&no_arguments().unwrap()), Some(Vec::new()));
    }

    #[test]
    fn an_address_argument_reads_back_as_that_address() {
        let encoded = address_argument(&owner()).unwrap();
        let items = read_stack(&encoded).unwrap();
        let [Item::Cell(slice)] = items.as_slice() else {
            panic!("{items:?}");
        };
        assert_eq!(read_address(slice), Some(owner()));
    }
}
