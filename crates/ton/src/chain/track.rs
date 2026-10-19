use bahaa_chain::{ChainError, Signed, TxId, TxStatus};
use bahaa_types::Address;

use ruint::aliases::U256;

use super::send::LIFETIME;
use super::{TonChain, amount, hex};
use crate::transaction::{INTERNAL_TRANSFER, Outcome, Outgoing, Transaction};
use crate::wallet;

const SEARCH_SLACK: u64 = 3_600;
const MAX_SEARCHED: usize = 256;

impl TonChain {
    pub(super) async fn send_signed(&self, signed: &Signed) -> Result<TxId, ChainError> {
        self.http.send(&signed.bytes).await?;
        Ok(signed.id.clone())
    }

    pub(super) async fn track(
        &self,
        id: &TxId,
        from: &Address,
        expires_at: Option<u64>,
    ) -> Result<TxStatus, ChainError> {
        let wanted = message_id(id).ok_or(ChainError::Invalid("not a TON message id"))?;
        let expires = expires_at.ok_or(ChainError::Invalid(
            "a TON transfer can only be tracked with its expiry",
        ))?;
        let cutoff = expires.saturating_sub(LIFETIME + SEARCH_SLACK);
        let Some(first) = self
            .find(from, cutoff, |transaction| {
                transaction.external == Some(wanted)
            })
            .await?
        else {
            return Ok(TxStatus::NotFound);
        };
        let forward = match first.outcome().ok_or_else(unreadable)? {
            Outcome::Failed(reason) => return Ok(TxStatus::Failed(reason.to_owned())),
            Outcome::Sent { forward } => forward,
        };
        let paid = first.fees.checked_add(forward).ok_or_else(unreadable)?;
        match first
            .sent
            .iter()
            .find(|message| message.op == Some(wallet::TOKEN_TRANSFER))
        {
            None => self.confirmed(&first, paid),
            Some(transfer) => self.followed(from, &first, transfer, paid).await,
        }
    }

    async fn followed(
        &self,
        from: &Address,
        first: &Transaction,
        transfer: &Outgoing,
        paid: U256,
    ) -> Result<TxStatus, ChainError> {
        let Some(second) = self.received(&transfer.to, first, transfer).await? else {
            return Ok(TxStatus::Pending);
        };
        let forwarded = match forwarded(&second) {
            Ok(forwarded) => forwarded,
            Err(failed) => return Ok(failed),
        };
        let Some(third) = self.received(&forwarded.to, &second, forwarded).await? else {
            return Ok(TxStatus::Pending);
        };
        let (returned, processing) = match refunded(&third, from) {
            Err(failed) => return Ok(failed),
            Ok(None) => (U256::ZERO, U256::ZERO),
            Ok(Some(refund)) => match self.received(from, &third, refund).await? {
                Some(back) => (refund.value, back.fees),
                None => return Ok(TxStatus::Pending),
            },
        };
        let spent = paid
            .checked_add(transfer.value)
            .and_then(|total| total.checked_sub(returned))
            .and_then(|total| total.checked_add(processing))
            .ok_or_else(unreadable)?;
        self.confirmed(first, spent)
    }

    async fn received(
        &self,
        account: &Address,
        after: &Transaction,
        message: &Outgoing,
    ) -> Result<Option<Transaction>, ChainError> {
        let hash = message.hash;
        self.find(account, u64::from(after.now), move |transaction| {
            transaction.received == Some(hash)
        })
        .await
    }

    async fn find(
        &self,
        account: &Address,
        since: u64,
        matches: impl Fn(&Transaction) -> bool + Send,
    ) -> Result<Option<Transaction>, ChainError> {
        let Some(mut next) = self.http.account(account).await?.last else {
            return Ok(None);
        };
        let mut searched = 0;
        loop {
            for transaction in self.http.transactions(account, next).await? {
                if u64::from(transaction.now) < since {
                    return Ok(None);
                }
                if matches(&transaction) {
                    return Ok(Some(transaction));
                }
                searched += 1;
                if searched >= MAX_SEARCHED {
                    return Err(ChainError::Network(
                        "the wallet history is too busy to search".to_owned(),
                    ));
                }
                next = transaction.previous;
            }
            if next.1 == 0 {
                return Ok(None);
            }
        }
    }

