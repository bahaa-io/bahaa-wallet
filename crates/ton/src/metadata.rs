use std::time::Duration;

use bahaa_chain::ChainError;
use bahaa_types::MAX_DECIMALS;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tycho_types::boc::Boc;
use tycho_types::cell::{Cell, CellSlice, HashBytes, Load};
use tycho_types::dict::Dict;
use url::Url;

const OFF_CHAIN: u8 = 0x01;
const ON_CHAIN: u8 = 0x00;
const SNAKE: u8 = 0x00;
const CHUNKS: u8 = 0x01;
const MAX_TEXT: usize = 1024;
const MAX_CELLS: usize = 64;
const MAX_FILE: usize = 64 * 1024;
const MAX_REDIRECTS: usize = 3;
const TIMEOUT: Duration = Duration::from_secs(15);
pub(crate) const DEFAULT_DECIMALS: u8 = 9;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Content {
    pub uri: Option<String>,
    pub name: Option<String>,
    pub symbol: Option<String>,
    pub decimals: Option<String>,
}

impl Content {
    pub(crate) fn complete(&self) -> bool {
        self.name.is_some() && self.symbol.is_some() && self.decimals.is_some()
    }

    pub(crate) fn fill(&mut self, other: Content) {
        self.name = self.name.take().or(other.name);
        self.symbol = self.symbol.take().or(other.symbol);
        self.decimals = self.decimals.take().or(other.decimals);
    }

    pub(crate) fn decimals(&self) -> Result<u8, ChainError> {
        let Some(text) = &self.decimals else {
            return Ok(DEFAULT_DECIMALS);
        };
        text.trim()
            .parse::<u8>()
            .ok()
            .filter(|decimals| *decimals <= MAX_DECIMALS)
            .ok_or(ChainError::Invalid(
                "this token reports impossible decimals",
            ))
    }
}

pub(crate) fn content(boc: &[u8]) -> Option<Content> {
    let cell = Boc::decode(boc).ok()?;
    let mut slice = cell.as_slice().ok()?;
    match slice.load_u8().ok()? {
        OFF_CHAIN => Some(Content {
            uri: Some(text(snake(slice)?)?),
            ..Content::default()
        }),
        ON_CHAIN => {
            let dict = Dict::<HashBytes, Cell>::load_from(&mut slice).ok()?;
            let field = |key: &str| -> Option<Option<String>> {
                let hash = HashBytes(Sha256::digest(key.as_bytes()).into());
                match dict.get(hash).ok()? {
                    None => Some(None),
                    Some(value) => Some(Some(text(data(&value)?)?)),
                }
            };
            Some(Content {
                uri: field("uri")?,
                name: field("name")?,
                symbol: field("symbol")?,
                decimals: field("decimals")?,
            })
        }
        _ => None,
    }
}

pub(crate) fn file(bytes: &[u8]) -> Option<Content> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let object = value.as_object()?;
    let field = |key: &str| -> Option<Option<String>> {
        match object.get(key) {
            None | Some(Value::Null) => Some(None),
            Some(Value::String(text)) => Some(Some(text.clone())),
            Some(Value::Number(number)) => Some(Some(number.to_string())),
            Some(_) => None,
        }
    };
    Some(Content {
        uri: None,
        name: field("name")?,
        symbol: field("symbol")?,
        decimals: field("decimals")?,
    })
}

pub(crate) async fn fetch(uri: &str) -> Result<Content, ChainError> {
    let url = Url::parse(uri)
        .ok()
        .filter(|url| url.scheme() == "https")
        .ok_or(ChainError::Unsupported(
            "token details kept outside an https address",
        ))?;
    let network = |text: String| ChainError::Network(text);
    let client = reqwest::Client::builder()
        .user_agent(concat!("bahaa-wallet/", env!("CARGO_PKG_VERSION")))
        .timeout(TIMEOUT)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.url().scheme() == "https" && attempt.previous().len() <= MAX_REDIRECTS {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .build()
        .map_err(|err| network(err.to_string()))?;
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|err| network(err.without_url().to_string()))?;
    if !response.status().is_success() {
        return Err(network(format!(
            "the token's details file answered HTTP {}",
            response.status()
        )));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|err| network(err.without_url().to_string()))?
    {
        if bytes.len() + chunk.len() > MAX_FILE {
            return Err(ChainError::Invalid("the token's details file is too large"));
        }
        bytes.extend_from_slice(&chunk);
    }
    file(&bytes).ok_or(ChainError::Invalid(
        "the token's details file is not readable",
    ))
}

