use bahaa_chain::{ChainError, Failure, FeeLine, FeeQuote, Signed, Transfer, TxId, TxStatus};
use ruint::aliases::U256;
use serde_json::{Value, json};

use super::{BALANCE_OF, TronChain, amount, bytes, count, malformed, now_ms, tron_hex, word};
use crate::fees::{self, Prices, Quote, Resources};
use crate::http::{hex, node_text, unhex};
use crate::proto::Raw;
use crate::tx::{self, Anchor};

const STALE_AFTER_MS: i64 = 60_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TronPlan {
    pub(super) transfer: Transfer,
    pub(super) raw: Raw,
}

impl TronChain {
    pub(super) async fn plan(
        &self,
        transfer: &Transfer,
    ) -> Result<(TronPlan, FeeQuote), ChainError> {
        self.sendable(transfer)?;
        let (from, to) = (bytes(&transfer.from)?, bytes(&transfer.to)?);
        let token = transfer.asset.contract().map(bytes).transpose()?;
        let decimals = match &token {
            None => self.network.native.decimals,
            Some(contract) => self.token_decimals(&from, contract).await?,
        };
        if !transfer.max && transfer.amount.decimals() != decimals {
            return Err(ChainError::Invalid(
                "the amount uses the wrong number of decimals",
            ));
        }
        let balance = self
            .account(&from)
            .await?
            .map_or(U256::ZERO, |(free, _)| free);
        let held = match &token {
            None => None,
            Some(contract) => Some(self.call(&from, contract, BALANCE_OF, &word(&from)).await?),
        };
        let mut transfer = transfer.clone();
        if transfer.max {
            transfer.take_all(amount(held.unwrap_or(balance), decimals)?)?;
        }
        let native = (
            self.network.native.symbol.clone(),
            self.network.native.decimals,
        );
        let funds = |units: U256, max: U256| -> Result<(), ChainError> {
            if let (Some(contract), Some(held)) = (token, held) {
                if held < units {
                    return Err(insufficient(self.symbol(&contract), units, held, decimals)?);
                }
                if balance < max {
                    return Err(insufficient(native.0.clone(), max, balance, native.1)?);
                }
                return Ok(());
            }
            let needed = units.checked_add(max).ok_or_else(overflow)?;
            if balance < needed {
                return Err(insufficient(native.0.clone(), needed, balance, native.1)?);
            }
            Ok(())
        };
        funds(transfer.amount.units(), U256::ZERO)?;
        let prices = self.prices().await?;
        let resources = self.resources(&from).await?;
        let memo = transfer.memo.as_deref();
        let (raw, quote) = match token {
            None => {
                let new_account = self.account(&to).await?.is_none();
                let anchor = self.anchor().await?;
                let coins = |sun: u64| -> Result<(Raw, Quote), ChainError> {
                    let sun = i64::try_from(sun)
                        .map_err(|_| ChainError::Invalid("the amount is too large"))?;
                    let raw = tx::raw(tx::coins(&from, &to, sun), &anchor, 0, memo)?;
                    let quote = fees::quote(
                        tx::bandwidth(&raw),
                        None,
                        (new_account, memo.is_some()),
                        &resources,
                        &prices,
                    )?;
                    Ok((raw, quote))
                };
                let mut sun = u64::try_from(transfer.amount.units())
                    .map_err(|_| ChainError::Invalid("the amount is too large"))?;
                if transfer.max {
                    let Some(most) = fees::most(sun, |sun| Ok(coins(sun)?.1.max))? else {
                        return Err(short_of_fee(&native, coins(sun)?.1.max, balance)?);
                    };
                    sun = most;
                    transfer.amount = amount(U256::from(sun), decimals)?;
                }
                coins(sun)?
            }
            Some(contract) => {
                let data = tx::transfer_data(&to, transfer.amount.units());
                let energy = self.simulate(&from, &contract, &data).await?;
                let limit = fees::fee_limit(energy, &prices)?;
                let anchor = self.anchor().await?;
                let raw = tx::raw(tx::token(&from, &contract, data), &anchor, limit, memo)?;
                let quote = fees::quote(
                    tx::bandwidth(&raw),
                    Some((energy, limit)),
                    (false, memo.is_some()),
                    &resources,
                    &prices,
                )?;
                (raw, quote)
            }
        };
        funds(transfer.amount.units(), U256::from(quote.max))?;
        Ok((TronPlan { transfer, raw }, self.fee_quote(quote)?))
    }

    fn sendable(&self, transfer: &Transfer) -> Result<(), ChainError> {
        if transfer.asset.network() != &self.network.id {
            return Err(ChainError::Invalid("the asset belongs to another network"));
        }
        if !transfer.max && transfer.amount.is_zero() {
            return Err(ChainError::Invalid("send more than zero"));
        }
        if transfer.asset.contract().is_none() && transfer.from == transfer.to {
            return Err(ChainError::Invalid(
                "Tron does not allow sending TRX to the same account",
            ));
        }
        Ok(())
    }

