#![allow(clippy::unwrap_used)]

use std::fmt::Write;
use std::path::{Path, PathBuf};

use bahaa_config::{Checkpoint, Coin, Fiat, Settings};
use bahaa_store::{
    Contact, CustomToken, Fetched, Listing, Mark, Marked, Outgoing, Received, SavedPrices,
    SavedQuote, Scan, SentKind, SentStatus, SentTx, Store, StoreError, StoreKey, SwapRecord,
    SwapStatus, TrustedBlock,
};
use bahaa_types::{
    Address, Amount, AssetId, ChainFamily, Collection, Details, MAX_NFTS, Nft, NftKind, Price,
    Trust,
};
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use zeroize::Zeroizing;

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

fn key(byte: u8) -> StoreKey {
    StoreKey::new(
        Zeroizing::new([byte; 32]),
        Zeroizing::new([byte ^ 0xff; 32]),
    )
}

fn unlocked(scratch: &Scratch) -> Store {
    let store = Store::open(&scratch.file()).unwrap();
    store.unlock(key(7)).unwrap();
    store
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
            color: None,
        },
    }
}

#[test]
fn a_new_store_starts_empty_with_default_settings() {
    let scratch = Scratch::new();
    let store = unlocked(&scratch);
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
        let store = unlocked(&scratch);
        store.save_settings(&settings).unwrap();
        store.save_contact(&contact("Mom")).unwrap();
        store.save_token(&token()).unwrap();
    }
    let store = unlocked(&scratch);
    assert_eq!(store.settings().unwrap(), settings);
    assert_eq!(store.contacts().unwrap(), [contact("Mom")]);
    assert_eq!(store.tokens().unwrap(), [token()]);
}

#[test]
fn contacts_are_keyed_by_network_and_address() {
    let scratch = Scratch::new();
    let store = unlocked(&scratch);
    store.save_contact(&contact("Mom")).unwrap();
    store.save_contact(&contact("Mother")).unwrap();
    let on_eth = Contact {
        network: "ethereum".parse().unwrap(),
        ..contact("Mom on Ethereum")
    };
    store.save_contact(&on_eth).unwrap();
    let mut names: Vec<String> = store
        .contacts()
        .unwrap()
        .into_iter()
        .map(|c| c.name)
        .collect();
    names.sort();
    assert_eq!(names, ["Mom on Ethereum", "Mother"]);
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
    let store = unlocked(&scratch);
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
    let store = unlocked(&scratch);
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
        kind: SentKind::Transfer,
        private: false,
    }
}

fn swap(status: SwapStatus) -> SwapRecord {
    SwapRecord {
        id: format!("0x{}", "ef".repeat(32)),
        network: "bsc".parse().unwrap(),
        provider: "relay".to_owned(),
        reference: format!("0x{}", "12".repeat(32)),
        from: evm("0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"),
        to: evm("0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"),
        deposit: evm("0x70997970C51812dc3A010C7d01b50e0d17dc79C8"),
        sell: "bsc".parse().unwrap(),
        amount: Amount::parse("0.25", 18).unwrap(),
        buy: "base".parse().unwrap(),
        expected: Amount::parse("0.07", 18).unwrap(),
        minimum: Amount::parse("0.069", 18).unwrap(),
        created_at: 1_760_000_000,
        updated_at: 1_760_000_000,
        status,
        order: None,
        approval: None,
    }
}

#[test]
fn swaps_are_sealed_kept_and_updated_in_place() {
    let scratch = Scratch::new();
    {
        let store = unlocked(&scratch);
        store.save_swap(&swap(SwapStatus::Waiting)).unwrap();
        store.save_swap(&swap(SwapStatus::Done)).unwrap();
        store.lock();
        assert_eq!(store.swaps(), Err(StoreError::Locked));
    }
    let store = unlocked(&scratch);
    assert_eq!(store.swaps().unwrap(), [swap(SwapStatus::Done)]);
    assert!(!SwapStatus::Refunded.is_open());
    assert!(SwapStatus::Swapping.is_open());
    let mut backwards = swap(SwapStatus::Waiting);
    backwards.minimum = Amount::parse("0.08", 18).unwrap();
    let mut same = swap(SwapStatus::Waiting);
    same.buy = same.sell.clone();
    let mut hidden = swap(SwapStatus::Waiting);
    hidden.provider = "relay\u{202e}".to_owned();
    for bad in [
        backwards,
        same,
        hidden,
        swap(SwapStatus::Failed("x".repeat(201))),
    ] {
        assert!(matches!(store.save_swap(&bad), Err(StoreError::Invalid(_))));
    }
}

const OWNER: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";
const APES: &str = "0xBC4CA0EdA7647A8aB7C2061c2E118A18a936f13D";

