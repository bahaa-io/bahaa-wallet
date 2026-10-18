use std::collections::BTreeSet;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

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

use crate::wallet::address_boc;

const MAX_REPLY: usize = 4 * 1024 * 1024;
const MAX_MESSAGE: usize = 200;
const TIMEOUT: Duration = Duration::from_secs(20);
const NOT_DEPLOYED: i64 = -13;

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
            client,
            zero_state,
            verified: Mutex::new(BTreeSet::new()),
        })
    }

    pub(crate) async fn balance(&self, address: &Address) -> Result<U256, ChainError> {
        Ok(self
            .pool
            .call(None, |endpoint| async move {
                let base = &endpoint.url;
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
        Ok(self
            .pool
            .call(None, |endpoint| async move {
                let base = &endpoint.url;
                match self.ready(&endpoint).await? {
                    Provider::Center => {
                        let stack = match arg {
                            Some(arg) => {
                                let boc = address_boc(arg).map_err(Failure::Fatal)?;
                                json!([["tvm.Slice", STANDARD.encode(boc)]])
                            }
                            None => json!([]),
                        };
                        let body = json!({"address": address.to_string(), "method": method, "stack": stack});
                        let result = center(self.fetch(base, "v2/runGetMethod", Some(&body)).await?)?;
                        executed(method, &result["exit_code"], &result["stack"], center_item)
                    }
                    Provider::Api => {
                        let path = match arg {
                            Some(arg) => {
                                format!("blockchain/accounts/{address}/methods/{method}?args={arg}")
                            }
                            None => format!("blockchain/accounts/{address}/methods/{method}"),
                        };
                        match api(self.fetch(base, &path, None).await?)? {
                            None => Ok(None),
                            Some(reply) => {
                                executed(method, &reply["exit_code"], &reply["stack"], api_item)
                            }
                        }
                    }
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

fn executed(
    method: &str,
    exit_code: &Value,
    stack: &Value,
    read: fn(&Value) -> Option<Item>,
) -> Result<Option<Vec<Item>>, Failure<ChainError>> {
    match exit_code.as_i64() {
        Some(0) => {}
        Some(NOT_DEPLOYED) => return Ok(None),
        Some(code) => {
            return Err(Failure::Fatal(ChainError::Rejected(format!(
                "the contract method {method} stopped with exit code {code}"
            ))));
        }
        None => return Err(retry("a get-method reply has no exit code")),
    }
    stack
        .as_array()
        .ok_or_else(|| retry("a get-method reply has no stack"))?
        .iter()
        .map(|item| read(item).ok_or_else(|| retry("a get-method reply has an unreadable value")))
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

fn center_item(item: &Value) -> Option<Item> {
    match item[0].as_str()? {
        "num" => number(item[1].as_str()?).map(Item::Num),
        "cell" => STANDARD
            .decode(item[1]["bytes"].as_str()?)
            .ok()
            .map(Item::Cell),
        _ => None,
    }
}

fn api_item(item: &Value) -> Option<Item> {
    match item["type"].as_str()? {
        "num" => number(item["num"].as_str()?).map(Item::Num),
        "cell" => unhex(item["cell"].as_str()?).map(Item::Cell),
        _ => None,
    }
}

fn number(text: &str) -> Option<U256> {
    let digits = text.strip_prefix("0x")?;
    (!digits.is_empty() && digits.len() <= 64)
        .then(|| U256::from_str_radix(digits, 16).ok())
        .flatten()
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
        let (key, value) = query.split_once('=')?;
        url.query_pairs_mut().append_pair(key, value);
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
    }

    #[test]
    fn stack_values_are_read_strictly() {
        assert_eq!(
            center_item(&json!(["num", "0x56110d7a1790"])),
            Some(Item::Num(U256::from(94_631_240_538_000u64)))
        );
        assert_eq!(
            api_item(&json!({"type": "cell", "cell": "b5ee"})),
            Some(Item::Cell(vec![0xb5, 0xee]))
        );
        for bad in [
            json!(["num", "-0x5"]),
            json!(["num", "12"]),
            json!(["tuple", []]),
            json!(["cell", {"bytes": "%%"}]),
        ] {
            assert_eq!(center_item(&bad), None, "{bad}");
        }
        assert_eq!(api_item(&json!({"type": "cell", "cell": "b5e"})), None);
    }

    #[test]
    fn exit_codes_tell_missing_contracts_from_failures() {
        let stack = json!([["num", "0x1"]]);
        assert_eq!(
            executed("seqno", &json!(0), &stack, center_item).ok(),
            Some(Some(vec![Item::Num(U256::from(1u8))]))
        );
        assert_eq!(
            executed("seqno", &json!(-13), &stack, center_item).ok(),
            Some(None)
        );
        assert!(matches!(
            executed("seqno", &json!(11), &stack, center_item),
            Err(Failure::Fatal(ChainError::Rejected(_)))
        ));
        assert!(matches!(
            executed("seqno", &json!(0), &json!([["list", []]]), center_item),
            Err(Failure::Retry(_))
        ));
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
}
