#![allow(clippy::unwrap_used)]

mod common;

use std::fmt::Write;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use bahaa_chain::{Chain, ChainError, FeeItem, Signed, Transfer, TxId, TxStatus, check_signed};
use bahaa_crypto::{Mnemonic, Seed};
use bahaa_tron::TronChain;
use bahaa_types::{Address, Amount};
use common::{GENESIS, Log, OWNER, OWNER_HEX, USDT, USDT_HEX, node, tron, tron_on};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use url::Url;

const KNOWN: &str = "TPjjvMwjPoDC32V2dGDYTkLH4E5LAtBZ6C";
const KNOWN_HEX: &str = "419705bf55c3dcc6d277ebb8fe2a68762268822ba2";
const FRESH: &str = "TB7mhtkvfhsRBRhe5FuRa4tFXSEyGDe4eA";
const HEAD: u64 = 87_210_587;

struct Ledger {
    trx: u64,
    usdt: u64,
    resources: Value,
    head_age_ms: i64,
    broadcast: Mutex<Value>,
    info: Mutex<Value>,
    pending: Mutex<bool>,
    seen: Mutex<Vec<(String, Value)>>,
}

impl Ledger {
    fn new(trx: u64, usdt: u64, resources: Value) -> Arc<Self> {
        Arc::new(Self {
            trx,
            usdt,
            resources,
            head_age_ms: 0,
            broadcast: Mutex::new(Value::Null),
            info: Mutex::new(json!({})),
            pending: Mutex::new(false),
            seen: Mutex::new(Vec::new()),
        })
    }

    fn asked(&self, path: &str) -> Vec<Value> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(seen, _)| seen == path)
            .map(|(_, body)| body.clone())
            .collect()
    }
}

fn now_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

fn word(value: u64) -> String {
    format!("{value:064x}")
}

fn parameters() -> Value {
    let list: Vec<Value> = [
        ("getTransactionFee", 1_000),
        ("getEnergyFee", 100),
        ("getCreateAccountFee", 100_000),
        ("getCreateNewAccountFeeInSystemContract", 1_000_000),
        ("getMaxFeeLimit", 15_000_000_000_u64),
        ("getFreeNetLimit", 600),
    ]
    .into_iter()
    .map(|(key, value)| json!({"key": key, "value": value}))
    .collect();
    json!({"chainParameter": list})
}

fn txid_of(bytes: &[u8]) -> String {
    assert_eq!(bytes[0], 0x0a);
    let (mut length, mut at, mut shift) = (0usize, 1, 0);
    loop {
        length |= usize::from(bytes[at] & 0x7f) << shift;
        shift += 7;
        at += 1;
        if bytes[at - 1] & 0x80 == 0 {
            break;
        }
    }
    let digest = Sha256::digest(&bytes[at..at + length]);
    hex(&digest)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
        .collect()
}

fn answer(ledger: &Ledger, path: &str, body: &Value) -> Value {
    ledger
        .seen
        .lock()
        .unwrap()
        .push((path.to_owned(), body.clone()));
    match path {
        "/wallet/getblockbynum" => json!({"blockID": GENESIS}),
        "/wallet/getblock" => json!({
            "blockID": format!("{HEAD:016x}{}", "ab".repeat(24)),
            "block_header": {"raw_data": {"number": HEAD, "timestamp": now_ms() - ledger.head_age_ms}}
        }),
        "/wallet/getaccount" => match body["address"].as_str().unwrap() {
            OWNER_HEX => json!({"address": OWNER_HEX, "balance": ledger.trx}),
            KNOWN_HEX => json!({"address": KNOWN_HEX, "create_time": 1}),
            _ => json!({}),
        },
        "/wallet/getaccountresource" => ledger.resources.clone(),
        "/wallet/getchainparameters" => parameters(),
        "/wallet/triggerconstantcontract" => {
            let energy = if body["data"]
                .as_str()
                .unwrap_or("")
                .contains(&KNOWN_HEX[2..])
            {
                64_285
            } else {
                130_285
            };
            json!({
                "result": {"result": true},
                "energy_used": energy,
                "constant_result": [word(ledger.usdt)],
                "transaction": {"ret": [{}]}
            })
        }
        "/wallet/broadcasthex" => {
            let reply = ledger.broadcast.lock().unwrap().clone();
            if reply.is_null() {
                let id = txid_of(&unhex(body["transaction"].as_str().unwrap()));
                json!({"result": true, "txid": id})
            } else {
                reply
            }
        }
        "/wallet/gettransactioninfobyid" => ledger.info.lock().unwrap().clone(),
        "/wallet/gettransactionfrompending" if *ledger.pending.lock().unwrap() => {
            json!({"txID": body["value"]})
        }
        "/wallet/gettransactionfrompending" => json!({}),
        _ => json!({"Error": "unexpected path"}),
    }
}

