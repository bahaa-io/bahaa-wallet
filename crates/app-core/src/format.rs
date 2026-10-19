use bahaa_engine::{Address, Amount, Fiat, Network, Price, TonForm};

use crate::text;

const LIST_DECIMALS: usize = 6;
const PRICE_DECIMALS: usize = 2;
const PRICE_DIGITS: usize = 4;
const SHORT_ENDS: usize = 6;
const MINUTE: u64 = 60;
const HOUR: u64 = 60 * MINUTE;
const DAY: u64 = 24 * HOUR;

pub(crate) fn amount(amount: Amount, symbol: &str) -> String {
    let text = amount.to_string();
    let (whole, fraction) = split(&text);
    let kept = fraction[..fraction.len().min(LIST_DECIMALS)].trim_end_matches('0');
    if whole == "0" && kept.is_empty() && !amount.is_zero() {
        return format!("< 0.{}1 {symbol}", "0".repeat(LIST_DECIMALS - 1));
    }
    with_symbol(&number(whole, kept), symbol)
}

pub(crate) fn exact(amount: Amount, symbol: &str) -> String {
    let text = amount.to_string();
    let (whole, fraction) = split(&text);
    with_symbol(&number(whole, fraction), symbol)
}

pub(crate) fn fiat(value: Option<Amount>, fiat: Fiat) -> String {
    let Some(value) = value else {
        return text::NO_VALUE.to_owned();
    };
    let text = value.fixed();
    let (whole, fraction) = split(&text);
    currency(&number(whole, fraction), fiat)
}

pub(crate) fn price(price: Option<Price>, fiat: Fiat) -> String {
    let Some(price) = price else {
        return text::NO_VALUE.to_owned();
    };
    let text = price.to_string();
    let (whole, fraction) = split(&text);
    let kept = if whole == "0" {
        let zeros = fraction.len() - fraction.trim_start_matches('0').len();
        &fraction[..fraction.len().min(zeros + PRICE_DIGITS)]
    } else {
        &fraction[..fraction.len().min(PRICE_DECIMALS)]
    };
    currency(&number(whole, kept), fiat)
}

pub(crate) fn address(address: &Address, network: &Network) -> String {
    address
        .ton_friendly(TonForm {
            bounceable: false,
            testnet: network.testnet,
        })
        .unwrap_or_else(|| address.to_string())
}

pub(crate) fn short(text: &str) -> String {
    let count = text.chars().count();
    if count <= SHORT_ENDS * 2 + 1 {
        return text.to_owned();
    }
    let start: String = text.chars().take(SHORT_ENDS).collect();
    let end: String = text.chars().skip(count - SHORT_ENDS).collect();
    format!("{start}…{end}")
}

pub(crate) fn ago(time: u64, now: u64) -> String {
    let passed = now.saturating_sub(time);
    match passed {
        0..MINUTE => text::JUST_NOW.to_owned(),
        MINUTE..HOUR => text::minutes_ago(passed / MINUTE),
        HOUR..DAY => text::hours_ago(passed / HOUR),
        _ => when(time),
    }
}

pub(crate) fn when(time: u64) -> String {
    let days = time / DAY;
    let seconds = time % DAY;
    let (year, month, day) = civil(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}",
        seconds / HOUR,
        seconds % HOUR / MINUTE
    )
}

fn civil(days: u64) -> (u64, u64, u64) {
    let shifted = days + 719_468;
    let era = shifted / 146_097;
    let day_of_era = shifted % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    (year, month, day)
}

fn split(text: &str) -> (&str, &str) {
    text.split_once('.').unwrap_or((text, ""))
}

fn number(whole: &str, fraction: &str) -> String {
    let grouped = group(whole);
    if fraction.is_empty() {
        grouped
    } else {
        format!("{grouped}.{fraction}")
    }
}

fn group(digits: &str) -> String {
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

fn with_symbol(number: &str, symbol: &str) -> String {
    format!("{number} {symbol}")
}

fn currency(number: &str, fiat: Fiat) -> String {
    match fiat {
        Fiat::Usd => format!("${number}"),
        Fiat::Eur => format!("€{number}"),
        Fiat::Irt => format!("{number} IRT"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn amount_of(text: &str, decimals: u8) -> Amount {
        Amount::parse(text, decimals).unwrap()
    }

    #[test]
    fn lists_round_down_to_six_decimals_and_group_thousands() {
        assert_eq!(
            amount(amount_of("1234.5678919", 18), "BNB"),
            "1,234.567891 BNB"
        );
        assert_eq!(amount(amount_of("0.0000001", 18), "BNB"), "< 0.000001 BNB");
        assert_eq!(amount(amount_of("0", 6), "USDT"), "0 USDT");
        assert_eq!(amount(amount_of("1000000", 6), "USDT"), "1,000,000 USDT");
        assert_eq!(amount(amount_of("2.500000", 6), "USDT"), "2.5 USDT");
        assert_eq!(
            exact(amount_of("1234.5678919", 18), "BNB"),
            "1,234.5678919 BNB"
        );
    }

    #[test]
    fn fiat_and_prices_follow_the_currency() {
        assert_eq!(fiat(Some(amount_of("1234.5", 2)), Fiat::Usd), "$1,234.50");
        assert_eq!(fiat(Some(amount_of("0.07", 2)), Fiat::Eur), "€0.07");
        assert_eq!(
            fiat(Some(amount_of("1234567", 0)), Fiat::Irt),
            "1,234,567 IRT"
        );
        assert_eq!(fiat(None, Fiat::Usd), text::NO_VALUE);
        let parsed = |text| Price::parse(text).unwrap();
        assert_eq!(price(Some(parsed("67000.129")), Fiat::Usd), "$67,000.12");
        assert_eq!(
            price(Some(parsed("0.0000123456")), Fiat::Usd),
            "$0.00001234"
        );
        assert_eq!(price(Some(parsed("0.999")), Fiat::Usd), "$0.999");
        assert_eq!(price(Some(parsed("3")), Fiat::Irt), "3 IRT");
    }

    #[test]
    fn addresses_are_shortened_past_the_look_alike_window() {
        assert_eq!(
            short("0xA749C5a714991dc2EF2cF36a238514adA4cbBf69"),
            "0xA749…cbBf69"
        );
        assert_eq!(short("short"), "short");
    }

    #[test]
    fn times_read_as_recent_or_as_a_utc_date() {
        let now = 1_792_402_641;
        assert_eq!(ago(now - 10, now), text::JUST_NOW);
        assert_eq!(ago(now - 125, now), "2 min ago");
        assert_eq!(ago(now - 7_300, now), "2 h ago");
        assert_eq!(when(0), "1970-01-01 00:00");
        assert_eq!(when(951_782_400), "2000-02-29 00:00");
        assert_eq!(ago(now - 2 * DAY, now), when(now - 2 * DAY));
    }
}
