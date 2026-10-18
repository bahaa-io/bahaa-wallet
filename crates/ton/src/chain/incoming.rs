use std::time::{SystemTime, UNIX_EPOCH};

use bahaa_chain::{ChainError, Incoming, ScanCursor, Scanned, TxId};
use bahaa_config::Protocol;
use bahaa_types::{Address, AssetId};

use super::{TonChain, amount, hex};
use crate::transaction::{Credit, Transaction};

const MAX_SCANNED: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum End {
    Missing { checked: u64 },
    Start { lt: u64, now: u32 },
    Connected { from: u64, since: u64 },
    Capped { lt: u64, now: u32 },
}

impl TonChain {
    pub fn can_scan(&self) -> bool {
        self.network.endpoints.iter().any(|endpoint| {
            matches!(
                endpoint.protocol,
                Protocol::TonLiteservers | Protocol::TonApi
            )
        })
    }

    pub async fn scan(
        &self,
        owner: &Address,
        assets: &[AssetId],
        cursor: Option<ScanCursor>,
    ) -> Result<Option<Scanned>, ChainError> {
        let native = AssetId::Native(self.network.id.clone());
        if !assets.contains(&native) {
            return Ok(None);
        }
        let head = self.http.account(owner).await?.last;
        let newest = head.map_or(0, |(_, lt)| lt.saturating_add(1));
        if cursor.is_some_and(|cursor| cursor.next >= newest) {
            return Ok(None);
        }
        let stop = cursor.map_or(0, |cursor| cursor.next);
        let mut found = Vec::new();
        let mut end = End::Missing {
            checked: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs()),
        };
        let mut scanned = 0;
        let mut next = head;
        'walk: while let Some(from) = next {
            for transaction in self.http.transactions(owner, from).await? {
                if let Some(credit) = &transaction.credit {
                    found.push(self.incoming(owner, &native, &transaction, credit)?);
                }
                scanned += 1;
                let previous = transaction.previous;
                next = Some(previous);
                if previous.1 == 0 {
                    next = None;
                    end = End::Start {
                        lt: transaction.lt,
                        now: transaction.now,
                    };
                } else if let Some(cursor) = cursor.filter(|_| previous.1 < stop) {
                    next = None;
                    end = End::Connected {
                        from: cursor.complete_from,
                        since: cursor.complete_since,
                    };
                } else if scanned >= MAX_SCANNED {
                    next = None;
                    end = End::Capped {
                        lt: transaction.lt,
                        now: transaction.now,
                    };
                }
                if next.is_none() {
                    break 'walk;
                }
            }
        }
        Ok(Some(Scanned {
            cursor: covered(newest, end),
            found,
        }))
    }

    fn incoming(
        &self,
        owner: &Address,
        native: &AssetId,
        transaction: &Transaction,
        credit: &Credit,
    ) -> Result<Incoming, ChainError> {
        Ok(Incoming {
            id: TxId::new(hex(&transaction.hash))?,
            index: 0,
            from: credit.from,
            to: *owner,
            asset: native.clone(),
            amount: amount(credit.nano, self.network.native.decimals)?,
            block: transaction.lt,
            time: u64::from(transaction.now),
        })
    }
}

fn covered(newest: u64, end: End) -> ScanCursor {
    let (complete_from, complete_since) = match end {
        End::Missing { checked } => (0, checked),
        End::Start { lt, now } => (lt, u64::from(now)),
        End::Connected { from, since } => (from, since),
        End::Capped { lt, now } => (lt, u64::from(now).saturating_add(1)),
    };
    ScanCursor {
        next: newest,
        complete_from,
        complete_since,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_says_how_far_back_the_scan_is_complete() {
        assert_eq!(
            covered(
                0,
                End::Missing {
                    checked: 1_700_000_900
                }
            ),
            ScanCursor {
                next: 0,
                complete_from: 0,
                complete_since: 1_700_000_900
            }
        );
        assert_eq!(
            covered(
                90,
                End::Start {
                    lt: 30,
                    now: 1_700_000_100
                }
            ),
            ScanCursor {
                next: 90,
                complete_from: 30,
                complete_since: 1_700_000_100
            }
        );
        assert_eq!(
            covered(
                90,
                End::Connected {
                    from: 12,
                    since: 1_700_000_000
                }
            ),
            ScanCursor {
                next: 90,
                complete_from: 12,
                complete_since: 1_700_000_000
            }
        );
        assert_eq!(
            covered(
                90,
                End::Capped {
                    lt: 40,
                    now: 1_700_000_500
                }
            ),
            ScanCursor {
                next: 90,
                complete_from: 40,
                complete_since: 1_700_000_501
            }
        );
    }
}