async fn serve(ledger: &Arc<Ledger>) -> Url {
    let ledger = Arc::clone(ledger);
    node(Log::default(), move |path, body| {
        (200, answer(&ledger, path, body))
    })
    .await
}

async fn chain(ledger: &Arc<Ledger>) -> TronChain {
    TronChain::new(tron_on(&[serve(ledger).await])).unwrap()
}

fn seed() -> Seed {
    Mnemonic::parse("test test test test test test test test test test test junk")
        .unwrap()
        .to_seed("")
}

fn send(to: &str, asset: &str, amount: &str) -> Transfer {
    Transfer {
        account: 0,
        from: tron(OWNER),
        to: tron(to),
        asset: asset.parse().unwrap(),
        amount: Amount::parse(amount, 6).unwrap(),
        max: false,
    }
}

fn trx(sun: u64) -> Amount {
    Amount::new(sun.try_into().unwrap(), 6).unwrap()
}

fn lines(quote: &bahaa_chain::FeeQuote) -> Vec<(FeeItem, Amount)> {
    quote
        .lines()
        .iter()
        .map(|line| (line.item.clone(), line.amount))
        .collect()
}

fn bandwidth_of(quote: &bahaa_chain::FeeQuote) -> u64 {
    match quote.lines()[0].item {
        FeeItem::Bandwidth { bytes } => bytes,
        _ => panic!("no bandwidth line: {quote:?}"),
    }
}

#[tokio::test]
async fn trx_to_an_active_account_rides_on_free_bandwidth_and_signs_what_was_quoted() {
    let ledger = Ledger::new(
        5_000_000,
        0,
        json!({"freeNetLimit": 600, "freeNetUsed": 100}),
    );
    let chain = chain(&ledger).await;
    let transfer = send(KNOWN, "tron", "1.5");
    let (plan, quote) = chain.prepare(&transfer).await.unwrap();
    let bytes = bandwidth_of(&quote);
    assert!((250..300).contains(&bytes), "{bytes}");
    assert_eq!(lines(&quote), [(FeeItem::Bandwidth { bytes }, trx(0))]);
    assert_eq!((quote.expected(), quote.max()), (trx(0), trx(0)));
    let signed = chain.sign(&plan, &seed()).unwrap();
    assert_eq!(signed.id.as_str(), txid_of(&signed.bytes));
    assert_eq!(u64::try_from(signed.bytes.len()).unwrap() + 64, bytes);
    let decoded = chain.decode(&signed).unwrap();
    check_signed(&transfer, &decoded).unwrap();
    assert_eq!(decoded.sequence, None);
    let expires = decoded.expires_at.unwrap();
    let now = u64::try_from(now_ms() / 1000).unwrap();
    assert!(
        (now + 290..=now + 300).contains(&expires),
        "{expires} vs {now}"
    );
    assert_eq!(ledger.asked("/wallet/getaccount")[1]["address"], KNOWN_HEX);
    let mut wrong = signed.clone();
    wrong.id = TxId::new("ab".repeat(32)).unwrap();
    assert_eq!(
        chain.decode(&wrong),
        Err(ChainError::Mismatch("the transaction id"))
    );
}

