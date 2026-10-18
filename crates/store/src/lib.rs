use std::fs::OpenOptions;
use std::path::Path;

use bahaa_config::{Checkpoint, Coin, Settings};
use bahaa_types::{Address, Amount, AssetId, NetworkId, is_plain_text};
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const SCHEMA: u64 = 1;
const SCHEMA_KEY: &str = "schema";
const SETTINGS_KEY: &str = "settings";
const MAX_CONTACT_NAME: usize = 40;
const MAX_TX_ID: usize = 128;
const MAX_REASON: usize = 200;

const META: TableDefinition<'static, &str, u64> = TableDefinition::new("meta");
const SETTINGS: TableDefinition<'static, &str, &[u8]> = TableDefinition::new("settings");
const CONTACTS: TableDefinition<'static, &str, &[u8]> = TableDefinition::new("contacts");
const TOKENS: TableDefinition<'static, &str, &[u8]> = TableDefinition::new("tokens");
const SENT: TableDefinition<'static, &str, &[u8]> = TableDefinition::new("sent");
const RECEIVED: TableDefinition<'static, &str, &[u8]> = TableDefinition::new("received");
const SCANS: TableDefinition<'static, &str, &[u8]> = TableDefinition::new("scans");
const TRUSTED: TableDefinition<'static, &str, &[u8]> = TableDefinition::new("trusted");

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

pub struct Store {
    db: Database,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        prepare(path)?;
        let db = Database::create(path).map_err(database)?;
        let store = Self { db };
        store.migrate()?;
        Ok(store)
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

    pub fn save_scan(&self, scan: &Scan, found: &[Received]) -> Result<(), StoreError> {
        if scan.complete_from > scan.next {
            return Err(StoreError::Invalid(
                "a scan cannot be complete beyond where it stopped",
            ));
        }
        for entry in found {
            if !is_plain_text(&entry.id, MAX_TX_ID) {
                return Err(StoreError::Invalid("not a transaction id"));
            }
            if entry.network != scan.network || entry.to != scan.address {
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
                received
                    .insert(key.as_str(), encode(entry)?.as_slice())
                    .map_err(database)?;
            }
            let key = address_key(&scan.network, &scan.address);
            txn.open_table(SCANS)
                .map_err(database)?
                .insert(key.as_str(), encode(scan)?.as_slice())
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
            for table in [SETTINGS, CONTACTS, TOKENS, SENT, RECEIVED, SCANS, TRUSTED] {
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
        let txn = self.db.begin_read().map_err(database)?;
        let table = txn.open_table(table).map_err(database)?;
        Ok(table
            .get(key)
            .map_err(database)?
            .map(|value| value.value().to_vec()))
    }

    fn put(
        &self,
        table: TableDefinition<'_, &str, &[u8]>,
        key: &str,
        bytes: &[u8],
    ) -> Result<(), StoreError> {
        let txn = self.db.begin_write().map_err(database)?;
        txn.open_table(table)
            .map_err(database)?
            .insert(key, bytes)
            .map_err(database)?;
        txn.commit().map_err(database)
    }

    fn delete(
        &self,
        table: TableDefinition<'_, &str, &[u8]>,
        key: &str,
    ) -> Result<bool, StoreError> {
        let txn = self.db.begin_write().map_err(database)?;
        let removed = txn
            .open_table(table)
            .map_err(database)?
            .remove(key)
            .map_err(database)?
            .is_some();
        txn.commit().map_err(database)?;
        Ok(removed)
    }

    fn all<T: DeserializeOwned>(
        &self,
        table: TableDefinition<'_, &str, &[u8]>,
    ) -> Result<Vec<T>, StoreError> {
        let txn = self.db.begin_read().map_err(database)?;
        let table = txn.open_table(table).map_err(database)?;
        table
            .iter()
            .map_err(database)?
            .map(|entry| {
                let (_, value) = entry.map_err(database)?;
                serde_json::from_slice(value.value())
                    .map_err(|err| StoreError::Corrupt(err.to_string()))
            })
            .collect()
    }
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
