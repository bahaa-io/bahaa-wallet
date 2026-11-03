use std::collections::{BTreeSet, HashMap};
use std::sync::{Mutex, MutexGuard, PoisonError};

use bahaa_chain::{ChainError, Failure, Pool, TxId, http};
use bahaa_config::Endpoint;
use bahaa_types::{hex, to_plain_text};
use futures_util::stream::{self, StreamExt, TryStreamExt};
use reqwest::{Client, StatusCode};
use serde_json::Value;
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};

use crate::header::block_hash;
use crate::source::{Branch, Estimates, Seen, TARGETS, Unspent, already, branch, rate};
use crate::tx::{Hash, MAX_MONEY, Outpoint};

const PAGE: usize = 25;
const MAX_PAGES: usize = 10;
const PARALLEL: usize = 4;
const MAX_REFUSAL: usize = 200;

#[derive(Debug)]
pub(crate) struct Esplora {
    pool: Pool,
    http: Client,
    genesis: String,
    verified: Mutex<BTreeSet<String>>,
}

impl Esplora {
    pub(crate) fn new(endpoints: Vec<Endpoint>, genesis: &[u8; 32]) -> Result<Self, ChainError> {
        Ok(Self {
            pool: Pool::new(endpoints),
            http: http::client()?,
            genesis: hex::encode(genesis),
            verified: Mutex::new(BTreeSet::new()),
        })
    }

    pub(crate) async fn histories(
        &self,
        scripts: &[Vec<u8>],
    ) -> Result<Vec<Vec<Seen>>, ChainError> {
        self.each(scripts, |esplora, endpoint, script| {
            esplora.history(endpoint, script)
        })
        .await
    }

    pub(crate) async fn unspent(
        &self,
        scripts: &[Vec<u8>],
    ) -> Result<Vec<Vec<Unspent>>, ChainError> {
        self.each(scripts, |esplora, endpoint, script| {
            esplora.coins(endpoint, script)
        })
        .await
    }

    pub(crate) async fn transactions(
        &self,
        ids: &[Hash],
    ) -> Result<Vec<Option<Vec<u8>>>, ChainError> {
        self.each(ids, Self::raw).await
    }

    pub(crate) async fn headers(&self, heights: &[u64]) -> Result<Vec<Vec<u8>>, ChainError> {
        self.each(heights, Self::header).await
    }

    pub(crate) async fn merkle(&self, items: &[(Hash, u64)]) -> Result<Vec<Branch>, ChainError> {
        self.each(items, Self::proof).await
    }

    pub(crate) async fn estimates(&self) -> Result<Estimates, ChainError> {
        Ok(self
            .pool
            .call(None, |endpoint| async move {
                self.verify(&endpoint).await?;
                let body = self
                    .get(&endpoint, "fee-estimates")
                    .await?
                    .ok_or_else(malformed)?;
                let estimates: HashMap<String, Box<RawValue>> =
                    serde_json::from_slice(&body).map_err(|_| malformed())?;
                Ok(Estimates {
                    relay: None,
                    targets: TARGETS
                        .iter()
                        .filter_map(|blocks| {
                            let text = estimates.get(&blocks.to_string())?;
                            Some((*blocks, rate(text.get(), 1_000)?))
                        })
                        .collect(),
                })
            })
            .await?)
    }

    pub(crate) async fn broadcast(&self, raw: &[u8], id: &TxId) -> Result<TxId, ChainError> {
        let body = hex::encode(raw);
        Ok(self
            .pool
            .call_all(None, |endpoint| {
                let body = body.clone();
                async move {
                    self.verify(&endpoint).await?;
                    let (status, reply) =
                        http::exchange(self.http.post(url(&endpoint, "tx")).body(body)).await?;
                    let text = to_plain_text(&String::from_utf8_lossy(&reply), MAX_REFUSAL);
                    match status {
                        status if status.is_success() && text == id.as_str() => Ok(id.clone()),
                        status if status.is_success() => Err(Failure::Retry(ChainError::Network(
                            "a server named another transaction".to_owned(),
                        ))),
                        StatusCode::BAD_REQUEST if already(&text) => Ok(id.clone()),
                        StatusCode::BAD_REQUEST => Err(Failure::Fatal(ChainError::Rejected(text))),
                        status => Err(Failure::Retry(ChainError::Network(format!(
                            "HTTP {status}"
                        )))),
                    }
                }
            })
            .await?)
    }

    async fn history(
        &self,
        endpoint: Endpoint,
        script: &[u8],
    ) -> Result<Vec<Seen>, Failure<ChainError>> {
        let hash = scripthash(script);
        let mut found = Vec::new();
        let mut path = format!("scripthash/{hash}/txs");
        for _ in 0..MAX_PAGES {
            let page = entries(&self.json(&endpoint, &path).await?, seen)?;
            let confirmed: Vec<&Seen> = page.iter().filter(|seen| seen.height > 0).collect();
            let last = confirmed.last().map(|seen| seen.txid);
            let full = confirmed.len() >= PAGE;
            found.extend(page);
            match last {
                Some(last) if full => {
                    path = format!("scripthash/{hash}/txs/chain/{}", hex::encode(&last));
                }
                _ => break,
            }
        }
        Ok(found)
    }

    async fn coins(
        &self,
        endpoint: Endpoint,
        script: &[u8],
    ) -> Result<Vec<Unspent>, Failure<ChainError>> {
        let path = format!("scripthash/{}/utxo", scripthash(script));
        entries(&self.json(&endpoint, &path).await?, unspent)
    }

