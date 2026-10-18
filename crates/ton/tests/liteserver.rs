#![allow(clippy::unwrap_used)]

use std::fmt::Write;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use adnl::AdnlPeer;
use adnl::crypto::{KeyPair, SecretKey};
use bahaa_chain::{Chain, ChainError};
use bahaa_config::{Endpoint, Network, Protocol, Registry};
use bahaa_ton::TonChain;
use bahaa_types::{Address, AssetId};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tonlib_core::cell::{ArcCell, BagOfCells, Cell, CellBuilder};
use tonlib_core::{TonAddress, TonHash};
use url::Url;

const FIXTURE: &str = include_str!("fixtures/account_proofs.json");
const QUOTE: &str = include_str!("fixtures/quote.json");
const SENDER: [u8; 32] = [
    0x13, 0xfd, 0x99, 0x07, 0xed, 0x89, 0x66, 0x0d, 0x9b, 0x90, 0xf9, 0x25, 0xbd, 0x54, 0xa6, 0xb0,
    0x29, 0xca, 0xfb, 0x36, 0x8a, 0x17, 0x28, 0x88, 0xf1, 0xff, 0xc6, 0xfd, 0x69, 0x06, 0x24, 0xe5,
];
const GET_CONFIG_PARAMS: u32 = 0x2a11_1c19;
const MAINNET: &str = "17a3a92992aabea785a7a090985a265cd31f323d849da51239737e321fb05569";
const TESTNET: [u8; 32] = [0x82; 32];
const OWNER: [u8; 32] = [
    0xf8, 0xb8, 0x63, 0x5d, 0x07, 0x86, 0xb7, 0x7f, 0x9d, 0xa6, 0x60, 0xe6, 0x5f, 0xd3, 0x1d, 0x67,
    0x0a, 0x67, 0x5d, 0xd4, 0x30, 0x2f, 0xac, 0xba, 0x69, 0x2f, 0x46, 0xb5, 0x71, 0xeb, 0x53, 0x60,
];
const FRESH: [u8; 32] = [
    0x5c, 0x9d, 0x2f, 0x41, 0x66, 0x13, 0x37, 0xa8, 0x0e, 0x71, 0xc4, 0x58, 0x2b, 0x9a, 0xf0, 0x06,
    0xd3, 0x4e, 0x81, 0x27, 0x5c, 0x9d, 0x2f, 0x41, 0x66, 0x13, 0x37, 0xa8, 0x0e, 0x71, 0xc4, 0x58,
];
const MASTER: [u8; 32] = [0xb1; 32];
const WALLET: [u8; 32] = [0x7f; 32];
const GET_MASTERCHAIN_INFO: u32 = 0x89b5_e62e;
const GET_ACCOUNT_STATE: u32 = 0x6b89_0e25;
const RUN_SMC_METHOD: u32 = 0x5cc6_5dd2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tamper {
    Nothing,
    State,
    ShardProof,
    Block,
    Anchor,
    Swap,
    Forge,
}

#[derive(Clone, Copy)]
struct Behaviour {
    recording: &'static str,
    foreign: bool,
    refuse: bool,
    deployed: bool,
    holder: [u8; 32],
    tamper: Tamper,
}

const GOOD: Behaviour = Behaviour {
    recording: FIXTURE,
    foreign: false,
    refuse: false,
    deployed: true,
    holder: OWNER,
    tamper: Tamper::Nothing,
};

#[derive(Default)]
struct Tl(Vec<u8>);

impl Tl {
    fn u32(mut self, value: u32) -> Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    fn i32(self, value: i32) -> Self {
        self.u32(u32::from_le_bytes(value.to_le_bytes()))
    }

    fn raw(mut self, value: &[u8]) -> Self {
        self.0.extend_from_slice(value);
        self
    }

    fn bytes(mut self, value: &[u8]) -> Self {
        let start = self.0.len();
        if value.len() < 254 {
            self.0.push(u8::try_from(value.len()).unwrap());
        } else {
            self.0.push(0xfe);
            self.0
                .extend_from_slice(&u32::try_from(value.len()).unwrap().to_le_bytes()[..3]);
        }
        self.0.extend_from_slice(value);
        while !(self.0.len() - start).is_multiple_of(4) {
            self.0.push(0);
        }
        self
    }

