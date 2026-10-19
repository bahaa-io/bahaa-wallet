use std::fmt;

use ruint::UintTryFrom;
use ruint::aliases::{U256, U512};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{Amount, MAX_DECIMALS};

pub const MAX_SCALE: u32 = 36;
const MAX_EXPONENT_DIGITS: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Price {
    digits: U256,
    scale: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum PriceError {
    #[error("no price")]
    Empty,
    #[error("not a price")]
    Invalid,
    #[error("price has more than {MAX_SCALE} decimal places")]
    TooPrecise,
    #[error("price is too large")]
    Overflow,
}

impl Price {
    pub fn is_zero(&self) -> bool {
        self.digits.is_zero()
    }

    pub fn parse(text: &str) -> Result<Self, PriceError> {
        if text.is_empty() {
            return Err(PriceError::Empty);
        }
        let (number, exponent) = match text.split_once(['e', 'E']) {
            Some((number, exponent)) => (number, exponent_value(exponent)?),
            None => (text, 0),
        };
        let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
        if (whole.is_empty() && fraction.is_empty())
            || !only_digits(whole)
            || !only_digits(fraction)
        {
            return Err(PriceError::Invalid);
        }
        let fraction = fraction.trim_end_matches('0');
        let mut digits = digits(whole)?;
        for c in fraction.chars() {
            digits = push_digit(digits, c)?;
        }
        let places = i64::try_from(fraction.len()).map_err(|_| PriceError::TooPrecise)?;
        let shift = exponent - places;
        let (digits, scale) = if shift >= 0 {
            let shift = u32::try_from(shift).map_err(|_| PriceError::Overflow)?;
            let factor = U256::uint_try_from(power(shift)?).map_err(|_| PriceError::Overflow)?;
            (digits.checked_mul(factor).ok_or(PriceError::Overflow)?, 0)
        } else {
            (
                digits,
                u32::try_from(-shift).map_err(|_| PriceError::TooPrecise)?,
            )
        };
        Self::normalized(digits, scale)
    }

    pub fn value_of(&self, amount: Amount, places: u8) -> Result<Amount, PriceError> {
        if places > MAX_DECIMALS {
            return Err(PriceError::TooPrecise);
        }
        let product = U512::from(amount.units())
            .checked_mul(U512::from(self.digits))
            .and_then(|product| product.checked_mul(power(u32::from(places)).ok()?))
            .ok_or(PriceError::Overflow)?;
        let divisor = power(u32::from(amount.decimals()) + self.scale)?;
        let (quotient, remainder) = product.div_rem(divisor);
        let rounded = if remainder >= divisor - remainder {
            quotient.checked_add(U512::from(1u8))
        } else {
            Some(quotient)
        }
        .ok_or(PriceError::Overflow)?;
        let units = U256::uint_try_from(rounded).map_err(|_| PriceError::Overflow)?;
        Amount::new(units, places).map_err(|_| PriceError::TooPrecise)
    }

    fn normalized(mut digits: U256, mut scale: u32) -> Result<Self, PriceError> {
        let ten = U256::from(10u8);
        if digits.is_zero() {
            scale = 0;
        }
        while scale > 0 && (digits % ten).is_zero() {
            digits /= ten;
            scale -= 1;
        }
        if scale > MAX_SCALE {
            return Err(PriceError::TooPrecise);
        }
        Ok(Self { digits, scale })
    }
}

impl TryFrom<String> for Price {
    type Error = PriceError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::parse(&text)
    }
}

impl From<Price> for String {
    fn from(price: Price) -> Self {
        price.to_string()
    }
}

impl fmt::Display for Price {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = self.digits.to_string();
        let scale = usize::try_from(self.scale).map_err(|_| fmt::Error)?;
        if scale == 0 {
            return f.write_str(&text);
        }
        let padded = format!("{text:0>width$}", width = scale + 1);
        let (whole, fraction) = padded.split_at(padded.len() - scale);
        write!(f, "{whole}.{fraction}")
    }
}

fn exponent_value(text: &str) -> Result<i64, PriceError> {
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    if digits.is_empty() || digits.len() > MAX_EXPONENT_DIGITS || !only_digits(digits) {
        return Err(PriceError::Invalid);
    }
    let value: i64 = digits.parse().map_err(|_| PriceError::Invalid)?;
    Ok(if negative { -value } else { value })
}

fn only_digits(text: &str) -> bool {
    text.bytes().all(|byte| byte.is_ascii_digit())
}

fn digits(text: &str) -> Result<U256, PriceError> {
    text.chars().try_fold(U256::ZERO, push_digit)
}

