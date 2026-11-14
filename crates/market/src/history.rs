use bahaa_types::Price;
use serde::Deserialize;
use serde_json::value::RawValue;

use crate::MarketError;
use crate::protocol;

pub const MAX_POINTS: usize = 10_000;
const DAY: u64 = 86_400;
const SINCE_2000: u64 = 946_684_800;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Span {
    Day,
    Week,
    Month,
    Year,
    All,
}

impl Span {
    pub const ALL: [Self; 5] = [Self::Day, Self::Week, Self::Month, Self::Year, Self::All];

    pub(crate) fn query(self, now: u64) -> (&'static str, u64) {
        match self {
            Self::Day => ("5m", now.saturating_sub(DAY)),
            Self::Week => ("1h", now.saturating_sub(7 * DAY)),
            Self::Month => ("1h", now.saturating_sub(30 * DAY)),
            Self::Year => ("1d", now.saturating_sub(365 * DAY)),
            Self::All => ("1w", SINCE_2000),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Point {
    pub time: u64,
    pub price: Price,
}

#[derive(Deserialize)]
struct Meta {
    columns: Vec<String>,
}

pub(crate) fn read(body: &[u8]) -> Result<Vec<Point>, MarketError> {
    let (rows, meta) = protocol::open::<Vec<Vec<Box<RawValue>>>, Meta>(body)?;
    let columns = meta.map(|meta| meta.columns).unwrap_or_default();
    let column = |name: &str| {
        columns
            .iter()
            .position(|column| column == name)
            .ok_or_else(|| MarketError::Format(format!("history without a {name} column")))
    };
    let (time, close) = (column("t")?, column("c")?);
    if rows.len() > MAX_POINTS {
        return Err(MarketError::Format("history with too many rows".to_owned()));
    }
    let mut points: Vec<Point> = Vec::with_capacity(rows.len());
    for row in &rows {
        let (Some(time), Some(close)) = (row.get(time), row.get(close)) else {
            return Err(MarketError::Format("history row is too short".to_owned()));
        };
        if close.get() == "null" {
            continue;
        }
        let time: u64 = time
            .get()
            .parse()
            .map_err(|_| MarketError::Format("history time isn't a unix time".to_owned()))?;
        let price = Price::parse(close.get())
            .map_err(|_| MarketError::Format("history price isn't a price".to_owned()))?;
        if points.last().is_some_and(|last| last.time >= time) {
            return Err(MarketError::Format(
                "history isn't in time order".to_owned(),
            ));
        }
        points.push(Point { time, price });
    }
    Ok(points)
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPLY: &str = r#"{"ok": true, "meta": {"columns": ["t", "o", "h", "l", "c", "v"]},
        "result": [[1794441600, 73925.52, 74128.37, 71069.82, 71377.02, null],
                   [1794528000, 71377.02, 72000, 70000, null, null],
                   [1794614400, 71377.02, 72000, 70000, 1.002e-5, 12.5]]}"#;

    #[test]
    fn reads_closing_prices_exactly_and_skips_empty_bars() {
        let points = read(REPLY.as_bytes()).unwrap();
        assert_eq!(points.len(), 2);
        assert_eq!(points[0].time, 1_794_441_600);
        assert_eq!(points[0].price.to_string(), "71377.02");
        assert_eq!(points[1].price.to_string(), "0.00001002");
    }

    #[test]
    fn refuses_histories_it_cannot_trust() {
        let reply = |columns: &str, rows: &str| {
            read(
                format!(r#"{{"ok": true, "meta": {{"columns": {columns}}}, "result": {rows}}}"#)
                    .as_bytes(),
            )
        };
        let columns = r#"["t", "c"]"#;
        assert!(reply(columns, "[[2, 1], [1, 1]]").is_err());
        assert!(reply(columns, "[[1, 1], [1, 2]]").is_err());
        assert!(reply(columns, "[[1, -3]]").is_err());
        assert!(reply(columns, "[[-1, 3]]").is_err());
        assert!(reply(columns, "[[1]]").is_err());
        assert!(reply(r#"["t"]"#, "[[1, 1]]").is_err());
        assert_eq!(reply(columns, "[[1, 3], [2, 4]]").unwrap().len(), 2);
        assert!(matches!(
            read(br#"{"ok": false, "code": "NOT_FOUND", "description": "unknown asset"}"#),
            Err(MarketError::Api { .. })
        ));
    }

    #[test]
    fn each_span_asks_for_a_fitting_resolution() {
        let now = 1_800_000_000;
        assert_eq!(Span::Day.query(now), ("5m", now - DAY));
        assert_eq!(Span::Month.query(now), ("1h", now - 30 * DAY));
        assert_eq!(Span::Year.query(now), ("1d", now - 365 * DAY));
        assert_eq!(Span::All.query(now), ("1w", SINCE_2000));
    }
}
