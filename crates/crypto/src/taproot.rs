use k256::elliptic_curve::PrimeField;
use k256::elliptic_curve::sec1::ToSec1Point;
use k256::schnorr::signature::hazmat::PrehashVerifier;
use k256::schnorr::{Signature, SigningKey, VerifyingKey};
use k256::{FieldBytes, NonZeroScalar, ProjectivePoint, PublicKey, Scalar, SecretKey};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::SignError;

const TWEAK_TAG: &[u8] = b"TapTweak";

pub fn taproot_output(public: &[u8; 33]) -> Result<[u8; 32], SignError> {
    let public = PublicKey::from_sec1_bytes(public).map_err(|_| SignError)?;
    let point = public.to_sec1_point(true);
    let x = point.x().ok_or(SignError)?;
    let internal = VerifyingKey::from_bytes(x).map_err(|_| SignError)?;
    let tweak = tweak(&internal)?;
    let output = (ProjectivePoint::from(*internal.as_affine())
        + ProjectivePoint::mul_by_generator(&tweak))
    .to_affine();
    let output = VerifyingKey::try_from(output)
        .or_else(|_| VerifyingKey::try_from(-output))
        .map_err(|_| SignError)?;
    Ok(output.to_bytes().into())
}

pub fn taproot_sign(key: &SecretKey, sighash: &[u8; 32]) -> Result<[u8; 64], SignError> {
    let mut aux = Zeroizing::new([0u8; 32]);
    getrandom::fill(&mut *aux).map_err(|_| SignError)?;
    sign_with(key, sighash, &aux)
}

pub fn taproot_verify(output: &[u8; 32], sighash: &[u8; 32], signature: &[u8; 64]) -> bool {
    let (Ok(key), Ok(signature)) = (
        VerifyingKey::from_bytes(&FieldBytes::from(*output)),
        Signature::try_from(&signature[..]),
    ) else {
        return false;
    };
    key.verify_prehash(sighash, &signature).is_ok()
}

fn sign_with(key: &SecretKey, sighash: &[u8; 32], aux: &[u8; 32]) -> Result<[u8; 64], SignError> {
    let internal = SigningKey::from(key);
    let tweak = tweak(internal.verifying_key())?;
    let tweaked: Option<NonZeroScalar> =
        NonZeroScalar::new(**internal.as_nonzero_scalar() + tweak).into();
    let signing = SigningKey::from(tweaked.ok_or(SignError)?);
    let signature = signing.sign_raw(sighash, aux).map_err(|_| SignError)?;
    Ok(signature.to_bytes())
}

pub fn tagged_hash(tag: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let tag = Sha256::digest(tag);
    let mut hash = Sha256::new().chain_update(tag).chain_update(tag);
    for part in parts {
        hash.update(part);
    }
    hash.finalize().into()
}

fn tweak(internal: &VerifyingKey) -> Result<Scalar, SignError> {
    let hash = tagged_hash(TWEAK_TAG, &[&internal.to_bytes()]);
    Option::from(Scalar::from_repr(FieldBytes::from(hash))).ok_or(SignError)
}

#[cfg(test)]
mod tests {
    use bahaa_types::hex;

    use super::*;

    fn public(text: &str) -> [u8; 33] {
        let mut bytes = [0x02; 33];
        bytes[1..].copy_from_slice(&hex::decode(text).unwrap());
        bytes
    }

    #[test]
    fn bip341_and_bip86_internal_keys_give_the_published_output_keys() {
        for (internal, output) in [
            (
                "d6889cb081036e0faefa3a35157ad71086b123b2b144b649798b494c300a961d",
                "53a1f6e454df1aa2776a2814a721372d6258050de330b3c6d10ee8f4e0dda343",
            ),
            (
                "cc8a4bc64d897bddc5fbc2f670f7a8ba0b386779106cf1223c6fc5d7cd6fc115",
                "a60869f0dbcf1dc659c9cecbaf8050135ea9e8cdc487053f1dc6880949dc684c",
            ),
            (
                "83dfe85a3151d2517290da461fe2815591ef69f2b18a2ce63f01697a8b313145",
                "a82f29944d65b86ae6b5e5cc75e294ead6c59391a1edc5e016e3498c67fc7bbb",
            ),
        ] {
            assert_eq!(
                hex::encode(&taproot_output(&public(internal)).unwrap()),
                output
            );
        }
    }

    #[test]
    fn signatures_verify_under_the_output_key_whatever_the_randomness() {
        let key = SecretKey::from_slice(
            &hex::decode("6b973d88838f27366ed61c9ad6367663045cb456e28335c109e30717ae0c6baa")
                .unwrap(),
        )
        .unwrap();
        let output = taproot_output(&crate::compressed(&key.public_key()).unwrap()).unwrap();
        let sighash = [7u8; 32];
        for signature in [
            sign_with(&key, &sighash, &[0; 32]).unwrap(),
            taproot_sign(&key, &sighash).unwrap(),
        ] {
            assert!(taproot_verify(&output, &sighash, &signature));
            assert!(!taproot_verify(&output, &[8; 32], &signature));
        }
        assert_ne!(
            taproot_sign(&key, &sighash).unwrap(),
            taproot_sign(&key, &sighash).unwrap()
        );
    }
}