fn data(value: &Cell) -> Option<Vec<u8>> {
    let mut slice = value.as_slice().ok()?;
    match slice.load_u8().ok()? {
        SNAKE => snake(slice),
        CHUNKS => {
            let chunks = Dict::<u32, Cell>::load_from(&mut slice).ok()?;
            let mut out = Vec::new();
            for (at, entry) in chunks.iter().enumerate() {
                let (index, chunk) = entry.ok()?;
                if usize::try_from(index).ok()? != at || at >= MAX_CELLS {
                    return None;
                }
                let chunk = chunk.as_slice().ok()?;
                if chunk.size_refs() != 0 {
                    return None;
                }
                bytes(chunk, &mut out)?;
            }
            Some(out)
        }
        _ => None,
    }
}

fn snake(first: CellSlice<'_>) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut slice = first;
    for _ in 0..MAX_CELLS {
        bytes(slice, &mut out)?;
        match slice.size_refs() {
            0 => return Some(out),
            1 => slice = slice.get_reference(0).ok()?.as_slice().ok()?,
            _ => return None,
        }
    }
    None
}

fn bytes(mut slice: CellSlice<'_>, out: &mut Vec<u8>) -> Option<()> {
    let bits = slice.size_bits();
    if !bits.is_multiple_of(8) {
        return None;
    }
    let mut buffer = [0u8; 128];
    let read = slice.load_raw(&mut buffer, bits).ok()?;
    if out.len() + read.len() > MAX_TEXT {
        return None;
    }
    out.extend_from_slice(read);
    Some(())
}

