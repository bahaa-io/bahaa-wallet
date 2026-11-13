use std::sync::LazyLock;

use alloy_primitives::keccak256;
use bahaa_chain::{ChainError, abi, http};
use bahaa_types::to_plain_text;
use ens_normalize_rs::EnsNameNormalizer;
use futures_util::future::join_all;
use reqwest::{Client, Url};
use ruint::aliases::U256;
use serde_json::{Value, json};

use super::calls::bytes_of;
use super::{EvmChain, hex};

pub(crate) const UNIVERSAL: [u8; 20] = [
    0xee, 0xee, 0xee, 0xee, 0x14, 0xd7, 0x18, 0xc2, 0xb4, 0x7d, 0x99, 0x23, 0xde, 0xab, 0x13, 0x35,
    0xe1, 0x44, 0xee, 0xee,
];
const RESOLVE: &str = "resolve(bytes,bytes)";
const ADDRESS: &str = "addr(bytes32,uint256)";
const OFFCHAIN_LOOKUP: &str = "OffchainLookup(address,string[],bytes,bytes4,bytes)";
const BATCH_QUERY: &str = "query((address,string[],bytes)[])";
const HTTP_ERROR: &str = "HttpError(uint16,string)";
const ERROR: &str = "Error(string)";
const NOT_FOUND: [&str; 4] = [
    "ResolverNotFound(bytes)",
    "ResolverNotContract(bytes,address)",
    "UnsupportedResolverProfile(bytes4)",
    "ResolverError(bytes)",
];
const LOCAL_BATCH: &str = "x-batch-gateway:true";
const ROUNDS: usize = 4;
const MAX_URLS: usize = 8;
const MAX_BATCH: usize = 16;
const MAX_LABEL: usize = 255;
const MAX_REASON: usize = 120;
const MAINNET: u64 = 1;

struct Request {
    sender: [u8; 20],
    urls: Vec<String>,
    data: Vec<u8>,
}

struct Lookup {
    request: Request,
    callback: [u8; 4],
    extra: Vec<u8>,
}

enum Fetch {
    Http(u16, String),
    Other(String),
}

impl EvmChain {
    pub async fn ens_address(&self, name: &str, coin: u64) -> Result<Option<Vec<u8>>, ChainError> {
        if self.chain_id != MAINNET {
            return Err(ChainError::Unsupported("ENS names outside Ethereum"));
        }
        let name = normalize(name)?;
        let mut query = selector(ADDRESS).to_vec();
        query.extend_from_slice(&namehash(&name));
        abi::push_uint(&mut query, U256::from(coin));
        let mut call = selector(RESOLVE).to_vec();
        call.extend(abi::encode_bytes(&[&dns_encoded(&name)?, &query]));
        let Some(reply) = self.read_offchain(&UNIVERSAL, call).await? else {
            return Ok(None);
        };
        let address = abi::bytes_at(&reply, 0)
            .and_then(|result| abi::bytes_at(result, 0))
            .ok_or_else(ChainError::malformed)?;
        Ok((!address.is_empty()).then(|| address.to_vec()))
    }

    async fn read_offchain(
        &self,
        to: &[u8; 20],
        mut data: Vec<u8>,
    ) -> Result<Option<Vec<u8>>, ChainError> {
        let client = http::files()?;
        for _ in 0..ROUNDS {
            let answer = self
                .rpc
                .answer(
                    "eth_call",
                    json!([{"to": hex(to), "data": hex(&data)}, "latest"]),
                )
                .await?;
            let error = match answer {
                Ok(result) => return Ok(Some(bytes_of(&result)?)),
                Err(error) => error,
            };
            let revert = reverted(&error).ok_or_else(|| {
                ChainError::Rejected(to_plain_text(
                    error["message"]
                        .as_str()
                        .unwrap_or("the name lookup failed"),
                    MAX_REASON,
                ))
            })?;
            if NOT_FOUND
                .iter()
                .any(|signature| revert.starts_with(&selector(signature)))
            {
                return Ok(None);
            }
            if let Some((status, _)) = read_http_error(&revert) {
                return Err(ChainError::Network(format!(
                    "the name's server answered HTTP {status}"
                )));
            }
            let lookup = Lookup::read(&revert).ok_or(ChainError::Invalid(
                "the name's resolver answered in a way this wallet can't read",
            ))?;
            if lookup.request.sender != *to {
                return Err(ChainError::Invalid(
                    "the name's resolver asked for a lookup on another contract",
                ));
            }
            let response = gateway(&client, &lookup).await?;
            data = lookup.callback.to_vec();
            data.extend(abi::encode_bytes(&[&response, &lookup.extra]));
        }
        Err(ChainError::Network(
            "the name needed too many lookups".to_owned(),
        ))
    }
}

