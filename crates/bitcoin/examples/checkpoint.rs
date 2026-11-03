#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::process::ExitCode;

use bahaa_bitcoin::BitcoinChain;
use bahaa_config::{BlockCheckpoint, ChainKind, Registry};
use bahaa_types::hex;

const NETWORKS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../config/src/networks.json");

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
        let ChainKind::Bitcoin {
            checkpoint: old, ..
        } = network.kind
        else {
            continue;
        };
        let fresh = match BitcoinChain::new(network.clone()) {
            Ok(chain) => chain.next_checkpoint().await,
            Err(err) => Err(err),
        };
        match fresh {
            Ok(fresh) if fresh.height > old.height => {
                println!("{}: block {} -> {}", network.id, old.height, fresh.height);
                println!("{}", block(&fresh));
                let before = block(&old);
                if text.matches(&before).count() == 1 {
                    text = text.replacen(&before, &block(&fresh), 1);
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
                "{}: block {} is still the newest period start",
                network.id, old.height
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

fn block(checkpoint: &BlockCheckpoint) -> String {
    format!(
        "    \"checkpoint\": {{\n      \"height\": {},\n      \"hash\": \"{}\"\n    }}",
        checkpoint.height,
        hex::encode(&checkpoint.hash)
    )
}