#[tokio::test]
async fn a_new_account_costs_activation_and_the_balance_must_cover_it() {
    let quoted = |trx_held| async move {
        let ledger = Ledger::new(trx_held, 0, json!({"freeNetLimit": 600}));
        chain(&ledger)
            .await
            .prepare(&send(FRESH, "tron", "1.5"))
            .await
    };
    let (_, quote) = quoted(5_000_000).await.unwrap();
    let bytes = bandwidth_of(&quote);
    assert_eq!(
        lines(&quote),
        [
            (FeeItem::Bandwidth { bytes }, trx(100_000)),
            (FeeItem::Activation, trx(1_000_000))
        ]
    );
    assert_eq!(
        (quote.expected(), quote.max()),
        (trx(1_100_000), trx(1_100_000))
    );
    assert_eq!(
        quoted(2_599_999).await.unwrap_err(),
        ChainError::Insufficient {
            symbol: "TRX".to_owned(),
            needed: trx(2_600_000),
            available: trx(2_599_999)
        }
    );
    let ledger = Ledger::new(5_000_000, 0, json!({}));
    assert!(matches!(
        chain(&ledger)
            .await
            .prepare(&send(OWNER, "tron", "1"))
            .await,
        Err(ChainError::Invalid(_))
    ));
}

#[tokio::test]
async fn a_usdt_quote_simulates_the_exact_call_and_caps_energy() {
    let usdt = format!("tron/{USDT}");
    let resources = json!({"freeNetLimit": 600, "EnergyLimit": 60_000, "EnergyUsed": 10_000});
    let ledger = Ledger::new(20_000_000, 10_000_000, resources.clone());
    let usdt_chain = chain(&ledger).await;
    let transfer = send(FRESH, &usdt, "2.5");
    let (plan, quote) = usdt_chain.prepare(&transfer).await.unwrap();
    let bytes = bandwidth_of(&quote);
    assert_eq!(
        lines(&quote),
        [
            (FeeItem::Bandwidth { bytes }, trx(0)),
            (FeeItem::Energy { units: 130_285 }, trx(8_028_500))
        ]
    );
    assert_eq!(quote.max(), trx(10_634_200));
    let simulated = ledger
        .asked("/wallet/triggerconstantcontract")
        .into_iter()
        .find(|body| body.get("data").is_some())
        .unwrap();
    let fresh_hex = match tron(FRESH) {
        Address::Tron(bytes) => hex(&bytes),
        _ => unreachable!(),
    };
    assert_eq!(
        simulated,
        json!({
            "owner_address": OWNER_HEX,
            "contract_address": USDT_HEX,
            "data": format!("a9059cbb{:0>64}{}", fresh_hex, word(2_500_000))
        })
    );
    let decoded = usdt_chain
        .decode(&usdt_chain.sign(&plan, &seed()).unwrap())
        .unwrap();
    check_signed(&transfer, &decoded).unwrap();
    let short_of_usdt = send(FRESH, &usdt, "11");
    let Err(ChainError::Insufficient { symbol, .. }) = usdt_chain.prepare(&short_of_usdt).await
    else {
        panic!("expected a refusal");
    };
    assert_eq!(symbol, "USDT");
    let poor = Ledger::new(10_634_199, 10_000_000, resources);
    assert_eq!(
        chain(&poor).await.prepare(&transfer).await.unwrap_err(),
        ChainError::Insufficient {
            symbol: "TRX".to_owned(),
            needed: trx(10_634_200),
            available: trx(10_634_199)
        }
    );
}

fn everything(to: &str, asset: &str) -> Transfer {
    Transfer {
        amount: Amount::default(),
        max: true,
        ..send(to, asset, "1")
    }
}

