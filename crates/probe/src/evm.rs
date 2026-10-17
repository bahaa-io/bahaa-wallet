use anyhow::{Context, Result, bail, ensure};
use futures_util::future::join_all;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::clock::unix_now;
use crate::net::Net;
use crate::report::{self, Extras, Outcome, Report, timed};

const TRANSFER: &str = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";
const DECIMALS: &str = "0x313ce567";
const L1_BASE_FEE: &str = "0x519b4bd3";
const GAS_PRICE_ORACLE: &str = "0x420000000000000000000000000000000000000F";
const FROM: &str = "0x1000000000000000000000000000000000000001";
const TO: &str = "0x000000000000000000000000000000000000dEaD";
const STALE_AFTER: u64 = 120;
const LOG_BLOCKS: u128 = 10;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    Standard,
    OpStack,
}

struct Token {
    symbol: &'static str,
    address: &'static str,
    decimals: u8,
}

struct Network {
    name: &'static str,
    chain_id: u64,
    family: Family,
    token: Token,
    rpcs: &'static [(&'static str, &'static str)],
}

const NETWORKS: [Network; 7] = [
    Network {
        name: "bsc",
        chain_id: 56,
        family: Family::Standard,
        token: Token {
            symbol: "USDC",
            address: "0x8AC76a51cc950d9822D68b83fE1Ad97B32Cd580d",
            decimals: 18,
        },
        rpcs: &[
            ("official", "https://bsc-dataseed.bnbchain.org"),
            ("publicnode", "https://bsc-rpc.publicnode.com"),
            ("drpc", "https://bsc.drpc.org"),
            ("1rpc", "https://1rpc.io/bnb"),
            ("llamarpc", "https://binance.llamarpc.com"),
        ],
    },
    Network {
        name: "ethereum",
        chain_id: 1,
        family: Family::Standard,
        token: Token {
            symbol: "USDC",
            address: "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
            decimals: 6,
        },
        rpcs: &[
            ("publicnode", "https://ethereum-rpc.publicnode.com"),
            ("drpc", "https://eth.drpc.org"),
            ("1rpc", "https://1rpc.io/eth"),
            ("llamarpc", "https://eth.llamarpc.com"),
            ("merkle", "https://eth.merkle.io"),
            ("cloudflare", "https://cloudflare-eth.com"),
        ],
    },
    Network {
        name: "polygon",
        chain_id: 137,
        family: Family::Standard,
        token: Token {
            symbol: "USDC",
            address: "0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359",
            decimals: 6,
        },
        rpcs: &[
            ("official", "https://polygon-rpc.com"),
            ("publicnode", "https://polygon-bor-rpc.publicnode.com"),
            ("drpc", "https://polygon.drpc.org"),
            ("1rpc", "https://1rpc.io/matic"),
        ],
    },
    Network {
        name: "arbitrum",
        chain_id: 42161,
        family: Family::Standard,
        token: Token {
            symbol: "USDC",
            address: "0xaf88d065e77c8cC2239327C5EDb3A432268e5831",
            decimals: 6,
        },
        rpcs: &[
            ("official", "https://arb1.arbitrum.io/rpc"),
            ("publicnode", "https://arbitrum-one-rpc.publicnode.com"),
            ("drpc", "https://arbitrum.drpc.org"),
            ("1rpc", "https://1rpc.io/arb"),
        ],
    },
    Network {
        name: "avalanche",
        chain_id: 43114,
        family: Family::Standard,
        token: Token {
            symbol: "USDC",
            address: "0xB97EF9Ef8734C71904D8002F8b6Bc66Dd9c48a6E",
            decimals: 6,
        },
        rpcs: &[
            ("official", "https://api.avax.network/ext/bc/C/rpc"),
            ("publicnode", "https://avalanche-c-chain-rpc.publicnode.com"),
            ("drpc", "https://avalanche.drpc.org"),
            ("1rpc", "https://1rpc.io/avax/c"),
        ],
    },
    Network {
        name: "base",
        chain_id: 8453,
        family: Family::OpStack,
        token: Token {
            symbol: "USDC",
            address: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
            decimals: 6,
        },
        rpcs: &[
            ("official", "https://mainnet.base.org"),
            ("publicnode", "https://base-rpc.publicnode.com"),
            ("drpc", "https://base.drpc.org"),
            ("1rpc", "https://1rpc.io/base"),
            ("llamarpc", "https://base.llamarpc.com"),
        ],
    },
    Network {
        name: "optimism",
        chain_id: 10,
        family: Family::OpStack,
        token: Token {
            symbol: "USDC",
            address: "0x0b2C639c533813f4Aa9D7837CAf62653d097Ff85",
            decimals: 6,
        },
        rpcs: &[
            ("official", "https://mainnet.optimism.io"),
            ("publicnode", "https://optimism-rpc.publicnode.com"),
            ("drpc", "https://optimism.drpc.org"),
            ("1rpc", "https://1rpc.io/op"),
        ],
    },
];

#[derive(Deserialize)]
struct Reply {
    result: Option<Value>,
    error: Option<RpcError>,
}

#[derive(Deserialize)]
struct RpcError {
    code: i64,
    message: String,
}

impl Reply {
    fn into_result(self) -> Result<Value> {
        if let Some(error) = self.error {
            bail!("{} {}", error.code, error.message);
        }
        self.result.context("reply without a result")
    }
}

struct Rpc<'a> {
    net: &'a Net,
    url: &'a str,
}

