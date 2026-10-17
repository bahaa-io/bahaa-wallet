use std::time::Duration;

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use crate::clock::unix_now;
use crate::net::{Net, Paced};
use crate::report::{self, Extras, Outcome, Report, timed};

const ACCOUNT: &str = "EQCD39VS5jcptHL8vMjEXrzGaRcCVYto7HUn4bpAOg8xqB2N";
const USDT_MASTER: &str = "EQCxE6mUtQJKFnGfaROTKOt1lZbDiiX1kCixRv7Nw2Id_sDs";
const GAP: Duration = Duration::from_millis(1100);
const STALE_AFTER: u64 = 60;

pub async fn run(net: &Net, report: &mut Report) {
    report::section("ton http");
    let (center, api) = tokio::join!(timed(toncenter(net)), timed(tonapi(net)));
    report.record("toncenter", center.0, center.1);
    report.record("tonapi", api.0, api.1);
}

async fn toncenter(net: &Net) -> Result<Outcome> {
    let mut api = Paced::new(net, "https://toncenter.com/api", GAP);
    let (latency, info) = timed(api.get("/v3/masterchainInfo")).await;
    let (seqno, age) = head(&info?, "/last/seqno", "/last/gen_utime")?;
    let mut extras = Extras::default();
    extras.add(
        "balance",
        api.expect(
            &format!("/v2/getAddressBalance?address={ACCOUNT}"),
            "/result",
        )
        .await,
    );
    extras.add("get-method", center_method(&mut api).await);
    extras.add(
        "history",
        api.expect(
            &format!("/v3/transactions?account={ACCOUNT}&limit=1"),
            "/transactions/0",
        )
        .await,
    );
    extras.add(
        "jettons",
        api.expect(
            &format!("/v3/jetton/wallets?owner_address={ACCOUNT}&limit=1"),
            "/jetton_wallets",
        )
        .await,
    );
    Ok(summary(latency, seqno, age, extras))
}

async fn center_method(api: &mut Paced<'_>) -> Result<String> {
    let reply = api
        .post(
            "/v2/runGetMethod",
            &json!({"address": USDT_MASTER, "method": "get_jetton_data", "stack": []}),
        )
        .await?;
    let code = reply
        .pointer("/result/exit_code")
        .and_then(Value::as_i64)
        .context("no exit code")?;
    ensure!(code == 0, "exit code {code}");
    Ok("yes".to_owned())
}

async fn tonapi(net: &Net) -> Result<Outcome> {
    let mut api = Paced::new(net, "https://tonapi.io/v2", GAP);
    let (latency, info) = timed(api.get("/blockchain/masterchain-head")).await;
    let (seqno, age) = head(&info?, "/seqno", "/gen_utime")?;
    let mut extras = Extras::default();
    extras.add(
        "balance",
        api.expect(&format!("/accounts/{ACCOUNT}"), "/balance")
            .await,
    );
    extras.add(
        "get-method",
        api.expect(
            &format!("/blockchain/accounts/{USDT_MASTER}/methods/get_jetton_data"),
            "/success",
        )
        .await,
    );
    extras.add(
        "history",
        api.expect(&format!("/accounts/{ACCOUNT}/events?limit=1"), "/events/0")
            .await,
    );
    extras.add(
        "jettons",
        api.expect(&format!("/accounts/{ACCOUNT}/jettons"), "/balances")
            .await,
    );
    Ok(summary(latency, seqno, age, extras))
}

fn head(info: &Value, seqno: &str, utime: &str) -> Result<(u64, u64)> {
    let seqno = info
        .pointer(seqno)
        .and_then(Value::as_u64)
        .context("no masterchain seqno")?;
    let stamp = info
        .pointer(utime)
        .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()))
        .context("no block time")?;
    let age = unix_now()?.saturating_sub(stamp);
    ensure!(
        age <= STALE_AFTER,
        "masterchain block {seqno} is {age} s old"
    );
    Ok((seqno, age))
}

fn summary(latency: Duration, seqno: u64, age: u64, extras: Extras) -> Outcome {
    let mut outcome = Outcome::new(format!(
        "{} ms, masterchain {seqno}, {age} s old",
        latency.as_millis()
    ));
    outcome.extras(extras);
    outcome
}
