#![allow(clippy::unwrap_used)]

use std::fmt::Write;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use adnl::AdnlPeer;
use adnl::crypto::{KeyPair, SecretKey};
use bahaa_chain::{Chain, ChainError, Incoming, ScanCursor, Signed, TxId, TxStatus};
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
const TRACK: &str = include_str!("fixtures/track.json");
const SENT: &str = "b5ee9c72410205010001100001e5880027fb320fdb12cc1b3721f24b7aa94d605395f66d142e5111e3ff8dfad20c49ca039b4b3b73fffff88b566b9008000000559e4f7c61649cead47b81fc559961f1397ad6d17bf54953722f26dc0e635781c76df6d6f81c372949e1c8cb6a2cd85973bdab865f605980a59459cdf5e17aca0701020a0ec3c86d0302030000016862005bfa00866642a075cac79e914142651bcacae9fd297238395e0f73c0e9af00f3a018685d68000000000000000000000000010400ac0f8a7ea5000000006acd70d7506cc74a400801bca4e3e99d4b7eeed43fa097305639a10a68520be36b0b239c7ece42e7159acf0004ff6641fb62598366e43e496f5529ac0a72becda285ca223c7ff1bf5a41893942027a92ffc8";
const SENT_ID: &str = "88d7201d27158e7cbaf4cceaef8ce46f460e9dc1e084490b2485eb6e6112bd54";
const SENT_EXPIRES: u64 = 1_791_848_961;
const LATEST: u64 = 1_792_170_273;
const LATEST_LT: i64 = 110_383_004_000_040;
const YOUNG: [u8; 32] = [
    0x33, 0xa2, 0xfe, 0x1a, 0x7b, 0xd6, 0x96, 0xdf, 0x31, 0xe1, 0x8d, 0xc8, 0x3b, 0x1c, 0xab, 0xb7,
    0xee, 0x33, 0x40, 0xe1, 0x48, 0xf6, 0x25, 0x0d, 0xac, 0x2b, 0x15, 0x49, 0x21, 0x0f, 0x17, 0x43,
];
const DEPLOYED_ID: &str = "fbeb45558f4b19cbd0bfd02efea09b2303382c8fbecb471de9df383b75234891";
const DEPLOYED_EXPIRES: u64 = 1_791_888_072 + 300;
const REFUSAL: &str = "cannot apply external message to current state : External message was not accepted: cannot run message on account: inbound external message rejected by transaction 13FD9907ED89660D9B90F925BD54A6B029CAFB368A172888F1FFC6FD690624E5:\nexitcode=133, steps=49, gas_used=0\nVM Log (truncated):\n...SIGNU";
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
const GET_TRANSACTIONS: u32 = 0x1c40_e7a1;
const SEND_MESSAGE: u32 = 0x690a_d482;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tamper {
    Nothing,
    State,
    ShardProof,
    Block,
    Anchor,
    Swap,
    Forge,
    History,
    Replay,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Delivery {
    Taken,
    Ignored,
    Refused(i32, &'static str),
}

#[derive(Clone, Copy)]
struct Behaviour {
    recording: &'static str,
    foreign: bool,
    refuse: bool,
    deployed: bool,
    holder: [u8; 32],
    tamper: Tamper,
    delivery: Delivery,
}

const GOOD: Behaviour = Behaviour {
    recording: FIXTURE,
    foreign: false,
    refuse: false,
    deployed: true,
    holder: OWNER,
    tamper: Tamper::Nothing,
    delivery: Delivery::Taken,
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
        Tamper::Nothing
        | Tamper::Block
        | Tamper::Anchor
        | Tamper::Swap
        | Tamper::History
        | Tamper::Replay => {}
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
        GET_TRANSACTIONS => history(behaviour, data),
        SEND_MESSAGE => delivered(behaviour, data),
        _ => Tl::default().u32(0xbba9_e148).i32(-400).bytes(b"unknown").0,
    }
}

fn history(behaviour: Behaviour, data: &[u8]) -> Vec<u8> {
    let asked = i64::from_le_bytes(data[44..52].try_into().unwrap());
    let lt = if behaviour.tamper == Tamper::Replay {
        LATEST_LT
    } else {
        asked
    };
    let Some(mut list) = recorded(behaviour.recording, &["transactions", &lt.to_string()]) else {
        return Tl::default().u32(0xbba9_e148).i32(-400).bytes(b"unknown").0;
    };
    if behaviour.tamper == Tamper::History {
        let middle = list.len() / 2;
        list[middle] ^= 1;
    }
    list
}

fn delivered(behaviour: Behaviour, data: &[u8]) -> Vec<u8> {
    let (body, _) = read_bytes(&data[4..]);
    match behaviour.delivery {
        Delivery::Refused(code, message) => {
            Tl::default()
                .u32(0xbba9_e148)
                .i32(code)
                .bytes(message.as_bytes())
                .0
        }
        Delivery::Ignored => Tl::default().u32(0x3950_e597).i32(0).0,
        Delivery::Taken if body == unhex(SENT).as_slice() => {
            Tl::default().u32(0x3950_e597).i32(1).0
        }
        Delivery::Taken => {
            Tl::default()
                .u32(0xbba9_e148)
                .i32(0)
                .bytes(b"unexpected message")
                .0
        }
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

async fn tracking_chain(behaviour: Behaviour) -> TonChain {
    let behaviour = Behaviour {
        recording: TRACK,
        ..behaviour
    };
    let server = liteserver(30, behaviour, Arc::default()).await;
    TonChain::new(network(config(&[server], mainnet()).await)).unwrap()
}

fn sent_id() -> TxId {
    TxId::new(SENT_ID.to_owned()).unwrap()
}

#[tokio::test]
async fn a_sent_transfer_is_found_in_the_proven_wallet_history() {
    let chain = tracking_chain(GOOD).await;
    assert_eq!(
        chain
            .status(&sent_id(), &ton(SENDER), Some(SENT_EXPIRES))
            .await
            .unwrap(),
        TxStatus::Confirmed {
            block: None,
            fee: Some(bahaa_types::Amount::parse("0.000640998", 9).unwrap())
        }
    );
    let unknown = TxId::new("ab".repeat(32)).unwrap();
    assert_eq!(
        chain
            .status(&unknown, &ton(SENDER), Some(SENT_EXPIRES))
            .await
            .unwrap(),
        TxStatus::NotFound
    );
    assert_eq!(
        chain
            .status(&sent_id(), &ton(SENDER), Some(LATEST + 3_900 + 1))
            .await
            .unwrap(),
        TxStatus::NotFound
    );
    assert!(matches!(
        chain.status(&sent_id(), &ton(SENDER), None).await,
        Err(ChainError::Invalid(_))
    ));
    let deployed = TxId::new(DEPLOYED_ID.to_owned()).unwrap();
    assert_eq!(
        chain
            .status(&deployed, &ton(YOUNG), Some(DEPLOYED_EXPIRES))
            .await
            .unwrap(),
        TxStatus::Confirmed {
            block: None,
            fee: Some(bahaa_types::Amount::parse("0.00046067", 9).unwrap())
        }
    );
    assert_eq!(
        chain
            .status(&unknown, &ton(YOUNG), Some(DEPLOYED_EXPIRES))
            .await
            .unwrap(),
        TxStatus::NotFound
    );
    let fresh = quote_chain_for(Tamper::Nothing).await;
    assert_eq!(
        fresh
            .status(&sent_id(), &ton([0x5c; 32]), Some(SENT_EXPIRES))
            .await
            .unwrap(),
        TxStatus::NotFound
    );
}

#[tokio::test]
async fn history_that_does_not_continue_the_proven_state_is_refused() {
    for tamper in [Tamper::History, Tamper::Replay] {
        let chain = tracking_chain(Behaviour { tamper, ..GOOD }).await;
        let unknown = TxId::new("ab".repeat(32)).unwrap();
        assert!(
            matches!(
                chain
                    .status(&unknown, &ton(SENDER), Some(SENT_EXPIRES))
                    .await,
                Err(ChainError::Network(_))
            ),
            "{tamper:?}"
        );
    }
}

#[tokio::test]
async fn transfers_are_handed_over_and_only_message_refusals_are_final() {
    let signed = Signed {
        id: sent_id(),
        bytes: unhex(SENT),
    };
    let taking = tracking_chain(GOOD).await;
    assert_eq!(taking.broadcast(&signed).await, Ok(sent_id()));
    let refusing = tracking_chain(Behaviour {
        delivery: Delivery::Refused(-701, REFUSAL),
        ..GOOD
    })
    .await;
    assert_eq!(
        refusing.broadcast(&signed).await,
        Err(ChainError::Rejected(
            "the wallet contract refused the message with exit code 133".to_owned()
        ))
    );
    for delivery in [Delivery::Refused(651, "state not ready"), Delivery::Ignored] {
        let busy = tracking_chain(Behaviour { delivery, ..GOOD }).await;
        assert!(
            matches!(busy.broadcast(&signed).await, Err(ChainError::Network(_))),
            "{delivery:?}"
        );
    }
}

fn summed(found: &[Incoming]) -> (usize, ruint::aliases::U256) {
    (
        found.len(),
        found.iter().map(|incoming| incoming.amount.units()).sum(),
    )
}

fn incoming(id: &str, from: &str, to: [u8; 32], nano: &str, lt: u64, time: u64) -> Incoming {
    Incoming {
        id: TxId::new(id.to_owned()).unwrap(),
        index: 0,
        from: Address::parse(bahaa_types::ChainFamily::Ton, from).unwrap(),
        to: ton(to),
        asset: gram(),
        amount: bahaa_types::Amount::parse(nano, 9).unwrap(),
        block: lt,
        time,
    }
}

#[tokio::test]
async fn a_first_scan_reads_back_to_the_cap_or_the_first_transaction() {
    let chain = tracking_chain(GOOD).await;
    let busy = chain
        .scan(&ton(SENDER), &[gram()], None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        busy.cursor,
        ScanCursor {
            next: 110_383_004_000_041,
            complete_from: 67_288_602_000_008,
            complete_since: 1_771_939_107
        }
    );
    assert_eq!(
        summed(&busy.found),
        (34, ruint::aliases::U256::from(915_967_845u64))
    );
    assert_eq!(
        busy.found[0],
        incoming(
            "ca7ce93aedf52d8acefc446f635ebb2569f1720a05abe8d9e14d0133cc72a544",
            "0:aaa71dfbda12f506eff5ec31ad26ea9a2133107c8d7493b0331c318aef12c4dc",
            SENDER,
            "0.000000001",
            109_775_840_000_021,
            1_791_927_519
        )
    );
    assert_eq!(
        busy.found[33].id.as_str(),
        "c0c5d70321653e4534a4ee8308933151be8d8667ae360255255e57112248961e"
    );
    let young = chain
        .scan(&ton(YOUNG), &[gram()], None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        young.cursor,
        ScanCursor {
            next: 109_677_718_000_019,
            complete_from: 109_677_195_000_019,
            complete_since: 1_791_888_066
        }
    );
    assert_eq!(
        young.found,
        [
            incoming(
                "7ab267622380d313537edc04dbe4cc6036252295be130c43fd67f80c18740eab",
                "0:2a0ef9232b5aab6a2ded5e2c60bc24815af53d50c34f40ea98d5c08ad5f45b01",
                YOUNG,
                "0.00001",
                109_677_718_000_018,
                1_791_888_274
            ),
            incoming(
                "dfa71cba2dc3d7eb03c5b2ce5897547c6b6a47f589854c4b65ba36722d129375",
                "0:3f0483d026c5651998a75832a9ea573e35c6dd8d49912f70f275c7bde7effacf",
                YOUNG,
                "0.0007",
                109_677_195_000_019,
                1_791_888_066
            ),
        ]
    );
    let fresh = quote_chain_for(Tamper::Nothing).await;
    let nothing = fresh
        .scan(&ton([0x5c; 32]), &[gram()], None)
        .await
        .unwrap()
        .unwrap();
    let checked = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert_eq!((nothing.cursor.next, nothing.cursor.complete_from), (0, 0));
    assert!(nothing.cursor.complete_since.abs_diff(checked) < 60);
    assert_eq!(nothing.found, []);
    assert_eq!(
        fresh
            .scan(&ton([0x5c; 32]), &[gram()], Some(nothing.cursor))
            .await,
        Ok(None)
    );
}

#[tokio::test]
async fn later_scans_read_only_what_is_new() {
    let asked = Arc::new(Mutex::new(0));
    let behaviour = Behaviour {
        recording: TRACK,
        ..GOOD
    };
    let server = liteserver(31, behaviour, Arc::clone(&asked)).await;
    let chain = TonChain::new(network(config(&[server], mainnet()).await)).unwrap();
    let earlier = ScanCursor {
        next: 109_579_107_000_018,
        complete_from: 5,
        complete_since: 7,
    };
    let later = chain
        .scan(&ton(SENDER), &[gram()], Some(earlier))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        later.cursor,
        ScanCursor {
            next: 110_383_004_000_041,
            ..earlier
        }
    );
    assert_eq!(
        summed(&later.found),
        (12, ruint::aliases::U256::from(1_100_001u64))
    );
    assert_eq!(*asked.lock().unwrap(), 4);
    assert_eq!(
        chain
            .scan(&ton(SENDER), &[gram()], Some(later.cursor))
            .await,
        Ok(None)
    );
    assert_eq!(*asked.lock().unwrap(), 6);
    let tokens_only = &assets()[1..];
    assert_eq!(chain.scan(&ton(SENDER), tokens_only, None).await, Ok(None));
    assert_eq!(*asked.lock().unwrap(), 6);
    let lying = tracking_chain(Behaviour {
        tamper: Tamper::History,
        ..GOOD
    })
    .await;
    assert!(matches!(
        lying.scan(&ton(SENDER), &[gram()], Some(earlier)).await,
        Err(ChainError::Network(_))
    ));
}
