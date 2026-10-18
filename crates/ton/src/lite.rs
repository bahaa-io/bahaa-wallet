use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use adnl::AdnlPeer;
use bahaa_chain::{ChainError, Failure, Pool, PoolError};
use bahaa_config::{Endpoint, Protocol};
use bahaa_types::Address;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use ruint::aliases::U256;
use serde_json::Value;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::time::timeout;
use tonlib_core::cell::BagOfCells;
use url::Url;

use crate::cells::{Account, account};
use crate::fees::{Config, PARAMS};
use crate::proof::{self, Proven};
use crate::tl::{self, AccountState, Answer, BlockId, ConfigInfo};
use crate::transaction::{self, MAX_LISTED, Transaction};

const CONNECT: Duration = Duration::from_secs(5);
const ANSWER: Duration = Duration::from_secs(10);
const MAX_CONFIG: usize = 1024 * 1024;
const MAX_SERVERS: usize = 64;
const MAX_FRAMES: usize = 4;
const NOT_APPLIED: &str = "cannot apply external message";
const EXIT_CODE: &str = "exitcode=";

type Session = Arc<Mutex<Option<AdnlPeer<TcpStream>>>>;

struct Servers {
    pool: Pool,
    keys: HashMap<String, [u8; 32]>,
    sessions: HashMap<String, Session>,
}

pub(crate) struct Lite {
    client: reqwest::Client,
    zero_state: [u8; 32],
    servers: Mutex<HashMap<String, Arc<Servers>>>,
}

impl std::fmt::Debug for Lite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Lite")
    }
}

