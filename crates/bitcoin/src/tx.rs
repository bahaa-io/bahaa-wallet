use bahaa_chain::ChainError;
use bahaa_types::hex;
use sha2::{Digest, Sha256};

pub(crate) const VERSION: u32 = 2;
pub(crate) const SEQUENCE: u32 = 0xffff_fffd;
pub(crate) const SIGHASH_ALL: u8 = 1;
pub(crate) const MAX_SIGNATURE: usize = 73;
pub(crate) const MAX_MONEY: u64 = 2_100_000_000_000_000;

const MAX_BYTES: usize = 4_000_000;
const WITNESS_MARKER: u8 = 0x00;
const WITNESS_FLAG: u8 = 0x01;
const KEY_CODE: [u8; 4] = [0x19, 0x76, 0xa9, 0x14];
const KEY_CODE_END: [u8; 2] = [0x88, 0xac];

pub(crate) type Hash = [u8; 32];

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Outpoint {
    pub txid: Hash,
    pub vout: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Input {
    pub previous: Outpoint,
    pub script: Vec<u8>,
    pub sequence: u32,
    pub witness: Vec<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Output {
    pub value: u64,
    pub script: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Tx {
    pub version: u32,
    pub inputs: Vec<Input>,
    pub outputs: Vec<Output>,
    pub lock_time: u32,
}

impl Tx {
    pub(crate) fn bytes(&self) -> Vec<u8> {
        self.encode(self.inputs.iter().any(|input| !input.witness.is_empty()))
    }

    pub(crate) fn id(&self) -> Hash {
        shown(sha256d(&self.encode(false)))
    }

    pub(crate) fn vsize(&self) -> u64 {
        let base = self.encode(false).len() as u64;
        let total = self.bytes().len() as u64;
        (base * 3 + total).div_ceil(4)
    }

    pub(crate) fn sighash(&self, index: usize, key_hash: &[u8; 20], value: u64) -> Hash {
        let mut prevouts = Vec::with_capacity(self.inputs.len() * 36);
        let mut sequences = Vec::with_capacity(self.inputs.len() * 4);
        for input in &self.inputs {
            outpoint(&mut prevouts, &input.previous);
            sequences.extend_from_slice(&input.sequence.to_le_bytes());
        }
        let mut outputs = Vec::new();
        for output in &self.outputs {
            write_output(&mut outputs, output);
        }
        let input = &self.inputs[index];
        let mut preimage = Vec::with_capacity(182);
        preimage.extend_from_slice(&self.version.to_le_bytes());
        preimage.extend_from_slice(&sha256d(&prevouts));
        preimage.extend_from_slice(&sha256d(&sequences));
        outpoint(&mut preimage, &input.previous);
        preimage.extend_from_slice(&KEY_CODE);
        preimage.extend_from_slice(key_hash);
        preimage.extend_from_slice(&KEY_CODE_END);
        preimage.extend_from_slice(&value.to_le_bytes());
        preimage.extend_from_slice(&input.sequence.to_le_bytes());
        preimage.extend_from_slice(&sha256d(&outputs));
        preimage.extend_from_slice(&self.lock_time.to_le_bytes());
        preimage.extend_from_slice(&u32::from(SIGHASH_ALL).to_le_bytes());
        sha256d(&preimage)
    }

    fn encode(&self, witness: bool) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&self.version.to_le_bytes());
        if witness {
            out.extend_from_slice(&[WITNESS_MARKER, WITNESS_FLAG]);
        }
        compact(&mut out, self.inputs.len());
        for input in &self.inputs {
            outpoint(&mut out, &input.previous);
            compact(&mut out, input.script.len());
            out.extend_from_slice(&input.script);
            out.extend_from_slice(&input.sequence.to_le_bytes());
        }
        compact(&mut out, self.outputs.len());
        for output in &self.outputs {
            write_output(&mut out, output);
        }
        if witness {
            for input in &self.inputs {
                compact(&mut out, input.witness.len());
                for item in &input.witness {
                    compact(&mut out, item.len());
                    out.extend_from_slice(item);
                }
            }
        }
        out.extend_from_slice(&self.lock_time.to_le_bytes());
        out
    }
}

pub(crate) fn parse(bytes: &[u8]) -> Result<Tx, ChainError> {
    if bytes.len() > MAX_BYTES {
        return Err(ChainError::malformed());
    }
    let mut reader = Reader(bytes);
    let version = reader.u32()?;
    let witness = reader.0.first() == Some(&WITNESS_MARKER);
    if witness && reader.take(2)? != [WITNESS_MARKER, WITNESS_FLAG] {
        return Err(ChainError::malformed());
    }
    let mut inputs = Vec::new();
    for _ in 0..reader.count()? {
        let txid = shown(
            reader
                .take(32)?
                .try_into()
                .map_err(|_| ChainError::malformed())?,
        );
        let vout = reader.u32()?;
        let script = reader.var()?.to_vec();
        let sequence = reader.u32()?;
        inputs.push(Input {
            previous: Outpoint { txid, vout },
            script,
            sequence,
            witness: Vec::new(),
        });
    }
    let mut outputs = Vec::new();
    for _ in 0..reader.count()? {
        let value = reader.u64()?;
        if value > MAX_MONEY {
            return Err(ChainError::malformed());
        }
        outputs.push(Output {
            value,
            script: reader.var()?.to_vec(),
        });
    }
    if inputs.is_empty() || outputs.is_empty() {
        return Err(ChainError::malformed());
    }
    if witness {
        for input in &mut inputs {
            for _ in 0..reader.count()? {
                input.witness.push(reader.var()?.to_vec());
            }
        }
        if inputs.iter().all(|input| input.witness.is_empty()) {
            return Err(ChainError::malformed());
        }
    }
    let lock_time = reader.u32()?;
    if !reader.0.is_empty() {
        return Err(ChainError::malformed());
    }
    Ok(Tx {
        version,
        inputs,
        outputs,
        lock_time,
    })
}

pub(crate) fn signature(compact: &[u8; 64]) -> Vec<u8> {
    let (r, s) = compact.split_at(32);
    let (r, s) = (integer(r), integer(s));
    let mut out = vec![0x30, u8::try_from(4 + r.len() + s.len()).unwrap_or(u8::MAX)];
    for part in [r, s] {
        out.push(0x02);
        out.push(u8::try_from(part.len()).unwrap_or(u8::MAX));
        out.extend_from_slice(&part);
    }
    out.push(SIGHASH_ALL);
    out
}

pub(crate) fn signs_everything(signature: &[u8]) -> bool {
    signature.len() <= MAX_SIGNATURE
        && signature.first() == Some(&0x30)
        && signature.get(1).map(|len| usize::from(*len) + 3) == Some(signature.len())
        && signature.last() == Some(&SIGHASH_ALL)
}

pub(crate) fn id_text(id: &Hash) -> String {
    hex::encode(id)
}

pub(crate) fn id_of(text: &str) -> Result<Hash, ChainError> {
    hex::decode_array(text).ok_or(ChainError::Invalid("not a Bitcoin transaction id"))
}

fn integer(bytes: &[u8]) -> Vec<u8> {
    let start = bytes
        .iter()
        .position(|byte| *byte != 0)
        .unwrap_or(bytes.len() - 1);
    let mut out = Vec::with_capacity(33);
    if bytes[start] & 0x80 != 0 {
        out.push(0);
    }
    out.extend_from_slice(&bytes[start..]);
    out
}

pub(crate) fn sha256d(data: &[u8]) -> Hash {
    Sha256::digest(Sha256::digest(data)).into()
}

pub(crate) fn shown(mut hash: Hash) -> Hash {
    hash.reverse();
    hash
}

fn outpoint(out: &mut Vec<u8>, point: &Outpoint) {
    out.extend_from_slice(&shown(point.txid));
    out.extend_from_slice(&point.vout.to_le_bytes());
}

fn write_output(out: &mut Vec<u8>, output: &Output) {
    out.extend_from_slice(&output.value.to_le_bytes());
    compact(out, output.script.len());
    out.extend_from_slice(&output.script);
}

fn compact(out: &mut Vec<u8>, value: usize) {
    if let Ok(small) = u8::try_from(value)
        && small < 0xfd
    {
        out.push(small);
    } else if let Ok(value) = u16::try_from(value) {
        out.push(0xfd);
        out.extend_from_slice(&value.to_le_bytes());
    } else if let Ok(value) = u32::try_from(value) {
        out.push(0xfe);
        out.extend_from_slice(&value.to_le_bytes());
    } else {
        out.push(0xff);
        out.extend_from_slice(&u64::try_from(value).unwrap_or(u64::MAX).to_le_bytes());
    }
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], ChainError> {
        if count > self.0.len() {
            return Err(ChainError::malformed());
        }
        let (head, rest) = self.0.split_at(count);
        self.0 = rest;
        Ok(head)
    }

    fn u32(&mut self) -> Result<u32, ChainError> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes(
            bytes.try_into().map_err(|_| ChainError::malformed())?,
        ))
    }

    fn u64(&mut self) -> Result<u64, ChainError> {
        let bytes = self.take(8)?;
        Ok(u64::from_le_bytes(
            bytes.try_into().map_err(|_| ChainError::malformed())?,
        ))
    }

    fn count(&mut self) -> Result<usize, ChainError> {
        let [first] = self.take(1)? else {
            return Err(ChainError::malformed());
        };
        let (value, least) = match first {
            0xfd => (u64::from(u16::from_le_bytes(self.array()?)), 0xfd),
            0xfe => (u64::from(u32::from_le_bytes(self.array()?)), 0x1_0000),
            0xff => (u64::from_le_bytes(self.array()?), 0x1_0000_0000),
            small => (u64::from(*small), 0),
        };
        let value = usize::try_from(value).map_err(|_| ChainError::malformed())?;
        if value < least || value > self.0.len() {
            return Err(ChainError::malformed());
        }
        Ok(value)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], ChainError> {
        self.take(N)?
            .try_into()
            .map_err(|_| ChainError::malformed())
    }

    fn var(&mut self) -> Result<&'a [u8], ChainError> {
        let len = self.count()?;
        self.take(len)
    }
}

