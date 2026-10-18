#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use bahaa_chain::ChainError;
use bahaa_config::{Endpoint, Network, Protocol, Registry};
use bahaa_ton::TonChain;
use bahaa_types::{Address, AssetId, ChainFamily};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tonlib_core::cell::BagOfCells;
use url::Url;

type Log = Arc<Mutex<Vec<String>>>;

const ZERO_STATE: &str = "17a3a92992aabea785a7a090985a265cd31f323d849da51239737e321fb05569";
const TESTNET_ZERO_STATE: &str = "823f81f306ff02694f935cf5021548e3ce2b86b529812af6a12148879e95a128";
const USDT: &str = "0:b113a994b5024a16719f69139328eb759596c38a25f59028b146fecdc3621dfe";
const HOLDER: &str = "0:f8b8635d0786b77f9da660e65fd31d670a675dd4302facba692f46b571eb5360";
const JETTON_WALLET: &str = "0:7f024f7ec0cc57b87eaf017046adaf9f6c521afb809a159cb2a2194f4153356f";
const HOLDER_CELL: &str = "b5ee9c72010101010024000043801f170c6ba0f0d6eff3b4cc1ccbfa63ace14cebba8605f5974d25e8d6ae3d6a6c10";
const USDT_CELL: &str = "b5ee9c72010101010024000043801622753296a04942ce33ed2272651d6eb2b2d87144beb2051628dfd9b86c43bfd0";
const WALLET_CELL: &str = "b5ee9c72010101010024000043800fe049efd8198af70fd5e02e08d5b5f3ed8a435f701342b396544329e82a66adf0";
const CODE_CELL: &str =
    "b5ee9c72010101010023000842028f452d7a4dfd74066b682365177259ed05734435be76b5fd4bd5d8af2b7c3d68";

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
        .collect()
}

fn base64_of(hex: &str) -> String {
    STANDARD.encode(unhex(hex))
}

async fn node(
    log: Log,
    reply: impl Fn(&str, &Value) -> (u16, Value) + Send + Sync + 'static,
) -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let reply = Arc::new(reply);
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let (log, reply) = (Arc::clone(&log), Arc::clone(&reply));
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buffer = [0u8; 8192];
                let (path, body) = loop {
                    let read = socket.read(&mut buffer).await.unwrap_or(0);
                    if read == 0 {
                        return;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    let text = String::from_utf8_lossy(&request).to_string();
                    let Some(end) = text.find("\r\n\r\n") else {
                        continue;
                    };
                    let length = text
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .map(str::to_owned)
                        })
                        .map_or(0, |value| value.trim().parse::<usize>().unwrap());
                    if request.len() >= end + 4 + length {
                        let path = text.split(' ').nth(1).unwrap().to_owned();
                        break (path, request[end + 4..end + 4 + length].to_vec());
                    }
                };
                let body = if body.is_empty() {
                    Value::Null
                } else {
                    serde_json::from_slice(&body).unwrap()
                };
                log.lock().unwrap().push(path.clone());
                let (status, answer) = reply(&path, &body);
                let text = answer.to_string();
                let response = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                    text.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    url
}

fn ton_on(endpoints: &[(Protocol, &Url)]) -> Network {
    let registry = Registry::builtin().unwrap();
    let mut network = registry.get(&"ton".parse().unwrap()).unwrap().clone();
    network.endpoints = endpoints
        .iter()
        .map(|(protocol, url)| Endpoint {
            protocol: *protocol,
            url: (*url).clone(),
            features: Vec::new(),
            min_gap_ms: 0,
        })
        .collect();
    network
}

fn holder() -> Address {
    Address::parse(ChainFamily::Ton, HOLDER).unwrap()
}

fn assets() -> Vec<AssetId> {
    let master = Address::parse(ChainFamily::Ton, USDT).unwrap();
    vec![
        "ton".parse().unwrap(),
        AssetId::Token("ton".parse().unwrap(), master),
    ]
}

fn amounts(balances: &[bahaa_chain::Balance]) -> Vec<String> {
    balances
        .iter()
        .map(|balance| balance.amount.to_string())
        .collect()
}

fn slice_address(stack: &Value) -> String {
    let boc = STANDARD.decode(stack[0][1].as_str().unwrap()).unwrap();
    let root = BagOfCells::parse(&boc).unwrap().single_root().unwrap();
    let address = root.parser().load_address().unwrap();
    format!("{}:{}", address.workchain, address.hash_part.to_hex())
}

