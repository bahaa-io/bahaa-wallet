use std::collections::{BTreeSet, HashMap};
use std::fmt::Write;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bahaa_chain::{ChainError, Failure, Pool};
use bahaa_config::{Endpoint, Protocol};
use bahaa_types::{Address, to_plain_text};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use reqwest::StatusCode;
use reqwest::header::RETRY_AFTER;
use ruint::aliases::U256;
use serde_json::{Value, json};
use url::Url;

use crate::cells::{Account, Status};
use crate::fees::Config;
use crate::lite::{
    Lite, proven_account, proven_config, proven_library, proven_transactions, refused,
};
use crate::tl::{AccountState, BlockId, ConfigInfo};
use crate::transaction::{MAX_LISTED, Transaction};
use crate::vm::{self, Ran};

const MAX_REPLY: usize = 4 * 1024 * 1024;
const MAX_MESSAGE: usize = 200;
const TIMEOUT: Duration = Duration::from_secs(20);
const CONFIG_FOR: Duration = Duration::from_secs(600);
const MAX_LIBRARIES: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Item {
    Num(U256),
    Cell(Vec<u8>),
}

#[derive(Clone, Copy)]
enum Provider {
    Center,
    Api,
}

type Reply = (StatusCode, Option<Value>);

#[derive(Debug)]
pub(crate) struct Http {
    pool: Pool,
    client: reqwest::Client,
    zero_state: [u8; 32],
    verified: Mutex<BTreeSet<String>>,
    config: Mutex<Option<(Instant, Config)>>,
    libraries: Mutex<HashMap<[u8; 32], Vec<u8>>>,
    lite: Lite,
}

