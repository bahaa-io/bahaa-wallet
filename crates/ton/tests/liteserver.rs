#![allow(clippy::unwrap_used)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use adnl::AdnlPeer;
use adnl::crypto::{KeyPair, SecretKey};
use bahaa_chain::ChainError;
use bahaa_config::{Endpoint, Network, Protocol, Registry};
use bahaa_ton::TonChain;
use bahaa_types::{Address, AssetId};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tonlib_core::cell::{BagOfCells, CellBuilder};
use tonlib_core::{TonAddress, TonHash};
use url::Url;

const MAINNET: [u8; 32] = [0x17; 32];
const TESTNET: [u8; 32] = [0x82; 32];
const OWNER: [u8; 32] = [0xab; 32];
const MASTER: [u8; 32] = [0xb1; 32];
const WALLET: [u8; 32] = [0x7f; 32];
const GET_MASTERCHAIN_INFO: u32 = 0x89b5_e62e;
const GET_ACCOUNT_STATE: u32 = 0x6b89_0e25;
const RUN_SMC_METHOD: u32 = 0x5cc6_5dd2;

#[derive(Clone, Copy)]
struct Behaviour {
    zero_state: [u8; 32],
    refuse: bool,
    deployed: bool,
    holder: [u8; 32],
}

const GOOD: Behaviour = Behaviour {
    zero_state: MAINNET,
    refuse: false,
    deployed: true,
    holder: OWNER,
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

fn boc(builder: &mut CellBuilder) -> Vec<u8> {
    BagOfCells::from_root(builder.build().unwrap())
        .serialize(false)
        .unwrap()
}

fn ton_address(hash: [u8; 32]) -> TonAddress {
    TonAddress::new(0, TonHash::from(hash))
}

fn account(hash: &[u8; 32], balance: u64) -> Vec<u8> {
    let mut cell = CellBuilder::new();
    cell.store_bit(true).unwrap();
    cell.store_address(&ton_address(*hash)).unwrap();
    cell.store_u8(3, 1).unwrap().store_u8(8, 9).unwrap();
    cell.store_u8(3, 1).unwrap().store_u8(8, 200).unwrap();
    cell.store_u8(3, 0)
        .unwrap()
        .store_u32(32, 1_790_000_000)
        .unwrap()
        .store_bit(false)
        .unwrap();
    cell.store_u64(64, 77).unwrap();
    cell.store_u8(4, 8).unwrap().store_u64(64, balance).unwrap();
    cell.store_bit(false).unwrap().store_bit(false).unwrap();
    boc(&mut cell)
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
    BagOfCells::from_root(tonlib_core::cell::Cell::clone(&rest))
        .serialize(false)
        .unwrap()
}

fn address_cell(hash: [u8; 32]) -> tonlib_core::cell::ArcCell {
    let mut inner = CellBuilder::new();
    inner.store_address(&ton_address(hash)).unwrap();
    inner.build().unwrap().to_arc()
}

fn as_cell(hash: [u8; 32]) -> CellBuilder {
    let mut value = CellBuilder::new();
    value
        .store_u8(8, 3)
        .unwrap()
        .store_reference(&address_cell(hash))
        .unwrap();
    value
}

fn as_slice(hash: [u8; 32]) -> CellBuilder {
    let mut value = CellBuilder::new();
    value
        .store_u8(8, 4)
        .unwrap()
        .store_reference(&address_cell(hash))
        .unwrap();
    value.store_u16(10, 0).unwrap().store_u16(10, 267).unwrap();
    value.store_u8(3, 0).unwrap().store_u8(3, 0).unwrap();
    value
}

fn as_int(number: u64) -> CellBuilder {
    let mut value = CellBuilder::new();
    value
        .store_u8(8, 2)
        .unwrap()
        .store_u8(7, 0)
        .unwrap()
        .store_bit(false)
        .unwrap();
    let mut bits = [0u8; 32];
    bits[24..].copy_from_slice(&number.to_be_bytes());
    value.store_bits(256, &bits).unwrap();
    value
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
            Tl::default()
                .u32(0x8583_2881)
                .block()
                .raw(&[3; 32])
                .i32(-1)
                .raw(&behaviour.zero_state)
                .raw(&[4; 32])
                .0
        }
        GET_ACCOUNT_STATE => {
            let hash: [u8; 32] = data[88..120].try_into().unwrap();
            Tl::default()
                .u32(0x7079_c751)
                .block()
                .block()
                .bytes(b"")
                .bytes(b"")
                .bytes(&account(&hash, 2_500_000_000))
                .0
        }
        RUN_SMC_METHOD => {
            let hash: [u8; 32] = data[92..124].try_into().unwrap();
            let method = i64::from_le_bytes(data[124..132].try_into().unwrap());
            let (params, _) = read_bytes(&data[132..]);
            let result = |exit: i32, stack: Option<Vec<u8>>| {
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
            };
            match (hash, method) {
                (MASTER, 103_289) => {
                    let root = BagOfCells::parse(params).unwrap().single_root().unwrap();
                    let slice = root.reference(1).unwrap();
                    let asked = slice.parser().load_address().unwrap();
                    assert_eq!(asked.hash_part.as_slice(), OWNER);
                    result(0, Some(stack(vec![as_slice(WALLET)])))
                }
                (WALLET, 97_026) if behaviour.deployed => result(
                    0,
                    Some(stack(vec![
                        as_int(94_631_240_538_000),
                        as_cell(behaviour.holder),
                        as_cell(MASTER),
                        as_cell([0xc0; 32]),
                    ])),
                ),
                (WALLET, 97_026) => result(-256, None),
                _ => result(11, None),
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

async fn config(servers: &[(SocketAddr, [u8; 32])], zero_state: [u8; 32]) -> Url {
    let entries: Vec<_> = servers
        .iter()
        .map(|(address, key)| {
            json!({
                "ip": 2_130_706_433,
                "port": address.port(),
                "id": {"@type": "pub.ed25519", "key": STANDARD.encode(key)}
            })
        })
        .collect();
    let body = json!({
        "liteservers": entries,
        "validator": {"zero_state": {"root_hash": STANDARD.encode(zero_state)}}
    })
    .to_string();
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

fn network(config: Url) -> Network {
    let registry = Registry::builtin().unwrap();
    let mut network = registry.get(&"ton".parse().unwrap()).unwrap().clone();
    network.kind = bahaa_config::ChainKind::Ton {
        global_id: -239,
        zero_state: MAINNET,
    };
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

fn assets() -> Vec<AssetId> {
    vec![
        "ton".parse().unwrap(),
        AssetId::Token(
            "ton".parse().unwrap(),
            Address::Ton {
                workchain: 0,
                hash: MASTER,
            },
        ),
    ]
}

fn owner() -> Address {
    Address::Ton {
        workchain: 0,
        hash: OWNER,
    }
}

fn amounts(balances: &[bahaa_chain::Balance]) -> Vec<String> {
    balances
        .iter()
        .map(|balance| balance.amount.to_string())
        .collect()
}

#[tokio::test]
async fn reads_gram_and_usdt_over_adnl_from_the_listed_servers() {
    let asked = Arc::new(Mutex::new(0));
    let server = liteserver(1, GOOD, Arc::clone(&asked)).await;
    let chain = TonChain::new(network(config(&[server], MAINNET).await)).unwrap();
    let balances = chain.balances(&owner(), &assets()).await.unwrap();
    assert_eq!(amounts(&balances), ["2.5", "94631240.538"]);
    assert_eq!(*asked.lock().unwrap(), 6);
    let again = chain.balances(&owner(), &assets()).await.unwrap();
    assert_eq!(amounts(&again), ["2.5", "94631240.538"]);
    assert_eq!(*asked.lock().unwrap(), 10);
}

#[tokio::test]
async fn servers_of_another_network_or_not_ready_are_passed_over() {
    let (foreign_asked, busy_asked, good_asked) = (
        Arc::new(Mutex::new(0)),
        Arc::new(Mutex::new(0)),
        Arc::new(Mutex::new(0)),
    );
    let foreign = Behaviour {
        zero_state: TESTNET,
        ..GOOD
    };
    let busy = Behaviour {
        refuse: true,
        ..GOOD
    };
    let servers = [
        liteserver(2, foreign, Arc::clone(&foreign_asked)).await,
        liteserver(3, busy, Arc::clone(&busy_asked)).await,
        liteserver(4, GOOD, Arc::clone(&good_asked)).await,
    ];
    let chain = TonChain::new(network(config(&servers, MAINNET).await)).unwrap();
    for _ in 0..3 {
        let balances = chain.balances(&owner(), &assets()[..1]).await.unwrap();
        assert_eq!(amounts(&balances), ["2.5"]);
    }
    assert!(*foreign_asked.lock().unwrap() <= 1);
    assert!(*busy_asked.lock().unwrap() <= 2);
    assert!(*good_asked.lock().unwrap() >= 4);
}

#[tokio::test]
async fn an_undeployed_token_wallet_holds_nothing_and_a_foreign_one_is_refused() {
    let fresh = Behaviour {
        deployed: false,
        ..GOOD
    };
    let server = liteserver(5, fresh, Arc::default()).await;
    let chain = TonChain::new(network(config(&[server], MAINNET).await)).unwrap();
    assert_eq!(
        amounts(&chain.balances(&owner(), &assets()).await.unwrap()),
        ["2.5", "0"]
    );
    let foreign = Behaviour {
        holder: MASTER,
        ..GOOD
    };
    let server = liteserver(6, foreign, Arc::default()).await;
    let chain = TonChain::new(network(config(&[server], MAINNET).await)).unwrap();
    assert!(matches!(
        chain.balances(&owner(), &assets()).await,
        Err(ChainError::Network(message)) if message.contains("does not belong")
    ));
}

#[tokio::test]
async fn a_server_list_for_another_network_is_not_used() {
    let asked = Arc::new(Mutex::new(0));
    let server = liteserver(7, GOOD, Arc::clone(&asked)).await;
    let chain = TonChain::new(network(config(&[server], TESTNET).await)).unwrap();
    assert!(matches!(
        chain.balances(&owner(), &assets()[..1]).await,
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
                let hex = "17".repeat(32);
                let body = if path.ends_with("/liteserver/get_masterchain_info") {
                    json!({"init": {"root_hash": hex}})
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
async fn when_no_liteserver_can_answer_the_http_providers_do() {
    let foreign = Behaviour {
        zero_state: TESTNET,
        ..GOOD
    };
    let busy = Behaviour {
        refuse: true,
        ..GOOD
    };
    for (secret, behaviour) in [(8, foreign), (9, busy)] {
        let server = liteserver(secret, behaviour, Arc::default()).await;
        let asked = Arc::new(Mutex::new(Vec::new()));
        let mut network = network(config(&[server], MAINNET).await);
        network.endpoints.push(Endpoint {
            protocol: Protocol::TonApi,
            url: tonapi(Arc::clone(&asked)).await,
            features: Vec::new(),
            min_gap_ms: 0,
        });
        let chain = TonChain::new(network).unwrap();
        let balances = chain.balances(&owner(), &assets()[..1]).await.unwrap();
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