fn push_digit(value: U256, c: char) -> Result<U256, PriceError> {
    let digit = c.to_digit(10).ok_or(PriceError::Invalid)?;
    value
        .checked_mul(U256::from(10u8))
        .and_then(|value| value.checked_add(U256::from(digit)))
        .ok_or(PriceError::Overflow)
}

fn power(exponent: u32) -> Result<U512, PriceError> {
    U512::from(10u8)
        .checked_pow(U512::from(exponent))
        .ok_or(PriceError::Overflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn price(text: &str) -> Price {
        Price::parse(text).unwrap()
    }

    fn amount(text: &str, decimals: u8) -> Amount {
        Amount::parse(text, decimals).unwrap()
    }

    #[test]
    fn prices_are_stored_as_their_exact_decimal_text() {
        let tiny = price("0.000000000009222");
        let json = serde_json::to_string(&tiny).unwrap();
        assert_eq!(json, "\"0.000000000009222\"");
        assert_eq!(serde_json::from_str::<Price>(&json).unwrap(), tiny);
        assert!(serde_json::from_str::<Price>("\"-1\"").is_err());
        assert!(serde_json::from_str::<Price>("1.5").is_err());
    }

    #[test]
    fn parses_plain_and_exponent_forms_exactly() {
        assert_eq!(price("714.3").to_string(), "714.3");
        assert_eq!(price("0.092039").to_string(), "0.092039");
        assert_eq!(price("1.2e-7").to_string(), "0.00000012");
        assert_eq!(price("1.2E-07").to_string(), "0.00000012");
        assert_eq!(price("3e5").to_string(), "300000");
        assert_eq!(price("1e+3").to_string(), "1000");
        assert_eq!(price("2.50").to_string(), "2.5");
        assert_eq!(price("0.000").to_string(), "0");
        assert_eq!(price("007").to_string(), "7");
        assert_eq!(price(&format!("1.{}", "0".repeat(200))).to_string(), "1");
        assert!(price("0").is_zero());
    }

    #[test]
    fn equal_values_are_equal_whatever_the_spelling() {
        assert_eq!(price("1.5"), price("15e-1"));
        assert_eq!(price("1500"), price("1.5e3"));
        assert_eq!(price("0.10"), price("0.1"));
    }

    #[test]
    fn rejects_what_is_not_a_price() {
        for text in [
            "-1", "e5", "1e", "1e+", "1e-", "abc", "1.2.3", "NaN", "Infinity", " 1", "1 ", "0x10",
            "1e1000", ".",
        ] {
            assert_eq!(Price::parse(text), Err(PriceError::Invalid), "{text:?}");
        }
        assert_eq!(Price::parse(""), Err(PriceError::Empty));
        assert_eq!(Price::parse("1e999"), Err(PriceError::Overflow));
        assert_eq!(Price::parse("1e-999"), Err(PriceError::TooPrecise));
        assert_eq!(
            Price::parse(&format!("0.{}1", "0".repeat(36))),
            Err(PriceError::TooPrecise)
        );
    }

    #[test]
    fn values_amounts_exactly_and_rounds_half_up_once() {
        let bnb = price("714.3");
        assert_eq!(
            bnb.value_of(amount("1.5", 18), 2).unwrap().fixed(),
            "1071.45"
        );
        assert_eq!(
            bnb.value_of(amount("0.000000000000000001", 18), 2)
                .unwrap()
                .fixed(),
            "0.00"
        );
        assert_eq!(
            price("0.005").value_of(amount("1", 0), 2).unwrap().fixed(),
            "0.01"
        );
        assert_eq!(
            price("0.0049").value_of(amount("1", 0), 2).unwrap().fixed(),
            "0.00"
        );
        assert_eq!(
            price("1.3465")
                .value_of(amount("2.5", 9), 2)
                .unwrap()
                .fixed(),
            "3.37"
        );
        assert_eq!(
            price("1.2e-7")
                .value_of(amount("1000000", 6), 6)
                .unwrap()
                .fixed(),
            "0.120000"
        );
        assert_eq!(
            price("115000")
                .value_of(amount("0.01", 9), 0)
                .unwrap()
                .fixed(),
            "1150"
        );
    }

    #[test]
    fn value_overflow_is_an_error_not_a_wrong_number() {
        let max = Amount::new(U256::MAX, 0).unwrap();
        assert_eq!(price("1000").value_of(max, 2), Err(PriceError::Overflow));
        assert_eq!(
            price("1").value_of(amount("1", 0), 78),
            Err(PriceError::TooPrecise)
        );
    }
}
