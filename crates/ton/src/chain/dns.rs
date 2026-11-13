use bahaa_chain::ChainError;
use bahaa_types::Address;
use ruint::aliases::U256;
use sha2::{Digest, Sha256};
use tycho_types::boc::Boc;
use tycho_types::cell::Load;
use tycho_types::models::StdAddr;

use super::TonChain;
use crate::http::Item;
use crate::vm::Arg;

const WALLET: &str = "wallet";
const SMART_CONTRACT: u16 = 0x9fd3;
const NEXT_RESOLVER: u16 = 0xba93;
const MAX_HOPS: usize = 6;
const MAX_NAME: usize = 126;
const SHORT: usize = 126;

impl TonChain {
    pub async fn dns_address(&self, name: &str) -> Result<Option<Address>, ChainError> {
        let mut rest = encoded(name)?;
        let category = U256::from_be_bytes::<32>(Sha256::digest(WALLET.as_bytes()).into());
        let mut resolver = self.http.dns_root().await?;
        for _ in 0..MAX_HOPS {
            let asked = self
                .http
                .run(
                    &resolver,
                    "dnsresolve",
                    &[Arg::Bytes(&rest), Arg::Number(category)],
                )
                .await;
            let stack = match asked {
                Ok(Some(stack)) => stack,
                Ok(None) | Err(ChainError::Rejected(_)) => return Ok(None),
                Err(err) => return Err(err),
            };
            let [Item::Num(bits), record] = stack.as_slice() else {
                return Err(ChainError::malformed());
            };
            let bits = usize::try_from(*bits).map_err(|_| ChainError::malformed())?;
            if bits == 0 {
                return Ok(None);
            }
            if bits % 8 != 0 || bits > rest.len() * 8 {
                return Err(ChainError::Invalid(
                    "a TON name resolver split the name in a broken way",
                ));
            }
            let Item::Cell(boc) = record else {
                return Ok(None);
            };
            if bits == rest.len() * 8 {
                return Ok(read_record(boc, SMART_CONTRACT));
            }
            resolver = read_record(boc, NEXT_RESOLVER)
                .ok_or(ChainError::Invalid("a TON name resolver pointed nowhere"))?;
            rest.drain(..bits / 8);
        }
        Err(ChainError::Network(
            "the name needed too many lookups".to_owned(),
        ))
    }
}

pub fn normalize(name: &str) -> Result<String, ChainError> {
    let lower = name.trim().to_ascii_lowercase();
    let valid = !lower.is_empty()
        && lower.len() <= MAX_NAME
        && lower
            .split('.')
            .all(|label| !label.is_empty() && label.bytes().all(|byte| byte.is_ascii_graphic()));
    if valid {
        Ok(lower)
    } else {
        Err(ChainError::Invalid("this is not a valid TON name"))
    }
}

fn encoded(name: &str) -> Result<Vec<u8>, ChainError> {
    let name = normalize(name)?;
    let mut raw = Vec::with_capacity(name.len() + 2);
    for label in name.rsplit('.') {
        raw.extend_from_slice(label.as_bytes());
        raw.push(0);
    }
    if raw.len() < SHORT {
        raw.insert(0, 0);
    }
    Ok(raw)
}

fn read_record(boc: &[u8], tag: u16) -> Option<Address> {
    let cell = Boc::decode(boc).ok()?;
    let mut slice = cell.as_slice().ok()?;
    if slice.load_u16().ok()? != tag {
        return None;
    }
    let address = StdAddr::load_from(&mut slice).ok()?;
    Some(Address::Ton {
        workchain: address.workchain,
        hash: address.address.0,
    })
}

#[cfg(fuzzing)]
pub(crate) fn fuzz(data: &[u8]) {
    let _ = read_record(data, SMART_CONTRACT);
    let _ = read_record(data, NEXT_RESOLVER);
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = encoded(text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boc(text: &str) -> Vec<u8> {
        bahaa_types::hex::decode(text).unwrap()
    }

    #[test]
    fn names_are_sent_reversed_with_each_label_closed_by_a_zero() {
        assert_eq!(encoded("Alice.TON").unwrap(), b"\0ton\0alice\0");
        assert_eq!(encoded("durov.t.me").unwrap(), b"\0me\0t\0durov\0");
        assert_eq!(normalize(" Foundation.ton ").unwrap(), "foundation.ton");
        for bad in ["", "a..ton", "al ice.ton", "алиса.ton"] {
            assert!(normalize(bad).is_err(), "{bad}");
        }
        let long = format!("{}.ton", "a".repeat(130));
        assert!(encoded(&long).is_err());
    }

    #[test]
    fn real_records_read_as_their_addresses() {
        let next = boc(
            "b5ee9c72010101010026000047ba938016ee9b2bd640a87e30d80d66e357115ae09efc4ac26195f2c3126d14fa196d99f0",
        );
        assert_eq!(
            read_record(&next, NEXT_RESOLVER).unwrap().to_string(),
            "0:b774d95eb20543f186c06b371ab88ad704f7e256130caf96189368a7d0cb6ccf"
        );
        assert_eq!(read_record(&next, SMART_CONTRACT), None);
        let wallet = boc(
            "b5ee9c720101010100270000499fd380107bfaaa5cc6e5368e5f9799188bd798cd22e04ab16d1d8ea4fc37480741e6350010",
        );
        assert_eq!(
            read_record(&wallet, SMART_CONTRACT).unwrap().to_string(),
            "0:83dfd552e63729b472fcbcc8c45ebcc6691702558b68ec7527e1ba403a0f31a8"
        );
        assert_eq!(read_record(&wallet[..20], SMART_CONTRACT), None);
    }
}
