use bahaa_types::Address;
use sha2::{Digest, Sha256};

use crate::request::Content;

const PROOF_PREFIX: &[u8] = b"ton-proof-item-v2/";
const CONNECT_PREFIX: &[u8] = b"ton-connect";
const DATA_PREFIX: &[u8] = b"ton-connect/sign-data/";
const MARK: [u8; 2] = [0xff, 0xff];

pub fn proof_hash(
    address: &Address,
    domain: &str,
    timestamp: u64,
    payload: &str,
) -> Option<[u8; 32]> {
    let Address::Ton { workchain, hash } = address else {
        return None;
    };
    let length = u32::try_from(domain.len()).ok()?;
    let message = Sha256::new()
        .chain_update(PROOF_PREFIX)
        .chain_update(i32::from(*workchain).to_be_bytes())
        .chain_update(hash)
        .chain_update(length.to_le_bytes())
        .chain_update(domain.as_bytes())
        .chain_update(timestamp.to_le_bytes())
        .chain_update(payload.as_bytes())
        .finalize();
    Some(
        Sha256::new()
            .chain_update(MARK)
            .chain_update(CONNECT_PREFIX)
            .chain_update(message)
            .finalize()
            .into(),
    )
}

pub fn data_hash(
    address: &Address,
    domain: &str,
    timestamp: u64,
    content: &Content,
) -> Option<[u8; 32]> {
    let Address::Ton { workchain, hash } = address else {
        return None;
    };
    let (prefix, data): (&[u8], &[u8]) = match content {
        Content::Text(text) => (b"txt", text.as_bytes()),
        Content::Binary(bytes) => (b"bin", bytes),
        Content::Cell { .. } => return None,
    };
    Some(
        Sha256::new()
            .chain_update(MARK)
            .chain_update(DATA_PREFIX)
            .chain_update(i32::from(*workchain).to_be_bytes())
            .chain_update(hash)
            .chain_update(u32::try_from(domain.len()).ok()?.to_be_bytes())
            .chain_update(domain.as_bytes())
            .chain_update(timestamp.to_be_bytes())
            .chain_update(prefix)
            .chain_update(u32::try_from(data.len()).ok()?.to_be_bytes())
            .chain_update(data)
            .finalize()
            .into(),
    )
}

#[cfg(test)]
mod tests {
    use bahaa_types::hex;
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use ed25519_dalek::{Signature, VerifyingKey};

    use super::*;

    fn verifies(public: &str, hash: [u8; 32], signature: &[u8]) -> bool {
        let key = VerifyingKey::from_bytes(&hex::decode_array::<32>(public).unwrap()).unwrap();
        let signature = Signature::from_slice(signature).unwrap();
        key.verify_strict(&hash, &signature).is_ok()
    }

    #[test]
    fn a_real_sign_in_proof_verifies() {
        let (address, _) =
            Address::parse_ton("EQBDMWTZnPVO5ccZIb6ttsVArQa1_Pwe47fBggvhG8OtgOf0").unwrap();
        let payload = "nmqQIzwMkZaOWonYxHXFtrXyziCbxOj9zlUfR7x5B1sQCUtG8Bk5zmMPkcbEzmCJ";
        let hash = proof_hash(&address, "tonplay.io", 1_678_206_305, payload).unwrap();
        let signature = STANDARD
            .decode("VJPpOKHfg79Z1We7SCnI+qwSShIm8uyFpUM5H5tnTxKzafsHrXkg2ivoouXeCSlgZoHj/y9utZh9vEh4oo5LAQ==")
            .unwrap();
        let public = "df00c53eb5a7fd84535c559859c084188fdae82faa40821706ba6025f3ac9bfc";
        assert!(verifies(public, hash, &signature));
        let shifted = proof_hash(&address, "tonplay.io", 1_678_206_306, payload).unwrap();
        assert!(!verifies(public, shifted, &signature));
    }

    #[test]
    fn real_text_and_binary_signatures_from_the_demo_app_verify() {
        let (address, _) = Address::parse_ton(
            "0:f4809e5ffac9dc42a6b1d94c5e74ad5fd86378de675c805f2274d0055cbc9378",
        )
        .unwrap();
        let public = "22e795a07e832fc9084ca35a488a711f1dbedef637d4e886a6997d93ee2c2e37";
        let domain = "ton-connect.github.io";
        let text = Content::Text("Hello, TON!".repeat(100));
        let signature = hex::decode("7bc628f6d634ab6ddaf10463742b13f0ede3cb828737d9ce1962cc808fbfe7035e77c1a3d0b682acf02d645cc1a244992b276552c0e1c57d30b03c2820d73d01").unwrap();
        assert!(verifies(
            public,
            data_hash(&address, domain, 1_747_759_882, &text).unwrap(),
            &signature
        ));
        let binary = Content::Binary(b"Hello, TON!".to_vec());
        let signature = hex::decode("9cf4c1c16b47afce46940eb9cd410894f31544b74206c2254bb1651f9b32cf5b0e482b78a2e8251e54d3517fae4b06c6f23546667d63ff62dccce70451698d01").unwrap();
        assert!(verifies(
            public,
            data_hash(&address, domain, 1_747_760_435, &binary).unwrap(),
            &signature
        ));
        let cell = Content::Cell {
            schema: String::new(),
            cell: Vec::new(),
        };
        assert_eq!(data_hash(&address, domain, 0, &cell), None);
    }
}
