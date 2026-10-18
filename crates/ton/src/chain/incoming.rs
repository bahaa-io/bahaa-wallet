use std::time::{SystemTime, UNIX_EPOCH};

use bahaa_chain::{ChainError, Incoming, ScanCursor, Scanned, TxId};
use bahaa_config::Protocol;
use bahaa_types::{Address, AssetId};

use super::{TonChain, amount, hex};
use crate::transaction::Transaction;

const MAX_SCANNED: usize = 64;
const SETTLE: u64 = 300;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum End {
    Missing,
    Start { now: u32 },
    Connected,
    Capped { now: u32 },
}

struct History {
    address: Address,
    asset: AssetId,
    decimals: u8,
    tokens: bool,
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
        let histories = self.histories(owner, assets).await?;
        if histories.is_empty() {
            return Ok(None);
        }
        let stop = cursor.map_or(0, |cursor| cursor.next);
        let mut found = Vec::new();
        let mut walked = Vec::with_capacity(histories.len());
        for history in &histories {
            walked.push(self.walk(owner, history, stop, &mut found).await?);
        }
        let checked = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        let next = covered(cursor, &walked, checked);
        if cursor.is_some_and(|cursor| cursor.next == next.next) && found.is_empty() {
            return Ok(None);
        }
        Ok(Some(Scanned {
            cursor: next,
            found,
        }))
    }

    async fn histories(
        &self,
        owner: &Address,
        assets: &[AssetId],
    ) -> Result<Vec<History>, ChainError> {
        let mut histories = Vec::new();
        for asset in assets
            .iter()
            .filter(|asset| asset.network() == &self.network.id)
        {
            match asset.contract() {
                None => histories.push(History {
                    address: *owner,
                    asset: asset.clone(),
                    decimals: self.network.native.decimals,
                    tokens: false,
                }),
                Some(master) => {
                    let Some(token) = self
                        .network
                        .tokens
                        .iter()
                        .find(|token| token.contract == *master)
                    else {
                        continue;
                    };
                    histories.push(History {
                        address: self.jetton_wallet(owner, master).await?,
                        asset: asset.clone(),
                        decimals: token.coin.decimals,
                        tokens: true,
                    });
                }
            }
        }
        Ok(histories)
    }

    async fn walk(
        &self,
        owner: &Address,
        history: &History,
        stop: u64,
        found: &mut Vec<Incoming>,
    ) -> Result<(Option<u32>, End), ChainError> {
        let Some(mut next) = self.http.account(&history.address).await?.last else {
            return Ok((None, End::Missing));
        };
        let mut newest = None;
        let mut oldest = None;
        let mut scanned = 0;
        loop {
            let list = match self.http.transactions(&history.address, next).await {
                Ok(list) => list,
                Err(err) => {
                    return match oldest {
                        Some(now) => Ok((newest, End::Capped { now })),
                        None => Err(err),
                    };
                }
            };
            for transaction in list {
                newest.get_or_insert(transaction.now);
                if u64::from(transaction.now) < stop {
                    return Ok((newest, End::Connected));
                }
                if let Some(entry) = receipt(owner, history, &transaction)? {
                    found.push(entry);
                }
                scanned += 1;
                oldest = Some(transaction.now);
                next = transaction.previous;
                if next.1 == 0 {
                    return Ok((
                        newest,
                        End::Start {
                            now: transaction.now,
                        },
                    ));
                }
                if scanned >= MAX_SCANNED {
                    return Ok((
                        newest,
                        End::Capped {
                            now: transaction.now,
                        },
                    ));
                }
            }
        }
    }
}

fn receipt(
    owner: &Address,
    history: &History,
    transaction: &Transaction,
) -> Result<Option<Incoming>, ChainError> {
    let credit = if history.tokens {
        &transaction.tokens
    } else {
        &transaction.credit
    };
    let Some(credit) = credit else {
        return Ok(None);
    };
    Ok(Some(Incoming {
        id: TxId::new(hex(&transaction.hash))?,
        index: 0,
        from: credit.from,
        to: *owner,
        asset: history.asset.clone(),
        amount: amount(credit.units, history.decimals)?,
        block: transaction.lt,
        time: u64::from(transaction.now),
    }))
}

fn covered(cursor: Option<ScanCursor>, walked: &[(Option<u32>, End)], checked: u64) -> ScanCursor {
    let stop = cursor.map_or(0, |cursor| cursor.next);
    let newest = walked.iter().filter_map(|(head, _)| *head).max();
    let seen = newest.map_or(checked, |newest| checked.min(u64::from(newest)));
    let next = stop.max(seen.saturating_sub(SETTLE));
    let since = walked
        .iter()
        .filter_map(|(_, end)| match end {
            End::Missing => None,
            End::Start { now } => Some(u64::from(*now)),
            End::Connected => cursor.map(|cursor| cursor.complete_since),
            End::Capped { now } => Some(u64::from(*now).saturating_add(1)),
        })
        .max()
        .unwrap_or_else(|| cursor.map_or(checked, |cursor| cursor.complete_since))
        .min(next);
    ScanCursor {
        next,
        complete_from: since,
        complete_since: since,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_792_300_000;

    fn cursor(next: u64, since: u64) -> ScanCursor {
        ScanCursor {
            next,
            complete_from: since,
            complete_since: since,
        }
    }

    #[test]
    fn coverage_is_the_latest_point_every_history_reaches() {
        let first = covered(
            None,
            &[
                (Some(1_792_200_000), End::Capped { now: 1_791_000_000 }),
                (Some(1_792_100_000), End::Start { now: 1_791_500_000 }),
                (None, End::Missing),
            ],
            NOW,
        );
        assert_eq!(first, cursor(1_792_199_700, 1_791_500_000));
        let later = covered(
            Some(first),
            &[
                (Some(1_792_250_000), End::Connected),
                (Some(1_792_150_000), End::Connected),
            ],
            NOW,
        );
        assert_eq!(later, cursor(1_792_249_700, 1_791_500_000));
        let gap = covered(
            Some(later),
            &[(Some(1_792_290_000), End::Capped { now: 1_792_260_000 })],
            NOW,
        );
        assert_eq!(gap, cursor(1_792_289_700, 1_792_260_001));
    }

    #[test]
    fn the_cursor_never_runs_ahead_of_the_clock_or_backwards() {
        assert_eq!(
            covered(None, &[(None, End::Missing)], NOW),
            cursor(NOW - SETTLE, NOW - SETTLE)
        );
        assert_eq!(
            covered(
                None,
                &[(Some(1_900_000_000), End::Start { now: 1_800_000_000 })],
                NOW
            ),
            cursor(NOW - SETTLE, NOW - SETTLE)
        );
        let ahead = cursor(NOW, 5);
        assert_eq!(
            covered(Some(ahead), &[(Some(1_700_000_000), End::Connected)], NOW),
            cursor(NOW, 5)
        );
        assert_eq!(
            covered(Some(ahead), &[(None, End::Missing)], NOW),
            cursor(NOW, 5)
        );
    }
}
