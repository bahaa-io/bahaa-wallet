use bahaa_config::Network;
use bahaa_crypto::Seed;
use bahaa_types::{Address, AssetId};

use crate::{
    Balance, ChainError, Cursor, Decoded, FeeQuote, HistoryPage, Signed, Transfer, TxId, TxStatus,
};

pub trait Chain: Send + Sync {
    type Plan: Send + Sync;

    fn network(&self) -> &Network;

    fn derive(&self, seed: &Seed, account: u32) -> Result<Address, ChainError>;

    fn balances(
        &self,
        address: &Address,
        assets: &[AssetId],
    ) -> impl Future<Output = Result<Vec<Balance>, ChainError>> + Send;

    fn prepare(
        &self,
        transfer: &Transfer,
    ) -> impl Future<Output = Result<(Self::Plan, FeeQuote), ChainError>> + Send;

    fn sign(&self, plan: &Self::Plan, seed: &Seed) -> Result<Signed, ChainError>;

    fn decode(&self, signed: &Signed) -> Result<Decoded, ChainError>;

    fn broadcast(&self, signed: &Signed) -> impl Future<Output = Result<TxId, ChainError>> + Send;

    fn status(&self, id: &TxId) -> impl Future<Output = Result<TxStatus, ChainError>> + Send;
}

pub trait HistoryProvider: Send + Sync {
    fn history(
        &self,
        address: &Address,
        before: Option<&Cursor>,
    ) -> impl Future<Output = Result<HistoryPage, ChainError>> + Send;
}
