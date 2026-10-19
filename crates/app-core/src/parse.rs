use bahaa_engine::{Address, Amount, ChainFamily};

use crate::text;

pub(crate) fn amount(text: &str, decimals: u8) -> Result<Amount, String> {
    let trimmed = text.trim();
    if trimmed.contains(',') {
        return Err(text::AMOUNT_COMMA.to_owned());
    }
    let normalized = if trimmed.starts_with('.') {
        format!("0{trimmed}")
    } else {
        trimmed.to_owned()
    };
    let amount =
        Amount::parse(&normalized, decimals).map_err(|err| text::sentence(&err.to_string()))?;
    if amount.is_zero() {
        return Err(text::AMOUNT_ZERO.to_owned());
    }
    Ok(amount)
}

pub(crate) fn address(text: &str, family: ChainFamily) -> Result<Address, String> {
    Address::parse(family, text.trim()).map_err(|err| text::sentence(&err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts_are_read_the_way_people_type_them() {
        assert_eq!(amount(" 1.5 ", 18).unwrap().to_string(), "1.5");
        assert_eq!(amount(".25", 6).unwrap().to_string(), "0.25");
        assert_eq!(amount("1,5", 6).unwrap_err(), text::AMOUNT_COMMA);
        assert_eq!(amount("0", 6).unwrap_err(), text::AMOUNT_ZERO);
        assert_eq!(
            amount("0.0000001", 6).unwrap_err(),
            "At most 6 digits after the dot"
        );
        assert_eq!(amount("", 6).unwrap_err(), "Enter an amount");
    }

    #[test]
    fn addresses_are_trimmed_and_checked_for_the_family() {
        let text = " 0xA749C5a714991dc2EF2cF36a238514adA4cbBf69\n";
        assert!(address(text, ChainFamily::Evm).is_ok());
        assert_eq!(
            address(text, ChainFamily::Tron).unwrap_err(),
            "This is not a valid tron address"
        );
    }
}
