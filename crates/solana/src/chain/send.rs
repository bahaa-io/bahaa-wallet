use std::sync::PoisonError;
use std::time::{Duration, Instant};

use bahaa_chain::{
    ChainError, Failure, FeeItem, FeeLine, FeeQuote, Signed, Transfer, TxId, TxStatus, amount,
};
use bahaa_types::to_plain_text;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use futures_util::future::{try_join, try_join5};
use ruint::aliases::U256;
use serde_json::{Value, json};

use super::{SolanaChain, TOKEN_READ, bytes, held};
use crate::programs::{Shape, TOKEN, TOKEN_2022, token_account};
use crate::rpc::{Account, count, key, text};
use crate::tx::{self, Key};

const SIMULATION_LIMIT: u32 = 1_400_000;
const SIGNATURE_FEE: u64 = 5_000;
const MAX_PRICE: u64 = 10_000_000;
const MICRO: u128 = 1_000_000;
const TOKEN_ACCOUNT: u64 = 165;
const RENT_FOR: Duration = Duration::from_secs(600);
const MAX_REASON: usize = 200;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SolanaPlan {
    pub(super) transfer: Transfer,
    pub(super) message: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Rent {
    account: u64,
    token: u64,
}

impl SolanaChain {
    pub(super) async fn plan(
        &self,
        transfer: &Transfer,
    ) -> Result<(SolanaPlan, FeeQuote), ChainError> {
        self.sendable(transfer)?;
        let (from, to) = (bytes(&transfer.from)?, bytes(&transfer.to)?);
        let mint = transfer.asset.contract().map(bytes).transpose()?;
        let decimals = match &mint {
            None => self.network.native.decimals,
            Some(mint) => self.token_decimals(mint).await?,
        };
        if !transfer.max && transfer.amount.decimals() != decimals {
            return Err(ChainError::Invalid(
                "the amount uses the wrong number of decimals",
            ));
        }
        let token_accounts = match &mint {
            None => Vec::new(),
            Some(mint) => vec![token_account(&from, mint)?, token_account(&to, mint)?],
        };
        let (wallets, tokens, rent, blockhash, price) =
            self.read(from, to, &token_accounts).await?;
        let state = State {
            sol: Funds {
                symbol: self.network.native.symbol.clone(),
                decimals: self.network.native.decimals,
                held: wallets[0].as_ref().map_or(0, |account| account.lamports),
            },
            fresh: recipient(wallets[1].as_ref())?,
            rent,
        };
        let mut transfer = transfer.clone();
        let mut shape = Shape {
            from,
            to,
            token: mint.map(|mint| (mint, decimals)),
            units: 0,
            memo: transfer.memo.clone(),
            limit: SIMULATION_LIMIT,
            price: 0,
        };
        let opening = if let Some(mint) = &mint {
            held(tokens[1].as_ref(), &to, mint)?;
            let source = held(tokens[0].as_ref(), &from, mint)?;
            if transfer.max {
                transfer.take_all(amount(U256::from(source), decimals)?)?;
            }
            shape.units = units_of(&transfer)?;
            Funds {
                symbol: self.symbol(mint),
                decimals,
                held: source,
            }
            .cover(shape.units)?;
            let opening = if tokens[1].is_none() { rent.token } else { 0 };
            state.sol.cover(SIGNATURE_FEE.saturating_add(opening))?;
            opening
        } else {
            shape.units = if transfer.max {
                state.sol.held.saturating_sub(SIGNATURE_FEE)
            } else {
                units_of(&transfer)?
            };
            state.sol.cover(shape.units.saturating_add(SIGNATURE_FEE))?;
            state.first_deposit(shape.units)?;
            0
        };
        let consumed = self.simulate(&shape, blockhash).await?;
        shape.limit = consumed
            .checked_add(consumed / 5)
            .and_then(|limit| u32::try_from(limit).ok())
            .ok_or_else(ChainError::overflow)?
            .min(SIMULATION_LIMIT);
        shape.price = price;
        let priority = u64::try_from((u128::from(shape.limit) * u128::from(price)).div_ceil(MICRO))
            .map_err(|_| ChainError::overflow())?;
        let fee = SIGNATURE_FEE
            .checked_add(priority)
            .ok_or_else(ChainError::overflow)?;
        if mint.is_none() && transfer.max {
            shape.units = state
                .sol
                .held
                .checked_sub(fee)
                .filter(|units| *units > 0)
                .ok_or_else(|| state.sol.short(fee.saturating_add(1)))?;
            state.first_deposit(shape.units)?;
            transfer.amount = amount(U256::from(shape.units), decimals)?;
        }
        let moved = if mint.is_none() { shape.units } else { 0 };
        state.spend(&[fee, opening, moved])?;
        let message = tx::compile(from, blockhash, &shape.instructions()?)?;
        let fee = self.fee_quote(shape.limit, priority, opening)?;
        Ok((SolanaPlan { transfer, message }, fee))
    }

    async fn read(
        &self,
        from: Key,
        to: Key,
        token_accounts: &[Key],
    ) -> Result<(Vec<Option<Account>>, Vec<Option<Account>>, Rent, Key, u64), ChainError> {
        let mut writable = vec![from];
        if token_accounts.is_empty() {
            writable.push(to);
        }
        writable.extend_from_slice(token_accounts);
        try_join5(
            self.accounts(&[from, to], Some(0)),
            async {
                if token_accounts.is_empty() {
                    Ok(Vec::new())
                } else {
                    self.accounts(token_accounts, Some(TOKEN_READ)).await
                }
            },
            self.rent(),
            self.blockhash(),
            self.price(&writable),
        )
        .await
    }

    fn fee_quote(&self, limit: u32, priority: u64, opening: u64) -> Result<FeeQuote, ChainError> {
        let native = |lamports: u64| amount(U256::from(lamports), self.network.native.decimals);
        let mut lines = vec![
            FeeLine {
                item: FeeItem::Signature,
                amount: native(SIGNATURE_FEE)?,
            },
            FeeLine {
                item: FeeItem::Priority {
                    units: u64::from(limit),
                },
                amount: native(priority)?,
            },
        ];
        if opening > 0 {
            lines.push(FeeLine {
                item: FeeItem::Activation,
                amount: native(opening)?,
            });
        }
        let total = [SIGNATURE_FEE, priority, opening]
            .into_iter()
            .try_fold(0u64, u64::checked_add)
            .ok_or_else(ChainError::overflow)?;
        FeeQuote::new(native(total)?, native(total)?, lines)
    }

    fn sendable(&self, transfer: &Transfer) -> Result<(), ChainError> {
        if transfer.asset.network() != &self.network.id {
            return Err(ChainError::Invalid("the asset belongs to another network"));
        }
        if !transfer.max && transfer.amount.is_zero() {
            return Err(ChainError::Invalid("send more than zero"));
        }
        if transfer.from == transfer.to {
            return Err(ChainError::Invalid("send to another address"));
        }
        Ok(())
    }

    async fn rent(&self) -> Result<Rent, ChainError> {
        let cached = *self.rent.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((at, rent)) = cached
            && at.elapsed() < RENT_FOR
        {
            return Ok(rent);
        }
        let minimum = |size: u64| async move {
            count(
                &self
                    .rpc
                    .call("getMinimumBalanceForRentExemption", json!([size]))
                    .await?,
            )
        };
        let (account, token) = try_join(minimum(0), minimum(TOKEN_ACCOUNT)).await?;
        let rent = Rent { account, token };
        *self.rent.lock().unwrap_or_else(PoisonError::into_inner) = Some((Instant::now(), rent));
        Ok(rent)
    }

    async fn blockhash(&self) -> Result<Key, ChainError> {
        let reply = self
            .rpc
            .call("getLatestBlockhash", json!([{"commitment": "confirmed"}]))
            .await?;
        key(&reply["value"]["blockhash"])
    }

    async fn price(&self, writable: &[Key]) -> Result<u64, ChainError> {
        let keys: Vec<String> = writable.iter().map(text).collect();
        let reply = self
            .rpc
            .call("getRecentPrioritizationFees", json!([keys]))
            .await?;
        Ok(price(&reply)?.min(MAX_PRICE))
    }

    async fn simulate(&self, shape: &Shape, blockhash: Key) -> Result<u64, ChainError> {
        let message = tx::compile(shape.from, blockhash, &shape.instructions()?)?;
        let unsigned = tx::transaction(&message, &[0; 64]);
        let reply = self
            .rpc
            .call(
                "simulateTransaction",
                json!([STANDARD.encode(unsigned), {
                    "encoding": "base64",
                    "sigVerify": false,
                    "replaceRecentBlockhash": true,
                    "commitment": "confirmed"
                }]),
            )
            .await?;
        let outcome = &reply["value"];
        if !outcome["err"].is_null() {
            return Err(ChainError::Rejected(failure(&outcome["err"])));
        }
        Some(count(&outcome["unitsConsumed"])?)
            .filter(|units| *units > 0)
            .ok_or_else(|| ChainError::Network("an endpoint sent no compute estimate".to_owned()))
    }

    pub(super) async fn push(&self, signed: &Signed) -> Result<TxId, ChainError> {
        self.rpc
            .call_all(
                "sendTransaction",
                json!([STANDARD.encode(&signed.bytes), {
                    "encoding": "base64",
                    "preflightCommitment": "confirmed"
                }]),
                |reply| accepted(reply, &signed.id),
            )
            .await?;
        Ok(signed.id.clone())
    }

    pub(super) async fn lookup(&self, id: &TxId) -> Result<TxStatus, ChainError> {
        let reply = self
            .rpc
            .call(
                "getSignatureStatuses",
                json!([[id.as_str()], {"searchTransactionHistory": true}]),
            )
            .await?;
        let Some([status]) = reply["value"].as_array().map(Vec::as_slice) else {
            return Err(ChainError::malformed());
        };
        if status.is_null() {
            return Ok(TxStatus::NotFound);
        }
        if !status["err"].is_null() {
            return Ok(TxStatus::Failed(failure(&status["err"])));
        }
        match status["confirmationStatus"].as_str() {
            Some("confirmed" | "finalized") => {}
            Some("processed") | None => return Ok(TxStatus::Pending),
            Some(_) => return Err(ChainError::malformed()),
        }
        let found = self
            .rpc
            .call(
                "getTransaction",
                json!([id.as_str(), {
                    "encoding": "base64",
                    "commitment": "confirmed",
                    "maxSupportedTransactionVersion": 0
                }]),
            )
            .await?;
        if found.is_null() {
            return Ok(TxStatus::Pending);
        }
        if signature_of(&found).as_deref() != Some(id.as_str()) {
            return Err(ChainError::Network(
                "an endpoint answered for another transaction".to_owned(),
            ));
        }
        Ok(TxStatus::Confirmed {
            block: Some(count(&found["slot"])?),
            fee: Some(amount(
                U256::from(count(&found["meta"]["fee"])?),
                self.network.native.decimals,
            )?),
            transaction: Some(id.clone()),
        })
    }
}

struct Funds {
    symbol: String,
    decimals: u8,
    held: u64,
}

impl Funds {
    fn cover(&self, needed: u64) -> Result<(), ChainError> {
        if needed <= self.held {
            Ok(())
        } else {
            Err(self.short(needed))
        }
    }

    fn short(&self, needed: u64) -> ChainError {
        match (
            amount(U256::from(needed), self.decimals),
            amount(U256::from(self.held), self.decimals),
        ) {
            (Ok(needed), Ok(available)) => ChainError::Insufficient {
                symbol: self.symbol.clone(),
                needed,
                available,
            },
            (Err(err), _) | (_, Err(err)) => err,
        }
    }
}

struct State {
    sol: Funds,
    fresh: bool,
    rent: Rent,
}

impl State {
    fn first_deposit(&self, units: u64) -> Result<(), ChainError> {
        if self.fresh && units < self.rent.account {
            return Err(ChainError::Invalid(
                "a new Solana address needs a bigger first deposit, the network's minimum balance",
            ));
        }
        Ok(())
    }

    fn spend(&self, costs: &[u64]) -> Result<(), ChainError> {
        let spent = costs
            .iter()
            .try_fold(0u64, |total, cost| total.checked_add(*cost))
            .ok_or_else(ChainError::overflow)?;
        self.sol.cover(spent)?;
        let left = self.sol.held - spent;
        if left > 0 && left < self.rent.account {
            return Err(ChainError::Invalid(
                "this would leave less SOL than an account must keep, send everything with Max or leave more",
            ));
        }
        Ok(())
    }
}

fn recipient(account: Option<&Account>) -> Result<bool, ChainError> {
    let Some(account) = account else {
        return Ok(true);
    };
    if account.executable {
        return Err(ChainError::Invalid(
            "this address is a program, not a wallet",
        ));
    }
    if account.owner == TOKEN || account.owner == TOKEN_2022 {
        return Err(ChainError::Invalid(
            "this address holds tokens for a wallet, ask for the wallet's own address",
        ));
    }
    Ok(false)
}

fn units_of(transfer: &Transfer) -> Result<u64, ChainError> {
    u64::try_from(transfer.amount.units())
        .map_err(|_| ChainError::Invalid("the amount is too large"))
}

fn price(reply: &Value) -> Result<u64, ChainError> {
    let mut fees = reply
        .as_array()
        .ok_or_else(ChainError::malformed)?
        .iter()
        .map(|entry| count(&entry["prioritizationFee"]))
        .collect::<Result<Vec<u64>, _>>()?;
    fees.sort_unstable();
    Ok(fees.get(fees.len() * 3 / 4).copied().unwrap_or(0))
}

fn accepted(
    reply: Result<Value, Failure<ChainError>>,
    id: &TxId,
) -> Result<(), Failure<ChainError>> {
    match reply? {
        Value::String(sent) if sent == id.as_str() => Ok(()),
        _ => Err(Failure::Retry(ChainError::Network(
            "the node answered with another transaction id".to_owned(),
        ))),
    }
}

fn signature_of(found: &Value) -> Option<String> {
    let bytes = match found["transaction"].as_array().map(Vec::as_slice) {
        Some([Value::String(encoded), Value::String(kind)]) if kind == "base64" => {
            STANDARD.decode(encoded).ok()?
        }
        _ => return None,
    };
    Some(bs58::encode(bytes.get(1..65)?).into_string())
}

fn failure(err: &Value) -> String {
    to_plain_text(&format!("the transfer would fail: {err}"), MAX_REASON)
}

#[cfg(fuzzing)]
pub(super) fn fuzz(value: &Value) {
    let _ = price(value);
    let _ = signature_of(value);
    let _ = failure(&value["err"]);
    if let Some(Ok(id)) = value["result"].as_str().map(TxId::new) {
        let _ = accepted(Ok(value["result"].clone()), &id);
    }
    let _ = count(value);
}

#[cfg(test)]
mod tests {
    use bahaa_config::Registry;
    use bahaa_crypto::Mnemonic;
    use bahaa_types::SolanaPath;

    use super::*;

    const MAINNET: &str = r##"[{
        "id": "solana",
        "name": "Solana",
        "color": "#9945ff",
        "family": "solana",
        "genesis": "45296998a6f8e2a784db5d9f95e18fc23f70441a1039446801089879b08c7ef0",
        "testnet": false,
        "native": {"symbol": "SOL", "name": "Solana", "decimals": 9},
        "endpoints": [
            {"protocol": "solana-rpc", "url": "https://api.mainnet-beta.solana.com"},
            {"protocol": "solana-rpc", "url": "https://solana-rpc.publicnode.com"}
        ],
        "explorer": {"tx": "https://solscan.io/tx/{tx}", "address": "https://solscan.io/account/{address}"}
    }]"##;

    #[tokio::test]
    #[ignore = "talks to the real Solana mainnet"]
    async fn live_nodes_accept_our_signature_and_refuse_only_the_empty_account() {
        let registry = Registry::from_json(MAINNET).unwrap();
        let network = registry.get(&"solana".parse().unwrap()).unwrap().clone();
        let chain = SolanaChain::new(network).unwrap();
        let seed = Mnemonic::from_entropy(&[0x5c; 16]).unwrap().to_seed("");
        let key = super::super::key(&seed, 0, SolanaPath::default()).unwrap();
        let from = key.verifying_key().to_bytes();
        let blockhash = chain.blockhash().await.unwrap();
        let shape = Shape {
            from,
            to: [0x27; 32],
            token: None,
            units: 1_000_000,
            memo: Some("deposit 4417".to_owned()),
            limit: 1_000,
            price: 0,
        };
        let message = tx::compile(from, blockhash, &shape.instructions().unwrap()).unwrap();
        let (bytes, signature) = tx::sign(&message, &key);
        let signed = Signed {
            id: TxId::new(bs58::encode(signature).into_string()).unwrap(),
            bytes,
        };
        let refused = chain.push(&signed).await.unwrap_err();
        let reason = refused.to_string().to_ascii_lowercase();
        assert!(
            reason.contains("accountnotfound") || reason.contains("prior credit"),
            "{reason}"
        );
        let stranger = ed25519_dalek::SigningKey::from_bytes(&[0x42; 32]);
        let (forged, _) = tx::sign(&message, &stranger);
        for (bytes, expected) in [
            (&signed.bytes, "AccountNotFound"),
            (&forged, "SignatureFailure"),
        ] {
            let checked = chain
                .rpc
                .call(
                    "simulateTransaction",
                    json!([STANDARD.encode(bytes), {"encoding": "base64", "sigVerify": true}]),
                )
                .await
                .unwrap();
            assert_eq!(checked["value"]["err"], expected);
        }
    }

    #[test]
    fn the_priority_price_is_the_upper_quarter_of_recent_slots() {
        let reply = |fees: &[u64]| -> Value {
            fees.iter()
                .enumerate()
                .map(|(slot, fee)| json!({"slot": slot, "prioritizationFee": fee}))
                .collect()
        };
        assert_eq!(price(&reply(&[])).unwrap(), 0);
        assert_eq!(price(&reply(&[0, 0, 0, 195_096])).unwrap(), 195_096);
        assert_eq!(price(&reply(&[9, 0, 0, 0, 0, 0, 0, 0])).unwrap(), 0);
        assert_eq!(price(&reply(&[1, 2, 3, 4, 5, 6, 7, 8])).unwrap(), 7);
        assert!(price(&json!([{"prioritizationFee": -1}])).is_err());
        assert!(price(&json!({})).is_err());
    }

    #[test]
    fn a_broadcast_counts_only_when_the_node_names_our_transaction() {
        let id = TxId::new("5VERv8NMvzbJMEkV8xnrLkEaWRtSz9CosKDYjCJjBRnbJLgp8uirBgmQpjKhoR4tjF3ZpRzrFmBV6UjKdiSZkQUW").unwrap();
        assert!(accepted(Ok(json!(id.as_str())), &id).is_ok());
        assert!(matches!(
            accepted(Ok(json!("1111")), &id),
            Err(Failure::Retry(_))
        ));
        assert!(matches!(
            accepted(
                Err(Failure::Fatal(ChainError::Rejected("x".to_owned()))),
                &id
            ),
            Err(Failure::Fatal(_))
        ));
    }

    #[test]
    fn short_funds_name_what_is_missing() {
        let sol = Funds {
            symbol: "SOL".to_owned(),
            decimals: 9,
            held: 1_000,
        };
        assert!(sol.cover(1_000).is_ok());
        let ChainError::Insufficient {
            symbol,
            needed,
            available,
        } = sol.cover(1_001).unwrap_err()
        else {
            panic!("expected a shortfall");
        };
        assert_eq!(
            (symbol.as_str(), needed.to_string(), available.to_string()),
            ("SOL", "0.000001001".to_owned(), "0.000001".to_owned())
        );
    }

    #[test]
    fn failures_read_as_plain_text() {
        let reason = failure(&json!({"InstructionError": [2, {"Custom": 1}]}));
        assert!(reason.starts_with("the transfer would fail: {\"InstructionError\""));
        assert!(bahaa_types::is_plain_text(&reason, MAX_REASON));
    }
}
