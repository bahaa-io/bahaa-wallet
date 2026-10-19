#![allow(clippy::unwrap_used)]

mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use bahaa_chain::{ChainError, Incoming, ScanCursor};
use bahaa_config::Feature;
use bahaa_tron::TronChain;
use common::{GENESIS, Log, USDT, USDT_HEX, assets, node, tron, tron_on};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use url::Url;

const US: &str = "TQrY8tryqsYVCYS3MFbtffiPp2ccyn4STm";
const US_HEX: &str = "41a346f2bd7d43a5d90b7c57a18196be96b2840e61";
const SENDER_HEX: &str = "41421f99b9529484b7e863e1fd0399194f85a1cf9e";
const OTHER_HEX: &str = "419705bf55c3dcc6d277ebb8fe2a68762268822ba2";
const TRANSFER: &str = "ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";
const DAY_MS: u64 = 24 * 60 * 60 * 1000;

fn now_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

fn hex(bytes: &[u8]) -> String {
    bahaa_types::hex::encode(bytes)
}

fn unhex(text: &str) -> Vec<u8> {
    bahaa_types::hex::decode(text.trim().trim_start_matches("0x")).unwrap()
}

fn varint(mut value: u64, out: &mut Vec<u8>) {
    loop {
        let low = u8::try_from(value & 0x7f).unwrap();
        value >>= 7;
        if value == 0 {
            out.push(low);
            return;
        }
        out.push(low | 0x80);
    }
}

fn number(tag: u64, value: u64, out: &mut Vec<u8>) {
    varint(tag << 3, out);
    varint(value, out);
}

fn bytes(tag: u64, data: &[u8], out: &mut Vec<u8>) {
    varint(tag << 3 | 2, out);
    varint(u64::try_from(data.len()).unwrap(), out);
    out.extend_from_slice(data);
}

fn raw_transfer(to: &str, sun: u64, memo: Option<&[u8]>) -> Vec<u8> {
    raw_between(SENDER_HEX, to, sun, memo)
}

fn raw_between(from: &str, to: &str, sun: u64, memo: Option<&[u8]>) -> Vec<u8> {
    let mut transfer = Vec::new();
    bytes(1, &unhex(from), &mut transfer);
    bytes(2, &unhex(to), &mut transfer);
    number(3, sun, &mut transfer);
    let mut any = Vec::new();
    bytes(
        1,
        b"type.googleapis.com/protocol.TransferContract",
        &mut any,
    );
    bytes(2, &transfer, &mut any);
    let mut contract = Vec::new();
    number(1, 1, &mut contract);
    bytes(2, &any, &mut contract);
    let mut raw = Vec::new();
    bytes(1, &[0x12, 0x34], &mut raw);
    bytes(4, &[7; 8], &mut raw);
    number(8, 1_792_000_060_000, &mut raw);
    if let Some(memo) = memo {
        bytes(10, memo, &mut raw);
    }
    bytes(11, &contract, &mut raw);
    number(14, 1_792_000_000_000, &mut raw);
    raw
}

fn coin_entry(raw: &[u8], time: u64, block: u64) -> Value {
    json!({
        "txID": hex(&Sha256::digest(raw)),
        "raw_data_hex": hex(raw),
        "raw_data": {"contract": [{"type": "TransferContract"}]},
        "ret": [{"contractRet": "SUCCESS"}],
        "blockNumber": block,
        "block_timestamp": time
    })
}

fn word(hex41: &str) -> String {
    format!("{:0>64}", &hex41[2..])
}

fn transfer_log(token: &str, to: &str, units: u64) -> Value {
    log_between(token, SENDER_HEX, to, units)
}

fn log_between(token: &str, from: &str, to: &str, units: u64) -> Value {
    json!({
        "address": &token[2..],
        "topics": [TRANSFER, word(from), word(to)],
        "data": format!("{units:064x}")
    })
}

struct History {
    start: u64,
    coins: Vec<Value>,
    tokens: Vec<Value>,
    receipts: BTreeMap<String, Value>,
    queries: Mutex<Vec<BTreeMap<String, String>>>,
}

fn query(path: &str) -> (String, BTreeMap<String, String>) {
    let url = Url::parse(&format!("http://fake{path}")).unwrap();
    (
        url.path().to_owned(),
        url.query_pairs().into_owned().collect(),
    )
}

