use std::sync::PoisonError;
use std::time::{Duration, Instant};

use bahaa_chain::{
    ChainError, Failure, FeeItem, FeeLine, FeeQuote, Pace, Signed, Transfer, TxId, TxStatus,
    amount, enough,
};
use bahaa_types::{Amount, to_plain_text};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use futures_util::future::{try_join, try_join3, try_join5};
use ruint::aliases::U256;
use serde_json::{Value, json};

use super::{SolanaChain, bytes};
use crate::programs::{Moved, Shape, TOKEN, TOKEN_2022, token_account, token_record};
use crate::rpc::{self, Account, count, key, text};
use crate::token::{Blocked, Mint, Scale, frozen, held, needs_memo};
use crate::tx::{self, Key};

const SIMULATION_LIMIT: u32 = 1_400_000;
const SIGNATURE_FEE: u64 = 5_000;
const MAX_PRICE: u64 = 10_000_000;
const MICRO: u128 = 1_000_000;
const TOKEN_ACCOUNT: u64 = 165;
const TOKEN_RECORD: u64 = 80;
const COMPRESSED_FEE: u64 = 6_000;
const RENT_FOR: Duration = Duration::from_secs(600);
const MAX_REASON: usize = 200;
const CONFIRM_SLOTS: u64 = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SolanaPlan {
    pub(super) transfer: Transfer,
    pub(super) message: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Rent {
    account: u64,
    token: u64,
    record: u64,
    size: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Opening {
    paid: u64,
    refund: u64,
}

impl Opening {
    fn of(tokens: &[Option<Account>], rent: Rent) -> Self {
        let account = if tokens[1].is_none() { rent.token } else { 0 };
        let [_, _, source, destination] = tokens else {
            return Self {
                paid: account,
                refund: 0,
            };
        };
        let record = if destination.is_none() {
            rent.record
        } else {
            0
        };
        Self {
            paid: account.saturating_add(record),
            refund: source.as_ref().map_or(0, |closed| closed.lamports),
        }
    }

    fn net(self) -> u64 {
        self.paid.saturating_sub(self.refund)
    }
}

impl SolanaChain {
    pub(super) async fn plan(
        &self,
        transfer: &Transfer,
    ) -> Result<(SolanaPlan, FeeQuote), ChainError> {
        self.sendable(transfer)?;
        let (from, to) = (bytes(&transfer.from)?, bytes(&transfer.to)?);
        let (moved, token) = self.moved(transfer, &from).await?;
        let (mint, decimals) = self.units_of_move(&moved, transfer)?;
        let coin = moved == Moved::Coin;
        let token_accounts = touched(&moved, mint.as_ref(), &from, &to)?;
        let program = program_fee(&moved);
        let size = token.as_ref().map_or(TOKEN_ACCOUNT, |found| found.opening);
        let ((wallets, tokens, rent, blockhash, price), epoch) = try_join(
            self.read(from, to, &token_accounts, &locked(&moved), size),
            async {
                match token.as_ref().and_then(|found| found.fee) {
                    Some(_) => self.epoch().await.map(Some),
                    None => Ok(None),
                }
            },
        )
        .await?;
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
            moved,
            units: 0,
            memo: transfer.memo.clone(),
            limit: SIMULATION_LIMIT,
            price: 0,
        };
        let (withheld, opening) = if mint.is_some() {
            let withheld =
                self.token_units(&mut shape, &mut transfer, token.as_ref(), &tokens, epoch)?;
            let opening = Opening::of(&tokens, rent);
            state
                .sol
                .cover(SIGNATURE_FEE.saturating_add(opening.paid))?;
            (withheld, opening)
        } else if !coin {
            if transfer.max {
                transfer.take_all(amount(U256::from(1u8), 0)?)?;
            }
            shape.units = 1;
            state.sol.cover(SIGNATURE_FEE.saturating_add(program))?;
            (None, Opening::default())
        } else {
            shape.units = if transfer.max {
                state.sol.held.saturating_sub(SIGNATURE_FEE)
            } else {
                units_of(&transfer)?
            };
            state.sol.cover(shape.units.saturating_add(SIGNATURE_FEE))?;
            state.first_deposit(shape.units)?;
            (None, Opening::default())
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
        if coin && transfer.max {
            shape.units = state
                .sol
                .held
                .checked_sub(fee)
                .filter(|units| *units > 0)
                .ok_or_else(|| state.sol.short(fee.saturating_add(1)))?;
            state.first_deposit(shape.units)?;
            transfer.amount = amount(U256::from(shape.units), decimals)?;
        }
        let sent = if coin { shape.units } else { 0 };
        state.spend(&[fee, opening.paid, sent, program], opening.refund)?;
        let message = tx::compile(from, blockhash, &shape.instructions()?)?;
        let mut fee = self.fee_quote(shape.limit, priority, opening.net(), program)?;
        if let Some(withheld) = withheld {
            fee = fee.withholding(withheld);
        }
        Ok((SolanaPlan { transfer, message }, fee))
    }

    fn token_units(
        &self,
        shape: &mut Shape,
        transfer: &mut Transfer,
        token: Option<&Mint>,
        tokens: &[Option<Account>],
        epoch: Option<u64>,
    ) -> Result<Option<Amount>, ChainError> {
        let (mint, decimals) = match &shape.moved {
            Moved::Token { mint, decimals, .. } => (*mint, *decimals),
            Moved::Programmable { mint, .. } => (*mint, 0),
            _ => return Err(ChainError::malformed()),
        };
        let (owner, scale) =
            token.map_or((TOKEN, Scale::PLAIN), |found| (found.program, found.scale));
        held(tokens[1].as_ref(), &owner, &shape.to, &mint)?;
        let source = held(tokens[0].as_ref(), &owner, &shape.from, &mint)?;
        if let Some(found) = token {
            sendable_token(found, tokens, shape.memo.is_some())?;
        }
        if transfer.max {
            shape.units = source;
            transfer.take_all(amount(scale.shown(source)?, decimals)?)?;
        } else {
            shape.units = scale.raw(transfer.amount.units())?;
            if shape.units == 0 {
                return Err(ChainError::Invalid(
                    "the amount is too small for this token",
                ));
            }
            transfer.amount = amount(scale.shown(shape.units)?, decimals)?;
        }
        enough(
            &self.symbol(&mint),
            scale.shown(shape.units)?,
            scale.shown(source)?,
            decimals,
        )?;
        let (Some(config), Moved::Token { fee, .. }) =
            (token.and_then(|found| found.fee), &mut shape.moved)
        else {
            return Ok(None);
        };
        let charged = config.fee(epoch.ok_or_else(ChainError::malformed)?, shape.units);
        *fee = Some(charged);
        if charged == 0 {
            return Ok(None);
        }
        Ok(Some(amount(scale.shown(charged)?, decimals)?))
    }

    async fn read(
        &self,
        from: Key,
        to: Key,
        token_accounts: &[Key],
        locked: &[Key],
        size: u64,
    ) -> Result<(Vec<Option<Account>>, Vec<Option<Account>>, Rent, Key, u64), ChainError> {
        let mut writable = vec![from];
        if token_accounts.is_empty() {
            writable.push(to);
        }
        writable.extend_from_slice(token_accounts);
        writable.extend_from_slice(locked);
        try_join5(
            self.accounts(&[from, to], Some(0)),
            async {
                if token_accounts.is_empty() {
                    Ok(Vec::new())
                } else {
                    self.accounts(token_accounts, None).await
                }
            },
            self.rent(size),
            self.blockhash(),
            self.price(&writable),
        )
        .await
    }

    fn fee_quote(
        &self,
        limit: u32,
        priority: u64,
        opening: u64,
        program: u64,
    ) -> Result<FeeQuote, ChainError> {
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
        if program > 0 {
            lines.push(FeeLine {
                item: FeeItem::Contracts,
                amount: native(program)?,
            });
        }
        let total = [SIGNATURE_FEE, priority, opening, program]
            .into_iter()
            .try_fold(0u64, u64::checked_add)
            .ok_or_else(ChainError::overflow)?;
        Ok(FeeQuote::new(native(total)?, native(total)?, lines)?
            .paced(Pace::blocks(CONFIRM_SLOTS, self.network.block_ms)))
    }

    async fn moved(
        &self,
        transfer: &Transfer,
        from: &Key,
    ) -> Result<(Moved, Option<Mint>), ChainError> {
        let Some(nft) = transfer.asset.nft() else {
            return Ok(match transfer.asset.contract() {
                None => (Moved::Coin, None),
                Some(mint) => {
                    let mint = bytes(mint)?;
                    let found = self
                        .mints(&[mint], true)
                        .await?
                        .into_iter()
                        .next()
                        .ok_or_else(ChainError::malformed)?;
                    self.quote_mint(&found, mint);
                    (
                        Moved::Token {
                            mint,
                            decimals: found.decimals,
                            program: found.program,
                            fee: None,
                        },
                        Some(found),
                    )
                }
            });
        };
        if !transfer.max && transfer.amount.units() != U256::from(1u8) {
            return Err(ChainError::Invalid(
                "a Solana NFT is one of a kind, send just 1",
            ));
        }
        Ok((self.nft_move(from, &bytes(nft.address())?).await?, None))
    }

    fn units_of_move(
        &self,
        moved: &Moved,
        transfer: &Transfer,
    ) -> Result<(Option<Key>, u8), ChainError> {
        let (mint, decimals) = match moved {
            Moved::Coin => (None, self.network.native.decimals),
            Moved::Token { mint, decimals, .. } => (Some(*mint), *decimals),
            Moved::Core { .. } | Moved::Compressed(_) => (None, 0),
            Moved::Programmable { mint, .. } => (Some(*mint), 0),
        };
        if !transfer.max && transfer.amount.decimals() != decimals {
            return Err(ChainError::Invalid(
                "the amount uses the wrong number of decimals",
            ));
        }
        Ok((mint, decimals))
    }

    fn sendable(&self, transfer: &Transfer) -> Result<(), ChainError> {
        if transfer.asset.is_nft() {
            transfer.check_nft(&self.network.id)?;
        } else {
            transfer.check(&self.network.id)?;
        }
        if transfer.from == transfer.to {
            return Err(ChainError::Invalid("send to another address"));
        }
        Ok(())
    }

    async fn rent(&self, size: u64) -> Result<Rent, ChainError> {
        let cached = *self.rent.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((at, rent)) = cached
            && at.elapsed() < RENT_FOR
            && rent.size == size
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
        let (account, token, record) =
            try_join3(minimum(0), minimum(size), minimum(TOKEN_RECORD)).await?;
        let rent = Rent {
            account,
            token,
            record,
            size,
        };
        *self.rent.lock().unwrap_or_else(PoisonError::into_inner) = Some((Instant::now(), rent));
        Ok(rent)
    }

    async fn epoch(&self) -> Result<u64, ChainError> {
        let reply = self
            .rpc
            .call("getEpochInfo", json!([{"commitment": "confirmed"}]))
            .await?;
        count(&reply["epoch"])
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
                None,
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
                    "maxSupportedTransactionVersion": 1
                }]),
            )
            .await?;
        if found.is_null() {
            return Ok(TxStatus::Pending);
        }
        tx::fetched(&rpc::encoded(&found["transaction"])?, id)?;
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
        enough(
            &self.symbol,
            U256::from(needed),
            U256::from(self.held),
            self.decimals,
        )
    }

    fn short(&self, needed: u64) -> ChainError {
        ChainError::insufficient(
            self.symbol.clone(),
            U256::from(needed),
            U256::from(self.held),
            self.decimals,
        )
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

    fn spend(&self, costs: &[u64], refund: u64) -> Result<(), ChainError> {
        let spent = costs
            .iter()
            .try_fold(0u64, |total, cost| total.checked_add(*cost))
            .ok_or_else(ChainError::overflow)?;
        self.sol.cover(spent)?;
        let left = (self.sol.held - spent).saturating_add(refund);
        if left > 0 && left < self.rent.account {
            return Err(ChainError::Invalid(
                "this would leave less SOL than an account must keep, send everything with Max or leave more",
            ));
        }
        Ok(())
    }
}

