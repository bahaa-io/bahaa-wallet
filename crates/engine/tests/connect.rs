#![allow(clippy::unwrap_used)]

mod common;

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bahaa_chain::ChainError;
use bahaa_config::{Network, Registry};
use bahaa_connect::{Bridges, Content, SessionKey, data_hash, proof_hash};
use bahaa_crypto::SecretString;
use bahaa_engine::{Approved, AskKind, AskReview, Connections, Engine, EngineError, KeyringError};
use bahaa_testkit::{Reply, serve};
use bahaa_types::{Address, hex};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use common::{Asked, HARDHAT, Scratch, config, until};
use ed25519_dalek::{Signature, VerifyingKey};
use serde_json::{Value, json};
use url::Url;

const MAINNET: &str = "17a3a92992aabea785a7a090985a265cd31f323d849da51239737e321fb05569";
const RECORDING: &str = include_str!("fixtures/ton.json");
const ELSEWHERE: &str = "UQCi1ivY6QriyKIQZrSTwhvf4qXI19qIPe4XAOYo-qMMUEiR";

#[derive(Default)]
struct Relay {
    next: u64,
    kept: Vec<Kept>,
}

struct Kept {
    id: u64,
    from: String,
    to: String,
    message: String,
}

type Shared = Arc<Mutex<Relay>>;

fn keep(relay: &Shared, from: String, to: String, message: String) {
    let mut relay = relay.lock().unwrap();
    relay.next += 1;
    let id = relay.next;
    relay.kept.push(Kept {
        id,
        from,
        to,
        message,
    });
}

async fn bridge(relay: Shared) -> Url {
    serve(move |request| {
        let url = Url::parse(&format!("http://bridge{}", request.path())).unwrap();
        let query: HashMap<String, String> = url.query_pairs().into_owned().collect();
        if url.path().ends_with("/message") {
            keep(
                &relay,
                query["client_id"].clone(),
                query["to"].clone(),
                String::from_utf8(request.body).unwrap(),
            );
            return Reply::json(200, &json!({"message": "OK", "statusCode": 200}));
        }
        let ids: Vec<&str> = query["client_id"].split(',').collect();
        let after: u64 = query
            .get("last_event_id")
            .and_then(|id| id.parse().ok())
            .unwrap_or(0);
        let mut body = String::new();
        for kept in &relay.lock().unwrap().kept {
            if kept.id > after && ids.contains(&kept.to.as_str()) {
                let _ = write!(
                    body,
                    "event: message\nid: {}\ndata: {{\"from\":\"{}\",\"message\":\"{}\"}}\n\n",
                    kept.id, kept.from, kept.message
                );
            }
        }
        let quiet = body.is_empty();
        Reply {
            status: 200,
            headers: vec![("Content-Type", "text/event-stream".to_owned())],
            body: body.into_bytes(),
            delay: Duration::from_millis(if quiet { 200 } else { 0 }),
        }
    })
    .await
}

struct App {
    key: SessionKey,
    relay: Shared,
    wallet: Option<[u8; 32]>,
    seen: u64,
}

impl App {
    fn new(relay: &Shared) -> Self {
        Self {
            key: SessionKey::generate().unwrap(),
            relay: Arc::clone(relay),
            wallet: None,
            seen: 0,
        }
    }

    fn link(&self, manifest: &Url, proof: bool) -> String {
        let mut items = vec![json!({"name": "ton_addr"})];
        if proof {
            items.push(json!({"name": "ton_proof", "payload": "nonce-1"}));
        }
        items.push(json!({"name": "ton_future"}));
        let request = json!({"manifestUrl": manifest.as_str(), "items": items}).to_string();
        format!(
            "tc://?v=2&id={}&r={}",
            hex::encode(&self.key.id()),
            url::form_urlencoded::byte_serialize(request.as_bytes()).collect::<String>()
        )
    }

