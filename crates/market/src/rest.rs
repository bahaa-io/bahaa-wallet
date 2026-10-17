use std::collections::BTreeSet;
use std::time::Duration;

use bahaa_types::PriceId;
use reqwest::StatusCode;
use reqwest::header::{CONTENT_TYPE, ETAG, IF_NONE_MATCH, RETRY_AFTER};
use thiserror::Error;
use url::Url;

use crate::protocol::{self, Quotes};

pub const MAX_REPLY: usize = 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum MarketError {
    #[error("price service unreachable: {0}")]
    Network(String),
    #[error("price service answered HTTP {0}")]
    Http(u16),
    #[error("price service asks to slow down")]
    RateLimited(Option<Duration>),
    #[error("price service refused the request: {code} {description}")]
    Api { code: String, description: String },
    #[error("price service sent something unexpected: {0}")]
    Format(String),
    #[error("price service reply is larger than {MAX_REPLY} bytes")]
    TooLarge,
    #[error("{0}")]
    Invalid(&'static str),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fetch {
    Fresh {
        quotes: Quotes,
        etag: Option<String>,
    },
    NotModified,
}

#[derive(Clone, Debug)]
pub struct RestClient {
    http: reqwest::Client,
    rates: Url,
}

impl RestClient {
    pub fn new(api: &Url) -> Result<Self, MarketError> {
        let rates = api
            .join("v1/rates")
            .map_err(|_| MarketError::Invalid("not a usable price service address"))?;
        let http = reqwest::Client::builder()
            .user_agent(concat!("bahaa-wallet/", env!("CARGO_PKG_VERSION")))
            .timeout(TIMEOUT)
            .build()
            .map_err(|err| MarketError::Network(err.to_string()))?;
        Ok(Self { http, rates })
    }

    pub async fn fetch(
        &self,
        wanted: &BTreeSet<PriceId>,
        base: &str,
        etag: Option<&str>,
    ) -> Result<Fetch, MarketError> {
        if !protocol::is_base(base) {
            return Err(MarketError::Invalid(
                "a price base is 3 to 5 capital letters",
            ));
        }
        if wanted.is_empty() {
            return Ok(Fetch::Fresh {
                quotes: Quotes::new(),
                etag: None,
            });
        }
        let mut request = self
            .http
            .post(self.rates.clone())
            .header(CONTENT_TYPE, "application/json")
            .body(protocol::query(wanted, base).to_string());
        if let Some(etag) = etag {
            request = request.header(IF_NONE_MATCH, etag);
        }
        let mut response = request
            .send()
            .await
            .map_err(|err| MarketError::Network(err.without_url().to_string()))?;
        let status = response.status();
        if status == StatusCode::NOT_MODIFIED {
            return Ok(Fetch::NotModified);
        }
        if status == StatusCode::TOO_MANY_REQUESTS {
            let wait = response
                .headers()
                .get(RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse().ok())
                .map(Duration::from_secs);
            return Err(MarketError::RateLimited(wait));
        }
        let etag = response
            .headers()
            .get(ETAG)
            .and_then(|value| value.to_str().ok())
            .filter(|value| value.len() <= 128 && value.bytes().all(|byte| byte.is_ascii_graphic()))
            .map(str::to_owned);
        if response
            .content_length()
            .is_some_and(|len| usize::try_from(len).map_or(true, |len| len > MAX_REPLY))
        {
            return Err(MarketError::TooLarge);
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|err| MarketError::Network(err.without_url().to_string()))?
        {
            if body.len() + chunk.len() > MAX_REPLY {
                return Err(MarketError::TooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        let items = protocol::read_envelope(&body);
        if !status.is_success() {
            return Err(items.err().unwrap_or(MarketError::Http(status.as_u16())));
        }
        Ok(Fetch::Fresh {
            quotes: protocol::quotes(items?, base, wanted),
            etag,
        })
    }
}
