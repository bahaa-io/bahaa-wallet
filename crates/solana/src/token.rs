use bahaa_chain::ChainError;
use bahaa_types::MAX_DECIMALS;
use ruint::UintTryTo;
use ruint::aliases::{U256, U512};

use crate::metaplex::Reader;
use crate::programs::{TOKEN, TOKEN_2022};
use crate::rpc::Account;
use crate::tx::Key;

pub(crate) const TOKEN_READ: usize = 109;
const ACCOUNT_LEN: usize = 165;
const MINT_LEN: usize = 82;
const MINT_KIND: u8 = 1;
const ACCOUNT_KIND: u8 = 2;
const HEADER: usize = 4;
const MULTISIG_LEN: usize = 355;
const FROZEN: u8 = 2;

const TRANSFER_FEE_CONFIG: u16 = 1;
const TRANSFER_FEE_AMOUNT: u16 = 2;
const DEFAULT_ACCOUNT_STATE: u16 = 6;
const IMMUTABLE_OWNER: u16 = 7;
const MEMO_TRANSFER: u16 = 8;
const NON_TRANSFERABLE: u16 = 9;
const INTEREST_BEARING: u16 = 10;
const NON_TRANSFERABLE_ACCOUNT: u16 = 13;
const TRANSFER_HOOK: u16 = 14;
const TRANSFER_HOOK_ACCOUNT: u16 = 15;
const METADATA_POINTER: u16 = 18;
const TOKEN_METADATA: u16 = 19;
const SCALED_UI_AMOUNT: u16 = 25;
const PAUSABLE: u16 = 26;
const PAUSABLE_ACCOUNT: u16 = 27;