    async fn raw(
        &self,
        endpoint: Endpoint,
        id: &Hash,
    ) -> Result<Option<Vec<u8>>, Failure<ChainError>> {
        let path = format!("tx/{}/hex", hex::encode(id));
        match self.get(&endpoint, &path).await? {
            Some(body) => decode(&body).map(Some),
            None => Ok(None),
        }
    }

    async fn proof(
        &self,
        endpoint: Endpoint,
        item: &(Hash, u64),
    ) -> Result<Branch, Failure<ChainError>> {
        let (id, height) = item;
        let path = format!("tx/{}/merkle-proof", hex::encode(id));
        branch(&self.json(&endpoint, &path).await?, *height).ok_or_else(malformed)
    }

    async fn header(
        &self,
        endpoint: Endpoint,
        height: &u64,
    ) -> Result<Vec<u8>, Failure<ChainError>> {
        let hash = self
            .get(&endpoint, &format!("block-height/{height}"))
            .await?
            .and_then(|body| hex::decode_array::<32>(String::from_utf8_lossy(&body).trim()))
            .ok_or_else(malformed)?;
        let header = self
            .get(&endpoint, &format!("block/{}/header", hex::encode(&hash)))
            .await?
            .ok_or_else(malformed)
            .and_then(|body| decode(&body))?;
        if block_hash(&header) != Some(hash) {
            return Err(Failure::Retry(ChainError::Network(
                "a server sent a header that is not that block".to_owned(),
            )));
        }
        Ok(header)
    }

    async fn each<'a, I: Sync, T, F, Fut>(
        &'a self,
        items: &'a [I],
        op: F,
    ) -> Result<Vec<T>, ChainError>
    where
        F: Fn(&'a Self, Endpoint, &'a I) -> Fut,
        Fut: Future<Output = Result<T, Failure<ChainError>>>,
    {
        let op = &op;
        let pending: Vec<_> = items
            .iter()
            .map(|item| {
                self.pool.call(None, move |endpoint| async move {
                    self.verify(&endpoint).await?;
                    op(self, endpoint, item).await
                })
            })
            .collect();
        Ok(stream::iter(pending)
            .buffered(PARALLEL)
            .try_collect()
            .await?)
    }

    async fn verify(&self, endpoint: &Endpoint) -> Result<(), Failure<ChainError>> {
        let key = endpoint.url.to_string();
        if self.verified().contains(&key) {
            return Ok(());
        }
        let genesis = self
            .get(endpoint, "block-height/0")
            .await?
            .unwrap_or_default();
        if String::from_utf8_lossy(&genesis).trim() != self.genesis {
            return Err(Failure::Unreachable(ChainError::Network(format!(
                "{} serves another network",
                endpoint.url.host_str().unwrap_or_default()
            ))));
        }
        self.verified().insert(key);
        Ok(())
    }

    fn verified(&self) -> MutexGuard<'_, BTreeSet<String>> {
        self.verified.lock().unwrap_or_else(PoisonError::into_inner)
    }

    async fn json(&self, endpoint: &Endpoint, path: &str) -> Result<Value, Failure<ChainError>> {
        let body = self.get(endpoint, path).await?.ok_or_else(malformed)?;
        serde_json::from_slice(&body).map_err(|_| malformed())
    }

    async fn get(
        &self,
        endpoint: &Endpoint,
        path: &str,
    ) -> Result<Option<Vec<u8>>, Failure<ChainError>> {
        let (status, body) = http::exchange(self.http.get(url(endpoint, path))).await?;
        match status {
            status if status.is_success() => Ok(Some(body)),
            StatusCode::NOT_FOUND => Ok(None),
            status => Err(Failure::Retry(ChainError::Network(format!(
                "HTTP {status}"
            )))),
        }
    }
}

fn url(endpoint: &Endpoint, path: &str) -> String {
    format!("{}/{path}", endpoint.url.as_str().trim_end_matches('/'))
}

fn scripthash(script: &[u8]) -> String {
    hex::encode(&Sha256::digest(script))
}

fn malformed() -> Failure<ChainError> {
    Failure::Retry(ChainError::malformed())
}

fn decode(body: &[u8]) -> Result<Vec<u8>, Failure<ChainError>> {
    hex::decode(String::from_utf8_lossy(body).trim()).ok_or_else(malformed)
}

fn entries<T>(list: &Value, read: fn(&Value) -> Option<T>) -> Result<Vec<T>, Failure<ChainError>> {
    list.as_array()
        .ok_or_else(malformed)?
        .iter()
        .map(|entry| read(entry).ok_or_else(malformed))
        .collect()
}

fn seen(entry: &Value) -> Option<Seen> {
    Some(Seen {
        txid: hex::decode_array(entry["txid"].as_str()?)?,
        height: confirmed_height(&entry["status"])?,
    })
}

fn confirmed_height(status: &Value) -> Option<i64> {
    if status["confirmed"].as_bool()? {
        status["block_height"].as_i64().filter(|height| *height > 0)
    } else {
        Some(0)
    }
}

fn unspent(entry: &Value) -> Option<Unspent> {
    Some(Unspent {
        outpoint: Outpoint {
            txid: hex::decode_array(entry["txid"].as_str()?)?,
            vout: u32::try_from(entry["vout"].as_u64()?).ok()?,
        },
        value: entry["value"]
            .as_u64()
            .filter(|value| *value <= MAX_MONEY)?,
        height: confirmed_height(&entry["status"])?,
    })
}

#[cfg(fuzzing)]
pub(crate) fn fuzz(value: &Value) {
    let _ = entries(value, seen);
    let _ = entries(value, unspent);
}
