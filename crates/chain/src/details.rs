use bahaa_types::{
    Details, MAX_NFT_ABOUT, MAX_NFT_LINK, MAX_NFT_TEXT, is_plain_text, to_plain_text,
};
use base64::Engine as _;
use base64::alphabet::STANDARD;
use base64::engine::DecodePaddingMode;
use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig};
use percent_encoding::percent_decode_str;
use reqwest::{Client, Url};
use serde_json::Value;

use crate::{ChainError, http};

pub const MAX_DETAILS_FILE: usize = 256 * 1024;
const MIN_ID: usize = 43;
const MAX_ID: usize = 128;
const BOM: &[u8] = b"\xEF\xBB\xBF";
const IMAGES: [&str; 4] = ["https://", "ipfs://", "ar://", "data:image/"];
const BASE64: GeneralPurpose = GeneralPurpose::new(
    &STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gateways {
    pub ipfs: Vec<Url>,
    pub arweave: Vec<Url>,
}

impl Gateways {
    pub fn builtin() -> Result<Self, url::ParseError> {
        Ok(Self {
            ipfs: vec![
                Url::parse("https://ipfs.filebase.io/ipfs/")?,
                Url::parse("https://gateway.pinata.cloud/ipfs/")?,
            ],
            arweave: vec![Url::parse("https://arweave.net/")?],
        })
    }
}

#[derive(Clone, Debug)]
pub struct Fetcher {
    client: Client,
    gateways: Gateways,
}

impl Fetcher {
    pub fn new(gateways: Gateways) -> Result<Self, ChainError> {
        Ok(Self {
            client: http::files()?,
            gateways,
        })
    }

    pub async fn details(&self, link: &str) -> Result<Details, ChainError> {
        let unreadable = || ChainError::Invalid("the NFT's details file is not readable");
        let urls = match locate(link, &self.gateways) {
            Some(Located::Inline(bytes)) => return read_details(&bytes).ok_or_else(unreadable),
            Some(Located::Web(urls)) => urls,
            None => return Err(ChainError::Unsupported("this kind of NFT details link")),
        };
        let mut failed = ChainError::Unsupported("NFT details without a gateway");
        for url in urls {
            match http::file(&self.client, url, MAX_DETAILS_FILE).await {
                Ok(bytes) => return read_details(&bytes).ok_or_else(unreadable),
                Err(err) => failed = err,
            }
        }
        Err(failed)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Located {
    Web(Vec<Url>),
    Inline(Vec<u8>),
}

pub(crate) fn locate(link: &str, gateways: &Gateways) -> Option<Located> {
    if let Some(rest) = link.strip_prefix("ipfs://") {
        let rest = rest.strip_prefix("ipfs/").unwrap_or(rest);
        return stored(&gateways.ipfs, rest).map(Located::Web);
    }
    if let Some(rest) = link.strip_prefix("ar://") {
        return stored(&gateways.arweave, rest).map(Located::Web);
    }
    if let Some(rest) = link.strip_prefix("data:") {
        return inline(rest).map(Located::Inline);
    }
    let url = Url::parse(link)
        .ok()
        .filter(|url| url.scheme() == "https")?;
    let gateway_path = url
        .path()
        .strip_prefix("/ipfs/")
        .filter(|_| url.query().is_none() && url.fragment().is_none());
    if let Some(urls) = gateway_path.and_then(|rest| stored(&gateways.ipfs, rest)) {
        return Some(Located::Web(urls));
    }
    Some(Located::Web(vec![url]))
}

fn stored(bases: &[Url], rest: &str) -> Option<Vec<Url>> {
    let (id, path) = rest.split_once('/').unwrap_or((rest, ""));
    let id_fine = (MIN_ID..=MAX_ID).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
    let path_fine = !path.contains(['?', '#', '\\'])
        && path
            .split('/')
            .all(|segment| segment != "." && segment != "..");
    if !id_fine || !path_fine {
        return None;
    }
    let urls: Vec<Url> = bases
        .iter()
        .filter_map(|base| Url::parse(&format!("{base}{rest}")).ok())
        .collect();
    (!urls.is_empty()).then_some(urls)
}

fn inline(rest: &str) -> Option<Vec<u8>> {
    let (kind, body) = rest.split_once(',')?;
    let mut parts = kind.split(';');
    let media = parts.next()?.trim();
    if !(media.is_empty()
        || media.eq_ignore_ascii_case("application/json")
        || media.eq_ignore_ascii_case("text/plain"))
    {
        return None;
    }
    if parts.any(|part| part.trim().eq_ignore_ascii_case("base64")) {
        BASE64.decode(body).ok()
    } else {
        Some(percent_decode_str(body).collect())
    }
}

pub fn read_details(bytes: &[u8]) -> Option<Details> {
    let value: Value = serde_json::from_slice(bytes.strip_prefix(BOM).unwrap_or(bytes)).ok()?;
    let object = value.as_object()?;
    let text = |key: &str, max: usize| {
        object
            .get(key)
            .and_then(Value::as_str)
            .map(|text| to_plain_text(text, max))
            .filter(|text| !text.is_empty())
    };
    let image = ["image", "image_url"]
        .into_iter()
        .find_map(|key| object.get(key).and_then(Value::as_str))
        .filter(|link| {
            is_plain_text(link, MAX_NFT_LINK) && IMAGES.iter().any(|start| link.starts_with(start))
        })
        .map(str::to_owned);
    Some(Details {
        name: text("name", MAX_NFT_TEXT),
        about: text("description", MAX_NFT_ABOUT),
        image,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CID: &str = "QmeSjSinHpPnmXmspMjwiXyN6zS4E9zccariGR3jxcaWtq";
    const AR: &str = "Jbn9nHCxRAOnmGW3eK0Uv3D-Q1-tkAbY-jlDQGrLc2Y";

    fn gateways() -> Gateways {
        Gateways {
            ipfs: vec![
                Url::parse("https://one.example/ipfs/").unwrap(),
                Url::parse("https://two.example/ipfs/").unwrap(),
            ],
            arweave: vec![Url::parse("https://ar.example/").unwrap()],
        }
    }

    fn web(link: &str) -> Option<Vec<String>> {
        match locate(link, &gateways())? {
            Located::Web(urls) => Some(urls.iter().map(ToString::to_string).collect()),
            Located::Inline(_) => None,
        }
    }

    #[test]
    fn content_links_go_through_the_gateways_in_order() {
        let both = |path: &str| {
            Some(vec![
                format!("https://one.example/ipfs/{path}"),
                format!("https://two.example/ipfs/{path}"),
            ])
        };
        assert_eq!(web(&format!("ipfs://{CID}/1")), both(&format!("{CID}/1")));
        assert_eq!(web(&format!("ipfs://ipfs/{CID}")), both(CID));
        assert_eq!(
            web(&format!("https://ipfs.io/ipfs/{CID}/7.json")),
            both(&format!("{CID}/7.json"))
        );
        assert_eq!(
            web(&format!("ar://{AR}")),
            Some(vec![format!("https://ar.example/{AR}")])
        );
        assert_eq!(
            web("https://nft.fragment.com/username/durov.json"),
            Some(vec![
                "https://nft.fragment.com/username/durov.json".to_owned()
            ])
        );
        assert_eq!(
            web(&format!("https://ipfs.io/ipfs/{CID}/1?x=1")),
            Some(vec![format!("https://ipfs.io/ipfs/{CID}/1?x=1")])
        );
    }

    #[test]
    fn strange_or_plain_links_are_refused() {
        for link in [
            "http://example.org/1.json".to_owned(),
            "javascript:alert(1)".to_owned(),
            "ftp://example.org/1".to_owned(),
            "ipfs://short/1".to_owned(),
            format!("ipfs://{CID}/../../etc"),
            format!("ipfs://{CID}/a?b"),
            format!("ipfs://{CID}%2F/1"),
            format!("ipfs://{CID}/a\\b"),
            "data:image/svg+xml;base64,PHN2Zz4=".to_owned(),
            "data:application/json;base64".to_owned(),
            String::new(),
        ] {
            assert_eq!(locate(&link, &gateways()), None, "{link}");
        }
    }

    #[test]
    fn inline_files_are_read_without_any_request() {
        let json = r#"{"name":"Noun 1"}"#;
        for link in [
            format!("data:application/json;base64,{}", BASE64.encode(json)),
            format!(
                "data:application/json;charset=utf-8;base64,{}",
                BASE64.encode(json).trim_end_matches('=')
            ),
            format!("data:application/json,{json}"),
            "data:application/json;utf8,%7B%22name%22%3A%22Noun%201%22%7D".to_owned(),
            format!("data:,{json}"),
        ] {
            assert_eq!(
                locate(&link, &gateways()),
                Some(Located::Inline(json.as_bytes().to_vec())),
                "{link}"
            );
        }
    }

    #[test]
    fn details_keep_only_clean_texts_and_image_links() {
        let file = br#"{"name":"Ape\u202e #1\n","description":"A  long\tstory","image":"ipfs://QmPbxeGcXhYQQNgsC6a36dDyYUcHgMLnGKnF8pVFmGsvqi","attributes":[]}"#;
        assert_eq!(
            read_details(file),
            Some(Details {
                name: Some("Ape #1".to_owned()),
                about: Some("A long story".to_owned()),
                image: Some("ipfs://QmPbxeGcXhYQQNgsC6a36dDyYUcHgMLnGKnF8pVFmGsvqi".to_owned()),
            })
        );
        let bom = [BOM, br#"{"image_url":"https://x.example/1.png","name":7}"#].concat();
        assert_eq!(
            read_details(&bom),
            Some(Details {
                name: None,
                about: None,
                image: Some("https://x.example/1.png".to_owned()),
            })
        );
        let long = format!(
            r#"{{"name":"{}","description":"{}"}}"#,
            "n".repeat(500),
            "d".repeat(900)
        );
        let read = read_details(long.as_bytes()).unwrap();
        assert_eq!(read.name.unwrap().chars().count(), MAX_NFT_TEXT);
        assert_eq!(read.about.unwrap().chars().count(), MAX_NFT_ABOUT);
        for unsafe_image in [
            r#"{"image":"javascript:alert(1)"}"#,
            r#"{"image":"http://x.example/1.png"}"#,
            r#"{"image":"data:text/html,<b>hi</b>"}"#,
        ] {
            assert_eq!(read_details(unsafe_image.as_bytes()).unwrap().image, None);
        }
        for broken in [&b"[]"[..], b"\"name\"", b"{", b""] {
            assert_eq!(read_details(broken), None);
        }
    }
}