#[cfg(fuzzing)]
pub(crate) fn fuzz(bytes: &[u8]) {
    if let Ok(tx) = parse(bytes) {
        assert_eq!(tx.bytes(), bytes);
        let _ = tx.id();
        let _ = tx.vsize();
        let _ = tx.sighash(0, &[0; 20], 0);
    }
}

#[cfg(test)]
mod tests {
    use bahaa_crypto::{SecretKey, secp256k1_sign};

    use super::*;

    const UNSIGNED: &str = "0100000002fff7f7881a8099afa6940d42d1e7f6362bec38171ea3edf433541db4e4ad969f0000000000eeffffffef51e1b804cc89d182d279655c3aa89e815b1b309fe287d9b2b55d57b90ec68a0100000000ffffffff02202cb206000000001976a9148280b37df378db99f66f85c95a783a76ac7a6d5988ac9093510d000000001976a9143bde42dbee7e4dbe6a21b2d50ce2f0167faa815988ac11000000";
    const SIGNED: &str = "01000000000102fff7f7881a8099afa6940d42d1e7f6362bec38171ea3edf433541db4e4ad969f00000000494830450221008b9d1dc26ba6a9cb62127b02742fa9d754cd3bebf337f7a55d114c8e5cdd30be022040529b194ba3f9281a99f2b1c0a19c0489bc22ede944ccf4ecbab4cc618ef3ed01eeffffffef51e1b804cc89d182d279655c3aa89e815b1b309fe287d9b2b55d57b90ec68a0100000000ffffffff02202cb206000000001976a9148280b37df378db99f66f85c95a783a76ac7a6d5988ac9093510d000000001976a9143bde42dbee7e4dbe6a21b2d50ce2f0167faa815988ac000247304402203609e17b84f6a7d30c80bfa610b5b4542f32a8a0d5447a12fb1366d7f01cc44a0220573a954c4518331561406f90300e8f3358f51928d43c212a8caed02de67eebee0121025476c2e83188368da1ff3e292e7acafcdb3566bb0ad253f62fc70f07aeee635711000000";

