use reqwest::Client;
use serde_json::Value;
use url::Url;

use crate::ConnectError;

const DEFAULT_BRIDGE: &str = "https://connect.ton.org/bridge";
const WALLETS_LIST: &str =
    "https://raw.githubusercontent.com/ton-connect/wallets-list/main/wallets-v2.json";
const MAX_LIST: usize = 1024 * 1024;
const MAX_WALLETS: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bridges {
    pub default: Url,
    pub list: Option<Url>,
}

impl Bridges {
    pub fn builtin() -> Result<Self, ConnectError> {
        let parse =
            |text| Url::parse(text).map_err(|_| ConnectError::Invalid("bad bridge address"));
        Ok(Self {
            default: parse(DEFAULT_BRIDGE)?,
            list: Some(parse(WALLETS_LIST)?),
        })
    }

    pub async fn pick(&self, client: &Client, link: &str) -> Url {
        if link.starts_with("tc:") {
            return self.default.clone();
        }
        let Some(list) = &self.list else {
            return self.default.clone();
        };
        match bahaa_chain::http::file(client, list.clone(), MAX_LIST).await {
            Ok(bytes) => {
                bridge_for(&read_wallets(&bytes), link).unwrap_or_else(|| self.default.clone())
            }
            Err(_) => self.default.clone(),
        }
    }
}

pub fn read_wallets(bytes: &[u8]) -> Vec<(String, Url)> {
    let Ok(Value::Array(wallets)) = serde_json::from_slice::<Value>(bytes) else {
        return Vec::new();
    };
    wallets
        .iter()
        .take(MAX_WALLETS)
        .filter_map(|wallet| {
            let universal = wallet.get("universal_url")?.as_str()?;
            let bridge = wallet
                .get("bridge")?
                .as_array()?
                .iter()
                .find(|bridge| bridge.get("type").and_then(Value::as_str) == Some("sse"))?
                .get("url")?
                .as_str()?;
            let bridge = Url::parse(bridge)
                .ok()
                .filter(|url| url.scheme() == "https")?;
            Url::parse(universal)
                .ok()
                .filter(|url| url.scheme() == "https")?;
            Some((universal.to_owned(), bridge))
        })
        .collect()
}

pub fn bridge_for(wallets: &[(String, Url)], link: &str) -> Option<Url> {
    wallets
        .iter()
        .filter(|(universal, _)| {
            link.strip_prefix(universal.as_str())
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(['?', '&', '/', '#']))
        })
        .max_by_key(|(universal, _)| universal.len())
        .map(|(_, bridge)| bridge.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = r#"[
        {"app_name": "tonkeeper", "universal_url": "https://app.tonkeeper.com/ton-connect",
         "bridge": [{"type": "sse", "url": "https://bridge.tonapi.io/bridge"}, {"type": "js", "key": "tonkeeper"}]},
        {"app_name": "telegram-wallet", "universal_url": "https://t.me/wallet?attach=wallet",
         "bridge": [{"type": "sse", "url": "https://walletbot.me/tonconnect-bridge/bridge"}]},
        {"app_name": "plain", "universal_url": "https://t.me/wallet",
         "bridge": [{"type": "sse", "url": "http://insecure.example/bridge"}]},
        {"app_name": "broken", "bridge": []}
    ]"#;

    #[test]
    fn the_link_host_picks_that_wallets_bridge() {
        let wallets = read_wallets(LIST.as_bytes());
        assert_eq!(wallets.len(), 2);
        let pick = |link: &str| bridge_for(&wallets, link).map(|url| url.to_string());
        assert_eq!(
            pick("https://app.tonkeeper.com/ton-connect?v=2&id=ab").as_deref(),
            Some("https://bridge.tonapi.io/bridge")
        );
        assert_eq!(
            pick("https://t.me/wallet?attach=wallet&startattach=tonconnect-v__2").as_deref(),
            Some("https://walletbot.me/tonconnect-bridge/bridge")
        );
        assert_eq!(pick("https://app.tonkeeper.com/ton-connectx?v=2"), None);
        assert_eq!(pick("tc://?v=2"), None);
        assert_eq!(read_wallets(b"{}").len(), 0);
    }
}