impl Http {
    pub(crate) fn new(endpoints: Vec<Endpoint>, zero_state: [u8; 32]) -> Result<Self, ChainError> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("bahaa-wallet/", env!("CARGO_PKG_VERSION")))
            .timeout(TIMEOUT)
            .build()
            .map_err(|err| ChainError::Network(err.to_string()))?;
        Ok(Self {
            pool: Pool::new(endpoints),
            lite: Lite::new(client.clone(), zero_state),
            client,
            zero_state,
            verified: Mutex::new(BTreeSet::new()),
            config: Mutex::new(None),
            libraries: Mutex::new(HashMap::new()),
        })
    }

    pub(crate) async fn balance(&self, address: &Address) -> Result<U256, ChainError> {
        Ok(self
            .pool
            .call(None, |endpoint| async move {
                let base = &endpoint.url;
                if endpoint.protocol == Protocol::TonLiteservers {
                    return self.lite.balance(base, address).await;
                }
                match self.ready(&endpoint).await? {
                    Provider::Center => {
                        let path = format!("v2/getAddressBalance?address={address}");
                        let result = center(self.fetch(base, &path, None).await?)?;
                        decimal(&result)
                    }
                    Provider::Api => {
                        let path = format!("accounts/{address}");
                        api(self.fetch(base, &path, None).await?)?.map_or(Ok(U256::ZERO), |reply| {
                            reply["balance"]
                                .as_u64()
                                .map(U256::from)
                                .or_else(|| decimal(&reply["balance"]).ok())
                                .ok_or_else(malformed)
                        })
                    }
                }
            })
            .await?)
    }

    pub(crate) async fn run(
        &self,
        address: &Address,
        method: &str,
        arg: Option<&Address>,
    ) -> Result<Option<Vec<Item>>, ChainError> {
        let account = self.account(address).await?;
        if !matches!(account.status, Status::Active { .. }) {
            return Ok(None);
        }
        let params = self.config().await?.encoded().ok_or_else(|| {
            ChainError::Network("the network config cannot be encoded".to_owned())
        })?;
        let now = u32::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs()),
        )
        .unwrap_or(u32::MAX);
        let mut libraries = Vec::new();
        for _ in 0..=MAX_LIBRARIES {
            match vm::run(&account.boc, method, arg, &params, &libraries, now)? {
                Ran::Done(items) => return Ok(Some(items)),
                Ran::Missing(hash) => libraries.push(self.library(&hash).await?),
            }
        }
        Err(ChainError::Network(
            "a contract needs too many libraries".to_owned(),
        ))
    }

    pub(crate) async fn account(&self, address: &Address) -> Result<Account, ChainError> {
        Ok(self
            .pool
            .call(None, |endpoint| async move {
                let base = &endpoint.url;
                match endpoint.protocol {
                    Protocol::TonLiteservers => self.lite.account(base, address).await,
                    Protocol::TonApi => {
                        self.ready(&endpoint).await?;
                        let path = format!("liteserver/get_account_state/{address}");
                        let reply =
                            api(self.fetch(base, &path, None).await?)?.ok_or_else(unsupported)?;
                        let state = proxied_state(&reply).ok_or_else(malformed)?;
                        proven_account(&state.block, &state, address)
                    }
                    _ => Err(unsupported()),
                }
            })
            .await?)
    }

    pub(crate) async fn config(&self) -> Result<Config, ChainError> {
        let cached = self
            .config
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .filter(|(at, _)| at.elapsed() < CONFIG_FOR);
        if let Some((_, config)) = cached {
            return Ok(config);
        }
        let config = self
            .pool
            .call(None, |endpoint| async move {
                let base = &endpoint.url;
                match endpoint.protocol {
                    Protocol::TonLiteservers => self.lite.config(base).await,
                    Protocol::TonApi => {
                        self.ready(&endpoint).await?;
                        let head = api(self
                            .fetch(base, "liteserver/get_masterchain_info", None)
                            .await?)?
                        .ok_or_else(unsupported)?;
                        let last = proxied_block(&head["last"]).ok_or_else(malformed)?;
                        let path = format!(
                            "liteserver/get_config_all/({},{:016x},{},{},{})?mode=0",
                            last.workchain,
                            u64::from_be_bytes(last.shard.to_be_bytes()),
                            last.seqno,
                            hex(&last.root_hash),
                            hex(&last.file_hash)
                        );
                        let reply =
                            api(self.fetch(base, &path, None).await?)?.ok_or_else(unsupported)?;
                        let info = proxied_config(&reply).ok_or_else(malformed)?;
                        proven_config(&last, &info)
                    }
                    _ => Err(unsupported()),
                }
            })
            .await?;
        *self.config.lock().unwrap_or_else(PoisonError::into_inner) =
            Some((Instant::now(), config.clone()));
        Ok(config)
    }

    pub(crate) async fn library(&self, hash: &[u8; 32]) -> Result<Vec<u8>, ChainError> {
        let known = self
            .libraries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(hash)
            .cloned();
        if let Some(library) = known {
            return Ok(library);
        }
        let library = self
            .pool
            .call(None, |endpoint| async move {
                let base = &endpoint.url;
                match endpoint.protocol {
                    Protocol::TonLiteservers => self.lite.library(base, hash).await,
                    Protocol::TonApi => {
                        self.ready(&endpoint).await?;
                        let path = format!("blockchain/libraries/{}", hex(hash));
                        let reply =
                            api(self.fetch(base, &path, None).await?)?.ok_or_else(unsupported)?;
                        let data = reply["boc"]
                            .as_str()
                            .and_then(unhex)
                            .ok_or_else(malformed)?;
                        proven_library(data, hash)
                    }
                    _ => Err(unsupported()),
                }
            })
            .await?;
        self.libraries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(*hash, library.clone());
        Ok(library)
    }

    pub(crate) async fn send(&self, boc: &[u8]) -> Result<(), ChainError> {
        let encoded = STANDARD.encode(boc);
        let encoded = encoded.as_str();
        Ok(self
            .pool
            .call(None, |endpoint| async move {
                let base = &endpoint.url;
                if endpoint.protocol == Protocol::TonLiteservers {
                    return self.lite.send(base, boc).await;
                }
                let provider = self.ready(&endpoint).await?;
                let reply = match provider {
                    Provider::Api => {
                        let body = json!({ "body": encoded });
                        self.fetch(base, "liteserver/send_message", Some(&body))
                            .await?
                    }
                    Provider::Center => {
                        let body = json!({ "boc": encoded });
                        self.fetch(base, "v2/sendBoc", Some(&body)).await?
                    }
                };
                let error = reply.1.as_ref().and_then(|reply| reply["error"].as_str());
                if let Some(rejected) = error.and_then(refused) {
                    return Err(Failure::Fatal(rejected));
                }
                match provider {
                    Provider::Api => api(reply)?.ok_or_else(unsupported).map(drop),
                    Provider::Center => center(reply).map(drop),
                }
            })
            .await?)
    }

    pub(crate) async fn transactions(
        &self,
        address: &Address,
        from: ([u8; 32], u64),
    ) -> Result<Vec<Transaction>, ChainError> {
        Ok(self
            .pool
            .call(None, |endpoint| async move {
                let base = &endpoint.url;
                match endpoint.protocol {
                    Protocol::TonLiteservers => self.lite.transactions(base, address, from).await,
                    Protocol::TonApi => {
                        self.ready(&endpoint).await?;
                        let path = format!(
                            "liteserver/get_transactions/{address}?count={MAX_LISTED}&lt={}&hash={}",
                            from.1,
                            hex(&from.0)
                        );
                        let reply =
                            api(self.fetch(base, &path, None).await?)?.ok_or_else(unsupported)?;
                        let list = reply["transactions"]
                            .as_str()
                            .and_then(unhex)
                            .ok_or_else(malformed)?;
                        proven_transactions(&list, address, from)
                    }
                    _ => Err(unsupported()),
                }
            })
            .await?)
    }

    async fn ready(&self, endpoint: &Endpoint) -> Result<Provider, Failure<ChainError>> {
        let provider = match endpoint.protocol {
            Protocol::TonCenter => Provider::Center,
            Protocol::TonApi => Provider::Api,
            _ => {
                return Err(Failure::Unsupported(ChainError::Network(
                    "this endpoint kind is not used yet".to_owned(),
                )));
            }
        };
        let key = endpoint.url.to_string();
        if self.is_verified(&key) {
            return Ok(provider);
        }
        let base = &endpoint.url;
        let zero_state = match provider {
            Provider::Center => {
                let result = center(self.fetch(base, "v2/getMasterchainInfo", None).await?)?;
                result["init"]["root_hash"]
                    .as_str()
                    .and_then(|text| STANDARD.decode(text).ok())
            }
            Provider::Api => api(self
                .fetch(base, "liteserver/get_masterchain_info", None)
                .await?)?
            .and_then(|reply| reply["init"]["root_hash"].as_str().and_then(unhex)),
        };
        if zero_state.as_deref() != Some(self.zero_state.as_slice()) {
            return Err(Failure::Retry(ChainError::Network(
                "an endpoint serves another TON network".to_owned(),
            )));
        }
        self.verified
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key);
        tokio::time::sleep(Duration::from_millis(u64::from(endpoint.min_gap_ms))).await;
        Ok(provider)
    }

    fn is_verified(&self, key: &str) -> bool {
        self.verified
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(key)
    }

    async fn fetch(
        &self,
        base: &Url,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Reply, Failure<ChainError>> {
        let network = |text: String| Failure::Retry(ChainError::Network(text));
        let url = join(base, path)
            .ok_or_else(|| network("an endpoint URL cannot take a path".to_owned()))?;
        let request = match body {
            Some(body) => self.client.post(url).json(body),
            None => self.client.get(url),
        };
        let mut response = request
            .send()
            .await
            .map_err(|err| network(err.without_url().to_string()))?;
        let status = response.status();
        if status == StatusCode::TOO_MANY_REQUESTS {
            let wait = response
                .headers()
                .get(RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse().ok())
                .map(Duration::from_secs);
            return Err(Failure::RateLimited(
                ChainError::Network("rate limited".to_owned()),
                wait,
            ));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|err| network(err.without_url().to_string()))?
        {
            if bytes.len() + chunk.len() > MAX_REPLY {
                return Err(network("reply too large".to_owned()));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok((status, serde_json::from_slice(&bytes).ok()))
    }
}

fn proxied_block(value: &Value) -> Option<BlockId> {
    let shard = u64::from_str_radix(value["shard"].as_str()?, 16).ok()?;
    Some(BlockId {
        workchain: i32::try_from(value["workchain"].as_i64()?).ok()?,
        shard: i64::from_be_bytes(shard.to_be_bytes()),
        seqno: i32::try_from(value["seqno"].as_i64()?).ok()?,
        root_hash: unhex(value["root_hash"].as_str()?)?.try_into().ok()?,
        file_hash: unhex(value["file_hash"].as_str()?)?.try_into().ok()?,
    })
}

fn proxied_state(reply: &Value) -> Option<AccountState> {
    Some(AccountState {
        block: proxied_block(&reply["id"])?,
        shard_block: proxied_block(&reply["shardblk"])?,
        shard_proof: unhex(reply["shard_proof"].as_str()?)?,
        proof: unhex(reply["proof"].as_str()?)?,
        state: unhex(reply["state"].as_str()?)?,
    })
}

fn proxied_config(reply: &Value) -> Option<ConfigInfo> {
    Some(ConfigInfo {
        block: proxied_block(&reply["id"])?,
        state_proof: unhex(reply["state_proof"].as_str()?)?,
        config_proof: unhex(reply["config_proof"].as_str()?)?,
    })
}

fn center((status, reply): Reply) -> Result<Value, Failure<ChainError>> {
    if matches!(
        status,
        StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
    ) {
        return Err(unsupported());
    }
    let reply = reply.ok_or_else(|| retry("reply is not JSON"))?;
    if status.is_success() && reply["ok"] == true {
        return Ok(reply["result"].clone());
    }
    Err(refusal(status, &reply["error"]))
}

fn api((status, reply): Reply) -> Result<Option<Value>, Failure<ChainError>> {
    let missing = reply
        .as_ref()
        .and_then(|reply| reply["error"].as_str())
        .is_some_and(|error| error.to_ascii_lowercase().contains("not found"));
    if status == StatusCode::NOT_FOUND && missing {
        return Ok(None);
    }
    if matches!(
        status,
        StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
    ) {
        return Err(unsupported());
    }
    let reply = reply.ok_or_else(|| retry("reply is not JSON"))?;
    if status.is_success() {
        return Ok(Some(reply));
    }
    Err(refusal(status, &reply["error"]))
}

fn refusal(status: StatusCode, error: &Value) -> Failure<ChainError> {
    let message = to_plain_text(error.as_str().unwrap_or("unknown error"), MAX_MESSAGE);
    let lower = message.to_ascii_lowercase();
    if lower.contains("rate limit") || lower.contains("ratelimit") {
        Failure::RateLimited(ChainError::Network(message), None)
    } else if status.is_server_error() || status.is_success() {
        Failure::Retry(ChainError::Network(message))
    } else {
        Failure::Fatal(ChainError::Rejected(message))
    }
}

fn decimal(value: &Value) -> Result<U256, Failure<ChainError>> {
    value
        .as_str()
        .filter(|text| {
            !text.is_empty() && text.len() <= 78 && text.bytes().all(|byte| byte.is_ascii_digit())
        })
        .and_then(|text| U256::from_str_radix(text, 10).ok())
        .ok_or_else(|| retry("an endpoint sent a malformed balance"))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).ok())
        .collect()
}

