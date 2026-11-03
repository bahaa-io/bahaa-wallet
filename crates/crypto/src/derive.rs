use ed25519_dalek::SigningKey;
use hmac::{Hmac, Mac};
use k256::elliptic_curve::PrimeField;
use k256::elliptic_curve::sec1::ToSec1Point;
use k256::{FieldBytes, ProjectivePoint, PublicKey, Scalar, SecretKey};
use sha2::Sha512;
use sha3::{Digest, Keccak256};
use thiserror::Error;
use zeroize::Zeroizing;

use crate::path::HARDENED;
use crate::{DerivationPath, Seed, SeedKind};

const BIP32_KEY: &[u8] = b"Bitcoin seed";
const SLIP10_ED25519_KEY: &[u8] = b"ed25519 seed";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum DeriveError {
    #[error("ed25519 keys can only be derived along fully hardened paths")]
    NotHardened,
    #[error("this path leads to an invalid key, use the next index")]
    InvalidKey,
    #[error("this wallet's key belongs to another kind of network")]
    OtherCurve,
    #[error("this wallet is a single key without an account key")]
    SingleKey,
    #[error("public keys can only derive unhardened children")]
    Hardened,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtendedPublic {
    key: [u8; 33],
    chain: [u8; 32],
}

impl ExtendedPublic {
    pub fn key(&self) -> [u8; 33] {
        self.key
    }

    pub fn child(&self, index: u32) -> Result<Self, DeriveError> {
        if index & HARDENED != 0 {
            return Err(DeriveError::Hardened);
        }
        let output = hmac512(&self.chain, &[&self.key, &index.to_be_bytes()])?;
        let tweak = scalar(
            output[..32]
                .try_into()
                .map_err(|_| DeriveError::InvalidKey)?,
        )?;
        let parent = PublicKey::from_sec1_bytes(&self.key).map_err(|_| DeriveError::InvalidKey)?;
        let point = ProjectivePoint::GENERATOR * *tweak + parent.to_projective();
        let child =
            PublicKey::from_affine(point.to_affine()).map_err(|_| DeriveError::InvalidKey)?;
        let mut chain = [0u8; 32];
        chain.copy_from_slice(&output[32..]);
        Ok(Self {
            key: compressed(&child)?,
            chain,
        })
    }
}

pub fn secp256k1_extended(
    seed: &Seed,
    path: &DerivationPath,
) -> Result<ExtendedPublic, DeriveError> {
    if seed.kind() != SeedKind::Bip39 {
        return Err(DeriveError::SingleKey);
    }
    secp256k1_node(seed.expose(), path.indices())?.public()
}

pub fn compressed(key: &PublicKey) -> Result<[u8; 33], DeriveError> {
    key.to_sec1_point(true)
        .as_bytes()
        .try_into()
        .map_err(|_| DeriveError::InvalidKey)
}

struct Node {
    key: Zeroizing<[u8; 32]>,
    chain: Zeroizing<[u8; 32]>,
}

impl Node {
    fn secret(&self) -> Result<SecretKey, DeriveError> {
        SecretKey::from_bytes(&FieldBytes::from(*self.key)).map_err(|_| DeriveError::InvalidKey)
    }

    fn public(&self) -> Result<ExtendedPublic, DeriveError> {
        Ok(ExtendedPublic {
            key: compressed(&self.secret()?.public_key())?,
            chain: *self.chain,
        })
    }
}

pub fn secp256k1(seed: &Seed, path: &DerivationPath) -> Result<SecretKey, DeriveError> {
    match seed.kind() {
        SeedKind::Secp256k1 => {
            SecretKey::from_slice(seed.expose()).map_err(|_| DeriveError::InvalidKey)
        }
        SeedKind::Bip39 => secp256k1_node(seed.expose(), path.indices())?.secret(),
        SeedKind::Ed25519 => Err(DeriveError::OtherCurve),
    }
}

pub fn secp256k1_address(key: &PublicKey) -> [u8; 20] {
    let hash = Keccak256::digest(&key.to_sec1_point(false).as_bytes()[1..]);
    let mut out = [0u8; 20];
    out.copy_from_slice(&hash[12..]);
    out
}

pub fn ed25519(seed: &Seed, path: &DerivationPath) -> Result<SigningKey, DeriveError> {
    match seed.kind() {
        SeedKind::Ed25519 => {
            let key: Zeroizing<[u8; 32]> = Zeroizing::new(
                seed.expose()
                    .try_into()
                    .map_err(|_| DeriveError::InvalidKey)?,
            );
            Ok(SigningKey::from_bytes(&key))
        }
        SeedKind::Bip39 => Ok(SigningKey::from_bytes(
            &ed25519_node(seed.expose(), path)?.key,
        )),
        SeedKind::Secp256k1 => Err(DeriveError::OtherCurve),
    }
}

fn secp256k1_node(seed: &[u8], indices: &[u32]) -> Result<Node, DeriveError> {
    let mut node = split(&*hmac512(BIP32_KEY, &[seed])?);
    scalar(&node.key)?;
    for &index in indices {
        let parent = scalar(&node.key)?;
        let prefix: Zeroizing<Vec<u8>> = if index & HARDENED == 0 {
            Zeroizing::new(compressed(&node.secret()?.public_key())?.to_vec())
        } else {
            let mut prefix = Zeroizing::new(Vec::with_capacity(33));
            prefix.push(0);
            prefix.extend_from_slice(&node.key[..]);
            prefix
        };
        let output = hmac512(&node.chain[..], &[&prefix, &index.to_be_bytes()])?;
        let tweak = scalar(
            output[..32]
                .try_into()
                .map_err(|_| DeriveError::InvalidKey)?,
        )?;
        let child = Zeroizing::new(*tweak + *parent);
        if bool::from(child.is_zero()) {
            return Err(DeriveError::InvalidKey);
        }
        let mut key = Zeroizing::new([0u8; 32]);
        key.copy_from_slice(&child.to_repr());
        let mut chain = Zeroizing::new([0u8; 32]);
        chain.copy_from_slice(&output[32..]);
        node = Node { key, chain };
    }
    Ok(node)
}

fn ed25519_node(seed: &[u8], path: &DerivationPath) -> Result<Node, DeriveError> {
    if !path.is_hardened() {
        return Err(DeriveError::NotHardened);
    }
    let mut node = split(&*hmac512(SLIP10_ED25519_KEY, &[seed])?);
    for index in path.indices() {
        node = split(&*hmac512(
            &node.chain[..],
            &[&[0], &node.key[..], &index.to_be_bytes()],
        )?);
    }
    Ok(node)
}

fn scalar(bytes: &[u8; 32]) -> Result<Zeroizing<Scalar>, DeriveError> {
    let value: Option<Scalar> = Scalar::from_repr(FieldBytes::from(*bytes)).into();
    let value = Zeroizing::new(value.ok_or(DeriveError::InvalidKey)?);
    if bool::from(value.is_zero()) {
        return Err(DeriveError::InvalidKey);
    }
    Ok(value)
}

fn split(output: &[u8; 64]) -> Node {
    let mut key = Zeroizing::new([0u8; 32]);
    let mut chain = Zeroizing::new([0u8; 32]);
    key.copy_from_slice(&output[..32]);
    chain.copy_from_slice(&output[32..]);
    Node { key, chain }
}

fn hmac512(key: &[u8], parts: &[&[u8]]) -> Result<Zeroizing<[u8; 64]>, DeriveError> {
    let mut mac = Hmac::<Sha512>::new_from_slice(key).map_err(|_| DeriveError::InvalidKey)?;
    for part in parts {
        mac.update(part);
    }
    let mut out = Zeroizing::new([0u8; 64]);
    out.copy_from_slice(&mac.finalize().into_bytes());
    Ok(out)
}

#[cfg(test)]
mod tests {

    use bahaa_types::hex::encode as hex;
    use serde_json::Value;

    use super::*;

    fn bytes(text: &str) -> Vec<u8> {
        bahaa_types::hex::decode(text).unwrap()
    }

    fn vectors(json: &str) -> Vec<Value> {
        serde_json::from_str::<Value>(json)
            .unwrap()
            .as_array()
            .unwrap()
            .clone()
    }

    fn xprv_parts(xprv: &str) -> (u8, u32, String, String) {
        let data = bs58::decode(xprv).with_check(None).into_vec().unwrap();
        assert_eq!(data.len(), 78);
        assert_eq!(data[45], 0);
        let child = u32::from_be_bytes(data[9..13].try_into().unwrap());
        (data[4], child, hex(&data[13..45]), hex(&data[46..78]))
    }

    #[test]
    fn bip32_reference_vectors() {
        let mut checked = 0;
        for vector in vectors(include_str!("../tests/bip32_vectors.json")) {
            let seed = bytes(vector["seed"].as_str().unwrap());
            for chain in vector["chains"].as_array().unwrap() {
                let path: DerivationPath = chain["path"].as_str().unwrap().parse().unwrap();
                let node = secp256k1_node(&seed, path.indices()).unwrap();
                let (depth, child, chain_code, key) = xprv_parts(chain["xprv"].as_str().unwrap());
                assert_eq!(usize::from(depth), path.indices().len(), "{path}");
                assert_eq!(child, path.indices().last().copied().unwrap_or(0), "{path}");
                assert_eq!(hex(&node.chain[..]), chain_code, "{path}");
                assert_eq!(hex(&node.key[..]), key, "{path}");
                checked += 1;
            }
        }
        assert_eq!(checked, 17);
    }

    #[test]
    fn public_children_match_the_private_ones_on_every_bip32_vector() {
        let mut checked = 0;
        for vector in vectors(include_str!("../tests/bip32_vectors.json")) {
            let seed = bytes(vector["seed"].as_str().unwrap());
            for chain in vector["chains"].as_array().unwrap() {
                let path: DerivationPath = chain["path"].as_str().unwrap().parse().unwrap();
                let Some((&last, parent)) = path.indices().split_last() else {
                    continue;
                };
                let parent = secp256k1_node(&seed, parent).unwrap().public().unwrap();
                let child = parent.child(last);
                if last & HARDENED == 0 {
                    let expected = secp256k1_node(&seed, path.indices())
                        .unwrap()
                        .public()
                        .unwrap();
                    assert_eq!(child, Ok(expected), "{path}");
                    checked += 1;
                } else {
                    assert_eq!(child, Err(DeriveError::Hardened), "{path}");
                }
            }
        }
        assert_eq!(checked, 6);
    }

    #[test]
    fn trezor_master_keys() {
        for vector in vectors(include_str!("../tests/bip39_vectors.json")) {
            let seed = bytes(vector["seed"].as_str().unwrap());
            let node = secp256k1_node(&seed, &[]).unwrap();
            let (_, _, chain_code, key) = xprv_parts(vector["xprv"].as_str().unwrap());
            assert_eq!(hex(&node.chain[..]), chain_code);
            assert_eq!(hex(&node.key[..]), key);
        }
    }

    #[test]
    fn slip10_ed25519_reference_vectors() {
        let mut checked = 0;
        for vector in vectors(include_str!("../tests/slip10_vectors.json")) {
            let seed = bytes(vector["seed"].as_str().unwrap());
            for chain in vector["chains"].as_array().unwrap() {
                let path: DerivationPath = chain["path"].as_str().unwrap().parse().unwrap();
                let node = ed25519_node(&seed, &path).unwrap();
                assert_eq!(
                    hex(&node.chain[..]),
                    chain["chain_code"].as_str().unwrap(),
                    "{path}"
                );
                assert_eq!(
                    hex(&node.key[..]),
                    chain["private"].as_str().unwrap(),
                    "{path}"
                );
                let public = SigningKey::from_bytes(&node.key).verifying_key().to_bytes();
                assert_eq!(hex(&public), chain["public"].as_str().unwrap(), "{path}");
                checked += 1;
            }
        }
        assert_eq!(checked, 12);
    }

    #[test]
    fn ed25519_refuses_soft_steps() {
        let path: DerivationPath = "m/44'/607'/0'/0".parse().unwrap();
        assert!(matches!(
            ed25519_node(&[1; 64], &path),
            Err(DeriveError::NotHardened)
        ));
    }
}
