use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use bahaa_config::{ChainKind, Endpoint, Feature, Network, Protocol, Registry};
use serde_json::{Value, json};
use sha3::{Digest, Keccak256};
use url::Url;

pub struct Anvil {
    child: Child,
    url: Url,
    network: Network,
    http: reqwest::Client,
}

impl Anvil {
    pub fn start(network: &str, extra: &[&str]) -> Self {
        let binary = std::env::var("BAHAA_ANVIL").unwrap();
        let registry = Registry::builtin().unwrap();
        let mut network = registry.get(&network.parse().unwrap()).unwrap().clone();
        let ChainKind::Evm { chain_id, .. } = network.kind else {
            panic!("not an EVM network")
        };
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let child = Command::new(binary)
            .args(["--chain-id", &chain_id.to_string()])
            .args(["--port", &port.to_string(), "--silent"])
            .args(extra)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let url = Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap();
        network.endpoints = vec![Endpoint {
            protocol: Protocol::JsonRpc,
            url: url.clone(),
            features: vec![Feature::Logs],
            min_gap_ms: 0,
            history_gap_ms: 0,
        }];
        Self {
            child,
            url,
            network,
            http: reqwest::Client::new(),
        }
    }

    pub fn url(&self) -> &Url {
        &self.url
    }

    pub fn network(&self) -> Network {
        self.network.clone()
    }

    pub async fn ready(&self) {
        for _ in 0..300 {
            if self.rpc("eth_chainId", json!([])).await.is_some() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("anvil did not start");
    }

    pub async fn rpc(&self, method: &str, params: Value) -> Option<Value> {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let reply: Value = self
            .http
            .post(self.url.clone())
            .json(&body)
            .send()
            .await
            .ok()?
            .json()
            .await
            .ok()?;
        reply.get("result").cloned()
    }

    pub async fn fund_token(&self, token: &str, owner: &[u8; 20], units: &str) {
        let wanted = format!("0x{units:0>64}");
        for slot in 0u8..20 {
            let mut key = [0u8; 64];
            key[12..32].copy_from_slice(owner);
            key[63] = slot;
            let storage = format!("0x{}", bahaa_types::hex::encode(&Keccak256::digest(key)));
            self.rpc("anvil_setStorageAt", json!([token, storage, wanted]))
                .await
                .unwrap();
            let data = format!("0x70a08231{:0>64}", bahaa_types::hex::encode(owner));
            let held = self
                .rpc("eth_call", json!([{"to": token, "data": data}, "latest"]))
                .await;
            if held.as_ref().and_then(Value::as_str) == Some(wanted.as_str()) {
                return;
            }
        }
        panic!("no balance slot found for {token}");
    }
}

impl Drop for Anvil {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
