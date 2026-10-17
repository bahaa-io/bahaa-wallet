pub fn is_plain_text(text: &str, max_chars: usize) -> bool {
    let count = text.chars().count();
    (1..=max_chars).contains(&count) && text.trim() == text && !text.chars().any(hidden)
}

pub fn to_plain_text(text: &str, max_chars: usize) -> String {
    let visible: String = text
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .filter(|&c| !hidden(c))
        .collect();
    let words: Vec<&str> = visible.split_whitespace().collect();
    words
        .join(" ")
        .chars()
        .take(max_chars)
        .collect::<String>()
        .trim_end()
        .to_owned()
}

fn hidden(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ordinary_names_in_any_script() {
        for text in ["Mom", "Exchange wallet", "مامان", "钱包", "Café 2"] {
            assert!(is_plain_text(text, 40), "{text}");
        }
    }

    #[test]
    fn rejects_hidden_or_reordering_characters() {
        for text in [
            "",
            " Mom",
            "Mom ",
            "Mo\u{202E}m",
            "Mo\u{200B}m",
            "Mo\u{7}m",
            "Mo\nm",
            "\u{FEFF}Mom",
        ] {
            assert!(!is_plain_text(text, 40), "{text:?}");
        }
        assert!(!is_plain_text(&"a".repeat(41), 40));
        assert!(is_plain_text(&"é".repeat(40), 40));
    }

    #[test]
    fn cleans_untrusted_text_into_plain_text() {
        let cases = [
            ("nonce too low", "nonce too low"),
            ("  insufficient\n\tfunds  ", "insufficient funds"),
            ("evil\u{202E}txt\u{200B}", "eviltxt"),
            ("\u{7}\u{1b}[31mred", "[31mred"),
            ("", ""),
        ];
        for (input, cleaned) in cases {
            assert_eq!(to_plain_text(input, 40), cleaned, "{input:?}");
        }
        assert_eq!(to_plain_text("abcdef", 3), "abc");
        assert_eq!(to_plain_text("ab cdef", 3), "ab");
        for input in ["a\u{202E}b", "x\ny", "مامان پول"] {
            assert!(is_plain_text(&to_plain_text(input, 40), 40));
        }
    }
}
