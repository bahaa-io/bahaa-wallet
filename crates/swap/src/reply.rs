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
            .and_then(|value| {
                let text = |field: &str| value.get(field).and_then(Value::as_str);
                text("message")
                    .or_else(|| text("description"))
                    .or_else(|| text("errorType"))
                    .map(clean)
            })
            .filter(|reason| !reason.is_empty())
            .unwrap_or_else(|| format!("the provider refused the request ({status})"));
        return Err(SwapError::Refused(reason));
    }
    Err(SwapError::Unreachable(format!(
        "the provider is not answering ({status})"
    )))
}

pub(crate) fn seconds(number: &serde_json::Number) -> Option<u64> {
    if let Some(whole) = number.as_u64() {
        return Some(whole);
    }
    let text = number.to_string();
    let (whole, part) = text.split_once('.')?;
    let whole: u64 = whole.parse().ok()?;
    whole.checked_add(u64::from(part.bytes().any(|digit| digit != b'0')))
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

#[cfg(test)]
mod tests {
    use super::seconds;

    #[test]
    fn a_fractional_time_estimate_rounds_up_to_whole_seconds() {
        let number = |text: &str| serde_json::from_str::<serde_json::Number>(text).unwrap();
        assert_eq!(seconds(&number("1.5")), Some(2));
        assert_eq!(seconds(&number("7")), Some(7));
        assert_eq!(seconds(&number("12.0")), Some(12));
        assert_eq!(seconds(&number("-3")), None);
    }
}
