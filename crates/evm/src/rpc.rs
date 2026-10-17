use std::collections::BTreeSet;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use bahaa_chain::{ChainError, Failure, Pool};
use bahaa_config::{Endpoint, Feature};
use reqwest::StatusCode;
use reqwest::header::RETRY_AFTER;
use ruint::aliases::U256;
use serde_json::{Value, json};
use url::Url;

const MAX_REPLY: usize = 4 * 1024 * 1024;
const MAX_MESSAGE: usize = 200;
const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug)]
pub(crate) struct Rpc {
    pool: Pool,
    http: reqwest::Client,
    chain_id: u64,
    verified: Mutex<BTreeSet<String>>,
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
        self.send(&endpoint.url, method, params).await
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
        if !status.is_success() {
            return Err(network(format!("HTTP {status}")));
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
        let reply: Value =
            serde_json::from_slice(&body).map_err(|_| network("reply is not JSON".to_owned()))?;
        if let Some(error) = reply.get("error") {
            let code = error["code"].as_i64().unwrap_or(0);
            let message: String = error["message"]
                .as_str()
                .unwrap_or("unknown error")
                .chars()
                .take(MAX_MESSAGE)
                .collect();
            return Err(classify(code, message));
        }
        reply
            .get("result")
            .cloned()
            .ok_or_else(|| network("reply without a result".to_owned()))
    }
}

pub(crate) fn classify(code: i64, message: String) -> Failure<ChainError> {
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
    if matches!(code, -32601 | -32603 | -32001 | -32002 | -32004) {
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
            (-32601, "the method eth_getLogs does not exist", "retry"),
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
            assert_eq!(
                kind(&classify(code, message.to_owned())),
                expected,
                "{message}"
            );
        }
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
