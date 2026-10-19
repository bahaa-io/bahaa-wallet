use std::fs::OpenOptions;
use std::path::Path;
use std::sync::{PoisonError, RwLock, RwLockReadGuard};

use bahaa_config::{Checkpoint, Coin, Settings};
use bahaa_types::{Address, Amount, AssetId, NetworkId, Price, PriceId, is_plain_text};
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition, TableHandle};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use thiserror::Error;

mod seal;

pub use seal::StoreKey;

const SCHEMA: u64 = 2;
const PLAIN_SCHEMA: u64 = 1;
const SCHEMA_KEY: &str = "schema";
const SETTINGS_KEY: &str = "settings";
const MAX_CONTACT_NAME: usize = 40;
const MAX_TX_ID: usize = 128;
const MAX_REASON: usize = 200;
const MAX_QUOTES: usize = 512;
const MAX_MEMO: usize = 120;

const META: TableDefinition<'static, &str, u64> = TableDefinition::new("meta");
const SETTINGS: TableDefinition<'static, &str, &[u8]> = TableDefinition::new("settings");
const CONTACTS: TableDefinition<'static, &str, &[u8]> = TableDefinition::new("contacts");
const TOKENS: TableDefinition<'static, &str, &[u8]> = TableDefinition::new("tokens");
const SENT: TableDefinition<'static, &str, &[u8]> = TableDefinition::new("sent");
const RECEIVED: TableDefinition<'static, &str, &[u8]> = TableDefinition::new("received");
const SCANS: TableDefinition<'static, &str, &[u8]> = TableDefinition::new("scans");
const OUTGOING: TableDefinition<'static, &str, &[u8]> = TableDefinition::new("outgoing");
const TRUSTED: TableDefinition<'static, &str, &[u8]> = TableDefinition::new("trusted");
const PRICES: TableDefinition<'static, &str, &[u8]> = TableDefinition::new("prices");
const ACCOUNTS: TableDefinition<'static, &str, &[u8]> = TableDefinition::new("accounts");
const SEAL: TableDefinition<'static, &str, &[u8]> = TableDefinition::new("seal");
const SEALED: [TableDefinition<'static, &str, &[u8]>; 8] = [
    CONTACTS, TOKENS, SENT, RECEIVED, SCANS, OUTGOING, PRICES, ACCOUNTS,
];
const CHECK: &str = "check";
const CHECKED: &[u8] = b"bahaa store";
pub const MAX_ACCOUNTS: usize = 20;
const HIGHEST_INDEX: u32 = (1 << 31) - 1;

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum StoreError {
    #[error("another Bahaa Wallet is already using this data")]
    AlreadyOpen,
    #[error("this data was written by a newer Bahaa Wallet (schema {0}), update the app")]
    NewerSchema(u64),
    #[error("{0}")]
    Invalid(&'static str),
    #[error("a stored entry could not be read: {0}")]
    Corrupt(String),
    #[error("local database error: {0}")]
    Database(String),
    #[error("unlock the wallet to read its saved data")]
    Locked,
    #[error("the saved data belongs to another vault")]
    OtherVault,
    #[error("could not access the data file: {0}")]
    Io(std::io::ErrorKind),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contact {
    pub network: NetworkId,
    pub address: Address,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomToken {
    pub asset: AssetId,
    pub coin: Coin,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SentTx {
    pub id: String,
    pub network: NetworkId,
    pub from: Address,
    pub to: Address,
    pub asset: AssetId,
    pub amount: Amount,
    pub sequence: Option<u64>,
    pub fee_max: Amount,
    pub fee_paid: Option<Amount>,
    pub block: Option<u64>,
    pub created_at: u64,
    #[serde(default)]
    pub expires_at: Option<u64>,
    pub status: SentStatus,
    pub raw: Option<String>,
    #[serde(default)]
    pub transaction: Option<String>,
    #[serde(default)]
    pub memo: Option<String>,
    #[serde(default)]
    pub replaces: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SentStatus {
    Sending,
    Pending,
    Confirmed,
    Failed(String),
    Dropped,
}

impl SentStatus {
    pub fn is_open(&self) -> bool {
        matches!(self, Self::Sending | Self::Pending)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Received {
    pub id: String,
    pub index: u64,
    pub network: NetworkId,
    pub from: Address,
    pub to: Address,
    pub asset: AssetId,
    pub amount: Amount,
    pub block: u64,
    pub time: u64,
    #[serde(default)]
    pub memo: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Outgoing {
    pub id: String,
    pub index: u64,
    pub network: NetworkId,
    pub from: Address,
    pub to: Address,
    pub asset: AssetId,
    pub amount: Amount,
    pub block: u64,
    pub time: u64,
    #[serde(default)]
    pub memo: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scan {
    pub network: NetworkId,
    pub address: Address,
    pub next: u64,
    pub complete_from: u64,
    pub complete_since: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedBlock {
    pub network: NetworkId,
    pub checkpoint: Checkpoint,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedPrices {
    pub base: String,
    pub quotes: Vec<SavedQuote>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedQuote {
    pub id: PriceId,
    pub price: Price,
    pub updated_at: u64,
}

pub struct Store {
    db: Database,
    key: RwLock<Option<StoreKey>>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        prepare(path)?;
        let db = Database::create(path).map_err(database)?;
        let store = Self {
            db,
            key: RwLock::new(None),
        };
        store.migrate()?;
        Ok(store)
    }

    pub fn unlock(&self, key: StoreKey) -> Result<(), StoreError> {
        self.take(key, false)
    }

    pub fn adopt(&self, key: StoreKey) -> Result<(), StoreError> {
        self.take(key, true)
    }

    pub fn lock(&self) {
        *self.key.write().unwrap_or_else(PoisonError::into_inner) = None;
    }

    pub fn is_unlocked(&self) -> bool {
        self.key().is_some()
    }

    fn take(&self, key: StoreKey, fresh: bool) -> Result<(), StoreError> {
        let txn = self.db.begin_write().map_err(database)?;
        {
            let mut meta = txn.open_table(META).map_err(database)?;
            let schema = meta
                .get(SCHEMA_KEY)
                .map_err(database)?
                .map_or(SCHEMA, |value| value.value());
            let mut seal = txn.open_table(SEAL).map_err(database)?;
            let check = key.name(SEAL.name(), CHECK)?;
            let known = seal.get(check.as_str()).map_err(database)?.is_some();
            let foreign = !known && seal.iter().map_err(database)?.next().is_some();
            if schema == PLAIN_SCHEMA {
                for table in SEALED {
                    let mut opened = txn.open_table(table).map_err(database)?;
                    let mut plain = Vec::new();
                    for entry in opened.iter().map_err(database)? {
                        let (name, value) = entry.map_err(database)?;
                        plain.push((name.value().to_owned(), value.value().to_vec()));
                    }
                    for (name, value) in plain {
                        opened.remove(name.as_str()).map_err(database)?;
                        let sealed_name = key.name(table.name(), &name)?;
                        let sealed = key.seal(table.name(), &sealed_name, &value)?;
                        opened
                            .insert(sealed_name.as_str(), sealed.as_slice())
                            .map_err(database)?;
                    }
                }
            } else if foreign && !fresh {
                return Err(StoreError::OtherVault);
            } else if foreign {
                for table in SEALED {
                    let mut opened = txn.open_table(table).map_err(database)?;
                    opened.retain(|_, _| false).map_err(database)?;
                }
                seal.retain(|_, _| false).map_err(database)?;
            }
            if known {
                let stored = seal
                    .get(check.as_str())
                    .map_err(database)?
                    .map(|value| value.value().to_vec())
                    .unwrap_or_default();
                if key
                    .open(SEAL.name(), &check, &stored)
                    .ok()
                    .as_deref()
                    .map(Vec::as_slice)
                    != Some(CHECKED)
                {
                    return Err(StoreError::OtherVault);
                }
            } else {
                let sealed = key.seal(SEAL.name(), &check, CHECKED)?;
                seal.insert(check.as_str(), sealed.as_slice())
                    .map_err(database)?;
            }
            meta.insert(SCHEMA_KEY, SCHEMA).map_err(database)?;
        }
        txn.commit().map_err(database)?;
        *self.key.write().unwrap_or_else(PoisonError::into_inner) = Some(key);
        Ok(())
    }

    fn key(&self) -> RwLockReadGuard<'_, Option<StoreKey>> {
        self.key.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn entry(
        &self,
        table: TableDefinition<'_, &str, &[u8]>,
        key: &str,
        bytes: &[u8],
    ) -> Result<(String, Vec<u8>), StoreError> {
        if !sealed(table) {
            return Ok((key.to_owned(), bytes.to_vec()));
        }
        let guard = self.key();
        let seal = guard.as_ref().ok_or(StoreError::Locked)?;
        let name = seal.name(table.name(), key)?;
        let value = seal.seal(table.name(), &name, bytes)?;
        Ok((name, value))
    }

    fn stored_name(
        &self,
        table: TableDefinition<'_, &str, &[u8]>,
        key: &str,
    ) -> Result<String, StoreError> {
        if !sealed(table) {
            return Ok(key.to_owned());
        }
        self.key()
            .as_ref()
            .ok_or(StoreError::Locked)?
            .name(table.name(), key)
    }

    pub fn settings(&self) -> Result<Settings, StoreError> {
        match self.get(SETTINGS, SETTINGS_KEY)? {
            Some(bytes) => {
                let text =
                    String::from_utf8(bytes).map_err(|err| StoreError::Corrupt(err.to_string()))?;
                Settings::from_json(&text).map_err(|err| StoreError::Corrupt(err.to_string()))
            }
            None => Ok(Settings::default()),
        }
    }

    pub fn save_settings(&self, settings: &Settings) -> Result<(), StoreError> {
        let text = settings
            .to_json()
            .map_err(|err| StoreError::Corrupt(err.to_string()))?;
        Settings::from_json(&text)
            .map_err(|_| StoreError::Invalid("these settings are out of range"))?;
        self.put(SETTINGS, SETTINGS_KEY, text.as_bytes())
    }

    pub fn contacts(&self) -> Result<Vec<Contact>, StoreError> {
        self.all(CONTACTS)
    }

    pub fn save_contact(&self, contact: &Contact) -> Result<(), StoreError> {
        if !is_plain_text(&contact.name, MAX_CONTACT_NAME) {
            return Err(StoreError::Invalid(
                "a contact name must be 1 to 40 visible characters, without hidden ones",
            ));
        }
        let key = address_key(&contact.network, &contact.address);
        self.put(CONTACTS, &key, &encode(contact)?)
    }

    pub fn remove_contact(
        &self,
        network: &NetworkId,
        address: &Address,
    ) -> Result<bool, StoreError> {
        self.delete(CONTACTS, &address_key(network, address))
    }

    pub fn tokens(&self) -> Result<Vec<CustomToken>, StoreError> {
        self.all(TOKENS)
    }

    pub fn save_token(&self, token: &CustomToken) -> Result<(), StoreError> {
        if token.asset.contract().is_none() {
            return Err(StoreError::Invalid(
                "a custom token needs a contract address",
            ));
        }
        token.coin.check().map_err(StoreError::Invalid)?;
        self.put(TOKENS, &token.asset.to_string(), &encode(token)?)
    }

    pub fn remove_token(&self, asset: &AssetId) -> Result<bool, StoreError> {
        self.delete(TOKENS, &asset.to_string())
    }

    pub fn sent(&self) -> Result<Vec<SentTx>, StoreError> {
        self.all(SENT)
    }

    pub fn save_sent(&self, tx: &SentTx) -> Result<(), StoreError> {
        if !is_plain_text(&tx.id, MAX_TX_ID) {
            return Err(StoreError::Invalid("not a transaction id"));
        }
        if let SentStatus::Failed(reason) = &tx.status
            && !is_plain_text(reason, MAX_REASON)
        {
            return Err(StoreError::Invalid(
                "a failure reason must be short plain text",
            ));
        }
        if tx
            .transaction
            .as_ref()
            .is_some_and(|hash| !is_plain_text(hash, MAX_TX_ID))
        {
            return Err(StoreError::Invalid("not a transaction hash"));
        }
        if !memo_ok(tx.memo.as_ref()) {
            return Err(StoreError::Invalid("a memo must be short plain text"));
        }
        if tx.status.is_open() != tx.raw.is_some() {
            return Err(StoreError::Invalid(
                "only a transaction still on its way keeps its signed bytes",
            ));
        }
        self.put(SENT, &format!("{}/{}", tx.network, tx.id), &encode(tx)?)
    }

    pub fn received(&self) -> Result<Vec<Received>, StoreError> {
        self.all(RECEIVED)
    }

    pub fn scans(&self) -> Result<Vec<Scan>, StoreError> {
        self.all(SCANS)
    }

    pub fn outgoing(&self) -> Result<Vec<Outgoing>, StoreError> {
        self.all(OUTGOING)
    }

    pub fn save_scan(
        &self,
        scan: &Scan,
        found: &[Received],
        sent: &[Outgoing],
    ) -> Result<(), StoreError> {
        if scan.complete_from > scan.next {
            return Err(StoreError::Invalid(
                "a scan cannot be complete beyond where it stopped",
            ));
        }
        for entry in found {
            if !is_plain_text(&entry.id, MAX_TX_ID) {
                return Err(StoreError::Invalid("not a transaction id"));
            }
            if !memo_ok(entry.memo.as_ref()) {
                return Err(StoreError::Invalid("a memo must be short plain text"));
            }
            if entry.network != scan.network || entry.to != scan.address {
                return Err(StoreError::Invalid(
                    "a transfer must belong to the scan that found it",
                ));
            }
        }
        for entry in sent {
            if !is_plain_text(&entry.id, MAX_TX_ID) {
                return Err(StoreError::Invalid("not a transaction id"));
            }
            if !memo_ok(entry.memo.as_ref()) {
                return Err(StoreError::Invalid("a memo must be short plain text"));
            }
            if entry.network != scan.network || entry.from != scan.address {
                return Err(StoreError::Invalid(
                    "a transfer must belong to the scan that found it",
                ));
            }
        }
        let txn = self.db.begin_write().map_err(database)?;
        {
            let mut received = txn.open_table(RECEIVED).map_err(database)?;
            for entry in found {
                let key = format!("{}/{}/{}", entry.network, entry.id, entry.index);
                let (name, value) = self.entry(RECEIVED, &key, &encode(entry)?)?;
                received
                    .insert(name.as_str(), value.as_slice())
                    .map_err(database)?;
            }
            let mut outgoing = txn.open_table(OUTGOING).map_err(database)?;
            for entry in sent {
                let key = format!("{}/{}/{}", entry.network, entry.id, entry.index);
                let (name, value) = self.entry(OUTGOING, &key, &encode(entry)?)?;
                outgoing
                    .insert(name.as_str(), value.as_slice())
                    .map_err(database)?;
            }
            let key = address_key(&scan.network, &scan.address);
            let (name, value) = self.entry(SCANS, &key, &encode(scan)?)?;
            txn.open_table(SCANS)
                .map_err(database)?
                .insert(name.as_str(), value.as_slice())
                .map_err(database)?;
        }
        txn.commit().map_err(database)
    }

    pub fn trusted(&self) -> Result<Vec<TrustedBlock>, StoreError> {
        self.all(TRUSTED)
    }

    pub fn save_trusted(&self, block: &TrustedBlock) -> Result<(), StoreError> {
        if block.checkpoint.seqno == 0 {
            return Err(StoreError::Invalid(
                "a trusted block cannot be the zero state",
            ));
        }
        self.put(TRUSTED, block.network.as_str(), &encode(block)?)
    }

    pub fn prices(&self, base: &str) -> Result<Option<SavedPrices>, StoreError> {
        match self.get(PRICES, base)? {
            Some(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|err| StoreError::Corrupt(err.to_string())),
            None => Ok(None),
        }
    }

    pub fn save_prices(&self, prices: &SavedPrices) -> Result<(), StoreError> {
        let base = &prices.base;
        if !(3..=5).contains(&base.len()) || !base.bytes().all(|byte| byte.is_ascii_uppercase()) {
            return Err(StoreError::Invalid("not a price base"));
        }
        if prices.quotes.len() > MAX_QUOTES {
            return Err(StoreError::Invalid("too many prices to keep"));
        }
        self.put(PRICES, base, &encode(prices)?)
    }

    pub fn account_indexes(&self, wallet: u32) -> Result<Vec<u32>, StoreError> {
        match self.get(ACCOUNTS, &wallet.to_string())? {
            Some(bytes) => {
                serde_json::from_slice(&bytes).map_err(|err| StoreError::Corrupt(err.to_string()))
            }
            None => Ok(vec![0]),
        }
    }

    pub fn save_account_indexes(&self, wallet: u32, indexes: &[u32]) -> Result<(), StoreError> {
        let ordered = indexes.windows(2).all(|pair| pair[0] < pair[1]);
        let sized = (1..=MAX_ACCOUNTS).contains(&indexes.len());
        if !ordered || !sized || indexes.iter().any(|index| *index > HIGHEST_INDEX) {
            return Err(StoreError::Invalid(
                "a wallet keeps one to twenty distinct accounts",
            ));
        }
        self.put(ACCOUNTS, &wallet.to_string(), &encode(&indexes)?)
    }

    pub fn forget_account_indexes(&self, wallet: u32) -> Result<(), StoreError> {
        self.delete(ACCOUNTS, &wallet.to_string()).map(drop)
    }

    fn migrate(&self) -> Result<(), StoreError> {
        let txn = self.db.begin_write().map_err(database)?;
        {
            let mut meta = txn.open_table(META).map_err(database)?;
            let found = meta
                .get(SCHEMA_KEY)
                .map_err(database)?
                .map(|value| value.value());
            match found {
                Some(schema) if schema > SCHEMA => return Err(StoreError::NewerSchema(schema)),
                Some(_) => {}
                None => {
                    meta.insert(SCHEMA_KEY, SCHEMA).map_err(database)?;
                }
            }
            for table in [
                SETTINGS, CONTACTS, TOKENS, SENT, RECEIVED, SCANS, TRUSTED, OUTGOING, PRICES,
                ACCOUNTS, SEAL,
            ] {
                txn.open_table(table).map_err(database)?;
            }
        }
        txn.commit().map_err(database)
    }

    fn get(
        &self,
        table: TableDefinition<'_, &str, &[u8]>,
        key: &str,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        let name = self.stored_name(table, key)?;
        let txn = self.db.begin_read().map_err(database)?;
        let opened = txn.open_table(table).map_err(database)?;
        let Some(value) = opened.get(name.as_str()).map_err(database)? else {
            return Ok(None);
        };
        if !sealed(table) {
            return Ok(Some(value.value().to_vec()));
        }
        let guard = self.key();
        let seal = guard.as_ref().ok_or(StoreError::Locked)?;
        Ok(Some(
            seal.open(table.name(), &name, value.value())?.to_vec(),
        ))
    }

    fn put(
        &self,
        table: TableDefinition<'_, &str, &[u8]>,
        key: &str,
        bytes: &[u8],
    ) -> Result<(), StoreError> {
        let (name, value) = self.entry(table, key, bytes)?;
        let txn = self.db.begin_write().map_err(database)?;
        txn.open_table(table)
            .map_err(database)?
            .insert(name.as_str(), value.as_slice())
            .map_err(database)?;
        txn.commit().map_err(database)
    }

    fn delete(
        &self,
        table: TableDefinition<'_, &str, &[u8]>,
        key: &str,
    ) -> Result<bool, StoreError> {
        let name = self.stored_name(table, key)?;
        let txn = self.db.begin_write().map_err(database)?;
        let removed = txn
            .open_table(table)
            .map_err(database)?
            .remove(name.as_str())
            .map_err(database)?
            .is_some();
        txn.commit().map_err(database)?;
        Ok(removed)
    }

    fn all<T: DeserializeOwned>(
        &self,
        table: TableDefinition<'_, &str, &[u8]>,
    ) -> Result<Vec<T>, StoreError> {
        let guard = self.key();
        let seal = if sealed(table) {
            Some(guard.as_ref().ok_or(StoreError::Locked)?)
        } else {
            None
        };
        let txn = self.db.begin_read().map_err(database)?;
        let opened = txn.open_table(table).map_err(database)?;
        opened
            .iter()
            .map_err(database)?
            .map(|entry| {
                let (name, value) = entry.map_err(database)?;
                let read = |bytes: &[u8]| {
                    serde_json::from_slice(bytes)
                        .map_err(|err| StoreError::Corrupt(err.to_string()))
                };
                match seal {
                    Some(seal) => read(&seal.open(table.name(), name.value(), value.value())?),
                    None => read(value.value()),
                }
            })
            .collect()
    }
}

fn sealed(table: TableDefinition<'_, &str, &[u8]>) -> bool {
    SEALED.iter().any(|known| known.name() == table.name())
}

fn memo_ok(memo: Option<&String>) -> bool {
    memo.is_none_or(|memo| !memo.is_empty() && is_plain_text(memo, MAX_MEMO))
}

fn address_key(network: &NetworkId, address: &Address) -> String {
    format!("{network}/{}:{address}", address.family())
}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, StoreError> {
    serde_json::to_vec(value).map_err(|err| StoreError::Corrupt(err.to_string()))
}

fn database(err: impl Into<redb::Error>) -> StoreError {
    match err.into() {
        redb::Error::DatabaseAlreadyOpen => StoreError::AlreadyOpen,
        other => StoreError::Database(other.to_string()),
    }
}

fn prepare(path: &Path) -> Result<(), StoreError> {
    if path.exists() {
        return Ok(());
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(_) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(err) => Err(StoreError::Io(err.kind())),
    }
}
