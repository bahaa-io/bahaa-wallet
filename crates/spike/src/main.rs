#![allow(clippy::print_stdout, clippy::print_stderr)]

mod bsc;
mod keys;

use std::process::ExitCode;

use anyhow::Result;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let command = std::env::args().nth(1).unwrap_or_default();
    let result = match command.as_str() {
        "new" => new(),
        "address" => address(),
        "bsc" => bsc::transfer().await,
        _ => {
            eprintln!("usage: bahaa-spike new | address | bsc");
            return ExitCode::FAILURE;
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("bahaa-spike: {}", clean(&format!("{err:#}")));
            ExitCode::FAILURE
        }
    }
}

fn new() -> Result<()> {
    let path = keys::create()?;
    println!("testnet mnemonic written to {}", path.display());
    println!("TESTNET ONLY. Never send real funds to these addresses.");
    address()
}

fn address() -> Result<()> {
    let phrase = keys::load()?;
    println!("bsc testnet  {}", keys::evm(&phrase)?.address());
    Ok(())
}

fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if unsafe_to_print(c) { ' ' } else { c })
        .collect()
}

fn unsafe_to_print(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}
