use crate::tx::{Hash, sha256d, shown};

pub(crate) const HEADER_BYTES: usize = 80;

const TIME_AT: usize = 68;

pub(crate) fn block_hash(header: &[u8]) -> Option<Hash> {
    (header.len() == HEADER_BYTES).then(|| shown(sha256d(header)))
}

pub(crate) fn time(header: &[u8]) -> Option<u64> {
    if header.len() != HEADER_BYTES {
        return None;
    }
    let bytes = header.get(TIME_AT..TIME_AT + 4)?.try_into().ok()?;
    Some(u64::from(u32::from_le_bytes(bytes)))
}

#[cfg(test)]
mod tests {
    use bahaa_types::hex;

    use super::*;

    const GENESIS: &str = "0100000000000000000000000000000000000000000000000000000000000000000000003ba3edfd7a7b12b27ac72c3e67768f617fc81bc3888a51323a9fb8aa4b1e5e4a29ab5f49ffff001d1dac2b7c";

    #[test]
    fn the_genesis_header_has_its_hash_and_time() {
        let genesis = hex::decode(GENESIS).unwrap();
        assert_eq!(
            hex::encode(&block_hash(&genesis).unwrap()),
            "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f"
        );
        assert_eq!(time(&genesis), Some(1_231_006_505));
        assert_eq!(time(&genesis[..79]), None);
        assert_eq!(block_hash(&genesis[..79]), None);
    }
}