fn ape(token: u64) -> Nft {
    Nft {
        asset: format!("ethereum/nft/{APES}/{token}").parse().unwrap(),
        kind: NftKind::Erc721,
        amount: Amount::parse("1", 0).unwrap(),
        collection: Some(Collection {
            address: Some(evm(APES)),
            name: Some("BoredApeYachtClub".to_owned()),
        }),
        name: None,
        link: Some(format!(
            "ipfs://QmeSjSinHpPnmXmspMjwiXyN6zS4E9zccariGR3jxcaWtq/{token}"
        )),
        trust: Trust::Trusted,
        lock: None,
    }
}

fn listing(items: Vec<Nft>) -> Listing {
    Listing {
        network: "ethereum".parse().unwrap(),
        address: evm(OWNER),
        items,
        capped: false,
        at: 1_700_000_000,
    }
}

#[test]
fn nft_listings_are_sealed_and_replaced_per_account() {
    let scratch = Scratch::new();
    {
        let store = unlocked(&scratch);
        store.save_listing(&listing(vec![ape(1), ape(2)])).unwrap();
        store.save_listing(&listing(vec![ape(2)])).unwrap();
        let mut other = listing(vec![ape(3)]);
        other.network = "optimism".parse().unwrap();
        assert!(matches!(
            store.save_listing(&other),
            Err(StoreError::Invalid(_))
        ));
        store.lock();
        assert_eq!(store.listings(), Err(StoreError::Locked));
    }
    let store = unlocked(&scratch);
    assert_eq!(store.listings().unwrap(), [listing(vec![ape(2)])]);
    store.save_listing(&listing(Vec::new())).unwrap();
    assert_eq!(store.listings().unwrap(), [listing(Vec::new())]);
}

#[test]
fn nft_listings_are_validated() {
    let scratch = Scratch::new();
    let store = unlocked(&scratch);
    let mut coin = ape(1);
    coin.asset = AssetId::Native("ethereum".parse().unwrap());
    let mut fraction = ape(1);
    fraction.amount = Amount::parse("1", 18).unwrap();
    let mut none = ape(1);
    none.amount = Amount::parse("0", 0).unwrap();
    let mut hidden = ape(1);
    hidden.name = Some("Ape\u{202e}".to_owned());
    let mut collection = ape(1);
    collection.collection = Some(Collection {
        address: None,
        name: Some(String::new()),
    });
    let mut link = ape(1);
    link.link = Some("x".repeat(2_049));
    for bad in [
        vec![coin],
        vec![fraction],
        vec![none],
        vec![hidden],
        vec![collection],
        vec![link],
        vec![ape(1), ape(1)],
        (0..=u64::try_from(MAX_NFTS).unwrap()).map(ape).collect(),
    ] {
        assert!(matches!(
            store.save_listing(&listing(bad)),
            Err(StoreError::Invalid(_))
        ));
    }
    assert_eq!(store.listings().unwrap(), Vec::<Listing>::new());
    let full: Vec<Nft> = (0..u64::try_from(MAX_NFTS).unwrap()).map(ape).collect();
    store.save_listing(&listing(full)).unwrap();
}

#[test]
fn shown_and_hidden_choices_are_kept_changed_and_forgotten() {
    let scratch = Scratch::new();
    let one = ape(1).asset;
    let two = ape(2).asset;
    {
        let store = unlocked(&scratch);
        store.save_mark(&one, Some(Mark::Shown)).unwrap();
        store.save_mark(&two, Some(Mark::Shown)).unwrap();
        store.save_mark(&one, Some(Mark::Hidden)).unwrap();
        assert!(matches!(
            store.save_mark(
                &AssetId::Native("ethereum".parse().unwrap()),
                Some(Mark::Hidden)
            ),
            Err(StoreError::Invalid(_))
        ));
        store.lock();
        assert_eq!(store.marks(), Err(StoreError::Locked));
    }
    let store = unlocked(&scratch);
    let mut marks = store.marks().unwrap();
    marks.sort_by(|a, b| a.asset.cmp(&b.asset));
    assert_eq!(
        marks,
        [
            Marked {
                asset: one.clone(),
                mark: Mark::Hidden
            },
            Marked {
                asset: two.clone(),
                mark: Mark::Shown
            },
        ]
    );
    store.save_mark(&one, None).unwrap();
    store.save_mark(&one, None).unwrap();
    assert_eq!(
        store.marks().unwrap(),
        [Marked {
            asset: two,
            mark: Mark::Shown
        }]
    );
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
        let store = unlocked(&scratch);
        store.save_sent(&first).unwrap();
        store.save_sent(&second).unwrap();
        let mut done = first.clone();
        done.status = SentStatus::Confirmed;
        done.raw = None;
        done.block = Some(42);
        done.fee_paid = Some(Amount::parse("0.00002", 18).unwrap());
        store.save_sent(&done).unwrap();
    }
    let store = unlocked(&scratch);
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
    let store = unlocked(&scratch);
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
        watch: None,
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
        let store = unlocked(&scratch);
        store
            .save_scan(&scan(200), &[received(1), received(2)], &[outgoing(4)])
            .unwrap();
        store
            .save_scan(&scan(300), &[received(2)], &[outgoing(4), outgoing(5)])
            .unwrap();
    }
    let store = unlocked(&scratch);
    assert_eq!(store.scans().unwrap(), [scan(300)]);
    let mut found = store.received().unwrap();
    found.sort_by_key(|entry| entry.index);
    assert_eq!(found, [received(1), received(2)]);
    let mut sent = store.outgoing().unwrap();
    sent.sort_by_key(|entry| entry.index);
    assert_eq!(sent, [outgoing(4), outgoing(5)]);
}

