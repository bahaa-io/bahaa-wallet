use std::cmp::Reverse;
use std::collections::HashMap;

use bahaa_chain::{
    ChainError, Decoded, FeeItem, FeeLine, FeeQuote, FeeRate, Pace, RateUnit, Signed, Transfer,
    TxId, TxStatus,
};
use bahaa_crypto::{
    Seed, compressed, secp256k1_sign, secp256k1_verify, taproot_output, taproot_sign,
    taproot_verify,
};
use bahaa_types::{Address, Amount, AssetId, BitcoinScript, BitcoinType};
use futures_util::future::try_join;
use ruint::aliases::U256;

use crate::script::{dust, hash160, key_address, locking};
use crate::source::Estimates;
use crate::tx::{
    Input, MAX_SIGNATURE, Outpoint, Output, SEQUENCE, Tx, VERSION, id_of, id_text, parse,
    read_signature, signature,
};

use super::BitcoinChain;
use super::account::{CHANGE, Coin, Funds, script};

const KEY_BYTES: usize = 33;
const SCHNORR_BYTES: usize = 64;

#[derive(Clone, Debug)]
pub struct BitcoinPlan {
    pub(super) transfer: Transfer,
    pub(super) tx: Tx,
    pub(super) spent: Vec<Coin>,
}

#[derive(Debug, PartialEq, Eq)]
struct Picked {
    coins: Vec<Coin>,
    outputs: Vec<Output>,
    sent: u64,
    size: u64,
    paid: u64,
    leftover: u64,
}

impl BitcoinChain {
    pub(super) async fn plan(
        &self,
        transfer: &Transfer,
    ) -> Result<(BitcoinPlan, FeeQuote), ChainError> {
        transfer.check(&self.network.id)?;
        self.native(std::slice::from_ref(&transfer.asset))?;
        if transfer.memo.is_some() {
            return Err(ChainError::Invalid("Bitcoin transfers carry no memo"));
        }
        let to = self.recipient(&transfer.to)?;
        let units = u64::try_from(transfer.amount.units()).map_err(|_| ChainError::overflow())?;
        let (funds, estimates) =
            try_join(self.funds(&transfer.from), self.sources.estimates()).await?;
        let rate = estimates.rate();
        if self.takes_change(&transfer.from, &transfer.to) {
            return Err(ChainError::Invalid(
                "send to another address, this one takes this wallet's change",
            ));
        }
        let change = Output {
            value: dust(script(&funds.change)?),
            script: locking(script(&funds.change)?),
        };
        let picked = pick(&funds, &to, &change, units, transfer.max, rate).map_err(|needed| {
            ChainError::insufficient(
                self.network.native.symbol.clone(),
                U256::from(needed),
                U256::from(funds.total().unwrap_or(u64::MAX)),
                self.network.native.decimals,
            )
        })?;
        if picked.sent < dust(script(&transfer.to)?) {
            return Err(ChainError::Invalid(
                "this amount is below the smallest output Bitcoin relays",
            ));
        }
        let mut lines = vec![FeeLine {
            item: FeeItem::Size {
                vbytes: picked.size,
            },
            amount: self.sats(picked.paid)?,
        }];
        if picked.leftover > 0 {
            lines.push(FeeLine {
                item: FeeItem::Leftover,
                amount: self.sats(picked.leftover)?,
            });
        }
        let fee = self.sats(
            picked
                .paid
                .checked_add(picked.leftover)
                .ok_or_else(ChainError::overflow)?,
        )?;
        let mut planned = transfer.clone();
        planned.amount = self.sats(picked.sent)?;
        let (spent, tx) = unsigned(&picked.coins, picked.outputs);
        let plan = BitcoinPlan {
            transfer: planned,
            tx,
            spent,
        };
        Ok((
            plan,
            FeeQuote::new(fee, fee, lines)?.paced(self.pace(&estimates, rate)),
        ))
    }

