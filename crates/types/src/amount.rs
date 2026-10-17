use std::cmp::Ordering;
use std::fmt;

use ruint::aliases::U256;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const MAX_DECIMALS: u8 = 77;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(into = "Stored", try_from = "Stored")]
pub struct Amount {
    units: U256,
    decimals: u8,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    value: String,
    decimals: u8,
}

impl From<Amount> for Stored {
    fn from(amount: Amount) -> Self {
        Self {
            value: amount.to_string(),
            decimals: amount.decimals,
        }
    }
}

impl TryFrom<Stored> for Amount {
    type Error = AmountError;

    fn try_from(stored: Stored) -> Result<Self, Self::Error> {
        Self::parse(&stored.value, stored.decimals)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum AmountError {
    #[error("enter an amount")]
    Empty,
    #[error("use digits and at most one dot")]
    Invalid,
    #[error("at most {0} digits after the dot")]
    TooPrecise(u8),
    #[error("amount is too large")]
    Overflow,
    #[error("amount would go below zero")]
    Underflow,
    #[error("{0} decimals is more than the supported {MAX_DECIMALS}")]
    DecimalsTooLarge(u8),
    #[error("amounts with {0} and {1} decimals cannot be combined")]
    DecimalsMismatch(u8, u8),
}

impl Amount {
    pub fn new(units: U256, decimals: u8) -> Result<Self, AmountError> {
        if decimals > MAX_DECIMALS {
            return Err(AmountError::DecimalsTooLarge(decimals));
        }
        Ok(Self { units, decimals })
    }

    pub fn zero(decimals: u8) -> Result<Self, AmountError> {
        Self::new(U256::ZERO, decimals)
    }

    pub fn parse(text: &str, decimals: u8) -> Result<Self, AmountError> {
        if decimals > MAX_DECIMALS {
            return Err(AmountError::DecimalsTooLarge(decimals));
        }
        if text.is_empty() {
            return Err(AmountError::Empty);
        }
        let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
        if (whole.is_empty() && fraction.is_empty())
            || !only_digits(whole)
            || !only_digits(fraction)
        {
            return Err(AmountError::Invalid);
        }
        let fraction = fraction.trim_end_matches('0');
        let places = u8::try_from(fraction.len())
            .ok()
            .filter(|places| *places <= decimals)
            .ok_or(AmountError::TooPrecise(decimals))?;
        let fraction = digits(fraction)?
            .checked_mul(power(decimals - places))
            .ok_or(AmountError::Overflow)?;
        let units = digits(whole)?
            .checked_mul(power(decimals))
            .and_then(|whole| whole.checked_add(fraction))
            .ok_or(AmountError::Overflow)?;
        Ok(Self { units, decimals })
    }

    pub fn units(&self) -> U256 {
        self.units
    }

    pub fn decimals(&self) -> u8 {
        self.decimals
    }

    pub fn is_zero(&self) -> bool {
        self.units.is_zero()
    }

    pub fn fixed(&self) -> String {
        let (whole, fraction) = self.parts();
        if fraction.is_empty() {
            whole
        } else {
            format!("{whole}.{fraction}")
        }
    }

    pub fn checked_add(self, other: Self) -> Result<Self, AmountError> {
        let decimals = self.same_decimals(other)?;
        let units = self
            .units
            .checked_add(other.units)
            .ok_or(AmountError::Overflow)?;
        Ok(Self { units, decimals })
    }

    pub fn checked_sub(self, other: Self) -> Result<Self, AmountError> {
        let decimals = self.same_decimals(other)?;
        let units = self
            .units
            .checked_sub(other.units)
            .ok_or(AmountError::Underflow)?;
        Ok(Self { units, decimals })
    }

    fn parts(&self) -> (String, String) {
        let scale = power(self.decimals);
        let whole = (self.units / scale).to_string();
        let width = usize::from(self.decimals);
        if width == 0 {
            return (whole, String::new());
        }
        let fraction = format!("{:0>width$}", (self.units % scale).to_string());
        (whole, fraction)
    }

    fn same_decimals(self, other: Self) -> Result<u8, AmountError> {
        if self.decimals == other.decimals {
            Ok(self.decimals)
        } else {
            Err(AmountError::DecimalsMismatch(self.decimals, other.decimals))
        }
    }
}

impl PartialOrd for Amount {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        (self.decimals == other.decimals).then(|| self.units.cmp(&other.units))
    }
}

impl fmt::Display for Amount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (whole, fraction) = self.parts();
        let fraction = fraction.trim_end_matches('0');
        if fraction.is_empty() {
            f.write_str(&whole)
        } else {
            write!(f, "{whole}.{fraction}")
        }
    }
}

fn only_digits(text: &str) -> bool {
    text.bytes().all(|byte| byte.is_ascii_digit())
}

fn digits(text: &str) -> Result<U256, AmountError> {
    text.chars().try_fold(U256::ZERO, |value, c| {
        let digit = c.to_digit(10).ok_or(AmountError::Invalid)?;
        value
            .checked_mul(U256::from(10u8))
            .and_then(|value| value.checked_add(U256::from(digit)))
            .ok_or(AmountError::Overflow)
    })
}

fn power(exponent: u8) -> U256 {
    U256::from(10u8).pow(U256::from(exponent))
}

#[cfg(test)]
mod tests {
    use super::*;

    const U256_MAX: &str =
        "115792089237316195423570985008687907853269984665640564039457584007913129639935";

    fn amount(text: &str, decimals: u8) -> Amount {
        Amount::parse(text, decimals).unwrap()
    }