#[test]
fn a_bad_scan_saves_nothing() {
    let scratch = Scratch::new();
    let store = unlocked(&scratch);
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
        let store = unlocked(&scratch);
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
    let store = unlocked(&scratch);
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
    drop(unlocked(&scratch));
    {
        let db = Database::create(scratch.file()).unwrap();
        let txn = db.begin_write().unwrap();
        txn.open_table(TableDefinition::<&str, &[u8]>::new("contacts"))
            .unwrap()
            .insert("bsc/evm:0x0", b"{not json".as_slice())
            .unwrap();
        txn.commit().unwrap();
    }
    let store = unlocked(&scratch);
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
    drop(unlocked(&scratch));
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
        let store = unlocked(&scratch);
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
    let store = unlocked(&scratch);
    assert_eq!(store.prices("USDT").unwrap(), Some(usdt));
    assert_eq!(store.prices("IRT").unwrap(), None);
}

#[test]
fn each_wallet_keeps_its_own_accounts() {
    let scratch = Scratch::new();
    {
        let store = unlocked(&scratch);
        assert_eq!(store.account_indexes(1).unwrap(), [0]);
        store.save_account_indexes(1, &[0, 1, 4]).unwrap();
        for bad in [&[][..], &[1, 0], &[2, 2], &[1u32 << 31]] {
            assert!(matches!(
                store.save_account_indexes(2, bad),
                Err(StoreError::Invalid(_))
            ));
        }
        let many: Vec<u32> = (0..21).collect();
        assert!(store.save_account_indexes(2, &many).is_err());
    }
    let store = unlocked(&scratch);
    assert_eq!(store.account_indexes(1).unwrap(), [0, 1, 4]);
    assert_eq!(store.account_indexes(2).unwrap(), [0]);
    store.forget_account_indexes(1).unwrap();
    assert_eq!(store.account_indexes(1).unwrap(), [0]);
}

#[test]
fn private_data_is_sealed_at_rest_and_needs_the_key() {
    let scratch = Scratch::new();
    {
        let store = unlocked(&scratch);
        store.save_contact(&contact("Alice")).unwrap();
        store
            .save_sent(&sent(&"ab".repeat(32), SentStatus::Confirmed))
            .unwrap();
        store.save_trusted(&trusted("ton", 9)).unwrap();
    }
    {
        let db = Database::create(scratch.file()).unwrap();
        let txn = db.begin_read().unwrap();
        for name in ["contacts", "sent"] {
            let table = txn
                .open_table(TableDefinition::<&str, &[u8]>::new(name))
                .unwrap();
            for entry in table.iter().unwrap() {
                let (key, value) = entry.unwrap();
                let key = key.value().to_owned();
                let value = String::from_utf8_lossy(value.value()).into_owned();
                assert_eq!(key.len(), 64, "{name}");
                for secret in ["Alice", "70997970", "abab", "bsc"] {
                    assert!(!key.contains(secret) && !value.contains(secret), "{name}");
                }
            }
        }
    }
    let store = Store::open(&scratch.file()).unwrap();
    assert!(matches!(store.contacts(), Err(StoreError::Locked)));
    assert!(matches!(
        store.save_contact(&contact("Bob")),
        Err(StoreError::Locked)
    ));
    assert_eq!(store.trusted().unwrap().len(), 1);
    assert!(matches!(store.unlock(key(8)), Err(StoreError::OtherVault)));
    store.unlock(key(7)).unwrap();
    assert_eq!(store.contacts().unwrap(), [contact("Alice")]);
    store.lock();
    assert!(matches!(store.sent(), Err(StoreError::Locked)));
    store.adopt(key(8)).unwrap();
    assert_eq!(store.contacts().unwrap().len(), 0);
    assert_eq!(store.sent().unwrap().len(), 0);
    assert_eq!(store.trusted().unwrap().len(), 1);
}