async fn most(trx_held: u64, usdt_held: u64, resources: Value, transfer: &Transfer) -> Transfer {
    let ledger = Ledger::new(trx_held, usdt_held, resources);
    let chain = chain(&ledger).await;
    let (plan, quote) = chain.prepare(transfer).await.unwrap();
    let planned = chain.planned(&plan).clone();
    let signed = chain.sign(&plan, &seed()).unwrap();
    let decoded = chain.decode(&signed).unwrap();
    check_signed(&planned, &decoded).unwrap();
    assert!(planned.max && !decoded.everything);
    assert_eq!(
        u64::try_from(signed.bytes.len()).unwrap() + 64,
        bandwidth_of(&quote)
    );
    if planned.asset.contract().is_none() {
        assert_eq!(planned.amount.checked_add(quote.max()), Ok(trx(trx_held)));
    }
    planned
}

#[tokio::test]
async fn a_max_send_leaves_exactly_the_fee_behind() {
    let free = json!({"freeNetLimit": 600, "freeNetUsed": 100});
    let rides_free = most(5_000_000, 0, free.clone(), &everything(KNOWN, "tron")).await;
    assert_eq!(rides_free.amount, trx(5_000_000));
    let burns = most(5_000_000, 0, json!({}), &everything(KNOWN, "tron")).await;
    assert!(
        (4_700_000..4_750_000).contains(&u64::try_from(burns.amount.units()).unwrap()),
        "{burns:?}"
    );
    let activates = most(5_000_000, 0, free.clone(), &everything(FRESH, "tron")).await;
    assert_eq!(activates.amount, trx(3_900_000));
    let usdt = format!("tron/{USDT}");
    let tokens = most(
        20_000_000,
        2_500_000,
        free.clone(),
        &everything(FRESH, &usdt),
    )
    .await;
    assert_eq!(tokens.amount, trx(2_500_000));
    let refused = |trx_held: u64, usdt_held: u64, transfer: Transfer| {
        let resources = free.clone();
        async move {
            let ledger = Ledger::new(trx_held, usdt_held, resources);
            chain(&ledger).await.prepare(&transfer).await.unwrap_err()
        }
    };
    assert_eq!(
        refused(1_100_000, 0, everything(FRESH, "tron")).await,
        ChainError::Insufficient {
            symbol: "TRX".to_owned(),
            needed: trx(1_100_001),
            available: trx(1_100_000)
        }
    );
    let nothing = ChainError::Invalid("there is nothing to send");
    assert_eq!(refused(0, 0, everything(KNOWN, "tron")).await, nothing);
    assert_eq!(
        refused(5_000_000, 0, everything(KNOWN, &usdt)).await,
        nothing
    );
    assert!(matches!(
        refused(5_000_000, 0, everything(OWNER, "tron")).await,
        ChainError::Invalid(_)
    ));
}

#[tokio::test]
async fn a_stale_head_block_is_not_used_as_the_anchor() {
    let mut ledger = Ledger::new(5_000_000, 0, json!({"freeNetLimit": 600}));
    Arc::get_mut(&mut ledger).unwrap().head_age_ms = 5 * 60 * 1000;
    assert!(matches!(
        chain(&ledger).await.prepare(&send(KNOWN, "tron", "1")).await,
        Err(ChainError::Network(message)) if message.contains("behind")
    ));
}

async fn signed(chain: &TronChain) -> Signed {
    let (plan, _) = chain.prepare(&send(KNOWN, "tron", "1")).await.unwrap();
    chain.sign(&plan, &seed()).unwrap()
}

