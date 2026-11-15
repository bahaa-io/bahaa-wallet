use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bahaa_connect::SessionKey;
use bahaa_types::hex;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};
use url::Url;

use crate::{Reply, serve};

#[derive(Clone, Default)]
pub struct Relay(Arc<Mutex<Kept>>);

#[derive(Default)]
struct Kept {
    next: u64,
    messages: Vec<Message>,
}

struct Message {
    id: u64,
    from: String,
    to: String,
    body: String,
}

impl Relay {
    pub fn keep(&self, from: String, to: String, body: String) {
        let mut kept = self.0.lock().unwrap();
        kept.next += 1;
        let id = kept.next;
        kept.messages.push(Message { id, from, to, body });
    }

    fn after(&self, seen: u64, to: &str) -> Option<(u64, String, String)> {
        self.0
            .lock()
            .unwrap()
            .messages
            .iter()
            .find(|message| message.id > seen && message.to == to)
            .map(|message| (message.id, message.from.clone(), message.body.clone()))
    }

    fn stream(&self, after: u64, ids: &[&str]) -> String {
        let mut body = String::new();
        for message in &self.0.lock().unwrap().messages {
            if message.id > after && ids.contains(&message.to.as_str()) {
                let _ = write!(
                    body,
                    "event: message\nid: {}\ndata: {{\"from\":\"{}\",\"message\":\"{}\"}}\n\n",
                    message.id, message.from, message.body
                );
            }
        }
        body
    }
}

pub async fn bridge(relay: Relay) -> Url {
    let url = serve(move |request| {
        let url = Url::parse(&format!("http://bridge{}", request.path())).unwrap();
        let query: HashMap<String, String> = url.query_pairs().into_owned().collect();
        if url.path().ends_with("/message") {
            relay.keep(
                query["client_id"].clone(),
                query["to"].clone(),
                String::from_utf8(request.body).unwrap(),
            );
            return Reply::json(200, &json!({"message": "OK", "statusCode": 200}));
        }
        let ids: Vec<&str> = query["client_id"].split(',').collect();
        let after = query
            .get("last_event_id")
            .and_then(|id| id.parse().ok())
            .unwrap_or(0);
        let body = relay.stream(after, &ids);
        let quiet = body.is_empty();
        Reply {
            status: 200,
            headers: vec![("Content-Type", "text/event-stream".to_owned())],
            body: body.into_bytes(),
            delay: Duration::from_millis(if quiet { 200 } else { 0 }),
        }
    })
    .await;
    url.join("bridge").unwrap()
}

pub async fn manifest(name: &str) -> Url {
    let details = json!({
        "url": "https://demo.example.org",
        "name": name,
        "iconUrl": "https://demo.example.org/icon.png",
    });
    let url = serve(move |_| Reply::json(200, &details)).await;
    url.join("tonconnect-manifest.json").unwrap()
}

pub struct Dapp {
    key: SessionKey,
    relay: Relay,
    wallet: Option<[u8; 32]>,
    seen: u64,
}

impl Dapp {
    pub fn new(relay: &Relay) -> Self {
        Self {
            key: SessionKey::generate().unwrap(),
            relay: relay.clone(),
            wallet: None,
            seen: 0,
        }
    }

    pub fn link(&self, manifest: &Url, proof: bool) -> String {
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

    pub async fn next(&mut self) -> Value {
        let me = hex::encode(&self.key.id());
        let (id, from, body) = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(found) = self.relay.after(self.seen, &me) {
                    return found;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        self.seen = id;
        let wallet = hex::decode_array::<32>(&from).unwrap();
        self.wallet.get_or_insert(wallet);
        let opened = self
            .key
            .open(&wallet, &STANDARD.decode(body).unwrap())
            .unwrap();
        serde_json::from_slice(&opened).unwrap()
    }

    pub fn send(&self, method: &str, params: &Value, id: u64) {
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
        self.relay.keep(
            hex::encode(&self.key.id()),
            hex::encode(&wallet),
            STANDARD.encode(sealed),
        );
    }
}