fn serve(history: &History, path: &str, body: &Value) -> (u16, Value) {
    if path == "/wallet/getblockbynum" {
        return (200, json!({"blockID": GENESIS}));
    }
    if path == "/wallet/gettransactioninfobyid" {
        let id = body["value"].as_str().unwrap();
        return (
            200,
            history
                .receipts
                .get(id)
                .cloned()
                .unwrap_or_else(|| json!({})),
        );
    }
    let (route, asked) = query(path);
    history.queries.lock().unwrap().push(asked.clone());
    let from: u64 = asked["min_timestamp"].parse().unwrap();
    let after = |entries: &[Value]| -> Vec<Value> {
        entries
            .iter()
            .filter(|entry| entry["block_timestamp"].as_u64().unwrap() >= from)
            .cloned()
            .collect()
    };
    let page = |data: Vec<Value>, fingerprint: Option<&str>| {
        let mut meta = json!({"at": now_ms(), "page_size": data.len()});
        if let Some(fingerprint) = fingerprint {
            meta["fingerprint"] = json!(fingerprint);
        }
        (200, json!({"data": data, "success": true, "meta": meta}))
    };
    if route == format!("/v1/accounts/{US}/transactions") {
        let entries = after(&history.coins);
        let (first, rest) = entries.split_at(entries.len().min(3));
        return match asked.get("fingerprint").map(String::as_str) {
            None if !rest.is_empty() => page(first.to_vec(), Some("Second")),
            None => page(first.to_vec(), None),
            Some("Second") => page(rest.to_vec(), None),
            Some(_) => (400, json!({"success": false, "error": "bad fingerprint"})),
        };
    }
    if route == format!("/v1/accounts/{US}/transactions/trc20") {
        assert_eq!(asked["contract_address"], USDT);
        return page(after(&history.tokens), None);
    }
    (404, json!("missing"))
}

fn history() -> History {
    let start = now_ms() - 3 * DAY_MS;
    let paid = raw_transfer(US_HEX, 1_500_000, None);
    let memo = raw_transfer(US_HEX, 2_000_000, Some(b"deposit 7731"));
    let elsewhere = raw_transfer(OTHER_HEX, 9_000_000, None);
    let mut tampered = coin_entry(&raw_transfer(US_HEX, 5_000_000, None), start + 4, 104);
    tampered["raw_data_hex"] = json!(hex(&raw_transfer(US_HEX, 50_000_000, None)));
    let mut delegation = coin_entry(&raw_transfer(US_HEX, 1, None), start + 5, 105);
    delegation["raw_data"]["contract"][0]["type"] = json!("DelegateResourceContract");
    let paying = raw_between(US_HEX, OTHER_HEX, 4_000_000, None);
    let coins = vec![
        coin_entry(&paid, start + 1, 101),
        coin_entry(&memo, start + 2, 102),
        coin_entry(&elsewhere, start + 3, 103),
        tampered,
        delegation,
        coin_entry(&paying, start + 6, 106),
    ];
    let (single, double, reverted) = ("a1".repeat(32), "b2".repeat(32), "c3".repeat(32));
    let listed = |id: &str, time: u64| json!({"transaction_id": id, "block_timestamp": time, "to": US, "type": "Transfer", "value": "1"});
    let tokens = vec![
        listed(&single, start + 10),
        listed(&double, start + 20),
        listed(&double, start + 20),
        listed(&reverted, start + 30),
    ];
    let receipt = |id: &str, block: u64, time: u64, result: &str, logs: Vec<Value>| json!({"id": id, "blockNumber": block, "blockTimeStamp": time, "receipt": {"result": result}, "log": logs});
    let receipts = BTreeMap::from([
        (
            single.clone(),
            receipt(
                &single,
                110,
                start + 10,
                "SUCCESS",
                vec![
                    transfer_log(OTHER_HEX, US_HEX, 99),
                    transfer_log(USDT_HEX, US_HEX, 2_500_000),
                ],
            ),
        ),
        (
            double.clone(),
            receipt(
                &double,
                120,
                start + 20,
                "SUCCESS",
                vec![
                    transfer_log(USDT_HEX, US_HEX, 1_000_000),
                    transfer_log(USDT_HEX, OTHER_HEX, 7),
                    transfer_log(USDT_HEX, US_HEX, 3_000_000),
                    log_between(USDT_HEX, US_HEX, OTHER_HEX, 600_000),
                ],
            ),
        ),
        (
            reverted.clone(),
            receipt(
                &reverted,
                130,
                start + 30,
                "REVERT",
                vec![transfer_log(USDT_HEX, US_HEX, 5)],
            ),
        ),
    ]);
    History {
        start,
        coins,
        tokens,
        receipts,
        queries: Mutex::new(Vec::new()),
    }
}

