use k256::ecdsa::signature::hazmat::PrehashVerifier;
use k256::ecdsa::{RecoveryId, Signature, SigningKey, VerifyingKey};
use k256::{PublicKey, SecretKey};
use thiserror::Error;

use crate::secp256k1_address;

pub const SIGNATURE_BYTES: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[error("signing failed")]
pub struct SignError;

pub fn secp256k1_sign(
    key: &SecretKey,
    prehash: &[u8; 32],
) -> Result<([u8; SIGNATURE_BYTES], bool), SignError> {
    let (signature, recovery) = SigningKey::from(key).sign_prehash_recoverable(prehash);
    if recovery.is_x_reduced() {
        return Err(SignError);
    }
    Ok((signature.to_bytes().into(), recovery.is_y_odd()))
}

pub fn secp256k1_recover(
    prehash: &[u8; 32],
    signature: &[u8; SIGNATURE_BYTES],
    odd: bool,
) -> Option<[u8; 20]> {
    let signature = Signature::from_slice(signature).ok()?;
    if signature.normalize_s() != signature {
        return None;
    }
    let key = VerifyingKey::recover_from_prehash(prehash, &signature, RecoveryId::new(odd, false))
        .ok()?;
    Some(secp256k1_address(&PublicKey::from(&key)))
}

pub fn secp256k1_verify(
    public: &[u8; 33],
    prehash: &[u8; 32],
    signature: &[u8; SIGNATURE_BYTES],
) -> bool {
    let (Ok(key), Ok(signature)) = (
        VerifyingKey::from_sec1_bytes(public),
        Signature::from_slice(signature),
    ) else {
        return false;
    };
    signature.normalize_s() == signature && key.verify_prehash(prehash, &signature).is_ok()
}

#[cfg(test)]
mod tests {
    use bahaa_types::hex;
    use k256::elliptic_curve::PrimeField;

    use super::*;

    const EIP155_KEY: [u8; 32] = [0x46; 32];
    const EIP155_HASH: &str = "daf5a779ae972f972197303d7b574746c7ef83eadac0f2791ad23db92e4c8e53";
    const EIP155_SIGNATURE: &str = "28ef61340bd939bc2195fe537567866003e1a15d3c71ff63e1590620aa63627667cbe9d8997f761aecb703304b3800ccf555c9f3dc64214b297fb1966a3b6d83";
    const EIP155_SIGNER: &str = "9d8a62f656a8d1615c1294fd71e9cfb3e4855a4f";

    #[test]
    fn the_eip155_example_signs_and_recovers_to_its_sender() {
        let key = SecretKey::from_slice(&EIP155_KEY).unwrap();
        let hash = hex::decode_array::<32>(EIP155_HASH).unwrap();
        let (signature, odd) = secp256k1_sign(&key, &hash).unwrap();
        assert_eq!(
            (hex::encode(&signature), odd),
            (EIP155_SIGNATURE.to_owned(), false)
        );
        let signer = secp256k1_recover(&hash, &signature, odd).unwrap();
        assert_eq!(hex::encode(&signer), EIP155_SIGNER);
        assert_ne!(secp256k1_recover(&hash, &signature, !odd), Some(signer));
    }

    #[test]
    fn a_high_s_twin_of_a_good_signature_is_refused() {
        let hash = hex::decode_array::<32>(EIP155_HASH).unwrap();
        let low = Signature::from_slice(&hex::decode(EIP155_SIGNATURE).unwrap()).unwrap();
        let (r, s) = low.split_scalars();
        let high = Signature::from_scalars(r.to_repr(), (-*s).to_repr()).unwrap();
        let high: [u8; SIGNATURE_BYTES] = high.to_bytes().into();
        assert_eq!(secp256k1_recover(&hash, &high, true), None);
        assert_eq!(secp256k1_recover(&hash, &[0; SIGNATURE_BYTES], false), None);
    }
}
