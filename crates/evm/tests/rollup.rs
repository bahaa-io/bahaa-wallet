#![allow(clippy::unwrap_used)]

mod common;

use std::sync::{Arc, Mutex};

use alloy_consensus::{SignableTransaction, TxEnvelope};
use alloy_eips::eip2718::Decodable2718;
use bahaa_chain::{Chain, ChainError, FeeItem, Transfer, TxId, TxStatus};
use bahaa_crypto::Mnemonic;
use bahaa_evm::EvmChain;
use bahaa_types::{Address, Amount, ChainFamily};
use common::{Log, network_on, node, owner, result, word};
use ruint::aliases::U256;
use serde_json::{Value, json};
use url::Url;

const ORACLE: &str = "0x420000000000000000000000000000000000000f";
const GET_L1_FEE: &str = "0x49948e0e";
const GET_OPERATOR_FEE: &str = "0x275aedd2";
const BASE_FEE: u128 = 5_000_000;
const TIP: u128 = 1_000_000;
const ESTIMATE: u128 = 30_000;
const L1_FEE: u128 = 1_530_000_000;
const OPERATOR_PER_GAS: u128 = 7;
const AMOUNT: u128 = 250_000_000_000_000_000;

type Calls = Arc<Mutex<Vec<Value>>>;

fn hex_of(value: u128) -> String {
    format!("0x{value:x}")
}

fn unhex(text: &str) -> Vec<u8> {
    let digits = text.trim_start_matches("0x");
    (0..digits.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&digits[at..at + 2], 16).unwrap())
        .collect()
}

fn last_word(data: &str) -> u128 {
    u128::from_str_radix(&data[data.len() - 32..], 16).unwrap()
}

fn oracle(params: &Value) -> (u16, Value) {
    let data = params[0]["data"].as_str().unwrap();
    let reverted = (
        200,
        json!({"jsonrpc": "2.0", "id": 1, "error": {"code": 3, "message": "execution reverted"}}),
    );
    if params[0]["to"] != ORACLE {
        return reverted;
    }
    if data.starts_with(GET_L1_FEE) {
        result(&json!(word(L1_FEE)))
    } else if data.starts_with(GET_OPERATOR_FEE) {
        result(&json!(word(last_word(data) * OPERATOR_PER_GAS)))
    } else {
        reverted
    }
}

async fn fake(chain_id: &str, balance: u128, receipt: Value) -> (Url, Calls) {
    let calls = Calls::default();
    let seen = Arc::clone(&calls);
    let chain_id = chain_id.to_owned();
    let url = node(Log::default(), move |method, params| match method {
        "eth_chainId" => result(&json!(chain_id)),
        "eth_getBalance" => result(&json!(hex_of(balance))),
        "eth_getTransactionCount" => result(&json!("0x7")),
        "eth_estimateGas" => result(&json!(hex_of(ESTIMATE))),
        "eth_feeHistory" => result(&json!({"baseFeePerGas": [hex_of(BASE_FEE), hex_of(BASE_FEE)]})),
        "eth_maxPriorityFeePerGas" => result(&json!(hex_of(TIP))),
        "eth_getTransactionReceipt" => result(&receipt),
        "eth_call" => {
            seen.lock().unwrap().push(params.clone());
            oracle(params)
        }
        _ => (502, json!("bad gateway")),
    })
    .await;
    (url, calls)
}

fn send_eth(network: &str) -> Transfer {
    Transfer {
        account: 0,
        from: owner(),
        to: Address::Evm([9; 20]),
        asset: network.parse().unwrap(),
        amount: Amount::new(U256::from(AMOUNT), 18).unwrap(),
    }
}

fn receipt(extra: &Value) -> Value {
    let mut receipt = json!({
        "status": "0x1",
        "blockNumber": "0x10",
        "gasUsed": "0x5208",
        "effectiveGasPrice": hex_of(BASE_FEE + TIP)
    });
    for (field, value) in extra.as_object().unwrap() {
        receipt[field] = value.clone();
    }
    receipt
}

