use bahaa_chain::ChainError;
use bahaa_crypto::taproot_output;
use bahaa_types::{Address, BitcoinScript, BitcoinType};
use ripemd::Ripemd160;
use sha2::{Digest, Sha256};

const DUP: u8 = 0x76;
const HASH160: u8 = 0xa9;
const EQUAL: u8 = 0x87;
const EQUALVERIFY: u8 = 0x88;
const CHECKSIG: u8 = 0xac;
const WITNESS_V0: u8 = 0x00;
const WITNESS_V1: u8 = 0x51;
const PUSH_20: u8 = 0x14;
const PUSH_32: u8 = 0x20;

pub(crate) fn hash160(data: &[u8]) -> [u8; 20] {
    Ripemd160::digest(Sha256::digest(data)).into()
}

pub(crate) fn key_address(
    public: &[u8; 33],
    kind: BitcoinType,
    testnet: bool,
) -> Result<Address, ChainError> {
    let script = match kind {
        BitcoinType::NativeSegwit => BitcoinScript::WitnessKeyHash(hash160(public)),
        BitcoinType::Taproot => BitcoinScript::Taproot(
            taproot_output(public).map_err(|_| ChainError::Invalid("an unusable Taproot key"))?,
        ),
    };
    Ok(Address::Bitcoin { script, testnet })
}

pub(crate) fn locking(script: BitcoinScript) -> Vec<u8> {
    let with = |head: &[u8], body: &[u8], tail: &[u8]| [head, body, tail].concat();
    match script {
        BitcoinScript::PubkeyHash(hash) => {
            with(&[DUP, HASH160, PUSH_20], &hash, &[EQUALVERIFY, CHECKSIG])
        }
        BitcoinScript::ScriptHash(hash) => with(&[HASH160, PUSH_20], &hash, &[EQUAL]),
        BitcoinScript::WitnessKeyHash(hash) => with(&[WITNESS_V0, PUSH_20], &hash, &[]),
        BitcoinScript::WitnessScriptHash(hash) => with(&[WITNESS_V0, PUSH_32], &hash, &[]),
        BitcoinScript::Taproot(key) => with(&[WITNESS_V1, PUSH_32], &key, &[]),
    }
}

pub(crate) fn read(script: &[u8]) -> Option<BitcoinScript> {
    match script {
        [DUP, HASH160, PUSH_20, hash @ .., EQUALVERIFY, CHECKSIG] => {
            Some(BitcoinScript::PubkeyHash(hash.try_into().ok()?))
        }
        [HASH160, PUSH_20, hash @ .., EQUAL] => {
            Some(BitcoinScript::ScriptHash(hash.try_into().ok()?))
        }
        [WITNESS_V0, PUSH_20, hash @ ..] => {
            Some(BitcoinScript::WitnessKeyHash(hash.try_into().ok()?))
        }
        [WITNESS_V0, PUSH_32, hash @ ..] => {
            Some(BitcoinScript::WitnessScriptHash(hash.try_into().ok()?))
        }
        [WITNESS_V1, PUSH_32, key @ ..] => Some(BitcoinScript::Taproot(key.try_into().ok()?)),
        _ => None,
    }
}

pub(crate) fn dust(script: BitcoinScript) -> u64 {
    match script {
        BitcoinScript::PubkeyHash(_) => 546,
        BitcoinScript::ScriptHash(_) => 540,
        BitcoinScript::WitnessKeyHash(_) => 294,
        BitcoinScript::WitnessScriptHash(_) | BitcoinScript::Taproot(_) => 330,
    }
}

#[cfg(test)]
mod tests {
    use bahaa_types::hex;

    use super::*;

    fn parsed(text: &str) -> BitcoinScript {
        match Address::parse(bahaa_types::ChainFamily::Bitcoin, text).unwrap() {
            Address::Bitcoin { script, .. } => script,
            other => panic!("{other}"),
        }
    }

    #[test]
    fn bip84_public_keys_give_the_published_addresses() {
        for (key, address) in [
            (
                "0330d54fd0dd420a6e5f8d3624f5f3482cae350f79d5f0753bf5beef9c2d91af3c",
                "bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu",
            ),
            (
                "03025324888e429ab8e3dbaf1f7802648b9cd01e9b418485c5fa4c1b9b5700e1a6",
                "bc1q8c6fshw2dlwun7ekn9qwf37cu2rn755upcp6el",
            ),
        ] {
            let key: [u8; 33] = hex::decode_array(key).unwrap();
            assert_eq!(
                key_address(&key, BitcoinType::NativeSegwit, false)
                    .unwrap()
                    .to_string(),
                address
            );
        }
    }

    #[test]
    fn every_address_kind_has_its_locking_script_and_reads_back() {
        for (address, script, dust_limit) in [
            (
                "1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa",
                "76a91462e907b15cbf27d5425399ebf6f0fb50ebb88f1888ac",
                546,
            ),
            (
                "3J98t1WpEZ73CNmQviecrnyiWrnqRhWNLy",
                "a914b472a266d0bd89c13706a4132ccfb16f7c3b9fcb87",
                540,
            ),
            (
                "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4",
                "0014751e76e8199196d454941c45d1b3a323f1433bd6",
                294,
            ),
            (
                "bc1qrp33g0q5c5txsp9arysrx4k6zdkfs4nce4xj0gdcccefvpysxf3qccfmv3",
                "00201863143c14c5166804bd19203356da136c985678cd4d27a1b8c6329604903262",
                330,
            ),
            (
                "bc1p5cyxnuxmeuwuvkwfem96lqzszd02n6xdcjrs20cac6yqjjwudpxqkedrcr",
                "5120a60869f0dbcf1dc659c9cecbaf8050135ea9e8cdc487053f1dc6880949dc684c",
                330,
            ),
        ] {
            let kind = parsed(address);
            assert_eq!(hex::encode(&locking(kind)), script, "{address}");
            assert_eq!(read(&locking(kind)), Some(kind));
            assert_eq!(dust(kind), dust_limit);
        }
        for odd in [&b""[..], &[0x6a, 0x01, 0x00], &[0x00, 0x14, 1, 2, 3]] {
            assert_eq!(read(odd), None);
        }
    }
}
