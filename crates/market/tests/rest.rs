#![allow(clippy::unwrap_used)]

use std::collections::{BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bahaa_market::{Fetch, MarketError, RestClient, Span};
use bahaa_testkit::Reply;
use bahaa_types::PriceId;
use url::Url;

const BODY: &str = r#"{"ok": true, "meta": {}, "result": [
    {"category": "crypto", "symbol": "BNB", "prices": {"USDT": 714.3}, "updated_at_unix": 1792250000},
    {"category": "crypto", "symbol": "GRAM", "prices": {"USDT": 1.3465}, "updated_at_unix": 1792250001}
]}"#;

fn ok(body: &str, etag: &str) -> Reply {
    plain(
        200,
        &[("Content-Type", "application/json"), ("ETag", etag)],
        body,
    )
}

fn plain(status: u16, headers: &[(&'static str, &str)], body: &str) -> Reply {
    Reply {
        status,
        headers: headers
            .iter()
            .map(|(name, value)| (*name, (*value).to_owned()))
            .collect(),
        body: body.as_bytes().to_vec(),
        delay: Duration::ZERO,
    }
}

async fn serve(replies: Vec<Reply>) -> (Url, Arc<Mutex<Vec<String>>>) {
    let queue = Mutex::new(VecDeque::from(replies));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    let url = bahaa_testkit::serve(move |request| {
        log.lock().unwrap().push(request.text());
        queue.lock().unwrap().pop_front().unwrap()
    })
    .await;
    (url, seen)
}

fn wanted() -> BTreeSet<PriceId> {
    ["crypto/BNB", "crypto/GRAM"]
        .iter()
        .map(|id| id.parse().unwrap())
        .collect()
}

#[tokio::test]
async fn fetches_quotes_then_revalidates_with_the_etag() {
    let (url, server) = serve(vec![
        ok(BODY, "W/\"7\""),
        plain(304, &[("ETag", "W/\"7\"")], ""),
    ])
    .await;
    let client = RestClient::new(&url).unwrap();
    let Fetch::Fresh { quotes, etag } = client.fetch(&wanted(), "USDT", None).await.unwrap() else {
        panic!("expected fresh quotes");
    };
    assert_eq!(quotes.len(), 2);
    assert_eq!(
        quotes[&"crypto/GRAM".parse().unwrap()].price.to_string(),
        "1.3465"
    );
    assert_eq!(etag.as_deref(), Some("W/\"7\""));
    assert_eq!(
        client
            .fetch(&wanted(), "USDT", etag.as_deref())
            .await
            .unwrap(),
        Fetch::NotModified
    );
    let seen = server.lock().unwrap().clone();
    assert!(seen[0].starts_with("POST /v1/rates "));
    assert!(seen[0].contains(r#""right":["BNB","GRAM"]"#));
    assert!(seen[0].contains(r#""bases":["USDT"]"#));
    assert!(!seen[0].to_ascii_lowercase().contains("if-none-match"));
    assert!(
        seen[1]
            .to_ascii_lowercase()
            .contains("if-none-match: w/\"7\"")
    );
}

#[tokio::test]
async fn honors_rate_limits() {
    let (url, _server) = serve(vec![plain(429, &[("Retry-After", "30")], "")]).await;
    let client = RestClient::new(&url).unwrap();
    assert_eq!(
        client.fetch(&wanted(), "USDT", None).await.unwrap_err(),
        MarketError::RateLimited(Some(Duration::from_secs(30)))
    );
}

#[tokio::test]
async fn reports_api_errors_with_their_code() {
    let body = r#"{"ok": false, "code": "UNKNOWN_BASE", "description": "no such base"}"#;
    let (url, _server) = serve(vec![plain(
        400,
        &[("Content-Type", "application/json")],
        body,
    )])
    .await;
    let client = RestClient::new(&url).unwrap();
    assert!(
        matches!(client.fetch(&wanted(), "USDT", None).await.unwrap_err(), MarketError::Api { code, .. } if code == "UNKNOWN_BASE")
    );
}

#[tokio::test]
async fn refuses_oversized_replies() {
    let huge = "x".repeat(bahaa_market::MAX_REPLY + 1);
    let (url, _server) = serve(vec![ok(&huge, "W/\"1\"")]).await;
    let client = RestClient::new(&url).unwrap();
    assert_eq!(
        client.fetch(&wanted(), "USDT", None).await.unwrap_err(),
        MarketError::TooLarge
    );
}

#[tokio::test]
async fn needs_no_request_for_nothing_and_checks_the_base() {
    let client = RestClient::new(&Url::parse("http://127.0.0.1:9/").unwrap()).unwrap();
    assert!(
        matches!(client.fetch(&BTreeSet::new(), "USDT", None).await.unwrap(), Fetch::Fresh { quotes, .. } if quotes.is_empty())
    );
    assert!(matches!(
        client.fetch(&wanted(), "usdt", None).await.unwrap_err(),
        MarketError::Invalid(_)
    ));
}

#[tokio::test]
async fn reads_a_price_history_for_the_asked_span_and_base() {
    let history = r#"{"ok": true, "meta": {"columns": ["t", "o", "h", "l", "c", "v"]},
        "result": [[1794441600, 1, 1, 1, 1.25, null], [1794528000, 1, 1, 1, 1.5, null]]}"#;
    let (url, server) = serve(vec![
        ok(history, "W/\"1\""),
        plain(
            404,
            &[],
            r#"{"ok": false, "code": "NOT_FOUND", "description": "unknown asset"}"#,
        ),
    ])
    .await;
    let client = RestClient::new(&url).unwrap();
    let gram: PriceId = "crypto/GRAM".parse().unwrap();
    let points = client
        .history(&gram, "EUR", Span::Week, 1_794_600_000)
        .await
        .unwrap();
    assert_eq!(
        points
            .iter()
            .map(|point| point.price.to_string())
            .collect::<Vec<_>>(),
        ["1.25", "1.5"]
    );
    assert!(
        server.lock().unwrap()[0]
            .starts_with("GET /v1/history/crypto/GRAM?res=1h&from=1793995200&base=EUR ")
    );
    assert!(matches!(
        client.history(&gram, "EUR", Span::Day, 1_794_600_000).await,
        Err(MarketError::Api { .. })
    ));
    assert!(matches!(
        client.history(&gram, "eur", Span::Day, 1_794_600_000).await,
        Err(MarketError::Invalid(_))
    ));
}

#[tokio::test]
#[ignore = "talks to api.bahaa.io"]
async fn live_bahaa_api() {
    let client = RestClient::new(&Url::parse("https://api.bahaa.io/").unwrap()).unwrap();
    let wanted: BTreeSet<PriceId> = ["crypto/BNB", "crypto/GRAM", "crypto/USDT", "crypto/POLYG"]
        .iter()
        .map(|id| id.parse().unwrap())
        .collect();
    let Fetch::Fresh { quotes, etag } = client.fetch(&wanted, "USDT", None).await.unwrap() else {
        panic!("expected fresh quotes");
    };
    assert_eq!(quotes.len(), 4);
    assert!(quotes.values().all(|quote| !quote.price.is_zero()));
    let again = client
        .fetch(&wanted, "USDT", etag.as_deref())
        .await
        .unwrap();
    assert!(matches!(again, Fetch::NotModified | Fetch::Fresh { .. }));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let gram: PriceId = "crypto/GRAM".parse().unwrap();
    for span in Span::ALL {
        let points = client.history(&gram, "USDT", span, now).await.unwrap();
        assert!(points.len() > 20, "{span:?} gave {}", points.len());
    }
}
