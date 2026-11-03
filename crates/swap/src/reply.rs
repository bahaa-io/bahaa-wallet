use bahaa_chain::http::exchange;
use bahaa_chain::{ChainError, Failure};
use bahaa_types::to_plain_text;
use reqwest::RequestBuilder;
use serde_json::Value;

use crate::order::SwapError;

const MAX_REASON: usize = 200;

pub(crate) async fn fetch(request: RequestBuilder) -> Result<Value, SwapError> {
    let (status, body) = exchange(request).await.map_err(unreachable)?;
    let value = serde_json::from_slice::<Value>(&body);
    if status.is_success() {
        return value.map_err(|_| unreadable());
    }
    if status.is_client_error() {
        let reason = value
            .ok()
            .and_then(|value| value.get("message")?.as_str().map(clean))
            .filter(|reason| !reason.is_empty())
            .unwrap_or_else(|| format!("the provider refused the request ({status})"));
        return Err(SwapError::Refused(reason));
    }
    Err(SwapError::Unreachable(format!(
        "the provider is not answering ({status})"
    )))
}

pub(crate) fn unreadable() -> SwapError {
    SwapError::Untrusted("the provider sent a reply this wallet can't read")
}

fn unreachable(failure: Failure<ChainError>) -> SwapError {
    let (Failure::Retry(err)
    | Failure::RateLimited(err, _)
    | Failure::Unreachable(err)
    | Failure::Unsupported(err)
    | Failure::Fatal(err)) = failure;
    SwapError::Unreachable(err.to_string())
}

fn clean(text: &str) -> String {
    to_plain_text(text, MAX_REASON)
}