    pub(super) fn signed(&self, plan: &BitcoinPlan, seed: &Seed) -> Result<Signed, ChainError> {
        let mut tx = plan.tx.clone();
        let spent = plan
            .spent
            .iter()
            .map(|coin| {
                Ok(Output {
                    value: coin.value,
                    script: locking(script(&coin.address)?),
                })
            })
            .collect::<Result<Vec<_>, ChainError>>()?;
        let stranger = || ChainError::Invalid("this seed does not own the sending account");
        for (index, coin) in plan.spent.iter().enumerate() {
            let owner = self
                .book()
                .place(&coin.address)
                .map_or(coin.address, |place| place.owner);
            if owner != plan.transfer.from {
                return Err(stranger());
            }
            let key = self.key(seed, plan.transfer.account, &coin.address)?;
            let public = compressed(&key.public_key())?;
            let failed = |_| ChainError::Invalid("signing failed");
            tx.inputs[index].witness = match script(&coin.address)? {
                BitcoinScript::WitnessKeyHash(hash) if hash160(&public) == hash => {
                    let (compact, _) = secp256k1_sign(&key, &tx.sighash(index, &hash, coin.value))
                        .map_err(failed)?;
                    vec![signature(&compact), public.to_vec()]
                }
                BitcoinScript::Taproot(output) if taproot_output(&public).ok() == Some(output) => {
                    let sighash = tx.taproot_sighash(index, &spent);
                    vec![taproot_sign(&key, &sighash).map_err(failed)?.to_vec()]
                }
                _ => return Err(stranger()),
            };
        }
        Ok(Signed {
            id: TxId::new(id_text(&tx.id()))?,
            bytes: tx.bytes(),
        })
    }

    pub(super) fn read(&self, signed: &Signed) -> Result<Decoded, ChainError> {
        let unusual = ChainError::Invalid("this is not a transfer this wallet makes");
        let (tx, from) = self.verified(signed)?;
        let mut paid = Vec::new();
        let mut changes = Vec::new();
        for output in &tx.outputs {
            let address = self
                .address(&output.script)
                .ok_or_else(|| unusual.clone())?;
            if self.takes_change(&from, &address) {
                changes.push((address, output.value));
            } else {
                paid.push((address, output.value));
            }
        }
        let (([(to, value)], [] | [_]) | ([], [(to, value)])) =
            (paid.as_slice(), changes.as_slice())
        else {
            return Err(unusual);
        };
        Ok(Decoded {
            from,
            to: *to,
            asset: AssetId::Native(self.network.id.clone()),
            amount: self.sats(*value)?,
            everything: false,
            memo: None,
            sequence: tx.inputs.first().map(|input| conflict(&input.previous)),
            expires_at: None,
        })
    }

    pub(super) fn verified(&self, signed: &Signed) -> Result<(Tx, Address), ChainError> {
        let unusual = ChainError::Invalid("this is not a transfer this wallet makes");
        let tx = parse(&signed.bytes)?;
        if TxId::new(id_text(&tx.id()))? != signed.id {
            return Err(ChainError::Mismatch("the transaction id"));
        }
        if tx.version != VERSION || tx.lock_time != 0 {
            return Err(unusual);
        }
        let spent = {
            let book = self.book();
            tx.inputs
                .iter()
                .map(|input| book.coin(&input.previous))
                .collect::<Option<Vec<(Address, u64)>>>()
                .ok_or_else(|| unusual.clone())?
        };
        let outputs = spent
            .iter()
            .map(|(address, value)| {
                Ok(Output {
                    value: *value,
                    script: locking(script(address)?),
                })
            })
            .collect::<Result<Vec<_>, ChainError>>()?;
        let mut from = None;
        for (index, (input, (address, value))) in tx.inputs.iter().zip(&spent).enumerate() {
            let valid = match (script(address)?, input.witness.as_slice()) {
                (BitcoinScript::WitnessKeyHash(hash), [signature, key]) => {
                    let sighash = tx.sighash(index, &hash, *value);
                    <[u8; KEY_BYTES]>::try_from(key.as_slice()).is_ok_and(|key| {
                        hash160(&key) == hash
                            && read_signature(signature)
                                .is_some_and(|signed| secp256k1_verify(&key, &sighash, &signed))
                    })
                }
                (BitcoinScript::Taproot(output), [signature]) => {
                    let sighash = tx.taproot_sighash(index, &outputs);
                    <[u8; SCHNORR_BYTES]>::try_from(signature.as_slice())
                        .is_ok_and(|signed| taproot_verify(&output, &sighash, &signed))
                }
                _ => false,
            };
            if !valid || !input.script.is_empty() || input.sequence != SEQUENCE {
                return Err(ChainError::Mismatch("the signatures"));
            }
            let address = *address;
            let owner = self
                .book()
                .place(&address)
                .map_or(address, |place| place.owner);
            if from.replace(owner).is_some_and(|before| before != owner) {
                return Err(ChainError::Mismatch("the sender"));
            }
        }
        let from = from.ok_or(unusual)?;
        Ok((tx, from))
    }

