use std::collections::BTreeSet;

use bahaa_config::{Coin, Fiat, Registry};
use bahaa_market::{Quote, Snapshot, Status};
use bahaa_store::CustomToken;
use bahaa_types::{Address, Amount, AssetId, ChainFamily, NetworkId, Price, PriceId};

use crate::WalletId;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub wallet: WalletId,
    pub family: ChainFamily,
    pub index: u32,
    pub address: Address,
    pub network: Option<NetworkId>,
}

impl Account {
    pub fn serves(&self, network: &NetworkId, family: ChainFamily) -> bool {
        self.family == family && self.network.as_ref().is_none_or(|only| only == network)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Held {
    pub wallet: WalletId,
    pub address: Address,
    pub asset: AssetId,
    pub amount: Amount,
    pub staked: Option<Amount>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkHealth {
    pub network: NetworkId,
    pub problem: Option<String>,
    pub history_problem: Option<String>,
    pub checked_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Holding {
    pub wallet: WalletId,
    pub address: Address,
    pub asset: AssetId,
    pub symbol: String,
    pub name: String,
    pub amount: Amount,
    pub staked: Option<Amount>,
    pub price: Option<Price>,
    pub price_at: Option<u64>,
    pub value: Option<Amount>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Portfolio {
    pub fiat: Fiat,
    pub holdings: Vec<Holding>,
    pub total: Option<Amount>,
    pub networks: Vec<NetworkHealth>,
    pub prices: Status,
}

pub fn coin<'a>(
    registry: &'a Registry,
    custom: &'a [CustomToken],
    asset: &AssetId,
) -> Option<&'a Coin> {
    let builtin = registry.get(asset.network()).and_then(|network| {
        network
            .assets()
            .find(|(id, _)| id == asset)
            .map(|(_, coin)| coin)
    });
    builtin.or_else(|| {
        custom
            .iter()
            .find(|token| &token.asset == asset)
            .map(|token| &token.coin)
    })
}

pub(crate) fn value(
    registry: &Registry,
    custom: &[CustomToken],
    snapshot: &Snapshot,
    fiat: Fiat,
    asset: &AssetId,
    amount: Amount,
) -> Option<Amount> {
    quote(coin(registry, custom, asset)?, snapshot, fiat)?
        .price
        .value_of(amount, fiat.places())
        .ok()
}

fn quote(coin: &Coin, snapshot: &Snapshot, fiat: Fiat) -> Option<Quote> {
    coin.price
        .as_ref()
        .filter(|_| snapshot.base == fiat.price_base())
        .and_then(|id| snapshot.quotes.get(id))
        .copied()
}

pub(crate) fn wanted<'a>(
    registry: &Registry,
    custom: &[CustomToken],
    held: &'a [Held],
    received: impl IntoIterator<Item = &'a AssetId>,
) -> BTreeSet<PriceId> {
    held.iter()
        .filter(|holding| !holding.amount.is_zero() || holding.staked.is_some())
        .map(|holding| &holding.asset)
        .chain(received)
        .filter_map(|asset| coin(registry, custom, asset)?.price.clone())
        .collect()
}

pub(crate) fn build(
    registry: &Registry,
    custom: &[CustomToken],
    held: &[Held],
    networks: Vec<NetworkHealth>,
    snapshot: &Snapshot,
    fiat: Fiat,
) -> Portfolio {
    let holdings: Vec<Holding> = held
        .iter()
        .filter_map(|holding| {
            let coin = coin(registry, custom, &holding.asset)?;
            let quote = quote(coin, snapshot, fiat);
            let price = quote.map(|quote| quote.price);
            let owned = match holding.staked {
                Some(staked) => holding.amount.checked_add(staked).ok(),
                None => Some(holding.amount),
            };
            let value = price
                .zip(owned)
                .and_then(|(price, owned)| price.value_of(owned, fiat.places()).ok());
            Some(Holding {
                wallet: holding.wallet,
                address: holding.address,
                asset: holding.asset.clone(),
                symbol: coin.symbol.clone(),
                name: coin.name.clone(),
                amount: holding.amount,
                staked: holding.staked,
                price,
                price_at: quote.map(|quote| quote.updated_at),
                value,
            })
        })
        .collect();
    let total = holdings
        .iter()
        .filter_map(|holding| holding.value)
        .try_fold(None, |sum: Option<Amount>, value| match sum {
            None => Some(Some(value)),
            Some(sum) => sum.checked_add(value).ok().map(Some),
        })
        .flatten();
    Portfolio {
        fiat,
        holdings,
        total,
        networks,
        prices: snapshot.status,
    }
}

#[cfg(test)]
mod tests {
    use bahaa_market::{Quote, Quotes};

    use super::*;

    fn registry() -> Registry {
        Registry::builtin().unwrap()
    }

    fn held(asset: &str, amount: &str, decimals: u8) -> Held {
        Held {
            wallet: crate::keyring::test_wallet(1),
            address: Address::Evm([1; 20]),
            asset: asset.parse().unwrap(),
            amount: Amount::parse(amount, decimals).unwrap(),
            staked: None,
        }
    }

    fn snapshot(base: &str, prices: &[(&str, &str)]) -> Snapshot {
        let quotes: Quotes = prices
            .iter()
            .map(|(id, price)| {
                (
                    id.parse().unwrap(),
                    Quote {
                        price: Price::parse(price).unwrap(),
                        updated_at: 1,
                    },
                )
            })
            .collect();
        Snapshot {
            base: base.to_owned(),
            quotes,
            status: Status::Live,
        }
    }

    const USDT: &str = "bsc/0x55d398326f99059fF775485246999027B3197955";

    #[test]
    fn staked_coins_are_shown_apart_but_count_toward_the_value() {
        let mut trx = held("tron", "10", 6);
        trx.staked = Some(Amount::parse("90", 6).unwrap());
        let mut locked = held("tron", "0", 6);
        locked.staked = Some(Amount::parse("5", 6).unwrap());
        let prices = snapshot("USDT", &[("crypto/TRX", "0.25")]);
        let portfolio = build(
            &registry(),
            &[],
            &[trx.clone(), locked.clone()],
            Vec::new(),
            &prices,
            Fiat::Usd,
        );
        let shown: Vec<(String, Option<String>, Option<String>)> = portfolio
            .holdings
            .iter()
            .map(|h| {
                (
                    h.amount.to_string(),
                    h.staked.map(|staked| staked.to_string()),
                    h.value.map(|value| value.fixed()),
                )
            })
            .collect();
        assert_eq!(
            shown,
            [
                (
                    "10".to_owned(),
                    Some("90".to_owned()),
                    Some("25.00".to_owned())
                ),
                (
                    "0".to_owned(),
                    Some("5".to_owned()),
                    Some("1.25".to_owned())
                ),
            ]
        );
        assert_eq!(portfolio.total.unwrap().fixed(), "26.25");
        let asked = wanted(&registry(), &[], &[locked], &[]);
        assert_eq!(asked.len(), 1);
    }

    #[test]
    fn values_holdings_exactly_and_totals_them() {
        let held = [
            held("bsc", "1.5", 18),
            held(USDT, "100", 18),
            held("ethereum", "0", 18),
        ];
        let prices = snapshot(
            "USDT",
            &[
                ("crypto/BNB", "714.3"),
                ("crypto/USDT", "0.99873"),
                ("crypto/ETH", "2408.54"),
            ],
        );
        let portfolio = build(&registry(), &[], &held, Vec::new(), &prices, Fiat::Usd);
        let values: Vec<Option<String>> = portfolio
            .holdings
            .iter()
            .map(|h| h.value.map(|v| v.fixed()))
            .collect();
        assert_eq!(
            values,
            [
                Some("1071.45".to_owned()),
                Some("99.87".to_owned()),
                Some("0.00".to_owned())
            ]
        );
        assert_eq!(portfolio.total.unwrap().fixed(), "1171.32");
        assert_eq!(portfolio.holdings[1].symbol, "USDT");
        assert_eq!(portfolio.prices, Status::Live);
    }

    #[test]
    fn prices_in_another_base_are_never_mixed_in() {
        let held = [held("bsc", "1", 18)];
        let prices = snapshot("USDT", &[("crypto/BNB", "714.3")]);
        let portfolio = build(&registry(), &[], &held, Vec::new(), &prices, Fiat::Irt);
        assert_eq!(portfolio.holdings[0].value, None);
        assert_eq!(portfolio.total, None);
    }

    #[test]
    fn unknown_assets_are_left_out_and_custom_tokens_are_named() {
        let custom = [CustomToken {
            asset: "bsc/0x0E09FaBB73Bd3Ade0a17ECC321fD13a19e81cE82"
                .parse()
                .unwrap(),
            coin: Coin {
                symbol: "CAKE".to_owned(),
                name: "PancakeSwap".to_owned(),
                decimals: 18,
                price: Some("crypto/CAKE".parse().unwrap()),
            },
        }];
        let held = [
            held("bsc/0x0E09FaBB73Bd3Ade0a17ECC321fD13a19e81cE82", "3", 18),
            held("bsc/0x1111111111111111111111111111111111111111", "9", 18),
        ];
        let portfolio = build(
            &registry(),
            &custom,
            &held,
            Vec::new(),
            &Snapshot::default(),
            Fiat::Usd,
        );
        assert_eq!(portfolio.holdings.len(), 1);
        assert_eq!(portfolio.holdings[0].symbol, "CAKE");
        assert_eq!(portfolio.holdings[0].price, None);
    }

    #[test]
    fn held_and_received_coins_are_priced() {
        let held = [
            held("bsc", "1", 18),
            held(USDT, "0", 18),
            held("ethereum", "0.1", 18),
        ];
        let priced = |received: &[AssetId]| -> Vec<String> {
            wanted(&registry(), &[], &held, received)
                .iter()
                .map(ToString::to_string)
                .collect()
        };
        assert_eq!(priced(&[]), ["crypto/BNB", "crypto/ETH"]);
        let dust: AssetId = "tron".parse().unwrap();
        assert_eq!(priced(&[dust]), ["crypto/BNB", "crypto/ETH", "crypto/TRX"]);
    }
}