struct Center {
    zero_state: &'static str,
    deployed: bool,
    owner_cell: &'static str,
    runs: Mutex<Vec<Value>>,
}

impl Center {
    fn new(zero_state: &'static str) -> Arc<Self> {
        Arc::new(Self {
            zero_state,
            deployed: true,
            owner_cell: HOLDER_CELL,
            runs: Mutex::new(Vec::new()),
        })
    }

    fn answer(&self, path: &str, body: &Value) -> (u16, Value) {
        let ok = |result: Value| (200, json!({"ok": true, "result": result}));
        if path == "/v2/getMasterchainInfo" {
            return ok(json!({"init": {"root_hash": base64_of(self.zero_state)}}));
        }
        if path.starts_with("/v2/getAddressBalance?") {
            return ok(json!("1561453563"));
        }
        if path != "/v2/runGetMethod" {
            return (404, json!({"ok": false, "error": "not found"}));
        }
        self.runs.lock().unwrap().push(body.clone());
        let cell = |hex: &str| json!(["cell", {"bytes": base64_of(hex)}]);
        match (
            body["address"].as_str().unwrap(),
            body["method"].as_str().unwrap(),
        ) {
            (USDT, "get_wallet_address") => {
                ok(json!({"exit_code": 0, "stack": [cell(WALLET_CELL)]}))
            }
            (JETTON_WALLET, "get_wallet_data") if self.deployed => ok(json!({
                "exit_code": 0,
                "stack": [["num", "0x56110d7a1790"], cell(self.owner_cell), cell(USDT_CELL), cell(CODE_CELL)]
            })),
            (JETTON_WALLET, "get_wallet_data") => {
                ok(json!({"exit_code": -13, "stack": [["num", "0x17b02"]]}))
            }
            _ => ok(json!({"exit_code": 11, "stack": []})),
        }
    }
}

async fn serve(center: &Arc<Center>, log: Log) -> Url {
    let center = Arc::clone(center);
    node(log, move |path, body| center.answer(path, body)).await
}

#[tokio::test]
async fn reads_gram_and_usdt_through_ton_center_and_caches_the_token_wallet() {
    let (center, log) = (Center::new(ZERO_STATE), Log::default());
    let url = serve(&center, Arc::clone(&log)).await;
    let chain = TonChain::new(ton_on(&[(Protocol::TonCenter, &url)])).unwrap();
    for _ in 0..2 {
        let balances = chain.balances(&holder(), &assets()).await.unwrap();
        assert_eq!(amounts(&balances), ["1.561453563", "94631240.538"]);
    }
    let runs = center.runs.lock().unwrap().clone();
    let methods: Vec<&str> = runs
        .iter()
        .map(|run| run["method"].as_str().unwrap())
        .collect();
    assert_eq!(
        methods,
        ["get_wallet_address", "get_wallet_data", "get_wallet_data"]
    );
    assert_eq!(slice_address(&runs[0]["stack"]), HOLDER);
    assert_eq!(runs[1]["stack"], json!([]));
    let identity = log
        .lock()
        .unwrap()
        .iter()
        .filter(|path| *path == "/v2/getMasterchainInfo")
        .count();
    assert_eq!(identity, 1);
    assert!(log.lock().unwrap().contains(&format!(
        "/v2/getAddressBalance?address={}",
        HOLDER.replace(':', "%3A")
    )));
}

#[tokio::test]
async fn an_undeployed_token_wallet_holds_nothing_and_a_foreign_one_is_refused() {
    let mut fresh = Center::new(ZERO_STATE);
    Arc::get_mut(&mut fresh).unwrap().deployed = false;
    let url = serve(&fresh, Log::default()).await;
    let chain = TonChain::new(ton_on(&[(Protocol::TonCenter, &url)])).unwrap();
    let balances = chain.balances(&holder(), &assets()).await.unwrap();
    assert_eq!(amounts(&balances), ["1.561453563", "0"]);
    let mut foreign = Center::new(ZERO_STATE);
    Arc::get_mut(&mut foreign).unwrap().owner_cell = USDT_CELL;
    let url = serve(&foreign, Log::default()).await;
    let chain = TonChain::new(ton_on(&[(Protocol::TonCenter, &url)])).unwrap();
    assert!(matches!(
        chain.balances(&holder(), &assets()).await,
        Err(ChainError::Network(message)) if message.contains("does not belong")
    ));
}

