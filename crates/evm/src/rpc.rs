use std::collections::{BTreeSet, HashMap};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use bahaa_chain::{ChainError, Failure, Pool};
use bahaa_config::{Endpoint, Feature};
use bahaa_types::to_plain_text;
use reqwest::StatusCode;
use reqwest::header::RETRY_AFTER;
use ruint::aliases::U256;
use serde_json::{Value, json};
use url::Url;

const MAX_REPLY: usize = 4 * 1024 * 1024;
const MAX_MESSAGE: usize = 200;
const TIMEOUT: Duration = Duration::from_secs(15);
const REFUSAL_MEMORY: Duration = Duration::from_secs(3600);

#[derive(Debug)]
pub(crate) struct Rpc {
    pool: Pool,
    http: reqwest::Client,
    chain_id: u64,
    verified: Mutex<BTreeSet<String>>,
    refused: Mutex<HashMap<(String, String), Instant>>,
}

impl Rpc {
    pub(crate) fn new(endpoints: Vec<Endpoint>, chain_id: u64) -> Result<Self, ChainError> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("bahaa-wallet/", env!("CARGO_PKG_VERSION")))
            .timeout(TIMEOUT)
            .build()
            .map_err(|err| ChainError::Network(err.to_string()))?;
        Ok(Self {
            pool: Pool::new(endpoints),
            http,
            chain_id,
            verified: Mutex::new(BTreeSet::new()),
            refused: Mutex::new(HashMap::new()),
        })
    }

    pub(crate) async fn call(&self, method: &str, params: Value) -> Result<Value, ChainError> {
        self.call_with(None, method, params).await
    }

    pub(crate) async fn call_with(
        &self,
        need: Option<Feature>,
        method: &str,
        params: Value,
    ) -> Result<Value, ChainError> {
        Ok(self
            .pool
            .call(need, |endpoint| {
                self.attempt(endpoint, method, params.clone())
            })
            .await?)
    }

    async fn attempt(
        &self,
        endpoint: Endpoint,
        method: &str,
        params: Value,
    ) -> Result<Value, Failure<ChainError>> {
        let key = endpoint.url.to_string();
        let refusal = (key.clone(), method.to_owned());
        if self.refused_recently(&refusal) {
            return Err(Failure::Unsupported(ChainError::Network(format!(
                "an endpoint does not serve {method}"
            ))));
        }
        if !self.is_verified(&key) {
            let reported = quantity(&self.send(&endpoint.url, "eth_chainId", json!([])).await?)
                .map_err(Failure::Retry)?;
            if reported != U256::from(self.chain_id) {
                return Err(Failure::Retry(ChainError::Network(format!(
                    "an endpoint reports chain {reported}, expected {}",
                    self.chain_id
                ))));
            }
            self.verified
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(key);
        }
        let result = self.send(&endpoint.url, method, params).await;
        if let Err(Failure::Unsupported(_)) = &result {
            self.refused
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(refusal, Instant::now() + REFUSAL_MEMORY);
        }
        result
    }

    fn refused_recently(&self, refusal: &(String, String)) -> bool {
        let mut refused = self.refused.lock().unwrap_or_else(PoisonError::into_inner);
        match refused.get(refusal) {
            Some(until) if *until > Instant::now() => true,
            Some(_) => {
                refused.remove(refusal);
                false
            }
            None => false,
        }
    }

    fn is_verified(&self, key: &str) -> bool {
        self.verified
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(key)
    }

    async fn send(
        &self,
        url: &Url,
        method: &str,
        params: Value,
    ) -> Result<Value, Failure<ChainError>> {
        let network = |text: String| Failure::Retry(ChainError::Network(text));
        let mut response = self
            .http
            .post(url.clone())
            .json(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}))
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
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|err| network(err.without_url().to_string()))?
        {
            if body.len() + chunk.len() > MAX_REPLY {
                return Err(network("reply too large".to_owned()));
            }
            body.extend_from_slice(&chunk);
        }
        let reply: Option<Value> = serde_json::from_slice(&body).ok();
        let error = reply
            .as_ref()
            .and_then(|reply| reply.get("error"))
            .map(|error| {
                let code = error["code"].as_i64().unwrap_or(0);
                classify(code, error["message"].as_str().unwrap_or("unknown error"))
            });
        if !status.is_success() {
            return Err(match error {
                Some(failure @ (Failure::Unsupported(_) | Failure::RateLimited(..))) => failure,
                _ => network(format!("HTTP {status}")),
            });
        }
        if let Some(failure) = error {
            return Err(failure);
        }
        let reply = reply.ok_or_else(|| network("reply is not JSON".to_owned()))?;
        reply
            .get("result")
            .cloned()
            .ok_or_else(|| network("reply without a result".to_owned()))
    }
}

