use std::fmt;
use std::str::FromStr;

use bahaa_types::hex;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Color {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

impl FromStr for Color {
    type Err = &'static str;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let [red, green, blue] = text
            .strip_prefix('#')
            .and_then(hex::decode_array::<3>)
            .ok_or("a color is # and six hex digits")?;
        Ok(Self { red, green, blue })
    }
}

impl TryFrom<String> for Color {
    type Error = &'static str;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        text.parse()
    }
}

impl From<Color> for String {
    fn from(color: Color) -> Self {
        color.to_string()
    }
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.red, self.green, self.blue)
    }
}

#[cfg(test)]
mod tests {
    use super::Color;

    #[test]
    fn colors_read_and_print_as_hex() {
        let color: Color = "#F0B90b".parse().unwrap();
        assert_eq!(
            color,
            Color {
                red: 0xf0,
                green: 0xb9,
                blue: 0x0b
            }
        );
        assert_eq!(color.to_string(), "#f0b90b");
        assert_eq!(serde_json::to_string(&color).unwrap(), "\"#f0b90b\"");
        for bad in ["f0b90b", "#f0b90", "#f0b90bb", "#g0b90b", ""] {
            assert!(bad.parse::<Color>().is_err(), "{bad}");
        }
    }
}