async fn chain(history: &Arc<History>, plain: Log) -> TronChain {
    let shared = Arc::clone(history);
    let indexed = node(Log::default(), move |path, body| serve(&shared, path, body)).await;
    let other = node(plain, |path, _| match path {
        "/wallet/getblockbynum" => (200, json!({"blockID": GENESIS})),
        _ => (500, json!("not me")),
    })
    .await;
    let mut network = tron_on(&[indexed, other]);
    network.endpoints[0].features = vec![Feature::History];
    TronChain::new(network).unwrap()
}

fn summary(found: &[Incoming]) -> Vec<(String, u64, String, u64, u64)> {
    found
        .iter()
        .map(|entry| {
            (
                entry.asset.to_string(),
                entry.index,
                entry.amount.to_string(),
                entry.block,
                entry.time,
            )
        })
        .collect()
}

#[tokio::test]
async fn the_first_scan_reads_verified_trx_and_known_token_transfers_to_us() {
    let history = Arc::new(history());
    let plain = Log::default();
    let chain = chain(&history, Arc::clone(&plain)).await;
    let wanted = assets(&["tron", &format!("tron/{USDT}")]);
    let scanned = chain.scan(&tron(US), &wanted, None).await.unwrap().unwrap();
    let start = history.start;
    let usdt = format!("tron/{USDT}");
    let second = |offset: u64| (start + offset) / 1000;
    assert_eq!(
        summary(&scanned.found),
        [
            ("tron".to_owned(), 0, "1.5".to_owned(), 101, second(1)),
            ("tron".to_owned(), 0, "2".to_owned(), 102, second(2)),
            (usdt.clone(), 1, "2.5".to_owned(), 110, second(10)),
            (usdt.clone(), 0, "1".to_owned(), 120, second(20)),
            (usdt, 2, "3".to_owned(), 120, second(20)),
        ]
    );
    assert!(scanned.found.iter().all(|entry| entry.to == tron(US)));
    let sent: Vec<(String, u64, String, u64)> = scanned
        .sent
        .iter()
        .map(|entry| {
            (
                entry.asset.to_string(),
                entry.index,
                entry.amount.to_string(),
                entry.block,
            )
        })
        .collect();
    assert_eq!(
        sent,
        [
            ("tron".to_owned(), 0, "4".to_owned(), 106),
            (format!("tron/{USDT}"), 3, "0.6".to_owned(), 120),
        ]
    );
    assert!(scanned.sent.iter().all(|entry| entry.from == tron(US)));
    assert_eq!(
        scanned.sent[0].to,
        bahaa_types::Address::Tron(unhex(&OTHER_HEX[2..]).try_into().unwrap())
    );
    assert_eq!(
        scanned.found[0].from,
        bahaa_types::Address::Tron(unhex(&SENDER_HEX[2..]).try_into().unwrap())
    );
    assert_eq!(
        scanned.found[1].id.as_str(),
        hex(&Sha256::digest(raw_transfer(
            US_HEX,
            2_000_000,
            Some(b"deposit 7731")
        )))
    );
    let cursor = scanned.cursor;
    let settled = now_ms() - 5 * 60 * 1000;
    assert!(cursor.next.abs_diff(settled) < 60_000, "{cursor:?}");
    let reach = now_ms() - 30 * DAY_MS;
    assert!(cursor.complete_from.abs_diff(reach) < 60_000, "{cursor:?}");
    assert_eq!(cursor.complete_since, cursor.complete_from / 1000);
    let queries = history.queries.lock().unwrap().clone();
    assert_eq!(queries.len(), 6);
    let sides: Vec<(Option<&str>, Option<&str>)> = queries
        .iter()
        .map(|asked| {
            (
                asked.get("only_to").map(String::as_str),
                asked.get("only_from").map(String::as_str),
            )
        })
        .collect();
    let (to, from) = ((Some("true"), None), (None, Some("true")));
    assert_eq!(sides, [to, to, from, from, to, from]);
    for asked in &queries {
        assert_eq!(asked["min_timestamp"], cursor.complete_from.to_string());
        assert_eq!(asked["only_confirmed"], "true");
    }
    assert_eq!(queries[1]["fingerprint"], "Second");
    assert_eq!(queries[3]["fingerprint"], "Second");
    assert_eq!(*plain.lock().unwrap(), Vec::<String>::new());
    let again = chain
        .scan(&tron(US), &wanted, Some(cursor))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again.found, Vec::<Incoming>::new());
    assert!(again.cursor.next >= cursor.next);
    assert_eq!(again.cursor.complete_from, cursor.complete_from);
    let latest = history.queries.lock().unwrap().clone();
    assert_eq!(latest.len(), 10);
    assert!(
        latest[6..]
            .iter()
            .all(|asked| asked["min_timestamp"] == cursor.next.to_string())
    );
}

