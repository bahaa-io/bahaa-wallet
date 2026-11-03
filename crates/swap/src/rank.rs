use std::cmp::Ordering;

use bahaa_chain::Wait;
use bahaa_types::{Amount, AssetId, Price};
use ruint::aliases::U256;

use crate::route::{Charged, Route};

pub const WARN_BPS: u32 = 200;
pub const REFUSE_BPS: u32 = 1_000;
const WHOLE_BPS: u32 = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Caution {
    BelowMarket(u32),
    CostlyFees(u32),
    Unpriced,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    BelowMarket(u32),
    Inconsistent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Value {
    pub sold: Amount,
    pub received: Amount,
    pub extra: Amount,
    pub net: Amount,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ranked {
    pub route: Route,
    pub value: Option<Value>,
    pub cautions: Vec<Caution>,
    pub refused: Option<Refusal>,
}

pub fn rank(
    routes: Vec<Route>,
    price: impl Fn(&AssetId) -> Option<Price>,
    places: u8,
) -> Vec<Ranked> {
    let mut ranked: Vec<Ranked> = routes
        .into_iter()
        .map(|route| judge(route, &price, places))
        .collect();
    ranked.sort_by(order);
    ranked
}

fn judge(route: Route, price: &impl Fn(&AssetId) -> Option<Price>, places: u8) -> Ranked {
    let consistent = !route.expected.is_zero()
        && route
            .minimum
            .partial_cmp(&route.expected)
            .is_some_and(Ordering::is_le);
    if !consistent {
        return Ranked {
            route,
            value: None,
            cautions: Vec::new(),
            refused: Some(Refusal::Inconsistent),
        };
    }
    let value = valued(&route, price, places);
    let mut cautions = Vec::new();
    let mut refused = None;
    match &value {
        None => cautions.push(Caution::Unpriced),
        Some(value) => {
            let short = shortfall(value.sold, value.received);
            if short > REFUSE_BPS {
                refused = Some(Refusal::BelowMarket(short));
            } else if short > WARN_BPS {
                cautions.push(Caution::BelowMarket(short));
            }
            let fees = share(value.extra, value.sold);
            if fees > WARN_BPS {
                cautions.push(Caution::CostlyFees(fees));
            }
        }
    }
    Ranked {
        route,
        value,
        cautions,
        refused,
    }
}

fn valued(route: &Route, price: &impl Fn(&AssetId) -> Option<Price>, places: u8) -> Option<Value> {
    let value_of = |asset: &AssetId, amount: Amount| price(asset)?.value_of(amount, places).ok();
    let sold = value_of(&route.sell, route.amount)?;
    let received = value_of(&route.buy, route.expected)?;
    let mut extra = Amount::zero(places).ok()?;
    for cost in route
        .costs
        .iter()
        .filter(|cost| cost.charged == Charged::Extra)
    {
        extra = extra
            .checked_add(value_of(&cost.asset, cost.amount)?)
            .ok()?;
    }
    let net = match received.checked_sub(extra) {
        Ok(net) => net,
        Err(_) => Amount::zero(places).ok()?,
    };
    Some(Value {
        sold,
        received,
        extra,
        net,
    })
}

fn shortfall(sold: Amount, received: Amount) -> u32 {
    sold.checked_sub(received)
        .map_or(0, |lost| share(lost, sold))
}

fn share(part: Amount, whole: Amount) -> u32 {
    if whole.is_zero() {
        return 0;
    }
    let bps = part.units().saturating_mul(U256::from(WHOLE_BPS)) / whole.units();
    u32::try_from(bps).unwrap_or(u32::MAX)
}

fn order(a: &Ranked, b: &Ranked) -> Ordering {
    a.refused
        .is_some()
        .cmp(&b.refused.is_some())
        .then_with(|| match (&a.value, &b.value) {
            (Some(left), Some(right)) => right.net.units().cmp(&left.net.units()),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        })
        .then_with(|| a.route.custody().cmp(&b.route.custody()))
        .then_with(|| b.route.expected.units().cmp(&a.route.expected.units()))
        .then_with(|| waited(a.route.wait).cmp(&waited(b.route.wait)))
}

fn waited(wait: Option<Wait>) -> u64 {
    match wait {
        Some(Wait::About(seconds) | Wait::Over(seconds)) => seconds,
        Some(Wait::Slow | Wait::Stuck) | None => u64::MAX,
    }
}