pub fn normalize(name: &str) -> Result<String, ChainError> {
    static NORMALIZER: LazyLock<EnsNameNormalizer> = LazyLock::new(EnsNameNormalizer::default);
    let normalized = NORMALIZER
        .normalize(name)
        .map_err(|_| ChainError::Invalid("this is not a valid name"))?;
    if normalized.is_empty() || !normalized.contains('.') {
        return Err(ChainError::Invalid("this is not a valid name"));
    }
    Ok(normalized)
}

fn dns_encoded(name: &str) -> Result<Vec<u8>, ChainError> {
    let mut encoded = Vec::with_capacity(name.len() + 2);
    for label in name.split('.') {
        let length = u8::try_from(label.len())
            .ok()
            .filter(|length| *length > 0 && usize::from(*length) <= MAX_LABEL)
            .ok_or(ChainError::Invalid("this is not a valid name"))?;
        encoded.push(length);
        encoded.extend_from_slice(label.as_bytes());
    }
    encoded.push(0);
    Ok(encoded)
}

fn namehash(name: &str) -> [u8; 32] {
    name.rsplit('.').fold([0; 32], |node, label| {
        let mut joined = node.to_vec();
        joined.extend_from_slice(keccak256(label.as_bytes()).as_slice());
        keccak256(&joined).0
    })
}

fn selector(signature: &str) -> [u8; 4] {
    let hash = keccak256(signature.as_bytes());
    [hash[0], hash[1], hash[2], hash[3]]
}

fn reverted(error: &Value) -> Option<Vec<u8>> {
    let data = match &error["data"] {
        Value::String(_) => &error["data"],
        Value::Object(inner) => inner.get("data")?,
        _ => return None,
    };
    bytes_of(data).ok().filter(|bytes| bytes.len() >= 4)
}

impl Request {
    fn read(fields: &[u8]) -> Option<Self> {
        let urls = abi::bytes_list(fields, 1)?
            .into_iter()
            .map(|url| String::from_utf8(url.to_vec()).ok())
            .collect::<Option<Vec<String>>>()?;
        Some(Self {
            sender: abi::address(fields, 0)?,
            urls,
            data: abi::bytes_at(fields, 2)?.to_vec(),
        })
    }
}

impl Lookup {
    fn read(revert: &[u8]) -> Option<Self> {
        let args = revert.strip_prefix(&selector(OFFCHAIN_LOOKUP))?;
        let callback = abi::uint(args, 3)?.to_be_bytes::<32>();
        if callback[4..].iter().any(|byte| *byte != 0) {
            return None;
        }
        Some(Self {
            request: Request::read(args)?,
            callback: callback[..4].try_into().ok()?,
            extra: abi::bytes_at(args, 4)?.to_vec(),
        })
    }
}

async fn gateway(client: &Client, lookup: &Lookup) -> Result<Vec<u8>, ChainError> {
    let request = &lookup.request;
    let local = request.urls.iter().any(|url| url == LOCAL_BATCH);
    if local && let Some(query) = request.data.strip_prefix(&selector(BATCH_QUERY)) {
        let requests = batch(query).ok_or(ChainError::Invalid(
            "the name's resolver sent a batch this wallet can't read",
        ))?;
        let answers = join_all(requests.iter().map(|request| fetch(client, request))).await;
        return Ok(batch_answer(answers));
    }
    fetch(client, request).await.map_err(|failed| match failed {
        Fetch::Http(status, _) => {
            ChainError::Network(format!("the name's server answered HTTP {status}"))
        }
        Fetch::Other(reason) => ChainError::Network(reason),
    })
}

fn batch(query: &[u8]) -> Option<Vec<Request>> {
    let list = abi::tail(query, 0)?;
    let count = usize::try_from(abi::uint(list, 0)?).ok()?;
    if count > MAX_BATCH {
        return None;
    }
    let body = list.get(32..)?;
    (0..count)
        .map(|index| Request::read(abi::tail(body, index)?))
        .collect()
}

fn batch_answer(answers: Vec<Result<Vec<u8>, Fetch>>) -> Vec<u8> {
    let mut failures = Vec::new();
    abi::push_uint(&mut failures, U256::from(answers.len()));
    let mut responses = Vec::with_capacity(answers.len());
    for answer in answers {
        let (failed, response) = match answer {
            Ok(response) => (false, response),
            Err(Fetch::Http(status, message)) => (true, http_error(status, &message)),
            Err(Fetch::Other(message)) => (true, error_text(&message)),
        };
        abi::push_uint(&mut failures, U256::from(u8::from(failed)));
        responses.push(response);
    }
    let mut listed = Vec::new();
    abi::push_bytes_list(&mut listed, &responses);
    let mut encoded = Vec::new();
    abi::push_uint(&mut encoded, U256::from(64u8));
    abi::push_uint(&mut encoded, U256::from(64 + failures.len()));
    encoded.extend(failures);
    encoded.extend(listed);
    encoded
}

