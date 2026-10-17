#![allow(clippy::unwrap_used)]

use std::fmt::Write;

use bahaa_crypto::{DerivationPath, Mnemonic};
use k256::elliptic_curve::sec1::ToEncodedPoint;
use sha3::{Digest, Keccak256};

const HARDHAT: &str = "test test test test test test test test test test test junk";

fn path(text: &str) -> DerivationPath {
    text.parse().unwrap()
}

fn evm_address(phrase: &str, index: u32) -> String {
    let seed = Mnemonic::parse(phrase).unwrap().to_seed("");
    let key = bahaa_crypto::secp256k1(&seed, &path(&format!("m/44'/60'/0'/0/{index}"))).unwrap();
    let point = key.public_key().to_encoded_point(false);
    let hash = Keccak256::digest(&point.as_bytes()[1..]);
    hash[12..].iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

#[test]
fn derives_the_accounts_every_ethereum_tool_derives() {
    assert_eq!(
        evm_address(HARDHAT, 0),
        "f39fd6e51aad88f6f4ce6ab8827279cfffb92266"
    );
    assert_eq!(
        evm_address(HARDHAT, 1),
        "70997970c51812dc3a010c7d01b50e0d17dc79c8"
    );
    assert_eq!(
        evm_address(HARDHAT, 2),
        "3c44cdddb6a900fa2b585dd299e03d12fa4293bc"
    );
}

#[test]
fn ton_keys_need_a_hardened_path() {
    let seed = Mnemonic::parse(HARDHAT).unwrap().to_seed("");
    assert!(bahaa_crypto::ed25519(&seed, &path("m/44'/607'/0'")).is_ok());
    assert!(bahaa_crypto::ed25519(&seed, &path("m/44'/607'/0'/0")).is_err());
}

#[test]
fn passphrases_lead_to_different_wallets() {
    let mnemonic = Mnemonic::parse(HARDHAT).unwrap();
    let plain = bahaa_crypto::ed25519(&mnemonic.to_seed(""), &path("m/44'/607'/0'")).unwrap();
    let hidden = bahaa_crypto::ed25519(&mnemonic.to_seed("extra"), &path("m/44'/607'/0'")).unwrap();
    assert_ne!(plain.verifying_key(), hidden.verifying_key());
}