#[tokio::test]
async fn broadcasting_follows_what_the_node_says() {
    let ledger = Ledger::new(5_000_000, 0, json!({"freeNetLimit": 600}));
    let chain = chain(&ledger).await;
    let signed = signed(&chain).await;
    assert_eq!(chain.broadcast(&signed).await.unwrap(), signed.id);
    let sent = ledger.asked("/wallet/broadcasthex");
    assert_eq!(
        unhex(sent[0]["transaction"].as_str().unwrap()),
        signed.bytes
    );
    let replies = [
        (
            json!({"result": false, "code": "DUP_TRANSACTION_ERROR"}),
            Err(ChainError::Network(
                "network problem: the node has already seen this transaction".to_owned(),
            )),
        ),
        (
            json!({"result": false, "code": "CONTRACT_VALIDATE_ERROR", "message": "Contract validate error : balance is not sufficient."}),
            Err(ChainError::Rejected(
                "Contract validate error : balance is not sufficient.".to_owned(),
            )),
        ),
        (
            json!({"result": false, "code": "SIGERROR", "message": "56616c6964617465207369676e6174757265206572726f72"}),
            Err(ChainError::Rejected("Validate signature error".to_owned())),
        ),
        (
            json!({"result": false, "code": "TAPOS_ERROR"}),
            Err(ChainError::Rejected("TAPOS_ERROR".to_owned())),
        ),
    ];
    for (reply, expected) in replies {
        *ledger.broadcast.lock().unwrap() = reply.clone();
        assert_eq!(chain.broadcast(&signed).await, expected, "{reply}");
    }
    *ledger.broadcast.lock().unwrap() = json!({"result": true, "txid": "00".repeat(32)});
    assert!(matches!(
        chain.broadcast(&signed).await,
        Err(ChainError::Network(_))
    ));
}

#[tokio::test]
async fn a_busy_node_hands_the_broadcast_to_the_next_endpoint() {
    let (busy, calm) = (
        Ledger::new(5_000_000, 0, json!({"freeNetLimit": 600})),
        Ledger::new(5_000_000, 0, json!({"freeNetLimit": 600})),
    );
    *busy.broadcast.lock().unwrap() =
        json!({"result": false, "code": "SERVER_BUSY", "message": "Server busy"});
    let chain = TronChain::new(tron_on(&[serve(&busy).await, serve(&calm).await])).unwrap();
    let signed = signed(&chain).await;
    assert_eq!(chain.broadcast(&signed).await.unwrap(), signed.id);
    assert_eq!(busy.asked("/wallet/broadcasthex").len(), 1);
    assert_eq!(calm.asked("/wallet/broadcasthex").len(), 1);
}

#[tokio::test]
async fn a_broadcast_reaches_every_node_and_one_acceptance_is_enough() {
    let (first, second) = (
        Ledger::new(5_000_000, 0, json!({"freeNetLimit": 600})),
        Ledger::new(5_000_000, 0, json!({"freeNetLimit": 600})),
    );
    let chain = TronChain::new(tron_on(&[serve(&first).await, serve(&second).await])).unwrap();
    let signed = signed(&chain).await;
    assert_eq!(chain.broadcast(&signed).await.unwrap(), signed.id);
    assert_eq!(first.asked("/wallet/broadcasthex").len(), 1);
    assert_eq!(second.asked("/wallet/broadcasthex").len(), 1);
    *first.broadcast.lock().unwrap() = json!({"result": false, "code": "DUP_TRANSACTION_ERROR"});
    assert_eq!(chain.broadcast(&signed).await.unwrap(), signed.id);
    *second.broadcast.lock().unwrap() =
        json!({"result": false, "code": "TAPOS_ERROR", "message": "Tapos check error"});
    assert_eq!(
        chain.broadcast(&signed).await,
        Err(ChainError::Rejected("Tapos check error".to_owned()))
    );
}

