use bahaa_chain::{ChainError, Dialect, Failure, JsonRpc};
use bahaa_types::to_plain_text;
use ruint::aliases::U256;
use serde_json::Value;

const MAX_MESSAGE: usize = 200;

pub(crate) type Rpc = JsonRpc<Evm>;

#[derive(Debug)]
pub(crate) struct Evm {
    pub(crate) chain_id: u64,
}

impl Dialect for Evm {
    const CHECK: &'static str = "eth_chainId";

    fn check(&self, reply: &Value) -> Result<(), ChainError> {
        let reported = quantity(reply)?;
        if reported == U256::from(self.chain_id) {
            Ok(())
        } else {
            Err(ChainError::Network(format!(
                "an endpoint reports chain {reported}, expected {}",
                self.chain_id
            )))
        }
    }

    fn classify(&self, error: &Value) -> Failure<ChainError> {
        classify(
            error["code"].as_i64().unwrap_or(0),
            error["message"].as_str().unwrap_or("unknown error"),
        )
    }
}

pub(crate) fn classify(code: i64, message: &str) -> Failure<ChainError> {
    let message = to_plain_text(message, MAX_MESSAGE);
    let lower = message.to_ascii_lowercase();
    let limited = code == -32005
        || [
            "rate limit",
            "too many requests",
            "limit exceeded",
            "limit reached",
            "upgrade to paid",
        ]
        .iter()
        .any(|needle| lower.contains(needle));
    if limited {
        return Failure::RateLimited(ChainError::Network(message), None);
    }
    if ["archive", "personal token", "pruned", "missing trie node"]
        .iter()
        .any(|needle| lower.contains(needle))
    {
        return Failure::Unsupported(ChainError::Pruned);
    }
    let unsupported = code == -32601
        || [
            "block range",
            "blocks range",
            "free plan",
            "specify an address",
            "does not exist",
            "method not found",
        ]
        .iter()
        .any(|needle| lower.contains(needle));
    if unsupported {
        return Failure::Unsupported(ChainError::Network(message));
    }
    if matches!(code, -32603 | -32001 | -32002 | -32004) {
        return Failure::Retry(ChainError::Network(message));
    }
    Failure::Fatal(ChainError::Rejected(message))
}

pub(crate) fn quantity(value: &Value) -> Result<U256, ChainError> {
    let invalid = || ChainError::Network("an endpoint sent a malformed number".to_owned());
    let digits = value
        .as_str()
        .and_then(|text| text.strip_prefix("0x"))
        .filter(|digits| !digits.is_empty() && digits.len() <= 64)
        .ok_or_else(invalid)?;
    U256::from_str_radix(digits, 16).map_err(|_| invalid())
}

#[cfg(test)]
mod tests {
    use bahaa_testkit::kind;
    use serde_json::json;

    use super::*;

    #[test]
    fn classifies_node_errors_like_phase_zero_saw_them() {
        let cases = [
            (-32005, "limit exceeded", "limited"),
            (-32011, "request limit reached", "limited"),
            (
                15,
                "You reached Public endpoint rate limit, please upgrade to paid plan",
                "limited",
            ),
            (
                -32003,
                "Rate limit exceeded on Nodies public endpoints",
                "limited",
            ),
            (
                -32601,
                "the method eth_getLogs does not exist",
                "unsupported",
            ),
            (
                -32602,
                "Archive requests require a personal token. Get one at: https://www.allnodes.com/publicnode",
                "pruned",
            ),
            (
                -32701,
                "Please specify an address in your request or, to remove restrictions, order a dedicated full node here: https://www.allnodes.com/bnb/host",
                "unsupported",
            ),
            (-32000, "missing trie node 5e1a", "pruned"),
            (-32701, "exceed maximum block range: 10000", "unsupported"),
            (
                -32602,
                "eth_getLogs is limited to 0 - 50 blocks range",
                "unsupported",
            ),
            (
                35,
                "ranges over 10000 blocks are not supported on free plan",
                "unsupported",
            ),
            (-32000, "transaction type not supported", "fatal"),
            (-32603, "Internal error", "retry"),
            (
                -32000,
                "insufficient funds for gas * price + value",
                "fatal",
            ),
            (-32000, "nonce too low", "fatal"),
            (3, "execution reverted", "fatal"),
        ];
        for (code, message, expected) in cases {
            assert_eq!(kind(&classify(code, message)), expected, "{message}");
        }
    }

    #[test]
    fn node_messages_arrive_as_plain_text() {
        let hostile = format!("nonce\u{202E} too\nlow\u{7}{}", "x".repeat(500));
        let Failure::Fatal(ChainError::Rejected(message)) = classify(-32000, &hostile) else {
            panic!("expected a rejection");
        };
        assert!(message.starts_with("nonce too low"));
        assert_eq!(message.chars().count(), MAX_MESSAGE);
        assert!(bahaa_types::is_plain_text(&message, MAX_MESSAGE));
    }

    #[test]
    fn parses_quantities_strictly() {
        assert_eq!(quantity(&json!("0x38")).unwrap(), U256::from(56u8));
        assert_eq!(quantity(&json!("0x0")).unwrap(), U256::ZERO);
        for bad in [
            json!("0x"),
            json!("38"),
            json!(56),
            json!(null),
            json!(format!("0x{}", "f".repeat(65))),
        ] {
            assert!(quantity(&bad).is_err(), "{bad}");
        }
    }
}
