use std::time::Duration;

use reqwest::header::RETRY_AFTER;
use reqwest::{Client, RequestBuilder, StatusCode};

use crate::{ChainError, Failure};

const MAX_REPLY: usize = 4 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(15);

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
