use bahaa_chain::{ChainError, TxId};
use bahaa_config::{Network, Protocol};
use bahaa_types::{Amount, Price};
use ruint::aliases::U256;

use crate::electrum::Electrum;
use crate::esplora::Esplora;
use crate::proof::Header;
use crate::tx::{Hash, Outpoint};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Seen {
    pub txid: Hash,
    pub height: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Unspent {
    pub outpoint: Outpoint,
    pub value: u64,
    pub height: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Branch {
    pub pos: u64,
    pub hashes: Vec<Hash>,
}

#[derive(Debug)]
pub(crate) struct Sources {
    electrum: Option<Electrum>,
    esplora: Option<Esplora>,
}

impl Sources {
    pub(crate) fn new(network: &Network, genesis: &[u8; 32]) -> Result<Self, ChainError> {
        let of = |protocol| {
            network
                .endpoints
                .iter()
                .filter(|endpoint| endpoint.protocol == protocol)
                .cloned()
                .collect::<Vec<_>>()
        };
        let (electrum, esplora) = (of(Protocol::Electrum), of(Protocol::Esplora));
        Ok(Self {
            electrum: (!electrum.is_empty())
                .then(|| Electrum::new(electrum, genesis))
                .transpose()?,
            esplora: (!esplora.is_empty())
                .then(|| Esplora::new(esplora, genesis))
                .transpose()?,
        })
    }

    pub(crate) async fn histories(
        &self,
        scripts: &[Vec<u8>],
    ) -> Result<Vec<Vec<Seen>>, ChainError> {
        self.either(
            |electrum| electrum.histories(scripts),
            |esplora| esplora.histories(scripts),
        )
        .await
    }

    pub(crate) async fn unspent(
        &self,
        scripts: &[Vec<u8>],
    ) -> Result<Vec<Vec<Unspent>>, ChainError> {
        self.either(
            |electrum| electrum.unspent(scripts),
            |esplora| esplora.unspent(scripts),
        )
        .await
    }

    pub(crate) async fn transactions(
        &self,
        ids: &[Hash],
    ) -> Result<Vec<Option<Vec<u8>>>, ChainError> {
        self.either(
            |electrum| electrum.transactions(ids),
            |esplora| esplora.transactions(ids),
        )
        .await
    }

    pub(crate) async fn headers(&self, heights: &[u64]) -> Result<Vec<Vec<u8>>, ChainError> {
        self.either(
            |electrum| electrum.headers(heights),
            |esplora| esplora.headers(heights),
        )
        .await
    }

    pub(crate) async fn range(&self, start: u64, count: u64) -> Result<Vec<Header>, ChainError> {
        match &self.electrum {
            Some(electrum) => electrum.range(start, count).await,
            None => Err(ChainError::Unsupported(
                "header ranges without Electrum servers",
            )),
        }
    }

    pub(crate) async fn merkle(&self, items: &[(Hash, u64)]) -> Result<Vec<Branch>, ChainError> {
        self.either(
            |electrum| electrum.merkle(items),
            |esplora| esplora.merkle(items),
        )
        .await
    }

    pub(crate) async fn rates(&self) -> Result<Vec<u64>, ChainError> {
        self.either(Electrum::rates, Esplora::rates).await
    }

    pub(crate) async fn broadcast(&self, raw: &[u8], id: &TxId) -> Result<TxId, ChainError> {
        self.either(
            |electrum| electrum.broadcast(raw, id),
            |esplora| esplora.broadcast(raw, id),
        )
        .await
    }

    async fn either<'a, T, A, B>(
        &'a self,
        electrum: impl FnOnce(&'a Electrum) -> A,
        esplora: impl FnOnce(&'a Esplora) -> B,
    ) -> Result<T, ChainError>
    where
        A: Future<Output = Result<T, ChainError>>,
        B: Future<Output = Result<T, ChainError>>,
    {
        if let Some(client) = &self.electrum {
            match electrum(client).await {
                Err(ChainError::Network(_)) if self.esplora.is_some() => {}
                done => return done,
            }
        }
        match &self.esplora {
            Some(client) => esplora(client).await,
            None => Err(ChainError::Network(
                "this network has no Bitcoin servers".to_owned(),
            )),
        }
    }
}

pub(crate) fn rate(text: &str, scale: u64) -> Option<u64> {
    let price = Price::parse(text).ok()?;
    let units = price
        .value_of(Amount::new(U256::from(scale), 0).ok()?, 0)
        .ok()?;
    u64::try_from(units.units()).ok()
}

pub(crate) fn branch(reply: &serde_json::Value, height: u64) -> Option<Branch> {
    if reply["block_height"].as_u64()? != height {
        return None;
    }
    Some(Branch {
        pos: reply["pos"].as_u64()?,
        hashes: reply["merkle"]
            .as_array()?
            .iter()
            .map(|node| bahaa_types::hex::decode_array(node.as_str()?))
            .collect::<Option<_>>()?,
    })
}

pub(crate) fn already(message: &str) -> bool {
    message.to_lowercase().contains("already")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fee_rates_are_read_exactly_from_their_text() {
        let electrum = 100_000_000;
        assert_eq!(rate("1.002e-05", electrum), Some(1_002));
        assert_eq!(rate("0.00001002", electrum), Some(1_002));
        assert_eq!(rate("1e-6", electrum), Some(100));
        assert_eq!(rate("-1", electrum), None);
        assert_eq!(rate("\"x\"", electrum), None);
        assert_eq!(rate("0.7060000000000001", 1_000), Some(706));
        assert_eq!(rate("1.067", 1_000), Some(1_067));
    }
}
