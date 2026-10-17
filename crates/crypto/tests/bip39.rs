#![allow(clippy::unwrap_used)]

use bahaa_crypto::{ExposeSecret, Mnemonic};
use serde_json::Value;

fn vectors() -> Vec<Value> {
    serde_json::from_str::<Value>(include_str!("bip39_vectors.json"))
        .unwrap()
        .as_array()
        .unwrap()
        .clone()
}

fn bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).unwrap())
        .collect()
}

#[test]
fn entropy_becomes_the_reference_phrase() {
    for vector in vectors() {
        let mnemonic = Mnemonic::from_entropy(&bytes(vector["entropy"].as_str().unwrap())).unwrap();
        assert_eq!(
            mnemonic.phrase().expose_secret(),
            vector["phrase"].as_str().unwrap()
        );
    }
}

#[test]
fn reference_phrases_parse_and_give_the_reference_seed() {
    for vector in vectors() {
        let phrase = vector["phrase"].as_str().unwrap();
        let mnemonic = Mnemonic::parse(phrase).unwrap();
        assert_eq!(mnemonic.phrase().expose_secret(), phrase);
        assert_eq!(
            mnemonic.to_seed("TREZOR").expose(),
            bytes(vector["seed"].as_str().unwrap())
        );
    }
}

#[test]
fn every_reference_vector_is_covered() {
    let vectors = vectors();
    assert_eq!(vectors.len(), 24);
    let words: Vec<usize> = vectors
        .iter()
        .map(|vector| vector["phrase"].as_str().unwrap().split(' ').count())
        .collect();
    for count in [12, 18, 24] {
        assert!(words.contains(&count));
    }
}
