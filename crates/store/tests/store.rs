#![allow(clippy::unwrap_used)]

use std::fmt::Write;
use std::path::{Path, PathBuf};

use bahaa_config::{Checkpoint, Coin, Fiat, Settings};
use bahaa_store::{
    Contact, CustomToken, Outgoing, Received, SavedPrices, SavedQuote, Scan, SentStatus, SentTx,
    Store, StoreError, TrustedBlock,
};
use bahaa_types::{Address, Amount, ChainFamily, Price};
use redb::{Database, TableDefinition};

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).unwrap();
        let name = random.iter().fold(String::new(), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        });
        let dir = std::env::temp_dir().join(format!("bahaa-store-test-{name}"));
        std::fs::create_dir(&dir).unwrap();
        Self(dir)
    }

    fn file(&self) -> PathBuf {
        self.0.join("wallet.redb")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn evm(text: &str) -> Address {
    Address::parse(ChainFamily::Evm, text).unwrap()
}

fn contact(name: &str) -> Contact {
    Contact {
        network: "bsc".parse().unwrap(),
        address: evm("0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed"),
        name: name.to_owned(),
    }
}

fn token() -> CustomToken {
    CustomToken {
        asset: "bsc/0x0E09FaBB73Bd3Ade0a17ECC321fD13a19e81cE82"
            .parse()
            .unwrap(),
        coin: Coin {
            symbol: "CAKE".to_owned(),
            name: "PancakeSwap".to_owned(),
            decimals: 18,
            price: Some("crypto/CAKE".parse().unwrap()),
        },
    }
}

#[test]
fn a_new_store_starts_empty_with_default_settings() {
    let scratch = Scratch::new();
    let store = Store::open(&scratch.file()).unwrap();
    assert_eq!(store.settings().unwrap(), Settings::default());
    assert_eq!(store.contacts().unwrap(), []);
    assert_eq!(store.tokens().unwrap(), []);
}

#[test]
fn everything_survives_a_restart() {
    let scratch = Scratch::new();
    let settings = Settings {
        fiat: Fiat::Irt,
        lock_after_minutes: 5,
        ..Settings::default()
    };
    {
        let store = Store::open(&scratch.file()).unwrap();
        store.save_settings(&settings).unwrap();
        store.save_contact(&contact("Mom")).unwrap();
        store.save_token(&token()).unwrap();
    }
    let store = Store::open(&scratch.file()).unwrap();
    assert_eq!(store.settings().unwrap(), settings);
    assert_eq!(store.contacts().unwrap(), [contact("Mom")]);
    assert_eq!(store.tokens().unwrap(), [token()]);
}

#[test]
fn contacts_are_keyed_by_network_and_address() {
    let scratch = Scratch::new();
    let store = Store::open(&scratch.file()).unwrap();
    store.save_contact(&contact("Mom")).unwrap();
    store.save_contact(&contact("Mother")).unwrap();
    let on_eth = Contact {
        network: "ethereum".parse().unwrap(),
        ..contact("Mom on Ethereum")
    };
    store.save_contact(&on_eth).unwrap();
    let names: Vec<String> = store
        .contacts()
        .unwrap()
        .into_iter()
        .map(|c| c.name)
        .collect();
    assert_eq!(names, ["Mother", "Mom on Ethereum"]);
    let first = contact("x");
    assert!(
        store
            .remove_contact(&first.network, &first.address)
            .unwrap()
    );
    assert!(
        !store
            .remove_contact(&first.network, &first.address)
            .unwrap()
    );
    assert_eq!(store.contacts().unwrap(), [on_eth]);
}

#[test]
fn contact_names_must_be_plain_text() {
    let scratch = Scratch::new();
    let store = Store::open(&scratch.file()).unwrap();
    for name in ["", " Mom", "Mo\u{202E}m", &"a".repeat(41)] {
        assert!(
            matches!(
                store.save_contact(&contact(name)),
                Err(StoreError::Invalid(_))
            ),
            "{name:?}"
        );
    }
    assert_eq!(store.contacts().unwrap(), []);
}

#[test]
fn custom_tokens_are_validated() {
    let scratch = Scratch::new();
    let store = Store::open(&scratch.file()).unwrap();
    let native = CustomToken {
        asset: "bsc".parse().unwrap(),
        ..token()
    };
    assert!(matches!(
        store.save_token(&native),
        Err(StoreError::Invalid(_))
    ));
    let mut bad = token();
    bad.coin.symbol = "CA KE".to_owned();
    assert!(matches!(
        store.save_token(&bad),
        Err(StoreError::Invalid(_))
    ));
    store.save_token(&token()).unwrap();
    assert!(store.remove_token(&token().asset).unwrap());
    assert_eq!(store.tokens().unwrap(), []);
}

fn sent(id: &str, status: SentStatus) -> SentTx {
    let raw = status.is_open().then(|| "02f8".to_owned());
    SentTx {
        id: id.to_owned(),
        network: "bsc".parse().unwrap(),
        from: evm("0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"),
        to: evm("0x70997970C51812dc3A010C7d01b50e0d17dc79C8"),
        asset: "bsc".parse().unwrap(),
        amount: Amount::parse("0.25", 18).unwrap(),
        sequence: Some(7),
        fee_max: Amount::parse("0.0001", 18).unwrap(),
        fee_paid: None,
        block: None,
        created_at: 1_760_000_000,
        expires_at: None,
        status,
        raw,
        transaction: None,
        memo: None,
        replaces: None,
    }
}

#[test]
fn records_from_before_expiry_times_still_load() {
    let mut older = serde_json::to_value(sent("0x01", SentStatus::Pending)).unwrap();
    older.as_object_mut().unwrap().remove("expires_at");
    let loaded: SentTx = serde_json::from_value(older).unwrap();
    assert_eq!(loaded, sent("0x01", SentStatus::Pending));
    let mut tron = sent("0x02", SentStatus::Pending);
    tron.expires_at = Some(1_760_000_300);
    let round = serde_json::to_value(&tron).unwrap();
    assert_eq!(serde_json::from_value::<SentTx>(round).unwrap(), tron);
}

#[test]
fn sent_transactions_are_kept_and_updated_in_place() {
    let scratch = Scratch::new();
    let first = sent(&format!("0x{}", "ab".repeat(32)), SentStatus::Sending);
    let second = sent(&format!("0x{}", "cd".repeat(32)), SentStatus::Pending);
    {
        let store = Store::open(&scratch.file()).unwrap();
        store.save_sent(&first).unwrap();
        store.save_sent(&second).unwrap();
        let mut done = first.clone();
        done.status = SentStatus::Confirmed;
        done.raw = None;
        done.block = Some(42);
        done.fee_paid = Some(Amount::parse("0.00002", 18).unwrap());
        store.save_sent(&done).unwrap();
    }
    let store = Store::open(&scratch.file()).unwrap();
    let all = store.sent().unwrap();
    assert_eq!(all.len(), 2);
    let done = all.iter().find(|tx| tx.id == first.id).unwrap();
    assert_eq!(done.status, SentStatus::Confirmed);
    assert_eq!(done.fee_paid.unwrap().to_string(), "0.00002");
    assert_eq!(all.iter().find(|tx| tx.id == second.id).unwrap(), &second);
}

#[test]
fn sent_transactions_are_validated() {
    let scratch = Scratch::new();
    let store = Store::open(&scratch.file()).unwrap();
    let id = format!("0x{}", "ab".repeat(32));
    let mut keeps_bytes = sent(&id, SentStatus::Confirmed);
    keeps_bytes.raw = Some("02f8".to_owned());
    let mut lost_bytes = sent(&id, SentStatus::Pending);
    lost_bytes.raw = None;
    let bad = [
        sent("", SentStatus::Pending),
        sent("0xab\u{202E}", SentStatus::Pending),
        sent(&id, SentStatus::Failed("bad\nreason".to_owned())),
        sent(&id, SentStatus::Failed("x".repeat(201))),
        keeps_bytes,
        lost_bytes,
    ];
    for tx in bad {
        assert!(
            matches!(store.save_sent(&tx), Err(StoreError::Invalid(_))),
            "{tx:?}"
        );
    }
    store
        .save_sent(&sent(&id, SentStatus::Failed("nonce too low".to_owned())))
        .unwrap();
    store.save_sent(&sent(&id, SentStatus::Dropped)).unwrap();
    assert_eq!(store.sent().unwrap()[0].status, SentStatus::Dropped);
}

fn scan(next: u64) -> Scan {
    Scan {
        network: "bsc".parse().unwrap(),
        address: evm("0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"),
        next,
        complete_from: 100,
        complete_since: 1_760_000_000,
    }
}

fn outgoing(index: u64) -> Outgoing {
    Outgoing {
        id: format!("0x{}", "aa".repeat(32)),
        index,
        network: "bsc".parse().unwrap(),
        from: evm("0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"),
        to: evm("0x70997970C51812dc3A010C7d01b50e0d17dc79C8"),
        asset: "bsc/0x55d398326f99059fF775485246999027B3197955"
            .parse()
            .unwrap(),
        amount: Amount::parse("3", 18).unwrap(),
        block: 160,
        time: 1_760_000_400,
        memo: None,
    }
}

fn received(index: u64) -> Received {
    Received {
        id: format!("0x{}", "ef".repeat(32)),
        index,
        network: "bsc".parse().unwrap(),
        from: evm("0x70997970C51812dc3A010C7d01b50e0d17dc79C8"),
        to: evm("0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"),
        asset: "bsc/0x55d398326f99059fF775485246999027B3197955"
            .parse()
            .unwrap(),
        amount: Amount::parse("12.5", 18).unwrap(),
        block: 150,
        time: 1_760_000_300,
        memo: None,
    }
}

#[test]
fn a_scan_and_what_it_found_are_saved_together() {
    let scratch = Scratch::new();
    {
        let store = Store::open(&scratch.file()).unwrap();
        store
            .save_scan(&scan(200), &[received(1), received(2)], &[outgoing(4)])
            .unwrap();
        store
            .save_scan(&scan(300), &[received(2)], &[outgoing(4), outgoing(5)])
            .unwrap();
    }
    let store = Store::open(&scratch.file()).unwrap();
    assert_eq!(store.scans().unwrap(), [scan(300)]);
    assert_eq!(store.received().unwrap(), [received(1), received(2)]);
    assert_eq!(store.outgoing().unwrap(), [outgoing(4), outgoing(5)]);
}

#[test]
fn a_bad_scan_saves_nothing() {
    let scratch = Scratch::new();
    let store = Store::open(&scratch.file()).unwrap();
    let mut foreign = received(1);
    foreign.to = evm("0x70997970C51812dc3A010C7d01b50e0d17dc79C8");
    let mut hidden = received(2);
    hidden.id = "0xef\u{202E}".to_owned();
    let mut backwards = scan(200);
    backwards.complete_from = 201;
    let mut stranger = outgoing(1);
    stranger.from = evm("0x70997970C51812dc3A010C7d01b50e0d17dc79C8");
    let mut odd = outgoing(2);
    odd.id = "0xef\u{202E}".to_owned();
    for (scan, found, sent) in [
        (scan(200), vec![received(3), foreign], Vec::new()),
        (scan(200), vec![hidden], Vec::new()),
        (backwards, Vec::new(), Vec::new()),
        (scan(200), vec![received(3)], vec![stranger]),
        (scan(200), Vec::new(), vec![odd]),
    ] {
        assert!(matches!(
            store.save_scan(&scan, &found, &sent),
            Err(StoreError::Invalid(_))
        ));
    }
    assert_eq!(store.scans().unwrap(), Vec::<Scan>::new());
    assert_eq!(store.received().unwrap(), Vec::<Received>::new());
    assert_eq!(store.outgoing().unwrap(), Vec::<Outgoing>::new());
}

fn trusted(network: &str, seqno: u32) -> TrustedBlock {
    TrustedBlock {
        network: network.parse().unwrap(),
        checkpoint: Checkpoint {
            seqno,
            root_hash: [1; 32],
            file_hash: [2; 32],
        },
    }
}

#[test]
fn the_latest_trusted_block_of_each_network_survives_a_restart() {
    let scratch = Scratch::new();
    {
        let store = Store::open(&scratch.file()).unwrap();
        assert_eq!(store.trusted().unwrap(), Vec::new());
        store.save_trusted(&trusted("ton", 99_746_941)).unwrap();
        store.save_trusted(&trusted("ton", 99_766_000)).unwrap();
        store
            .save_trusted(&trusted("ton-testnet", 92_078_093))
            .unwrap();
        assert_eq!(
            store.save_trusted(&trusted("ton", 0)),
            Err(StoreError::Invalid(
                "a trusted block cannot be the zero state"
            ))
        );
    }
    let store = Store::open(&scratch.file()).unwrap();
    assert_eq!(
        store.trusted().unwrap(),
        [
            trusted("ton", 99_766_000),
            trusted("ton-testnet", 92_078_093)
        ]
    );
}

#[test]
fn refuses_data_from_a_newer_version() {
    let scratch = Scratch::new();
    drop(Store::open(&scratch.file()).unwrap());
    {
        let db = Database::create(scratch.file()).unwrap();
        let txn = db.begin_write().unwrap();
        txn.open_table(TableDefinition::<&str, u64>::new("meta"))
            .unwrap()
            .insert("schema", 99)
            .unwrap();
        txn.commit().unwrap();
    }
    assert!(matches!(
        Store::open(&scratch.file()),
        Err(StoreError::NewerSchema(99))
    ));
}

#[test]
fn damaged_entries_are_reported_not_trusted() {
    let scratch = Scratch::new();
    drop(Store::open(&scratch.file()).unwrap());
    {
        let db = Database::create(scratch.file()).unwrap();
        let txn = db.begin_write().unwrap();
        txn.open_table(TableDefinition::<&str, &[u8]>::new("contacts"))
            .unwrap()
            .insert("bsc/evm:0x0", b"{not json".as_slice())
            .unwrap();
        txn.commit().unwrap();
    }
    let store = Store::open(&scratch.file()).unwrap();
    assert!(matches!(store.contacts(), Err(StoreError::Corrupt(_))));
}

#[test]
fn only_one_wallet_can_use_the_data_at_a_time() {
    let scratch = Scratch::new();
    let _first = Store::open(&scratch.file()).unwrap();
    assert!(matches!(
        Store::open(&scratch.file()),
        Err(StoreError::AlreadyOpen)
    ));
}

#[cfg(unix)]
#[test]
fn the_data_file_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = Scratch::new();
    drop(Store::open(&scratch.file()).unwrap());
    let mode = std::fs::metadata(Path::new(&scratch.file()))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[test]
fn the_last_prices_of_each_base_survive_a_restart() {
    let scratch = Scratch::new();
    let usdt = SavedPrices {
        base: "USDT".to_owned(),
        quotes: vec![SavedQuote {
            id: "crypto/TRX".parse().unwrap(),
            price: Price::parse("0.331").unwrap(),
            updated_at: 1_792_385_000,
        }],
    };
    {
        let store = Store::open(&scratch.file()).unwrap();
        assert_eq!(store.prices("USDT").unwrap(), None);
        store.save_prices(&usdt).unwrap();
        let mut bad = usdt.clone();
        bad.base = "usdt".to_owned();
        assert!(matches!(
            store.save_prices(&bad),
            Err(StoreError::Invalid(_))
        ));
        bad.base = "IRT".to_owned();
        bad.quotes = vec![usdt.quotes[0].clone(); 513];
        assert!(matches!(
            store.save_prices(&bad),
            Err(StoreError::Invalid(_))
        ));
    }
    let store = Store::open(&scratch.file()).unwrap();
    assert_eq!(store.prices("USDT").unwrap(), Some(usdt));
    assert_eq!(store.prices("IRT").unwrap(), None);
}