fn program_fee(moved: &Moved) -> u64 {
    match moved {
        Moved::Compressed(moving) if moving.leaf.v2.is_some() => COMPRESSED_FEE,
        _ => 0,
    }
}

fn locked(moved: &Moved) -> Vec<Key> {
    match moved {
        Moved::Compressed(moving) => vec![moving.leaf.tree],
        _ => Vec::new(),
    }
}

fn sendable_token(mint: &Mint, tokens: &[Option<Account>], memo: bool) -> Result<(), ChainError> {
    match mint.blocked {
        Some(Blocked::Bound) => {
            return Err(ChainError::Invalid(
                "this token is bound to its holder and can't be sent",
            ));
        }
        Some(Blocked::Paused) => {
            return Err(ChainError::Invalid(
                "the token's issuer has paused its transfers",
            ));
        }
        Some(Blocked::Hooked) => {
            return Err(ChainError::Unsupported(
                "tokens that run their own program on every transfer",
            ));
        }
        None => {}
    }
    let [source, destination] = [&tokens[0], &tokens[1]].map(|account| {
        account
            .as_ref()
            .filter(|account| account.owner == mint.program)
    });
    if source.is_some_and(frozen) {
        return Err(ChainError::Invalid(
            "the token's issuer froze this wallet's token account",
        ));
    }
    match destination {
        Some(account) if frozen(account) => Err(ChainError::Invalid(
            "the token's issuer froze the recipient's token account",
        )),
        Some(account) if !memo && needs_memo(account)? => Err(ChainError::Invalid(
            "the recipient's token account takes only transfers with a memo",
        )),
        None if mint.starts_frozen => Err(ChainError::Invalid(
            "new accounts of this token start frozen, the recipient must open one through its issuer",
        )),
        _ => Ok(()),
    }
}

