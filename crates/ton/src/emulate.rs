use std::collections::HashMap;

use bahaa_chain::ChainError;
use bahaa_types::Address;
use tycho_executor::{Executor, ExecutorInspector, ExecutorParams, ParsedConfig};
use tycho_types::boc::Boc;
use tycho_types::cell::{Cell, CellBuilder, CellSlice, HashBytes, Lazy, Load};
use tycho_types::dict::{Dict, RawDict};
use tycho_types::models::{
    AccountState, BlockchainConfig, BlockchainConfigParams, ComputePhase, LibDescr,
    OptionalAccount, ShardAccount, StdAddr, TxInfo,
};

use crate::fees::Settings;

const CONFIG_ADDRESS: i32 = 0;
const WORKCHAINS: i32 = 12;
const PRECOMPILED: i32 = 45;
const WORKCHAIN_V1: u8 = 0xa6;
const WORKCHAIN_V2: u8 = 0xa7;
const PRECOMPILED_SMC: u8 = 0xb0;
const WORKCHAIN_HEAD_BITS: u16 = 8 + 32 + 24 + 16 + 512 + 32;
const BASIC_FORMAT_BITS: u16 = 4 + 32 + 64;
const EXTENDED_FORMAT_BITS: u16 = 4 + 12 + 12 + 12 + 32;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Hop {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub gas_fees: u64,
    pub charged_gas_fees: u64,
    pub fees: u64,
    pub out: Vec<Vec<u8>>,
    pub transaction: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Emulated {
    Done(Hop),
    Missing([u8; 32]),
}

pub(crate) struct Emulator {
    config: ParsedConfig,
    params: ExecutorParams,
    precompiled: HashMap<[u8; 32], u64>,
    settings: Settings,
}

impl Emulator {
    pub(crate) fn new(
        params: &[(i32, Vec<u8>)],
        libraries: &[Vec<u8>],
        settings: Settings,
        now: u32,
    ) -> Option<Self> {
        let mut dict = Dict::<u32, Cell>::new();
        let mut address = None;
        let mut precompiled = HashMap::new();
        for (id, boc) in params {
            let cell = Boc::decode(boc).ok()?;
            let cell = match *id {
                CONFIG_ADDRESS => {
                    address = Some(cell.as_slice().ok()?.load_u256().ok()?);
                    cell
                }
                WORKCHAINS => workchains_v1(&cell)?,
                PRECOMPILED => {
                    precompiled = gas_overrides(&cell)?;
                    cell
                }
                _ => cell,
            };
            dict.set(u32::try_from(*id).ok()?, cell).ok()?;
        }
        let config = BlockchainConfig {
            address: address?,
            params: BlockchainConfigParams::from_raw(dict.root().clone()?),
        };
        let mut library_dict = Dict::new();
        for boc in libraries {
            let lib = Boc::decode(boc).ok()?;
            let mut publishers = Dict::new();
            publishers.set(HashBytes::ZERO, ()).ok()?;
            library_dict
                .set(*lib.repr_hash(), LibDescr { lib, publishers })
                .ok()?;
        }
        Some(Self {
            config: ParsedConfig::parse(config, now).ok()?,
            params: ExecutorParams {
                libraries: library_dict,
                block_unixtime: now,
                ..ExecutorParams::default()
            },
            precompiled,
            settings,
        })
    }

    pub(crate) fn run(
        &self,
        address: &Address,
        state: &[u8],
        message: &[u8],
    ) -> Result<Emulated, ChainError> {
        self.run_from(address, state, message, false)
    }

    pub(crate) fn run_settled(
        &self,
        address: &Address,
        state: &[u8],
        message: &[u8],
    ) -> Result<Emulated, ChainError> {
        self.run_from(address, state, message, true)
    }

    fn run_from(
        &self,
        address: &Address,
        state: &[u8],
        message: &[u8],
        settled: bool,
    ) -> Result<Emulated, ChainError> {
        let unreadable = || ChainError::Network("a transfer cannot be emulated".to_owned());
        let Address::Ton { workchain, hash } = address else {
            return Err(ChainError::Invalid("not a TON address"));
        };
        let mut account = if state.is_empty() {
            OptionalAccount(None)
        } else {
            Boc::decode(state)
                .ok()
                .and_then(|cell| cell.parse::<OptionalAccount>().ok())
                .ok_or_else(unreadable)?
        };
        if settled && let Some(existing) = account.0.as_mut() {
            existing.storage_stat.last_paid = self.params.block_unixtime;
            existing.storage_stat.due_payment = None;
        }
        let last_lt = account
            .0
            .as_ref()
            .map_or(0, |account| account.last_trans_lt);
        let shard = ShardAccount {
            account: Lazy::new(&account).map_err(|_| unreadable())?,
            last_trans_hash: HashBytes::ZERO,
            last_trans_lt: last_lt,
        };
        let message = Boc::decode(message).map_err(|_| unreadable())?;
        let mut inspector = ExecutorInspector::default();
        let output = Executor::new(&self.params, &self.config)
            .with_min_lt(last_lt.saturating_add(1))
            .begin_ordinary_ext(
                &StdAddr::new(*workchain, HashBytes(*hash)),
                false,
                message,
                &shard,
                Some(&mut inspector),
            )
            .map_err(|_| unreadable())?
            .commit()
            .map_err(|_| unreadable())?;
        if let Some(missing) = inspector.missing_library {
            return Ok(Emulated::Missing(missing.0));
        }
        let raw = Boc::encode(output.transaction.inner());
        let transaction = output.transaction.load().map_err(|_| unreadable())?;
        let TxInfo::Ordinary(info) = transaction.load_info().map_err(|_| unreadable())? else {
            return Err(unreadable());
        };
        let (computed, exit_code, gas_fees) = match &info.compute_phase {
            ComputePhase::Executed(phase) => (
                Some(phase.success),
                Some(phase.exit_code),
                u64::try_from(phase.gas_fees.into_inner()).unwrap_or(u64::MAX),
            ),
            ComputePhase::Skipped(_) => (None, None, 0),
        };
        let acted = info.action_phase.as_ref().map(|phase| phase.success);
        let code = match output.new_state.load_account() {
            Ok(Some(account)) => match account.state {
                AccountState::Active(init) => init.code.map(|code| code.repr_hash().0),
                _ => None,
            },
            _ => None,
        };
        let charged_gas_fees = match code.and_then(|code| self.precompiled.get(&code)) {
            Some(gas) => self.settings.gas_fee(*gas).ok_or_else(unreadable)?,
            None => gas_fees,
        };
        let mut out = Vec::new();
        for message in transaction.out_msgs.values() {
            out.push(Boc::encode(message.map_err(|_| unreadable())?));
        }
        Ok(Emulated::Done(Hop {
            success: succeeded(computed, acted, info.aborted, info.bounce_phase.is_some()),
            exit_code,
            gas_fees,
            charged_gas_fees,
            fees: u64::try_from(transaction.total_fees.tokens.into_inner()).unwrap_or(u64::MAX),
            out,
            transaction: raw,
        }))
    }
}

fn succeeded(compute: Option<bool>, action: Option<bool>, aborted: bool, bounced: bool) -> bool {
    compute == Some(true) && action == Some(true) && !aborted && !bounced
}

fn workchains_v1(param: &Cell) -> Option<Cell> {
    let all: RawDict<32> = param.parse().ok()?;
    let mut converted = RawDict::<32>::new();
    for entry in all.iter_owned() {
        let (key, value) = entry.ok()?;
        let slice = value.0.apply_allow_exotic(&value.1);
        let mut builder = CellBuilder::new();
        if slice.get_u8(0).ok()? == WORKCHAIN_V2 {
            let basic = slice.get_bit(8 + 32 + 24).ok()?;
            let format = if basic {
                BASIC_FORMAT_BITS
            } else {
                EXTENDED_FORMAT_BITS
            };
            let mut fields = slice;
            fields.skip_first(8, 0).ok()?;
            builder.store_u8(WORKCHAIN_V1).ok()?;
            builder
                .store_slice(fields.get_prefix(WORKCHAIN_HEAD_BITS - 8 + format, 0))
                .ok()?;
        } else {
            builder.store_slice(slice).ok()?;
        }
        let cell = builder.build().ok()?;
        converted
            .set(key.as_data_slice(), cell.as_slice().ok()?)
            .ok()?;
    }
    CellBuilder::build_from(&converted).ok()
}

fn gas_overrides(param: &Cell) -> Option<HashMap<[u8; 32], u64>> {
    let mut slice = param.as_slice().ok()?;
    slice.load_u8().ok()?;
    let list = Dict::<HashBytes, CellSlice<'_>>::load_from(&mut slice).ok()?;
    let mut overrides = HashMap::new();
    for entry in list.iter() {
        let (code, mut value) = entry.ok()?;
        if value.load_u8().ok()? != PRECOMPILED_SMC {
            return None;
        }
        overrides.insert(code.0, value.load_u64().ok()?);
    }
    Some(overrides)
}

pub(crate) fn destination(message: &[u8]) -> Option<(Address, u128)> {
    let parsed = Boc::decode(message)
        .ok()?
        .parse::<tycho_types::models::OwnedMessage>()
        .ok()?;
    let tycho_types::models::MsgInfo::Int(info) = parsed.info else {
        return None;
    };
    let tycho_types::models::IntAddr::Std(address) = info.dst else {
        return None;
    };
    Some((
        Address::Ton {
            workchain: address.workchain,
            hash: address.address.0,
        },
        info.value.tokens.into_inner(),
    ))
}

#[cfg(test)]
pub(crate) mod tests {
    use ruint::aliases::U256;
    use serde_json::Value;

    use super::*;
    use crate::lite::proven_config;
    use crate::tl::{self, Answer};
    use crate::wallet::{self, Order, Token};

    const TRACK: &str = include_str!("../tests/fixtures/track.json");
    pub(crate) const SENDER: &str =
        "13fd9907ed89660d9b90f925bd54a6b029cafb368a172888f1ffc6fd690624e5";
    pub(crate) const OURS: &str =
        "b7f4010ccc8540eb958f3d228284ca379595d3fa52e47072bc1ee781d35e01e7";
    pub(crate) const THEIRS: &str =
        "7f024f7ec0cc57b87eaf017046adaf9f6c521afb809a159cb2a2194f4153356f";
    const FRESH: &str = "ab757f92e7ee0e6e06002d9d6a546fbd4b01b161024f66783ebf90868a5eb73e";
    const OWNER: &str = "f8b8635d0786b77f9da660e65fd31d670a675dd4302facba692f46b571eb5360";
    const USDT_CODE: &str = "89468f02c78e570802e39979c8516fc38df07ea76a48357e0536f2ba7b3ee37b";
    const LIBRARY: &str = "8f452d7a4dfd74066b682365177259ed05734435be76b5fd4bd5d8af2b7c3d68";
    pub(crate) const NOW: u32 = 1_792_300_000;

    fn unhex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
            .collect()
    }

    fn recorded(path: &[&str]) -> Vec<u8> {
        let fixture: Value = serde_json::from_str(TRACK).unwrap();
        unhex(
            path.iter()
                .fold(&fixture, |value, key| &value[*key])
                .as_str()
                .unwrap(),
        )
    }

    pub(crate) fn ton(hash: &str) -> Address {
        Address::Ton {
            workchain: 0,
            hash: unhex(hash).try_into().unwrap(),
        }
    }

    pub(crate) fn state(hash: &str) -> Vec<u8> {
        let Some(Answer::Ok(state)) = tl::account_state(&recorded(&["accounts", hash])) else {
            panic!("an account state expected");
        };
        state.state
    }

    pub(crate) fn emulator() -> Emulator {
        emulator_with(true)
    }

    pub(crate) fn emulator_with(library: bool) -> Emulator {
        let Some(Answer::Ok(info)) = tl::config_info(&recorded(&["config"])) else {
            panic!("a config expected");
        };
        let config = proven_config(&info.block.clone(), &info).unwrap();
        let Some(Answer::Ok(entries)) = tl::library_result(&recorded(&["libraries", LIBRARY]))
        else {
            panic!("a library expected");
        };
        let libraries = if library {
            vec![entries[0].1.clone()]
        } else {
            Vec::new()
        };
        Emulator::new(
            &config.encoded().unwrap(),
            &libraries,
            config.settings(NOW).unwrap(),
            NOW,
        )
        .unwrap()
    }

    pub(crate) fn order(units: u64, attached: u64) -> Order {
        Order {
            wallet: ton(SENDER),
            contract: bahaa_types::TonContract::W5,
            wallet_id: wallet::wallet_id(-239),
            seqno: 11,
            valid_until: NOW + 300,
            to: ton(OWNER),
            nano: U256::from(attached),
            bounce: true,
            token: Some(Token {
                wallet: ton(OURS),
                units: U256::from(units),
                query_id: 5,
            }),
        }
    }

    pub(crate) fn done(emulated: Emulated) -> Hop {
        let Emulated::Done(hop) = emulated else {
            panic!("a finished hop expected");
        };
        hop
    }

    #[test]
    fn the_network_config_is_made_readable_for_the_executor() {
        let Some(Answer::Ok(info)) = tl::config_info(&recorded(&["config"])) else {
            panic!("a config expected");
        };
        let params = proven_config(&info.block.clone(), &info)
            .unwrap()
            .encoded()
            .unwrap();
        let find = |id: i32| {
            Boc::decode(&params.iter().find(|(known, _)| *known == id).unwrap().1).unwrap()
        };
        let workchains = find(WORKCHAINS);
        let original: RawDict<32> = workchains.parse().unwrap();
        let (_, value) = original.iter_owned().next().unwrap().unwrap();
        assert_eq!(
            value.0.apply_allow_exotic(&value.1).get_u8(0).unwrap(),
            WORKCHAIN_V2
        );
        let converted: Dict<i32, tycho_types::models::WorkchainDescription> =
            workchains_v1(&workchains).unwrap().parse().unwrap();
        let basechain = converted.get(0).unwrap().unwrap();
        assert!(basechain.active && basechain.accept_msgs);
        assert_eq!(
            gas_overrides(&find(PRECOMPILED)).unwrap(),
            HashMap::from([(unhex(USDT_CODE).try_into().unwrap(), 1_000)])
        );
    }

    #[test]
    fn a_usdt_hop_is_charged_the_networks_fixed_gas() {
        let emulator = emulator();
        let message = wallet::delivered(&order(1_000_000, 50_000_000)).unwrap();
        let first = done(emulator.run(&ton(OURS), &state(OURS), &message).unwrap());
        assert!(first.success, "{first:?}");
        assert_eq!(first.charged_gas_fees, 66_668);
        assert!(first.gas_fees > first.charged_gas_fees);
        let [forwarded] = first.out.as_slice() else {
            panic!("one forwarded transfer expected");
        };
        let (to, value) = destination(forwarded).unwrap();
        assert_eq!(to, ton(THEIRS));
        assert!(value < 50_000_000);
        let second = done(
            emulator
                .run(&ton(THEIRS), &state(THEIRS), forwarded)
                .unwrap(),
        );
        assert!(second.success, "{second:?}");
        let targets: Vec<Address> = second
            .out
            .iter()
            .map(|message| destination(message).unwrap().0)
            .collect();
        assert_eq!(targets, [ton(OWNER), ton(SENDER)]);
        let mut fresh = order(1_000_000, 50_000_000);
        fresh.to = Address::Ton {
            workchain: 0,
            hash: [0x5c; 32],
        };
        let first = done(
            emulator
                .run(
                    &ton(OURS),
                    &state(OURS),
                    &wallet::delivered(&fresh).unwrap(),
                )
                .unwrap(),
        );
        let [forwarded] = first.out.as_slice() else {
            panic!("one forwarded transfer expected");
        };
        assert_eq!(destination(forwarded).unwrap().0, ton(FRESH));
        let deployed = done(emulator.run(&ton(FRESH), &[], forwarded).unwrap());
        assert!(deployed.success, "{deployed:?}");
        assert_eq!(deployed.charged_gas_fees, 66_668);
    }

    #[test]
    fn a_transfer_the_sender_cannot_cover_is_refused() {
        let emulator = emulator();
        let message = wallet::delivered(&order(u64::MAX >> 8, 50_000_000)).unwrap();
        let refused = done(emulator.run(&ton(OURS), &state(OURS), &message).unwrap());
        assert!(!refused.success);
        assert!(refused.exit_code.is_some_and(|code| code != 0));
        let missing = Emulator::new(&[], &[], emulator.settings, NOW);
        assert!(missing.is_none());
    }

    #[test]
    fn a_hop_succeeds_only_when_every_phase_does() {
        assert!(succeeded(Some(true), Some(true), false, false));
        for failed in [
            succeeded(Some(false), Some(true), false, false),
            succeeded(None, Some(true), false, false),
            succeeded(Some(true), Some(false), false, false),
            succeeded(Some(true), None, false, false),
            succeeded(Some(true), Some(true), true, false),
            succeeded(Some(true), Some(true), false, true),
        ] {
            assert!(!failed);
        }
    }
}
