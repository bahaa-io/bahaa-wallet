use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use bahaa_chain::{ChainError, Failure, Pool, TxId};
use bahaa_config::Endpoint;
use bahaa_types::{hex, to_plain_text};
use rustls::ClientConfig;
use rustls::pki_types::ServerName;
use rustls_platform_verifier::ConfigVerifierExt;
use serde::Deserialize;
use serde_json::value::RawValue;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::TlsConnector;

use crate::source::{Seen, Unspent, already, rate};
use crate::tx::{Hash, MAX_MONEY, Outpoint};

const MAX_REPLY: u64 = 4 * 1024 * 1024;
const MAX_REFUSAL: usize = 200;
const MAX_BATCH: usize = 20;
const CONNECT: Duration = Duration::from_secs(5);
const ANSWER: Duration = Duration::from_secs(20);
const CLIENT: &str = concat!("bahaa-wallet/", env!("CARGO_PKG_VERSION"));
const PROTOCOL: &str = "1.4";
const FEE_TARGET: u32 = 2;
const SATS_PER_COIN: u64 = 100_000_000;

type Call = (&'static str, Value);

#[derive(Clone, Debug, PartialEq, Eq)]
struct Refusal {
    method: &'static str,
    message: String,
}

impl Refusal {
    fn failure(&self) -> Failure<ChainError> {
        Failure::Retry(ChainError::Network(format!(
            "an Electrum server refused {}: {}",
            self.method, self.message
        )))
    }
}

type Answer = Result<Box<RawValue>, Refusal>;

trait Duplex: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T: AsyncRead + AsyncWrite + Unpin + Send> Duplex for T {}

struct Link {
    stream: BufReader<Box<dyn Duplex>>,
    next: u64,
}

type Slot = Arc<tokio::sync::Mutex<Option<Link>>>;

pub(crate) struct Electrum {
    pool: Pool,
    genesis: String,
    tls: TlsConnector,
    links: Mutex<HashMap<String, Slot>>,
}

impl std::fmt::Debug for Electrum {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Electrum")
            .field("pool", &self.pool)
            .field("genesis", &self.genesis)
            .finish_non_exhaustive()
    }
}

impl Electrum {
    pub(crate) fn new(endpoints: Vec<Endpoint>, genesis: &[u8; 32]) -> Result<Self, ChainError> {
        let tls = ClientConfig::with_platform_verifier()
            .map_err(|err| ChainError::Network(err.to_string()))?;
        Ok(Self {
            pool: Pool::new(endpoints),
            genesis: hex::encode(genesis),
            tls: TlsConnector::from(Arc::new(tls)),
            links: Mutex::new(HashMap::new()),
        })
    }

    pub(crate) async fn histories(
        &self,
        scripts: &[Vec<u8>],
    ) -> Result<Vec<Vec<Seen>>, ChainError> {
        let lists = self
            .batch(&each("blockchain.scripthash.get_history", scripts), results)
            .await?;
        lists.iter().map(|list| entries(list, seen)).collect()
    }

    pub(crate) async fn unspent(
        &self,
        scripts: &[Vec<u8>],
    ) -> Result<Vec<Vec<Unspent>>, ChainError> {
        let lists = self
            .batch(&each("blockchain.scripthash.listunspent", scripts), results)
            .await?;
        lists.iter().map(|list| entries(list, unspent)).collect()
    }

