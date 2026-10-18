use bahaa_chain::{ChainError, FeeItem};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Prices {
    pub per_byte: u64,
    pub per_energy: u64,
    pub create_account: u64,
    pub activation: u64,
    pub max_fee_limit: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Resources {
    pub free_bandwidth: u64,
    pub staked_bandwidth: u64,
    pub energy: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Quote {
    pub expected: u64,
    pub max: u64,
    pub lines: Vec<(FeeItem, u64)>,
}

impl Quote {
    fn add(&mut self, item: FeeItem, expected: u64, max: u64) -> Result<(), ChainError> {
        self.expected = self.expected.checked_add(expected).ok_or_else(overflow)?;
        self.max = self.max.checked_add(max).ok_or_else(overflow)?;
        self.lines.push((item, expected));
        Ok(())
    }
}

pub(crate) fn fee_limit(energy: u64, prices: &Prices) -> Result<i64, ChainError> {
    if prices.per_energy == 0 {
        return Err(ChainError::Network(
            "an endpoint reports no energy price".to_owned(),
        ));
    }
    let limit = energy
        .checked_add(energy / 5)
        .and_then(|padded| padded.checked_mul(prices.per_energy))
        .ok_or_else(overflow)?;
    if limit > prices.max_fee_limit {
        return Err(ChainError::Invalid(
            "this transfer needs more energy than Tron allows",
        ));
    }
    i64::try_from(limit).map_err(|_| overflow())
}

pub(crate) fn quote(
    bytes: u64,
    energy: Option<(u64, i64)>,
    new_account: bool,
    resources: &Resources,
    prices: &Prices,
) -> Result<Quote, ChainError> {
    let mut quote = Quote::default();
    let staked = resources.staked_bandwidth >= bytes;
    let bandwidth = if new_account {
        if staked { 0 } else { prices.create_account }
    } else if staked || resources.free_bandwidth >= bytes {
        0
    } else {
        bytes.checked_mul(prices.per_byte).ok_or_else(overflow)?
    };
    quote.add(FeeItem::Bandwidth { bytes }, bandwidth, bandwidth)?;
    if new_account {
        quote.add(FeeItem::Activation, prices.activation, prices.activation)?;
    }
    if let Some((energy, fee_limit)) = energy {
        let cap = u64::try_from(fee_limit).map_err(|_| overflow())? / prices.per_energy.max(1);
        let burn = |units: u64| {
            units
                .saturating_sub(resources.energy)
                .checked_mul(prices.per_energy)
                .ok_or_else(overflow)
        };
        quote.add(FeeItem::Energy { units: energy }, burn(energy)?, burn(cap)?)?;
    }
    Ok(quote)
}

fn overflow() -> ChainError {
    ChainError::Invalid("fee overflow")
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRICES: Prices = Prices {
        per_byte: 1_000,
        per_energy: 100,
        create_account: 100_000,
        activation: 1_000_000,
        max_fee_limit: 15_000_000_000,
    };

    fn resources(free: u64, staked: u64, energy: u64) -> Resources {
        Resources {
            free_bandwidth: free,
            staked_bandwidth: staked,
            energy,
        }
    }

    #[test]
    fn bandwidth_is_free_while_the_daily_or_staked_allowance_covers_it() {
        let cases = [
            (resources(600, 0, 0), 0),
            (resources(0, 300, 0), 0),
            (resources(264, 264, 0), 265_000),
            (resources(0, 0, 0), 265_000),
        ];
        for (held, burn) in cases {
            let quote = quote(265, None, false, &held, &PRICES).unwrap();
            assert_eq!(
                quote.lines,
                [(FeeItem::Bandwidth { bytes: 265 }, burn)],
                "{held:?}"
            );
            assert_eq!((quote.expected, quote.max), (burn, burn));
        }
    }

    #[test]
    fn activating_a_new_account_costs_one_trx_and_cannot_use_free_bandwidth() {
        let fresh = quote(265, None, true, &resources(600, 0, 0), &PRICES).unwrap();
        assert_eq!(
            fresh.lines,
            [
                (FeeItem::Bandwidth { bytes: 265 }, 100_000),
                (FeeItem::Activation, 1_000_000)
            ]
        );
        assert_eq!(fresh.expected, 1_100_000);
        let staked = quote(265, None, true, &resources(0, 265, 0), &PRICES).unwrap();
        assert_eq!(staked.expected, 1_000_000);
    }

    #[test]
    fn energy_burns_what_staking_does_not_cover_up_to_the_fee_limit() {
        let limit = fee_limit(130_285, &PRICES).unwrap();
        assert_eq!(limit, 156_342 * 100);
        let burnt = quote(
            345,
            Some((130_285, limit)),
            false,
            &resources(600, 0, 0),
            &PRICES,
        )
        .unwrap();
        assert_eq!(
            burnt.lines,
            [
                (FeeItem::Bandwidth { bytes: 345 }, 0),
                (FeeItem::Energy { units: 130_285 }, 13_028_500)
            ]
        );
        assert_eq!((burnt.expected, burnt.max), (13_028_500, 15_634_200));
        let staked = quote(
            345,
            Some((130_285, limit)),
            false,
            &resources(600, 0, 140_000),
            &PRICES,
        )
        .unwrap();
        assert_eq!((staked.expected, staked.max), (0, 1_634_200));
        let rich = quote(
            345,
            Some((130_285, limit)),
            false,
            &resources(600, 0, 1_000_000),
            &PRICES,
        )
        .unwrap();
        assert_eq!((rich.expected, rich.max), (0, 0));
    }

    #[test]
    fn fee_limits_stay_within_what_the_network_allows() {
        assert!(matches!(
            fee_limit(200_000_000, &PRICES),
            Err(ChainError::Invalid(_))
        ));
        let free = Prices {
            per_energy: 0,
            ..PRICES
        };
        assert!(matches!(fee_limit(1, &free), Err(ChainError::Network(_))));
    }
}