fn join(base: &Url, path: &str) -> Option<Url> {
    let (path, query) = path
        .split_once('?')
        .map_or((path, None), |(path, query)| (path, Some(query)));
    let mut url = base.clone();
    url.path_segments_mut()
        .ok()?
        .pop_if_empty()
        .extend(path.split('/'));
    if let Some(query) = query {
        for pair in query.split('&') {
            let (key, value) = pair.split_once('=')?;
            url.query_pairs_mut().append_pair(key, value);
        }
    }
    Some(url)
}

fn retry(text: &str) -> Failure<ChainError> {
    Failure::Retry(ChainError::Network(text.to_owned()))
}

fn unsupported() -> Failure<ChainError> {
    Failure::Unsupported(ChainError::Network(
        "an endpoint does not serve this request".to_owned(),
    ))
}

fn malformed() -> Failure<ChainError> {
    retry("an endpoint sent a malformed reply")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_extend_the_endpoint_url() {
        let center = Url::parse("https://toncenter.com/api").unwrap();
        assert_eq!(
            join(&center, "v2/getAddressBalance?address=0:ab")
                .unwrap()
                .as_str(),
            "https://toncenter.com/api/v2/getAddressBalance?address=0%3Aab"
        );
        let tonapi = Url::parse("https://tonapi.io/v2").unwrap();
        assert_eq!(
            join(&tonapi, "blockchain/accounts/0:ab/methods/seqno")
                .unwrap()
                .as_str(),
            "https://tonapi.io/v2/blockchain/accounts/0:ab/methods/seqno"
        );
        assert_eq!(
            join(
                &tonapi,
                "liteserver/get_transactions/0:ab?count=16&lt=7&hash=cd"
            )
            .unwrap()
            .as_str(),
            "https://tonapi.io/v2/liteserver/get_transactions/0:ab?count=16&lt=7&hash=cd"
        );
        assert_eq!(join(&tonapi, "a?b"), None);
    }

    #[test]
    fn provider_errors_are_classified() {
        assert!(matches!(
            refusal(
                StatusCode::BAD_GATEWAY,
                &json!("LITE_SERVER_NETWORK timeout")
            ),
            Failure::Retry(_)
        ));
        assert!(matches!(
            refusal(StatusCode::BAD_REQUEST, &json!("Incorrect address")),
            Failure::Fatal(ChainError::Rejected(_))
        ));
        assert!(matches!(
            refusal(StatusCode::FORBIDDEN, &json!("Ratelimit exceeded")),
            Failure::RateLimited(..)
        ));
        assert_eq!(
            api((
                StatusCode::NOT_FOUND,
                Some(json!({"error": "entity not found"}))
            ))
            .ok(),
            Some(None)
        );
        assert!(matches!(
            api((StatusCode::NOT_FOUND, None)),
            Err(Failure::Unsupported(_))
        ));
    }

    #[tokio::test]
    #[ignore = "talks to the real TON network, run with --test-threads=1"]
    async fn live_contracts_run_here_match_the_network_vm() {
        let registry = bahaa_config::Registry::builtin().unwrap();
        let ton = registry.get(&"ton".parse().unwrap()).unwrap().clone();
        let bahaa_config::ChainKind::Ton { zero_state, .. } = ton.kind else {
            panic!("not TON");
        };
        let endpoints = ton
            .endpoints
            .iter()
            .filter(|endpoint| endpoint.protocol == Protocol::TonLiteservers)
            .cloned()
            .collect();
        let http = Http::new(endpoints, zero_state).unwrap();
        let client = reqwest::Client::new();
        let tonapi = |path: String| {
            let client = client.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(1500)).await;
                client
                    .get(format!("https://tonapi.io/v2/{path}"))
                    .send()
                    .await
                    .unwrap()
                    .json::<Value>()
                    .await
                    .unwrap()
            }
        };
        let raw = |text: &str| Address::parse(bahaa_types::ChainFamily::Ton, text).unwrap();
        for master in [
            "EQCxE6mUtQJKFnGfaROTKOt1lZbDiiX1kCixRv7Nw2Id_sDs",
            "EQAvlWFDxGF2lXm67y4yzC17wYKD9A0guwPkMs1gOsM__NOT",
            "EQCvxJy4eG8hyHBFsZ7eePxrRsUQSFE_jpptRAYBmcG_DOGS",
            "EQA2kCVNwVsil2EM2mB0SkXytxCqQjS4mttjDpnXmwG9T6bO",
            "EQC98_qAmNEptUtPc7W6xdHh_ZHrBUFpw5Ft_IzNU20QAJav",
        ] {
            let master = raw(master);
            let holders = tonapi(format!("jettons/{master}/holders?limit=1")).await;
            let top = &holders["addresses"][0];
            let owner = raw(top["owner"]["address"].as_str().unwrap());
            let wallet = raw(top["address"].as_str().unwrap());
            let found = http
                .run(&master, "get_wallet_address", Some(&owner))
                .await
                .unwrap()
                .unwrap();
            let [Item::Cell(boc)] = found.as_slice() else {
                panic!("{master}: {found:?}");
            };
            assert_eq!(crate::wallet::read_address(boc), Some(wallet), "{master}");
            let data = http
                .run(&wallet, "get_wallet_data", None)
                .await
                .unwrap()
                .unwrap();
            let [
                Item::Num(balance),
                Item::Cell(holder),
                Item::Cell(minter),
                _,
            ] = data.as_slice()
            else {
                panic!("{master}: {data:?}");
            };
            assert_eq!(crate::wallet::read_address(holder), Some(owner), "{master}");
            assert_eq!(
                crate::wallet::read_address(minter),
                Some(master),
                "{master}"
            );
            let remote = tonapi(format!(
                "blockchain/accounts/{wallet}/methods/get_wallet_data"
            ))
            .await;
            assert_eq!(
                balance.to_string(),
                remote["decoded"]["balance"].as_str().unwrap(),
                "{master}"
            );
        }
    }
}