fn wei(value: u128) -> Amount {
    Amount::new(U256::from(value), 18).unwrap()
}

#[tokio::test]
async fn a_base_quote_prices_the_exact_transaction_on_l1_and_adds_the_operator_fee() {
    let gas_limit = ESTIMATE + ESTIMATE / 5;
    let price = BASE_FEE + TIP;
    let expected = ESTIMATE * price + L1_FEE + ESTIMATE * OPERATOR_PER_GAS;
    let max = gas_limit * (2 * BASE_FEE + TIP) + 2 * L1_FEE + gas_limit * OPERATOR_PER_GAS;
    let (url, calls) = fake("0x2105", AMOUNT + max, Value::Null).await;
    let chain = EvmChain::new(network_on("base", &[url])).unwrap();
    let transfer = send_eth("base");
    let (plan, quote) = chain.prepare(&transfer).await.unwrap();
    assert_eq!((quote.expected(), quote.max()), (wei(expected), wei(max)));
    let lines: Vec<(FeeItem, Amount)> = quote
        .lines()
        .iter()
        .map(|line| (line.item.clone(), line.amount))
        .collect();
    assert_eq!(
        lines,
        [
            (
                FeeItem::Gas {
                    units: 30_000,
                    price: wei(price)
                },
                wei(ESTIMATE * price)
            ),
            (FeeItem::L1Data, wei(L1_FEE)),
            (FeeItem::Operator, wei(ESTIMATE * OPERATOR_PER_GAS)),
        ]
    );
    let seed = Mnemonic::parse("test test test test test test test test test test test junk")
        .unwrap()
        .to_seed("");
    let signed = chain.sign(&plan, &seed).unwrap();
    let envelope = TxEnvelope::decode_2718(&mut &signed.bytes[..]).unwrap();
    let unsigned = envelope.as_eip1559().unwrap().tx().encoded_for_signing();
    let calls = calls.lock().unwrap();
    assert!(calls.iter().all(|params| params[1] == "latest"));
    let data: Vec<&str> = calls
        .iter()
        .map(|params| params[0]["data"].as_str().unwrap())
        .collect();
    let priced = unhex(data[0]);
    let length = usize::try_from(last_word(&data[0][..2 + 2 * 68])).unwrap();
    assert_eq!(&priced[..4], unhex(GET_L1_FEE));
    assert_eq!(priced[68..68 + length], unsigned);
    assert_eq!(
        data[1..]
            .iter()
            .map(|data| (data[..10].to_owned(), last_word(data)))
            .collect::<Vec<_>>(),
        [
            (GET_OPERATOR_FEE.to_owned(), ESTIMATE),
            (GET_OPERATOR_FEE.to_owned(), gas_limit)
        ]
    );
}

#[tokio::test]
async fn the_balance_must_cover_the_rollup_fees_too() {
    let gas_limit = ESTIMATE + ESTIMATE / 5;
    let max = gas_limit * (2 * BASE_FEE + TIP) + 2 * L1_FEE + gas_limit * OPERATOR_PER_GAS;
    let (url, _) = fake("0x2105", AMOUNT + max - 1, Value::Null).await;
    let chain = EvmChain::new(network_on("base", &[url])).unwrap();
    let Err(ChainError::Insufficient {
        symbol,
        needed,
        available,
    }) = chain.prepare(&send_eth("base")).await
    else {
        panic!("expected a refusal");
    };
    assert_eq!(
        (symbol.as_str(), needed, available),
        ("ETH", wei(AMOUNT + max), wei(AMOUNT + max - 1))
    );
}

