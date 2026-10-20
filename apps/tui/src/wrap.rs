use std::ops::Range;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub(crate) fn width(text: &str) -> u16 {
    cells(text.width())
}

pub(crate) fn cells(count: usize) -> u16 {
    u16::try_from(count).unwrap_or(u16::MAX)
}

pub(crate) fn lines(text: &str, width: u16) -> Vec<String> {
    text.split('\n')
        .flat_map(|paragraph| {
            let chars: Vec<char> = paragraph.chars().collect();
            let mut lines: Vec<String> = ranges(paragraph, width)
                .into_iter()
                .map(|range| {
                    chars[range]
                        .iter()
                        .collect::<String>()
                        .trim_end()
                        .to_owned()
                })
                .collect();
            if lines.is_empty() {
                lines.push(String::new());
            }
            lines
        })
        .collect()
}

pub(crate) fn ranges(text: &str, width: u16) -> Vec<Range<usize>> {
    let width = usize::from(width.max(1));
    let chars: Vec<(bool, usize)> = text
        .chars()
        .map(|c| (c == ' ', c.width().unwrap_or(0)))
        .collect();
    let mut ranges = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let mut used = 0;
        let mut end = start;
        let mut cut = None;
        while end < chars.len() {
            let (space, size) = chars[end];
            if space {
                end += 1;
                cut = Some(end);
                used += size;
                continue;
            }
            if used + size > width {
                break;
            }
            used += size;
            end += 1;
        }
        if end < chars.len() && !chars[end - 1].0 {
            end = cut.unwrap_or(end).max(start + 1);
        }
        ranges.push(start..end);
        start = end;
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::lines;

    #[test]
    fn words_wrap_and_long_words_break() {
        assert_eq!(lines("the quick brown fox", 9), ["the quick", "brown fox"]);
        assert_eq!(lines("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(lines("one\n\ntwo", 10), ["one", "", "two"]);
        assert_eq!(lines("a  b", 1), ["a", "b"]);
        assert_eq!(lines("", 5), [""]);
    }
}
