use bahaa_chain::{ChainError, Signed, TxId, TxStatus};
use bahaa_types::Address;

use super::send::LIFETIME;
use super::{TonChain, amount};
use crate::transaction::{Outcome, Transaction};

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
        let Some(mut next) = self.http.account(from).await?.last else {
            return Ok(TxStatus::NotFound);
        };
        let mut searched = 0;
        loop {
            for transaction in self.http.transactions(from, next).await? {
                if u64::from(transaction.now) < cutoff {
                    return Ok(TxStatus::NotFound);
                }
                if transaction.external == Some(wanted) {
                    return self.settled(&transaction);
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
                return Ok(TxStatus::NotFound);
            }
        }
    }

    fn settled(&self, transaction: &Transaction) -> Result<TxStatus, ChainError> {
        let unreadable =
            || ChainError::Network("a server sent an unreadable transaction".to_owned());
        match transaction.outcome().ok_or_else(unreadable)? {
            Outcome::Failed(reason) => Ok(TxStatus::Failed(reason.to_owned())),
            Outcome::Sent { forward } => {
                let paid = transaction
                    .fees
                    .checked_add(forward)
                    .ok_or_else(unreadable)?;
                Ok(TxStatus::Confirmed {
                    block: None,
                    fee: Some(amount(paid, self.network.native.decimals)?),
                })
            }
        }
    }
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
}