    pub(super) async fn push(&self, signed: &Signed) -> Result<TxId, ChainError> {
        self.sources.broadcast(&signed.bytes, &signed.id).await
    }

    pub(super) async fn lookup(&self, id: &TxId, from: &Address) -> Result<TxStatus, ChainError> {
        let Some(Some(tx)) = self.transactions(&[id_of(id.as_str())?]).await?.pop() else {
            return Ok(TxStatus::NotFound);
        };
        let ours = self.watched(&tx, from).ok_or(ChainError::Invalid(
            "this is not a transfer this wallet makes",
        ))?;
        let wanted = id_of(id.as_str())?;
        let height = self
            .histories(&[ours])
            .await?
            .into_iter()
            .flatten()
            .find(|seen| seen.txid == wanted)
            .map_or(0, |seen| seen.height);
        let Ok(height) = u64::try_from(height) else {
            return Ok(TxStatus::Pending);
        };
        if height == 0 {
            return Ok(TxStatus::Pending);
        }
        self.prove(&[(wanted, height)]).await?;
        Ok(TxStatus::Confirmed {
            block: Some(height),
            fee: self.fee_of(&tx).await.ok(),
            transaction: None,
        })
    }

    async fn fee_of(&self, tx: &Tx) -> Result<bahaa_types::Amount, ChainError> {
        let mut known = HashMap::new();
        self.load(
            &mut known,
            tx.inputs.iter().map(|input| input.previous.txid),
        )
        .await?;
        let mut spent = 0u64;
        for input in &tx.inputs {
            let output = super::spent(&known, &input.previous).ok_or_else(ChainError::malformed)?;
            spent = spent
                .checked_add(output.value)
                .ok_or_else(ChainError::overflow)?;
        }
        let paid = tx
            .outputs
            .iter()
            .try_fold(0u64, |sum, output| sum.checked_add(output.value))
            .ok_or_else(ChainError::overflow)?;
        self.sats(spent.checked_sub(paid).ok_or_else(ChainError::malformed)?)
    }

    pub(super) fn pace(&self, estimates: &Estimates, rate: u64) -> Pace {
        Pace {
            rate: Amount::new(U256::from(rate), 3).ok().map(|amount| FeeRate {
                amount,
                unit: RateUnit::SatPerVbyte,
            }),
            wait: estimates.wait(rate, self.network.block_ms),
        }
    }

    pub(super) fn recipient(&self, to: &Address) -> Result<Vec<u8>, ChainError> {
        match to {
            Address::Bitcoin { script, testnet } if *testnet == self.network.testnet => {
                Ok(locking(*script))
            }
            Address::Bitcoin { .. } => Err(ChainError::Invalid(
                "this address belongs to another Bitcoin network",
            )),
            _ => Err(ChainError::Invalid("not a Bitcoin address")),
        }
    }

    fn watched(&self, tx: &Tx, from: &Address) -> Option<Address> {
        let book = self.book();
        let spent = tx
            .inputs
            .first()
            .and_then(|input| book.coin(&input.previous))
            .map(|(address, _)| address);
        let signer = tx
            .inputs
            .first()
            .and_then(|input| input.witness.get(1))
            .and_then(|key| <[u8; KEY_BYTES]>::try_from(key.as_slice()).ok())
            .and_then(|key| {
                key_address(&key, BitcoinType::NativeSegwit, self.network.testnet).ok()
            });
        let outputs: Vec<Address> = tx
            .outputs
            .iter()
            .filter_map(|output| self.address(&output.script))
            .collect();
        let change = outputs
            .iter()
            .find(|address| book.holds(from, address))
            .copied();
        spent
            .or(signer)
            .or(change)
            .or_else(|| outputs.first().copied())
    }