    fn block(self) -> Self {
        self.i32(-1)
            .raw(&i64::MIN.to_le_bytes())
            .i32(1_000)
            .raw(&[1; 32])
            .raw(&[2; 32])
    }
}

fn read_bytes(data: &[u8]) -> (&[u8], usize) {
    let (length, start) = if data[0] < 254 {
        (usize::from(data[0]), 1)
    } else {
        (
            usize::from(data[1]) | usize::from(data[2]) << 8 | usize::from(data[3]) << 16,
            4,
        )
    };
    let used = start + length;
    (&data[start..used], used + (4 - used % 4) % 4)
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

fn mainnet() -> [u8; 32] {
    unhex(MAINNET).try_into().unwrap()
}

fn recorded(recording: &str, path: &[&str]) -> Option<Vec<u8>> {
    let fixture: Value = serde_json::from_str(recording).unwrap();
    let value = path.iter().fold(&fixture, |value, key| &value[*key]);
    value.as_str().map(unhex)
}

fn recorded_account(hash: &[u8; 32]) -> Vec<u8> {
    recorded(FIXTURE, &["accounts", &hex(hash)]).unwrap()
}

fn account_answer(answer: &[u8], tamper: Tamper) -> Vec<u8> {
    let head = &answer[..164];
    let (shard_proof, first) = read_bytes(&answer[164..]);
    let (proof, second) = read_bytes(&answer[164 + first..]);
    let (state, _) = read_bytes(&answer[164 + first + second..]);
    let (mut shard_proof, mut state) = (shard_proof.to_vec(), state.to_vec());
    match tamper {
        Tamper::State => {
            let middle = state.len() / 2;
            state[middle] ^= 1;
        }
        Tamper::ShardProof => {
            let middle = shard_proof.len() / 2;
            shard_proof[middle] ^= 1;
        }
        Tamper::Forge => state = forged(&head_hash(answer)),
        Tamper::Nothing | Tamper::Block | Tamper::Anchor | Tamper::Swap => {}
    }
    let mut head = head.to_vec();
    if tamper == Tamper::Anchor {
        head[20] ^= 1;
    }
    Tl::default()
        .raw(&head)
        .bytes(&shard_proof)
        .bytes(proof)
        .bytes(&state)
        .0
}

fn head_hash(answer: &[u8]) -> [u8; 32] {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let wanted = hex(answer);
    if fixture["accounts"][hex(&FRESH)] == wanted.as_str() {
        FRESH
    } else {
        OWNER
    }
}

fn forged(hash: &[u8; 32]) -> Vec<u8> {
    let mut cell = CellBuilder::new();
    cell.store_bit(true).unwrap();
    cell.store_address(&ton_address(*hash)).unwrap();
    cell.store_u8(3, 0)
        .unwrap()
        .store_u8(3, 0)
        .unwrap()
        .store_u8(3, 0)
        .unwrap();
    cell.store_u32(32, 0).unwrap().store_bit(false).unwrap();
    cell.store_u64(64, 0).unwrap();
    cell.store_u8(4, 5)
        .unwrap()
        .store_u64(40, 5_000_000_000)
        .unwrap();
    cell.store_bit(false).unwrap();
    cell.store_u8(2, 0).unwrap();
    BagOfCells::from_root(cell.build().unwrap())
        .serialize(false)
        .unwrap()
}

fn ton_address(hash: [u8; 32]) -> TonAddress {
    TonAddress::new(0, TonHash::from(hash))
}

fn stack(values: Vec<CellBuilder>) -> Vec<u8> {
    let mut rest = CellBuilder::new().build().unwrap().to_arc();
    let depth = values.len();
    for (at, mut value) in values.into_iter().enumerate() {
        let mut cell = CellBuilder::new();
        if at + 1 == depth {
            cell.store_u32(24, u32::try_from(depth).unwrap()).unwrap();
        }
        cell.store_reference(&rest).unwrap();
        cell.store_cell(&value.build().unwrap()).unwrap();
        rest = cell.build().unwrap().to_arc();
    }
    BagOfCells::from_root(Cell::clone(&rest))
        .serialize(false)
        .unwrap()
}

fn address_cell(hash: [u8; 32]) -> ArcCell {
    let mut inner = CellBuilder::new();
    inner.store_address(&ton_address(hash)).unwrap();
    inner.build().unwrap().to_arc()
}

fn as_cell(hash: [u8; 32]) -> CellBuilder {
    let mut value = CellBuilder::new();
    value.store_u8(8, 3).unwrap();
    value.store_reference(&address_cell(hash)).unwrap();
    value
}

fn as_slice(hash: [u8; 32]) -> CellBuilder {
    let mut value = CellBuilder::new();
    value.store_u8(8, 4).unwrap();
    value.store_reference(&address_cell(hash)).unwrap();
    value.store_u16(10, 0).unwrap().store_u16(10, 267).unwrap();
    value.store_u8(3, 0).unwrap().store_u8(3, 0).unwrap();
    value
}

fn as_int(number: u64) -> CellBuilder {
    let mut value = CellBuilder::new();
    value.store_u8(8, 2).unwrap().store_u8(7, 0).unwrap();
    value.store_bit(false).unwrap();
    let mut bits = [0u8; 32];
    bits[24..].copy_from_slice(&number.to_be_bytes());
    value.store_bits(256, &bits).unwrap();
    value
}

fn method_result(exit: i32, stack: Option<Vec<u8>>) -> Vec<u8> {
    let tl = Tl::default()
        .u32(0xa39a_616b)
        .u32(if stack.is_some() { 4 } else { 0 })
        .block()
        .block()
        .i32(exit);
    match stack {
        Some(stack) => tl.bytes(&stack).0,
        None => tl.0,
    }
}

fn reply(behaviour: Behaviour, data: &[u8]) -> Vec<u8> {
    let function = u32::from_le_bytes(data[..4].try_into().unwrap());
    if behaviour.refuse && function != GET_MASTERCHAIN_INFO {
        return Tl::default()
            .u32(0xbba9_e148)
            .i32(651)
            .bytes(b"state not ready")
            .0;
    }
    match function {
        GET_MASTERCHAIN_INFO => {
            let mut info = recorded(behaviour.recording, &["masterchain_info"]).unwrap();
            if behaviour.foreign {
                info[120..152].copy_from_slice(&TESTNET);
            }
            if matches!(behaviour.tamper, Tamper::Block | Tamper::Anchor) {
                info[20] ^= 1;
            }
            info
        }
        GET_ACCOUNT_STATE => {
            let hash: [u8; 32] = data[88..120].try_into().unwrap();
            let wanted = match (hash, behaviour.tamper) {
                (OWNER, Tamper::Swap) => FRESH,
                (other, _) => other,
            };
            let Some(answer) = recorded(behaviour.recording, &["accounts", &hex(&wanted)]) else {
                return Tl::default().u32(0xbba9_e148).i32(-400).bytes(b"unknown").0;
            };
            account_answer(&answer, behaviour.tamper)
        }
        GET_CONFIG_PARAMS => {
            let mut config = recorded(behaviour.recording, &["config"]).unwrap();
            if behaviour.tamper == Tamper::State {
                let middle = config.len() / 2;
                config[middle] ^= 1;
            }
            config
        }
        RUN_SMC_METHOD => {
            let hash: [u8; 32] = data[92..124].try_into().unwrap();
            let method = i64::from_le_bytes(data[124..132].try_into().unwrap());
            let (params, _) = read_bytes(&data[132..]);
            match (hash, method) {
                (MASTER, 103_289) => {
                    let root = BagOfCells::parse(params).unwrap().single_root().unwrap();
                    let asked = root.reference(1).unwrap().parser().load_address().unwrap();
                    assert_eq!(asked.hash_part.as_slice(), OWNER);
                    method_result(0, Some(stack(vec![as_slice(WALLET)])))
                }
                (WALLET, 97_026) if behaviour.deployed => method_result(
                    0,
                    Some(stack(vec![
                        as_int(94_631_240_538_000),
                        as_cell(behaviour.holder),
                        as_cell(MASTER),
                        as_cell([0xc0; 32]),
                    ])),
                ),
                (WALLET, 97_026) => method_result(-256, None),
                _ => method_result(11, None),
            }
        }
        _ => Tl::default().u32(0xbba9_e148).i32(-400).bytes(b"unknown").0,
    }
}

async fn liteserver(
    secret: u8,
    behaviour: Behaviour,
    asked: Arc<Mutex<usize>>,
) -> (SocketAddr, [u8; 32]) {
    let keys = KeyPair::from(&SecretKey::from_bytes([secret; 32]));
    let public = keys.public_key.to_bytes();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            let asked = Arc::clone(&asked);
            tokio::spawn(async move {
                let Ok(mut peer) = AdnlPeer::handle_handshake(socket, |_| Some(keys)).await else {
                    return;
                };
                while let Some(Ok(frame)) = peer.next().await {
                    assert_eq!(&frame[..4], 0xb48b_f97a_u32.to_le_bytes());
                    let id = &frame[4..36];
                    let (query, _) = read_bytes(&frame[36..]);
                    assert_eq!(&query[..4], 0x798c_06df_u32.to_le_bytes());
                    let (data, _) = read_bytes(&query[4..]);
                    *asked.lock().unwrap() += 1;
                    let answer = reply(behaviour, data);
                    let frame = Tl::default().u32(0x0fac_8416).raw(id).bytes(&answer).0;
                    if peer.send(Bytes::from(frame)).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    (address, public)
}

async fn serve(body: String) -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!(
        "http://{}/global-config.json",
        listener.local_addr().unwrap()
    ))
    .unwrap();
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let body = body.clone();
            tokio::spawn(async move {
                let mut buffer = [0u8; 4096];
                let _ = socket.read(&mut buffer).await;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    url
}

async fn config(servers: &[(SocketAddr, [u8; 32])], zero_state: [u8; 32]) -> Url {
    let entries: Vec<Value> = servers
        .iter()
        .map(|(address, key)| {
            json!({
                "ip": 2_130_706_433,
                "port": address.port(),
                "id": {"@type": "pub.ed25519", "key": STANDARD.encode(key)}
            })
        })
        .collect();
    serve(
        json!({
            "liteservers": entries,
            "validator": {"zero_state": {"root_hash": STANDARD.encode(zero_state)}}
        })
        .to_string(),
    )
    .await
}

fn network(config: Url) -> Network {
    let registry = Registry::builtin().unwrap();
    let mut network = registry.get(&"ton".parse().unwrap()).unwrap().clone();
    network.endpoints = vec![Endpoint {
        protocol: Protocol::TonLiteservers,
        url: config,
        features: Vec::new(),
        min_gap_ms: 0,
    }];
    network.tokens[0].contract = Address::Ton {
        workchain: 0,
        hash: MASTER,
    };
    network
}

fn gram() -> AssetId {
    "ton".parse().unwrap()
}

fn assets() -> Vec<AssetId> {
    vec![
        gram(),
        AssetId::Token(
            "ton".parse().unwrap(),
            Address::Ton {
                workchain: 0,
                hash: MASTER,
            },
        ),
    ]
}

fn ton(hash: [u8; 32]) -> Address {
    Address::Ton { workchain: 0, hash }
}

fn amounts(balances: &[bahaa_chain::Balance]) -> Vec<String> {
    balances
        .iter()
        .map(|balance| balance.amount.to_string())
        .collect()
}

async fn chain_for(behaviour: Behaviour) -> TonChain {
    let server = liteserver(1, behaviour, Arc::default()).await;
    TonChain::new(network(config(&[server], mainnet()).await)).unwrap()
}

#[test]
fn the_recording_is_replayed_byte_for_byte() {
    for hash in [OWNER, FRESH] {
        let recorded = recorded_account(&hash);
        assert_eq!(account_answer(&recorded, Tamper::Nothing), recorded);
    }
}

#[tokio::test]
async fn proven_gram_and_usdt_arrive_over_adnl() {
    let asked = Arc::new(Mutex::new(0));
    let server = liteserver(2, GOOD, Arc::clone(&asked)).await;
    let chain = TonChain::new(network(config(&[server], mainnet()).await)).unwrap();
    let balances = chain.balances(&ton(OWNER), &assets()).await.unwrap();
    assert_eq!(amounts(&balances), ["59570010.146444835", "94631240.538"]);
    assert_eq!(*asked.lock().unwrap(), 6);
    let again = chain.balances(&ton(OWNER), &assets()).await.unwrap();
    assert_eq!(amounts(&again), amounts(&balances));
    assert_eq!(*asked.lock().unwrap(), 10);
    let fresh = chain.balances(&ton(FRESH), &[gram()]).await.unwrap();
    assert_eq!(amounts(&fresh), ["0"]);
}

#[tokio::test]
async fn answers_their_proofs_do_not_cover_are_refused() {
    for tamper in [
        Tamper::State,
        Tamper::ShardProof,
        Tamper::Block,
        Tamper::Anchor,
        Tamper::Swap,
    ] {
        let chain = chain_for(Behaviour { tamper, ..GOOD }).await;
        let result = chain.balances(&ton(OWNER), &[gram()]).await;
        assert!(
            matches!(result, Err(ChainError::Network(_))),
            "{tamper:?}: {result:?}"
        );
    }
}

#[tokio::test]
async fn an_absence_proof_cannot_carry_an_invented_account() {
    let honest = chain_for(GOOD).await;
    assert_eq!(
        amounts(&honest.balances(&ton(FRESH), &[gram()]).await.unwrap()),
        ["0"]
    );
    let forging = chain_for(Behaviour {
        tamper: Tamper::Forge,
        ..GOOD
    })
    .await;
    let result = forging.balances(&ton(FRESH), &[gram()]).await;
    assert!(matches!(result, Err(ChainError::Network(_))), "{result:?}");
}

#[tokio::test]
async fn servers_of_another_network_or_not_ready_are_passed_over() {
    let (foreign_asked, busy_asked, good_asked) = (
        Arc::new(Mutex::new(0)),
        Arc::new(Mutex::new(0)),
        Arc::new(Mutex::new(0)),
    );
    let foreign = Behaviour {
        foreign: true,
        ..GOOD
    };
    let busy = Behaviour {
        refuse: true,
        ..GOOD
    };
    let servers = [
        liteserver(3, foreign, Arc::clone(&foreign_asked)).await,
        liteserver(4, busy, Arc::clone(&busy_asked)).await,
        liteserver(5, GOOD, Arc::clone(&good_asked)).await,
    ];
    let chain = TonChain::new(network(config(&servers, mainnet()).await)).unwrap();
    for _ in 0..3 {
        let balances = chain.balances(&ton(OWNER), &[gram()]).await.unwrap();
        assert_eq!(amounts(&balances), ["59570010.146444835"]);
    }
    assert!(*foreign_asked.lock().unwrap() <= 1);
    assert!(*busy_asked.lock().unwrap() <= 2);
    assert!(*good_asked.lock().unwrap() >= 4);
}

#[tokio::test]
async fn an_undeployed_token_wallet_holds_nothing_and_a_foreign_one_is_refused() {
    let fresh = chain_for(Behaviour {
        deployed: false,
        ..GOOD
    })
    .await;
    assert_eq!(
        amounts(&fresh.balances(&ton(OWNER), &assets()).await.unwrap()),
        ["59570010.146444835", "0"]
    );
    let foreign = chain_for(Behaviour {
        holder: MASTER,
        ..GOOD
    })
    .await;
    assert!(matches!(
        foreign.balances(&ton(OWNER), &assets()).await,
        Err(ChainError::Network(message)) if message.contains("does not belong")
    ));
}

#[tokio::test]
async fn a_server_list_for_another_network_is_not_used() {
    let asked = Arc::new(Mutex::new(0));
    let server = liteserver(6, GOOD, Arc::clone(&asked)).await;
    let chain = TonChain::new(network(config(&[server], TESTNET).await)).unwrap();
    assert!(matches!(
        chain.balances(&ton(OWNER), &[gram()]).await,
        Err(ChainError::Network(message)) if message.contains("another TON network")
    ));
    assert_eq!(*asked.lock().unwrap(), 0);
}

async fn tonapi(asked: Arc<Mutex<Vec<String>>>) -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!("http://{}/v2", listener.local_addr().unwrap())).unwrap();
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let asked = Arc::clone(&asked);
            tokio::spawn(async move {
                let mut buffer = [0u8; 4096];
                let read = socket.read(&mut buffer).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]).to_string();
                let path = request.split(' ').nth(1).unwrap_or_default().to_owned();
                asked.lock().unwrap().push(path.clone());
                let body = if path.ends_with("/liteserver/get_masterchain_info") {
                    json!({"init": {"root_hash": MAINNET}})
                } else {
                    json!({"balance": 9_000_000_000_u64, "status": "active"})
                }
                .to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    url
}

#[tokio::test]
async fn when_no_liteserver_can_be_trusted_the_http_providers_answer() {
    let cases = [
        Behaviour {
            foreign: true,
            ..GOOD
        },
        Behaviour {
            refuse: true,
            ..GOOD
        },
        Behaviour {
            tamper: Tamper::State,
            ..GOOD
        },
    ];
    for (secret, behaviour) in (7..).zip(cases) {
        let server = liteserver(secret, behaviour, Arc::default()).await;
        let asked = Arc::new(Mutex::new(Vec::new()));
        let mut network = network(config(&[server], mainnet()).await);
        network.endpoints.push(Endpoint {
            protocol: Protocol::TonApi,
            url: tonapi(Arc::clone(&asked)).await,
            features: Vec::new(),
            min_gap_ms: 0,
        });
        let chain = TonChain::new(network).unwrap();
        let balances = chain.balances(&ton(OWNER), &[gram()]).await.unwrap();
        assert_eq!(amounts(&balances), ["9"]);
        assert!(
            asked
                .lock()
                .unwrap()
                .iter()
                .any(|path| path.starts_with("/v2/accounts/0:"))
        );
    }
}

async fn quote_chain_for(tamper: Tamper) -> TonChain {
    let behaviour = Behaviour {
        recording: QUOTE,
        tamper,
        ..GOOD
    };
    let server = liteserver(20, behaviour, Arc::default()).await;
    TonChain::new(network(config(&[server], mainnet()).await)).unwrap()
}

fn send(amount: &str) -> bahaa_chain::Transfer {
    bahaa_chain::Transfer {
        account: 0,
        from: ton(SENDER),
        to: ton([0x5c; 32]),
        asset: gram(),
        amount: bahaa_types::Amount::parse(amount, 9).unwrap(),
    }
}

#[tokio::test]
async fn a_v5r1_quote_is_computed_from_proven_state_and_fee_settings() {
    let chain = quote_chain_for(Tamper::Nothing).await;
    let (_, quote) = chain.prepare(&send("0.01")).await.unwrap();
    let lines: Vec<(bahaa_chain::FeeItem, String)> = quote
        .lines()
        .iter()
        .map(|line| (line.item.clone(), line.amount.to_string()))
        .collect();
    assert_eq!(
        lines[0],
        (
            bahaa_chain::FeeItem::Compute { gas: 4_939 },
            "0.000329268".to_owned()
        )
    );
    assert_eq!(lines[1].0, bahaa_chain::FeeItem::Storage);
    assert_eq!(
        lines[2],
        (bahaa_chain::FeeItem::Forwarding, "0.000183201".to_owned())
    );
    assert_eq!(
        quote.max().units(),
        quote.expected().units() * ruint::aliases::U256::from(2u8)
    );
    let Err(ChainError::Insufficient {
        symbol, available, ..
    }) = chain.prepare(&send("2")).await
    else {
        panic!("expected a refusal");
    };
    assert_eq!(
        (symbol.as_str(), available.to_string()),
        ("GRAM", "1.848793222".to_owned())
    );
}

#[tokio::test]
async fn quotes_come_only_from_v5r1_wallets_and_know_when_not_to_bounce() {
    let older = chain_for(GOOD).await;
    let mut from_v4 = send("0.01");
    from_v4.from = ton(OWNER);
    assert!(matches!(
        older.prepare(&from_v4).await,
        Err(ChainError::Unsupported(_))
    ));
    let chain = quote_chain_for(Tamper::Nothing).await;
    let (plan, _) = chain.prepare(&send("0.01")).await.unwrap();
    let shown = format!("{plan:?}");
    assert!(
        shown.contains("bounce: false") && shown.contains("deploy: false"),
        "{shown}"
    );
}

#[tokio::test]
async fn a_quote_needs_a_config_its_proof_covers() {
    let chain = quote_chain_for(Tamper::State).await;
    assert!(matches!(
        chain.prepare(&send("0.01")).await,
        Err(ChainError::Network(_))
    ));
}
