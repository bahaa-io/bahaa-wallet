use std::collections::BTreeSet;
use std::fmt::Write;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use bahaa_chain::{ChainError, Failure, Pool};
use bahaa_config::{Endpoint, Feature};
use bahaa_types::to_plain_text;
use reqwest::StatusCode;
use reqwest::header::RETRY_AFTER;
use serde_json::{Value, json};
use url::Url;

const MAX_REPLY: usize = 4 * 1024 * 1024;
const MAX_MESSAGE: usize = 200;
const TIMEOUT: Duration = Duration::from_secs(15);
const BUSY: [&str; 4] = [
    "SERVER_BUSY",
    "NO_CONNECTION",
    "NOT_ENOUGH_EFFECTIVE_CONNECTION",
    "BLOCK_UNSOLIDIFIED",
];

#[derive(Clone, Copy)]
enum Request<'a> {
    Post(&'a Value),
    Get(&'a [(&'a str, String)]),
}

#[derive(Debug)]
pub(crate) struct Http {
    pool: Pool,
    client: reqwest::Client,
    genesis: String,
    verified: Mutex<BTreeSet<String>>,
}

impl Http {
    pub(crate) fn new(endpoints: Vec<Endpoint>, genesis: &[u8; 32]) -> Result<Self, ChainError> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("bahaa-wallet/", env!("CARGO_PKG_VERSION")))
            .timeout(TIMEOUT)
            .build()
            .map_err(|err| ChainError::Network(err.to_string()))?;
        Ok(Self {
            pool: Pool::new(endpoints),
            client,
            genesis: hex(genesis),
            verified: Mutex::new(BTreeSet::new()),
        })
    }

    pub(crate) async fn post(&self, path: &str, body: Value) -> Result<Value, ChainError> {
        self.request(None, path, Request::Post(&body)).await
    }

    pub(crate) async fn history(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<Value, ChainError> {
        self.request(Some(Feature::History), path, Request::Get(query))
            .await
    }

    async fn request(
        &self,
        need: Option<Feature>,
        path: &str,
        request: Request<'_>,
    ) -> Result<Value, ChainError> {
        Ok(self
            .pool
            .call(need, |endpoint| self.attempt(endpoint, path, request))
            .await?)
    }

    async fn attempt(
        &self,
        endpoint: Endpoint,
        path: &str,
        request: Request<'_>,
    ) -> Result<Value, Failure<ChainError>> {
        let key = endpoint.url.to_string();
        if !self.is_verified(&key) {
            let first = self
                .send(
                    &endpoint.url,
                    "wallet/getblockbynum",
                    Request::Post(&json!({"num": 0})),
                )
                .await?;
            let same = first["blockID"]
                .as_str()
                .is_some_and(|id| id.eq_ignore_ascii_case(&self.genesis));
            if !same {
                return Err(Failure::Retry(ChainError::Network(
                    "an endpoint serves another Tron network".to_owned(),
                )));
            }
            self.verified
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(key);
        }
        self.send(&endpoint.url, path, request).await
    }

    fn is_verified(&self, key: &str) -> bool {
        self.verified
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(key)
    }

    async fn send(
        &self,
        base: &Url,
        path: &str,
        request: Request<'_>,
    ) -> Result<Value, Failure<ChainError>> {
        let network = |text: String| Failure::Retry(ChainError::Network(text));
        let mut url = join(base, path)
            .ok_or_else(|| network("an endpoint URL cannot take a path".to_owned()))?;
        let builder = match request {
            Request::Post(body) => self.client.post(url).json(body),
            Request::Get(query) => {
                url.query_pairs_mut()
                    .extend_pairs(query.iter().map(|(key, value)| (*key, value.as_str())));
                self.client.get(url)
            }
        };
        let mut response = builder
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
        let reply: Option<Value> = serde_json::from_slice(&bytes).ok();
        let error = reply
            .as_ref()
            .and_then(|reply| reply.get("Error"))
            .map(|error| classify(error.as_str().unwrap_or("unknown error")));
        if matches!(
            status,
            StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
        ) {
            return Err(Failure::Unsupported(ChainError::Network(format!(
                "an endpoint does not serve /{path}"
            ))));
        }
        if !status.is_success() {
            return Err(match error {
                Some(failure @ Failure::RateLimited(..)) => failure,
                _ => network(format!("HTTP {status}")),
            });
        }
        if let Some(failure) = error {
            return Err(failure);
        }
        let reply = reply.ok_or_else(|| network("reply is not JSON".to_owned()))?;
        if let Some(code) = reply["code"].as_str().filter(|code| BUSY.contains(code)) {
            return Err(network(format!(
                "an endpoint cannot take transactions now: {code}"
            )));
        }
        Ok(reply)
    }
}

pub(crate) fn classify(message: &str) -> Failure<ChainError> {
    let message = to_plain_text(message, MAX_MESSAGE);
    let lower = message.to_ascii_lowercase();
    let limited = ["frequency limit", "rate limit", "too many requests"]
        .iter()
        .any(|needle| lower.contains(needle));
    if limited {
        return Failure::RateLimited(ChainError::Network(message), None);
    }
    Failure::Fatal(ChainError::Rejected(message))
}

pub(crate) fn node_text(value: &Value) -> String {
    let text = value.as_str().unwrap_or("unknown error");
    match unhex(text) {
        Some(bytes) => to_plain_text(&String::from_utf8_lossy(&bytes), MAX_MESSAGE),
        None => to_plain_text(text, MAX_MESSAGE),
    }
}

pub(crate) fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).ok())
        .collect()
}

fn join(base: &Url, path: &str) -> Option<Url> {
    let mut url = base.clone();
    url.path_segments_mut()
        .ok()?
        .pop_if_empty()
        .extend(path.split('/'));
    Some(url)
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_extend_the_endpoint_url() {
        let cases = [
            (
                "https://api.trongrid.io",
                "https://api.trongrid.io/wallet/getaccount",
            ),
            (
                "https://api.trongrid.io/",
                "https://api.trongrid.io/wallet/getaccount",
            ),
            ("https://host/tron/", "https://host/tron/wallet/getaccount"),
            (
                "https://host/tron?key=1",
                "https://host/tron/wallet/getaccount?key=1",
            ),
        ];
        for (base, joined) in cases {
            let base = Url::parse(base).unwrap();
            assert_eq!(join(&base, "wallet/getaccount").unwrap().as_str(), joined);
        }
    }

    #[test]
    fn node_errors_are_classified() {
        assert!(matches!(
            classify("The key exceeds the frequency limit(3), and the query server is suspended"),
            Failure::RateLimited(..)
        ));
        assert_eq!(
            classify("class org.tron.core.services.http.JsonFormat$ParseException : INVALID hex"),
            Failure::Fatal(ChainError::Rejected(
                "class org.tron.core.services.http.JsonFormat$ParseException : INVALID hex"
                    .to_owned()
            ))
        );
    }

    #[test]
    fn node_messages_are_decoded_and_cleaned() {
        assert_eq!(
            node_text(&json!(
                "536d61727420636f6e7472616374206973206e6f742065786973742e"
            )),
            "Smart contract is not exist."
        );
        assert_eq!(node_text(&json!("plain words")), "plain words");
        assert_eq!(node_text(&json!("e280ae6f6b")), "ok");
        assert_eq!(node_text(&json!(7)), "unknown error");
    }
}
