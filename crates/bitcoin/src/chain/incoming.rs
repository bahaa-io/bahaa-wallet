use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use bahaa_chain::{ChainError, Movement, ScanCursor, Scanned, TxId, WalkEnd, covered, now};
use bahaa_config::Feature;
use bahaa_types::{Address, AssetId};

use super::account::Book;
use super::{BitcoinChain, spent};
use crate::header::time;
use crate::source::Seen;
use crate::tx::{Hash, Tx, id_text};

const MARGIN: u64 = 7_200;
const PAGE: usize = 20;
const MAX_LOOKUPS: usize = 20;

#[derive(Default)]
struct Found {
    received: Vec<Movement>,
    sent: Vec<Movement>,
}

impl BitcoinChain {
    pub fn can_scan(&self) -> bool {
        self.network
            .endpoints
            .iter()
            .any(|endpoint| endpoint.has(Feature::History))
    }

    pub async fn last_used(&self, address: &Address) -> Result<Option<u64>, ChainError> {
        let seen = self.histories(&[*address]).await?;
        Ok(seen
            .into_iter()
            .flatten()
            .map(|seen| {
                u64::try_from(seen.height)
                    .ok()
                    .filter(|height| *height > 0)
                    .unwrap_or(u64::MAX)
            })
            .max())
    }

    pub async fn scan(
        &self,
        owner: &Address,
        assets: &[AssetId],
        cursor: Option<ScanCursor>,
    ) -> Result<Option<Scanned>, ChainError> {
        if !assets.contains(&AssetId::Native(self.network.id.clone())) {
            return Ok(None);
        }
        let used = self.used(owner).await?;
        let mine: HashSet<Hash> = used
            .histories
            .iter()
            .flat_map(|(_, seen)| seen.iter().map(|seen| seen.txid))
            .collect();
        let mut listed: Vec<Seen> = used
            .histories
            .into_iter()
            .flat_map(|(_, seen)| seen)
            .filter(|seen| seen.height > 0)
            .collect();
        listed.sort_by_key(|seen| (Reverse(seen.height), seen.txid));
        listed.dedup_by_key(|seen| seen.txid);
        let stop = cursor
            .map_or(0, |cursor| cursor.next)
            .saturating_sub(MARGIN);
        let mut known = HashMap::new();
        let mut found = Found::default();
        let mut newest = None;
        let mut oldest = None;
        let mut looked = 0;
        let mut end = None;
        for page in listed.chunks(PAGE) {
            let times = self.times(page).await?;
            let mut wanted = Vec::new();
            for (seen, time) in page.iter().zip(times) {
                newest.get_or_insert(time);
                if time < stop {
                    end = Some(WalkEnd::Connected);
                    break;
                }
                if looked == MAX_LOOKUPS {
                    end = Some(WalkEnd::Capped(time));
                    break;
                }
                looked += 1;
                wanted.push((*seen, time));
            }
            self.gather(owner, &mine, &wanted, &mut known).await?;
            let book = self.book();
            for (seen, time) in &wanted {
                match self.movement(&book, owner, &known, *seen, *time)? {
                    Some((true, moved)) => found.received.push(moved),
                    Some((false, moved)) => found.sent.push(moved),
                    None => {}
                }
                oldest = Some(*time);
            }
            if end.is_some() {
                break;
            }
        }
        let end = end.unwrap_or(match oldest {
            Some(time) => WalkEnd::Start(time),
            None => WalkEnd::Missing,
        });
        let next = covered(cursor, &[(newest, end)], now());
        let Found { received, sent } = found;
        if cursor.is_some_and(|cursor| cursor.next == next.next)
            && received.is_empty()
            && sent.is_empty()
        {
            return Ok(None);
        }
        Ok(Some(Scanned {
            cursor: next,
            found: received,
            sent,
        }))
    }

    async fn gather(
        &self,
        owner: &Address,
        mine: &HashSet<Hash>,
        wanted: &[(Seen, u64)],
        known: &mut HashMap<Hash, Tx>,
    ) -> Result<(), ChainError> {
        self.load(known, wanted.iter().map(|(seen, _)| seen.txid))
            .await?;
        let proofs = wanted
            .iter()
            .map(|(seen, _)| {
                u64::try_from(seen.height)
                    .map(|height| (seen.txid, height))
                    .map_err(|_| ChainError::malformed())
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.prove(&proofs).await?;
        let previous: Vec<Hash> = wanted
            .iter()
            .flat_map(|(seen, _)| &known[&seen.txid].inputs)
            .map(|input| input.previous.txid)
            .filter(|id| mine.contains(id))
            .collect();
        self.load(known, previous).await?;
        let senders: Vec<Hash> = {
            let book = self.book();
            wanted
                .iter()
                .map(|(seen, _)| &known[&seen.txid])
                .filter(|tx| !self.spends(&book, owner, tx, known))
                .filter_map(|tx| tx.inputs.first())
                .map(|input| input.previous.txid)
                .collect()
        };
        self.load(known, senders).await
    }

    async fn times(&self, page: &[Seen]) -> Result<Vec<u64>, ChainError> {
        let heights = page
            .iter()
            .map(|seen| u64::try_from(seen.height).map_err(|_| ChainError::malformed()))
            .collect::<Result<Vec<u64>, _>>()?;
        let headers = self.headers(&heights).await?;
        headers
            .iter()
            .map(|header| time(header).ok_or_else(ChainError::malformed))
            .collect()
    }

    fn spends(&self, book: &Book, owner: &Address, tx: &Tx, known: &HashMap<Hash, Tx>) -> bool {
        tx.inputs.iter().any(|input| {
            spent(known, &input.previous)
                .and_then(|output| self.address(&output.script))
                .is_some_and(|address| book.holds(owner, &address))
        })
    }

    fn movement(
        &self,
        book: &Book,
        owner: &Address,
        known: &HashMap<Hash, Tx>,
        seen: Seen,
        time: u64,
    ) -> Result<Option<(bool, Movement)>, ChainError> {
        let tx = &known[&seen.txid];
        let outputs: Vec<(Address, u64)> = tx
            .outputs
            .iter()
            .filter_map(|output| Some((self.address(&output.script)?, output.value)))
            .collect();
        let received = !self.spends(book, owner, tx, known);
        let (paid, other): (Vec<_>, Address) = if received {
            let Some(from) = tx
                .inputs
                .first()
                .and_then(|input| spent(known, &input.previous))
                .and_then(|output| self.address(&output.script))
            else {
                return Ok(None);
            };
            let ours = outputs
                .into_iter()
                .filter(|(address, _)| book.holds(owner, address))
                .collect();
            (ours, from)
        } else {
            let theirs: Vec<(Address, u64)> = outputs
                .into_iter()
                .filter(|(address, _)| !book.holds(owner, address))
                .collect();
            let Some((to, _)) = theirs.first().copied() else {
                return Ok(None);
            };
            (theirs, to)
        };
        let units = paid
            .iter()
            .try_fold(0u64, |sum, (_, value)| sum.checked_add(*value))
            .ok_or_else(ChainError::overflow)?;
        if units == 0 {
            return Ok(None);
        }
        let (from, to) = if received {
            (other, *owner)
        } else {
            (*owner, other)
        };
        Ok(Some((
            received,
            Movement {
                id: TxId::new(id_text(&seen.txid))?,
                index: 0,
                from,
                to,
                asset: AssetId::Native(self.network.id.clone()),
                amount: self.sats(units)?,
                block: u64::try_from(seen.height).map_err(|_| ChainError::malformed())?,
                time,
                memo: None,
            },
        )))
    }
}