    fn bytes(text: &str) -> Vec<u8> {
        hex::decode(text).unwrap()
    }

    #[test]
    fn the_bip143_example_signs_to_the_published_witness() {
        let unsigned = parse(&bytes(UNSIGNED)).unwrap();
        assert_eq!(unsigned.bytes(), bytes(UNSIGNED));
        let key_hash = hex::decode_array("1d0f172a0ecb48aee1be1f2687d2963ae33f71a1").unwrap();
        let sighash = unsigned.sighash(1, &key_hash, 600_000_000);
        assert_eq!(
            hex::encode(&sighash),
            "c37af31116d1b27caf68aae9e3ac82f1477929014d5b917657d0eb49478cb670"
        );
        let key = SecretKey::from_slice(&bytes(
            "619c335025c7f4012e556c2a58b2506e30b8511b53ade95ea316fd8c3286feb9",
        ))
        .unwrap();
        let (compact, _) = secp256k1_sign(&key, &sighash).unwrap();
        let signed = parse(&bytes(SIGNED)).unwrap();
        assert_eq!(signed.bytes(), bytes(SIGNED));
        assert_eq!(signed.inputs[1].witness[0], signature(&compact));
        assert!(signs_everything(&signed.inputs[1].witness[0]));
    }

    #[test]
    fn a_mainnet_transaction_has_its_published_id_size_and_fee() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/mainnet.json")).unwrap();
        let raw = bytes(fixture["hex"].as_str().unwrap());
        let tx = parse(&raw).unwrap();
        assert_eq!(tx.bytes(), raw);
        assert_eq!(id_text(&tx.id()), fixture["txid"].as_str().unwrap());
        assert_eq!(tx.vsize(), fixture["vsize"].as_u64().unwrap());
        let prevouts = fixture["prevouts"].as_array().unwrap();
        let spent: u64 = prevouts
            .iter()
            .map(|out| out["value"].as_u64().unwrap())
            .sum();
        let paid: u64 = tx.outputs.iter().map(|out| out.value).sum();
        assert_eq!(spent - paid, fixture["fee"].as_u64().unwrap());
        for (input, prevout) in tx.inputs.iter().zip(prevouts) {
            let key: [u8; 33] = input.witness[1].as_slice().try_into().unwrap();
            let script = crate::script::locking(bahaa_types::BitcoinScript::WitnessKeyHash(
                crate::script::hash160(&key),
            ));
            assert_eq!(hex::encode(&script), prevout["script"].as_str().unwrap());
            assert!(signs_everything(&input.witness[0]));
        }
    }

    #[test]
    fn signatures_are_minimal_der_with_sighash_all() {
        let mut compact = [0u8; 64];
        compact[0] = 0x80;
        compact[63] = 1;
        let encoded = signature(&compact);
        assert_eq!(&encoded[..5], &[0x30, 0x26, 0x02, 0x21, 0x00]);
        assert_eq!(
            &encoded[encoded.len() - 4..],
            &[0x02, 0x01, 0x01, SIGHASH_ALL]
        );
        assert!(signs_everything(&encoded));
        let mut other = encoded.clone();
        *other.last_mut().unwrap() = 0x02;
        assert!(!signs_everything(&other));
    }

    #[test]
    fn broken_or_padded_transactions_are_refused() {
        let good = bytes(SIGNED);
        let mut trailing = good.clone();
        trailing.push(0);
        let mut no_witness_flag = good.clone();
        no_witness_flag[5] = 0x02;
        let mut long_count = bytes(UNSIGNED);
        long_count.splice(4..5, [0xfd, 0x02, 0x00]);
        for bad in [
            &good[..good.len() - 1],
            &trailing[..],
            &no_witness_flag[..],
            &long_count[..],
            &[1, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            &[],
        ] {
            assert!(parse(bad).is_err(), "{}", hex::encode(bad));
        }
    }
}
