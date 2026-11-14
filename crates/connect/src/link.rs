use bahaa_types::hex;
use url::Url;

use crate::ConnectError;
use crate::request::ConnectRequest;

const VERSION: &str = "2";
const MAX_LINK: usize = 16 * 1024;
const MAX_TRACE: usize = 64;
const TELEGRAM: &str = "tonconnect-";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invitation {
    pub app: [u8; 32],
    pub request: ConnectRequest,
    pub link: String,
    pub trace: Option<String>,
}

pub fn read_link(text: &str) -> Result<Invitation, ConnectError> {
    let link = text.trim();
    if link.len() > MAX_LINK {
        return Err(ConnectError::Invalid("this link is too long"));
    }
    let url = Url::parse(link).map_err(|_| ConnectError::NotLink)?;
    let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    let attached = pairs
        .iter()
        .find(|(key, _)| key == "startattach")
        .map(|(_, value)| value.clone());
    let query = match attached {
        Some(value) => {
            let encoded = value.strip_prefix(TELEGRAM).ok_or(ConnectError::NotLink)?;
            let decoded = encoded
                .replace("--", "%")
                .replace("__", "=")
                .replace('-', "&");
            url::form_urlencoded::parse(decoded.as_bytes())
                .into_owned()
                .collect()
        }
        None => pairs,
    };
    let get = |name: &str| {
        query
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    };
    match get("v") {
        Some(VERSION) => {}
        Some(_) => {
            return Err(ConnectError::Unsupported(
                "this link uses a TON Connect version this wallet doesn't know",
            ));
        }
        None => return Err(ConnectError::NotLink),
    }
    let app = get("id")
        .and_then(hex::decode_array::<32>)
        .ok_or(ConnectError::Invalid("the link has no valid app id"))?;
    let request = ConnectRequest::read(
        get("r").ok_or(ConnectError::Invalid("the link has no connection request"))?,
    )?;
    let trace = get("trace_id")
        .filter(|trace| {
            trace.len() <= MAX_TRACE
                && trace
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
        })
        .map(str::to_owned);
    Ok(Invitation {
        app,
        request,
        link: link.to_owned(),
        trace,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::ConnectItem;

    const APP: &str = "c26eecc868c3878d73ac2ef3cb4a22b497d9f621c1b8eea54d74a99cc8ca7f5c";
    const REQUEST: &str = r#"{"manifestUrl":"https://app.example/tonconnect-manifest.json","items":[{"name":"ton_addr"},{"name":"ton_proof","payload":"abc"}]}"#;

    fn encoded() -> String {
        url::form_urlencoded::byte_serialize(REQUEST.as_bytes()).collect()
    }

    #[test]
    fn reads_unified_and_wallet_links() {
        for base in ["tc://", "https://app.tonkeeper.com/ton-connect"] {
            let link = format!(
                "{base}?v=2&id={APP}&r={}&ret=back&trace_id=01a1dc26-2690-71ca-afd1-194cf2fac2e4",
                encoded()
            );
            let invitation = read_link(&format!("  {link}\n")).unwrap();
            assert_eq!(hex::encode(&invitation.app), APP);
            assert_eq!(invitation.link, link);
            assert_eq!(
                invitation.trace.as_deref(),
                Some("01a1dc26-2690-71ca-afd1-194cf2fac2e4")
            );
            assert_eq!(
                invitation.request.items[1],
                ConnectItem::Proof {
                    payload: "abc".to_owned()
                }
            );
        }
    }

    #[test]
    fn reads_telegram_wallet_links() {
        let query = format!("v=2&id={APP}&r={}&ret=none", encoded());
        let telegram = query
            .replace('.', "%2E")
            .replace('-', "%2D")
            .replace('_', "%5F")
            .replace('&', "-")
            .replace('=', "__")
            .replace('%', "--");
        let link = format!("https://t.me/wallet?attach=wallet&startattach=tonconnect-{telegram}");
        let invitation = read_link(&link).unwrap();
        assert_eq!(
            invitation.request.manifest.as_str(),
            "https://app.example/tonconnect-manifest.json"
        );
    }

    #[test]
    fn refuses_other_links_and_versions() {
        assert_eq!(read_link("https://bahaa.io"), Err(ConnectError::NotLink));
        assert_eq!(read_link("not a link"), Err(ConnectError::NotLink));
        let old = format!("tc://?v=1&id={APP}&r={}", encoded());
        assert!(matches!(read_link(&old), Err(ConnectError::Unsupported(_))));
        let short = format!("tc://?v=2&id=abcd&r={}", encoded());
        assert!(matches!(read_link(&short), Err(ConnectError::Invalid(_))));
        assert!(matches!(
            read_link(&format!("tc://?v=2&id={APP}")),
            Err(ConnectError::Invalid(_))
        ));
    }
}
