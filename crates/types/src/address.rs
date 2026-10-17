use std::fmt::{self, Write};

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE};
use sha3::{Digest, Keccak256};
use thiserror::Error;

use crate::ChainFamily;

const TRON_PREFIX: u8 = 0x41;
const TON_BOUNCEABLE: u8 = 0x11;
const TON_NON_BOUNCEABLE: u8 = 0x51;
const TON_TESTNET: u8 = 0x80;
pub(crate) const TON_FRIENDLY_LEN: usize = 48;

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Address {
    Evm([u8; 20]),
    Tron([u8; 20]),
    Ton { workchain: i8, hash: [u8; 32] },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TonForm {
    pub bounceable: bool,
    pub testnet: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum AddressError {
    #[error("this is not a valid {0} address")]
    Format(ChainFamily),
    #[error("the address checksum does not match, check it for a typo")]
    Checksum,
    #[error("TON workchain must be between -128 and 127")]
    Workchain,
}

impl Address {
    pub fn parse(family: ChainFamily, text: &str) -> Result<Self, AddressError> {
        match family {
            ChainFamily::Evm => parse_evm(text),
            ChainFamily::Tron => parse_tron(text),
            ChainFamily::Ton => Self::parse_ton(text).map(|(address, _)| address),
        }
    }

    pub fn parse_ton(text: &str) -> Result<(Self, Option<TonForm>), AddressError> {
        if let Some((workchain, hash)) = text.split_once(':') {
            let workchain = workchain
                .parse::<i8>()
                .map_err(|_| AddressError::Workchain)?;
            let hash = decode_hex::<32>(hash).ok_or(AddressError::Format(ChainFamily::Ton))?;
            return Ok((Self::Ton { workchain, hash }, None));
        }
        parse_ton_friendly(text).map(|(address, form)| (address, Some(form)))
    }

    pub fn family(&self) -> ChainFamily {
        match self {
            Self::Evm(_) => ChainFamily::Evm,
            Self::Tron(_) => ChainFamily::Tron,
            Self::Ton { .. } => ChainFamily::Ton,
        }
    }

    pub fn ton_friendly(&self, form: TonForm) -> Option<String> {
        let Self::Ton { workchain, hash } = self else {
            return None;
        };
        let mut flags = if form.bounceable {
            TON_BOUNCEABLE
        } else {
            TON_NON_BOUNCEABLE
        };
        if form.testnet {
            flags |= TON_TESTNET;
        }
        let mut bytes = Vec::with_capacity(36);
        bytes.push(flags);
        bytes.extend_from_slice(&workchain.to_be_bytes());
        bytes.extend_from_slice(hash);
        bytes.extend_from_slice(&crc16(&bytes).to_be_bytes());
        Some(URL_SAFE.encode(bytes))
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Evm(bytes) => f.write_str(&checksummed(bytes)),
            Self::Tron(bytes) => f.write_str(&tron_base58(bytes)),
            Self::Ton { workchain, hash } => write!(f, "{workchain}:{}", hex(hash)),
        }
    }
}

impl fmt::Debug for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Address({} {self})", self.family())
    }
}

fn parse_evm(text: &str) -> Result<Address, AddressError> {
    let invalid = AddressError::Format(ChainFamily::Evm);
    let body = text.strip_prefix("0x").ok_or(invalid)?;
    let bytes = decode_hex::<20>(body).ok_or(invalid)?;
    let mixed_case = body.bytes().any(|byte| byte.is_ascii_lowercase())
        && body.bytes().any(|byte| byte.is_ascii_uppercase());
    if mixed_case && checksummed(&bytes) != text {
        return Err(AddressError::Checksum);
    }
    Ok(Address::Evm(bytes))
}

fn parse_tron(text: &str) -> Result<Address, AddressError> {
    let invalid = AddressError::Format(ChainFamily::Tron);
    let data = bs58::decode(text)
        .with_check(Some(TRON_PREFIX))
        .into_vec()
        .map_err(|err| match err {
            bs58::decode::Error::InvalidChecksum { .. } => AddressError::Checksum,
            _ => invalid,
        })?;
    let bytes: [u8; 20] = data
        .get(1..)
        .and_then(|rest| rest.try_into().ok())
        .ok_or(invalid)?;
    Ok(Address::Tron(bytes))
}

