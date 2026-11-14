use std::collections::BTreeMap;

use bahaa_engine::{Amount, AssetId, EngineError, Fiat, Point, Price, Span};
use ruint::aliases::U256;

use super::{choice, group};
use crate::action::{Action, Pick};
use crate::call::Call;
use crate::view::{CHART_LEVELS, Chart, ChartPoint, Glance, Tone};
use crate::{format, text};

const SHOWN: usize = 240;
const PLACES: u8 = 24;
const HUNDREDTHS: u16 = 10_000;

pub(crate) struct History {
    asset: AssetId,
    span: Span,
    loaded: BTreeMap<Span, Option<Vec<Point>>>,
}

impl History {
    pub(crate) fn open(asset: AssetId) -> (Self, Call) {
        let history = Self {
            asset,
            span: Span::Day,
            loaded: BTreeMap::new(),
        };
        let call = history.call();
        (history, call)
    }

    pub(crate) fn pick(&mut self, span: Span) -> Option<Call> {
        self.span = span;
        (!matches!(self.loaded.get(&span), Some(Some(_)))).then(|| self.call())
    }

    pub(crate) fn arrived(&mut self, span: Span, found: Result<Vec<Point>, EngineError>) {
        self.loaded.insert(span, found.ok());
    }

    pub(crate) fn chart(&self, live: Option<Price>, fiat: Fiat, now: u64) -> Chart {
        let spans = group(
            text::CHART,
            Span::ALL
                .into_iter()
                .map(|span| {
                    choice(
                        text::span_label(span),
                        Action::Pick(Pick::Span(span)),
                        span == self.span,
                    )
                })
                .collect(),
        );
        let waiting = |note: &str| Chart {
            glance: Glance {
                price: format::price(live, fiat),
                tone: Tone::Muted,
                ..Glance::default()
            },
            points: Vec::new(),
            high: 0,
            low: 0,
            spans: spans.clone(),
            note: Some(note.to_owned()),
        };
        let points = match self.loaded.get(&self.span) {
            None => return waiting(text::CHART_LOADING),
            Some(None) => return waiting(text::CHART_FAILED),
            Some(Some(points)) => shown(points, live, now),
        };
        let Some(units) = units(&points) else {
            return waiting(text::CHART_EMPTY);
        };
        let (high, low) = extremes(&units);
        let glance = |index: usize| glance(&points, &units, index, self.span, fiat);
        let range = units[high] - units[low];
        let live = points.last().is_some_and(|last| last.time == now);
        let points = units
            .iter()
            .enumerate()
            .map(|(index, value)| ChartPoint {
                level: level(*value - units[low], range),
                mark: if live && index + 1 == units.len() {
                    text::CHART_NOW.to_owned()
                } else {
                    format::chart_mark(points[index].time, self.span)
                },
                glance: glance(index),
            })
            .collect::<Vec<_>>();
        Chart {
            glance: Glance {
                when: text::span_period(self.span).to_owned(),
                ..glance(units.len() - 1)
            },
            points,
            high,
            low,
            spans,
            note: None,
        }
    }

    fn call(&self) -> Call {
        Call::PriceHistory(self.asset.clone(), self.span)
    }
}

fn shown(points: &[Point], live: Option<Price>, now: u64) -> Vec<Point> {
    let mut all = points.to_vec();
    if let Some(price) = live
        && all.last().is_some_and(|last| last.time < now)
    {
        all.push(Point { time: now, price });
    }
    let count = all.len();
    if count <= SHOWN {
        return all;
    }
    (0..SHOWN)
        .map(|step| all[step * (count - 1) / (SHOWN - 1)])
        .collect()
}

fn units(points: &[Point]) -> Option<Vec<U256>> {
    let one = Amount::new(U256::from(1u8), 0).ok()?;
    let units = points
        .iter()
        .map(|point| {
            point
                .price
                .value_of(one, PLACES)
                .ok()
                .map(|value| value.units())
        })
        .collect::<Option<Vec<_>>>()?;
    (!units.is_empty()).then_some(units)
}

fn extremes(units: &[U256]) -> (usize, usize) {
    let mut high = 0;
    let mut low = 0;
    for (index, value) in units.iter().enumerate() {
        if *value > units[high] {
            high = index;
        }
        if *value < units[low] {
            low = index;
        }
    }
    (high, low)
}

fn level(above_low: U256, range: U256) -> u16 {
    if range.is_zero() {
        return CHART_LEVELS / 2;
    }
    let scaled = above_low.saturating_mul(U256::from(CHART_LEVELS)) / range;
    u16::try_from(scaled).unwrap_or(CHART_LEVELS)
}