    pub(super) fn takes_change(&self, from: &Address, address: &Address) -> bool {
        let book = self.book();
        match book.place(address) {
            Some(place) => place.owner == *from && place.branch == CHANGE,
            None => address == from && !book.owns(from),
        }
    }
}

pub(super) fn conflict(outpoint: &Outpoint) -> u64 {
    let mut head = [0u8; 8];
    head.copy_from_slice(&outpoint.txid[..8]);
    u64::from_le_bytes(head) ^ u64::from(outpoint.vout)
}

fn pick(
    funds: &Funds,
    to: &[u8],
    change: &Output,
    amount: u64,
    max: bool,
    rate: u64,
) -> Result<Picked, u64> {
    let output = |value: u64, script: &[u8]| Output {
        value,
        script: script.to_vec(),
    };
    let mut ordered = funds.coins.clone();
    ordered.sort_by_key(|coin| (coin.height <= 0, Reverse(coin.value)));
    let total = |coins: &[Coin]| coins.iter().map(|coin| coin.value).sum::<u64>();
    if max {
        let held = total(&ordered);
        let (size, fee) = cost(&ordered, &[output(held, to)], rate);
        let sent = held.checked_sub(fee).filter(|sent| *sent > 0).ok_or(fee)?;
        return Ok(Picked {
            coins: ordered,
            outputs: vec![output(sent, to)],
            sent,
            size,
            paid: fee,
            leftover: 0,
        });
    }
    let mut chosen = Vec::new();
    for coin in &ordered {
        chosen.push(coin.clone());
        let held = total(&chosen);
        let (size, fee) = cost(
            &chosen,
            &[output(amount, to), output(0, &change.script)],
            rate,
        );
        if let Some(rest) = held
            .checked_sub(amount)
            .and_then(|rest| rest.checked_sub(fee))
            .filter(|rest| *rest >= change.value)
        {
            return Ok(Picked {
                coins: chosen,
                outputs: vec![output(amount, to), output(rest, &change.script)],
                sent: amount,
                size,
                paid: fee,
                leftover: 0,
            });
        }
        let (size, fee) = cost(&chosen, &[output(amount, to)], rate);
        if let Some(leftover) = held
            .checked_sub(amount)
            .and_then(|rest| rest.checked_sub(fee))
        {
            return Ok(Picked {
                coins: chosen,
                outputs: vec![output(amount, to)],
                sent: amount,
                size,
                paid: fee,
                leftover,
            });
        }
    }
    let (_, fee) = cost(&ordered, &[output(amount, to)], rate);
    Err(amount.saturating_add(fee))
}

pub(super) fn cost(coins: &[Coin], outputs: &[Output], rate: u64) -> (u64, u64) {
    let (sorted, mut draft) = unsigned(coins, outputs.to_vec());
    for (input, coin) in draft.inputs.iter_mut().zip(&sorted) {
        input.witness = match script(&coin.address) {
            Ok(BitcoinScript::Taproot(_)) => vec![vec![0; SCHNORR_BYTES]],
            _ => vec![vec![0; MAX_SIGNATURE], vec![0; KEY_BYTES]],
        };
    }
    let size = draft.vsize();
    (size, size.saturating_mul(rate).div_ceil(1_000))
}

pub(super) fn unsigned(coins: &[Coin], mut outputs: Vec<Output>) -> (Vec<Coin>, Tx) {
    outputs.sort();
    ordered(coins, outputs)
}

pub(super) fn ordered(coins: &[Coin], outputs: Vec<Output>) -> (Vec<Coin>, Tx) {
    let mut coins = coins.to_vec();
    coins.sort_by_key(|coin| coin.outpoint);
    let inputs = coins
        .iter()
        .map(|coin| Input {
            previous: coin.outpoint,
            script: Vec::new(),
            sequence: SEQUENCE,
            witness: Vec::new(),
        })
        .collect();
    let tx = Tx {
        version: VERSION,
        inputs,
        outputs,
        lock_time: 0,
    };
    (coins, tx)
}

