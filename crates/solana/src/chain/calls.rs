use std::collections::{BTreeMap, BTreeSet};

use bahaa_chain::{
    Call, ChainError, ContractSwap, DecodedCall, FeeItem, FeeLine, FeeQuote, PlannedCall, Signed,
    Step, amount,
};
use bahaa_crypto::Seed;
use bahaa_types::{Address, Amount, AssetId, to_plain_text};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use ruint::aliases::U256;
use serde_json::{Value, json};

use super::{SolanaChain, bytes, signer};
use crate::programs::{ASSOCIATED, BUDGET, MEMO, MEMO_V1, SYSTEM, TOKEN, TOKEN_2022, WRAPPED};
use crate::rpc::{count, key, text};
use crate::tx::{self, Key, MAX_TX, Wire};

const SIGNATURE_FEE: u64 = 5_000;
const APPROVE: u8 = 4;
const SET_AUTHORITY: u8 = 6;
const APPROVE_CHECKED: u8 = 13;
const ASSIGN: u32 = 1;
const ASSIGN_WITH_SEED: u32 = 10;
const MAX_REASON: usize = 200;
const SIGNATURE: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SolanaCall {
    planned: PlannedCall,
    message: Vec<u8>,
    fee_max: Amount,
}

impl SolanaCall {
    pub fn planned(&self) -> &PlannedCall {
        &self.planned
    }

    pub fn fee_max(&self) -> Amount {
        self.fee_max
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    fee: u64,
    rent: u64,
    units: u64,
}

struct Entry {
    index: usize,
    mint: Key,
    owner: Key,
    units: u128,
}

impl SolanaChain {
    pub async fn prepare_swap(
        &self,
        swap: &ContractSwap,
    ) -> Result<(Vec<SolanaCall>, FeeQuote), ChainError> {
        let message = self.unsigned(swap)?;
        let outcome = self.simulate_swap(swap).await?;
        let native = |units: u64| amount(U256::from(units), self.network.native.decimals);
        let priority = outcome.fee.saturating_sub(SIGNATURE_FEE);
        let mut lines = vec![FeeLine {
            item: FeeItem::Signature,
            amount: native(outcome.fee.min(SIGNATURE_FEE))?,
        }];
        if priority > 0 {
            lines.push(FeeLine {
                item: FeeItem::Priority {
                    units: outcome.units,
                },
                amount: native(priority)?,
            });
        }
        if outcome.rent > 0 {
            lines.push(FeeLine {
                item: FeeItem::Activation,
                amount: native(outcome.rent)?,
            });
        }
        let total = native(
            outcome
                .fee
                .checked_add(outcome.rent)
                .ok_or_else(ChainError::overflow)?,
        )?;
        let plan = SolanaCall {
            planned: PlannedCall {
                step: Step::Swap,
                account: swap.account,
                from: swap.from,
                call: swap.call.clone(),
            },
            message,
            fee_max: total,
        };
        Ok((vec![plan], FeeQuote::new(total, total, lines)?))
    }

    pub async fn recheck(&self, swap: &ContractSwap) -> Result<(), ChainError> {
        self.unsigned(swap)?;
        self.simulate_swap(swap).await.map(|_| ())
    }

    pub fn sign_call(&self, plan: &SolanaCall, seed: &Seed) -> Result<Signed, ChainError> {
        let key = signer(seed, plan.planned.account, &plan.planned.from)?;
        let (bytes, signature) = tx::sign(&plan.message, &key);
        Ok(Signed {
            id: tx::id(&signature)?,
            bytes,
        })
    }

    pub fn decode_call(&self, signed: &Signed) -> Result<DecodedCall, ChainError> {
        let invalid = || ChainError::Invalid("not a swap transaction this wallet can read");
        let wire = tx::read(&signed.bytes).ok_or_else(invalid)?;
        let [signature] = wire.signatures.as_slice() else {
            return Err(invalid());
        };
        let payer = *wire.keys.first().ok_or_else(invalid)?;
        if wire.header[0] != 1 || tx::id(signature)? != signed.id {
            return Err(invalid());
        }
        VerifyingKey::from_bytes(&payer)
            .map_err(|_| invalid())?
            .verify(wire.message, &Signature::from_bytes(signature))
            .map_err(|_| ChainError::Mismatch("the signature"))?;
        let program = swap_program(&wire).ok_or_else(invalid)?;
        Ok(DecodedCall {
            from: Address::Solana(payer),
            call: Call {
                to: Address::Solana(program),
                value: amount(U256::ZERO, self.network.native.decimals)?,
                data: tx::transaction(wire.message, &[0; SIGNATURE]),
            },
            sequence: None,
        })
    }

