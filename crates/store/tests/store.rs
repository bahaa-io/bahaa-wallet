#![allow(clippy::unwrap_used)]

use std::fmt::Write;
use std::path::{Path, PathBuf};

use bahaa_config::{Coin, Fiat, Settings};
use bahaa_store::{Contact, CustomToken, Store, StoreError};
use bahaa_types::{Address, ChainFamily};
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