#[test]
fn data_from_before_sealing_is_sealed_on_the_first_unlock() {
    let scratch = Scratch::new();
    drop(Store::open(&scratch.file()).unwrap());
    let alice = contact("Alice");
    {
        let db = Database::create(scratch.file()).unwrap();
        let txn = db.begin_write().unwrap();
        txn.open_table(TableDefinition::<&str, u64>::new("meta"))
            .unwrap()
            .insert("schema", 1)
            .unwrap();
        let json = serde_json::to_vec(&alice).unwrap();
        txn.open_table(TableDefinition::<&str, &[u8]>::new("contacts"))
            .unwrap()
            .insert("plain", json.as_slice())
            .unwrap();
        txn.commit().unwrap();
    }
    let store = unlocked(&scratch);
    assert_eq!(store.contacts().unwrap(), std::slice::from_ref(&alice));
    drop(store);
    {
        let db = Database::create(scratch.file()).unwrap();
        let txn = db.begin_read().unwrap();
        let table = txn
            .open_table(TableDefinition::<&str, &[u8]>::new("contacts"))
            .unwrap();
        let (key, value) = table.iter().unwrap().next().unwrap().unwrap();
        assert_ne!(key.value(), "plain");
        assert!(!String::from_utf8_lossy(value.value()).contains("Alice"));
        let schema = txn
            .open_table(TableDefinition::<&str, u64>::new("meta"))
            .unwrap()
            .get("schema")
            .unwrap()
            .unwrap()
            .value();
        assert_eq!(schema, 2);
    }
    assert_eq!(unlocked(&scratch).contacts().unwrap(), [alice]);
}

#[test]
fn wallet_names_are_kept_validated_and_forgotten() {
    let scratch = Scratch::new();
    let store = unlocked(&scratch);
    assert_eq!(store.wallet_name(1).unwrap(), None);
    store.save_wallet_name(1, "Savings").unwrap();
    store.save_wallet_name(2, "Daily").unwrap();
    assert_eq!(store.wallet_name(1).unwrap().as_deref(), Some("Savings"));
    for bad in ["", " Savings", "Sa\u{202E}vings", &"a".repeat(41)] {
        assert!(matches!(
            store.save_wallet_name(3, bad),
            Err(StoreError::Invalid(_))
        ));
    }
    store.forget_wallet_name(1).unwrap();
    assert_eq!(store.wallet_name(1).unwrap(), None);
    assert_eq!(store.wallet_name(2).unwrap().as_deref(), Some("Daily"));
    store.lock();
    assert!(matches!(store.wallet_name(2), Err(StoreError::Locked)));
}

fn fetched(token: u64, details: Option<Details>) -> Fetched {
    Fetched {
        asset: ape(token).asset,
        link: ape(token).link.unwrap(),
        details,
        at: 1_700_000_000,
    }
}

#[test]
fn fetched_details_are_sealed_validated_and_forgotten() {
    let scratch = Scratch::new();
    let found = Details {
        name: Some("Ape #1".to_owned()),
        about: None,
        image: Some("ipfs://QmPbxeGcXhYQQNgsC6a36dDyYUcHgMLnGKnF8pVFmGsvqi".to_owned()),
    };
    {
        let store = unlocked(&scratch);
        store
            .save_fetched(&fetched(1, Some(found.clone())))
            .unwrap();
        store.save_fetched(&fetched(2, None)).unwrap();
        let mut coin = fetched(3, None);
        coin.asset = AssetId::Native("ethereum".parse().unwrap());
        let mut hidden = fetched(3, Some(found.clone()));
        hidden.details.as_mut().unwrap().about = Some("a\u{202e}b".to_owned());
        let mut long = fetched(3, Some(found.clone()));
        long.details.as_mut().unwrap().about = Some("a".repeat(601));
        let mut link = fetched(3, None);
        link.link = String::new();
        for bad in [coin, hidden, long, link] {
            assert!(matches!(
                store.save_fetched(&bad),
                Err(StoreError::Invalid(_))
            ));
        }
        store.lock();
        assert_eq!(store.fetched(), Err(StoreError::Locked));
    }
    let store = unlocked(&scratch);
    let mut kept = store.fetched().unwrap();
    kept.sort_by(|a, b| a.asset.cmp(&b.asset));
    assert_eq!(kept, [fetched(1, Some(found)), fetched(2, None)]);
    store.forget_fetched(&ape(1).asset).unwrap();
    assert_eq!(store.fetched().unwrap(), [fetched(2, None)]);
}