impl Lite {
    pub(crate) fn new(client: reqwest::Client, zero_state: [u8; 32]) -> Self {
        Self {
            client,
            zero_state,
            servers: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) async fn balance(
        &self,
        config: &Url,
        owner: &Address,
    ) -> Result<U256, Failure<ChainError>> {
        Ok(self.account(config, owner).await?.balance)
    }

    pub(crate) async fn account(
        &self,
        config: &Url,
        owner: &Address,
    ) -> Result<Account, Failure<ChainError>> {
        let (workchain, hash) = parts(owner)?;
        self.ask(
            config,
            |last| tl::get_account_state(last, workchain, &hash),
            tl::account_state,
            |last, state| proven_account(last, &state, owner),
        )
        .await
    }

    pub(crate) async fn config(&self, config: &Url) -> Result<Config, Failure<ChainError>> {
        self.ask(
            config,
            |last| tl::get_config_params(last, &PARAMS),
            tl::config_info,
            |last, info| proven_config(last, &info),
        )
        .await
    }

    pub(crate) async fn library(
        &self,
        config: &Url,
        hash: &[u8; 32],
    ) -> Result<Vec<u8>, Failure<ChainError>> {
        self.ask(
            config,
            |_| tl::get_libraries(&[*hash]),
            tl::library_result,
            |_, entries| {
                entries
                    .into_iter()
                    .find(|(listed, _)| listed == hash)
                    .map(|(_, data)| data)
                    .ok_or_else(|| retry("a liteserver does not have a library"))
                    .and_then(|data| proven_library(data, hash))
            },
        )
        .await
    }

    pub(crate) async fn send(&self, config: &Url, boc: &[u8]) -> Result<(), Failure<ChainError>> {
        self.ask(
            config,
            |_| tl::send_message(boc),
            send_answer,
            |_, outcome| sent(outcome),
        )
        .await
    }

    pub(crate) async fn transactions(
        &self,
        config: &Url,
        owner: &Address,
        from: ([u8; 32], u64),
    ) -> Result<Vec<Transaction>, Failure<ChainError>> {
        let (workchain, hash) = parts(owner)?;
        self.ask(
            config,
            |_| tl::get_transactions(MAX_LISTED, workchain, &hash, from.1, &from.0),
            tl::transaction_list,
            |_, list| proven_transactions(&list.transactions, owner, from),
        )
        .await
    }

    async fn ask<A, T>(
        &self,
        config: &Url,
        function: impl Fn(&BlockId) -> Vec<u8>,
        decode: fn(&[u8]) -> Option<Answer<A>>,
        finish: impl Fn(&BlockId, A) -> Result<T, Failure<ChainError>>,
    ) -> Result<T, Failure<ChainError>> {
        let servers = self.servers(config).await?;
        let (function, finish) = (&function, &finish);
        let result = servers
            .pool
            .call(None, |endpoint| {
                let servers = Arc::clone(&servers);
                async move {
                    let session = servers.session(&endpoint)?;
                    let mut peer = session.lock().await;
                    let outcome = async {
                        let last = self.connect(&mut peer, &servers, &endpoint).await?;
                        let answer = exchange(open(&mut peer)?, &function(&last), decode).await?;
                        finish(&last, answer)
                    }
                    .await;
                    if matches!(outcome, Err(Failure::Retry(_))) {
                        *peer = None;
                    }
                    outcome
                }
            })
            .await;
        result.map_err(outer)
    }

    async fn connect(
        &self,
        peer: &mut Option<AdnlPeer<TcpStream>>,
        servers: &Servers,
        endpoint: &Endpoint,
    ) -> Result<BlockId, Failure<ChainError>> {
        if peer.is_none() {
            let key = servers
                .keys
                .get(endpoint.url.as_str())
                .ok_or_else(|| retry("a liteserver has no key"))?;
            let address = endpoint
                .url
                .host_str()
                .zip(endpoint.url.port())
                .ok_or_else(|| retry("a liteserver has no address"))?;
            let connected = timeout(CONNECT, AdnlPeer::connect(key, address))
                .await
                .map_err(|_| retry("a liteserver did not answer in time"))?
                .map_err(|_| retry("a liteserver refused the connection"))?;
            *peer = Some(connected);
        }
        let open = peer
            .as_mut()
            .ok_or_else(|| retry("a liteserver connection closed"))?;
        let info = exchange(open, &tl::get_masterchain_info(), tl::masterchain_info).await?;
        if info.zero_state != self.zero_state {
            return Err(retry("a liteserver serves another TON network"));
        }
        Ok(info.last)
    }

    async fn servers(&self, config: &Url) -> Result<Arc<Servers>, Failure<ChainError>> {
        let mut known = self.servers.lock().await;
        if let Some(servers) = known.get(config.as_str()) {
            return Ok(Arc::clone(servers));
        }
        let mut response = self
            .client
            .get(config.clone())
            .send()
            .await
            .map_err(|err| retry(&err.without_url().to_string()))?;
        if !response.status().is_success() {
            return Err(retry("the liteserver list is unavailable"));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|err| retry(&err.without_url().to_string()))?
        {
            if bytes.len() + chunk.len() > MAX_CONFIG {
                return Err(retry("the liteserver list is too large"));
            }
            bytes.extend_from_slice(&chunk);
        }
        let list: Value =
            serde_json::from_slice(&bytes).map_err(|_| retry("the liteserver list is not JSON"))?;
        let servers = Arc::new(self.read_list(&list)?);
        known.insert(config.to_string(), Arc::clone(&servers));
        Ok(servers)
    }

    fn read_list(&self, list: &Value) -> Result<Servers, Failure<ChainError>> {
        let zero_state = list["validator"]["zero_state"]["root_hash"]
            .as_str()
            .and_then(|text| STANDARD.decode(text).ok());
        if zero_state.as_deref() != Some(self.zero_state.as_slice()) {
            return Err(retry("the liteserver list is for another TON network"));
        }
        let entries = list["liteservers"]
            .as_array()
            .ok_or_else(|| retry("the liteserver list has no servers"))?;
        let mut endpoints = Vec::new();
        let mut keys = HashMap::new();
        let mut sessions = HashMap::new();
        for entry in entries.iter().take(MAX_SERVERS) {
            let Some((url, key)) = server(entry) else {
                continue;
            };
            keys.insert(url.to_string(), key);
            sessions.insert(url.to_string(), Session::default());
            endpoints.push(Endpoint {
                protocol: Protocol::TonLiteservers,
                url,
                features: Vec::new(),
                min_gap_ms: 0,
            });
        }
        if endpoints.is_empty() {
            return Err(retry("the liteserver list has no usable servers"));
        }
        let turn = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.subsec_nanos());
        let len = u32::try_from(endpoints.len()).unwrap_or(1);
        endpoints.rotate_left(usize::try_from(turn % len).unwrap_or(0));
        Ok(Servers {
            pool: Pool::new(endpoints),
            keys,
            sessions,
        })
    }
}

impl Servers {
    fn session(&self, endpoint: &Endpoint) -> Result<Session, Failure<ChainError>> {
        self.sessions
            .get(endpoint.url.as_str())
            .cloned()
            .ok_or_else(|| retry("a liteserver has no session"))
    }
}

fn server(entry: &Value) -> Option<(Url, [u8; 32])> {
    let ip = i32::try_from(entry["ip"].as_i64()?).ok()?;
    let port = u16::try_from(entry["port"].as_u64()?).ok()?;
    if entry["id"]["@type"] != "pub.ed25519" {
        return None;
    }
    let key: [u8; 32] = STANDARD
        .decode(entry["id"]["key"].as_str()?)
        .ok()?
        .try_into()
        .ok()?;
    let address = Ipv4Addr::from(u32::from_be_bytes(ip.to_be_bytes()));
    Some((Url::parse(&format!("adnl://{address}:{port}")).ok()?, key))
}

