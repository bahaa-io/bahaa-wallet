#![allow(clippy::unwrap_used, clippy::print_stdout)]

mod common;

use bahaa_bitcoin::BitcoinChain;
use bahaa_chain::{Chain, Transfer};
use bahaa_types::Amount;
use common::{FIRST, FRIEND, address, bitcoin_on, native, seed};
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
        let chain = BitcoinChain::new(bitcoin_on(&Url::parse(server).unwrap())).unwrap();
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