fn glance(points: &[Point], units: &[U256], index: usize, span: Span, fiat: Fiat) -> Glance {
    let (first, value) = (units[0], units[index]);
    let up = (value != first).then_some(value > first);
    let difference = value.abs_diff(first);
    let hundredths = if first.is_zero() {
        0
    } else {
        let scaled = difference.saturating_mul(U256::from(HUNDREDTHS)) + first / U256::from(2u8);
        u128::try_from(scaled / first).unwrap_or(u128::MAX)
    };
    let moved = match (up, Amount::new(difference, PLACES)) {
        (Some(up), Ok(difference)) => format::moved(up, difference, fiat),
        _ => String::new(),
    };
    Glance {
        price: format::price(Some(points[index].price), fiat),
        change: format::change(up, hundredths),
        moved,
        when: format::chart_time(points[index].time, span),
        tone: match up {
            Some(true) => Tone::Good,
            Some(false) => Tone::Bad,
            None => Tone::Muted,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn points(prices: &[&str]) -> Vec<Point> {
        prices
            .iter()
            .enumerate()
            .map(|(index, price)| Point {
                time: 1_794_441_600 + 3_600 * index as u64,
                price: Price::parse(price).unwrap(),
            })
            .collect()
    }

    fn history(span: Span, found: Result<Vec<Point>, EngineError>) -> History {
        let (mut history, _) = History::open("ton".parse().unwrap());
        history.pick(span);
        history.arrived(span, found);
        history
    }

    #[test]
    fn the_chart_ends_at_the_live_price_and_marks_the_extremes() {
        let history = history(Span::Week, Ok(points(&["100", "120", "80", "90"])));
        let live = Price::parse("95").ok();
        let chart = history.chart(live, Fiat::Usd, 1_800_000_000);
        assert_eq!(chart.note, None);
        assert_eq!(chart.points.len(), 5);
        assert_eq!((chart.high, chart.low), (1, 2));
        let levels: Vec<u16> = chart.points.iter().map(|point| point.level).collect();
        assert_eq!(levels, [5_000, 10_000, 0, 2_500, 3_750]);
        assert_eq!(chart.glance.price, "$95");
        assert_eq!(chart.glance.change, "▼ 5.00%");
        assert_eq!(chart.glance.moved, "−$5");
        assert_eq!(chart.glance.when, "past week");
        assert_eq!(chart.glance.tone, Tone::Bad);
        let peak = &chart.points[1].glance;
        assert_eq!(
            (peak.change.as_str(), peak.moved.as_str()),
            ("▲ 20.00%", "+$20")
        );
        assert_eq!(peak.when, "Nov 12, 01:00");
        assert_eq!(chart.points[0].glance.change, "0.00%");
        assert_eq!(chart.points[4].mark, text::CHART_NOW);
        assert_eq!(chart.points[3].mark, "Nov 12");
        assert_eq!(chart.points[0].glance.tone, Tone::Muted);
        let picked: Vec<bool> = chart
            .spans
            .choices
            .iter()
            .map(|choice| choice.selected)
            .collect();
        assert_eq!(picked, [false, true, false, false, false]);
    }

    #[test]
    fn long_histories_are_thinned_but_keep_both_ends() {
        let prices: Vec<String> = (1..=1_000).map(|price| price.to_string()).collect();
        let prices: Vec<&str> = prices.iter().map(String::as_str).collect();
        let chart = history(Span::All, Ok(points(&prices))).chart(None, Fiat::Usd, 0);
        assert_eq!(chart.points.len(), SHOWN);
        assert_eq!(chart.points[0].glance.price, "$1");
        assert_eq!(chart.points[SHOWN - 1].glance.price, "$1,000");
        assert_eq!(chart.points[SHOWN - 1].mark, "2026");
    }

    #[test]
    fn missing_failed_and_flat_histories_still_draw_a_chart() {
        let (mut history, call) = History::open("ton".parse().unwrap());
        assert!(matches!(call, Call::PriceHistory(_, Span::Day)));
        let live = Price::parse("1.5").ok();
        let loading = history.chart(live, Fiat::Eur, 0);
        assert_eq!(loading.note.as_deref(), Some(text::CHART_LOADING));
        assert_eq!(loading.glance.price, "€1.5");
        history.arrived(Span::Day, Err(EngineError::Stopped));
        assert_eq!(
            history.chart(live, Fiat::Eur, 0).note.as_deref(),
            Some(text::CHART_FAILED)
        );
        assert!(history.pick(Span::Day).is_some());
        history.arrived(Span::Day, Ok(points(&["2", "2"])));
        assert!(history.pick(Span::Day).is_none());
        let flat = history.chart(None, Fiat::Eur, 0);
        assert!(
            flat.points
                .iter()
                .all(|point| point.level == CHART_LEVELS / 2)
        );
        assert_eq!(flat.glance.moved, "");
        history.arrived(Span::Day, Ok(Vec::new()));
        assert_eq!(
            history.chart(None, Fiat::Eur, 0).note.as_deref(),
            Some(text::CHART_EMPTY)
        );
    }
}