async fn exchange<T>(
    peer: &mut AdnlPeer<TcpStream>,
    function: &[u8],
    read: fn(&[u8]) -> Option<Answer<T>>,
) -> Result<T, Failure<ChainError>> {
    let mut id = [0u8; 32];
    getrandom::fill(&mut id).map_err(|_| retry("no randomness for a query id"))?;
    timeout(ANSWER, peer.send(Bytes::from(tl::query(&id, function))))
        .await
        .map_err(|_| retry("a liteserver did not take the query in time"))?
        .map_err(|_| retry("a liteserver connection broke"))?;
    for _ in 0..MAX_FRAMES {
        let frame = timeout(ANSWER, peer.next())
            .await
            .map_err(|_| retry("a liteserver did not answer in time"))?
            .ok_or_else(|| retry("a liteserver closed the connection"))?
            .map_err(|_| retry("a liteserver sent a broken frame"))?;
        let Some(answer) = tl::answer(&id, &frame) else {
            continue;
        };
        return match read(answer) {
            Some(Answer::Ok(value)) => Ok(value),
            Some(Answer::Refused { code, message }) => Err(retry(&format!(
                "a liteserver refused with code {code}: {message}"
            ))),
            None => Err(retry("a liteserver sent an unreadable answer")),
        };
    }
    Err(retry("a liteserver kept answering something else"))
}

pub(crate) fn proven_account(
    last: &BlockId,
    state: &AccountState,
    owner: &Address,
) -> Result<Account, Failure<ChainError>> {
    let (workchain, hash) = parts(owner)?;
    if state.block != *last {
        return Err(retry("a liteserver answered for another block"));
    }
    let proven = proof::account(
        last,
        &state.shard_block,
        &state.shard_proof,
        &state.proof,
        workchain,
        &hash,
    )
    .ok_or_else(|| retry("a liteserver sent an account proof that does not hold"))?;
    let root = (!state.state.is_empty())
        .then(|| BagOfCells::parse(&state.state).ok()?.single_root().ok())
        .flatten();
    let last = match (proven, &root) {
        (Proven::Present { account, last }, Some(root))
            if root.cell_hash().as_slice() == account =>
        {
            Some(last)
        }
        (Proven::Absent, None) => None,
        _ => {
            return Err(retry(
                "a liteserver sent an account that its proof does not cover",
            ));
        }
    };
    let mut read = account(&state.state, owner)
        .ok_or_else(|| retry("a liteserver sent an unreadable account"))?;
    read.last = last;
    Ok(read)
}

pub(crate) fn proven_transactions(
    boc: &[u8],
    owner: &Address,
    from: ([u8; 32], u64),
) -> Result<Vec<Transaction>, Failure<ChainError>> {
    let (_, hash) = parts(owner)?;
    transaction::linked(boc, &hash, from)
        .ok_or_else(|| retry("a server sent transactions that do not continue the proven history"))
}

fn send_answer(answer: &[u8]) -> Option<Answer<Result<bool, (i32, String)>>> {
    Some(match tl::send_status(answer)? {
        Answer::Ok(status) => Answer::Ok(Ok(status)),
        Answer::Refused { code, message } => Answer::Ok(Err((code, message))),
    })
}

fn sent(outcome: Result<bool, (i32, String)>) -> Result<(), Failure<ChainError>> {
    match outcome {
        Ok(true) => Ok(()),
        Ok(false) => Err(retry("a liteserver did not take the message")),
        Err((code, message)) => Err(refused(&message).map_or_else(
            || retry(&format!("a liteserver refused with code {code}: {message}")),
            Failure::Fatal,
        )),
    }
}

pub(crate) fn refused(error: &str) -> Option<ChainError> {
    if !error.contains(NOT_APPLIED) {
        return None;
    }
    let code = error
        .split_once(EXIT_CODE)
        .map(|(_, rest)| {
            rest.bytes()
                .take_while(|byte| byte.is_ascii_digit() || *byte == b'-')
                .map(char::from)
                .collect::<String>()
        })
        .filter(|code| !code.is_empty() && code.len() <= 11);
    Some(ChainError::Rejected(match code {
        Some(code) => format!("the wallet contract refused the message with exit code {code}"),
        None => "the network refused the message".to_owned(),
    }))
}

pub(crate) fn proven_library(
    data: Vec<u8>,
    hash: &[u8; 32],
) -> Result<Vec<u8>, Failure<ChainError>> {
    if crate::vm::library_hash(&data).as_ref() == Some(hash) {
        Ok(data)
    } else {
        Err(retry(
            "a server sent a library that does not match its hash",
        ))
    }
}

