use std::collections::{BTreeMap, BTreeSet};

use bahaa_types::{Price, PriceId};
use serde::Deserialize;
use serde_json::value::RawValue;
use serde_json::{Value, json};

use crate::MarketError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Quote {
    pub price: Price,
    pub updated_at: u64,
}

pub type Quotes = BTreeMap<PriceId, Quote>;

#[derive(Debug, Deserialize)]
pub(crate) struct Envelope {
    ok: bool,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    result: Option<Vec<Item>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Item {
    category: String,
    symbol: String,
    #[serde(default)]
    prices: BTreeMap<String, Box<RawValue>>,
    #[serde(default)]
    updated_at_unix: u64,
}

pub fn is_base(base: &str) -> bool {
    (3..=5).contains(&base.len()) && base.bytes().all(|byte| byte.is_ascii_uppercase())
}

pub fn condition(wanted: &BTreeSet<PriceId>) -> Value {
    let mut by_category: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for id in wanted {
        by_category
            .entry(id.category())
            .or_default()
            .push(id.symbol());
    }
    let groups: Vec<Value> = by_category
        .into_iter()
        .map(|(category, symbols)| {
            json!({
                "operation": "and",
                "conditions": [
                    {"operation": "equals", "left": "category", "right": category},
                    {"operation": "in", "left": "symbol", "right": symbols},
                ],
            })
        })
        .collect();
    json!({"operation": "or", "conditions": groups})
}

pub fn query(wanted: &BTreeSet<PriceId>, base: &str) -> Value {
    json!({
        "condition": condition(wanted),
        "bases": [base],
        "limit": wanted.len().max(1),
    })
}

pub(crate) fn read_envelope(body: &[u8]) -> Result<Vec<Item>, MarketError> {
    let envelope: Envelope =
        serde_json::from_slice(body).map_err(|err| MarketError::Format(err.to_string()))?;
    if !envelope.ok {
        return Err(MarketError::Api {
            code: envelope.code.unwrap_or_default(),
            description: envelope.description.unwrap_or_default(),
        });
    }
    envelope
        .result
        .ok_or_else(|| MarketError::Format("reply without a result".to_owned()))
}

pub(crate) fn quotes(items: Vec<Item>, base: &str, wanted: &BTreeSet<PriceId>) -> Quotes {
    items
        .into_iter()
        .filter_map(|item| {
            let id: PriceId = format!("{}/{}", item.category, item.symbol).parse().ok()?;
            if !wanted.contains(&id) {
                return None;
            }
            let price = Price::parse(item.prices.get(base)?.get()).ok()?;
            Some((
                id,
                Quote {
                    price,
                    updated_at: item.updated_at_unix,
                },
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(list: &[&str]) -> BTreeSet<PriceId> {
        list.iter().map(|id| id.parse().unwrap()).collect()
    }

    const REPLY: &str = r#"{"ok": true, "meta": {"count": 4}, "result": [
        {"category": "crypto", "symbol": "BNB", "name": "BNB", "base": "USDT", "price": 714.3,
         "prices": {"USDT": 714.3, "IRT": 82144500}, "updated_at_unix": 1792250000},
        {"category": "crypto", "symbol": "GRAM", "name": "Gram (prev. Toncoin)", "base": "USDT", "price": 1.3465,
         "prices": {"USDT": 1.3465}, "updated_at_unix": 1792250001},
        {"category": "crypto", "symbol": "PEPE", "name": "Pepe", "base": "USDT", "price": 0.000000000009222,
         "prices": {"USDT": 0.000000000009222}, "updated_at_unix": 1792250002},
        {"category": "crypto", "symbol": "EVIL\u001b[31m", "name": "x", "base": "USDT", "price": 1,
         "prices": {"USDT": 1}, "updated_at_unix": 1}
    ]}"#;

    #[test]
    fn asks_only_for_the_wanted_symbols() {
        let query = query(
            &ids(&["crypto/BNB", "crypto/GRAM", "iran_market/GOLD_18K"]),
            "USDT",
        );
        assert_eq!(query["bases"], json!(["USDT"]));
        assert_eq!(query["limit"], 3);
        let groups = query["condition"]["conditions"].as_array().unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0]["conditions"][0]["right"], "crypto");
        assert_eq!(groups[0]["conditions"][1]["right"], json!(["BNB", "GRAM"]));
        assert_eq!(groups[1]["conditions"][1]["right"], json!(["GOLD_18K"]));
    }

    #[test]
    fn reads_exact_prices_for_wanted_assets_only() {
        let wanted = ids(&["crypto/BNB", "crypto/PEPE"]);
        let quotes = quotes(read_envelope(REPLY.as_bytes()).unwrap(), "USDT", &wanted);
        assert_eq!(quotes.len(), 2);
        let bnb = quotes[&"crypto/BNB".parse::<PriceId>().unwrap()];
        assert_eq!(
            (bnb.price.to_string(), bnb.updated_at),
            ("714.3".to_owned(), 1_792_250_000)
        );
        let pepe = quotes[&"crypto/PEPE".parse::<PriceId>().unwrap()];
        assert_eq!(pepe.price.to_string(), "0.000000000009222");
    }

    #[test]
    fn skips_items_without_the_requested_base_or_with_odd_symbols() {
        let wanted = ids(&["crypto/BNB", "crypto/GRAM"]);
        let quotes = quotes(read_envelope(REPLY.as_bytes()).unwrap(), "IRT", &wanted);
        assert_eq!(quotes.len(), 1);
        assert_eq!(
            quotes.values().next().unwrap().price.to_string(),
            "82144500"
        );
    }

    #[test]
    fn api_errors_are_reported() {
        let err = read_envelope(
            br#"{"ok": false, "code": "BAD_CONDITION", "description": "unknown field"}"#,
        )
        .unwrap_err();
        assert_eq!(
            err,
            MarketError::Api {
                code: "BAD_CONDITION".to_owned(),
                description: "unknown field".to_owned()
            }
        );
        assert!(matches!(
            read_envelope(b"<html>"),
            Err(MarketError::Format(_))
        ));
    }

    #[test]
    fn validates_bases() {
        for base in ["USDT", "IRT", "EUR"] {
            assert!(is_base(base));
        }
        for base in ["", "usd", "US", "TOOLONG", "U$D"] {
            assert!(!is_base(base));
        }
    }
}
