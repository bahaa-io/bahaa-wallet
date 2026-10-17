#![allow(clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::time::Duration;

use bahaa_market::{Market, MarketConfig, Request, Snapshot, Status};
use bahaa_types::PriceId;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use url::Url;

type Socket = WebSocketStream<TcpStream>;

fn ids(list: &[&str]) -> BTreeSet<PriceId> {
    list.iter().map(|id| id.parse().unwrap()).collect()
}

fn request(list: &[&str], base: &str) -> Request {
    Request {
        wanted: ids(list),
        base: base.to_owned(),
    }
}

fn item(symbol: &str, base: &str, price: &str) -> Value {
    serde_json::from_str(&format!(
        r#"{{"category": "crypto", "symbol": "{symbol}", "prices": {{"{base}": {price}}}, "updated_at_unix": 1792250000}}"#
    ))
    .unwrap()
}

fn config(stream: &str, api: &str) -> MarketConfig {
    MarketConfig {
        api: Url::parse(api).unwrap(),
        stream: Url::parse(stream).unwrap(),
        poll_every: Duration::from_millis(100),
        backoff_start: Duration::from_millis(50),
        backoff_max: Duration::from_millis(200),
        silence_limit: Duration::from_secs(5),
        connect_timeout: Duration::from_secs(2),
    }
}

async fn listen() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    (listener, format!("ws://{address}/"))
}

async fn accept(listener: &TcpListener) -> Socket {
    let (stream, _) = listener.accept().await.unwrap();
    tokio_tungstenite::accept_async(stream).await.unwrap()
}

async fn receive(socket: &mut Socket) -> Value {
    loop {
        if let Message::Text(text) = socket.next().await.unwrap().unwrap() {
            return serde_json::from_str(&text).unwrap();
        }
    }
}

async fn send(socket: &mut Socket, value: Value) {
    socket.send(Message::text(value.to_string())).await.unwrap();
}

async fn until(
    snapshots: &mut watch::Receiver<Snapshot>,
    done: impl Fn(&Snapshot) -> bool,
) -> Snapshot {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            {
                let current = snapshots.borrow_and_update();
                if done(&current) {
                    return current.clone();
                }
            }
            snapshots.changed().await.unwrap();
        }
    })
    .await
    .unwrap()
}

fn price(snapshot: &Snapshot, id: &str) -> Option<String> {
    snapshot
        .quotes
        .get(&id.parse().unwrap())
        .map(|quote| quote.price.to_string())
}

#[tokio::test]
async fn streams_a_snapshot_then_updates() {
    let (listener, url) = listen().await;
    let market = Market::spawn(config(&url, "http://127.0.0.1:9/")).unwrap();
    let mut snapshots = market.subscribe();
    market.want(request(&["crypto/BNB", "crypto/GRAM"], "USDT"));
    let mut server = accept(&listener).await;
    let subscribe = receive(&mut server).await;
    assert_eq!(subscribe["type"], "subscribe");
    assert_eq!(subscribe["bases"], json!(["USDT"]));
    assert_eq!(
        subscribe["condition"]["conditions"][0]["conditions"][1]["right"],
        json!(["BNB", "GRAM"])
    );
    send(
        &mut server,
        json!({"type": "subscribed", "id": "wallet", "count": 2}),
    )
    .await;
    send(&mut server, json!({"type": "snapshot", "id": "wallet", "result": [item("BNB", "USDT", "714.3"), item("GRAM", "USDT", "1.3465")]})).await;
    let live = until(&mut snapshots, |s| s.status == Status::Live).await;
    assert_eq!(price(&live, "crypto/GRAM").as_deref(), Some("1.3465"));
    send(
        &mut server,
        json!({"type": "rates", "id": "wallet", "result": [item("BNB", "USDT", "715.01")]}),
    )
    .await;
    let updated = until(&mut snapshots, |s| {
        price(s, "crypto/BNB").as_deref() == Some("715.01")
    })
    .await;
    assert_eq!(price(&updated, "crypto/GRAM").as_deref(), Some("1.3465"));
    send(&mut server, json!({"type": "removed", "id": "wallet", "result": [{"category": "crypto", "symbol": "GRAM"}]})).await;
    until(&mut snapshots, |s| price(s, "crypto/GRAM").is_none()).await;
}

#[tokio::test]
async fn answers_the_server_ping() {
    let (listener, url) = listen().await;
    let market = Market::spawn(config(&url, "http://127.0.0.1:9/")).unwrap();
    market.want(request(&["crypto/BNB"], "USDT"));
    let mut server = accept(&listener).await;
    receive(&mut server).await;
    send(&mut server, json!({"type": "ping"})).await;
    assert_eq!(receive(&mut server).await, json!({"type": "pong"}));
}