pub(crate) fn proven_config(
    last: &BlockId,
    info: &ConfigInfo,
) -> Result<Config, Failure<ChainError>> {
    if info.block != *last {
        return Err(retry("a liteserver answered for another block"));
    }
    proof::config(last, &info.state_proof, &info.config_proof, &PARAMS)
        .map(Config::new)
        .ok_or_else(|| retry("a liteserver sent a config proof that does not hold"))
}

fn open(
    peer: &mut Option<AdnlPeer<TcpStream>>,
) -> Result<&mut AdnlPeer<TcpStream>, Failure<ChainError>> {
    peer.as_mut()
        .ok_or_else(|| retry("a liteserver connection closed"))
}

fn parts(address: &Address) -> Result<(i8, [u8; 32]), Failure<ChainError>> {
    match address {
        Address::Ton { workchain, hash } => Ok((*workchain, *hash)),
        _ => Err(Failure::Fatal(ChainError::Invalid("not a TON address"))),
    }
}

fn outer(err: PoolError<ChainError>) -> Failure<ChainError> {
    match err {
        PoolError::Fatal(err) => Failure::Fatal(err),
        PoolError::Exhausted(err) => Failure::Retry(err),
        PoolError::Unavailable(_) | PoolError::NoEndpoint => {
            retry("no liteserver is available right now")
        }
    }
}

fn retry(text: &str) -> Failure<ChainError> {
    Failure::Retry(ChainError::Network(text.to_owned()))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn lite() -> Lite {
        Lite::new(reqwest::Client::new(), [0x17; 32])
    }

    #[test]
    fn reads_the_official_server_list() {
        let list = json!({
            "liteservers": [
                {"ip": -2_092_515_258, "port": 46_427, "id": {"@type": "pub.ed25519", "key": STANDARD.encode([5; 32])}},
                {"ip": 1, "port": 70_000, "id": {"@type": "pub.ed25519", "key": STANDARD.encode([5; 32])}},
                {"ip": 1, "port": 1, "id": {"@type": "pub.other", "key": STANDARD.encode([5; 32])}},
                {"ip": 1, "port": 1, "id": {"@type": "pub.ed25519", "key": STANDARD.encode([5; 31])}}
            ],
            "validator": {"zero_state": {"root_hash": STANDARD.encode([0x17; 32])}}
        });
        let servers = lite().read_list(&list).unwrap();
        assert_eq!(
            servers.keys.keys().collect::<Vec<_>>(),
            ["adnl://131.70.192.70:46427"]
        );
        let mut foreign = list.clone();
        foreign["validator"]["zero_state"]["root_hash"] = json!(STANDARD.encode([0x82; 32]));
        assert!(matches!(lite().read_list(&foreign), Err(Failure::Retry(_))));
        let mut empty = list;
        empty["liteservers"] = json!([]);
        assert!(matches!(lite().read_list(&empty), Err(Failure::Retry(_))));
    }

    #[test]
    fn only_message_refusals_are_final_and_they_name_the_exit_code() {
        let named = |code: &str| {
            Some(ChainError::Rejected(format!(
                "the wallet contract refused the message with exit code {code}"
            )))
        };
        assert_eq!(
            refused(
                "cannot apply external message to current state : External message was not accepted\nexitcode=133, steps=49"
            ),
            named("133")
        );
        assert_eq!(
            refused(
                "lite server error, code -701: cannot apply external message to current state : exitcode=-14, steps=3"
            ),
            named("-14")
        );
        let unnamed = Some(ChainError::Rejected(
            "the network refused the message".to_owned(),
        ));
        assert_eq!(
            refused(
                "cannot apply external message to current state : cannot deserialize bag-of-cells"
            ),
            unnamed
        );
        assert_eq!(
            refused("cannot apply external message exitcode=123456789012345"),
            unnamed
        );
        assert_eq!(refused("LITE_SERVER_NOTREADY: timeout"), None);
        assert_eq!(refused("state not ready"), None);
    }

    #[test]
    fn a_library_must_hash_to_what_was_asked() {
        let cell = tonlib_core::cell::CellBuilder::new().build().unwrap();
        let boc = BagOfCells::from_root(cell).serialize(false).unwrap();
        let hash = crate::vm::library_hash(&boc).unwrap();
        assert_eq!(proven_library(boc.clone(), &hash), Ok(boc.clone()));
        assert!(matches!(
            proven_library(boc, &[0; 32]),
            Err(Failure::Retry(ChainError::Network(_)))
        ));
    }
}
