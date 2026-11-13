use std::cmp::Ordering;

use bahaa_engine::{Amount, FeeOption, FeeQuote, FeeRate, NetworkId, RateUnit, Speed, Wait};
use ruint::aliases::U256;

use super::{button, cell, input, note, replaceable};
use crate::action::{Action, Field, Pick};
use crate::core::Ctx;
use crate::view::{Level, Note, Page, PageId, Row, Table, Tone};
use crate::{format, parse, text};

pub(crate) const CUSTOM: usize = 3;
const COSTLY: u64 = 3;

pub(crate) fn table(
    fee: &FeeQuote,
    chosen: Speed,
    network: &NetworkId,
    ctx: &Ctx<'_>,
) -> Option<Table> {
    let options = fee.options();
    let developer = ctx.data.settings.developer_mode && unit(fee).is_some();
    if options.len() < 2 || (same_price(options) && !developer) {
        return None;
    }
    let picked = FeeOption {
        speed: chosen,
        fee: fee.expected(),
        pace: fee.pace(),
    };
    let mut rows: Vec<Row> = options
        .iter()
        .enumerate()
        .map(|(position, option)| {
            row(
                ctx,
                network,
                option,
                option.speed == chosen,
                &picked,
                position,
            )
        })
        .collect();
    if developer {
        rows.push(if matches!(chosen, Speed::Custom(_)) {
            row(ctx, network, &picked, true, &picked, CUSTOM)
        } else {
            Row {
                cells: vec![
                    cell(text::CUSTOM_SPEED, Tone::Plain),
                    cell(text::NO_VALUE, Tone::Muted),
                    cell(text::SET_YOUR_OWN, Tone::Muted),
                    cell(String::new(), Tone::Muted),
                ],
                open: Some(Action::Pick(Pick::Speed(CUSTOM))),
                warn: false,
            }
        });
    }
    Some(Table {
        columns: [text::SPEED, text::WAIT, text::FEE, text::COMPARED]
            .map(str::to_owned)
            .to_vec(),
        rows,
        empty: String::new(),
    })
}

pub(crate) fn notes(
    fee: &FeeQuote,
    chosen: Speed,
    network: &NetworkId,
    ctx: &Ctx<'_>,
) -> Vec<Note> {
    let mut notes = Vec::new();
    if fee.options().len() > 1 && same_price(fee.options()) {
        notes.push(note(Level::Info, text::SAME_FEES));
    }
    if chosen == Speed::Slow && replaceable(ctx, network) {
        notes.push(note(Level::Info, text::SLOW_HELP));
    }
    notes
}

pub(crate) fn chosen(position: usize) -> Option<Speed> {
    Speed::CHOICES.get(position).copied()
}

pub(crate) fn custom_page(fee: &FeeQuote, typed: &str, error: Option<String>) -> Page {
    let unit = unit(fee).unwrap_or(RateUnit::SatPerVbyte);
    let mut field = input(Field::FeeRate, text::rate_unit(unit), typed);
    field.hint = hint(fee, unit, typed);
    Page {
        id: PageId::CustomFee,
        title: text::CUSTOM_FEE.to_owned(),
        text: Some(text::custom_about(unit).to_owned()),
        form: vec![field.into()],
        notes: vec![note(Level::Warning, text::CUSTOM_RISK)],
        error,
        buttons: vec![button(text::USE_THIS_FEE, Action::Submit)],
        ..Page::default()
    }
}

pub(crate) fn custom_speed(fee: &FeeQuote, typed: &str) -> Result<Speed, String> {
    let unit = unit(fee).ok_or(text::ENTER_FEE)?;
    if typed.trim().is_empty() {
        return Err(text::ENTER_FEE.to_owned());
    }
    Ok(Speed::Custom(FeeRate {
        amount: parse::amount(typed, places(unit))?,
        unit,
    }))
}

fn row(
    ctx: &Ctx<'_>,
    network: &NetworkId,
    option: &FeeOption,
    mine: bool,
    picked: &FeeOption,
    position: usize,
) -> Row {
    let name = match option.speed {
        Speed::Slow => text::SLOW_SPEED,
        Speed::Normal => text::NORMAL_SPEED,
        Speed::Fast => text::FAST_SPEED,
        Speed::Custom(_) => text::CUSTOM_SPEED,
    };
    Row {
        cells: vec![
            if mine {
                cell(text::picked(name), Tone::Good)
            } else {
                cell(name, Tone::Plain)
            },
            cell(format::short_wait(option.pace.wait), Tone::Muted),
            cell(worth(ctx, network, option.fee), Tone::Plain),
            if mine {
                cell(text::NO_VALUE, Tone::Muted)
            } else {
                cell(compared(ctx, network, option, picked), Tone::Muted)
            },
        ],
        open: Some(Action::Pick(Pick::Speed(position))),
        warn: false,
    }
}

