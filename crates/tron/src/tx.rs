use bahaa_chain::{ChainError, MAX_MEMO, memo_text};
use bahaa_types::is_plain_text;
use k256::ecdsa::{RecoveryId, Signature, SigningKey, VerifyingKey};
use k256::elliptic_curve::sec1::ToEncodedPoint;
use prost::Message;
use ruint::aliases::U256;
use sha2::{Digest as _, Sha256};
use sha3::{Digest as _, Keccak256};

use crate::proto::{Any, Contract, Raw, Transaction, TransferContract, TriggerSmartContract};

const PREFIX: u8 = 0x41;
const TRANSFER_TYPE: i32 = 1;
const TRIGGER_TYPE: i32 = 31;
const TRANSFER_URL: &str = "type.googleapis.com/protocol.TransferContract";
const TRIGGER_URL: &str = "type.googleapis.com/protocol.TriggerSmartContract";
const TRANSFER: [u8; 4] = [0xa9, 0x05, 0x9c, 0xbb];
const TRANSFER_LEN: usize = 4 + 32 + 32;
const SIGNATURE_LEN: usize = 65;
const RESULT_BYTES: u64 = 64;
const LIFETIME_MS: i64 = 5 * 60 * 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Anchor {
    pub number: u64,
    pub id: [u8; 32],
    pub time_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Coins {
        to: [u8; 20],
        sun: u64,
    },
    Token {
        contract: [u8; 20],
        to: [u8; 20],
        units: U256,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CoinsIn {
    pub id: [u8; 32],
    pub from: [u8; 20],
    pub to: [u8; 20],
    pub sun: u64,
    pub memo: Option<String>,
}

#[derive(Debug)]
pub(crate) struct Parsed {
    pub id: [u8; 32],
    pub from: [u8; 20],
    pub action: Action,
    pub expiration_ms: i64,
    pub memo: Option<String>,
}

pub(crate) fn coins(owner: &[u8; 20], to: &[u8; 20], sun: i64) -> Contract {
    let transfer = TransferContract {
        owner_address: tron(owner),
        to_address: tron(to),
        amount: sun,
    };
    contract(TRANSFER_TYPE, TRANSFER_URL, transfer.encode_to_vec())
}

pub(crate) fn token(owner: &[u8; 20], token: &[u8; 20], data: Vec<u8>) -> Contract {
    let trigger = TriggerSmartContract {
        owner_address: tron(owner),
        contract_address: tron(token),
        data,
        ..TriggerSmartContract::default()
    };
    contract(TRIGGER_TYPE, TRIGGER_URL, trigger.encode_to_vec())
}

fn contract(kind: i32, type_url: &str, value: Vec<u8>) -> Contract {
    Contract {
        kind,
        parameter: Some(Any {
            type_url: type_url.to_owned(),
            value,
        }),
    }
}

pub(crate) fn raw(
    contract: Contract,
    anchor: &Anchor,
    fee_limit: i64,
    memo: Option<&str>,
) -> Result<Raw, ChainError> {
    Ok(Raw {
        data: memo
            .map(|memo| memo.as_bytes().to_vec())
            .unwrap_or_default(),
        ref_block_bytes: anchor.number.to_be_bytes()[6..].to_vec(),
        ref_block_hash: anchor.id[8..16].to_vec(),
        expiration: anchor
            .time_ms
            .checked_add(LIFETIME_MS)
            .ok_or(ChainError::Invalid("impossible block time"))?,
        contract: vec![contract],
        timestamp: anchor.time_ms,
        fee_limit,
    })
}

pub(crate) fn bandwidth(raw: &Raw) -> u64 {
    let signed = Transaction {
        raw_data: Some(raw.clone()),
        signature: vec![vec![0; SIGNATURE_LEN]],
    };
    u64::try_from(signed.encoded_len())
        .unwrap_or(u64::MAX)
        .saturating_add(RESULT_BYTES)
}

pub(crate) fn transfer_data(to: &[u8; 20], units: U256) -> Vec<u8> {
    let mut data = Vec::with_capacity(TRANSFER_LEN);
    data.extend_from_slice(&TRANSFER);
    data.extend_from_slice(&[0u8; 12]);
    data.extend_from_slice(to);
    data.extend_from_slice(&units.to_be_bytes::<32>());
    data
}

fn parse_transfer(data: &[u8]) -> Option<([u8; 20], U256)> {
    if data.len() != TRANSFER_LEN || data[..4] != TRANSFER || data[4..16] != [0u8; 12] {
        return None;
    }
    let to: [u8; 20] = data[16..36].try_into().ok()?;
    Some((to, U256::from_be_slice(&data[36..])))
}

pub(crate) fn txid(raw: &Raw) -> [u8; 32] {
    Sha256::digest(raw.encode_to_vec()).into()
}

pub(crate) fn sign(raw: Raw, key: &k256::SecretKey) -> Result<(Vec<u8>, [u8; 32]), ChainError> {
    let id = txid(&raw);
    let (signature, recovery) = SigningKey::from(key)
        .sign_prehash_recoverable(&id)
        .map_err(|_| ChainError::Invalid("signing failed"))?;
    let mut bytes = signature.to_bytes().to_vec();
    bytes.push(27 + recovery.to_byte());
    let tx = Transaction {
        raw_data: Some(raw),
        signature: vec![bytes],
    };
    Ok((tx.encode_to_vec(), id))
}

pub(crate) fn parse(bytes: &[u8]) -> Result<Parsed, ChainError> {
    let invalid = || ChainError::Invalid("not a signed Tron transaction");
    let tx = Transaction::decode(bytes).map_err(|_| invalid())?;
    if tx.encode_to_vec() != bytes {
        return Err(invalid());
    }
    let raw = tx.raw_data.as_ref().ok_or_else(invalid)?;
    let [signature] = tx.signature.as_slice() else {
        return Err(invalid());
    };
    let id = txid(raw);
    let signer = recover(&id, signature).ok_or_else(invalid)?;
    let (owner, action) = action(raw)?;
    if owner != signer {
        return Err(ChainError::Invalid(
            "the signature does not belong to the sender",
        ));
    }
    let memo = if raw.data.is_empty() {
        None
    } else {
        let text = String::from_utf8(raw.data.clone()).map_err(|_| unreadable_memo())?;
        if !is_plain_text(&text, MAX_MEMO) {
            return Err(unreadable_memo());
        }
        Some(text)
    };
    Ok(Parsed {
        id,
        from: signer,
        action,
        expiration_ms: raw.expiration,
        memo,
    })
}

fn unreadable_memo() -> ChainError {
    ChainError::Invalid("the memo is not short plain text")
}

pub(crate) fn coins_in(raw_bytes: &[u8]) -> Option<CoinsIn> {
    let raw = Raw::decode(raw_bytes).ok()?;
    match action(&raw).ok()? {
        (from, Action::Coins { to, sun }) => Some(CoinsIn {
            id: Sha256::digest(raw_bytes).into(),
            from,
            to,
            sun,
            memo: memo_text(&raw.data),
        }),
        (_, Action::Token { .. }) => None,
    }
}

fn action(raw: &Raw) -> Result<([u8; 20], Action), ChainError> {
    let invalid = || ChainError::Invalid("not a signed Tron transaction");
    let [contract] = raw.contract.as_slice() else {
        return Err(invalid());
    };
    let parameter = contract.parameter.as_ref().ok_or_else(invalid)?;
    let (owner, action) = match (contract.kind, parameter.type_url.as_str()) {
        (TRANSFER_TYPE, TRANSFER_URL) => {
            let transfer: TransferContract = exact(&parameter.value).ok_or_else(invalid)?;
            let sun = u64::try_from(transfer.amount)
                .ok()
                .filter(|sun| *sun > 0)
                .ok_or_else(invalid)?;
            let to = address(&transfer.to_address).ok_or_else(invalid)?;
            (transfer.owner_address, Action::Coins { to, sun })
        }
        (TRIGGER_TYPE, TRIGGER_URL) => {
            let trigger: TriggerSmartContract = exact(&parameter.value).ok_or_else(invalid)?;
            if trigger.call_value != 0 || trigger.call_token_value != 0 || trigger.token_id != 0 {
                return Err(ChainError::Invalid(
                    "a token transfer must not also send coins",
                ));
            }
            let (to, units) = parse_transfer(&trigger.data)
                .ok_or(ChainError::Invalid("not a plain token transfer"))?;
            let contract = address(&trigger.contract_address).ok_or_else(invalid)?;
            (
                trigger.owner_address,
                Action::Token {
                    contract,
                    to,
                    units,
                },
            )
        }
        _ => return Err(ChainError::Invalid("not a plain transfer")),
    };
    Ok((address(&owner).ok_or_else(invalid)?, action))
}

fn exact<T: Message + Default>(bytes: &[u8]) -> Option<T> {
    let message = T::decode(bytes).ok()?;
    (message.encode_to_vec() == bytes).then_some(message)
}

fn recover(id: &[u8; 32], signature: &[u8]) -> Option<[u8; 20]> {
    let (body, [v]) = signature.split_at_checked(SIGNATURE_LEN - 1)? else {
        return None;
    };
    let recovery = match v {
        0 | 27 => 0,
        1 | 28 => 1,
        _ => return None,
    };
    let signature = Signature::from_slice(body).ok()?;
    if signature.normalize_s().is_some() {
        return None;
    }
    let key = VerifyingKey::recover_from_prehash(id, &signature, RecoveryId::from_byte(recovery)?)
        .ok()?;
    Some(address_of(&k256::PublicKey::from(&key)))
}

pub(crate) fn address_of(key: &k256::PublicKey) -> [u8; 20] {
    let point = key.to_encoded_point(false);
    let hash = Keccak256::digest(&point.as_bytes()[1..]);
    let mut out = [0u8; 20];
    out.copy_from_slice(&hash[12..]);
    out
}

fn tron(address: &[u8; 20]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(21);
    bytes.push(PREFIX);
    bytes.extend_from_slice(address);
    bytes
}

fn address(bytes: &[u8]) -> Option<[u8; 20]> {
    match bytes.split_first() {
        Some((&PREFIX, rest)) => rest.try_into().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> k256::SecretKey {
        k256::SecretKey::from_slice(&[byte; 32]).unwrap()
    }

    fn anchor() -> Anchor {
        let mut id = [0u8; 32];
        id[..8].copy_from_slice(&87_210_587u64.to_be_bytes());
        id[8..16].copy_from_slice(&[7; 8]);
        Anchor {
            number: 87_210_587,
            id,
            time_ms: 1_792_304_763_000,
        }
    }

    fn owner(key: &k256::SecretKey) -> [u8; 20] {
        address_of(&key.public_key())
    }

    #[test]
    fn the_reference_block_and_lifetime_follow_tapos() {
        let raw = raw(coins(&[1; 20], &[2; 20], 5), &anchor(), 0, None).unwrap();
        assert_eq!(raw.ref_block_bytes, 87_210_587u64.to_be_bytes()[6..]);
        assert_eq!(raw.ref_block_hash, [7; 8]);
        assert_eq!(raw.expiration - raw.timestamp, 300_000);
        assert_eq!(raw.fee_limit, 0);
    }

    #[test]
    fn signed_transfers_parse_back_to_what_was_built() {
        let key = key(3);
        let from = owner(&key);
        let (bytes, id) = sign(
            raw(coins(&from, &[9; 20], 1_500_000), &anchor(), 0, None).unwrap(),
            &key,
        )
        .unwrap();
        let parsed = parse(&bytes).unwrap();
        assert_eq!((parsed.id, parsed.from), (id, from));
        assert_eq!(
            parsed.action,
            Action::Coins {
                to: [9; 20],
                sun: 1_500_000
            }
        );
        assert_eq!(parsed.expiration_ms, 1_792_305_063_000);
        let data = transfer_data(&[9; 20], U256::from(2_500_000u64));
        let (bytes, _) = sign(
            raw(token(&from, &[5; 20], data), &anchor(), 7_800_000, None).unwrap(),
            &key,
        )
        .unwrap();
        assert_eq!(
            parse(&bytes).unwrap().action,
            Action::Token {
                contract: [5; 20],
                to: [9; 20],
                units: U256::from(2_500_000u64)
            }
        );
    }

    #[test]
    fn memos_are_signed_with_the_transfer_and_must_be_plain_text() {
        let key = key(5);
        let from = owner(&key);
        for contract in [
            coins(&from, &[9; 20], 1_000),
            token(&from, &[5; 20], transfer_data(&[9; 20], U256::from(3u8))),
        ] {
            let built = raw(contract, &anchor(), 1, Some("deposit 4417")).unwrap();
            assert_eq!(built.data, b"deposit 4417");
            let (bytes, id) = sign(built, &key).unwrap();
            let parsed = parse(&bytes).unwrap();
            assert_eq!(parsed.id, id);
            assert_eq!(parsed.memo.as_deref(), Some("deposit 4417"));
        }
        let plain = raw(coins(&from, &[9; 20], 1_000), &anchor(), 0, None).unwrap();
        assert_eq!(parse(&sign(plain, &key).unwrap().0).unwrap().memo, None);
        for bad in [&[0xff, 0xfe][..], "pay\u{202e}lla".as_bytes()] {
            let mut built = raw(coins(&from, &[9; 20], 1_000), &anchor(), 0, None).unwrap();
            built.data = bad.to_vec();
            let (bytes, _) = sign(built, &key).unwrap();
            assert!(matches!(parse(&bytes), Err(ChainError::Invalid(_))));
        }
    }

    #[test]
    fn the_bandwidth_estimate_is_the_signed_size_plus_the_result_reserve() {
        let key = key(4);
        let built = raw(coins(&owner(&key), &[9; 20], 1), &anchor(), 0, None).unwrap();
        let estimate = bandwidth(&built);
        let (bytes, _) = sign(built, &key).unwrap();
        assert_eq!(estimate, u64::try_from(bytes.len()).unwrap() + 64);
    }

    #[test]
    fn signatures_are_low_s_and_tampering_is_caught() {
        for byte in 1..40 {
            let key = key(byte);
            let (bytes, _) = sign(
                raw(coins(&owner(&key), &[9; 20], 1), &anchor(), 0, None).unwrap(),
                &key,
            )
            .unwrap();
            let tx = Transaction::decode(bytes.as_slice()).unwrap();
            let signature = Signature::from_slice(&tx.signature[0][..64]).unwrap();
            assert!(signature.normalize_s().is_none(), "key {byte}");
            assert!(matches!(tx.signature[0][64], 27 | 28));
        }
        let key = key(5);
        let (bytes, _) = sign(
            raw(coins(&owner(&key), &[9; 20], 1), &anchor(), 0, None).unwrap(),
            &key,
        )
        .unwrap();
        let mut tampered = bytes.clone();
        let amount_at = bytes.len() - 70;
        tampered[amount_at] ^= 1;
        assert!(parse(&tampered).is_err());
        let mut extended = bytes.clone();
        extended.extend_from_slice(&[0x18, 0x01]);
        assert!(parse(&extended).is_err());
    }

    #[test]
    fn coin_transfers_from_history_are_read_from_the_bytes_their_id_covers() {
        let key = key(7);
        let from = owner(&key);
        let built = raw(coins(&from, &[9; 20], 42), &anchor(), 0, None).unwrap();
        let bytes = built.encode_to_vec();
        let expected = CoinsIn {
            id: txid(&built),
            from,
            to: [9; 20],
            sun: 42,
            memo: None,
        };
        assert_eq!(coins_in(&bytes), Some(expected));
        let mut memo = bytes.clone();
        memo.extend_from_slice(&[0x52, 0x02, 0x68, 0x69]);
        let read = coins_in(&memo).unwrap();
        assert_eq!(read.id, <[u8; 32]>::from(Sha256::digest(&memo)));
        assert_ne!(read.id, txid(&built));
        assert_eq!((read.from, read.to, read.sun), (from, [9; 20], 42));
        assert_eq!(read.memo.as_deref(), Some("hi"));
        let data = transfer_data(&[9; 20], U256::from(1u8));
        let tokens = raw(token(&from, &[5; 20], data), &anchor(), 1, None).unwrap();
        assert_eq!(coins_in(&tokens.encode_to_vec()), None);
        assert_eq!(coins_in(&[0xff, 0x01]), None);
    }

    #[test]
    fn only_plain_transfers_signed_by_their_owner_are_accepted() {
        let key = key(6);
        let from = owner(&key);
        let signed = |contract: Contract| {
            sign(raw(contract, &anchor(), 0, None).unwrap(), &key)
                .unwrap()
                .0
        };
        let stranger = coins(&[8; 20], &[9; 20], 1);
        assert!(parse(&signed(stranger)).is_err());
        let zero = coins(&from, &[9; 20], 0);
        assert!(parse(&signed(zero)).is_err());
        let mut approve = transfer_data(&[9; 20], U256::from(1u8));
        approve[..4].copy_from_slice(&[0x09, 0x5e, 0xa7, 0xb3]);
        assert!(parse(&signed(token(&from, &[5; 20], approve))).is_err());
        let paying = TriggerSmartContract {
            owner_address: tron(&from),
            contract_address: tron(&[5; 20]),
            call_value: 1,
            data: transfer_data(&[9; 20], U256::from(1u8)),
            ..TriggerSmartContract::default()
        };
        let paying = contract(TRIGGER_TYPE, TRIGGER_URL, paying.encode_to_vec());
        assert!(parse(&signed(paying)).is_err());
        let mut mislabelled = coins(&from, &[9; 20], 1);
        mislabelled.kind = TRIGGER_TYPE;
        assert!(parse(&signed(mislabelled)).is_err());
        let mut built = raw(coins(&from, &[9; 20], 1), &anchor(), 0, None).unwrap();
        built.contract.push(coins(&from, &[9; 20], 1));
        assert!(parse(&sign(built, &key).unwrap().0).is_err());
    }
}