    async fn prices(&self) -> Result<Prices, ChainError> {
        let reply = self
            .http
            .post("wallet/getchainparameters", json!({}))
            .await?;
        let list = reply["chainParameter"].as_array().ok_or_else(malformed)?;
        let find = |key: &str| -> Result<u64, ChainError> {
            let entry = list
                .iter()
                .find(|entry| entry["key"] == key)
                .ok_or_else(malformed)?;
            count(entry, "value")
        };
        Ok(Prices {
            per_byte: find("getTransactionFee")?,
            per_energy: find("getEnergyFee")?,
            create_account: find("getCreateAccountFee")?,
            activation: find("getCreateNewAccountFeeInSystemContract")?,
            max_fee_limit: find("getMaxFeeLimit")?,
            memo: find("getMemoFee").unwrap_or(0),
        })
    }

    async fn resources(&self, owner: &[u8; 20]) -> Result<Resources, ChainError> {
        let reply = self
            .http
            .post(
                "wallet/getaccountresource",
                json!({"address": tron_hex(owner)}),
            )
            .await?;
        let left = |limit: &str, used: &str| -> Result<u64, ChainError> {
            Ok(count(&reply, limit)?.saturating_sub(count(&reply, used)?))
        };
        Ok(Resources {
            free_bandwidth: left("freeNetLimit", "freeNetUsed")?,
            staked_bandwidth: left("NetLimit", "NetUsed")?,
            energy: left("EnergyLimit", "EnergyUsed")?,
        })
    }

    async fn simulate(
        &self,
        owner: &[u8; 20],
        contract: &[u8; 20],
        data: &[u8],
    ) -> Result<u64, ChainError> {
        let reply = self
            .trigger(json!({
                "owner_address": tron_hex(owner),
                "contract_address": tron_hex(contract),
                "data": hex(data),
            }))
            .await?;
        reply["energy_used"]
            .as_u64()
            .filter(|energy| *energy > 0)
            .ok_or_else(|| ChainError::Network("an endpoint sent no energy estimate".to_owned()))
    }

    async fn anchor(&self) -> Result<Anchor, ChainError> {
        let reply = self
            .http
            .post("wallet/getblock", json!({"detail": false}))
            .await?;
        let header = &reply["block_header"]["raw_data"];
        let number = header["number"].as_u64().ok_or_else(malformed)?;
        let time_ms = header["timestamp"]
            .as_i64()
            .filter(|time| *time > 0)
            .ok_or_else(malformed)?;
        let id: [u8; 32] = reply["blockID"]
            .as_str()
            .and_then(unhex)
            .and_then(|id| id.try_into().ok())
            .ok_or_else(malformed)?;
        if id[..8] != number.to_be_bytes() {
            return Err(malformed());
        }
        if time_ms < now_ms().saturating_sub(STALE_AFTER_MS) {
            return Err(ChainError::Network(
                "an endpoint is behind the chain, try again".to_owned(),
            ));
        }
        Ok(Anchor {
            number,
            id,
            time_ms,
        })
    }

    fn fee_quote(&self, quote: Quote) -> Result<FeeQuote, ChainError> {
        let trx = |sun: u64| amount(U256::from(sun), self.network.native.decimals);
        let lines = quote
            .lines
            .into_iter()
            .map(|(item, sun)| {
                Ok(FeeLine {
                    item,
                    amount: trx(sun)?,
                })
            })
            .collect::<Result<Vec<_>, ChainError>>()?;
        FeeQuote::new(trx(quote.max)?, trx(quote.expected)?, lines)
    }

    pub(super) async fn push(&self, signed: &Signed) -> Result<TxId, ChainError> {
        self.http
            .post_all(
                "wallet/broadcasthex",
                json!({"transaction": hex(&signed.bytes)}),
                |reply| accepted(reply, &signed.id),
            )
            .await?;
        Ok(signed.id.clone())
    }

    pub(super) async fn lookup(&self, id: &TxId) -> Result<TxStatus, ChainError> {
        let asked = json!({"value": id.as_str()});
        let info = self
            .http
            .post("wallet/gettransactioninfobyid", asked.clone())
            .await?;
        let found = info.as_object().ok_or_else(malformed)?;
        if found.is_empty() {
            let pending = self
                .http
                .post("wallet/gettransactionfrompending", asked)
                .await?;
            return Ok(if pending.as_object().is_some_and(|tx| !tx.is_empty()) {
                TxStatus::Pending
            } else {
                TxStatus::NotFound
            });
        }
        let ours = info["id"]
            .as_str()
            .is_some_and(|text| text.eq_ignore_ascii_case(id.as_str()));
        if !ours {
            return Err(ChainError::Network(
                "an endpoint answered for another transaction".to_owned(),
            ));
        }
        let block = info["blockNumber"].as_u64().ok_or_else(malformed)?;
        let fee = count(&info, "fee")?;
        let outcome = info["receipt"]["result"].as_str();
        if info["result"] == "FAILED" || outcome.is_some_and(|result| result != "SUCCESS") {
            return Ok(TxStatus::Failed(failure(outcome, &info["resMessage"])));
        }
        Ok(TxStatus::Confirmed {
            block: Some(block),
            fee: Some(amount(U256::from(fee), self.network.native.decimals)?),
            transaction: Some(id.clone()),
        })
    }
}