#[tokio::test]
async fn a_long_history_is_read_a_few_pages_per_refresh() {
    let start = now_ms() - DAY_MS;
    let pages = Arc::new(Mutex::new(0u64));
    let counted = Arc::clone(&pages);
    let endless = node(Log::default(), move |path, _| {
        if path == "/wallet/getblockbynum" {
            return (200, json!({"blockID": GENESIS}));
        }
        let mut seen = counted.lock().unwrap();
        *seen += 1;
        let raw = raw_transfer(US_HEX, 1_000 + *seen, None);
        (
            200,
            json!({"data": [coin_entry(&raw, start + *seen, 100 + *seen)], "success": true, "meta": {"fingerprint": format!("Page{seen}")}}),
        )
    })
    .await;
    let mut network = tron_on(&[endless]);
    network.endpoints[0].features = vec![Feature::History];
    let chain = TronChain::new(network).unwrap();
    let cursor = ScanCursor {
        next: start,
        complete_from: start - 9,
        complete_since: (start - 9) / 1000,
    };
    let scanned = chain
        .scan(&tron(US), &assets(&["tron"]), Some(cursor))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(*pages.lock().unwrap(), 10);
    assert_eq!(scanned.found.len(), 5);
    assert_eq!(scanned.sent, Vec::new());
    assert_eq!(
        scanned.cursor,
        ScanCursor {
            next: start + 5,
            ..cursor
        }
    );
}

#[tokio::test]
async fn a_refusing_history_service_is_an_error_not_an_empty_history() {
    let refusing = node(Log::default(), |path, _| match path {
        "/wallet/getblockbynum" => (200, json!({"blockID": GENESIS})),
        _ => (
            200,
            json!({"success": false, "error": "A valid account address is required."}),
        ),
    })
    .await;
    let mut network = tron_on(&[refusing]);
    network.endpoints[0].features = vec![Feature::History];
    let chain = TronChain::new(network).unwrap();
    assert!(matches!(
        chain.scan(&tron(US), &assets(&["tron"]), None).await,
        Err(ChainError::Network(message)) if message.contains("valid account")
    ));
    let without = TronChain::new(tron_on(&[])).unwrap();
    assert!(!without.can_scan());
}

#[tokio::test]
#[ignore = "talks to the real Tron network"]
async fn live_tron_incoming_trx_and_usdt() {
    let registry = bahaa_config::Registry::builtin().unwrap();
    let chain = TronChain::new(registry.get(&"tron".parse().unwrap()).unwrap().clone()).unwrap();
    assert!(chain.can_scan());
    let busy = tron("TQrY8tryqsYVCYS3MFbtffiPp2ccyn4STm");
    let from = now_ms() - 6 * 60 * 60 * 1000;
    let cursor = ScanCursor {
        next: from,
        complete_from: from,
        complete_since: from / 1000,
    };
    let scanned = chain
        .scan(
            &busy,
            &assets(&["tron", &format!("tron/{USDT}")]),
            Some(cursor),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(scanned.cursor.next >= from);
    assert_eq!(scanned.cursor.complete_from, from);
    assert!(!scanned.found.is_empty(), "nothing arrived in six hours");
    for entry in &scanned.found {
        assert_eq!(entry.to, busy);
        assert!(!entry.amount.is_zero());
        assert!(entry.time * 1000 >= from - 1000, "{entry:?}");
        assert!(entry.block > 87_000_000);
    }
    assert!(!scanned.sent.is_empty(), "nothing was sent in six hours");
    for entry in &scanned.sent {
        assert_eq!(entry.from, busy);
        assert_ne!(entry.to, busy);
        assert!(!entry.amount.is_zero());
        assert!(entry.time * 1000 >= from - 1000, "{entry:?}");
    }
}
