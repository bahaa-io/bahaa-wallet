use serde::{Deserialize, Serialize};

use crate::ConfigError;

const LOCK_RANGE: std::ops::RangeInclusive<u16> = 1..=1440;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Fiat {
    #[default]
    Usd,
    Eur,
    Irt,
}

impl Fiat {
    pub fn code(self) -> &'static str {
        match self {
            Self::Usd => "USD",
            Self::Eur => "EUR",
            Self::Irt => "IRT",
        }
    }

    pub fn price_base(self) -> &'static str {
        match self {
            Self::Usd => "USDT",
            Self::Eur => "EUR",
            Self::Irt => "IRT",
        }
    }

    pub fn places(self) -> u8 {
        match self {
            Self::Usd | Self::Eur => 2,
            Self::Irt => 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub fiat: Fiat,
    pub developer_mode: bool,
    pub show_testnets: bool,
    pub lock_after_minutes: u16,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            fiat: Fiat::Usd,
            developer_mode: false,
            show_testnets: false,
            lock_after_minutes: 10,
        }
    }
}

impl Settings {
    pub fn from_json(text: &str) -> Result<Self, ConfigError> {
        let settings: Self = serde_json::from_str(text)
            .map_err(|err| ConfigError::Settings(format!("settings are not valid: {err}")))?;
        if !LOCK_RANGE.contains(&settings.lock_after_minutes) {
            return Err(ConfigError::Settings(
                "auto-lock must be between 1 minute and 24 hours".to_owned(),
            ));
        }
        Ok(settings)
    }

    pub fn to_json(&self) -> Result<String, ConfigError> {
        serde_json::to_string_pretty(self).map_err(|err| ConfigError::Settings(err.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_fields_take_safe_defaults() {
        let settings = Settings::from_json(r#"{"fiat": "irt"}"#).unwrap();
        assert_eq!(settings.fiat, Fiat::Irt);
        assert!(!settings.developer_mode);
        assert_eq!(settings.lock_after_minutes, 10);
        assert_eq!(Settings::from_json("{}").unwrap(), Settings::default());
    }

    #[test]
    fn round_trips_through_json() {
        let settings = Settings {
            fiat: Fiat::Eur,
            developer_mode: true,
            show_testnets: true,
            lock_after_minutes: 30,
        };
        assert_eq!(
            Settings::from_json(&settings.to_json().unwrap()).unwrap(),
            settings
        );
    }

    #[test]
    fn rejects_unsafe_or_unknown_values() {
        assert!(Settings::from_json(r#"{"lock_after_minutes": 0}"#).is_err());
        assert!(Settings::from_json(r#"{"lock_after_minutes": 1441}"#).is_err());
        assert!(Settings::from_json(r#"{"fiat": "btc"}"#).is_err());
        assert!(Settings::from_json("not json").is_err());
    }

    #[test]
    fn fiat_maps_to_price_bases() {
        assert_eq!((Fiat::Usd.price_base(), Fiat::Usd.places()), ("USDT", 2));
        assert_eq!((Fiat::Irt.price_base(), Fiat::Irt.places()), ("IRT", 0));
        assert_eq!(Fiat::Eur.code(), "EUR");
    }
}
