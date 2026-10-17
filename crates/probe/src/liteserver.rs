use std::net::Ipv4Addr;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail, ensure};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE};
use futures_util::future::join_all;
use serde::Deserialize;
use tokio::time::timeout;
use ton_liteapi::client::LiteClient;
use ton_liteapi::tl::common::{AccountId, Int256};
use ton_liteapi::types::LiteError;

use crate::clock::unix_now;
use crate::net::Net;
use crate::report::{self, Extras, Outcome, Report, timed};

const CONFIG: &str = "https://ton.org/global-config.json";
const ACCOUNT: &str = "EQCD39VS5jcptHL8vMjEXrzGaRcCVYto7HUn4bpAOg8xqB2N";
const DEADLINE: Duration = Duration::from_secs(10);
const MAX_BEHIND: u32 = 12;

#[derive(Deserialize)]
struct Config {
    liteservers: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    ip: i64,
    port: u16,
    id: Key,
}

#[derive(Deserialize)]
struct Key {
    key: String,
}

struct Server {
    address: Ipv4Addr,
    port: u16,
    key: Vec<u8>,
}

impl TryFrom<Entry> for Server {
    type Error = anyhow::Error;

    fn try_from(entry: Entry) -> Result<Self> {
        let ip = i32::try_from(entry.ip)?;
        let key = STANDARD.decode(&entry.id.key)?;
        ensure!(key.len() == 32, "server key has {} bytes", key.len());
        Ok(Self {
            address: Ipv4Addr::from(u32::from_be_bytes(ip.to_be_bytes())),
            port: entry.port,
            key,
        })
    }
}

struct Sample {
    connect: Duration,
    rtt: Duration,
    seqno: u32,
    drift: i64,
    version: u32,
    state: Result<String>,
}

pub async fn run(net: &Net, report: &mut Report) {
    report::section("ton liteservers");
    let (elapsed, setup) = timed(setup(net)).await;
    let (servers, account) = match setup {
        Ok(setup) => setup,
        Err(err) => return report.record("config", elapsed, Err(err)),
    };
    let samples = join_all(servers.iter().map(|server| timed(sample(server, &account)))).await;
    let best = samples
        .iter()
        .filter_map(|(_, sample)| sample.as_ref().ok())
        .map(|sample| sample.seqno)
        .max()
        .unwrap_or(0);
    for (server, (elapsed, sample)) in servers.iter().zip(samples) {
        let result = sample.and_then(|sample| judge(sample, server.port, best));
        report.record(&server.address.to_string(), elapsed, result);
    }
}

async fn setup(net: &Net) -> Result<(Vec<Server>, AccountId)> {
    let config: Config = net
        .http
        .get(CONFIG)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let servers = config
        .liteservers
        .into_iter()
        .map(Server::try_from)
        .collect::<Result<Vec<_>>>()?;
    Ok((servers, account_id(ACCOUNT)?))
}

async fn sample(server: &Server, account: &AccountId) -> Result<Sample> {
    let (connect, client) = timed(within(LiteClient::connect(
        (server.address, server.port),
        &server.key,
    )))
    .await;
    let mut client = client?;
    let (rtt, info) = timed(within(client.get_masterchain_info())).await;
    let info = info?;
    let version = within(client.get_version()).await?;
    let drift = i64::from(version.now) - i64::try_from(unix_now()?)?;
    let state = within(client.get_account_state(info.last.clone(), account.clone()))
        .await
        .and_then(|state| {
            ensure!(!state.state.is_empty(), "empty account state");
            Ok(format!("{} bytes", state.state.len()))
        });
    Ok(Sample {
        connect,
        rtt,
        seqno: info.last.seqno,
        drift,
        version: version.version,
        state,
    })
}

fn judge(sample: Sample, port: u16, best: u32) -> Result<Outcome> {
    let behind = best.saturating_sub(sample.seqno);
    if behind > MAX_BEHIND {
        bail!("masterchain {} is {behind} blocks behind", sample.seqno);
    }
    let mut outcome = Outcome::new(format!(
        "{} ms, masterchain {}, {behind} behind, clock {:+} s",
        sample.rtt.as_millis(),
        sample.seqno,
        sample.drift
    ));
    let mut extras = Extras::default();
    extras.add("port", Ok(port.to_string()));
    extras.add("connect", Ok(format!("{} ms", sample.connect.as_millis())));
    extras.add("version", Ok(format!("{:#x}", sample.version)));
    extras.add("account", sample.state);
    outcome.extras(extras);
    Ok(outcome)
}

async fn within<T>(work: impl Future<Output = Result<T, LiteError>>) -> Result<T> {
    timeout(DEADLINE, work)
        .await
        .context("timed out")?
        .map_err(describe)
}

fn describe(err: LiteError) -> anyhow::Error {
    match err {
        LiteError::ServerError(error) => anyhow!("liteserver error {}", error.code),
        other => anyhow!("{other:?}"),
    }
}

fn account_id(friendly: &str) -> Result<AccountId> {
    let bytes = URL_SAFE.decode(friendly)?;
    ensure!(bytes.len() == 36, "address has {} bytes", bytes.len());
    let (body, checksum) = bytes.split_at(34);
    ensure!(
        checksum == crc16(body).to_be_bytes(),
        "address checksum mismatch"
    );
    let workchain = i32::from(i8::from_be_bytes([body[1]]));
    let mut hash = [0; 32];
    hash.copy_from_slice(&body[2..]);
    Ok(AccountId {
        workchain,
        id: Int256(hash),
    })
}

fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for byte in data {
        crc ^= u16::from(*byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 == 0 {
                crc << 1
            } else {
                (crc << 1) ^ 0x1021
            };
        }
    }
    crc
}
