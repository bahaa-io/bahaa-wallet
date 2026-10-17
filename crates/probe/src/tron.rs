use std::time::Duration;

use anyhow::{Context, Result, ensure};
use futures_util::future::join_all;
use serde_json::{Value, json};

use crate::clock::unix_now;
use crate::net::{Net, Paced};
use crate::report::{self, Extras, Outcome, Report, timed};

const USDT: &str = "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t";
const OWNER: &str = "T9yD14Nj9j7xAB4dbGeiX9h8unkKHxuWwb";
const GAP: Duration = Duration::from_millis(400);
const STALE_AFTER: u64 = 60;

const PROVIDERS: [(&str, &str); 3] = [
    ("trongrid", "https://api.trongrid.io"),
    ("publicnode", "https://tron-rpc.publicnode.com"),
    ("drpc", "https://tron.drpc.org"),
];

pub async fn run(net: &Net, report: &mut Report) {
    report::section("tron");
    let checks = PROVIDERS
        .iter()
        .map(|(_, base)| timed(provider(Paced::new(net, base, GAP))));
    let results = join_all(checks).await;
    for ((label, _), (elapsed, result)) in PROVIDERS.iter().zip(results) {
        report.record(label, elapsed, result);
    }
}

async fn provider(mut api: Paced<'_>) -> Result<Outcome> {
    let (latency, block) = timed(api.post("/wallet/getnowblock", &json!({}))).await;
    let block = block?;
    let header = &block["block_header"]["raw_data"];
    let head = header["number"]
        .as_u64()
        .context("block without a number")?;
    let stamp = header["timestamp"]
        .as_u64()
        .context("block without a timestamp")?
        / 1000;
    let age = unix_now()?.saturating_sub(stamp);
    ensure!(age <= STALE_AFTER, "head block {head} is {age} s old");
    let parameters = api.post("/wallet/getchainparameters", &json!({})).await?;
    let energy = parameter(&parameters, "getEnergyFee")?;
    let bandwidth = parameter(&parameters, "getTransactionFee")?;
    let (decimals, simulated) = decimals(&mut api).await?;
    ensure!(
        decimals == 6,
        "USDT reports {decimals} decimals, expected 6"
    );
    let mut extras = Extras::default();
    extras.add("simulate", Ok(format!("{simulated} energy")));
    extras.add("account", account(&mut api).await);
    extras.add("resources", resources(&mut api).await);
    extras.add("estimate", estimate(&mut api).await);
    extras.add(
        "history",
        api.expect(
            &format!("/v1/accounts/{OWNER}/transactions/trc20?limit=1"),
            "/success",
        )
        .await,
    );
    let mut outcome = Outcome::new(format!(
        "{} ms, head {age} s old, energy {energy} sun, bandwidth {bandwidth} sun per byte",
        latency.as_millis()
    ));
    outcome.extras(extras);
    Ok(outcome)
}

fn parameter(parameters: &Value, key: &str) -> Result<u64> {
    parameters["chainParameter"]
        .as_array()
        .context("no chain parameters")?
        .iter()
        .find(|entry| entry["key"] == key)
        .and_then(|entry| entry["value"].as_u64())
        .with_context(|| format!("no {key} parameter"))
}

fn decimals_call() -> Value {
    json!({
        "owner_address": OWNER,
        "contract_address": USDT,
        "function_selector": "decimals()",
        "parameter": "",
        "visible": true,
    })
}

async fn decimals(api: &mut Paced<'_>) -> Result<(u128, u64)> {
    let reply = api
        .post("/wallet/triggerconstantcontract", &decimals_call())
        .await?;
    ensure!(
        reply["result"]["result"] == true,
        "call failed: {}",
        reply["result"]
    );
    let word = reply["constant_result"][0]
        .as_str()
        .context("no constant result")?;
    let energy = reply["energy_used"].as_u64().context("no energy_used")?;
    Ok((u128::from_str_radix(word, 16)?, energy))
}

async fn account(api: &mut Paced<'_>) -> Result<String> {
    let account = api
        .post(
            "/wallet/getaccount",
            &json!({"address": USDT, "visible": true}),
        )
        .await?;
    ensure!(account.get("address").is_some(), "empty account");
    Ok("ok".to_owned())
}

async fn resources(api: &mut Paced<'_>) -> Result<String> {
    let resources = api
        .post(
            "/wallet/getaccountresource",
            &json!({"address": OWNER, "visible": true}),
        )
        .await?;
    ensure!(resources.is_object(), "not an object");
    Ok("ok".to_owned())
}

async fn estimate(api: &mut Paced<'_>) -> Result<String> {
    let reply = api.post("/wallet/estimateenergy", &decimals_call()).await?;
    ensure!(reply["result"]["result"] == true, "{}", reply["result"]);
    let energy = reply["energy_required"]
        .as_u64()
        .context("no energy_required")?;
    Ok(format!("{energy} energy"))
}