    async fn next(&mut self) -> Value {
        let me = hex::encode(&self.key.id());
        let kept = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let found = self
                    .relay
                    .lock()
                    .unwrap()
                    .kept
                    .iter()
                    .find(|kept| kept.id > self.seen && kept.to == me)
                    .map(|kept| (kept.id, kept.from.clone(), kept.message.clone()));
                if let Some(found) = found {
                    return found;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let (id, from, message) = kept;
        self.seen = id;
        let wallet = hex::decode_array::<32>(&from).unwrap();
        self.wallet.get_or_insert(wallet);
        let opened = self
            .key
            .open(&wallet, &STANDARD.decode(message).unwrap())
            .unwrap();
        serde_json::from_slice(&opened).unwrap()
    }

    fn send(&self, method: &str, params: &Value, id: u64) {
        let wallet = self.wallet.unwrap();
        let params = if params.is_null() {
            json!([])
        } else {
            json!([params.to_string()])
        };
        let request = json!({"method": method, "params": params, "id": id.to_string()});
        let sealed = self
            .key
            .seal(&wallet, request.to_string().as_bytes())
            .unwrap();
        keep(
            &self.relay,
            hex::encode(&self.key.id()),
            hex::encode(&wallet),
            STANDARD.encode(sealed),
        );
    }
}

async fn manifest() -> Url {
    let url = serve(|_| {
        Reply::json(
            200,
            &json!({"url": "https://demo.example.org", "name": "Demo\u{202e} App", "iconUrl": "https://demo.example.org/icon.png"}),
        )
    })
    .await;
    url.join("tonconnect-manifest.json").unwrap()
}

fn ton_network(url: Url) -> Network {
    let mut network = common::ton_network("ton", url);
    network.tokens.clear();
    network
}

struct Setup {
    engine: Engine,
    relay: Shared,
    manifest: Url,
    _scratch: Scratch,
}

async fn setup() -> Setup {
    let scratch = Scratch::new();
    let asked = Asked::default();
    let registry = Registry::with_networks(vec![ton_network(
        common::liteserver(RECORDING, MAINNET, &asked).await,
    )])
    .unwrap();
    let mut keyring = bahaa_engine::Keyring::new(
        scratch.0.join("data").join("vault.bin"),
        bahaa_vault::KdfParams {
            memory_kib: 64,
            iterations: 1,
            lanes: 1,
        },
    );
    keyring.create(b"pw").unwrap();
    keyring.import(HARDHAT, "").unwrap();
    let relay = Shared::default();
    let mut settings = config(&scratch, registry, None);
    settings.connect = Some(Bridges {
        default: bridge(Arc::clone(&relay)).await.join("bridge").unwrap(),
        list: None,
    });
    let engine = Engine::start(settings).unwrap();
    engine.unlock(SecretString::from("pw")).await.unwrap();
    Setup {
        engine,
        relay,
        manifest: manifest().await,
        _scratch: scratch,
    }
}

async fn connect(setup: &Setup, app: &mut App, proof: bool) -> Value {
    let invite = setup
        .engine
        .connect_app(app.link(&setup.manifest, proof))
        .await
        .unwrap();
    let account = invite.accounts[0].clone();
    setup
        .engine
        .approve_app(invite.id, account, SecretString::from("pw"))
        .await
        .unwrap();
    app.next().await
}

async fn asked(setup: &Setup, count: usize) -> Connections {
    until(&mut setup.engine.connections(), |connections| {
        connections.asks.len() == count
    })
    .await
}

fn verify(key: &VerifyingKey, hash: &[u8; 32], signature: &Value) {
    let signature = STANDARD.decode(signature.as_str().unwrap()).unwrap();
    key.verify_strict(hash, &Signature::from_slice(&signature).unwrap())
        .unwrap();
}

#[tokio::test]
async fn an_app_connects_with_a_signed_proof_and_gets_text_signed() {
    let setup = setup().await;
    let mut app = App::new(&setup.relay);
    let invite = setup
        .engine
        .connect_app(app.link(&setup.manifest, true))
        .await
        .unwrap();
    assert_eq!(invite.app.name, "Demo App");
    assert_eq!(invite.app.domain, "demo.example.org");
    assert!(invite.proof);
    assert_eq!(invite.unsupported, ["ton_future"]);
    assert_eq!(invite.accounts.len(), 1);
    let account = invite.accounts[0].clone();
    assert_eq!(
        setup
            .engine
            .approve_app(invite.id, account.clone(), SecretString::from("nope"))
            .await,
        Err(EngineError::Keyring(KeyringError::WrongPassword))
    );
    let connected = setup
        .engine
        .approve_app(invite.id, account.clone(), SecretString::from("pw"))
        .await
        .unwrap();
    assert_eq!(connected.address, account.address);
    let event = app.next().await;
    assert_eq!(event["event"], "connect");
    assert_eq!(event["id"], 1);
    let items = event["payload"]["items"].as_array().unwrap();
    assert_eq!(items[0]["name"], "ton_addr");
    assert_eq!(items[0]["address"], account.address.to_string());
    assert_eq!(items[0]["network"], "-239");
    assert_eq!(items[2]["error"]["code"], 400);
    assert_eq!(
        event["payload"]["device"]["features"][1]["maxMessages"],
        255
    );
    let public = hex::decode_array::<32>(items[0]["publicKey"].as_str().unwrap()).unwrap();
    let key = VerifyingKey::from_bytes(&public).unwrap();
    let proof = &items[1]["proof"];
    assert_eq!(proof["domain"]["value"], "demo.example.org");
    let hash = proof_hash(
        &account.address,
        "demo.example.org",
        proof["timestamp"].as_u64().unwrap(),
        "nonce-1",
    )
    .unwrap();
    verify(&key, &hash, &proof["signature"]);
    let shown = until(&mut setup.engine.connections(), |connections| {
        connections.apps.len() == 1
    })
    .await;
    assert_eq!(shown.apps[0].app.name, "Demo App");

    app.send(
        "signData",
        &json!({"type": "text", "text": "Sign in to Demo"}),
        1,
    );
    let waiting = asked(&setup, 1).await;
    let ask = waiting.asks[0].clone();
    assert_eq!(ask.kind, AskKind::SignData);
    let Ok(AskReview::Data(review)) = setup.engine.review_request(ask.id).await else {
        panic!("a text to sign is shown as it is");
    };
    assert_eq!(review.content, Content::Text("Sign in to Demo".to_owned()));
    assert_eq!(
        setup
            .engine
            .approve_request(ask.id, SecretString::from("nope"))
            .await,
        Err(EngineError::Keyring(KeyringError::WrongPassword))
    );
    assert_eq!(
        setup
            .engine
            .approve_request(ask.id, SecretString::from("pw"))
            .await,
        Ok(Approved::Signed)
    );
    let answer = app.next().await;
    assert_eq!(answer["id"], "1");
    let result = &answer["result"];
    assert_eq!(result["domain"], "demo.example.org");
    let hash = data_hash(
        &account.address,
        "demo.example.org",
        result["timestamp"].as_u64().unwrap(),
        &Content::Text("Sign in to Demo".to_owned()),
    )
    .unwrap();
    verify(&key, &hash, &result["signature"]);
    asked(&setup, 0).await;
}

#[tokio::test]
async fn foreign_replayed_and_declined_requests_are_answered_and_the_app_can_leave() {
    let setup = setup().await;
    let mut app = App::new(&setup.relay);
    connect(&setup, &mut app, false).await;
    let message = json!([{"address": ELSEWHERE, "amount": "1000"}]);
    app.send(
        "sendTransaction",
        &json!({"messages": message, "from": ELSEWHERE}),
        1,
    );
    let refused = app.next().await;
    assert_eq!(refused["id"], "1");
    assert_eq!(refused["error"]["code"], 1);
    app.send(
        "sendTransaction",
        &json!({"messages": message, "network": "-3"}),
        2,
    );
    assert_eq!(app.next().await["error"]["code"], 1);
    app.send("transferAll", &json!({}), 3);
    assert_eq!(app.next().await["error"]["code"], 400);

    app.send("signData", &json!({"type": "text", "text": "one"}), 4);
    let ask = asked(&setup, 1).await.asks[0].clone();
    setup.engine.reject_request(ask.id).await.unwrap();
    let declined = app.next().await;
    assert_eq!(
        (declined["id"].clone(), declined["error"]["code"].clone()),
        (json!("4"), json!(300))
    );
    assert_eq!(
        setup.engine.reject_request(ask.id).await,
        Err(EngineError::UnknownRequest)
    );

    app.send("signData", &json!({"type": "text", "text": "replayed"}), 4);
    app.send("signData", &json!({"type": "text", "text": "two"}), 5);
    let waiting = asked(&setup, 1).await;
    let Ok(AskReview::Data(review)) = setup.engine.review_request(waiting.asks[0].id).await else {
        panic!("the newer request waits");
    };
    assert_eq!(review.content, Content::Text("two".to_owned()));

    app.send("disconnect", &Value::Null, 6);
    let ended = app.next().await;
    assert_eq!(ended, json!({"result": {}, "id": "6"}));
    let gone = until(&mut setup.engine.connections(), |connections| {
        connections.apps.is_empty()
    })
    .await;
    assert_eq!(gone.asks.len(), 0);
}

#[tokio::test]
async fn sessions_survive_a_lock_and_relayed_messages_are_signed_after_review() {
    let setup = setup().await;
    let mut app = App::new(&setup.relay);
    connect(&setup, &mut app, false).await;
    app.send("signData", &json!({"type": "binary", "bytes": "AQID"}), 1);
    asked(&setup, 1).await;
    setup.engine.lock().await.unwrap();
    let dropped = app.next().await;
    assert_eq!(dropped["error"]["code"], 300);
    assert_eq!(*setup.engine.connections().borrow(), Connections::default());
    setup.engine.unlock(SecretString::from("pw")).await.unwrap();
    let back = until(&mut setup.engine.connections(), |connections| {
        connections.apps.len() == 1
    })
    .await;

    let messages = json!([{"address": ELSEWHERE, "amount": "10000000"}]);
    app.send("signMessage", &json!({"messages": messages}), 2);
    let ask = asked(&setup, 1).await.asks[0].clone();
    assert_eq!(ask.kind, AskKind::SignMessage);
    assert_eq!(
        setup
            .engine
            .approve_request(ask.id, SecretString::from("pw"))
            .await,
        Err(EngineError::NotReviewed)
    );
    let Ok(AskReview::Transaction(review)) = setup.engine.review_request(ask.id).await else {
        panic!("a message for a relayer is reviewed like a transaction");
    };
    assert_eq!(review.fee, None);
    assert_eq!(review.readings.len(), 1);
    assert_eq!(
        review.readings[0].to,
        Address::parse_ton(ELSEWHERE).unwrap().0
    );
    assert_eq!(review.readings[0].amount.to_string(), "0.01");
    assert_eq!(
        setup
            .engine
            .approve_request(ask.id, SecretString::from("pw"))
            .await,
        Ok(Approved::Signed)
    );
    let signed = app.next().await;
    assert_eq!(signed["id"], "2");
    let internal = STANDARD
        .decode(signed["result"]["internalBoc"].as_str().unwrap())
        .unwrap();
    assert_eq!(internal[..4], [0xb5, 0xee, 0x9c, 0x72]);

    app.send("sendTransaction", &json!({"messages": messages}), 3);
    let ask = asked(&setup, 1).await.asks[0].clone();
    assert_eq!(ask.kind, AskKind::Send);
    let reviewed = setup.engine.review_request(ask.id).await;
    assert!(
        matches!(
            reviewed,
            Err(EngineError::Chain(
                ChainError::Insufficient { .. } | ChainError::Rejected(_)
            ))
        ),
        "{reviewed:?}"
    );

    setup
        .engine
        .disconnect_app(back.apps[0].id.clone())
        .await
        .unwrap();
    let gone = app.next().await;
    assert_eq!(gone["event"], "disconnect");
    assert_eq!(gone["id"], 2);
    assert_eq!(setup.engine.connections().borrow().apps.len(), 0);
    assert_eq!(
        setup.engine.disconnect_app(back.apps[0].id.clone()).await,
        Err(EngineError::UnknownApp)
    );
}