impl Rpc<'_> {
    async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let reply: Reply = self
            .post(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}))
            .await?;
        reply.into_result()
    }

    async fn post<T: DeserializeOwned>(&self, body: &Value) -> Result<T> {
        let response = self.net.http.post(self.url).json(body).send().await?;
        let status = response.status();
        let bytes = response.bytes().await?;
        serde_json::from_slice(&bytes).with_context(|| format!("HTTP {status}"))
    }
}

pub async fn run(net: &Net, report: &mut Report) {
    for network in &NETWORKS {
        report::section(&format!("evm {} ({})", network.name, network.chain_id));
        let checks = network
            .rpcs
            .iter()
            .map(|(_, url)| timed(endpoint(Rpc { net, url }, network)));
        let results = join_all(checks).await;
        for ((label, _), (elapsed, result)) in network.rpcs.iter().zip(results) {
            report.record(label, elapsed, result);
        }
    }
}

async fn endpoint(rpc: Rpc<'_>, network: &Network) -> Result<Outcome> {
    let chain_id = quantity(&rpc.call("eth_chainId", json!([])).await?)?;
    ensure!(
        chain_id == u128::from(network.chain_id),
        "wrong chain id {chain_id}, expected {}",
        network.chain_id
    );
    let (latency, block) = timed(rpc.call("eth_getBlockByNumber", json!(["latest", false]))).await;
    let block = block?;
    let head = quantity(&block["number"])?;
    let stamp = u64::try_from(quantity(&block["timestamp"])?)?;
    let age = unix_now()?.saturating_sub(stamp);
    ensure!(age <= STALE_AFTER, "head block {head} is {age} s old");
    let gas_price = quantity(&rpc.call("eth_gasPrice", json!([])).await?)?;
    let base_fee = match block.get("baseFeePerGas") {
        Some(fee) => format!("{} gwei", gwei(quantity(fee)?)),
        None => "none".to_owned(),
    };
    let token = network.token.address;
    let decimals = quantity(
        &rpc.call(
            "eth_call",
            json!([{"to": token, "data": DECIMALS}, "latest"]),
        )
        .await?,
    )?;
    ensure!(
        decimals == u128::from(network.token.decimals),
        "{} reports {decimals} decimals, expected {}",
        network.token.symbol,
        network.token.decimals
    );
    let mut extras = Extras::default();
    extras.add("estimate", estimate(&rpc).await);
    extras.add("history", fee_history(&rpc).await);
    extras.add("logs", logs(&rpc, token, head).await);
    extras.add("batch", batch(&rpc).await);
    if network.family == Family::OpStack {
        extras.add("l1 base", l1_base_fee(&rpc).await);
    }
    let mut outcome = Outcome::new(format!(
        "{} ms, head {age} s old, gas {} gwei, base fee {base_fee}",
        latency.as_millis(),
        gwei(gas_price)
    ));
    outcome.extras(extras);
    Ok(outcome)
}

async fn estimate(rpc: &Rpc<'_>) -> Result<String> {
    let gas = rpc
        .call(
            "eth_estimateGas",
            json!([{"from": FROM, "to": TO, "value": "0x0"}]),
        )
        .await?;
    Ok(quantity(&gas)?.to_string())
}

async fn fee_history(rpc: &Rpc<'_>) -> Result<String> {
    rpc.call("eth_feeHistory", json!(["0x4", "latest", [50]]))
        .await?;
    Ok("yes".to_owned())
}

async fn logs(rpc: &Rpc<'_>, token: &str, head: u128) -> Result<String> {
    let from = head.saturating_sub(LOG_BLOCKS - 1);
    let found = rpc
        .call(
            "eth_getLogs",
            json!([{
                "address": token,
                "topics": [TRANSFER],
                "fromBlock": format!("{from:#x}"),
                "toBlock": format!("{head:#x}"),
            }]),
        )
        .await?;
    let count = found.as_array().context("logs reply is not a list")?.len();
    Ok(format!("{count} in {LOG_BLOCKS} blocks"))
}

async fn batch(rpc: &Rpc<'_>) -> Result<String> {
    let replies: Vec<Reply> = rpc
        .post(&json!([
            {"jsonrpc": "2.0", "id": 1, "method": "eth_chainId", "params": []},
            {"jsonrpc": "2.0", "id": 2, "method": "eth_blockNumber", "params": []},
        ]))
        .await?;
    ensure!(replies.len() == 2, "{} replies to 2 calls", replies.len());
    for reply in replies {
        reply.into_result()?;
    }
    Ok("yes".to_owned())
}

async fn l1_base_fee(rpc: &Rpc<'_>) -> Result<String> {
    let fee = rpc
        .call(
            "eth_call",
            json!([{"to": GAS_PRICE_ORACLE, "data": L1_BASE_FEE}, "latest"]),
        )
        .await?;
    Ok(format!("{} gwei", gwei(quantity(&fee)?)))
}

fn quantity(value: &Value) -> Result<u128> {
    let text = value.as_str().context("expected a hex string")?;
    let digits = text.strip_prefix("0x").context("hex string without 0x")?;
    ensure!(!digits.is_empty(), "empty result");
    Ok(u128::from_str_radix(digits, 16)?)
}

fn gwei(wei: u128) -> String {
    let whole = wei / 1_000_000_000;
    let fraction = wei % 1_000_000_000;
    if fraction == 0 {
        whole.to_string()
    } else {
        format!("{whole}.{fraction:09}")
            .trim_end_matches('0')
            .to_owned()
    }
}
