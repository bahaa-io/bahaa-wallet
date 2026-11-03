use bahaa_chain::{ChainError, Dialect, Failure, JsonRpc};
use bahaa_types::{Address, ChainFamily, to_plain_text};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};

use crate::tx::Key;

const MAX_MESSAGE: usize = 200;

pub(crate) type Rpc = JsonRpc<Solana>;

#[derive(Debug)]
pub(crate) struct Solana {
    pub(crate) genesis: String,
}

impl Dialect for Solana {
    const CHECK: &'static str = "getGenesisHash";

    fn check(&self, reply: &Value) -> Result<(), ChainError> {
        if reply.as_str() == Some(self.genesis.as_str()) {
            Ok(())
        } else {
            Err(ChainError::Network(
                "an endpoint serves another Solana cluster".to_owned(),
            ))
        }
    }

    fn classify(&self, error: &Value) -> Failure<ChainError> {
        let message = error["message"].as_str().unwrap_or("unknown error");
        let detail = &error["data"]["err"];
        let message = match detail {
            Value::Null => message.to_owned(),
            Value::String(text) => format!("{message}: {text}"),
            other => format!("{message}: {other}"),
        };
        classify(error["code"].as_i64().unwrap_or(0), &message)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Account {
    pub lamports: u64,
    pub owner: Key,
    pub executable: bool,
    pub data: Vec<u8>,
}

pub(crate) fn classify(code: i64, message: &str) -> Failure<ChainError> {
    let message = to_plain_text(message, MAX_MESSAGE);
    let lower = message.to_ascii_lowercase();
    if ["rate limit", "too many requests"]
        .iter()
        .any(|needle| lower.contains(needle))
    {
        return Failure::RateLimited(ChainError::Network(message), None);
    }
    if matches!(code, -32001 | -32007 | -32009 | -32011) || lower.contains("cleaned up") {
        return Failure::Unsupported(ChainError::Pruned);
    }
    if code == -32601
        || ["personal token", "request blocked", "method not found"]
            .iter()
            .any(|needle| lower.contains(needle))
    {
        return Failure::Unsupported(ChainError::Network(message));
    }
    if lower.contains("already been processed") {
        return Failure::Unsupported(ChainError::Network(
            "the node has already seen this transaction".to_owned(),
        ));
    }
    if matches!(code, -32603 | -32004 | -32005 | -32014 | -32016) {
        return Failure::Retry(ChainError::Network(message));
    }
    Failure::Fatal(ChainError::Rejected(message))
}

pub(crate) fn text(key: &Key) -> String {
    Address::Solana(*key).to_string()
}

pub(crate) fn key(value: &Value) -> Result<Key, ChainError> {
    match value
        .as_str()
        .map(|text| Address::parse(ChainFamily::Solana, text))
    {
        Some(Ok(Address::Solana(key))) => Ok(key),
        _ => Err(ChainError::malformed()),
    }
}

pub(crate) fn count(value: &Value) -> Result<u64, ChainError> {
    value.as_u64().ok_or_else(ChainError::malformed)
}

pub(crate) fn accounts_params(keys: &[Key], length: Option<usize>) -> Value {
    let mut options = json!({"encoding": "base64", "commitment": "confirmed"});
    if let Some(length) = length {
        options["dataSlice"] = json!({"offset": 0, "length": length});
    }
    json!([keys.iter().map(text).collect::<Vec<_>>(), options])
}

pub(crate) fn accounts(reply: &Value, asked: usize) -> Result<Vec<Option<Account>>, ChainError> {
    let list = reply["value"]
        .as_array()
        .filter(|list| list.len() == asked)
        .ok_or_else(ChainError::malformed)?;
    list.iter()
        .map(|entry| match entry {
            Value::Null => Ok(None),
            entry => Ok(Some(Account {
                lamports: count(&entry["lamports"])?,
                owner: key(&entry["owner"])?,
                executable: entry["executable"]
                    .as_bool()
                    .ok_or_else(ChainError::malformed)?,
                data: data(&entry["data"])?,
            })),
        })
        .collect()
}

fn data(value: &Value) -> Result<Vec<u8>, ChainError> {
    match value.as_array().map(Vec::as_slice) {
        Some([Value::String(encoded), Value::String(kind)]) if kind == "base64" => STANDARD
            .decode(encoded)
            .map_err(|_| ChainError::malformed()),
        _ => Err(ChainError::malformed()),
    }
}

#[cfg(fuzzing)]
pub(crate) fn fuzz(value: &Value) {
    let _ = classify(
        value["code"].as_i64().unwrap_or_default(),
        value["message"].as_str().unwrap_or_default(),
    );
    let _ = accounts(value, value["value"].as_array().map_or(0, Vec::len));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(failure: &Failure<ChainError>) -> &'static str {
        match failure {
            Failure::Retry(_) => "retry",
            Failure::RateLimited(..) => "limited",
            Failure::Unreachable(_) => "unreachable",
            Failure::Unsupported(ChainError::Pruned) => "pruned",
            Failure::Unsupported(_) => "unsupported",
            Failure::Fatal(_) => "fatal",
        }
    }

    #[test]
    fn classifies_what_the_public_servers_answer() {
        let cases = [
            (
                -32001,
                "Block 464183995 cleaned up, does not exist on node. First available block: 464184163",
                "pruned",
            ),
            (
                -32011,
                "Transaction history is not available from this node",
                "pruned",
            ),
            (
                -32602,
                "Indexed requests require a personal token. Get one at: https://www.allnodes.com/publicnode",
                "unsupported",
            ),
            (-32602, "Request blocked", "unsupported"),
            (-32601, "Method not found", "unsupported"),
            (-32005, "Node is unhealthy", "retry"),
            (-32016, "Minimum context slot has not been reached", "retry"),
            (429, "Too many requests for a specific RPC call", "limited"),
            (
                -32002,
                "Transaction simulation failed: This transaction has already been processed",
                "unsupported",
            ),
            (
                -32002,
                "Transaction simulation failed: Attempt to debit an account but found no record of a prior credit.",
                "fatal",
            ),
            (
                -32602,
                "failed to deserialize solana_transaction::versioned::VersionedTransaction",
                "fatal",
            ),
        ];
        for (code, message, expected) in cases {
            assert_eq!(kind(&classify(code, message)), expected, "{message}");
        }
    }

    #[test]
    fn the_reason_a_node_gives_apart_is_kept() {
        let official = json!({"code": -32002, "message": "Transaction simulation failed", "data": {"err": "AccountNotFound", "logs": []}});
        let Failure::Fatal(ChainError::Rejected(reason)) = Solana {
            genesis: String::new(),
        }
        .classify(&official) else {
            panic!("expected a refusal");
        };
        assert_eq!(reason, "Transaction simulation failed: AccountNotFound");
    }

    #[test]
    fn accounts_are_read_strictly() {
        let owner = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
        let reply = json!({"context": {"slot": 1}, "value": [
            null,
            {"lamports": 2_039_280, "owner": owner, "executable": false, "data": ["AQID", "base64"], "space": 3}
        ]});
        let read = accounts(&reply, 2).unwrap();
        assert_eq!(read[0], None);
        let account = read[1].as_ref().unwrap();
        assert_eq!(account.lamports, 2_039_280);
        assert_eq!(text(&account.owner), owner);
        assert_eq!(account.data, [1, 2, 3]);
        assert!(accounts(&reply, 3).is_err());
        for (field, bad) in [
            ("lamports", json!(-1)),
            ("owner", json!("0x00")),
            ("executable", json!("no")),
            ("data", json!(["AQID", "base58"])),
            ("data", json!(["!!", "base64"])),
            ("data", json!("AQID")),
        ] {
            let mut broken = reply.clone();
            broken["value"][1][field] = bad;
            assert!(accounts(&broken, 2).is_err(), "{field}");
        }
    }

    #[test]
    fn account_requests_name_keys_in_base58() {
        let params = accounts_params(&[[0; 32]], Some(0));
        assert_eq!(params[0][0], "11111111111111111111111111111111");
        assert_eq!(params[1]["dataSlice"]["length"], 0);
        assert!(accounts_params(&[], None)[1].get("dataSlice").is_none());
    }
}
