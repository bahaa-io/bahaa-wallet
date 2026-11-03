use bahaa_chain::ChainError;
use bahaa_config::{BlockCheckpoint, RETARGET_BLOCKS};
use ruint::aliases::U256;

use crate::header::{HEADER_BYTES, block_hash};
use crate::tx::{Hash, sha256d, shown};

pub(crate) type Header = [u8; HEADER_BYTES];

const TIMESPAN: i64 = 14 * 24 * 60 * 60;
const PREVIOUS: std::ops::Range<usize> = 4..36;
const ROOT: std::ops::Range<usize> = 36..68;
const TIME: std::ops::Range<usize> = 68..72;
const BITS: std::ops::Range<usize> = 72..76;
const AMBIGUOUS_TX: usize = 64;

#[derive(Debug)]
pub(crate) struct Headers {
    first: u64,
    list: Vec<Header>,
}

impl Headers {
    pub(crate) fn start(
        checkpoint: &BlockCheckpoint,
        header: Header,
        limit: u32,
    ) -> Result<Self, ChainError> {
        if block_hash(&header) != Some(checkpoint.hash) || !has_work(&header, limit) {
            return Err(unproven("the checkpoint block"));
        }
        Ok(Self {
            first: checkpoint.height,
            list: vec![header],
        })
    }

    pub(crate) fn next(&self) -> u64 {
        self.first + self.list.len() as u64
    }

    pub(crate) fn get(&self, height: u64) -> Option<&Header> {
        self.list
            .get(usize::try_from(height.checked_sub(self.first)?).ok()?)
    }

    pub(crate) fn rewind(&mut self, depth: usize) {
        self.list
            .truncate(self.list.len().saturating_sub(depth).max(1));
    }

    pub(crate) fn links(&self, header: &Header) -> bool {
        self.list
            .last()
            .and_then(|last| block_hash(last))
            .is_some_and(|last| previous(header) == last)
    }

    pub(crate) fn extend(&mut self, headers: &[Header], limit: u32) -> Result<(), ChainError> {
        for header in headers {
            let height = self.next();
            let last = self.list.last().ok_or_else(ChainError::malformed)?;
            let expected = if height.is_multiple_of(RETARGET_BLOCKS) {
                let start = self
                    .get(height - RETARGET_BLOCKS)
                    .ok_or_else(|| unproven("a difficulty change"))?;
                retarget(time(start), time(last), bits(last), limit)
            } else {
                Some(bits(last))
            };
            if !self.links(header) || expected != Some(bits(header)) || !has_work(header, limit) {
                return Err(unproven("a block header"));
            }
            self.list.push(*header);
        }
        Ok(())
    }
}

pub(crate) fn has_work(header: &Header, limit: u32) -> bool {
    let (Some(target), Some(ceiling), Some(hash)) =
        (target(bits(header)), target(limit), block_hash(header))
    else {
        return false;
    };
    !target.is_zero() && target <= ceiling && U256::from_be_bytes(hash) <= target
}

pub(crate) fn proves(header: &Header, txid: &Hash, branch: &[Hash], pos: u64) -> bool {
    let mut node = shown(*txid);
    let mut at = pos;
    for sibling in branch {
        let sibling = shown(*sibling);
        node = if at & 1 == 1 {
            sha256d(&[sibling, node].concat())
        } else {
            sha256d(&[node, sibling].concat())
        };
        at >>= 1;
    }
    at == 0 && header[ROOT] == node
}

pub(crate) fn ambiguous(stripped_size: usize) -> bool {
    stripped_size == AMBIGUOUS_TX
}

pub(crate) fn unproven(what: &str) -> ChainError {
    ChainError::Network(format!("a server could not prove {what}"))
}

fn previous(header: &Header) -> Hash {
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&header[PREVIOUS]);
    shown(hash)
}

fn time(header: &Header) -> i64 {
    i64::from(u32::from_le_bytes(word(header, TIME)))
}

fn bits(header: &Header) -> u32 {
    u32::from_le_bytes(word(header, BITS))
}

fn word(header: &Header, range: std::ops::Range<usize>) -> [u8; 4] {
    let mut bytes = [0u8; 4];
    bytes.copy_from_slice(&header[range]);
    bytes
}

fn target(bits: u32) -> Option<U256> {
    if bits & 0x0080_0000 != 0 {
        return None;
    }
    let size = usize::try_from(bits >> 24).ok()?;
    let mantissa = U256::from(bits & 0x007f_ffff);
    if size <= 3 {
        Some(mantissa >> (8 * (3 - size)))
    } else {
        mantissa.checked_shl(8 * (size - 3))
    }
}

fn compact(target: U256) -> u32 {
    let mut size = target.bit_len().div_ceil(8);
    let mut mantissa = if size <= 3 {
        target << (8 * (3 - size))
    } else {
        target >> (8 * (size - 3))
    }
    .as_limbs()[0];
    if mantissa & 0x0080_0000 != 0 {
        mantissa >>= 8;
        size += 1;
    }
    u32::try_from(mantissa).unwrap_or(0) | (u32::try_from(size).unwrap_or(0) << 24)
}