    fn unsigned(&self, swap: &ContractSwap) -> Result<Vec<u8>, ChainError> {
        let network = &self.network.id;
        let elsewhere = swap.sell.network() != network
            || swap
                .buy
                .as_ref()
                .is_some_and(|(asset, _)| asset.network() != network);
        if elsewhere || swap.amount.is_zero() || swap.spender.is_some() {
            return Err(ChainError::Invalid("not a swap on this network"));
        }
        if !swap.call.value.is_zero() || swap.call.data.len() > MAX_TX + SIGNATURE + 1 {
            return Err(ChainError::Invalid(
                "the swap transaction is not one this wallet signs",
            ));
        }
        let from = bytes(&swap.from)?;
        let wire = tx::read(&swap.call.data)
            .ok_or(ChainError::Invalid("the swap transaction can't be read"))?;
        let fits = wire.signatures.len() == 1
            && wire.header[0] == 1
            && wire.keys.first() == Some(&from)
            && swap_program(&wire).map(Address::Solana) == Some(swap.call.to);
        if !fits {
            return Err(ChainError::Invalid(
                "the swap transaction needs other signers or calls another program",
            ));
        }
        Ok(wire.message.to_vec())
    }

    async fn simulate_swap(&self, swap: &ContractSwap) -> Result<Outcome, ChainError> {
        let from = bytes(&swap.from)?;
        let wire = tx::read(&swap.call.data)
            .ok_or(ChainError::Invalid("the swap transaction can't be read"))?;
        let params = json!([STANDARD.encode(&swap.call.data), {
            "encoding": "base64",
            "sigVerify": false,
            "replaceRecentBlockhash": true,
            "innerInstructions": true,
            "commitment": "confirmed",
            "accounts": {"addresses": [text(&from)], "encoding": "base64"},
        }]);
        let reply = self.rpc.call("simulateTransaction", params).await?;
        judge(&wire, &from, swap, &reply["value"])
    }
}

pub fn swap_instruction(transaction: &[u8]) -> Option<(Key, Vec<u8>)> {
    let wire = tx::read(transaction)?;
    let program = swap_program(&wire)?;
    let instruction =
        wire.instructions.iter().rev().find(|instruction| {
            wire.keys.get(usize::from(instruction.program)) == Some(&program)
        })?;
    Some((program, instruction.data.clone()))
}

pub(super) fn swap_program(wire: &Wire<'_>) -> Option<Key> {
    let common = [SYSTEM, TOKEN, TOKEN_2022, ASSOCIATED, BUDGET, MEMO, MEMO_V1];
    wire.instructions
        .iter()
        .rev()
        .filter_map(|instruction| wire.keys.get(usize::from(instruction.program)).copied())
        .find(|program| !common.contains(program))
}

fn judge(
    wire: &Wire<'_>,
    owner: &Key,
    swap: &ContractSwap,
    value: &Value,
) -> Result<Outcome, ChainError> {
    if !value["err"].is_null() {
        return Err(ChainError::Rejected(to_plain_text(
            &format!("the swap would fail: {}", value["err"]),
            MAX_REASON,
        )));
    }
    let keys = keys(wire, value)?;
    let balances = |field: &str| -> Result<Vec<u64>, ChainError> {
        value[field]
            .as_array()
            .filter(|list| list.len() == keys.len())
            .ok_or_else(ChainError::malformed)?
            .iter()
            .map(count)
            .collect()
    };
    let (pre, post) = (balances("preBalances")?, balances("postBalances")?);
    let (pre_tokens, post_tokens) = (
        entries(&value["preTokenBalances"], keys.len())?,
        entries(&value["postTokenBalances"], keys.len())?,
    );
    let mine: BTreeSet<usize> = std::iter::once(0)
        .chain(
            pre_tokens
                .iter()
                .chain(&post_tokens)
                .filter(|entry| entry.owner == *owner)
                .map(|entry| entry.index),
        )
        .collect();
    let signed = |units: u64| i128::from(units);
    let fee = count(&value["fee"])?;
    let mut moved: BTreeMap<Option<Key>, i128> = BTreeMap::new();
    let lamports: i128 = mine
        .iter()
        .map(|index| signed(post[*index]) - signed(pre[*index]))
        .sum();
    moved.insert(None, lamports + signed(fee));
    for (entries, sign) in [(&post_tokens, 1i128), (&pre_tokens, -1)] {
        for entry in entries
            .iter()
            .filter(|entry| entry.owner == *owner && entry.mint != WRAPPED)
        {
            let units = i128::try_from(entry.units).map_err(|_| ChainError::overflow())?;
            *moved.entry(Some(entry.mint)).or_default() += sign * units;
        }
    }
    let rent = mine
        .iter()
        .filter(|index| **index != 0 && pre[**index] == 0)
        .map(|index| post[*index])
        .sum();
    check_moves(&moved, swap)?;
    check_authority(wire, value, &keys, owner)?;
    let wallet = &value["accounts"][0];
    if wallet["owner"].as_str() != Some(&text(&SYSTEM)) || wallet["data"][0] != "" {
        return Err(ChainError::Invalid(
            "the swap would hand this wallet to a program",
        ));
    }
    Ok(Outcome {
        fee,
        rent,
        units: count(&value["unitsConsumed"]).unwrap_or(0),
    })
}

fn keys(wire: &Wire<'_>, value: &Value) -> Result<Vec<Key>, ChainError> {
    let loaded = |field: &str| -> Result<Vec<Key>, ChainError> {
        match value["loadedAddresses"][field].as_array() {
            Some(list) => list.iter().map(key).collect(),
            None if wire.lookups.is_empty() => Ok(Vec::new()),
            None => Err(ChainError::malformed()),
        }
    };
    let (writable, readonly) = (loaded("writable")?, loaded("readonly")?);
    let expected: (usize, usize) = wire.lookups.iter().fold((0, 0), |(w, r), lookup| {
        (w + lookup.writable, r + lookup.readonly)
    });
    if (writable.len(), readonly.len()) != expected {
        return Err(ChainError::malformed());
    }
    Ok(wire
        .keys
        .iter()
        .copied()
        .chain(writable)
        .chain(readonly)
        .collect())
}

fn entries(value: &Value, accounts: usize) -> Result<Vec<Entry>, ChainError> {
    value
        .as_array()
        .ok_or_else(ChainError::malformed)?
        .iter()
        .map(|entry| {
            let index = usize::try_from(count(&entry["accountIndex"])?)
                .ok()
                .filter(|index| *index < accounts)
                .ok_or_else(ChainError::malformed)?;
            Ok(Entry {
                index,
                mint: key(&entry["mint"])?,
                owner: key(&entry["owner"])?,
                units: entry["uiTokenAmount"]["amount"]
                    .as_str()
                    .and_then(|text| text.parse().ok())
                    .ok_or_else(ChainError::malformed)?,
            })
        })
        .collect()
}

fn mint_of(asset: &AssetId) -> Result<Option<Key>, ChainError> {
    asset.contract().map(bytes).transpose()
}

fn check_moves(moved: &BTreeMap<Option<Key>, i128>, swap: &ContractSwap) -> Result<(), ChainError> {
    let units =
        |amount: &Amount| i128::try_from(amount.units()).map_err(|_| ChainError::overflow());
    let sold = mint_of(&swap.sell)?;
    let bought = swap
        .buy
        .as_ref()
        .map(|(asset, minimum)| Ok::<_, ChainError>((mint_of(asset)?, units(minimum)?)))
        .transpose()?;
    if moved.get(&sold).copied().unwrap_or(0) != -units(&swap.amount)? {
        return Err(ChainError::Invalid(
            "the swap would take another amount than the one shown",
        ));
    }
    if let Some((mint, minimum)) = bought
        && moved.get(&mint).copied().unwrap_or(0) < minimum
    {
        return Err(ChainError::Invalid(
            "the swap would pay less than its minimum",
        ));
    }
    let others = moved.iter().any(|(mint, change)| {
        *mint != sold && bought.is_none_or(|(bought, _)| bought != *mint) && *change < 0
    });
    if others {
        return Err(ChainError::Invalid(
            "the swap would take other assets from this wallet",
        ));
    }
    Ok(())
}

fn check_authority(
    wire: &Wire<'_>,
    value: &Value,
    keys: &[Key],
    owner: &Key,
) -> Result<(), ChainError> {
    let refused = || {
        Err(ChainError::Invalid(
            "the swap would give someone access to this wallet's assets",
        ))
    };
    for instruction in &wire.instructions {
        let program = keys
            .get(usize::from(instruction.program))
            .ok_or_else(ChainError::malformed)?;
        let accounts: Vec<Key> = instruction
            .accounts
            .iter()
            .filter_map(|index| keys.get(usize::from(*index)).copied())
            .collect();
        if dangerous(program, &accounts, &instruction.data, owner) {
            return refused();
        }
    }
    for group in value["innerInstructions"].as_array().into_iter().flatten() {
        for inner in group["instructions"].as_array().into_iter().flatten() {
            let program = key(&inner["programId"])?;
            let risky = if let Some(parsed) = inner.get("parsed") {
                parsed_dangerous(&program, parsed, owner)
            } else {
                let accounts = inner["accounts"]
                    .as_array()
                    .ok_or_else(ChainError::malformed)?
                    .iter()
                    .map(key)
                    .collect::<Result<Vec<_>, _>>()?;
                let data = inner["data"]
                    .as_str()
                    .and_then(|text| bs58::decode(text).into_vec().ok())
                    .ok_or_else(ChainError::malformed)?;
                dangerous(&program, &accounts, &data, owner)
            };
            if risky {
                return refused();
            }
        }
    }
    Ok(())
}

fn dangerous(program: &Key, accounts: &[Key], data: &[u8], owner: &Key) -> bool {
    if *program == TOKEN || *program == TOKEN_2022 {
        let authority = match data.first() {
            Some(&APPROVE) => accounts.get(2),
            Some(&APPROVE_CHECKED) => accounts.get(3),
            Some(&SET_AUTHORITY) => accounts.get(1),
            _ => None,
        };
        return authority == Some(owner);
    }
    *program == SYSTEM
        && data
            .get(..4)
            .and_then(|kind| kind.try_into().ok())
            .map(u32::from_le_bytes)
            .is_some_and(|kind| kind == ASSIGN || kind == ASSIGN_WITH_SEED)
        && accounts.first() == Some(owner)
}

fn parsed_dangerous(program: &Key, parsed: &Value, owner: &Key) -> bool {
    let info = &parsed["info"];
    let party = |field: &str| key(&info[field]).ok();
    let field = match (parsed["type"].as_str(), *program == SYSTEM) {
        (Some("approve" | "approveChecked"), false) => "owner",
        (Some("setAuthority"), false) => "authority",
        (Some("assign" | "assignWithSeed"), true) => "account",
        _ => return false,
    };
    (*program == SYSTEM || *program == TOKEN || *program == TOKEN_2022)
        && party(field).as_ref() == Some(owner)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Value {
        serde_json::from_str(include_str!("../../tests/fixtures/jupiter.json")).unwrap()
    }

    fn whale() -> Key {
        crate::rpc::key(&json!("9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM")).unwrap()
    }

    fn swap(minimum: &str, amount_sold: &str) -> (Vec<u8>, ContractSwap) {
        let fixture = fixture();
        let data = STANDARD
            .decode(fixture["swap"]["swapTransaction"].as_str().unwrap())
            .unwrap();
        let wire = tx::read(&data).unwrap();
        let program = swap_program(&wire).unwrap();
        let swap = ContractSwap {
            account: 0,
            from: Address::Solana(whale()),
            call: Call {
                to: Address::Solana(program),
                value: Amount::parse("0", 9).unwrap(),
                data: data.clone(),
            },
            spender: None,
            sell: "solana".parse().unwrap(),
            amount: Amount::parse(amount_sold, 9).unwrap(),
            buy: Some((
                "solana/EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
                    .parse()
                    .unwrap(),
                Amount::parse(minimum, 6).unwrap(),
            )),
            ours: false,
            spent: None,
        };
        (data, swap)
    }

    fn judged(minimum: &str, sold: &str) -> Result<Outcome, ChainError> {
        let (data, swap) = swap(minimum, sold);
        let wire = tx::read(&data).unwrap();
        judge(&wire, &whale(), &swap, &fixture()["simulation"]["value"])
    }

    #[test]
    fn a_real_jupiter_swap_passes_with_its_fee_and_wrapped_sol_closed() {
        let outcome = judged("9", "0.1").unwrap();
        assert_eq!(outcome.fee, 5_000);
        assert_eq!(outcome.rent, 0);
    }

    #[test]
    fn another_amount_or_a_higher_minimum_is_refused() {
        assert_eq!(
            judged("9", "0.2").unwrap_err(),
            ChainError::Invalid("the swap would take another amount than the one shown")
        );
        assert_eq!(
            judged("10000", "0.1").unwrap_err(),
            ChainError::Invalid("the swap would pay less than its minimum")
        );
    }

    #[test]
    fn a_failed_simulation_or_a_new_delegate_is_refused() {
        let (data, swap) = swap("9", "0.1");
        let wire = tx::read(&data).unwrap();
        let mut value = fixture()["simulation"]["value"].clone();
        value["err"] = json!({"InstructionError": [4, {"Custom": 6001}]});
        assert!(matches!(
            judge(&wire, &whale(), &swap, &value),
            Err(ChainError::Rejected(_))
        ));
        let refused =
            ChainError::Invalid("the swap would give someone access to this wallet's assets");
        let mut value = fixture()["simulation"]["value"].clone();
        value["innerInstructions"][0]["instructions"][0] = json!({
            "programId": text(&TOKEN),
            "accounts": [text(&[9; 32]), text(&[8; 32]), text(&whale())],
            "data": bs58::encode([APPROVE, 1, 0, 0, 0, 0, 0, 0, 0]).into_string(),
        });
        assert_eq!(judge(&wire, &whale(), &swap, &value).unwrap_err(), refused);
        let mut value = fixture()["simulation"]["value"].clone();
        value["innerInstructions"][0]["instructions"][1]["parsed"] = json!({
            "type": "setAuthority",
            "info": {"authority": text(&whale()), "account": text(&[9; 32])},
        });
        assert_eq!(judge(&wire, &whale(), &swap, &value).unwrap_err(), refused);
    }
}