#[tokio::test]
async fn resubscribes_when_the_held_assets_or_base_change() {
    let (listener, url) = listen().await;
    let market = Market::spawn(config(&url, "http://127.0.0.1:9/")).unwrap();
    let mut snapshots = market.subscribe();
    market.want(request(&["crypto/BNB", "crypto/GRAM"], "USDT"));
    let mut server = accept(&listener).await;
    receive(&mut server).await;
    send(&mut server, json!({"type": "snapshot", "id": "wallet", "result": [item("BNB", "USDT", "714.3"), item("GRAM", "USDT", "1.3465")]})).await;
    until(&mut snapshots, |s| s.quotes.len() == 2).await;
    market.want(request(&["crypto/BNB"], "USDT"));
    assert_eq!(
        receive(&mut server).await,
        json!({"type": "unsubscribe", "id": "wallet"})
    );
    let again = receive(&mut server).await;
    assert_eq!(
        again["condition"]["conditions"][0]["conditions"][1]["right"],
        json!(["BNB"])
    );
    let trimmed = until(&mut snapshots, |s| s.quotes.len() == 1).await;
    assert!(price(&trimmed, "crypto/BNB").is_some());
    market.want(request(&["crypto/BNB"], "IRT"));
    receive(&mut server).await;
    let toman = receive(&mut server).await;
    assert_eq!(toman["bases"], json!(["IRT"]));
    let cleared = until(&mut snapshots, |s| s.base == "IRT").await;
    assert!(cleared.quotes.is_empty());
}

#[tokio::test]
async fn falls_back_to_polling_then_returns_to_live() {
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api = format!("http://{}/", http.local_addr().unwrap());
    tokio::spawn(async move {
        let body = json!({"ok": true, "result": [item("BNB", "USDT", "700")]}).to_string();
        loop {
            let (mut socket, _) = http.accept().await.unwrap();
            let mut buffer = [0u8; 8192];
            let _ = socket.read(&mut buffer).await;
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(reply.as_bytes()).await;
            let _ = socket.shutdown().await;
        }
    });
    let (listener, url) = listen().await;
    let market = Market::spawn(config(&url, &api)).unwrap();
    let mut snapshots = market.subscribe();
    market.want(request(&["crypto/BNB"], "USDT"));
    let refused = accept(&listener).await;
    drop(refused);
    let polled = until(&mut snapshots, |s| s.status == Status::Polling).await;
    assert_eq!(price(&polled, "crypto/BNB").as_deref(), Some("700"));
    let mut server = accept(&listener).await;
    receive(&mut server).await;
    send(
        &mut server,
        json!({"type": "snapshot", "id": "wallet", "result": [item("BNB", "USDT", "714.3")]}),
    )
    .await;
    let live = until(&mut snapshots, |s| s.status == Status::Live).await;
    assert_eq!(price(&live, "crypto/BNB").as_deref(), Some("714.3"));
}

#[tokio::test]
async fn an_error_frame_ends_the_stream_and_marks_it_offline() {
    let (listener, url) = listen().await;
    let market = Market::spawn(config(&url, "http://127.0.0.1:9/")).unwrap();
    let mut snapshots = market.subscribe();
    market.want(request(&["crypto/BNB"], "USDT"));
    let mut server = accept(&listener).await;
    receive(&mut server).await;
    send(
        &mut server,
        json!({"type": "snapshot", "id": "wallet", "result": [item("BNB", "USDT", "714.3")]}),
    )
    .await;
    until(&mut snapshots, |s| s.status == Status::Live).await;
    send(
        &mut server,
        json!({"type": "error", "id": "wallet", "code": "BAD_CONDITION"}),
    )
    .await;
    let offline = until(&mut snapshots, |s| s.status == Status::Offline).await;
    assert_eq!(price(&offline, "crypto/BNB").as_deref(), Some("714.3"));
}

#[tokio::test]
async fn stays_idle_and_offline_from_the_network_when_nothing_is_held() {
    let (listener, url) = listen().await;
    let market = Market::spawn(config(&url, "http://127.0.0.1:9/")).unwrap();
    market.want(request(&[], "USDT"));
    let attempt = tokio::time::timeout(Duration::from_millis(300), listener.accept()).await;
    assert!(attempt.is_err());
    assert_eq!(market.current().status, Status::Idle);
}

#[tokio::test]
#[ignore = "talks to api.bahaa.io"]
async fn live_bahaa_stream() {
    let market = Market::spawn(MarketConfig::bahaa().unwrap()).unwrap();
    let mut snapshots = market.subscribe();
    market.want(request(&["crypto/BNB", "crypto/GRAM"], "USDT"));
    let live = until(&mut snapshots, |s| {
        s.status == Status::Live && s.quotes.len() == 2
    })
    .await;
    assert!(live.quotes.values().all(|quote| !quote.price.is_zero()));
}
