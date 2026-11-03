use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use bahaa_chain::{ChainError, Failure, Pool};
use bahaa_config::{Endpoint, Feature};
use bahaa_types::{hex, to_plain_text};
use rustls::ClientConfig;
use rustls::pki_types::ServerName;
use rustls_platform_verifier::ConfigVerifierExt;
use serde::Deserialize;
use serde_json::value::RawValue;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::TlsConnector;

const MAX_REPLY: u64 = 4 * 1024 * 1024;
const MAX_REFUSAL: usize = 200;
const MAX_BATCH: usize = 20;
const CONNECT: Duration = Duration::from_secs(5);
const ANSWER: Duration = Duration::from_secs(20);
const CLIENT: &str = concat!("bahaa-wallet/", env!("CARGO_PKG_VERSION"));
const PROTOCOL: &str = "1.4";

pub(crate) type Call = (&'static str, Value);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Refusal {
    pub method: &'static str,
    pub message: String,
}

impl Refusal {
    pub(crate) fn failure(&self) -> Failure<ChainError> {
        Failure::Retry(ChainError::Network(format!(
            "an Electrum server refused {}: {}",
            self.method, self.message
        )))
    }
}

pub(crate) type Answer = Result<Box<RawValue>, Refusal>;

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

    pub(crate) async fn batch<T>(
        &self,
        need: Option<Feature>,
        calls: &[Call],
        judge: impl Fn(Vec<Answer>) -> Result<T, Failure<ChainError>>,
    ) -> Result<T, ChainError> {
        let judge = &judge;
        Ok(self
            .pool
            .call(need, |endpoint| async move {
                judge(self.exchange(&endpoint, calls).await?)
            })
            .await?)
    }

    pub(crate) async fn batch_all<T>(
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

pub(crate) fn results(answers: Vec<Answer>) -> Result<Vec<Value>, Failure<ChainError>> {
    answers
        .into_iter()
        .map(|answer| {
            let raw = answer.map_err(|refusal| refusal.failure())?;
            serde_json::from_str(raw.get()).map_err(|_| lost("an answer is not JSON"))
        })
        .collect()
}