fn tonapi(path: &str, deployed: bool) -> (u16, Value) {
    let cell = |hex: &str| json!({"type": "cell", "cell": hex});
    if path == "/liteserver/get_masterchain_info" {
        return (200, json!({"init": {"root_hash": ZERO_STATE}}));
    }
    if path == format!("/accounts/{HOLDER}") {
        return (
            200,
            json!({"balance": 59_570_010_146_444_835_u64, "status": "active"}),
        );
    }
    if path
        == format!(
            "/blockchain/accounts/{USDT}/methods/get_wallet_address?args={}",
            HOLDER.replace(':', "%3A")
        )
    {
        return (
            200,
            json!({"success": true, "exit_code": 0, "stack": [cell(WALLET_CELL)]}),
        );
    }
    if path == format!("/blockchain/accounts/{JETTON_WALLET}/methods/get_wallet_data") && deployed {
        return (
            200,
            json!({
                "success": true,
                "exit_code": 0,
                "stack": [{"type": "num", "num": "0x56110d7a1790"}, cell(HOLDER_CELL), cell(USDT_CELL), cell(CODE_CELL)]
            }),
        );
    }
    if path.starts_with("/accounts/") || path.starts_with("/blockchain/accounts/") {
        return (404, json!({"error": "entity not found"}));
    }
    (404, json!("missing"))
}

#[tokio::test]
async fn reads_the_same_through_tonapi_and_unknown_accounts_hold_nothing() {
    let url = node(Log::default(), |path, _| tonapi(path, true)).await;
    let chain = TonChain::new(ton_on(&[(Protocol::TonApi, &url)])).unwrap();
    let balances = chain.balances(&holder(), &assets()).await.unwrap();
    assert_eq!(amounts(&balances), ["59570010.146444835", "94631240.538"]);
    let nobody = Address::Ton {
        workchain: 0,
        hash: [7; 32],
    };
    let empty = chain.balances(&nobody, &assets()[..1]).await.unwrap();
    assert_eq!(amounts(&empty), ["0"]);
    let url = node(Log::default(), |path, _| tonapi(path, false)).await;
    let chain = TonChain::new(ton_on(&[(Protocol::TonApi, &url)])).unwrap();
    assert_eq!(
        amounts(&chain.balances(&holder(), &assets()).await.unwrap()),
        ["59570010.146444835", "0"]
    );
}

#[tokio::test]
async fn liteservers_wait_and_other_networks_and_busy_providers_are_skipped() {
    let (lite_log, testnet_log, busy_log) = (Log::default(), Log::default(), Log::default());
    let lite = node(Arc::clone(&lite_log), |_, _| (500, json!("unused"))).await;
    let testnet = serve(&Center::new(TESTNET_ZERO_STATE), Arc::clone(&testnet_log)).await;
    let busy = node(Arc::clone(&busy_log), |_, _| {
        (429, json!({"error": "Ratelimit exceeded"}))
    })
    .await;
    let api = node(Log::default(), |path, _| tonapi(path, true)).await;
    let chain = TonChain::new(ton_on(&[
        (Protocol::TonLiteservers, &lite),
        (Protocol::TonCenter, &testnet),
        (Protocol::TonCenter, &busy),
        (Protocol::TonApi, &api),
    ]))
    .unwrap();
    let balances = chain.balances(&holder(), &assets()[..1]).await.unwrap();
    assert_eq!(amounts(&balances), ["59570010.146444835"]);
    assert_eq!(*lite_log.lock().unwrap(), Vec::<String>::new());
    assert_eq!(*testnet_log.lock().unwrap(), ["/v2/getMasterchainInfo"]);
    assert_eq!(busy_log.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn custom_tokens_and_foreign_addresses_are_refused() {
    let url = node(Log::default(), |path, _| tonapi(path, true)).await;
    let chain = TonChain::new(ton_on(&[(Protocol::TonApi, &url)])).unwrap();
    let custom = AssetId::Token(
        "ton".parse().unwrap(),
        Address::Ton {
            workchain: 0,
            hash: [9; 32],
        },
    );
    assert_eq!(
        chain.balances(&holder(), &[custom]).await,
        Err(ChainError::Unsupported("custom TON tokens yet"))
    );
    assert!(matches!(
        chain.balances(&Address::Evm([1; 20]), &assets()).await,
        Err(ChainError::Invalid(_))
    ));
    assert!(matches!(
        chain.balances(&holder(), &["tron".parse().unwrap()]).await,
        Err(ChainError::Invalid(_))
    ));
}