fn parse_ton_friendly(text: &str) -> Result<(Address, TonForm), AddressError> {
    let invalid = AddressError::Format(ChainFamily::Ton);
    let url_safe = text.contains(['-', '_']);
    let standard = text.contains(['+', '/']);
    if text.len() != TON_FRIENDLY_LEN || (url_safe && standard) {
        return Err(invalid);
    }
    let bytes = if url_safe {
        URL_SAFE.decode(text)
    } else {
        STANDARD.decode(text)
    }
    .map_err(|_| invalid)?;
    let (body, checksum) = bytes.split_at_checked(34).ok_or(invalid)?;
    if checksum != crc16(body).to_be_bytes() {
        return Err(AddressError::Checksum);
    }
    let bounceable = match body[0] & !TON_TESTNET {
        TON_BOUNCEABLE => true,
        TON_NON_BOUNCEABLE => false,
        _ => return Err(invalid),
    };
    let form = TonForm {
        bounceable,
        testnet: body[0] & TON_TESTNET != 0,
    };
    let workchain = i8::from_be_bytes([body[1]]);
    let hash: [u8; 32] = body[2..].try_into().map_err(|_| invalid)?;
    Ok((Address::Ton { workchain, hash }, form))
}

fn checksummed(bytes: &[u8; 20]) -> String {
    let lower = hex(bytes);
    let hash = Keccak256::digest(lower.as_bytes());
    let mut out = String::with_capacity(42);
    out.push_str("0x");
    for (index, c) in lower.chars().enumerate() {
        let byte = hash[index / 2];
        let nibble = if index % 2 == 0 {
            byte >> 4
        } else {
            byte & 0x0f
        };
        out.push(if nibble >= 8 {
            c.to_ascii_uppercase()
        } else {
            c
        });
    }
    out
}

