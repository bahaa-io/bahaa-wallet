use bahaa_chain::{ChainError, FeeRate, RateUnit, Speed, TxId, Wait};
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

pub(crate) const TARGETS: [u32; 8] = [2, 3, 6, 12, 24, 72, 144, 504];

const FLOOR_RATE: u64 = 1_000;
const MAX_RATE: u64 = 5_000_000;
const FAST: u32 = 2;
const NORMAL: u32 = 6;
const SLOW: u32 = 144;
const RATE_PLACES: u8 = 3;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Estimates {
    pub relay: Option<u64>,
    pub targets: Vec<(u32, u64)>,
}

impl Estimates {
    pub(crate) fn rate(&self, speed: Speed) -> Result<u64, ChainError> {
        let fast = self.at(FAST);
        let normal = self.at(NORMAL).min(fast);
        Ok(match speed {
            Speed::Fast => fast,
            Speed::Normal => normal,
            Speed::Slow => self.at(SLOW).min(normal),
            Speed::Custom(rate) => self.custom(rate)?,
        })
    }

    fn floor(&self) -> u64 {
        self.relay.unwrap_or(FLOOR_RATE)
    }

    fn at(&self, target: u32) -> u64 {
        self.targets
            .iter()
            .find(|(blocks, _)| *blocks >= target)
            .or_else(|| self.targets.last())
            .map_or(0, |(_, rate)| *rate)
            .max(self.floor())
            .min(MAX_RATE)
    }

    fn custom(&self, rate: FeeRate) -> Result<u64, ChainError> {
        let places = rate.amount.decimals();
        if rate.unit != RateUnit::SatPerVbyte || places > RATE_PLACES {
            return Err(ChainError::Invalid(
                "a Bitcoin fee rate is in sat/vB with at most 3 decimals",
            ));
        }
        let scaled = rate
            .amount
            .units()
            .checked_mul(U256::from(10u8).pow(U256::from(RATE_PLACES - places)))
            .and_then(|units| u64::try_from(units).ok())
            .ok_or(ChainError::Invalid("this fee rate is far too high"))?;
        if scaled < self.floor() {
            return Err(ChainError::Invalid(
                "this fee rate is below what Bitcoin servers relay",
            ));
        }
        if scaled > MAX_RATE {
            return Err(ChainError::Invalid("this fee rate is far too high"));
        }
        Ok(scaled)
    }

    pub(crate) fn wait(&self, rate: u64, block_ms: u32) -> Option<Wait> {
        if self.relay.is_some_and(|relay| rate < relay) {
            return Some(Wait::Stuck);
        }
        let slowest = self.targets.last()?.0;
        Some(
            match self.targets.iter().find(|(_, needed)| rate >= *needed) {
                Some((blocks, _)) => Wait::blocks(u64::from(*blocks), block_ms),
                None => Wait::over_blocks(u64::from(slowest), block_ms),
            },
        )
    }
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

    pub(crate) async fn estimates(&self) -> Result<Estimates, ChainError> {
        self.either(Electrum::estimates, Esplora::estimates).await
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

    fn custom(text: &str, unit: RateUnit) -> Speed {
        let places = text.split('.').nth(1).map_or(0, str::len);
        Speed::Custom(FeeRate {
            amount: Amount::parse(text, u8::try_from(places).unwrap()).unwrap(),
            unit,
        })
    }

    #[test]
    fn each_speed_takes_its_own_target_never_below_what_servers_relay() {
        let estimates = Estimates {
            relay: Some(100),
            targets: vec![
                (2, 1_070),
                (3, 1_070),
                (6, 130),
                (12, 120),
                (24, 120),
                (144, 110),
                (504, 110),
            ],
        };
        let rate = |speed| estimates.rate(speed).unwrap();
        assert_eq!(
            [Speed::Fast, Speed::Normal, Speed::Slow].map(rate),
            [1_070, 130, 110]
        );
        let wait = |speed| estimates.wait(rate(speed), 600_000);
        assert_eq!(wait(Speed::Fast), Some(Wait::About(1_200)));
        assert_eq!(wait(Speed::Normal), Some(Wait::About(3_600)));
        assert_eq!(wait(Speed::Slow), Some(Wait::About(86_400)));
        let refused_slow = Estimates {
            relay: Some(100),
            targets: vec![(2, 3_000), (3, 2_500)],
        };
        assert_eq!(refused_slow.rate(Speed::Normal).unwrap(), 2_500);
        let odd = Estimates {
            relay: None,
            targets: vec![(2, 500), (6, 900), (144, 2_000)],
        };
        assert_eq!(
            [Speed::Fast, Speed::Normal, Speed::Slow].map(|speed| odd.rate(speed).unwrap()),
            [1_000, 1_000, 1_000]
        );
        assert_eq!(rate(custom("0.25", RateUnit::SatPerVbyte)), 250);
        assert_eq!(rate(custom("12", RateUnit::SatPerVbyte)), 12_000);
        for bad in [
            custom("0.05", RateUnit::SatPerVbyte),
            custom("0.0005", RateUnit::SatPerVbyte),
            custom("6000", RateUnit::SatPerVbyte),
            custom("2", RateUnit::Gwei),
        ] {
            assert!(estimates.rate(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn estimates_give_the_send_rate_and_the_blocks_any_rate_waits() {
        let estimates = Estimates {
            relay: Some(100),
            targets: vec![(2, 3_000), (6, 2_000), (24, 1_200), (504, 500)],
        };
        let wait = |rate| estimates.wait(rate, 600_000);
        assert_eq!(estimates.rate(Speed::Fast).unwrap(), 3_000);
        assert_eq!(wait(3_500), Some(Wait::About(1_200)));
        assert_eq!(wait(2_000), Some(Wait::About(3_600)));
        assert_eq!(wait(1_000), Some(Wait::About(302_400)));
        assert_eq!(wait(300), Some(Wait::Over(302_400)));
        assert_eq!(wait(50), Some(Wait::Stuck));
        assert_eq!(Estimates::default().rate(Speed::Fast).unwrap(), FLOOR_RATE);
        assert_eq!(Estimates::default().wait(2_000, 600_000), None);
        let quiet = Estimates {
            relay: Some(100),
            targets: vec![(2, 120)],
        };
        assert_eq!(quiet.rate(Speed::Fast).unwrap(), 120);
    }

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