const SECONDS_PER_YEAR: u128 = 31_556_736;
const BASIS_POINTS: u128 = 10_000;
const PLACES: u8 = 40;
const MAX_EXPONENT: u8 = 64;
const HALVINGS: usize = 8;
const TERMS: u64 = 40;
const MIN_SHIFT: i64 = -180;
const MAX_SHIFT: i64 = 128;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Mint {
    pub program: Key,
    pub decimals: u8,
    pub scale: Scale,
    pub fee: Option<FeeConfig>,
    pub blocked: Option<Blocked>,
    pub starts_frozen: bool,
    pub named: Option<(String, String)>,
    pub opening: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Blocked {
    Bound,
    Paused,
    Hooked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FeeConfig {
    older: Fee,
    newer: Fee,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Fee {
    epoch: u64,
    maximum: u64,
    points: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Scale {
    times: U256,
    over: U256,
    nearest: bool,
    varies: bool,
}

impl Mint {
    pub(crate) fn read(account: Option<&Account>, key: &Key, now: u64) -> Result<Self, ChainError> {
        let not_token = || ChainError::Invalid("this address is not a token");
        let account = account.ok_or_else(not_token)?;
        let data = &account.data;
        let decimals = match data.get(44..46) {
            Some([decimals, 1]) if *decimals <= MAX_DECIMALS => *decimals,
            _ => return Err(not_token()),
        };
        let extensions = match account.owner {
            TOKEN if data.len() == MINT_LEN => Vec::new(),
            TOKEN_2022 => entries(data, MINT_LEN, MINT_KIND).map_err(|_| not_token())?,
            _ => return Err(not_token()),
        };
        let mut mint = Self {
            program: account.owner,
            decimals,
            scale: Scale::PLAIN,
            fee: None,
            blocked: None,
            starts_frozen: false,
            named: None,
            opening: opening(account.owner, &extensions)?,
        };
        let mut pointer = None;
        let mut named = None;
        let mut scaled = false;
        let (mut bound, mut paused, mut hooked) = (false, false, false);
        for (kind, value) in extensions {
            let field = |at: usize| -> Result<[u8; 8], ChainError> {
                value
                    .get(at..at + 8)
                    .and_then(|bytes| bytes.try_into().ok())
                    .ok_or_else(ChainError::malformed)
            };
            match kind {
                TRANSFER_FEE_CONFIG => {
                    sized(value, 108)?;
                    mint.fee = Some(FeeConfig {
                        older: Fee::read(&value[72..90])?,
                        newer: Fee::read(&value[90..108])?,
                    });
                }
                TRANSFER_HOOK => {
                    sized(value, 64)?;
                    hooked = set(&value[32..64]).is_some();
                }
                NON_TRANSFERABLE => {
                    sized(value, 0)?;
                    bound = true;
                }
                PAUSABLE => {
                    sized(value, 33)?;
                    paused = flag(value[32])?;
                }
                DEFAULT_ACCOUNT_STATE => {
                    sized(value, 1)?;
                    mint.starts_frozen = value[0] == FROZEN;
                }
                METADATA_POINTER => {
                    sized(value, 64)?;
                    pointer = set(&value[32..64]);
                }
                TOKEN_METADATA => named = token_metadata(value, key),
                SCALED_UI_AMOUNT if !scaled => {
                    sized(value, 56)?;
                    scaled = true;
                    let current = if i128::from(now) >= i128::from(i64::from_le_bytes(field(40)?)) {
                        field(48)?
                    } else {
                        field(32)?
                    };
                    mint.scale = Scale::multiplier(u64::from_le_bytes(current))?;
                }
                INTEREST_BEARING if !scaled => {
                    sized(value, 52)?;
                    scaled = true;
                    let rate = |at: usize| i16::from_le_bytes([value[at], value[at + 1]]);
                    mint.scale = Scale::interest(
                        [field(32)?, field(42)?].map(i64::from_le_bytes),
                        [rate(40), rate(50)],
                        now,
                    )?;
                }
                SCALED_UI_AMOUNT | INTEREST_BEARING => return Err(ChainError::malformed()),
                _ => {}
            }
        }
        if pointer == Some(*key) {
            mint.named = named;
        }
        mint.blocked = [
            (bound, Blocked::Bound),
            (paused, Blocked::Paused),
            (hooked, Blocked::Hooked),
        ]
        .into_iter()
        .find_map(|(on, why)| on.then_some(why));
        Ok(mint)
    }
}

impl Fee {
    fn read(bytes: &[u8]) -> Result<Self, ChainError> {
        let word = |at: usize| -> Result<u64, ChainError> {
            bytes
                .get(at..at + 8)
                .and_then(|slice| slice.try_into().ok())
                .map(u64::from_le_bytes)
                .ok_or_else(ChainError::malformed)
        };
        Ok(Self {
            epoch: word(0)?,
            maximum: word(8)?,
            points: bytes
                .get(16..18)
                .map(|slice| u16::from_le_bytes([slice[0], slice[1]]))
                .ok_or_else(ChainError::malformed)?,
        })
    }
}

impl FeeConfig {
    pub(crate) fn fee(&self, epoch: u64, units: u64) -> u64 {
        let fee = if epoch >= self.newer.epoch {
            self.newer
        } else {
            self.older
        };
        let charged = (u128::from(units) * u128::from(fee.points)).div_ceil(BASIS_POINTS);
        u64::try_from(charged).map_or(fee.maximum, |charged| charged.min(fee.maximum))
    }
}

impl Scale {
    pub(crate) const PLAIN: Self = Self {
        times: U256::ONE,
        over: U256::ONE,
        nearest: false,
        varies: false,
    };

    pub(crate) fn varies(&self) -> bool {
        self.varies
    }

    fn multiplier(bits: u64) -> Result<Self, ChainError> {
        let unusable = || ChainError::Unsupported("tokens with this amount scale");
        let exponent = (bits >> 52) & 0x7ff;
        if bits >> 63 == 1 || exponent == 0 || exponent == 0x7ff {
            return Err(unusable());
        }
        let mantissa = U256::from((bits & ((1 << 52) - 1)) | (1 << 52));
        let shift = i64::try_from(exponent).map_err(|_| unusable())? - 1075;
        if !(MIN_SHIFT..=MAX_SHIFT).contains(&shift) {
            return Err(unusable());
        }
        let places = usize::try_from(shift.unsigned_abs()).map_err(|_| unusable())?;
        Ok(if shift >= 0 {
            Self {
                times: mantissa << places,
                over: U256::ONE,
                nearest: false,
                varies: true,
            }
        } else {
            Self {
                times: mantissa,
                over: U256::ONE << places,
                nearest: false,
                varies: true,
            }
        })
    }

    fn interest(times: [i64; 2], rates: [i16; 2], now: u64) -> Result<Self, ChainError> {
        let [started, updated] = times.map(i128::from);
        let now = i128::from(now);
        let exponent =
            i128::from(rates[0]) * (updated - started) + i128::from(rates[1]) * (now - updated);
        Ok(Self {
            times: exp(exponent, SECONDS_PER_YEAR * BASIS_POINTS)
                .ok_or(ChainError::Unsupported("tokens with this much interest"))?,
            over: U256::from(10u8).pow(U256::from(PLACES)),
            nearest: true,
            varies: true,
        })
    }

    pub(crate) fn shown(&self, raw: u64) -> Result<U256, ChainError> {
        let product = U512::from(raw) * U512::from(self.times);
        let over = U512::from(self.over);
        let shown = if self.nearest {
            (product + over / U512::from(2u8)) / over
        } else {
            product / over
        };
        shown.uint_try_to().map_err(|_| ChainError::overflow())
    }

    pub(crate) fn raw(&self, shown: U256) -> Result<u64, ChainError> {
        if self.shown(u64::MAX)? < shown {
            return Err(ChainError::Invalid("the amount is too large"));
        }
        let (mut low, mut high) = (0u64, u64::MAX);
        while low < high {
            let middle = low + (high - low) / 2;
            if self.shown(middle)? >= shown {
                high = middle;
            } else {
                low = middle + 1;
            }
        }
        Ok(if self.shown(low)? > shown {
            low.saturating_sub(1)
        } else {
            low
        })
    }
}

fn exp(numerator: i128, denominator: u128) -> Option<U256> {
    let one = U512::from(10u8).pow(U512::from(PLACES));
    let x = U512::from(numerator.unsigned_abs()) * one / U512::from(denominator);
    if x > U512::from(MAX_EXPONENT) * one {
        return None;
    }
    let small = x >> HALVINGS;
    let mut sum = one;
    let mut term = one;
    for step in 1..=TERMS {
        term = term * small / (one * U512::from(step));
        if term.is_zero() {
            break;
        }
        sum += term;
    }
    for _ in 0..HALVINGS {
        sum = sum * sum / one;
    }
    let value = if numerator < 0 { one * one / sum } else { sum };
    value.uint_try_to().ok()
}

pub(crate) fn held(
    account: Option<&Account>,
    program: &Key,
    owner: &Key,
    mint: &Key,
) -> Result<u64, ChainError> {
    let Some(account) = account.filter(|account| account.owner == *program) else {
        return Ok(0);
    };
    let data = &account.data;
    if data.len() < TOKEN_READ || data[..32] != mint[..] {
        return Err(ChainError::Network(
            "an endpoint sent a token account for another token".to_owned(),
        ));
    }
    if data[32..64] != owner[..] || !matches!(data[108], 1 | FROZEN) {
        return Ok(0);
    }
    data[64..72]
        .try_into()
        .map(u64::from_le_bytes)
        .map_err(|_| ChainError::malformed())
}

pub(crate) fn frozen(account: &Account) -> bool {
    account.data.get(108) == Some(&FROZEN)
}

pub(crate) fn needs_memo(account: &Account) -> Result<bool, ChainError> {
    if account.owner != TOKEN_2022 {
        return Ok(false);
    }
    Ok(entries(&account.data, ACCOUNT_LEN, ACCOUNT_KIND)?
        .iter()
        .any(|(kind, value)| *kind == MEMO_TRANSFER && value.first() == Some(&1)))
}

fn entries(data: &[u8], base: usize, kind: u8) -> Result<Vec<(u16, &[u8])>, ChainError> {
    if data.len() == base {
        return Ok(Vec::new());
    }
    let (head, mut rest) = data
        .split_at_checked(ACCOUNT_LEN + 1)
        .ok_or_else(ChainError::malformed)?;
    if head[base..ACCOUNT_LEN].iter().any(|byte| *byte != 0) || head[ACCOUNT_LEN] != kind {
        return Err(ChainError::malformed());
    }
    let mut found: Vec<(u16, &[u8])> = Vec::new();
    while let Some((header, after)) = rest.split_at_checked(HEADER) {
        let kind = u16::from_le_bytes([header[0], header[1]]);
        if kind == 0 {
            break;
        }
        let (value, next) = after
            .split_at_checked(usize::from(u16::from_le_bytes([header[2], header[3]])))
            .ok_or_else(ChainError::malformed)?;
        if found.iter().any(|(seen, _)| *seen == kind) {
            return Err(ChainError::malformed());
        }
        found.push((kind, value));
        rest = next;
    }
    Ok(found)
}

fn opening(program: Key, extensions: &[(u16, &[u8])]) -> Result<u64, ChainError> {
    let len = if program == TOKEN {
        ACCOUNT_LEN
    } else {
        tlv_len(extensions)
    };
    u64::try_from(len).map_err(|_| ChainError::overflow())
}

fn tlv_len(extensions: &[(u16, &[u8])]) -> usize {
    let mut needed = vec![(IMMUTABLE_OWNER, 0)];
    for (kind, _) in extensions {
        needed.extend_from_slice(match *kind {
            TRANSFER_FEE_CONFIG => &[(TRANSFER_FEE_AMOUNT, 8)][..],
            NON_TRANSFERABLE => &[(NON_TRANSFERABLE_ACCOUNT, 0)],
            TRANSFER_HOOK => &[(TRANSFER_HOOK_ACCOUNT, 1)],
            PAUSABLE => &[(PAUSABLE_ACCOUNT, 0)],
            _ => &[],
        });
    }
    needed.sort_unstable();
    needed.dedup();
    let len = needed
        .iter()
        .fold(ACCOUNT_LEN + 1, |len, (_, size)| len + HEADER + size);
    if len == MULTISIG_LEN { len + 2 } else { len }
}

fn token_metadata(value: &[u8], key: &Key) -> Option<(String, String)> {
    let mut read = Reader(value);
    read.key()?;
    if read.key()? != *key {
        return None;
    }
    Some((read.text()?, read.text()?))
}

fn sized(value: &[u8], len: usize) -> Result<(), ChainError> {
    if value.len() == len {
        Ok(())
    } else {
        Err(ChainError::malformed())
    }
}

fn set(bytes: &[u8]) -> Option<Key> {
    bytes
        .try_into()
        .ok()
        .filter(|key: &Key| key.iter().any(|byte| *byte != 0))
}

fn flag(byte: u8) -> Result<bool, ChainError> {
    match byte {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(ChainError::malformed()),
    }
}

#[cfg(fuzzing)]
pub(crate) fn fuzz(account: Option<&Account>) {
    if let Ok(mint) = Mint::read(account, &[0; 32], 1_800_000_000) {
        let _ = mint.scale.shown(u64::MAX);
        let _ = mint.scale.raw(U256::from(u64::MAX));
        let _ = mint.fee.map(|fee| fee.fee(u64::MAX, u64::MAX));
    }
    let _ = held(account, &TOKEN_2022, &[0; 32], &[0; 32]);
    if let Some(account) = account {
        let _ = (frozen(account), needs_memo(account));
    }
}

#[cfg(test)]
mod tests {
    use bahaa_types::Amount;
    use serde_json::Value;

    use super::*;

    fn recorded() -> Value {
        serde_json::from_str(include_str!("../tests/fixtures/token2022.json")).unwrap()
    }

    fn key(text: &str) -> Key {
        bs58::decode(text).into_vec().unwrap().try_into().unwrap()
    }

    fn account(value: &Value) -> Account {
        crate::rpc::account(value).unwrap().unwrap()
    }

    fn real(name: &str) -> (Key, Account, u64) {
        let all = recorded();
        let mint = &all["mints"][name];
        (
            key(mint["key"].as_str().unwrap()),
            account(&mint["account"]),
            all["recorded"].as_u64().unwrap(),
        )
    }

    fn token(program: Key, owner: &Key, mint: &Key, units: u64, state: u8) -> Account {
        let mut data = mint.to_vec();
        data.extend_from_slice(owner);
        data.extend_from_slice(&units.to_le_bytes());
        data.resize(ACCOUNT_LEN, 0);
        data[108] = state;
        Account {
            lamports: 2_039_280,
            owner: program,
            executable: false,
            space: 165,
            data,
        }
    }

    fn mint(owner: Key, decimals: u8, initialized: u8) -> Account {
        let mut data = vec![0; MINT_LEN];
        data[44] = decimals;
        data[45] = initialized;
        Account {
            lamports: 1,
            owner,
            executable: false,
            space: 82,
            data,
        }
    }

    fn extended(entries: &[(u16, &[u8])]) -> Account {
        let mut account = mint(TOKEN_2022, 6, 1);
        account.data.resize(ACCOUNT_LEN, 0);
        account.data.push(MINT_KIND);
        for (kind, value) in entries {
            account.data.extend_from_slice(&kind.to_le_bytes());
            account
                .data
                .extend_from_slice(&u16::try_from(value.len()).unwrap().to_le_bytes());
            account.data.extend_from_slice(value);
        }
        account
    }

    #[test]
    fn real_mints_read_as_their_program_describes_them() {
        let all = recorded();
        for name in ["PYUSD", "AAPLx", "BERN", "sUSD", "PUMP"] {
            let (key, account, now) = real(name);
            let found = Mint::read(Some(&account), &key, now).unwrap();
            assert_eq!(found.program, TOKEN_2022, "{name}");
            assert_eq!(
                found.opening,
                all["accounts"][name]["account"]["space"].as_u64().unwrap(),
                "{name}"
            );
            assert_eq!(found.blocked, None, "{name}");
            assert!(!found.starts_frozen, "{name}");
        }
        let (key, account, now) = real("PYUSD");
        let pyusd = Mint::read(Some(&account), &key, now).unwrap();
        assert_eq!(pyusd.decimals, 6);
        assert_eq!(
            pyusd.named,
            Some(("PayPal USD".to_owned(), "PYUSD".to_owned()))
        );
        assert_eq!(pyusd.fee.unwrap().fee(9_999, 1_000_000), 0);
        let (key, account, now) = real("BERN");
        let bern = Mint::read(Some(&account), &key, now).unwrap();
        assert_eq!(bern.named, None);
        let fees = bern.fee.unwrap();
        assert_eq!(fees.fee(697, 10_000), 420);
        assert_eq!(fees.fee(698, 10_000), 269);
        assert_eq!(fees.fee(698, 1), 1);
        assert_eq!(fees.fee(698, 0), 0);
        let (key, account, now) = real("AAPLx");
        assert_eq!(
            Mint::read(Some(&account), &key, now).unwrap().named,
            Some(("Apple xStock".to_owned(), "AAPLx".to_owned()))
        );
    }

    #[test]
    fn shown_amounts_match_what_the_token_program_answers() {
        let all = recorded();
        for name in ["AAPLx", "sUSD", "PYUSD"] {
            let (key, account, now) = real(name);
            let found = Mint::read(Some(&account), &key, now).unwrap();
            for pair in all["shown"][name].as_array().unwrap() {
                let raw: u64 = pair[0].as_str().unwrap().parse().unwrap();
                let shown = Amount::new(found.scale.shown(raw).unwrap(), found.decimals).unwrap();
                assert_eq!(shown.to_string(), pair[1].as_str().unwrap(), "{name} {raw}");
            }
        }
    }

    #[test]
    fn typed_amounts_become_the_largest_raw_amount_that_shows_no_more() {
        let (key, account, now) = real("AAPLx");
        let scale = Mint::read(Some(&account), &key, now).unwrap().scale;
        for raw in [0, 1, 7, 99_621_215, 123_456_789_012] {
            assert_eq!(scale.raw(scale.shown(raw).unwrap()).unwrap(), raw);
        }
        let typed = U256::from(100_000_000u64);
        let raw = scale.raw(typed).unwrap();
        assert_eq!(scale.shown(raw).unwrap(), typed);
        let skipped = (0..1_000u64)
            .find(|raw| {
                scale.shown(raw + 1).unwrap() - scale.shown(*raw).unwrap() == U256::from(2u8)
            })
            .unwrap();
        let unreachable = scale.shown(skipped).unwrap() + U256::ONE;
        let raw = scale.raw(unreachable).unwrap();
        assert_eq!(raw, skipped);
        assert!(scale.shown(raw).unwrap() < unreachable);
        assert!(scale.shown(raw + 1).unwrap() > unreachable);
        assert_eq!(Scale::PLAIN.raw(U256::from(42u8)).unwrap(), 42);
        assert!(Scale::PLAIN.raw(U256::from(u64::MAX) + U256::ONE).is_err());
        let halving = Scale::multiplier(0.5f64.to_bits()).unwrap();
        assert_eq!(halving.shown(3).unwrap(), U256::from(1u8));
        assert_eq!(halving.raw(U256::from(1u8)).unwrap(), 2);
    }

    #[test]
    fn interest_is_compounded_continuously_like_the_program_does() {
        let one = U256::from(10u8).pow(U256::from(PLACES));
        let year = i128::try_from(SECONDS_PER_YEAR * BASIS_POINTS).unwrap();
        let denominator = SECONDS_PER_YEAR * BASIS_POINTS;
        assert_eq!(exp(0, denominator), Some(one));
        let e = U256::from_str_radix("27182818284590452353602874713526624977572", 10).unwrap();
        let close = |value: U256, expected: U256| {
            let gap = if value > expected {
                value - expected
            } else {
                expected - value
            };
            assert!(gap < U256::from(100_000u32), "{value} vs {expected}");
        };
        close(exp(year, denominator).unwrap(), e);
        close(
            exp(-year, denominator).unwrap(),
            U256::from_str_radix("3678794411714423215955237701614608674458", 10).unwrap(),
        );
        assert!(exp(year * 64, denominator).is_some());
        assert_eq!(exp(year * 65, denominator), None);
        assert!(matches!(
            Scale::interest([0, 0], [i16::MAX, i16::MAX], 1 << 40),
            Err(ChainError::Unsupported(_))
        ));
    }

    #[test]
    fn odd_multipliers_are_refused() {
        for bad in [0.0f64, -1.0, f64::NAN, f64::INFINITY, 1e-300, 1e300] {
            assert!(Scale::multiplier(bad.to_bits()).is_err(), "{bad}");
        }
        assert_eq!(
            Scale::multiplier(1.0f64.to_bits())
                .unwrap()
                .shown(5)
                .unwrap(),
            U256::from(5u8)
        );
    }

    #[test]
    fn token_balances_come_only_from_this_wallets_account_of_that_program() {
        let (owner, mint) = ([1; 32], [2; 32]);
        assert_eq!(held(None, &TOKEN, &owner, &mint), Ok(0));
        let ours = token(TOKEN, &owner, &mint, 8_509_335, 1);
        assert_eq!(held(Some(&ours), &TOKEN, &owner, &mint), Ok(8_509_335));
        assert_eq!(held(Some(&ours), &TOKEN_2022, &owner, &mint), Ok(0));
        let frozen_one = token(TOKEN, &owner, &mint, 5, FROZEN);
        assert_eq!(held(Some(&frozen_one), &TOKEN, &owner, &mint), Ok(5));
        assert!(frozen(&frozen_one) && !frozen(&ours));
        for bad in [
            token(TOKEN, &owner, &[3; 32], 5, 1),
            Account {
                data: vec![0; 64],
                ..token(TOKEN, &owner, &mint, 5, 1)
            },
        ] {
            assert!(held(Some(&bad), &TOKEN, &owner, &mint).is_err());
        }
        let theirs = token(TOKEN, &[3; 32], &mint, 5, 1);
        assert_eq!(held(Some(&theirs), &TOKEN, &owner, &mint), Ok(0));
        let closed = token(TOKEN, &owner, &mint, 5, 0);
        assert_eq!(held(Some(&closed), &TOKEN, &owner, &mint), Ok(0));
        let gifted = Account {
            lamports: 1,
            owner: [0; 32],
            executable: false,
            data: Vec::new(),
            space: 0,
        };
        assert_eq!(held(Some(&gifted), &TOKEN, &owner, &mint), Ok(0));
        let all = recorded();
        let real = &all["accounts"]["AAPLx"];
        let account = account(&real["account"]);
        let (aapl, _, _) = super::tests::real("AAPLx");
        assert_eq!(
            held(
                Some(&account),
                &TOKEN_2022,
                &key(real["owner"].as_str().unwrap()),
                &aapl
            ),
            Ok(187_360)
        );
        assert_eq!(needs_memo(&account), Ok(false));
    }

    #[test]
    fn a_recipient_can_ask_for_a_memo_on_every_incoming_transfer() {
        let mut account = token(TOKEN_2022, &[1; 32], &[2; 32], 0, 1);
        account.data.push(ACCOUNT_KIND);
        account.data.extend_from_slice(&[7, 0, 0, 0]);
        let plain = account.clone();
        account.data.extend_from_slice(&[8, 0, 1, 0, 1]);
        assert_eq!(needs_memo(&plain), Ok(false));
        assert_eq!(needs_memo(&account), Ok(true));
        account.data[ACCOUNT_LEN] = MINT_KIND;
        assert!(needs_memo(&account).is_err());
        let classic = token(TOKEN, &[1; 32], &[2; 32], 0, 1);
        assert_eq!(needs_memo(&classic), Ok(false));
    }

    #[test]
    fn only_well_formed_initialized_mints_are_tokens() {
        let key = [2; 32];
        let classic = Mint::read(Some(&mint(TOKEN, 6, 1)), &key, 0).unwrap();
        assert_eq!(
            (classic.decimals, classic.opening, classic.scale),
            (6, 165, Scale::PLAIN)
        );
        let plain = Mint::read(Some(&mint(TOKEN_2022, 9, 1)), &key, 0).unwrap();
        assert_eq!((plain.program, plain.opening), (TOKEN_2022, 170));
        for bad in [
            None,
            Some(mint(TOKEN, 6, 0)),
            Some(mint(TOKEN, 90, 1)),
            Some(mint([0; 32], 6, 1)),
            Some(Account {
                data: vec![0; 165],
                ..mint(TOKEN, 6, 1)
            }),
        ] {
            assert!(
                matches!(
                    Mint::read(bad.as_ref(), &key, 0),
                    Err(ChainError::Invalid(_))
                ),
                "{bad:?}"
            );
        }
        let mut wrong_kind = extended(&[(NON_TRANSFERABLE, &[])]);
        wrong_kind.data[ACCOUNT_LEN] = ACCOUNT_KIND;
        let mut padded = extended(&[]);
        padded.data[100] = 1;
        let mut cut = extended(&[(PAUSABLE, &[0; 33])]);
        cut.data.truncate(cut.data.len() - 1);
        let scale = [0u8; 56];
        let interest = [0u8; 52];
        for bad in [
            wrong_kind,
            padded,
            cut,
            extended(&[(PAUSABLE, &[0; 32])]),
            extended(&[(PAUSABLE, &[0; 33]), (PAUSABLE, &[0; 33])]),
            extended(&[(SCALED_UI_AMOUNT, &scale), (INTEREST_BEARING, &interest)]),
        ] {
            assert!(Mint::read(Some(&bad), &key, 0).is_err());
        }
        let mut paused = [0u8; 33];
        paused[32] = 1;
        let mut hook = [0u8; 64];
        hook[40] = 9;
        let found = Mint::read(
            Some(&extended(&[
                (PAUSABLE, &paused),
                (TRANSFER_HOOK, &hook),
                (NON_TRANSFERABLE, &[]),
                (DEFAULT_ACCOUNT_STATE, &[FROZEN]),
            ])),
            &key,
            0,
        )
        .unwrap();
        assert_eq!(found.blocked, Some(Blocked::Bound));
        assert!(found.starts_frozen);
        let paused_only = Mint::read(Some(&extended(&[(PAUSABLE, &paused)])), &key, 0).unwrap();
        assert_eq!(paused_only.blocked, Some(Blocked::Paused));
        let hooked = Mint::read(Some(&extended(&[(TRANSFER_HOOK, &hook)])), &key, 0).unwrap();
        assert_eq!(hooked.blocked, Some(Blocked::Hooked));
        assert!(
            Mint::read(Some(&extended(&[(TRANSFER_HOOK, &[0; 64])])), &key, 0)
                .unwrap()
                .blocked
                .is_none()
        );
        assert_eq!(found.opening, 166 + 4 + 4 + 5 + 4);
    }
}