    fn confirmed(&self, first: &Transaction, paid: U256) -> Result<TxStatus, ChainError> {
        Ok(TxStatus::Confirmed {
            block: Some(first.lt),
            fee: Some(amount(paid, self.network.native.decimals)?),
            transaction: Some(TxId::new(hex(&first.hash))?),
        })
    }
}

fn forwarded(second: &Transaction) -> Result<&Outgoing, TxStatus> {
    if second.succeeded() != Some(true) {
        return Err(TxStatus::Failed(
            "your token wallet refused the transfer and sent the GRAM back".to_owned(),
        ));
    }
    second
        .sent
        .iter()
        .find(|message| message.op == Some(INTERNAL_TRANSFER))
        .ok_or_else(|| TxStatus::Failed("your token wallet did not forward the tokens".to_owned()))
}

fn refunded<'a>(third: &'a Transaction, from: &Address) -> Result<Option<&'a Outgoing>, TxStatus> {
    if third.succeeded() != Some(true) {
        return Err(TxStatus::Failed(
            "the recipient's token wallet refused the transfer and the tokens came back".to_owned(),
        ));
    }
    Ok(third.sent.iter().find(|message| message.to == *from))
}

fn unreadable() -> ChainError {
    ChainError::Network("a server sent an unreadable transaction".to_owned())
}

fn message_id(id: &TxId) -> Option<[u8; 32]> {
    let text = id.as_str();
    if text.len() != 64 {
        return None;
    }
    (0..64)
        .step_by(2)
        .map(|at| u8::from_str_radix(text.get(at..at + 2)?, 16).ok())
        .collect::<Option<Vec<u8>>>()?
        .try_into()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_ids_are_sixty_four_hex_digits() {
        let id = |text: String| message_id(&TxId::new(text).unwrap());
        assert_eq!(id("ab".repeat(32)), Some([0xab; 32]));
        assert_eq!(id("ab".repeat(31)), None);
        assert_eq!(id("ab".repeat(33)), None);
        assert_eq!(id("zz".repeat(32)), None);
    }

    #[test]
    fn each_token_hop_is_judged_by_its_own_transaction() {
        use crate::emulate::tests::{OURS, SENDER, THEIRS, done, emulator, order, state, ton};
        use crate::transaction::single;
        let hash = |text: &str| {
            let Address::Ton { hash, .. } = ton(text) else {
                panic!("not TON");
            };
            hash
        };
        let emulator = emulator();
        let delivered = |units: u64| crate::wallet::delivered(&order(units, 50_000_000)).unwrap();
        let first = done(
            emulator
                .run(&ton(OURS), &state(OURS), &delivered(1_000_000))
                .unwrap(),
        );
        let ours = single(&first.transaction, &hash(OURS)).unwrap();
        let next = forwarded(&ours).unwrap();
        assert_eq!(next.to, ton(THEIRS));
        let second = done(
            emulator
                .run(&ton(THEIRS), &state(THEIRS), &first.out[0])
                .unwrap(),
        );
        let theirs = single(&second.transaction, &hash(THEIRS)).unwrap();
        assert_eq!(theirs.received, Some(next.hash));
        let refund = refunded(&theirs, &ton(SENDER)).unwrap().unwrap();
        assert!(
            refund.value > U256::from(40_000_000u32) && refund.value < U256::from(50_000_000u32)
        );
        assert_eq!(refunded(&theirs, &ton(OURS)), Ok(None));
        assert!(matches!(
            forwarded(&theirs),
            Err(TxStatus::Failed(reason)) if reason.contains("did not forward")
        ));
        let refusing = done(
            emulator
                .run(&ton(OURS), &state(OURS), &delivered(u64::MAX >> 8))
                .unwrap(),
        );
        let refused = single(&refusing.transaction, &hash(OURS)).unwrap();
        assert!(matches!(
            forwarded(&refused),
            Err(TxStatus::Failed(reason)) if reason.contains("refused")
        ));
        assert!(matches!(
            refunded(&refused, &ton(SENDER)),
            Err(TxStatus::Failed(reason)) if reason.contains("came back")
        ));
    }
}