#[tokio::test]
async fn the_status_comes_from_the_receipt_or_the_pending_pool() {
    let ledger = Ledger::new(5_000_000, 0, json!({}));
    let chain = chain(&ledger).await;
    let id = TxId::new("cd".repeat(32)).unwrap();
    assert_eq!(
        chain.status(&id, &tron(OWNER), None).await.unwrap(),
        TxStatus::NotFound
    );
    *ledger.pending.lock().unwrap() = true;
    assert_eq!(
        chain.status(&id, &tron(OWNER), None).await.unwrap(),
        TxStatus::Pending
    );
    let receipts = [
        (
            json!({"id": id.as_str(), "blockNumber": 87_210_600, "fee": 1_100_000, "receipt": {"net_fee": 100_000}}),
            TxStatus::Confirmed {
                block: Some(87_210_600),
                fee: Some(trx(1_100_000)),
            },
        ),
        (
            json!({"id": id.as_str(), "blockNumber": 87_210_601, "receipt": {"net_usage": 265}}),
            TxStatus::Confirmed {
                block: Some(87_210_601),
                fee: Some(trx(0)),
            },
        ),
        (
            json!({"id": id.as_str(), "blockNumber": 87_210_602, "fee": 15_634_200, "result": "FAILED", "receipt": {"result": "OUT_OF_ENERGY"}}),
            TxStatus::Failed("the transaction ran out of energy".to_owned()),
        ),
        (
            json!({"id": id.as_str(), "blockNumber": 87_210_603, "receipt": {"result": "REVERT"}}),
            TxStatus::Failed("the transaction reverted".to_owned()),
        ),
    ];
    for (info, expected) in receipts {
        *ledger.info.lock().unwrap() = info.clone();
        assert_eq!(
            chain.status(&id, &tron(OWNER), None).await.unwrap(),
            expected,
            "{info}"
        );
    }
    *ledger.info.lock().unwrap() = json!({"id": "ef".repeat(32), "blockNumber": 1});
    assert!(matches!(
        chain.status(&id, &tron(OWNER), None).await,
        Err(ChainError::Network(_))
    ));
}

#[tokio::test]
#[ignore = "talks to the real Tron network"]
async fn live_tron_quotes_for_a_new_recipient() {
    let registry = bahaa_config::Registry::builtin().unwrap();
    let chain = TronChain::new(registry.get(&"tron".parse().unwrap()).unwrap().clone()).unwrap();
    let busy = tron("TQrY8tryqsYVCYS3MFbtffiPp2ccyn4STm");
    let fresh = bahaa_types::Address::Tron([
        0x5c, 0x9d, 0x2f, 0x41, 0x66, 0x13, 0x37, 0xa8, 0x0e, 0x71, 0xc4, 0x58, 0x2b, 0x9a, 0xf0,
        0x06, 0xd3, 0x4e, 0x81, 0x27,
    ]);
    let coins = Transfer {
        account: 0,
        from: busy,
        to: fresh,
        asset: "tron".parse().unwrap(),
        amount: Amount::parse("1", 6).unwrap(),
        max: false,
    };
    let (_, quote) = chain.prepare(&coins).await.unwrap();
    let items: Vec<&FeeItem> = quote.lines().iter().map(|line| &line.item).collect();
    assert!(
        matches!(items[..], [FeeItem::Bandwidth { bytes }, FeeItem::Activation] if (250..300).contains(bytes)),
        "{quote:?}"
    );
    assert!(
        quote.expected() >= Amount::parse("1", 6).unwrap(),
        "{quote:?}"
    );
    let tokens = Transfer {
        asset: format!("tron/{USDT}").parse().unwrap(),
        ..coins
    };
    let (_, quote) = chain.prepare(&tokens).await.unwrap();
    let items: Vec<&FeeItem> = quote.lines().iter().map(|line| &line.item).collect();
    assert!(
        matches!(items[..], [FeeItem::Bandwidth { .. }, FeeItem::Energy { units }] if (100_000..200_000).contains(units)),
        "{quote:?}"
    );
    assert!(quote.expected() <= quote.max(), "{quote:?}");
    let everything = Transfer {
        amount: Amount::default(),
        max: true,
        ..coins
    };
    let (plan, quote) = chain.prepare(&everything).await.unwrap();
    let planned = chain.planned(&plan);
    assert!(planned.max && !planned.amount.is_zero(), "{planned:?}");
    assert!(quote.max() >= Amount::parse("1", 6).unwrap(), "{quote:?}");
}