fn accepted(reply: &Value, id: &TxId) -> Result<(), Failure<ChainError>> {
    if reply["result"] == true {
        let same = reply["txid"]
            .as_str()
            .is_some_and(|txid| txid.eq_ignore_ascii_case(id.as_str()));
        if !same {
            return Err(Failure::Retry(ChainError::Network(
                "the node answered with another transaction id".to_owned(),
            )));
        }
        return Ok(());
    }
    if reply["code"] == "DUP_TRANSACTION_ERROR" {
        return Err(Failure::Unsupported(ChainError::Network(
            "the node has already seen this transaction".to_owned(),
        )));
    }
    let reason = match reply.get("message") {
        Some(message) if message.is_string() => node_text(message),
        _ => node_text(&reply["code"]),
    };
    Err(Failure::Fatal(ChainError::Rejected(reason)))
}

fn failure(outcome: Option<&str>, message: &Value) -> String {
    match outcome {
        Some("REVERT") => "the transaction reverted".to_owned(),
        Some("OUT_OF_ENERGY") => "the transaction ran out of energy".to_owned(),
        Some(other) if other != "SUCCESS" => format!("the transaction failed: {other}"),
        _ => node_text(message),
    }
}

fn short_of_fee(
    (symbol, decimals): &(String, u8),
    fee: u64,
    balance: U256,
) -> Result<ChainError, ChainError> {
    let needed = fee.checked_add(1).ok_or_else(overflow)?;
    insufficient(symbol.clone(), U256::from(needed), balance, *decimals)
}

fn insufficient(
    symbol: String,
    needed: U256,
    available: U256,
    decimals: u8,
) -> Result<ChainError, ChainError> {
    Ok(ChainError::Insufficient {
        symbol,
        needed: amount(needed, decimals)?,
        available: amount(available, decimals)?,
    })
}

fn overflow() -> ChainError {
    ChainError::Invalid("fee overflow")
}

#[cfg(test)]
mod tests {
    use bahaa_chain::Chain;
    use bahaa_config::Registry;
    use bahaa_crypto::Mnemonic;
    use bahaa_types::Address;

    use super::*;

    #[tokio::test]
    #[ignore = "talks to the real Tron Nile testnet"]
    async fn live_nile_accepts_our_bytes_and_refuses_only_the_empty_account() {
        let registry = Registry::builtin().unwrap();
        let nile = registry.get(&"tron-nile".parse().unwrap()).unwrap().clone();
        let chain = TronChain::new(nile).unwrap();
        let seed = Mnemonic::from_entropy(&[0x5c; 16]).unwrap().to_seed("");
        let from = bytes(&chain.derive(&seed, 0).unwrap()).unwrap();
        let anchor = chain.anchor().await.unwrap();
        let raw = tx::raw(tx::coins(&from, &[0x27; 20], 1_000), &anchor, 0, None).unwrap();
        let plan = TronPlan {
            transfer: Transfer {
                account: 0,
                from: Address::Tron(from),
                to: Address::Tron([0x27; 20]),
                asset: "tron-nile".parse().unwrap(),
                amount: amount(U256::from(1_000u16), 6).unwrap(),
                max: false,
                memo: None,
            },
            raw,
        };
        let signed = chain.sign(&plan, &seed).unwrap();
        chain.decode(&signed).unwrap();
        let refused = chain.push(&signed).await.unwrap_err();
        let ChainError::Rejected(reason) = &refused else {
            panic!("{refused:?}");
        };
        let named = format!("account [{}] does not exist", Address::Tron(from));
        assert!(reason.contains(&named), "{reason}");
        let stranger = k256::SecretKey::from_slice(&[0x42; 32]).unwrap();
        let other = tx::raw(tx::coins(&from, &[0x27; 20], 1_001), &anchor, 0, None).unwrap();
        let (forged, id) = tx::sign(other, &stranger).unwrap();
        let forged = Signed {
            id: TxId::new(hex(&id)).unwrap(),
            bytes: forged,
        };
        let refused = chain.push(&forged).await.unwrap_err();
        let ChainError::Rejected(reason) = &refused else {
            panic!("{refused:?}");
        };
        assert!(reason.to_ascii_lowercase().contains("sign"), "{reason}");
    }
}
