const MINUTE: u64 = 60;
const HOUR: u64 = 60 * MINUTE;
const DAY: u64 = 24 * HOUR;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Utc {
    pub year: u64,
    pub month: u64,
    pub day: u64,
    pub hour: u64,
    pub minute: u64,
    pub second: u64,
}

impl Utc {
    pub fn at(seconds: u64) -> Self {
        let (year, month, day) = civil(seconds / DAY);
        let rest = seconds % DAY;
        Self {
            year,
            month,
            day,
            hour: rest / HOUR,
            minute: rest % HOUR / MINUTE,
            second: rest % MINUTE,
        }
    }
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

#[cfg(test)]
mod tests {
    use super::Utc;

    #[test]
    fn unix_seconds_become_calendar_time() {
        let at = |year, month, day, hour, minute, second| Utc {
            year,
            month,
            day,
            hour,
            minute,
            second,
        };
        assert_eq!(Utc::at(0), at(1970, 1, 1, 0, 0, 0));
        assert_eq!(Utc::at(951_782_400), at(2000, 2, 29, 0, 0, 0));
        assert_eq!(Utc::at(1_793_722_541), at(2026, 11, 3, 16, 15, 41));
    }
}