fn text(bytes: Vec<u8>) -> Option<String> {
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use tycho_types::cell::CellBuilder;

    use super::*;

    fn snaked(prefix: Option<u8>, text: &[u8]) -> Cell {
        let room = 127 - usize::from(prefix.is_some());
        let (head, rest) = text.split_at(text.len().min(room));
        let mut builder = CellBuilder::new();
        if let Some(prefix) = prefix {
            builder.store_u8(prefix).unwrap();
        }
        builder
            .store_raw(head, u16::try_from(head.len() * 8).unwrap())
            .unwrap();
        if !rest.is_empty() {
            builder.store_reference(snaked(None, rest)).unwrap();
        }
        builder.build().unwrap()
    }

    fn key(name: &str) -> HashBytes {
        HashBytes(Sha256::digest(name.as_bytes()).into())
    }

    fn on_chain(fields: &[(&str, Cell)]) -> Vec<u8> {
        let mut dict = Dict::<HashBytes, Cell>::new();
        for (name, value) in fields {
            dict.set(key(name), value.clone()).unwrap();
        }
        let mut builder = CellBuilder::new();
        builder.store_u8(ON_CHAIN).unwrap();
        match dict.root() {
            Some(root) => {
                builder.store_bit(true).unwrap();
                builder.store_reference(root.clone()).unwrap();
            }
            None => builder.store_bit(false).unwrap(),
        }
        Boc::encode(builder.build().unwrap())
    }

    fn value(text: &str) -> Cell {
        snaked(Some(SNAKE), text.as_bytes())
    }

    #[test]
    fn off_chain_content_names_its_file() {
        let uri = format!("https://example.org/{}.json", "a".repeat(200));
        let boc = Boc::encode(snaked(Some(OFF_CHAIN), uri.as_bytes()));
        assert_eq!(
            content(&boc),
            Some(Content {
                uri: Some(uri),
                ..Content::default()
            })
        );
    }

    #[test]
    fn on_chain_fields_are_read_by_their_hashed_names() {
        let boc = on_chain(&[
            ("decimals", value("6")),
            ("uri", value("https://tether.to/usdt-ton.json")),
            ("image", value("https://tether.to/logo.png")),
        ]);
        let read = content(&boc).unwrap();
        assert_eq!(
            read,
            Content {
                uri: Some("https://tether.to/usdt-ton.json".to_owned()),
                name: None,
                symbol: None,
                decimals: Some("6".to_owned()),
            }
        );
        assert!(!read.complete());
        assert_eq!(read.decimals(), Ok(6));
        let long = "n".repeat(300);
        let full = content(&on_chain(&[
            ("name", value(&long)),
            ("symbol", value("STON")),
            ("decimals", value("9")),
        ]))
        .unwrap();
        assert!(full.complete());
        assert_eq!(full.name, Some(long));
    }

    #[test]
    fn chunked_values_are_joined_in_order() {
        let mut chunks = Dict::<u32, Cell>::new();
        chunks.set(0, snaked(None, b"Big ")).unwrap();
        chunks.set(1, snaked(None, b"Token")).unwrap();
        let mut builder = CellBuilder::new();
        builder.store_u8(CHUNKS).unwrap();
        builder.store_bit(true).unwrap();
        builder
            .store_reference(chunks.root().clone().unwrap())
            .unwrap();
        let read = content(&on_chain(&[("name", builder.build().unwrap())])).unwrap();
        assert_eq!(read.name.as_deref(), Some("Big Token"));
        let mut gap = Dict::<u32, Cell>::new();
        gap.set(0, snaked(None, b"Big ")).unwrap();
        gap.set(2, snaked(None, b"Token")).unwrap();
        let mut builder = CellBuilder::new();
        builder.store_u8(CHUNKS).unwrap();
        builder.store_bit(true).unwrap();
        builder
            .store_reference(gap.root().clone().unwrap())
            .unwrap();
        assert_eq!(
            content(&on_chain(&[("name", builder.build().unwrap())])),
            None
        );
    }

    #[test]
    fn hostile_or_broken_content_is_refused() {
        let mut odd = CellBuilder::new();
        odd.store_u8(SNAKE).unwrap();
        odd.store_bit(true).unwrap();
        let mut forked = CellBuilder::new();
        forked.store_u8(SNAKE).unwrap();
        forked.store_reference(value("a")).unwrap();
        forked.store_reference(value("b")).unwrap();
        let mut unknown = CellBuilder::new();
        unknown.store_u8(0x02).unwrap();
        let bad = [
            on_chain(&[("symbol", odd.build().unwrap())]),
            on_chain(&[("symbol", forked.build().unwrap())]),
            on_chain(&[("symbol", unknown.build().unwrap())]),
            on_chain(&[("symbol", snaked(Some(SNAKE), &[0xff, 0xfe]))]),
            on_chain(&[("name", value(&"x".repeat(MAX_TEXT + 1)))]),
            Boc::encode(snaked(Some(0x02), b"https://example.org")),
            vec![1, 2, 3],
        ];
        for (at, boc) in bad.iter().enumerate() {
            assert_eq!(content(boc), None, "case {at}");
        }
        let mut long_label = CellBuilder::new();
        long_label.store_bit(false).unwrap();
        long_label.store_ones(300).unwrap();
        long_label.store_bit(false).unwrap();
        let mut root = CellBuilder::new();
        root.store_u8(ON_CHAIN).unwrap();
        root.store_bit(true).unwrap();
        root.store_reference(long_label.build().unwrap()).unwrap();
        assert_eq!(content(&Boc::encode(root.build().unwrap())), None);
    }

    #[test]
    fn details_files_fill_only_what_is_missing() {
        let mut usdt = content(&on_chain(&[
            ("decimals", value("6")),
            ("uri", value("https://tether.to/usdt-ton.json")),
        ]))
        .unwrap();
        let details =
            file(br#"{"name": "Tether USD", "symbol": "USD", "decimals": "9", "image": 5}"#)
                .unwrap();
        usdt.fill(details);
        assert_eq!(
            (
                usdt.name.as_deref(),
                usdt.symbol.as_deref(),
                usdt.decimals()
            ),
            (Some("Tether USD"), Some("USD"), Ok(6))
        );
        assert_eq!(
            file(br#"{"symbol": "STON", "decimals": 9}"#)
                .unwrap()
                .decimals(),
            Ok(9)
        );
        assert_eq!(file(br#"{"symbol": "X"}"#).unwrap().decimals(), Ok(9));
        for broken in [&br#"{"symbol": ["X"]}"#[..], br#"["symbol"]"#, b"not json"] {
            assert_eq!(file(broken), None);
        }
        for impossible in ["78", "-1", "six", ""] {
            let content = Content {
                decimals: Some(impossible.to_owned()),
                ..Content::default()
            };
            assert!(content.decimals().is_err(), "{impossible}");
        }
    }
}
