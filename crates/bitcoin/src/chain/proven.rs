use std::collections::{HashMap, HashSet};

use bahaa_chain::ChainError;
use bahaa_config::{BlockCheckpoint, ChainKind, RETARGET_BLOCKS};

use super::account::{Coin, script};
use super::{BitcoinChain, spent};
use crate::header::block_hash;
use crate::proof::{Header, Headers, has_work, proves, unproven};
use crate::script::locking;
use crate::tx::{Hash, Outpoint};

const CHUNK: u64 = 2_016;
const REORG_DEPTH: usize = 12;
const MAX_REWINDS: usize = 3;

#[derive(Debug, Default)]
pub(crate) struct Trusted {
    headers: Option<Headers>,
    proven: HashSet<(Hash, u64)>,
    coins: HashSet<Outpoint>,
}

impl Trusted {
    pub(crate) fn forget(&mut self) {
        self.proven.clear();
        self.coins.clear();
    }
}

impl BitcoinChain {
    pub(crate) async fn check(&self, coins: &[Coin]) -> Result<(), ChainError> {
        let fresh: Vec<&Coin> = {
            let trusted = self.trusted();
            coins
                .iter()
                .filter(|coin| coin.height > 0 && !trusted.coins.contains(&coin.outpoint))
                .collect()
        };
        if fresh.is_empty() {
            return Ok(());
        }
        let mut known = HashMap::new();
        self.load(&mut known, fresh.iter().map(|coin| coin.outpoint.txid))
            .await?;
        for coin in &fresh {
            let paid = spent(&known, &coin.outpoint).is_some_and(|output| {
                output.value == coin.value
                    && script(&coin.address).is_ok_and(|kind| output.script == locking(kind))
            });
            if !paid {
                return Err(unproven("a coin it listed"));
            }
        }
        let items: Vec<(Hash, u64)> = fresh
            .iter()
            .map(|coin| {
                Ok((
                    coin.outpoint.txid,
                    u64::try_from(coin.height).map_err(|_| ChainError::malformed())?,
                ))
            })
            .collect::<Result<_, ChainError>>()?;
        self.prove(&items).await?;
        self.trusted()
            .coins
            .extend(fresh.iter().map(|coin| coin.outpoint));
        Ok(())
    }

    pub(crate) async fn prove(&self, items: &[(Hash, u64)]) -> Result<(), ChainError> {
        let mut fresh: Vec<(Hash, u64)> = {
            let trusted = self.trusted();
            items
                .iter()
                .filter(|item| !trusted.proven.contains(item))
                .copied()
                .collect()
        };
        fresh.sort_unstable();
        fresh.dedup();
        if fresh.is_empty() {
            return Ok(());
        }
        let branches = self.sources.merkle(&fresh).await?;
        let heights: Vec<u64> = fresh.iter().map(|(_, height)| *height).collect();
        let headers = self.headers(&heights).await?;
        for (((txid, _), branch), header) in fresh.iter().zip(&branches).zip(&headers) {
            if !proves(header, txid, &branch.hashes, branch.pos) {
                return Err(unproven("that a transaction is in its block"));
            }
        }
        self.trusted().proven.extend(fresh);
        Ok(())
    }

    pub(crate) async fn headers(&self, heights: &[u64]) -> Result<Vec<Header>, ChainError> {
        let (checkpoint, _) = self.consensus()?;
        if heights.iter().any(|height| *height >= checkpoint.height) {
            let _ = self.sync().await;
        }
        let mut found: Vec<Option<Header>> = {
            let trusted = self.trusted();
            heights
                .iter()
                .map(|height| trusted.headers.as_ref()?.get(*height).copied())
                .collect()
        };
        let missing: Vec<u64> = heights
            .iter()
            .zip(&found)
            .filter(|(_, header)| header.is_none())
            .map(|(height, _)| *height)
            .collect();
        if !missing.is_empty() {
            let (_, limit) = self.consensus()?;
            let mut fetched = self.sources.headers(&missing).await?.into_iter();
            for slot in found.iter_mut().filter(|slot| slot.is_none()) {
                let header: Header = fetched
                    .next()
                    .and_then(|bytes| bytes.try_into().ok())
                    .ok_or_else(ChainError::malformed)?;
                if !has_work(&header, limit) {
                    return Err(unproven("the work in a block header"));
                }
                *slot = Some(header);
            }
        }
        found
            .into_iter()
            .map(|header| header.ok_or_else(ChainError::malformed))
            .collect()
    }

    pub async fn next_checkpoint(&self) -> Result<BlockCheckpoint, ChainError> {
        let (old, _) = self.consensus()?;
        self.sync().await?;
        let trusted = self.trusted();
        let headers = trusted.headers.as_ref().ok_or_else(ChainError::malformed)?;
        let tip = headers.next().saturating_sub(1);
        let height = ((tip / RETARGET_BLOCKS).saturating_sub(1) * RETARGET_BLOCKS).max(old.height);
        let hash = headers
            .get(height)
            .and_then(|header| block_hash(header))
            .ok_or_else(ChainError::malformed)?;
        Ok(BlockCheckpoint { height, hash })
    }

    async fn sync(&self) -> Result<(), ChainError> {
        let _turn = self.syncing.lock().await;
        let (checkpoint, limit) = self.consensus()?;
        let mut rewinds = 0;
        loop {
            let start = self
                .trusted()
                .headers
                .as_ref()
                .map_or(checkpoint.height, Headers::next);
            let fetched = self.sources.range(start, CHUNK).await?;
            {
                let mut trusted = self.trusted();
                if trusted.headers.is_none() {
                    let first = *fetched.first().ok_or_else(ChainError::malformed)?;
                    trusted.headers = Some(Headers::start(&checkpoint, first, limit)?);
                }
                let headers = trusted.headers.as_mut().ok_or_else(ChainError::malformed)?;
                let new = if start == checkpoint.height {
                    fetched.get(1..).unwrap_or_default()
                } else {
                    &fetched
                };
                if let Some(first) = new.first()
                    && !headers.links(first)
                {
                    rewinds += 1;
                    if rewinds > MAX_REWINDS {
                        return Err(unproven("a block header"));
                    }
                    headers.rewind(REORG_DEPTH);
                    continue;
                }
                headers.extend(new, limit)?;
            }
            if (fetched.len() as u64) < CHUNK {
                return Ok(());
            }
        }
    }

    fn consensus(&self) -> Result<(BlockCheckpoint, u32), ChainError> {
        match self.network.kind {
            ChainKind::Bitcoin {
                checkpoint,
                pow_limit,
                ..
            } => Ok((checkpoint, pow_limit)),
            _ => Err(ChainError::Invalid("not a Bitcoin network")),
        }
    }
}
