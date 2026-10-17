use std::fmt;
use std::str::FromStr;

use thiserror::Error;

pub const HARDENED: u32 = 0x8000_0000;
const MAX_DEPTH: usize = 255;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DerivationPath(Vec<u32>);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[error("a derivation path looks like m/44'/60'/0'/0/0")]
pub struct PathError;

impl DerivationPath {
    pub fn indices(&self) -> &[u32] {
        &self.0
    }

    pub fn is_hardened(&self) -> bool {
        self.0.iter().all(|index| index & HARDENED != 0)
    }
}

impl FromStr for DerivationPath {
    type Err = PathError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let mut parts = text.split('/');
        if parts.next() != Some("m") {
            return Err(PathError);
        }
        let indices = parts
            .map(|part| {
                let (digits, hardened) = match part.strip_suffix(['\'', 'h', 'H']) {
                    Some(digits) => (digits, true),
                    None => (part, false),
                };
                let canonical = !digits.is_empty()
                    && digits.bytes().all(|byte| byte.is_ascii_digit())
                    && !(digits.len() > 1 && digits.starts_with('0'));
                let value: u32 = digits
                    .parse()
                    .ok()
                    .filter(|value| canonical && *value < HARDENED)
                    .ok_or(PathError)?;
                Ok(if hardened { value | HARDENED } else { value })
            })
            .collect::<Result<Vec<u32>, PathError>>()?;
        if indices.len() > MAX_DEPTH {
            return Err(PathError);
        }
        Ok(Self(indices))
    }
}

impl fmt::Display for DerivationPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("m")?;
        for index in &self.0 {
            write!(f, "/{}", index & !HARDENED)?;
            if index & HARDENED != 0 {
                f.write_str("'")?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(text: &str) -> DerivationPath {
        text.parse().unwrap()
    }

    #[test]
    fn parses_standard_wallet_paths() {
        assert_eq!(path("m").indices(), &[] as &[u32]);
        assert_eq!(
            path("m/44'/60'/0'/0/0").indices(),
            &[44 + HARDENED, 60 + HARDENED, HARDENED, 0, 0]
        );
        assert_eq!(path("m/44h/195H/0'/0/7"), path("m/44'/195'/0'/0/7"));
        assert_eq!(path("m/2147483647'").indices(), &[u32::MAX]);
    }

    #[test]
    fn displays_one_canonical_spelling() {
        for text in [
            "m",
            "m/44'/60'/0'/0/0",
            "m/0/2147483647'/1",
            "m/44'/607'/0'",
        ] {
            assert_eq!(path(text).to_string(), text);
        }
        assert_eq!(path("m/44h/607H/0h").to_string(), "m/44'/607'/0'");
    }

    #[test]
    fn rejects_paths_that_are_not_canonical() {
        let deep = format!("m{}", "/0".repeat(256));
        for text in [
            "",
            "44'/60'",
            "M/44'",
            "m/",
            "m//0",
            "m/-1",
            "m/2147483648",
            "m/01",
            "m/0''",
            "m/0x10",
            "m/ 0",
            "m/0/",
            &deep,
        ] {
            assert_eq!(text.parse::<DerivationPath>(), Err(PathError), "{text:?}");
        }
    }

    #[test]
    fn knows_when_every_step_is_hardened() {
        assert!(path("m/44'/607'/0'").is_hardened());
        assert!(!path("m/44'/60'/0'/0/0").is_hardened());
        assert!(path("m").is_hardened());
    }
}