fn compared(ctx: &Ctx<'_>, network: &NetworkId, option: &FeeOption, picked: &FeeOption) -> String {
    let order = option.fee.partial_cmp(&picked.fee);
    let cost = match (
        order,
        option.fee.checked_sub(picked.fee),
        picked.fee.checked_sub(option.fee),
    ) {
        (Some(Ordering::Greater), Ok(extra), _) => text::more(&worth(ctx, network, extra)),
        (Some(Ordering::Less), _, Ok(saved)) => text::less(&worth(ctx, network, saved)),
        _ => text::SAME_PRICE.to_owned(),
    };
    let time = match (option.pace.wait, picked.pace.wait) {
        (Some(Wait::About(this)), Some(Wait::About(mine))) => Some(match this.cmp(&mine) {
            Ordering::Less => text::sooner(&format::short_span(mine - this)),
            Ordering::Greater => text::later(&format::short_span(this - mine)),
            Ordering::Equal if order == Some(Ordering::Greater) => text::NO_FASTER.to_owned(),
            Ordering::Equal => text::JUST_AS_FAST.to_owned(),
        }),
        _ => None,
    };
    text::tradeoff(&cost, time.as_deref())
}

fn worth(ctx: &Ctx<'_>, network: &NetworkId, amount: Amount) -> String {
    match super::native_worth(ctx, network, amount) {
        Some(value) if !value.is_zero() => format::fiat(Some(value), ctx.fiat()),
        Some(_) if !amount.is_zero() => text::under(&format::fiat(
            Amount::new(U256::ONE, ctx.fiat().places()).ok(),
            ctx.fiat(),
        )),
        _ => format::exact(amount, &ctx.native_symbol(network)),
    }
}

fn same_price(options: &[FeeOption]) -> bool {
    options.windows(2).all(|pair| pair[0].fee == pair[1].fee)
}

fn unit(fee: &FeeQuote) -> Option<RateUnit> {
    fee.options()
        .iter()
        .find_map(|option| option.pace.rate.map(|rate| rate.unit))
}

fn places(unit: RateUnit) -> u8 {
    match unit {
        RateUnit::SatPerVbyte => 3,
        RateUnit::Gwei => 9,
        RateUnit::MicroLamports => 0,
    }
}

fn hint(fee: &FeeQuote, unit: RateUnit, typed: &str) -> Option<Note> {
    let rates: Vec<Amount> = fee
        .options()
        .iter()
        .filter_map(|option| option.pace.rate.map(|rate| rate.amount))
        .collect();
    let [slow, normal, fast] = rates.as_slice() else {
        return None;
    };
    let named = text::rate_unit(unit);
    let today = text::today(
        &slow.to_string(),
        &normal.to_string(),
        &fast.to_string(),
        named,
    );
    let (level, said) = match parse::amount(typed, places(unit)) {
        Ok(amount) if amount < *slow => {
            (Level::Warning, text::below_slow(&slow.to_string(), named))
        }
        Ok(amount) => match times(amount, *fast) {
            Some(times) if times >= COSTLY => (
                Level::Warning,
                text::times_fast(&times.to_string(), &fast.to_string(), named),
            ),
            _ => (Level::Info, today),
        },
        Err(_) => (Level::Info, today),
    };
    Some(note(level, &said))
}

fn times(amount: Amount, fast: Amount) -> Option<u64> {
    if fast.is_zero() {
        return None;
    }
    u64::try_from(amount.units() / fast.units()).ok()
}

#[cfg(test)]
mod tests {
    use bahaa_engine::{
        Amount, FeeItem, FeeLine, FeeOption, FeeQuote, FeeRate, Pace, RateUnit, Registry, Speed,
        Wait,
    };

    use super::{custom_page, custom_speed, table};
    use crate::core::{Ctx, Data, Host};
    use crate::view::Level;

    fn btc(text: &str) -> Amount {
        Amount::parse(text, 8).unwrap()
    }

    fn option(speed: Speed, fee: &str, rate: &str, seconds: u64) -> FeeOption {
        FeeOption {
            speed,
            fee: btc(fee),
            pace: Pace {
                rate: Some(FeeRate {
                    amount: Amount::parse(rate, 3).unwrap(),
                    unit: RateUnit::SatPerVbyte,
                }),
                wait: Some(Wait::About(seconds)),
            },
        }
    }

