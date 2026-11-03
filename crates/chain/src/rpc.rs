use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use bahaa_config::{Endpoint, Feature};
use serde_json::{Value, json};
use url::Url;

use crate::{ChainError, Failure, Pool, PoolError, http};

const REFUSAL_MEMORY: Duration = Duration::from_secs(3600);

pub trait Dialect: Send + Sync {
    const CHECK: &'static str;

    fn check(&self, reply: &Value) -> Result<(), ChainError>;

    fn classify(&self, error: &Value) -> Failure<ChainError>;
}

#[derive(Debug)]
pub struct JsonRpc<D> {
    dialect: D,
    pool: Pool,
    http: reqwest::Client,
    verified: Mutex<BTreeSet<String>>,
    verifying: Mutex<HashMap<String, Arc<futures_util::lock::Mutex<()>>>>,
    refused: Mutex<HashMap<(String, String), Instant>>,
}

impl<D: Dialect> JsonRpc<D> {
    pub fn new(endpoints: Vec<Endpoint>, dialect: D) -> Result<Self, ChainError> {
        let http = http::client()?;
        Ok(Self {
            dialect,
            pool: Pool::new(endpoints),
            http,
            verified: Mutex::new(BTreeSet::new()),
            verifying: Mutex::new(HashMap::new()),
            refused: Mutex::new(HashMap::new()),
        })
    }

    pub async fn call(&self, method: &str, params: Value) -> Result<Value, ChainError> {
        self.call_with(None, method, params).await
    }

    pub async fn call_with(
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

    pub async fn call_if_served(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Option<Value>, ChainError> {
        match self
            .pool
            .call(None, |endpoint| {
                self.attempt(endpoint, method, params.clone())
            })
            .await
        {
            Ok(value) => Ok(Some(value)),
            Err(PoolError::Unserved(_)) => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    pub async fn call_all<T>(
        &self,
        need: Option<Feature>,
        method: &str,
        params: Value,
        judge: impl Fn(Result<Value, Failure<ChainError>>) -> Result<T, Failure<ChainError>>,
    ) -> Result<T, ChainError> {
        let judge = &judge;
        Ok(self
            .pool
            .call_all(need, |endpoint| {
                let attempt = self.attempt(endpoint, method, params.clone());
                async move { judge(attempt.await) }
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
            let turn = Arc::clone(
                self.verifying
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .entry(key.clone())
                    .or_default(),
            );
            let _turn = turn.lock().await;
            if !self.is_verified(&key) {
                let reply = self.send(&endpoint.url, D::CHECK, json!([])).await?;
                self.dialect.check(&reply).map_err(Failure::Retry)?;
                self.verified
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(key);
            }
        }
        let result = self.send(&endpoint.url, method, params).await;
        if let Err(Failure::Unsupported(err)) = &result
            && *err != ChainError::Pruned
        {
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
        let (status, body) = http::exchange(
            self.http
                .post(url.clone())
                .json(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params})),
        )
        .await?;
        let reply: Option<Value> = serde_json::from_slice(&body).ok();
        let error = reply
            .as_ref()
            .and_then(|reply| reply.get("error"))
            .map(|error| self.dialect.classify(error));
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
