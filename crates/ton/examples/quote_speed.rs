#![allow(clippy::print_stdout, clippy::unwrap_used)]

use std::time::Instant;

use bahaa_chain::{Chain, Transfer};
use bahaa_config::Registry;
use bahaa_types::{Address, Amount, AssetId, ChainFamily};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let registry = Registry::builtin().unwrap();
    let ton = registry.get(&"ton".parse().unwrap()).unwrap().clone();
    let usdt = ton.tokens[0].contract;
    let chain = bahaa_ton::TonChain::new(ton.clone()).unwrap();
    let parse = |text: &str| Address::parse(ChainFamily::Ton, text).unwrap();
    let from = parse("0:13fd9907ed89660d9b90f925bd54a6b029cafb368a172888f1ffc6fd690624e5");
    let to = parse("0:f8b8635d0786b77f9da660e65fd31d670a675dd4302facba692f46b571eb5360");
    let gram = Transfer {
        account: 0,
        from,
        to,
        asset: "ton".parse().unwrap(),
        amount: Amount::parse("0.01", 9).unwrap(),
        max: false,
        memo: None,
    };
    let tokens = Transfer {
        asset: AssetId::Token(ton.id.clone(), usdt),
        amount: Amount::parse("0.01", 6).unwrap(),
        ..gram.clone()
    };
    for round in 0..3 {
        for (name, transfer) in [("gram", &gram), ("usdt", &tokens)] {
            let started = Instant::now();
            let outcome = chain
                .prepare(transfer)
                .await
                .map(|(_, quote)| quote.expected().to_string());
            println!(
                "round {round} {name}: {:.2?} {outcome:?}",
                started.elapsed()
            );
        }
        if round == 1 {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        }
    }
}
