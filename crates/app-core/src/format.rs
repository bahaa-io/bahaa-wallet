use bahaa_engine::{Address, Amount, FeeRate, Fiat, Network, Price, TonForm, Utc, Wait};

use crate::text;

const LIST_DECIMALS: usize = 6;
const PRICE_DECIMALS: usize = 2;
const PRICE_DIGITS: usize = 4;
const SHORT_ENDS: usize = 6;
const GROUP: usize = 4;
const HEX_PREFIX: &str = "0x";
const MINUTE: u64 = 60;
const HOUR: u64 = 60 * MINUTE;
const DAY: u64 = 24 * HOUR;
const FEW_SECONDS: u64 = 10;
const SECONDS_STEP: u64 = 5;

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

pub(crate) fn grouped(text: &str) -> String {
    let (prefix, rest) = text
        .strip_prefix(HEX_PREFIX)
        .map_or(("", text), |rest| (HEX_PREFIX, rest));
    let chars: Vec<char> = rest.chars().collect();
    let groups: Vec<String> = chars
        .chunks(GROUP)
        .map(|group| group.iter().collect())
        .collect();
    format!("{prefix}{}", groups.join(" "))
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

pub(crate) fn span(seconds: u64) -> String {
    if seconds <= FEW_SECONDS {
        return text::FEW_SECONDS.to_owned();
    }
    match seconds {
        0..MINUTE => text::duration(seconds / SECONDS_STEP * SECONDS_STEP, text::SECOND),
        MINUTE..HOUR => text::duration(seconds / MINUTE, text::MINUTE),
        HOUR..DAY => text::duration(seconds / HOUR, text::HOUR),
        _ => text::duration(seconds / DAY, text::DAY),
    }
}

pub(crate) fn wait(wait: Wait) -> String {
    match wait {
        Wait::About(seconds) if seconds > FEW_SECONDS => text::about(&span(seconds)),
        Wait::About(_) => text::FEW_SECONDS.to_owned(),
        Wait::Over(seconds) => text::more_than(&span(seconds)),
        Wait::Slow => text::SLOW.to_owned(),
        Wait::Stuck => text::STUCK.to_owned(),
    }
}

pub(crate) fn percent(bps: u32) -> String {
    match bps {
        0 => "0%".to_owned(),
        1..=4 => "< 0.1%".to_owned(),
        5..1_000 => {
            let tenths = (bps + 5) / 10;
            format!("{}.{}%", tenths / 10, tenths % 10)
        }
        _ => format!("{}%", (bps + 50) / 100),
    }
}

pub(crate) fn short_span(seconds: u64) -> String {
    match seconds {
        0..MINUTE => format!("{seconds} s"),
        MINUTE..HOUR => format!("{} min", seconds / MINUTE),
        HOUR..DAY => format!("{} h", seconds / HOUR),
        _ => format!("{} d", seconds / DAY),
    }
}

pub(crate) fn short_wait(wait: Option<Wait>) -> String {
    match wait {
        Some(Wait::About(seconds)) => format!("~{}", short_span(seconds)),
        Some(Wait::Over(seconds)) => format!("> {}", short_span(seconds)),
        Some(Wait::Slow) => text::SLOW_SHORT.to_owned(),
        Some(Wait::Stuck) => text::STUCK_SHORT.to_owned(),
        None => text::NO_VALUE.to_owned(),
    }
}

pub(crate) fn rate(rate: FeeRate) -> String {
    amount(rate.amount, text::rate_unit(rate.unit))
}

pub(crate) fn when(time: u64) -> String {
    let at = Utc::at(time);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        at.year, at.month, at.day, at.hour, at.minute
    )
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
    fn percents_and_short_waits_stay_short() {
        assert_eq!(percent(0), "0%");
        assert_eq!(percent(3), "< 0.1%");
        assert_eq!(percent(37), "0.4%");
        assert_eq!(percent(322), "3.2%");
        assert_eq!(percent(1_574), "16%");
        assert_eq!(short_wait(Some(Wait::About(2))), "~2 s");
        assert_eq!(short_wait(Some(Wait::About(812))), "~13 min");
        assert_eq!(short_wait(Some(Wait::Over(302_400))), "> 3 d");
        assert_eq!(short_wait(None), text::NO_VALUE);
    }

    #[test]
    fn waits_read_as_rounded_spans() {
        assert_eq!(wait(Wait::About(1)), "a few seconds");
        assert_eq!(wait(Wait::About(10)), "a few seconds");
        assert_eq!(wait(Wait::About(18)), "about 15 seconds");
        assert_eq!(wait(Wait::About(60)), "about 1 minute");
        assert_eq!(wait(Wait::About(1_200)), "about 20 minutes");
        assert_eq!(wait(Wait::About(3_600)), "about 1 hour");
        assert_eq!(wait(Wait::About(14_400)), "about 4 hours");
        assert_eq!(wait(Wait::About(86_400)), "about 1 day");
        assert_eq!(wait(Wait::Over(302_400)), "more than 3 days");
        assert_eq!(wait(Wait::Stuck), text::STUCK);
        assert_eq!(wait(Wait::Slow), text::SLOW);
        assert_eq!(span(125), "2 minutes");
        let rate = FeeRate {
            amount: amount_of("2.5", 3),
            unit: bahaa_engine::RateUnit::SatPerVbyte,
        };
        assert_eq!(self::rate(rate), "2.5 sat/vB");
        let limit = FeeRate {
            amount: amount_of("3.123456789", 9),
            unit: bahaa_engine::RateUnit::Gwei,
        };
        assert_eq!(self::rate(limit), "3.123456 gwei");
    }

    #[test]
    fn addresses_are_shown_in_groups_of_four() {
        assert_eq!(
            grouped("0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"),
            "0xf39F d6e5 1aad 88F6 F4ce 6aB8 8272 79cf fFb9 2266"
        );
        assert_eq!(
            grouped("TRovmURrSa5937phqN8yUCpqqiGt7vuTyR"),
            "TRov mURr Sa59 37ph qN8y UCpq qiGt 7vuT yR"
        );
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