fn retarget(first: i64, last: i64, last_bits: u32, limit: u32) -> Option<u32> {
    let span = (last - first).clamp(TIMESPAN / 4, TIMESPAN * 4);
    let scaled = target(last_bits)?.checked_mul(U256::from(span))? / U256::from(TIMESPAN);
    Some(compact(scaled.min(target(limit)?)))
}

#[cfg(test)]
mod tests {
    use bahaa_types::hex;
    use serde_json::Value;

    use super::*;

    const MAINNET_LIMIT: u32 = 0x1d00_ffff;

    fn fixture() -> Value {
        serde_json::from_str(include_str!("../tests/fixtures/headers.json")).unwrap()
    }

    fn header(text: &str) -> Header {
        hex::decode_array(text).unwrap()
    }

    fn mainnet() -> (u64, Vec<Header>) {
        let fixture = fixture();
        let bytes = hex::decode(fixture["headers"].as_str().unwrap()).unwrap();
        let list = bytes
            .chunks(HEADER_BYTES)
            .map(|chunk| chunk.try_into().unwrap())
            .collect();
        (fixture["start"].as_u64().unwrap(), list)
    }

    #[test]
    fn compact_targets_round_trip_like_bitcoin_core() {
        for bits in [
            0x1d00_ffff,
            0x1702_8c61,
            0x1e03_77ae,
            0x207f_ffff,
            0x0101_0000,
        ] {
            assert_eq!(compact(target(bits).unwrap()), bits, "{bits:x}");
        }
        assert_eq!(compact(target(0x0300_0001).unwrap()), 0x0101_0000);
        assert_eq!(target(0x0180_0000), None);
        assert_eq!(compact(target(0x0412_3456).unwrap()), 0x0412_3456);
    }

    #[test]
    fn the_mainnet_difficulty_change_is_recomputed_exactly() {
        let (start, headers) = mainnet();
        let boundary = usize::try_from(973_728 - start).unwrap();
        let opening = header(fixture()["first_of_period"].as_str().unwrap());
        let closing = &headers[boundary - 1];
        assert_ne!(bits(closing), bits(&headers[boundary]));
        assert_eq!(
            retarget(time(&opening), time(closing), bits(closing), MAINNET_LIMIT),
            Some(bits(&headers[boundary]))
        );
    }

    #[test]
    fn real_headers_link_and_carry_their_work() {
        let (start, list) = mainnet();
        let checkpoint = BlockCheckpoint {
            height: start,
            hash: block_hash(&list[0]).unwrap(),
        };
        let mut chain = Headers::start(&checkpoint, list[0], MAINNET_LIMIT).unwrap();
        chain.extend(&list[1..8], MAINNET_LIMIT).unwrap();
        assert_eq!(chain.next(), start + 8);
        assert!(matches!(
            chain.extend(&list[8..9], MAINNET_LIMIT),
            Err(ChainError::Network(_))
        ));
        let mut forged = list[2];
        forged[72] ^= 1;
        let mut fresh = Headers::start(&checkpoint, list[0], MAINNET_LIMIT).unwrap();
        fresh.extend(&list[1..2], MAINNET_LIMIT).unwrap();
        assert!(fresh.extend(&[forged], MAINNET_LIMIT).is_err());
        let mut skipped = Headers::start(&checkpoint, list[0], MAINNET_LIMIT).unwrap();
        assert!(skipped.extend(&list[2..3], MAINNET_LIMIT).is_err());
        let wrong = BlockCheckpoint {
            hash: [7; 32],
            ..checkpoint
        };
        assert!(Headers::start(&wrong, list[0], MAINNET_LIMIT).is_err());
        assert!(list.iter().all(|header| has_work(header, MAINNET_LIMIT)));
        let mut lazy = list[3];
        lazy[76..80].copy_from_slice(&[0; 4]);
        assert!(!has_work(&lazy, MAINNET_LIMIT));
    }

    #[test]
    fn a_real_transaction_is_proven_inside_its_block() {
        let proof = &fixture()["proof"];
        let block = header(proof["header"].as_str().unwrap());
        let txid: Hash = hex::decode_array(proof["txid"].as_str().unwrap()).unwrap();
        let branch: Vec<Hash> = proof["merkle"]
            .as_array()
            .unwrap()
            .iter()
            .map(|node| hex::decode_array(node.as_str().unwrap()).unwrap())
            .collect();
        let pos = proof["pos"].as_u64().unwrap();
        assert!(proves(&block, &txid, &branch, pos));
        assert!(!proves(&block, &txid, &branch, pos + 1));
        assert!(!proves(&block, &txid, &branch[1..], pos));
        assert!(!proves(&block, &[1; 32], &branch, pos));
        assert!(!proves(&block, &txid, &branch, pos | 1 << 20));
        assert!(ambiguous(64) && !ambiguous(65));
    }
}