fn touched(
    moved: &Moved,
    mint: Option<&Key>,
    from: &Key,
    to: &Key,
) -> Result<Vec<Key>, ChainError> {
    let Some(mint) = mint else {
        return Ok(Vec::new());
    };
    let program = match moved {
        Moved::Token { program, .. } => *program,
        _ => TOKEN,
    };
    let mut keys = vec![
        token_account(from, &program, mint)?,
        token_account(to, &program, mint)?,
    ];
    if matches!(moved, Moved::Programmable { .. }) {
        keys.extend([token_record(mint, &keys[0])?, token_record(mint, &keys[1])?]);
    }
    Ok(keys)
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

fn failure(err: &Value) -> String {
    to_plain_text(&format!("the transfer would fail: {err}"), MAX_REASON)
}

#[cfg(fuzzing)]
pub(super) fn fuzz(value: &Value) {
    let _ = price(value);
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

    #[tokio::test]
    #[ignore = "talks to the real Solana mainnet"]
    async fn live_nodes_accept_our_signature_and_refuse_only_the_empty_account() {
        let registry = Registry::builtin().unwrap();
        let network = registry.get(&"solana".parse().unwrap()).unwrap().clone();
        let chain = SolanaChain::new(network).unwrap();
        let seed = Mnemonic::from_entropy(&[0x5c; 16]).unwrap().to_seed("");
        let key = super::super::key(&seed, 0, SolanaPath::default()).unwrap();
        let from = key.verifying_key().to_bytes();
        let blockhash = chain.blockhash().await.unwrap();
        let shape = Shape {
            from,
            to: [0x27; 32],
            moved: Moved::Coin,
            units: 1_000_000,
            memo: Some("deposit 4417".to_owned()),
            limit: 1_000,
            price: 0,
        };
        let message = tx::compile(from, blockhash, &shape.instructions().unwrap()).unwrap();
        let (bytes, signature) = tx::sign(&message, &key);
        let signed = Signed {
            id: tx::id(&signature).unwrap(),
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
