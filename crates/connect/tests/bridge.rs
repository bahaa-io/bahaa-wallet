#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bahaa_connect::{Bridge, SessionKey, read_request};
use bahaa_testkit::{Reply, serve};
use bahaa_types::hex;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use tokio::sync::mpsc;
use url::Url;

type Posted = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

fn sse(body: String) -> Reply {
    Reply {
        status: 200,
        headers: vec![("Content-Type", "text/event-stream".to_owned())],
        body: body.into_bytes(),
        delay: Duration::ZERO,
    }
}

#[tokio::test]
async fn messages_go_out_sealed_and_come_back_opened() {
    let wallet = SessionKey::generate().unwrap();
    let app = SessionKey::generate().unwrap();
    let request = br#"{"method":"disconnect","params":[],"id":"3"}"#;
    let event = format!(
        "event: heartbeat\n\nevent: message\nid: 77\ndata: {{\"from\":\"{}\",\"message\":\"{}\"}}\n\n",
        hex::encode(&app.id()),
        STANDARD.encode(app.seal(&wallet.id(), request).unwrap())
    );
    let posted: Posted = Arc::default();
    let log = Arc::clone(&posted);
    let url = serve(move |request| {
        if request.path().contains("/events") {
            sse(event.clone())
        } else {
            log.lock()
                .unwrap()
                .push((request.path().to_owned(), request.body.clone()));
            Reply::json(
                200,
                &serde_json::json!({"message": "OK", "statusCode": 200}),
            )
        }
    })
    .await;
    let bridge = Bridge::new(Url::parse(&format!("{url}bridge")).unwrap()).unwrap();
    let (sink, mut found) = mpsc::channel(4);
    bridge
        .listen(&[wallet.id()], Some("70"), &sink)
        .await
        .unwrap();
    let incoming = found.try_recv().unwrap();
    assert_eq!(incoming.event.as_deref(), Some("77"));
    assert_eq!(incoming.from, app.id());
    let opened = wallet.open(&incoming.from, &incoming.sealed).unwrap();
    assert_eq!(read_request(&opened).unwrap().id, 3);
    assert!(found.try_recv().is_err());
    bridge
        .send(
            &wallet,
            &app.id(),
            b"{\"event\":\"disconnect\"}",
            Some("disconnect"),
            Some("01a1"),
        )
        .await
        .unwrap();
    let (path, body) = posted.lock().unwrap()[0].clone();
    assert!(path.starts_with(&format!(
        "/bridge/message?client_id={}&to={}&ttl=300&topic=disconnect&trace_id=01a1",
        hex::encode(&wallet.id()),
        hex::encode(&app.id())
    )));
    let sealed = STANDARD.decode(body).unwrap();
    assert_eq!(
        app.open(&wallet.id(), &sealed).unwrap(),
        b"{\"event\":\"disconnect\"}"
    );
}

#[tokio::test]
async fn refusals_are_reported() {
    let url = serve(|_| Reply::json(429, &serde_json::json!({"message": "slow down"}))).await;
    let bridge = Bridge::new(url).unwrap();
    let key = SessionKey::generate().unwrap();
    let (sink, _found) = mpsc::channel(1);
    assert!(matches!(
        bridge.listen(&[key.id()], None, &sink).await,
        Err(bahaa_connect::ConnectError::Refused(429))
    ));
    assert!(
        bridge
            .send(&key, &key.id(), b"x", None, None)
            .await
            .is_err()
    );
}

#[tokio::test]
#[ignore = "talks to connect.ton.org"]
async fn live_bridge_round_trip() {
    let bridge = Bridge::new(Url::parse("https://connect.ton.org/bridge").unwrap()).unwrap();
    let wallet = SessionKey::generate().unwrap();
    let app = SessionKey::generate().unwrap();
    let (sink, mut found) = mpsc::channel(4);
    let listener = bridge.clone();
    let ids = [wallet.id()];
    let task = tokio::spawn(async move { listener.listen(&ids, None, &sink).await });
    tokio::time::sleep(Duration::from_secs(2)).await;
    bridge
        .send(&app, &wallet.id(), b"ping", None, None)
        .await
        .unwrap();
    let incoming = tokio::time::timeout(Duration::from_secs(10), found.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        wallet.open(&incoming.from, &incoming.sealed).unwrap(),
        b"ping"
    );
    task.abort();
}
