use bahaa_chain::ChainError;
use bahaa_types::Address;
use num_bigint::Sign;
use ruint::aliases::U256;
use tycho_types::boc::Boc;
use tycho_types::cell::{Cell, CellBuilder, HashBytes};
use tycho_types::dict::{Dict, RawDict};
use tycho_types::models::{BlockchainConfigParams, LibDescr, OptionalAccount, StdAddr};
use tycho_vm::{BehaviourModifiers, OwnedCellSlice, RcStackValue, VmCaller, VmGetterArgs, tuple};

use crate::http::Item;

const STORAGE_PRICES: i32 = 18;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Ran {
    Done(Vec<Item>),
    Missing([u8; 32]),
}

pub(crate) fn run(
    state: &[u8],
    method: &str,
    arg: Option<&Address>,
    params: &[(i32, Vec<u8>)],
    libraries: &[Vec<u8>],
    now: u32,
) -> Result<Ran, ChainError> {
    let account = Boc::decode(state)
        .ok()
        .and_then(|cell| cell.parse::<OptionalAccount>().ok())
        .and_then(|account| account.0)
        .ok_or_else(unreadable)?;
    let caller = VmCaller {
        libraries: library_dict(libraries).ok_or_else(unreadable)?,
        behaviour_modifiers: BehaviourModifiers::default(),
        config: config(params, now).ok_or_else(unreadable)?,
    };
    let stack: Vec<RcStackValue> = match arg {
        Some(address) => tuple![slice argument(address)?],
        None => Vec::new(),
    };
    let mut args = VmGetterArgs::default();
    args.now = now;
    let output = caller
        .call_getter_ext(&account, method, stack, &args, None)
        .map_err(|_| unreadable())?;
    if let Some(hash) = output.missing_library {
        return Ok(Ran::Missing(hash.0));
    }
    if !output.success {
        return Err(ChainError::Rejected(format!(
            "the contract method {method} stopped with exit code {}",
            output.exit_code
        )));
    }
    output
        .stack
        .iter()
        .map(item)
        .collect::<Option<Vec<_>>>()
        .map(Ran::Done)
        .ok_or_else(unreadable)
}

pub(crate) fn library_hash(boc: &[u8]) -> Option<[u8; 32]> {
    Boc::decode(boc).ok().map(|cell| cell.repr_hash().0)
}

fn argument(address: &Address) -> Result<OwnedCellSlice, ChainError> {
    let Address::Ton { workchain, hash } = address else {
        return Err(ChainError::Invalid("not a TON address"));
    };
    let cell = CellBuilder::build_from(StdAddr::new(*workchain, HashBytes(*hash)))
        .map_err(|_| ChainError::Invalid("cannot encode the method arguments"))?;
    Ok(OwnedCellSlice::new_allow_exotic(cell))
}

fn item(value: &RcStackValue) -> Option<Item> {
    if let Some(number) = value.as_int() {
        let (sign, bytes) = number.to_bytes_be();
        if sign == Sign::Minus {
            return Some(Item::Negative);
        }
        return U256::try_from_be_slice(&bytes).map(Item::Num);
    }
    if let Some(cell) = value.as_cell() {
        return Some(Item::Cell(Boc::encode(cell)));
    }
    let slice = value.as_cell_slice()?;
    let mut builder = CellBuilder::new();
    builder.store_slice(slice.apply()).ok()?;
    Some(Item::Cell(Boc::encode(builder.build().ok()?)))
}

fn config(params: &[(i32, Vec<u8>)], now: u32) -> Option<BlockchainConfigParams> {
    let mut dict = Dict::<u32, Cell>::new();
    for (id, boc) in params {
        let cell = Boc::decode(boc).ok()?;
        let cell = if *id == STORAGE_PRICES {
            effective_prices(cell, now)?
        } else {
            cell
        };
        dict.set(u32::try_from(*id).ok()?, cell).ok()?;
    }
    Some(BlockchainConfigParams::from_raw(dict.root().clone()?))
}

fn effective_prices(prices: Cell, now: u32) -> Option<Cell> {
    let all = RawDict::<32>::from(Some(prices));
    let mut latest = None;
    for entry in all.iter_owned() {
        let (key, value) = entry.ok()?;
        if key.as_data_slice().load_u32().ok()? <= now {
            latest = Some((key, value));
        }
    }
    let (key, value) = latest?;
    let mut single = RawDict::<32>::new();
    single
        .set(key.as_data_slice(), value.0.apply_allow_exotic(&value.1))
        .ok()?;
    single.root().clone()
}

fn library_dict(libraries: &[Vec<u8>]) -> Option<Dict<HashBytes, LibDescr>> {
    let mut dict = Dict::new();
    for boc in libraries {
        let lib = Boc::decode(boc).ok()?;
        let mut publishers = Dict::new();
        publishers.set(HashBytes::ZERO, ()).ok()?;
        dict.set(*lib.repr_hash(), LibDescr { lib, publishers })
            .ok()?;
    }
    Some(dict)
}

fn unreadable() -> ChainError {
    ChainError::Network("the contract state cannot be run".to_owned())
}

#[cfg(test)]
mod tests {
    use serde_json::Value;
    use tycho_types::cell::CellFamily;

    use super::*;
    use crate::lite::proven_config;
    use crate::tl::{self, Answer};
    use crate::wallet::read_address;

