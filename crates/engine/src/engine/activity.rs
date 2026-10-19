use bahaa_store::{Outgoing, SentTx};
use bahaa_types::Address;

use super::{EngineError, Worker};
use crate::WalletId;
use crate::caution::Arrival;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Activity {
    pub wallet: WalletId,
    pub time: u64,
    pub kind: ActivityKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActivityKind {
    Sent(SentTx),
    SentElsewhere(Outgoing),
    Received(Arrival),
}

impl Worker {
    pub(super) fn activities(&self) -> Result<Vec<Activity>, EngineError> {
        let elsewhere = self.outgoing()?;
        let received = self.incoming()?.transfers;
        let owner = |address: &Address| {
            self.accounts
                .iter()
                .find(|account| account.address == *address)
                .map(|account| account.wallet)
        };
        let sent = self.history.iter().filter_map(|tx| {
            Some(Activity {
                wallet: owner(&tx.from)?,
                time: tx.created_at,
                kind: ActivityKind::Sent(tx.clone()),
            })
        });
        let elsewhere = elsewhere.into_iter().filter_map(|tx| {
            Some(Activity {
                wallet: owner(&tx.from)?,
                time: tx.time,
                kind: ActivityKind::SentElsewhere(tx),
            })
        });
        let received = received.into_iter().filter_map(|arrival| {
            Some(Activity {
                wallet: owner(&arrival.received.to)?,
                time: arrival.received.time,
                kind: ActivityKind::Received(arrival),
            })
        });
        let mut all: Vec<Activity> = sent.chain(elsewhere).chain(received).collect();
        all.sort_by_key(|activity| std::cmp::Reverse(activity.time));
        Ok(all)
    }
}
