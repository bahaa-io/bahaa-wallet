use std::time::Duration;

use reqwest::header::RETRY_AFTER;
use reqwest::redirect::Policy;
use reqwest::{Client, RequestBuilder, StatusCode, Url};

use crate::{ChainError, Failure};

const MAX_REPLY: usize = 4 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(15);
const MAX_REDIRECTS: usize = 3;

pub fn client() -> Result<Client, ChainError> {
    Client::builder()
        .user_agent(concat!("bahaa-wallet/", env!("CARGO_PKG_VERSION")))
        .timeout(TIMEOUT)
        .build()
        .map_err(|err| ChainError::Network(err.to_string()))
}

pub async fn exchange(
    request: RequestBuilder,
) -> Result<(StatusCode, Vec<u8>), Failure<ChainError>> {
    let network = |text: String| Failure::Retry(ChainError::Network(text));
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
    Ok((status, body))
}

pub fn files() -> Result<Client, ChainError> {
    Client::builder()
        .user_agent(concat!("bahaa-wallet/", env!("CARGO_PKG_VERSION")))
        .timeout(TIMEOUT)
        .redirect(Policy::custom(|attempt| {
            if attempt.url().scheme() == "https" && attempt.previous().len() <= MAX_REDIRECTS {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .build()
        .map_err(|err| ChainError::Network(err.to_string()))
}

pub async fn file(client: &Client, url: Url, max: usize) -> Result<Vec<u8>, ChainError> {
    let network = |err: reqwest::Error| ChainError::Network(err.without_url().to_string());
    let mut response = client.get(url).send().await.map_err(network)?;
    if !response.status().is_success() {
        return Err(ChainError::Network(format!(
            "the details file answered HTTP {}",
            response.status()
        )));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(network)? {
        if bytes.len() + chunk.len() > max {
            return Err(ChainError::Invalid("the details file is too large"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