#[cfg(test)]
mod tests {
    use bahaa_types::BitcoinScript;

    use super::*;

    fn coin(value: u64, confirmed: bool, n: u8) -> Coin {
        Coin {
            outpoint: Outpoint {
                txid: [n; 32],
                vout: 0,
            },
            value,
            height: if confirmed { 900_000 } else { 0 },
            address: Address::Bitcoin {
                script: BitcoinScript::WitnessKeyHash([n; 20]),
                testnet: false,
            },
        }
    }

    fn funds(coins: Vec<Coin>) -> Funds {
        Funds {
            change: coins[0].address,
            coins,
        }
    }

    const TO: [u8; 22] = [0; 22];
    const CHANGE_SCRIPT: [u8; 22] = [1; 22];

    fn change() -> Output {
        Output {
            value: 294,
            script: CHANGE_SCRIPT.to_vec(),
        }
    }

    #[test]
    fn exact_sends_take_confirmed_coins_first_and_keep_real_change() {
        let wallet = funds(vec![
            coin(50_000, false, 1),
            coin(20_000, true, 2),
            coin(30_000, true, 3),
        ]);
        let picked = pick(&wallet, &TO, &change(), 25_000, false, 1_000).unwrap();
        assert_eq!(picked.coins, [coin(30_000, true, 3)]);
        assert_eq!(picked.size, 141);
        assert_eq!(picked.paid, 141);
        assert_eq!(
            picked.outputs,
            [
                Output {
                    value: 25_000,
                    script: TO.to_vec()
                },
                Output {
                    value: 4_859,
                    script: CHANGE_SCRIPT.to_vec()
                },
            ]
        );
        let both = pick(&wallet, &TO, &change(), 45_000, false, 1_000).unwrap();
        assert_eq!(both.coins.len(), 2);
        assert_eq!(both.size, 209);
    }

    #[test]
    fn change_below_the_dust_limit_goes_to_the_fee_as_leftover() {
        let wallet = funds(vec![coin(25_300, true, 1)]);
        let picked = pick(&wallet, &TO, &change(), 25_000, false, 1_000).unwrap();
        assert_eq!(picked.outputs.len(), 1);
        assert_eq!(picked.paid, 110);
        assert_eq!(picked.leftover, 190);
        assert_eq!(picked.sent, 25_000);
    }

    #[test]
    fn max_spends_every_coin_less_the_fee_and_short_funds_say_what_they_need() {
        let wallet = funds(vec![coin(10_000, true, 1), coin(5_000, false, 2)]);
        let max = pick(&wallet, &TO, &change(), 0, true, 2_000).unwrap();
        assert_eq!(max.coins.len(), 2);
        assert_eq!(max.size, 178);
        assert_eq!(max.sent, 15_000 - 356);
        assert_eq!(
            pick(&wallet, &TO, &change(), 15_000, false, 2_000),
            Err(15_356)
        );
    }

    #[test]
    fn bip69_orders_inputs_by_id_and_outputs_by_value() {
        let (spent, tx) = unsigned(
            &[coin(1, true, 9), coin(1, true, 2)],
            vec![
                Output {
                    value: 7,
                    script: vec![2],
                },
                Output {
                    value: 7,
                    script: vec![1],
                },
                Output {
                    value: 3,
                    script: vec![9],
                },
            ],
        );
        assert_eq!(tx.inputs[0].previous.txid, [2; 32]);
        let order: Vec<_> = tx.inputs.iter().map(|input| input.previous).collect();
        let signed: Vec<_> = spent.iter().map(|coin| coin.outpoint).collect();
        assert_eq!(order, signed);
        let order: Vec<(u64, u8)> = tx
            .outputs
            .iter()
            .map(|out| (out.value, out.script[0]))
            .collect();
        assert_eq!(order, [(3, 9), (7, 1), (7, 2)]);
    }
}
