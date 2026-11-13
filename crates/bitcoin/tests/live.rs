#![allow(clippy::unwrap_used, clippy::print_stdout)]

mod common;

use bahaa_bitcoin::BitcoinChain;
use bahaa_chain::{Chain, Transfer};
use bahaa_types::Amount;
use common::{FIRST, FRIEND, address, mainnet_on, native, seed};
use url::Url;

const SERVERS: [&str; 2] = [
    "ssl://electrum.blockstream.info:50002",
    "ssl://e2.keff.org:50002",
];

#[tokio::test]
#[ignore = "needs the public Electrum servers"]
async fn every_server_reads_the_same_account_and_quotes_a_fee() {
    let mut seen = Vec::new();
    for server in SERVERS {
        let chain = BitcoinChain::new(mainnet_on(&Url::parse(server).unwrap())).unwrap();
        assert_eq!(chain.derive(&seed(), 0).unwrap(), address(FIRST));
        let held = chain.balances(&address(FIRST), &[native()]).await.unwrap();
        println!("{server}: {}", held[0].amount);
        seen.push(held[0].amount);
        let transfer = Transfer {
            account: 0,
            from: address(FIRST),
            to: address(FRIEND),
            asset: native(),
            amount: Amount::parse("0.00001", 8).unwrap(),
            max: false,
            memo: None,
        };
        match chain.prepare(&transfer).await {
            Ok((_, fee)) => println!("{server}: fee {}", fee.expected()),
            Err(err) => println!("{server}: {err}"),
        }
    }
    assert_eq!(seen[0], seen[1]);
}

#[tokio::test]
#[ignore = "needs the public Electrum servers"]
async fn every_server_reads_the_same_history() {
    let mut seen = Vec::new();
    for server in SERVERS {
        let chain = BitcoinChain::new(mainnet_on(&Url::parse(server).unwrap())).unwrap();
        chain.derive(&seed(), 0).unwrap();
        let scanned = chain
            .scan(&address(FIRST), &[native()], None)
            .await
            .unwrap()
            .unwrap();
        println!(
            "{server}: {} received, {} sent, cursor {:?}",
            scanned.found.len(),
            scanned.sent.len(),
            scanned.cursor
        );
        for moved in scanned.found.iter().chain(&scanned.sent).take(4) {
            println!(
                "  {} {} {} -> {} at {}",
                moved.id, moved.amount, moved.from, moved.to, moved.block
            );
        }
        seen.push((scanned.found, scanned.sent));
    }
    assert_eq!(seen[0], seen[1]);
}

#[tokio::test]
#[ignore = "needs the public Esplora and Electrum servers"]
async fn esplora_reads_what_electrum_reads() {
    let electrum = BitcoinChain::new(mainnet_on(&Url::parse(SERVERS[0]).unwrap())).unwrap();
    electrum.derive(&seed(), 0).unwrap();
    for base in [
        "https://blockstream.info/api/",
        "https://mempool.emzy.de/api/",
    ] {
        let mut network =
            common::bitcoin_with(&[(bahaa_config::Protocol::Esplora, Url::parse(base).unwrap())]);
        network.endpoints.remove(0);
        network.endpoints[0].min_gap_ms = 200;
        let esplora = BitcoinChain::new(network).unwrap();
        esplora.derive(&seed(), 0).unwrap();
        let held = esplora
            .balances(&address(FIRST), &[native()])
            .await
            .unwrap();
        let scanned = esplora
            .scan(&address(FIRST), &[native()], None)
            .await
            .unwrap()
            .unwrap();
        let reference = electrum
            .scan(&address(FIRST), &[native()], None)
            .await
            .unwrap()
            .unwrap();
        println!(
            "{base}: {} held, {} received, {} sent",
            held[0].amount,
            scanned.found.len(),
            scanned.sent.len()
        );
        assert_eq!(scanned.found, reference.found);
        assert_eq!(scanned.sent, reference.sent);
    }
}

#[tokio::test]
#[ignore = "needs the public Electrum servers"]
async fn old_address_types_read_the_same_on_every_server_and_sign_locally() {
    for kind in [
        bahaa_types::BitcoinType::Legacy,
        bahaa_types::BitcoinType::NestedSegwit,
    ] {
        let mut seen = Vec::new();
        for server in SERVERS {
            let chain = BitcoinChain::new(mainnet_on(&Url::parse(server).unwrap())).unwrap();
            let from = chain.derive_as(&seed(), 0, kind).unwrap();
            let held = chain.balances(&from, &[native()]).await.unwrap();
            let scanned = chain.scan(&from, &[native()], None).await.unwrap().unwrap();
            println!(
                "{kind:?} {server}: {from} holds {}, {} received, {} sent",
                held[0].amount,
                scanned.found.len(),
                scanned.sent.len()
            );
            let transfer = Transfer {
                account: 0,
                from,
                to: from,
                asset: native(),
                amount: Amount::parse("0.00001", 8).unwrap(),
                max: false,
                memo: None,
            };
            match chain.prepare(&transfer).await {
                Ok((plan, fee)) => {
                    let signed = chain.sign(&plan, &seed()).unwrap();
                    bahaa_chain::check_signed(
                        chain.planned(&plan),
                        &chain.decode(&signed).unwrap(),
                    )
                    .unwrap();
                    println!("  fee {}, signed and read back, not sent", fee.expected());
                }
                Err(err) => println!("  {err}"),
            }
            seen.push((held[0].amount, scanned.found, scanned.sent));
        }
        assert_eq!(seen[0], seen[1]);
    }
}

#[tokio::test]
#[ignore = "talks to real DNS-over-HTTPS servers"]
async fn live_bip353_names_resolve_through_checked_dnssec() {
    let chain = bahaa_bitcoin::BitcoinChain::new(
        bahaa_config::Registry::builtin()
            .unwrap()
            .get(&"bitcoin".parse().unwrap())
            .unwrap()
            .clone(),
    )
    .unwrap();
    assert_eq!(
        chain
            .bip353_address("\u{20bf}matt@mattcorallo.com")
            .await
            .unwrap()
            .map(|address| address.to_string())
            .as_deref(),
        Some("bc1qztwy6xen3zdtt7z0vrgapmjtfz8acjkfp5fp7l")
    );
    assert_eq!(
        chain
            .bip353_address("nobody-here-zz@mattcorallo.com")
            .await
            .unwrap(),
        None
    );
    assert!(chain.bip353_address("test@github.com").await.is_err());
}