    pub(crate) async fn transactions(
        &self,
        ids: &[Hash],
    ) -> Result<Vec<Option<Vec<u8>>>, ChainError> {
        let calls: Vec<Call> = ids
            .iter()
            .map(|id| ("blockchain.transaction.get", json!([hex::encode(id)])))
            .collect();
        let raws = self
            .batch(&calls, |answers| {
                answers
                    .into_iter()
                    .map(|answer| match answer {
                        Ok(raw) => text(&raw).map(Some).ok_or_else(malformed),
                        Err(refusal) if missing(&refusal.message) => Ok(None),
                        Err(refusal) => Err(refusal.failure()),
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .await?;
        raws.into_iter()
            .map(|raw| {
                raw.map(|raw| hex::decode(&raw).ok_or_else(ChainError::malformed))
                    .transpose()
            })
            .collect()
    }

    pub(crate) async fn headers(&self, heights: &[u64]) -> Result<Vec<Vec<u8>>, ChainError> {
        let calls: Vec<Call> = heights
            .iter()
            .map(|height| ("blockchain.block.header", json!([height])))
            .collect();
        let headers = self.batch(&calls, results).await?;
        headers
            .iter()
            .map(|header| {
                header
                    .as_str()
                    .and_then(hex::decode)
                    .ok_or_else(ChainError::malformed)
            })
            .collect()
    }

    pub(crate) async fn rates(&self) -> Result<Vec<u64>, ChainError> {
        let calls = [
            ("blockchain.estimatefee", json!([FEE_TARGET])),
            ("blockchain.relayfee", json!([])),
        ];
        self.batch(&calls, |answers| {
            answers
                .into_iter()
                .filter_map(|answer| match answer {
                    Ok(raw) => rate(raw.get(), SATS_PER_COIN).map(Ok),
                    Err(refusal) => Some(Err(refusal.failure())),
                })
                .collect()
        })
        .await
    }

    pub(crate) async fn broadcast(&self, raw: &[u8], id: &TxId) -> Result<TxId, ChainError> {
        let call = [(
            "blockchain.transaction.broadcast",
            json!([hex::encode(raw)]),
        )];
        self.batch_all(&call, |answers| match answers.into_iter().next() {
            Some(Ok(raw)) if text(&raw).as_deref() == Some(id.as_str()) => Ok(id.clone()),
            Some(Ok(_)) => Err(Failure::Retry(ChainError::Network(
                "an Electrum server named another transaction".to_owned(),
            ))),
            Some(Err(refusal)) if already(&refusal.message) => Ok(id.clone()),
            Some(Err(refusal)) => Err(Failure::Fatal(ChainError::Rejected(refusal.message))),
            None => Err(malformed()),
        })
        .await
    }

    async fn batch<T>(
        &self,
        calls: &[Call],
        judge: impl Fn(Vec<Answer>) -> Result<T, Failure<ChainError>>,
    ) -> Result<T, ChainError> {
        let judge = &judge;
        Ok(self
            .pool
            .call(None, |endpoint| async move {
                judge(self.exchange(&endpoint, calls).await?)
            })
            .await?)
    }

    async fn batch_all<T>(
        &self,
        calls: &[Call],
        judge: impl Fn(Vec<Answer>) -> Result<T, Failure<ChainError>>,
    ) -> Result<T, ChainError> {
        let judge = &judge;
        Ok(self
            .pool
            .call_all(None, |endpoint| async move {
                judge(self.exchange(&endpoint, calls).await?)
            })
            .await?)
    }

    async fn exchange(
        &self,
        endpoint: &Endpoint,
        calls: &[Call],
    ) -> Result<Vec<Answer>, Failure<ChainError>> {
        let slot = Arc::clone(
            self.links
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(endpoint.url.to_string())
                .or_default(),
        );
        let mut held = slot.lock().await;
        if let Some(mut link) = held.take()
            && let Ok(answers) = link.exchange(calls).await
        {
            *held = Some(link);
            return Ok(answers);
        }
        let mut link = self.open(endpoint).await?;
        let answers = link.exchange(calls).await?;
        *held = Some(link);
        Ok(answers)
    }

    async fn open(&self, endpoint: &Endpoint) -> Result<Link, Failure<ChainError>> {
        let unreachable = |text: String| Failure::Unreachable(ChainError::Network(text));
        let url = &endpoint.url;
        let (Some(host), Some(port)) = (url.host_str(), url.port()) else {
            return Err(Failure::Fatal(ChainError::Invalid(
                "an Electrum endpoint needs a host and a port",
            )));
        };
        let tcp = timeout(CONNECT, TcpStream::connect((host, port)))
            .await
            .map_err(|_| unreachable(format!("{host} did not answer")))?
            .map_err(|err| unreachable(format!("{host}: {err}")))?;
        let stream: Box<dyn Duplex> = match url.scheme() {
            "ssl" => {
                let name = ServerName::try_from(host.to_owned())
                    .map_err(|err| unreachable(err.to_string()))?;
                Box::new(
                    timeout(CONNECT, self.tls.connect(name, tcp))
                        .await
                        .map_err(|_| unreachable(format!("{host} did not finish TLS")))?
                        .map_err(|err| unreachable(format!("{host}: {err}")))?,
                )
            }
            "tcp" => Box::new(tcp),
            _ => {
                return Err(Failure::Fatal(ChainError::Invalid(
                    "an Electrum endpoint uses ssl://",
                )));
            }
        };
        let mut link = Link {
            stream: BufReader::new(stream),
            next: 0,
        };
        if link
            .ask("server.version", json!([CLIENT, PROTOCOL]))
            .await?
            .is_none()
        {
            return Err(unreachable(format!("{host} does not speak protocol 1.4")));
        }
        let features = link.ask("server.features", json!([])).await?;
        if features
            .as_ref()
            .and_then(|features| features["genesis_hash"].as_str())
            != Some(self.genesis.as_str())
        {
            return Err(unreachable(format!("{host} serves another network")));
        }
        Ok(link)
    }
}

impl Link {
    async fn ask(
        &mut self,
        method: &'static str,
        params: Value,
    ) -> Result<Option<Value>, Failure<ChainError>> {
        let answers = self.exchange(&[(method, params)]).await?;
        Ok(answers
            .into_iter()
            .next()
            .and_then(Result::ok)
            .and_then(|raw| serde_json::from_str(raw.get()).ok()))
    }

    async fn exchange(&mut self, calls: &[Call]) -> Result<Vec<Answer>, Failure<ChainError>> {
        let mut answers = Vec::with_capacity(calls.len());
        for part in calls.chunks(MAX_BATCH) {
            let replies = timeout(ANSWER, self.batch(part))
                .await
                .map_err(|_| lost("no answer in time"))??;
            answers.extend(replies);
        }
        Ok(answers)
    }

    async fn batch(&mut self, calls: &[Call]) -> Result<Vec<Answer>, Failure<ChainError>> {
        let first = self.next;
        let count = calls.len() as u64;
        self.next = first
            .checked_add(count)
            .ok_or_else(|| lost("too many requests"))?;
        let requests: Vec<Value> = calls
            .iter()
            .zip(first..)
            .map(|((method, params), id)| {
                json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
            })
            .collect();
        let mut line = match requests.as_slice() {
            [single] => serde_json::to_vec(single),
            many => serde_json::to_vec(many),
        }
        .map_err(|err| lost(&err.to_string()))?;
        line.push(b'\n');
        let stream = self.stream.get_mut();
        stream
            .write_all(&line)
            .await
            .map_err(|err| lost(&err.to_string()))?;
        stream.flush().await.map_err(|err| lost(&err.to_string()))?;
        let replies = loop {
            if let Some(replies) = self.replies().await? {
                break replies;
            }
        };
        let mut answers: Vec<Option<Answer>> = vec![None; calls.len()];
        for reply in replies {
            let slot = reply
                .id
                .and_then(|id| id.checked_sub(first))
                .and_then(|at| usize::try_from(at).ok())
                .filter(|at| *at < calls.len() && answers[*at].is_none())
                .ok_or_else(|| lost("an answer does not match a request"))?;
            let method = calls[slot].0;
            answers[slot] = Some(match (reply.error, reply.result) {
                (Some(error), _) => Err(Refusal {
                    method,
                    message: to_plain_text(
                        error["message"].as_str().unwrap_or("no reason given"),
                        MAX_REFUSAL,
                    ),
                }),
                (None, Some(result)) => Ok(result),
                (None, None) => return Err(lost("an answer has no result")),
            });
        }
        answers
            .into_iter()
            .collect::<Option<Vec<Answer>>>()
            .ok_or_else(|| lost("an answer is missing"))
    }

    async fn replies(&mut self) -> Result<Option<Vec<Reply>>, Failure<ChainError>> {
        let mut buffer = Vec::new();
        let read = (&mut self.stream)
            .take(MAX_REPLY)
            .read_until(b'\n', &mut buffer)
            .await
            .map_err(|err| lost(&err.to_string()))?;
        if read == 0 {
            return Err(lost("the connection closed"));
        }
        if buffer.last() != Some(&b'\n') {
            return Err(lost("reply too large"));
        }
        let unreadable = |_| lost("reply is not JSON");
        let replies = if buffer.trim_ascii_start().starts_with(b"[") {
            serde_json::from_slice::<Vec<Reply>>(&buffer).map_err(unreadable)?
        } else {
            let reply: Reply = serde_json::from_slice(&buffer).map_err(unreadable)?;
            if reply.id.is_none() && reply.method.is_some() {
                return Ok(None);
            }
            vec![reply]
        };
        Ok(Some(replies))
    }
}

#[derive(Deserialize)]
struct Reply {
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    result: Option<Box<RawValue>>,
    #[serde(default)]
    error: Option<Value>,
}

fn lost(text: &str) -> Failure<ChainError> {
    Failure::Retry(ChainError::Network(text.to_owned()))
}

fn malformed() -> Failure<ChainError> {
    Failure::Retry(ChainError::malformed())
}

fn each(method: &'static str, scripts: &[Vec<u8>]) -> Vec<Call> {
    scripts
        .iter()
        .map(|script| (method, json!([scripthash(script)])))
        .collect()
}

fn scripthash(script: &[u8]) -> String {
    let mut hash: [u8; 32] = Sha256::digest(script).into();
    hash.reverse();
    hex::encode(&hash)
}

fn entries<T>(list: &Value, read: fn(&Value) -> Option<T>) -> Result<Vec<T>, ChainError> {
    list.as_array()
        .ok_or_else(ChainError::malformed)?
        .iter()
        .map(|entry| read(entry).ok_or_else(ChainError::malformed))
        .collect()
}

fn seen(entry: &Value) -> Option<Seen> {
    Some(Seen {
        txid: hex::decode_array(entry["tx_hash"].as_str()?)?,
        height: entry["height"].as_i64()?,
    })
}

fn unspent(entry: &Value) -> Option<Unspent> {
    Some(Unspent {
        outpoint: Outpoint {
            txid: hex::decode_array(entry["tx_hash"].as_str()?)?,
            vout: u32::try_from(entry["tx_pos"].as_u64()?).ok()?,
        },
        value: entry["value"]
            .as_u64()
            .filter(|value| *value <= MAX_MONEY)?,
        confirmed: entry["height"].as_i64()? > 0,
    })
}

fn missing(message: &str) -> bool {
    let message = message.to_lowercase();
    message.contains("missing transaction") || message.contains("no such mempool")
}

fn text(raw: &RawValue) -> Option<String> {
    serde_json::from_str(raw.get()).ok()
}

fn results(answers: Vec<Answer>) -> Result<Vec<Value>, Failure<ChainError>> {
    answers
        .into_iter()
        .map(|answer| {
            let raw = answer.map_err(|refusal| refusal.failure())?;
            serde_json::from_str(raw.get()).map_err(|_| lost("an answer is not JSON"))
        })
        .collect()
}

#[cfg(fuzzing)]
pub(crate) fn fuzz(value: &Value) {
    let _ = entries(value, seen);
    let _ = entries(value, unspent);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripthashes_are_reversed_sha256_of_the_script() {
        let genesis_payout =
            hex::decode("76a91462e907b15cbf27d5425399ebf6f0fb50ebb88f1888ac").unwrap();
        assert_eq!(
            scripthash(&genesis_payout),
            "8b01df4e368ea28f8dc0423bcf7a4923e3a12d307c875e47a0cfbf90b5c39161"
        );
    }
}