fn http_error(status: u16, message: &str) -> Vec<u8> {
    let mut encoded = selector(HTTP_ERROR).to_vec();
    abi::push_uint(&mut encoded, U256::from(status));
    let text = abi::encode_bytes(&[message.as_bytes()]);
    abi::push_uint(&mut encoded, U256::from(64u8));
    encoded.extend_from_slice(&text[32..]);
    encoded
}

fn error_text(message: &str) -> Vec<u8> {
    let mut encoded = selector(ERROR).to_vec();
    encoded.extend(abi::encode_bytes(&[message.as_bytes()]));
    encoded
}

fn read_http_error(revert: &[u8]) -> Option<(u16, String)> {
    let args = revert.strip_prefix(&selector(HTTP_ERROR))?;
    let status = u16::try_from(abi::uint(args, 0)?).ok()?;
    let message = String::from_utf8(abi::bytes_at(args, 1)?.to_vec()).ok()?;
    Some((status, message))
}

async fn fetch(client: &Client, request: &Request) -> Result<Vec<u8>, Fetch> {
    let (sender, data) = (hex(&request.sender), hex(&request.data));
    let mut last = Fetch::Other("no server answered for this name".to_owned());
    for template in request.urls.iter().take(MAX_URLS) {
        let Ok(url) = Url::parse(
            &template
                .replace("{sender}", &sender)
                .replace("{data}", &data),
        ) else {
            continue;
        };
        if url.scheme() != "https" {
            continue;
        }
        let request = if template.contains("{data}") {
            client.get(url)
        } else {
            client
                .post(url)
                .json(&json!({"data": data, "sender": sender}))
        };
        match http::exchange(request).await {
            Ok((status, body)) if status.is_success() => {
                return serde_json::from_slice::<Value>(&body)
                    .ok()
                    .and_then(|reply| bytes_of(&reply["data"]).ok())
                    .ok_or_else(|| Fetch::Other("the name's server sent no data".to_owned()));
            }
            Ok((status, body)) if status.is_client_error() => {
                return Err(Fetch::Http(status.as_u16(), message(&body)));
            }
            Ok((status, body)) => last = Fetch::Http(status.as_u16(), message(&body)),
            Err(_) => last = Fetch::Other("the name's server can't be reached".to_owned()),
        }
    }
    Err(last)
}

fn message(body: &[u8]) -> String {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|reply| reply["message"].as_str().map(str::to_owned))
        .map(|text| to_plain_text(&text, MAX_REASON))
        .unwrap_or_default()
}

