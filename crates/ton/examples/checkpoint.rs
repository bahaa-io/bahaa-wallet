#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::process::ExitCode;

use bahaa_chain::ChainError;
use bahaa_config::{ChainKind, Checkpoint, Network, Registry};
use bahaa_ton::TonChain;

const NETWORKS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../config/src/networks.json");
const ATTEMPTS: usize = 8;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let write = std::env::args().skip(1).any(|arg| arg == "--write");
    let registry = match Registry::builtin() {
        Ok(registry) => registry,
        Err(err) => {
            eprintln!("the built-in network list is broken: {err}");
            return ExitCode::FAILURE;
        }
    };
    let mut text = match std::fs::read_to_string(NETWORKS) {
        Ok(text) => text,
        Err(err) => {
            eprintln!("cannot read {NETWORKS}: {err}");
            return ExitCode::FAILURE;
        }
    };
    let mut failed = false;
    let mut moved = false;
    for network in registry.networks() {
        let ChainKind::Ton {
            checkpoint: old, ..
        } = network.kind
        else {
            continue;
        };
        match refreshed(network).await {
            Ok(fresh) if fresh.seqno > old.seqno => {
                println!("{}: key block {} -> {}", network.id, old.seqno, fresh.seqno);
                println!("{}", block(&fresh, 4));
                if replace(&mut text, &old, &fresh) {
                    moved = true;
                } else {
                    eprintln!(
                        "{}: the old checkpoint is not in {NETWORKS} exactly once",
                        network.id
                    );
                    failed = true;
                }
            }
            Ok(_) => println!(
                "{}: key block {} is still the newest",
                network.id, old.seqno
            ),
            Err(err) => {
                eprintln!("{}: {err}", network.id);
                failed = true;
            }
        }
    }
    if write && moved && !failed {
        if let Err(err) = std::fs::write(NETWORKS, &text) {
            eprintln!("cannot write {NETWORKS}: {err}");
            return ExitCode::FAILURE;
        }
        println!("updated {NETWORKS}");
    } else if moved && !write {
        println!("run again with --write to update {NETWORKS}");
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

async fn refreshed(network: &Network) -> Result<Checkpoint, ChainError> {
    let chain = TonChain::new(network.clone())?;
    let mut reached = chain.checkpoint();
    for _ in 0..ATTEMPTS {
        match chain.refresh_checkpoint().await {
            Ok(fresh) => return Ok(fresh),
            Err(err) => {
                let now = chain.checkpoint();
                if now == reached {
                    return Err(err);
                }
                reached = now;
            }
        }
    }
    Err(ChainError::Network(
        "the walk to the newest key block did not finish".to_owned(),
    ))
}

fn block(checkpoint: &Checkpoint, indent: usize) -> String {
    let pad = " ".repeat(indent);
    format!(
        "{pad}\"checkpoint\": {{\n{pad}  \"seqno\": {},\n{pad}  \"root_hash\": \"{}\",\n{pad}  \"file_hash\": \"{}\"\n{pad}}}",
        checkpoint.seqno,
        hex(&checkpoint.root_hash),
        hex(&checkpoint.file_hash)
    )
}

fn replace(text: &mut String, old: &Checkpoint, fresh: &Checkpoint) -> bool {
    let before = block(old, 4);
    if text.matches(&before).count() != 1 {
        return false;
    }
    *text = text.replacen(&before, &block(fresh, 4), 1);
    true
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
        out
    })
}
