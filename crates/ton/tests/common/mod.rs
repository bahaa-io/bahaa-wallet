#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use bahaa_config::{ChainKind, Checkpoint, Network};
use bahaa_testkit::Reply;
use serde_json::Value;
use url::Url;

pub type Log = Arc<Mutex<Vec<String>>>;

pub const BLOCKS: &str = include_str!("../fixtures/blocks.json");
const KEY_BLOCK: u32 = 99_746_941;
const KEY_ROOT: &str = "09b48141aa77df65bc7b237ffe04fa540981b73d7e8ce14951ec8c03c24715b9";
const KEY_FILE: &str = "1d0561589eaf050d1cf9d8f145df05766d0adfa559570eb017dcaf92d8e7f48e";

pub fn pinned(mut network: Network) -> Network {
    if let ChainKind::Ton { checkpoint, .. } = &mut network.kind {
        *checkpoint = Checkpoint {
            seqno: KEY_BLOCK,
            root_hash: unhex(KEY_ROOT).try_into().unwrap(),
            file_hash: unhex(KEY_FILE).try_into().unwrap(),
        };
    }
    network
}

pub fn anchor_proof(masterchain_info: &[u8]) -> Vec<u8> {
    let seqno = i32::from_le_bytes(masterchain_info[16..20].try_into().unwrap());
    recorded(BLOCKS, &["anchors", &seqno.to_string()]).unwrap()
}

pub fn unhex(text: &str) -> Vec<u8> {
    bahaa_types::hex::decode(text.trim().trim_start_matches("0x")).unwrap()
}

pub fn hex(bytes: &[u8]) -> String {
    bahaa_types::hex::encode(bytes)
}

pub async fn node(
    log: Log,
    reply: impl Fn(&str, &Value) -> (u16, Value) + Send + Sync + 'static,
) -> Url {
    bahaa_testkit::serve(move |request| {
        let path = request.path().to_owned();
        log.lock().unwrap().push(path.clone());
        let (status, answer) = reply(&path, &request.json());
        Reply::json(status, &answer)
    })
    .await
}

pub fn read_bytes(data: &[u8]) -> (&[u8], usize) {
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

pub fn recorded(recording: &str, path: &[&str]) -> Option<Vec<u8>> {
    let fixture: Value = serde_json::from_str(recording).unwrap();
    path.iter()
        .fold(&fixture, |value, key| &value[*key])
        .as_str()
        .map(unhex)
}