#[cfg(fuzzing)]
pub(crate) fn fuzz(data: &[u8]) {
    let _ = Lookup::read(data);
    let _ = batch(data);
    let _ = read_http_error(data);
    if let Ok(text) = std::str::from_utf8(data)
        && let Ok(name) = normalize(text)
    {
        let _ = (dns_encoded(&name), namehash(&name));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(value: usize) -> Vec<u8> {
        let mut word = Vec::new();
        abi::push_uint(&mut word, U256::from(value));
        word
    }

    fn padded(bytes: &[u8]) -> Vec<u8> {
        let mut out = word(bytes.len());
        out.extend_from_slice(bytes);
        out.resize(32 + bytes.len().div_ceil(32) * 32, 0);
        out
    }

    fn strings(items: &[&str]) -> Vec<u8> {
        let mut list = word(items.len());
        let mut tails = Vec::new();
        for item in items {
            list.extend(word(items.len() * 32 + tails.len()));
            tails.extend(padded(item.as_bytes()));
        }
        list.extend(tails);
        list
    }

    fn request(sender: [u8; 20], urls: &[&str], data: &[u8]) -> Vec<u8> {
        let urls = strings(urls);
        let mut out = Vec::new();
        abi::push_address(&mut out, &sender);
        out.extend(word(96));
        out.extend(word(96 + urls.len()));
        out.extend(urls);
        out.extend(padded(data));
        out
    }

    fn lookup(
        sender: [u8; 20],
        urls: &[&str],
        data: &[u8],
        callback: [u8; 4],
        extra: &[u8],
    ) -> Vec<u8> {
        let (urls, data) = (strings(urls), padded(data));
        let mut out = selector(OFFCHAIN_LOOKUP).to_vec();
        abi::push_address(&mut out, &sender);
        out.extend(word(160));
        out.extend(word(160 + urls.len()));
        let mut callback_word = callback.to_vec();
        callback_word.resize(32, 0);
        out.extend(callback_word);
        out.extend(word(160 + urls.len() + data.len()));
        out.extend(urls);
        out.extend(data);
        out.extend(padded(extra));
        out
    }

    #[test]
    fn selectors_and_name_hashes_match_the_published_ones() {
        let hex = |bytes: &[u8]| bahaa_types::hex::encode(bytes);
        assert_eq!(hex(&selector(RESOLVE)), "9061b923");
        assert_eq!(hex(&selector(ADDRESS)), "f1cb7e06");
        assert_eq!(hex(&selector(OFFCHAIN_LOOKUP)), "556f1830");
        assert_eq!(hex(&selector(BATCH_QUERY)), "a780bab6");
        assert_eq!(hex(&selector(HTTP_ERROR)), "01800152");
        assert_eq!(hex(&selector(ERROR)), "08c379a0");
        assert_eq!(
            hex(&namehash("vitalik.eth")),
            "ee6c4522aab0003e8d14cd40a6af439055fd2577951148c14b6cea9a53475835"
        );
        assert_eq!(
            hex(&dns_encoded("my.name.eth").unwrap()),
            "026d79046e616d650365746800"
        );
        assert!(dns_encoded("a..eth").is_err());
    }

    #[test]
    fn names_are_normalized_the_ens_way() {
        assert_eq!(normalize("VITALIK.eth").unwrap(), "vitalik.eth");
        assert_eq!(normalize("jesse.base.eth").unwrap(), "jesse.base.eth");
        for bad in ["vitalik", "vit alik.eth", "", ".eth"] {
            assert!(normalize(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn an_offchain_lookup_and_its_batch_read_back_exactly() {
        let inner = request(
            [2; 20],
            &["https://gateway.example/{sender}/{data}.json"],
            &[7, 7],
        );
        let mut query = selector(BATCH_QUERY).to_vec();
        query.extend(word(32));
        query.extend(word(1));
        query.extend(word(32));
        query.extend(inner);
        let revert = lookup(
            UNIVERSAL,
            &[LOCAL_BATCH, "https://ccip-v3.ens.xyz"],
            &query,
            [1, 2, 3, 4],
            &[9; 40],
        );
        let read = Lookup::read(&revert).unwrap();
        assert_eq!(read.request.sender, UNIVERSAL);
        assert_eq!(read.request.urls, [LOCAL_BATCH, "https://ccip-v3.ens.xyz"]);
        assert_eq!(read.callback, [1, 2, 3, 4]);
        assert_eq!(read.extra, [9; 40]);
        let requests = batch(
            read.request
                .data
                .strip_prefix(&selector(BATCH_QUERY))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].sender, [2; 20]);
        assert_eq!(
            requests[0].urls,
            ["https://gateway.example/{sender}/{data}.json"]
        );
        assert_eq!(requests[0].data, [7, 7]);
        let mut odd = revert.clone();
        odd[4 + 3 * 32 + 5] = 1;
        assert!(Lookup::read(&odd).is_none());
        assert!(Lookup::read(&revert[..100]).is_none());
    }

    #[test]
    fn batch_answers_carry_each_failure_and_response() {
        let encoded = batch_answer(vec![
            Ok(vec![5; 33]),
            Err(Fetch::Http(404, "no such name".to_owned())),
            Err(Fetch::Other("unreachable".to_owned())),
        ]);
        let failures = abi::tail(&encoded, 0).unwrap();
        assert_eq!(abi::uint(failures, 0), Some(U256::from(3u8)));
        assert_eq!(
            (1..=3)
                .map(|index| abi::uint(failures, index).unwrap())
                .collect::<Vec<_>>(),
            [U256::ZERO, U256::from(1u8), U256::from(1u8)]
        );
        let responses = abi::bytes_list(&encoded, 1).unwrap();
        assert_eq!(responses[0], [5; 33]);
        assert_eq!(
            read_http_error(responses[1]),
            Some((404, "no such name".to_owned()))
        );
        assert!(responses[2].starts_with(&selector(ERROR)));
        assert_eq!(
            abi::bytes_at(&responses[2][4..], 0),
            Some(&b"unreachable"[..])
        );
    }

    #[test]
    fn revert_data_is_read_from_both_error_shapes() {
        let flat = json!({"code": 3, "message": "execution reverted", "data": "0x556f1830"});
        let nested = json!({"code": -32000, "data": {"data": "0x556f1830aa"}});
        assert_eq!(reverted(&flat), Some(vec![0x55, 0x6f, 0x18, 0x30]));
        assert_eq!(reverted(&nested), Some(vec![0x55, 0x6f, 0x18, 0x30, 0xaa]));
        assert_eq!(reverted(&json!({"code": 3, "data": "0x12"})), None);
        assert_eq!(reverted(&json!({"code": 3})), None);
    }
}