fn tron_base58(bytes: &[u8; 20]) -> String {
    let mut data = Vec::with_capacity(21);
    data.push(TRON_PREFIX);
    data.extend_from_slice(bytes);
    bs58::encode(data).with_check().into_string()
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

fn decode_hex<const N: usize>(text: &str) -> Option<[u8; N]> {
    if text.len() != N * 2 {
        return None;
    }
    let mut out = [0u8; N];
    let (pairs, _) = text.as_bytes().as_chunks::<2>();
    for (slot, [high, low]) in out.iter_mut().zip(pairs) {
        let high = char::from(*high).to_digit(16)?;
        let low = char::from(*low).to_digit(16)?;
        *slot = u8::try_from(high * 16 + low).ok()?;
    }
    Some(out)
}

fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for byte in data {
        crc ^= u16::from(*byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 == 0 {
                crc << 1
            } else {
                (crc << 1) ^ 0x1021
            };
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evm(text: &str) -> Result<Address, AddressError> {
        Address::parse(ChainFamily::Evm, text)
    }

    #[test]
    fn eip55_vectors_round_trip() {
        for text in [
            "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed",
            "0xfB6916095ca1df60bB79Ce92cE3Ea74c37c5d359",
            "0xdbF03B407c01E7cD3CBea99509d93f8DDDC8C6FB",
            "0xD1220A0cf47c7B9Be7A2E6BA89F429762e7b9aDb",
            "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266",
        ] {
            assert_eq!(evm(text).unwrap().to_string(), text);
        }
    }

    #[test]
    fn single_case_evm_addresses_are_accepted_and_checksummed() {
        let lower = evm("0x5aaeb6053f3e94c9b9a09f33669435e7ef1beaed").unwrap();
        let upper = evm("0x5AAEB6053F3E94C9B9A09F33669435E7EF1BEAED").unwrap();
        assert_eq!(lower, upper);
        assert_eq!(
            lower.to_string(),
            "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed"
        );
    }

    #[test]
    fn a_wrong_evm_checksum_is_a_typo() {
        assert_eq!(
            evm("0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAeD"),
            Err(AddressError::Checksum)
        );
        assert_eq!(
            evm("0x5AAeb6053F3E94C9b9A09f33669435E7Ef1BeAed"),
            Err(AddressError::Checksum)
        );
    }

    #[test]
    fn rejects_malformed_evm_addresses() {
        for text in [
            "",
            "5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed",
            "0X5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed",
            "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeA",
            "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed00",
            "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAeg",
            " 0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed",
        ] {
            assert_eq!(
                evm(text),
                Err(AddressError::Format(ChainFamily::Evm)),
                "{text:?}"
            );
        }
    }

    #[test]
    fn tron_addresses_match_known_bytes() {
        let usdt = Address::parse(ChainFamily::Tron, "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t").unwrap();
        let Address::Tron(bytes) = usdt else {
            panic!("not tron")
        };
        assert_eq!(hex(&bytes), "a614f803b6fd780986a42c78ec9c7f77e6ded13c");
        assert_eq!(usdt.to_string(), "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t");
        assert_eq!(
            Address::Tron([0; 20]).to_string(),
            "T9yD14Nj9j7xAB4dbGeiX9h8unkKHxuWwb"
        );
    }

    #[test]
    fn rejects_tron_typos_and_other_chains() {
        let tron = |text| Address::parse(ChainFamily::Tron, text);
        assert_eq!(
            tron("TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6u"),
            Err(AddressError::Checksum)
        );
        assert_eq!(
            tron("1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa"),
            Err(AddressError::Format(ChainFamily::Tron))
        );
        assert_eq!(
            tron("TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj60"),
            Err(AddressError::Format(ChainFamily::Tron))
        );
        assert_eq!(tron(""), Err(AddressError::Format(ChainFamily::Tron)));
    }

    #[test]
    fn ton_friendly_and_raw_forms_are_the_same_account() {
        let (friendly, form) =
            Address::parse_ton("0QBi-dfmY12lN3xLE4b1-C3jBvOsIIGQEro3kKnbbm6qWOMF").unwrap();
        assert_eq!(
            form,
            Some(TonForm {
                bounceable: false,
                testnet: true
            })
        );
        let (raw, none) = Address::parse_ton(
            "0:62F9D7E6635DA5377C4B1386F5F82DE306F3AC20819012BA3790A9DB6E6EAA58",
        )
        .unwrap();
        assert_eq!(none, None);
        assert_eq!(friendly, raw);
        assert_eq!(
            raw.to_string(),
            "0:62f9d7e6635da5377c4b1386f5f82de306f3ac20819012ba3790a9db6e6eaa58"
        );
    }

    #[test]
    fn ton_friendly_forms_round_trip() {
        for text in [
            "EQCD39VS5jcptHL8vMjEXrzGaRcCVYto7HUn4bpAOg8xqB2N",
            "EQCxE6mUtQJKFnGfaROTKOt1lZbDiiX1kCixRv7Nw2Id_sDs",
            "0QBi-dfmY12lN3xLE4b1-C3jBvOsIIGQEro3kKnbbm6qWOMF",
        ] {
            let (address, form) = Address::parse_ton(text).unwrap();
            assert_eq!(address.ton_friendly(form.unwrap()).unwrap(), text);
        }
        let (address, _) =
            Address::parse_ton("EQCD39VS5jcptHL8vMjEXrzGaRcCVYto7HUn4bpAOg8xqB2N").unwrap();
        let wallet = TonForm {
            bounceable: false,
            testnet: false,
        };
        assert!(address.ton_friendly(wallet).unwrap().starts_with("UQ"));
    }

    #[test]
    fn ton_accepts_either_base64_alphabet_but_not_both() {
        let standard = "0QBi+dfmY12lN3xLE4b1+C3jBvOsIIGQEro3kKnbbm6qWOMF";
        let url_safe = "0QBi-dfmY12lN3xLE4b1-C3jBvOsIIGQEro3kKnbbm6qWOMF";
        assert_eq!(Address::parse_ton(standard), Address::parse_ton(url_safe));
        let mixed = "0QBi+dfmY12lN3xLE4b1-C3jBvOsIIGQEro3kKnbbm6qWOMF";
        assert_eq!(
            Address::parse_ton(mixed),
            Err(AddressError::Format(ChainFamily::Ton))
        );
    }

    #[test]
    fn rejects_ton_typos_and_bad_forms() {
        assert_eq!(
            Address::parse_ton("EQCD39VS5jcptHL8vMjEXrzGaRcCVYto7HUn4bpAOg8xqB2M"),
            Err(AddressError::Checksum)
        );
        assert_eq!(
            Address::parse_ton(
                "300:62f9d7e6635da5377c4b1386f5f82de306f3ac20819012ba3790a9db6e6eaa58"
            ),
            Err(AddressError::Workchain)
        );
        for text in [
            "",
            "0:62f9",
            "EQCD39VS5jcptHL8vMjEXrzGaRcCVYto7HUn4bpAOg8xqB2",
            "0:zzf9d7e6635da5377c4b1386f5f82de306f3ac20819012ba3790a9db6e6eaa58",
        ] {
            assert_eq!(
                Address::parse_ton(text),
                Err(AddressError::Format(ChainFamily::Ton)),
                "{text:?}"
            );
        }
    }

    #[test]
    fn families_never_mix() {
        let evm_text = "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed";
        assert!(Address::parse(ChainFamily::Tron, evm_text).is_err());
        assert!(Address::parse(ChainFamily::Ton, evm_text).is_err());
        assert!(Address::parse(ChainFamily::Evm, "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t").is_err());
        assert_ne!(Address::Evm([7; 20]), Address::Tron([7; 20]));
        assert_eq!(Address::Tron([7; 20]).family(), ChainFamily::Tron);
    }
}
