use bahaa_types::to_plain_text;
use reqwest::Client;
use serde_json::Value;
use url::{Host, Url};

use crate::ConnectError;

const MAX_MANIFEST: usize = 64 * 1024;
const MAX_NAME: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub url: Url,
    pub domain: String,
    pub name: String,
    pub icon: Option<Url>,
}

pub async fn fetch_manifest(client: &Client, url: &Url) -> Result<Manifest, ConnectError> {
    let bytes = bahaa_chain::http::file(client, url.clone(), MAX_MANIFEST)
        .await
        .map_err(|_| ConnectError::Manifest("the app's details can't be downloaded"))?;
    read_manifest(&bytes)
}

pub fn read_manifest(bytes: &[u8]) -> Result<Manifest, ConnectError> {
    let broken = ConnectError::Manifest("the app's details can't be read");
    let value: Value = serde_json::from_slice(bytes).map_err(|_| broken.clone())?;
    let text = |name: &str| value.get(name).and_then(Value::as_str);
    let url = text("url")
        .and_then(|url| Url::parse(url.trim()).ok())
        .filter(|url| matches!(url.scheme(), "https" | "http"))
        .ok_or_else(|| broken.clone())?;
    let domain = domain_of(&url)?;
    let name = to_plain_text(text("name").unwrap_or_default(), MAX_NAME);
    if name.is_empty() {
        return Err(broken);
    }
    let icon = text("iconUrl")
        .and_then(|icon| Url::parse(icon.trim()).ok())
        .filter(|icon| icon.scheme() == "https");
    Ok(Manifest {
        url,
        domain,
        name,
        icon,
    })
}

fn domain_of(url: &Url) -> Result<String, ConnectError> {
    let Some(Host::Domain(host)) = url.host() else {
        return Err(ConnectError::Manifest("the app has no proper web address"));
    };
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 || labels.iter().any(|label| label.is_empty()) {
        return Err(ConnectError::Manifest("the app has no proper web address"));
    }
    Ok(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_spec_example_and_cleans_the_name() {
        let manifest = read_manifest(concat!(r#"{"url":"https://example.com","name":"Example"#, "\\u202e", r#" App","iconUrl":"https://example.com/icon-180.png","termsOfUseUrl":"https://example.com/terms"}"#).as_bytes()).unwrap();
        assert_eq!(manifest.domain, "example.com");
        assert_eq!(manifest.name, "Example App");
        assert_eq!(
            manifest.icon.unwrap().as_str(),
            "https://example.com/icon-180.png"
        );
        let port = read_manifest(br#"{"url":"https://app.example.org:8443/","name":"x","iconUrl":"http://example.org/i.png"}"#).unwrap();
        assert_eq!(port.domain, "app.example.org:8443");
        assert_eq!(port.icon, None);
    }

    #[test]
    fn refuses_apps_without_a_dotted_domain_or_a_name() {
        for body in [
            r#"{"url":"https://tonkeeper","name":"x"}"#,
            r#"{"url":"https://127.0.0.1","name":"x"}"#,
            r#"{"url":"https://example..com","name":"x"}"#,
            r#"{"url":"ftp://example.com","name":"x"}"#,
            r#"{"url":"https://example.com","name":"   "}"#,
            r#"{"name":"x"}"#,
            "<html>",
        ] {
            assert!(read_manifest(body.as_bytes()).is_err(), "{body}");
        }
    }
}