#[tokio::test]
async fn a_confirmed_base_transfer_counts_the_l1_and_operator_fees_it_paid() {
    let id = TxId::new(format!("0x{}", "ab".repeat(32))).unwrap();
    let gas = 21_000 * (BASE_FEE + TIP);
    let cases = [
        (json!({"l1Fee": hex_of(L1_FEE)}), Some(gas + L1_FEE), 0),
        (
            json!({"l1Fee": hex_of(L1_FEE), "operatorFeeScalar": "0x0", "operatorFeeConstant": "0x0"}),
            Some(gas + L1_FEE),
            0,
        ),
        (
            json!({"l1Fee": hex_of(L1_FEE), "operatorFeeScalar": "0x3e8"}),
            Some(gas + L1_FEE + 21_000 * OPERATOR_PER_GAS),
            1,
        ),
        (
            json!({"l1Fee": hex_of(L1_FEE), "operatorFeeConstant": "0xzz"}),
            None,
            0,
        ),
        (json!({}), None, 0),
    ];
    for (extra, fee, oracle_calls) in cases {
        let (url, calls) = fake("0x2105", 0, receipt(&extra)).await;
        let chain = EvmChain::new(network_on("base", &[url])).unwrap();
        let status = chain.status(&id, &common::owner(), None).await.unwrap();
        assert_eq!(
            status,
            TxStatus::Confirmed {
                block: Some(16),
                fee: fee.map(wei)
            },
            "{extra}"
        );
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), oracle_calls, "{extra}");
        assert!(calls.iter().all(|params| params[1] == "0x10"));
    }
}

#[tokio::test]
async fn other_networks_never_ask_the_oracle() {
    let (url, calls) = fake(
        "0x38",
        u128::MAX / 4,
        receipt(&json!({"l1Fee": hex_of(L1_FEE)})),
    )
    .await;
    let chain = EvmChain::new(network_on("bsc", &[url])).unwrap();
    let (_, quote) = chain.prepare(&send_eth("bsc")).await.unwrap();
    assert_eq!(quote.lines().len(), 1);
    assert_eq!(quote.expected(), wei(ESTIMATE * (BASE_FEE + TIP)));
    let id = TxId::new(format!("0x{}", "ab".repeat(32))).unwrap();
    assert_eq!(
        chain.status(&id, &common::owner(), None).await.unwrap(),
        TxStatus::Confirmed {
            block: Some(16),
            fee: Some(wei(21_000 * (BASE_FEE + TIP)))
        }
    );
    assert_eq!(*calls.lock().unwrap(), Vec::<Value>::new());
}

#[tokio::test]
#[ignore = "talks to the real Base and OP Mainnet networks"]
async fn live_base_and_optimism_quotes_include_the_l1_fee() {
    let registry = bahaa_config::Registry::builtin().unwrap();
    let evm = |text| Address::parse(ChainFamily::Evm, text).unwrap();
    for id in ["base", "optimism"] {
        let chain = EvmChain::new(registry.get(&id.parse().unwrap()).unwrap().clone()).unwrap();
        let transfer = Transfer {
            account: 0,
            from: evm("0x4200000000000000000000000000000000000016"),
            to: evm("0x000000000000000000000000000000000000dEaD"),
            asset: id.parse().unwrap(),
            amount: wei(1),
        };
        let (_, quote) = chain.prepare(&transfer).await.unwrap();
        let items: Vec<&FeeItem> = quote.lines().iter().map(|line| &line.item).collect();
        assert!(
            matches!(
                items[..],
                [FeeItem::Gas { units: 21_000, .. }, FeeItem::L1Data]
                    | [
                        FeeItem::Gas { units: 21_000, .. },
                        FeeItem::L1Data,
                        FeeItem::Operator
                    ]
            ),
            "{id}: {items:?}"
        );
        let l1 = quote.lines()[1].amount;
        assert!(!l1.is_zero(), "{id}: no L1 fee");
        assert!(l1 < Amount::parse("0.0001", 18).unwrap(), "{id}: {l1}");
        assert!(quote.expected() <= quote.max());
    }
}