pub(crate) fn classify(code: i64, message: &str) -> Failure<ChainError> {
    let message = to_plain_text(message, MAX_MESSAGE);
    let lower = message.to_ascii_lowercase();
    let limited = code == -32005
        || [
            "rate limit",
            "too many requests",
            "limit exceeded",
            "limit reached",
            "upgrade to paid",
        ]
        .iter()
        .any(|needle| lower.contains(needle));
    if limited {
        return Failure::RateLimited(ChainError::Network(message), None);
    }
    let unsupported = code == -32601
        || [
            "archive",
            "personal token",
            "block range",
            "blocks range",
            "free plan",
            "specify an address",
            "does not exist",
            "method not found",
        ]
        .iter()
        .any(|needle| lower.contains(needle));
    if unsupported {
        return Failure::Unsupported(ChainError::Network(message));
    }
    if matches!(code, -32603 | -32001 | -32002 | -32004) {
        return Failure::Retry(ChainError::Network(message));
    }
    Failure::Fatal(ChainError::Rejected(message))
}

pub(crate) fn quantity(value: &Value) -> Result<U256, ChainError> {
    let invalid = || ChainError::Network("an endpoint sent a malformed number".to_owned());
    let digits = value
        .as_str()
        .and_then(|text| text.strip_prefix("0x"))
        .filter(|digits| !digits.is_empty() && digits.len() <= 64)
        .ok_or_else(invalid)?;
    U256::from_str_radix(digits, 16).map_err(|_| invalid())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(failure: &Failure<ChainError>) -> &'static str {
        match failure {
            Failure::Retry(_) => "retry",
            Failure::RateLimited(..) => "limited",
            Failure::Unsupported(_) => "unsupported",
            Failure::Fatal(_) => "fatal",
        }
    }

    #[test]
    fn classifies_node_errors_like_phase_zero_saw_them() {
        let cases = [
            (-32005, "limit exceeded", "limited"),
            (-32011, "request limit reached", "limited"),
            (
                15,
                "You reached Public endpoint rate limit, please upgrade to paid plan",
                "limited",
            ),
            (
                -32003,
                "Rate limit exceeded on Nodies public endpoints",
                "limited",
            ),
            (
                -32601,
                "the method eth_getLogs does not exist",
                "unsupported",
            ),
            (
                -32602,
                "Archive requests require a personal token. Get one at: https://www.allnodes.com/publicnode",
                "unsupported",
            ),
            (-32701, "exceed maximum block range: 10000", "unsupported"),
            (
                -32602,
                "eth_getLogs is limited to 0 - 50 blocks range",
                "unsupported",
            ),
            (
                35,
                "ranges over 10000 blocks are not supported on free plan",
                "unsupported",
            ),
            (-32000, "transaction type not supported", "fatal"),
            (-32603, "Internal error", "retry"),
            (
                -32000,
                "insufficient funds for gas * price + value",
                "fatal",
            ),
            (-32000, "nonce too low", "fatal"),
            (3, "execution reverted", "fatal"),
        ];
        for (code, message, expected) in cases {
            assert_eq!(kind(&classify(code, message)), expected, "{message}");
        }
    }

    #[test]
    fn node_messages_arrive_as_plain_text() {
        let hostile = format!("nonce\u{202E} too\nlow\u{7}{}", "x".repeat(500));
        let Failure::Fatal(ChainError::Rejected(message)) = classify(-32000, &hostile) else {
            panic!("expected a rejection");
        };
        assert!(message.starts_with("nonce too low"));
        assert_eq!(message.chars().count(), MAX_MESSAGE);
        assert!(bahaa_types::is_plain_text(&message, MAX_MESSAGE));
    }

    #[test]
    fn parses_quantities_strictly() {
        assert_eq!(quantity(&json!("0x38")).unwrap(), U256::from(56u8));
        assert_eq!(quantity(&json!("0x0")).unwrap(), U256::ZERO);
        for bad in [
            json!("0x"),
            json!("38"),
            json!(56),
            json!(null),
            json!(format!("0x{}", "f".repeat(65))),
        ] {
            assert!(quantity(&bad).is_err(), "{bad}");
        }
    }
}