    #[test]
    fn stores_as_exact_readable_json() {
        for (text, decimals) in [
            ("1.5", 18),
            ("0", 6),
            ("123", 0),
            (U256_MAX, 0),
            ("0.000000000000000001", 18),
        ] {
            let original = amount(text, decimals);
            let json = serde_json::to_string(&original).unwrap();
            assert_eq!(
                json,
                format!("{{\"value\":\"{text}\",\"decimals\":{decimals}}}")
            );
            assert_eq!(serde_json::from_str::<Amount>(&json).unwrap(), original);
        }
        for bad in [
            r#"{"value":"1.5","decimals":0}"#,
            r#"{"value":"-1","decimals":18}"#,
            r#"{"value":"1","decimals":78}"#,
            r#"{"value":"1","decimals":18,"extra":1}"#,
            r#""1.5""#,
        ] {
            assert!(serde_json::from_str::<Amount>(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn parses_into_exact_base_units() {
        assert_eq!(amount("1", 18).units(), U256::from(10u64.pow(18)));
        assert_eq!(amount("1.5", 6).units(), U256::from(1_500_000u64));
        assert_eq!(amount("0.000000000000000001", 18).units(), U256::from(1u8));
        assert_eq!(amount(".5", 6).units(), U256::from(500_000u64));
        assert_eq!(amount("5.", 6).units(), U256::from(5_000_000u64));
        assert_eq!(amount("007", 0).units(), U256::from(7u8));
        assert_eq!(
            amount("1.50000000000000000000000", 1).units(),
            U256::from(15u8)
        );
        assert_eq!(amount("0", 9).units(), U256::ZERO);
    }

    #[test]
    fn rejects_anything_that_is_not_a_plain_amount() {
        for text in [
            "-1", "+1", "1e5", "1,5", " 1", "1 ", "1.2.3", "0x10", "١", "۵", "1_000", ".",
        ] {
            assert_eq!(
                Amount::parse(text, 18),
                Err(AmountError::Invalid),
                "{text:?}"
            );
        }
        assert_eq!(Amount::parse("", 18), Err(AmountError::Empty));
    }

    #[test]
    fn refuses_more_precision_than_the_asset_has() {
        assert_eq!(Amount::parse("1.55", 1), Err(AmountError::TooPrecise(1)));
        assert_eq!(Amount::parse("0.1", 0), Err(AmountError::TooPrecise(0)));
        assert_eq!(
            Amount::parse("0.0000001", 6),
            Err(AmountError::TooPrecise(6))
        );
    }

    #[test]
    fn detects_overflow_at_the_edge_of_256_bits() {
        assert_eq!(amount(U256_MAX, 0).units(), U256::MAX);
        let too_big =
            "115792089237316195423570985008687907853269984665640564039457584007913129639936";
        assert_eq!(Amount::parse(too_big, 0), Err(AmountError::Overflow));
        assert_eq!(Amount::parse(U256_MAX, 1), Err(AmountError::Overflow));
    }

    #[test]
    fn caps_decimals() {
        assert!(Amount::zero(MAX_DECIMALS).is_ok());
        assert_eq!(Amount::zero(78), Err(AmountError::DecimalsTooLarge(78)));
        assert_eq!(
            Amount::parse("1", 78),
            Err(AmountError::DecimalsTooLarge(78))
        );
    }

    #[test]
    fn displays_exactly_without_trailing_zeros() {
        assert_eq!(amount("1.5", 18).to_string(), "1.5");
        assert_eq!(amount("1", 18).to_string(), "1");
        assert_eq!(amount("0", 18).to_string(), "0");
        assert_eq!(
            amount("0.000000000000000001", 18).to_string(),
            "0.000000000000000001"
        );
        assert_eq!(amount("123.000450", 6).to_string(), "123.00045");
        assert_eq!(amount("42", 0).to_string(), "42");
        let one = Amount::new(U256::from(1u8), MAX_DECIMALS).unwrap();
        assert_eq!(one.to_string(), format!("0.{}1", "0".repeat(76)));
    }

    #[test]
    fn fixed_keeps_every_decimal_place() {
        assert_eq!(amount("12.5", 2).fixed(), "12.50");
        assert_eq!(amount("12", 2).fixed(), "12.00");
        assert_eq!(amount("0", 2).fixed(), "0.00");
        assert_eq!(amount("7", 0).fixed(), "7");
    }

    #[test]
    fn display_and_parse_round_trip() {
        let samples = [
            ("0.1", 1),
            ("714.3", 18),
            ("0.092039", 18),
            ("1000000", 6),
            ("0.000001", 6),
            ("1.1", 9),
            (U256_MAX, 0),
        ];
        for (text, decimals) in samples {
            let parsed = amount(text, decimals);
            assert_eq!(amount(&parsed.to_string(), decimals), parsed, "{text}");
        }
    }

    #[test]
    fn arithmetic_is_checked() {
        let a = amount("1.5", 6);
        let b = amount("0.25", 6);
        assert_eq!(a.checked_add(b).unwrap().to_string(), "1.75");
        assert_eq!(a.checked_sub(b).unwrap().to_string(), "1.25");
        assert_eq!(b.checked_sub(a), Err(AmountError::Underflow));
        assert_eq!(
            a.checked_add(amount("1", 18)),
            Err(AmountError::DecimalsMismatch(6, 18))
        );
        let max = amount(U256_MAX, 0);
        assert_eq!(max.checked_add(amount("1", 0)), Err(AmountError::Overflow));
    }

    #[test]
    fn only_amounts_with_equal_decimals_compare() {
        assert!(amount("2", 6) > amount("1.999999", 6));
        assert_eq!(amount("1", 6).partial_cmp(&amount("1", 18)), None);
        assert_ne!(amount("1", 6), amount("1", 18));
    }
}
