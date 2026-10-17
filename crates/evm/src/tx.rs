use alloy_consensus::transaction::SignerRecoverable;
use alloy_consensus::{SignableTransaction, TxEip1559, TxEnvelope};
use alloy_eips::eip2718::{Decodable2718, Encodable2718};
use alloy_primitives::Signature;
use bahaa_chain::ChainError;
use k256::ecdsa::SigningKey;
use ruint::aliases::U256;

const TRANSFER: [u8; 4] = [0xa9, 0x05, 0x9c, 0xbb];
const TRANSFER_LEN: usize = 4 + 32 + 32;

pub(crate) struct Parsed {
    pub from: [u8; 20],
    pub tx: TxEip1559,
}

pub(crate) fn transfer_data(to: &[u8; 20], amount: U256) -> Vec<u8> {
    let mut data = Vec::with_capacity(TRANSFER_LEN);
    data.extend_from_slice(&TRANSFER);
    data.extend_from_slice(&[0u8; 12]);
    data.extend_from_slice(to);
    data.extend_from_slice(&amount.to_be_bytes::<32>());
    data
}

pub(crate) fn parse_transfer(data: &[u8]) -> Option<([u8; 20], U256)> {
    if data.len() != TRANSFER_LEN || data[..4] != TRANSFER || data[4..16] != [0u8; 12] {
        return None;
    }
    let to: [u8; 20] = data[16..36].try_into().ok()?;
    Some((to, U256::from_be_slice(&data[36..])))
}

pub(crate) fn sign(
    tx: TxEip1559,
    key: &k256::SecretKey,
) -> Result<(Vec<u8>, [u8; 32]), ChainError> {
    let signing = SigningKey::from(key);
    let hash = tx.signature_hash();
    let (signature, recovery) = signing
        .sign_prehash_recoverable(hash.as_slice())
        .map_err(|_| ChainError::Invalid("signing failed"))?;
    let signature = Signature::from_signature_and_parity(signature, recovery.is_y_odd());
    let envelope = TxEnvelope::from(tx.into_signed(signature));
    Ok((envelope.encoded_2718(), envelope.tx_hash().0))
}

pub(crate) fn parse(bytes: &[u8]) -> Result<Parsed, ChainError> {
    let invalid = || ChainError::Invalid("not a signed EVM transaction");
    let envelope = TxEnvelope::decode_2718(&mut &bytes[..]).map_err(|_| invalid())?;
    if envelope.encoded_2718() != bytes {
        return Err(invalid());
    }
    let signed = envelope.as_eip1559().ok_or(invalid())?;
    let from = envelope.recover_signer().map_err(|_| invalid())?;
    Ok(Parsed {
        from: from.into_array(),
        tx: signed.tx().clone(),
    })
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Bytes, TxKind};

    use super::*;

    fn key(byte: u8) -> k256::SecretKey {
        k256::SecretKey::from_slice(&[byte; 32]).unwrap()
    }

    fn sample(nonce: u64) -> TxEip1559 {
        TxEip1559 {
            chain_id: 56,
            nonce,
            gas_limit: 21_000,
            max_fee_per_gas: 2_000_000_000,
            max_priority_fee_per_gas: 1_000_000_000,
            to: TxKind::Call([7u8; 20].into()),
            value: U256::from(5u8),
            input: Bytes::new(),
            ..TxEip1559::default()
        }
    }

    #[test]
    fn token_transfer_calls_round_trip() {
        let data = transfer_data(&[0xab; 20], U256::from(1_234_567u64));
        assert_eq!(hex_of(&data[..4]), "a9059cbb");
        assert_eq!(
            parse_transfer(&data),
            Some(([0xab; 20], U256::from(1_234_567u64)))
        );
    }

    #[test]
    fn anything_but_a_plain_transfer_is_not_recognised() {
        let good = transfer_data(&[1; 20], U256::from(1u8));
        let mut approve = good.clone();
        approve[..4].copy_from_slice(&[0x09, 0x5e, 0xa7, 0xb3]);
        let mut dirty = good.clone();
        dirty[4] = 1;
        let mut longer = good.clone();
        longer.push(0);
        for data in [approve, dirty, longer, good[..67].to_vec(), Vec::new()] {
            assert_eq!(parse_transfer(&data), None);
        }
    }

    #[test]
    fn signed_bytes_parse_back_to_the_same_transaction_and_signer() {
        let key = key(3);
        let (bytes, hash) = sign(sample(9), &key).unwrap();
        let parsed = parse(&bytes).unwrap();
        assert_eq!(parsed.tx, sample(9));
        assert_eq!(parsed.from, crate::chain::address_of(&key.public_key()));
        assert_eq!(hash.len(), 32);
        let mut tampered = bytes.clone();
        let last = tampered.len() - 40;
        tampered[last] ^= 1;
        assert!(parse(&tampered).map_or(true, |parsed| parsed.from
            != crate::chain::address_of(&key.public_key())));
    }

    #[test]
    fn signatures_are_always_low_s() {
        for nonce in 0..64 {
            let (bytes, _) = sign(sample(nonce), &key(5)).unwrap();
            let envelope = TxEnvelope::decode_2718(&mut &bytes[..]).unwrap();
            let signature = envelope.as_eip1559().unwrap().signature();
            assert!(signature.normalize_s().is_none(), "nonce {nonce}");
        }
    }

    fn hex_of(bytes: &[u8]) -> String {
        crate::chain::hex(bytes)[2..].to_owned()
    }
}