    fn quote(options: Vec<FeeOption>, chosen: usize) -> FeeQuote {
        let picked = options[chosen];
        let lines = vec![FeeLine {
            item: FeeItem::Size { vbytes: 141 },
            amount: picked.fee,
        }];
        FeeQuote::new(picked.fee, picked.fee, lines)
            .unwrap()
            .paced(picked.pace)
            .offering(options)
    }

    fn today() -> Vec<FeeOption> {
        vec![
            option(Speed::Slow, "0.00000017", "0.11", 86_400),
            option(Speed::Normal, "0.00000019", "0.13", 3_600),
            option(Speed::Fast, "0.00000151", "1.07", 1_200),
        ]
    }

    fn rows(fee: &FeeQuote, chosen: Speed, data: &Data) -> Option<Vec<Vec<String>>> {
        let registry = Registry::builtin().unwrap();
        let host = Host {
            folder: String::new(),
            updates: false,
        };
        let ctx = Ctx {
            registry: &registry,
            data,
            host: &host,
            now: 0,
        };
        table(fee, chosen, &"bitcoin".parse().unwrap(), &ctx).map(|table| {
            table
                .rows
                .into_iter()
                .map(|row| row.cells.into_iter().map(|cell| cell.text).collect())
                .collect()
        })
    }

    #[test]
    fn every_speed_shows_what_it_costs_and_saves_against_the_pick() {
        let shown = rows(&quote(today(), 1), Speed::Normal, &Data::default()).unwrap();
        assert_eq!(
            shown,
            [
                [
                    "Slow",
                    "~1 d",
                    "0.00000017 BTC",
                    "0.00000002 BTC less · 23 h later"
                ],
                ["Normal ✓", "~1 h", "0.00000019 BTC", "—"],
                [
                    "Fast",
                    "~20 min",
                    "0.00000151 BTC",
                    "0.00000132 BTC more · 40 min sooner"
                ],
            ]
        );
        let from_fast = rows(&quote(today(), 2), Speed::Fast, &Data::default()).unwrap();
        assert_eq!(from_fast[1][3], "0.00000132 BTC less · 40 min later");
        let mut same = today();
        same[2] = option(Speed::Fast, "0.00000151", "1.07", 3_600);
        let not_faster = rows(&quote(same, 1), Speed::Normal, &Data::default()).unwrap();
        assert_eq!(not_faster[2][3], "0.00000132 BTC more · no faster now");
    }

    #[test]
    fn equal_prices_need_no_table_and_developers_also_get_custom() {
        let flat = vec![
            option(Speed::Slow, "0.00000141", "1", 1_200),
            option(Speed::Normal, "0.00000141", "1", 1_200),
            option(Speed::Fast, "0.00000141", "1", 1_200),
        ];
        assert_eq!(
            rows(&quote(flat.clone(), 1), Speed::Normal, &Data::default()),
            None
        );
        let mut data = Data::default();
        data.settings.developer_mode = true;
        let shown = rows(&quote(flat, 1), Speed::Normal, &data).unwrap();
        assert_eq!(shown[3][0], "Custom");
        assert_eq!(
            rows(
                &FeeQuote::new(
                    btc("1"),
                    btc("1"),
                    vec![FeeLine {
                        item: FeeItem::Size { vbytes: 1 },
                        amount: btc("1")
                    }]
                )
                .unwrap(),
                Speed::Normal,
                &data
            ),
            None
        );
    }

    #[test]
    fn a_custom_fee_is_read_in_its_unit_and_warned_about() {
        let fee = quote(today(), 1);
        assert_eq!(
            custom_speed(&fee, "0.5"),
            Ok(Speed::Custom(FeeRate {
                amount: Amount::parse("0.5", 3).unwrap(),
                unit: RateUnit::SatPerVbyte,
            }))
        );
        assert!(custom_speed(&fee, "").is_err());
        assert!(custom_speed(&fee, "0.0001").is_err());
        let hint = |typed: &str| {
            let page = custom_page(&fee, typed, None);
            page.inputs().next().unwrap().hint.clone().unwrap()
        };
        assert_eq!(hint("").level, Level::Info);
        assert_eq!(
            hint("").text,
            "Today: slow 0.11 · normal 0.13 · fast 1.07 sat/vB"
        );
        assert_eq!(hint("0.05").level, Level::Warning);
        assert_eq!(hint("0.05").text, "Below today's slow price of 0.11 sat/vB");
        assert_eq!(hint("0.5").level, Level::Info);
        assert_eq!(hint("4").text, "3× today's fast price of 1.07 sat/vB");
    }
}
