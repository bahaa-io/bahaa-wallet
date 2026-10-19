use bahaa_chain::ChainError;
use bahaa_types::Address;
use ruint::aliases::U256;
use tonlib_core::TonAddress;
use tonlib_core::cell::{ArcCell, BagOfCells, CellParser};
use tycho_types::boc::de::{BocHeader, Options};
use tycho_types::cell::{Cell as Checked, CellFamily};

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
    pub boc: Vec<u8>,
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
            boc: Vec::new(),
        }
    }
}

pub(crate) fn bag(boc: &[u8]) -> Option<BagOfCells> {
    BocHeader::decode(boc, &Options::default())
        .ok()?
        .finalize(Checked::empty_context())
        .ok()?;
    BagOfCells::parse(boc).ok()
}

pub(crate) fn root(boc: &[u8]) -> Option<ArcCell> {
    bag(boc)?.single_root().ok()
}

pub(crate) fn parts(address: &Address) -> Result<(i8, [u8; 32]), ChainError> {
    match address {
        Address::Ton { workchain, hash } => Ok((*workchain, *hash)),
        _ => Err(ChainError::Invalid("not a TON address")),
    }
}

pub(crate) fn account(boc: &[u8], owner: &Address) -> Option<Account> {
    if boc.is_empty() {
        return Some(Account::missing());
    }
    account_at(&root(boc)?, boc, owner)
}

pub(crate) fn account_at(root: &ArcCell, boc: &[u8], owner: &Address) -> Option<Account> {
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
        boc: boc.to_vec(),
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

pub(crate) fn from_ton(address: &TonAddress) -> Option<Address> {
    Some(Address::Ton {
        workchain: i8::try_from(address.workchain).ok()?,
        hash: address.hash_part.as_slice().try_into().ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tonlib_core::TonHash;
    use tonlib_core::cell::{Cell, CellBuilder};

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
    fn a_bag_whose_cells_point_past_the_end_is_refused_not_trusted() {
        let hostile = [0xb5, 0xee, 0x9c, 0x72, 1, 1, 1, 1, 0, 3, 0, 1, 0, 5];
        assert!(bag(&hostile).is_none());
        assert!(crate::transaction::linked(&hostile, &[0; 32], ([0; 32], 0)).is_none());
        assert!(crate::wallet::parse(&hostile).is_err());
        let fine = [0xb5, 0xee, 0x9c, 0x72, 1, 1, 1, 1, 0, 2, 0, 0, 0];
        assert_eq!(bag(&fine).map(|bag| bag.roots.len()), Some(1));
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
}
