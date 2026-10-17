pub fn is_plain_text(text: &str, max_chars: usize) -> bool {
    let count = text.chars().count();
    (1..=max_chars).contains(&count)
        && text.trim() == text
        && !text.chars().any(|c| {
            c.is_control()
                || matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}')
        })
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
}
