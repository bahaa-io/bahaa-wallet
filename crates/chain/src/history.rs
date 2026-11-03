use crate::ScanCursor;

const SETTLE: u64 = 300;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WalkEnd {
    Missing,
    Start(u64),
    Connected,
    Capped(u64),
}

pub fn covered(
    cursor: Option<ScanCursor>,
    walked: &[(Option<u64>, WalkEnd)],
    checked: u64,
) -> ScanCursor {
    let stop = cursor.map_or(0, |cursor| cursor.next);
    let newest = walked.iter().filter_map(|(head, _)| *head).max();
    let seen = newest.map_or(checked, |newest| checked.min(newest));
    let next = stop.max(seen.saturating_sub(SETTLE));
    let since = walked
        .iter()
        .filter_map(|(_, end)| match end {
            WalkEnd::Missing => None,
            WalkEnd::Start(time) => Some(*time),
            WalkEnd::Connected => cursor.map(|cursor| cursor.complete_since),
            WalkEnd::Capped(time) => Some(time.saturating_add(1)),
        })
        .max()
        .unwrap_or_else(|| cursor.map_or(checked, |cursor| cursor.complete_since))
        .min(next);
    ScanCursor {
        next,
        complete_from: since,
        complete_since: since,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_792_300_000;

    fn cursor(next: u64, since: u64) -> ScanCursor {
        ScanCursor {
            next,
            complete_from: since,
            complete_since: since,
        }
    }

    #[test]
    fn coverage_is_the_latest_point_every_history_reaches() {
        let first = covered(
            None,
            &[
                (Some(1_792_200_000), WalkEnd::Capped(1_791_000_000)),
                (Some(1_792_100_000), WalkEnd::Start(1_791_500_000)),
                (None, WalkEnd::Missing),
            ],
            NOW,
        );
        assert_eq!(first, cursor(1_792_199_700, 1_791_500_000));
        let later = covered(
            Some(first),
            &[
                (Some(1_792_250_000), WalkEnd::Connected),
                (Some(1_792_150_000), WalkEnd::Connected),
            ],
            NOW,
        );
        assert_eq!(later, cursor(1_792_249_700, 1_791_500_000));
        let gap = covered(
            Some(later),
            &[(Some(1_792_290_000), WalkEnd::Capped(1_792_260_000))],
            NOW,
        );
        assert_eq!(gap, cursor(1_792_289_700, 1_792_260_001));
    }

    #[test]
    fn the_cursor_never_runs_ahead_of_the_clock_or_backwards() {
        assert_eq!(
            covered(None, &[(None, WalkEnd::Missing)], NOW),
            cursor(NOW - SETTLE, NOW - SETTLE)
        );
        assert_eq!(
            covered(
                None,
                &[(Some(1_900_000_000), WalkEnd::Start(1_800_000_000))],
                NOW
            ),
            cursor(NOW - SETTLE, NOW - SETTLE)
        );
        let ahead = cursor(NOW, 5);
        assert_eq!(
            covered(
                Some(ahead),
                &[(Some(1_700_000_000), WalkEnd::Connected)],
                NOW
            ),
            cursor(NOW, 5)
        );
        assert_eq!(
            covered(Some(ahead), &[(None, WalkEnd::Missing)], NOW),
            cursor(NOW, 5)
        );
    }
}