    const FIXTURE: &str = include_str!("../tests/fixtures/account_proofs.json");
    const MASTER: &str = "b113a994b5024a16719f69139328eb759596c38a25f59028b146fecdc3621dfe";
    const OWNER: &str = "f8b8635d0786b77f9da660e65fd31d670a675dd4302facba692f46b571eb5360";
    const WALLET: &str = "7f024f7ec0cc57b87eaf017046adaf9f6c521afb809a159cb2a2194f4153356f";
    const LIBRARY: &str = "8f452d7a4dfd74066b682365177259ed05734435be76b5fd4bd5d8af2b7c3d68";
    const NOW: u32 = 1_792_300_000;

    fn unhex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
            .collect()
    }

    fn recorded(path: &[&str]) -> Vec<u8> {
        let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
        unhex(
            path.iter()
                .fold(&fixture, |value, key| &value[*key])
                .as_str()
                .unwrap(),
        )
    }

    fn state(hash: &str) -> Vec<u8> {
        let Some(Answer::Ok(state)) = tl::account_state(&recorded(&["accounts", hash])) else {
            panic!("an account state expected");
        };
        state.state
    }

    fn params() -> Vec<(i32, Vec<u8>)> {
        let Some(Answer::Ok(info)) = tl::config_info(&recorded(&["config"])) else {
            panic!("a config expected");
        };
        proven_config(&info.block.clone(), &info)
            .unwrap()
            .encoded()
            .unwrap()
    }

    fn library() -> Vec<u8> {
        let Some(Answer::Ok(entries)) = tl::library_result(&recorded(&["libraries", LIBRARY]))
        else {
            panic!("a library expected");
        };
        entries[0].1.clone()
    }

    fn ton(hash: &str) -> Address {
        Address::Ton {
            workchain: 0,
            hash: unhex(hash).try_into().unwrap(),
        }
    }

    #[test]
    fn contracts_run_on_their_proven_state() {
        let params = params();
        let Ok(Ran::Done(found)) = run(
            &state(MASTER),
            "get_wallet_address",
            Some(&ton(OWNER)),
            &params,
            &[],
            NOW,
        ) else {
            panic!("the master should answer");
        };
        let [Item::Cell(address)] = found.as_slice() else {
            panic!("one address expected: {found:?}");
        };
        assert_eq!(read_address(address), Some(ton(WALLET)));
        assert_eq!(
            run(&state(WALLET), "get_wallet_data", None, &params, &[], NOW),
            Ok(Ran::Missing(unhex(LIBRARY).try_into().unwrap()))
        );
        let Ok(Ran::Done(data)) = run(
            &state(WALLET),
            "get_wallet_data",
            None,
            &params,
            &[library()],
            NOW,
        ) else {
            panic!("the wallet should answer");
        };
        let [
            Item::Num(balance),
            Item::Cell(owner),
            Item::Cell(master),
            Item::Cell(_),
        ] = data.as_slice()
        else {
            panic!("wallet data expected: {data:?}");
        };
        assert_eq!(*balance, U256::from(94_631_240_538_000u64));
        assert_eq!(read_address(owner), Some(ton(OWNER)));
        assert_eq!(read_address(master), Some(ton(MASTER)));
        assert_eq!(
            library_hash(&library()),
            Some(unhex(LIBRARY).try_into().unwrap())
        );
    }

    #[test]
    fn a_failing_method_is_refused() {
        assert!(matches!(
            run(&state(MASTER), "no_such_method", None, &params(), &[], NOW),
            Err(ChainError::Rejected(reason)) if reason.contains("exit code 11")
        ));
        assert!(matches!(
            run(
                &[0xb5, 0xee],
                "get_wallet_address",
                None,
                &params(),
                &[],
                NOW
            ),
            Err(ChainError::Network(_))
        ));
    }

    #[test]
    fn storage_prices_follow_the_clock() {
        let prices = params()
            .into_iter()
            .find(|(id, _)| *id == STORAGE_PRICES)
            .unwrap()
            .1;
        let since = |now: u32| {
            let single = effective_prices(Boc::decode(&prices).unwrap(), now).unwrap();
            let dict = RawDict::<32>::from(Some(single));
            let keys: Vec<u32> = dict
                .iter_owned()
                .map(|entry| entry.unwrap().0.as_data_slice().load_u32().unwrap())
                .collect();
            keys
        };
        assert_eq!(since(NOW), [1_777_500_000]);
        assert_eq!(since(1_700_000_000), [0]);
        assert!(matches!(
            run(
                &state(MASTER),
                "get_wallet_address",
                Some(&ton(OWNER)),
                &params(),
                &[],
                1_700_000_000
            ),
            Ok(Ran::Done(_))
        ));
    }

    #[test]
    fn stack_values_convert_strictly() {
        let cell = CellBuilder::build_from(StdAddr::new(0, HashBytes([7; 32]))).unwrap();
        let values: Vec<RcStackValue> = tuple![
            int 5,
            int -1,
            cell cell.clone(),
            slice OwnedCellSlice::new_allow_exotic(cell.clone()),
            [int 1]
        ];
        let converted: Vec<Option<Item>> = values.iter().map(item).collect();
        let boc = Boc::encode(&cell);
        assert_eq!(
            converted,
            [
                Some(Item::Num(U256::from(5u8))),
                Some(Item::Negative),
                Some(Item::Cell(boc.clone())),
                Some(Item::Cell(boc)),
                None
            ]
        );
        assert_eq!(
            library_dict(&[Boc::encode(Cell::empty_cell())])
                .unwrap()
                .values()
                .count(),
            1
        );
    }
}
