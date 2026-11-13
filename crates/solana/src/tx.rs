use bahaa_chain::{ChainError, TxId};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

pub(crate) type Key = [u8; 32];

pub(crate) const MAX_TX: usize = 1232;
const SIGNATURE: usize = 64;
const VERSIONED: u8 = 0x80;
const V1: u8 = 0x81;
const MAX_V1: usize = 4096;
const MAX_V1_ITEMS: u8 = 64;
const V1_CONFIG: u32 = 0b1_1111;
const MAX_LEN: usize = 0xffff;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Meta {
    pub key: Key,
    pub signer: bool,
    pub writable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Instruction {
    pub program: Key,
    pub accounts: Vec<Meta>,
    pub data: Vec<u8>,
}

#[derive(Debug)]
pub(crate) struct Parsed {
    pub signature: [u8; SIGNATURE],
    pub payer: Key,
    pub blockhash: Key,
    pub message: Vec<u8>,
    pub instructions: Vec<Instruction>,
}

pub(crate) fn compile(
    payer: Key,
    blockhash: Key,
    instructions: &[Instruction],
) -> Result<Vec<u8>, ChainError> {
    let mut metas = vec![Meta {
        key: payer,
        signer: true,
        writable: true,
    }];
    let programs = instructions.iter().map(|instruction| Meta {
        key: instruction.program,
        signer: false,
        writable: false,
    });
    for meta in instructions
        .iter()
        .flat_map(|instruction| instruction.accounts.iter().cloned())
        .chain(programs)
    {
        match metas.iter_mut().find(|known| known.key == meta.key) {
            Some(known) => {
                known.signer |= meta.signer;
                known.writable |= meta.writable;
            }
            None => metas.push(meta),
        }
    }
    metas.sort_by_key(|meta| (meta.key != payer, !meta.signer, !meta.writable, meta.key));
    let signers = metas.iter().filter(|meta| meta.signer).count();
    let called = |meta: &&Meta| {
        instructions
            .iter()
            .any(|instruction| instruction.program == meta.key)
    };
    if signers != 1 || metas.iter().filter(called).any(|meta| meta.writable) {
        return Err(ChainError::Invalid("a transfer has one signer"));
    }
    let readonly = metas.iter().filter(|meta| !meta.writable).count();
    let index = |key: &Key| -> Result<u8, ChainError> {
        metas
            .iter()
            .position(|meta| meta.key == *key)
            .and_then(|at| u8::try_from(at).ok())
            .ok_or(ChainError::Invalid("too many accounts in one transaction"))
    };
    let mut out = vec![
        1,
        0,
        u8::try_from(readonly)
            .map_err(|_| ChainError::Invalid("too many accounts in one transaction"))?,
    ];
    write_len(&mut out, metas.len())?;
    for meta in &metas {
        out.extend_from_slice(&meta.key);
    }
    out.extend_from_slice(&blockhash);
    write_len(&mut out, instructions.len())?;
    for instruction in instructions {
        out.push(index(&instruction.program)?);
        write_len(&mut out, instruction.accounts.len())?;
        for meta in &instruction.accounts {
            out.push(index(&meta.key)?);
        }
        write_len(&mut out, instruction.data.len())?;
        out.extend_from_slice(&instruction.data);
    }
    if out.len() + 1 + SIGNATURE > MAX_TX {
        return Err(ChainError::Invalid(
            "the transaction is too large, shorten the memo",
        ));
    }
    Ok(out)
}

pub(crate) fn sign(message: &[u8], key: &SigningKey) -> (Vec<u8>, [u8; SIGNATURE]) {
    let signature = key.sign(message).to_bytes();
    (transaction(message, &signature), signature)
}

pub(crate) fn id(signature: &[u8; SIGNATURE]) -> Result<TxId, ChainError> {
    TxId::new(bs58::encode(signature).into_string())
}

pub(crate) fn transaction(message: &[u8], signature: &[u8; SIGNATURE]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + SIGNATURE + message.len());
    out.push(1);
    out.extend_from_slice(signature);
    out.extend_from_slice(message);
    out
}

pub(crate) struct Wire<'a> {
    pub signatures: Vec<[u8; SIGNATURE]>,
    pub message: &'a [u8],
    pub versioned: bool,
    pub header: [u8; 3],
    pub keys: Vec<Key>,
    pub blockhash: Key,
    pub instructions: Vec<Compiled>,
    pub lookups: Vec<Lookup>,
}

pub(crate) struct Lookup {
    pub writable: usize,
    pub readonly: usize,
}

pub(crate) struct Compiled {
    pub program: u8,
    pub accounts: Vec<u8>,
    pub data: Vec<u8>,
}

pub(crate) fn read(bytes: &[u8]) -> Option<Wire<'_>> {
    let mut reader = Reader(bytes);
    let count = read_len(&mut reader)?;
    if count == 0 || count > MAX_TX / SIGNATURE {
        return None;
    }
    let mut signatures = Vec::with_capacity(count);
    for _ in 0..count {
        signatures.push(reader.array()?);
    }
    let message = reader.0;
    let mut reader = Reader(message);
    let versioned = *message.first()? & VERSIONED != 0;
    if versioned && reader.byte()? != VERSIONED {
        return None;
    }
    let header = reader.array()?;
    let mut keys = Vec::new();
    for _ in 0..read_len(&mut reader)? {
        keys.push(reader.array()?);
    }
    let blockhash = reader.array()?;
    let mut instructions = Vec::new();
    for _ in 0..read_len(&mut reader)? {
        let program = reader.byte()?;
        let len = read_len(&mut reader)?;
        let accounts = reader.take(len)?.to_vec();
        let len = read_len(&mut reader)?;
        let data = reader.take(len)?.to_vec();
        instructions.push(Compiled {
            program,
            accounts,
            data,
        });
    }
    let mut lookups = Vec::new();
    if versioned {
        for _ in 0..read_len(&mut reader)? {
            reader.array::<32>()?;
            let writable = read_len(&mut reader)?;
            reader.take(writable)?;
            let readonly = read_len(&mut reader)?;
            reader.take(readonly)?;
            lookups.push(Lookup { writable, readonly });
        }
    }
    reader.0.is_empty().then_some(Wire {
        signatures,
        message,
        versioned,
        header,
        keys,
        blockhash,
        instructions,
        lookups,
    })
}

pub(crate) fn read_any(bytes: &[u8]) -> Option<Wire<'_>> {
    if bytes.first() == Some(&V1) {
        read_v1(bytes)
    } else {
        read(bytes)
    }
}

fn read_v1(bytes: &[u8]) -> Option<Wire<'_>> {
    if bytes.len() > MAX_V1 {
        return None;
    }
    let mut reader = Reader(bytes);
    if reader.byte()? != V1 {
        return None;
    }
    let header: [u8; 3] = reader.array()?;
    let mask = u32::from_le_bytes(reader.array()?);
    if header[0] == 0 || mask & !V1_CONFIG != 0 {
        return None;
    }
    let blockhash = reader.array()?;
    let (count, addresses) = (reader.byte()?, reader.byte()?);
    if count > MAX_V1_ITEMS || addresses > MAX_V1_ITEMS {
        return None;
    }
    let mut keys = Vec::new();
    for _ in 0..addresses {
        keys.push(reader.array()?);
    }
    reader.take(4 * usize::try_from(mask.count_ones()).ok()?)?;
    let mut sizes = Vec::new();
    for _ in 0..count {
        let program = reader.byte()?;
        let accounts = reader.byte()?;
        let data = u16::from_le_bytes(reader.array()?);
        sizes.push((program, usize::from(accounts), usize::from(data)));
    }
    let mut instructions = Vec::new();
    for (program, accounts, data) in sizes {
        instructions.push(Compiled {
            program,
            accounts: reader.take(accounts)?.to_vec(),
            data: reader.take(data)?.to_vec(),
        });
    }
    let message = &bytes[..bytes.len() - reader.0.len()];
    let mut signatures = Vec::new();
    for _ in 0..header[0] {
        signatures.push(reader.array()?);
    }
    reader.0.is_empty().then_some(Wire {
        signatures,
        message,
        versioned: true,
        header,
        keys,
        blockhash,
        instructions,
        lookups: Vec::new(),
    })
}

pub(crate) fn fetched<'a>(bytes: &'a [u8], id: &TxId) -> Result<Wire<'a>, ChainError> {
    let wire = read_any(bytes).ok_or_else(ChainError::malformed)?;
    if self::id(&wire.signatures[0])? != *id {
        return Err(ChainError::Network(
            "an endpoint answered for another transaction".to_owned(),
        ));
    }
    Ok(wire)
}

pub(crate) fn parse(bytes: &[u8]) -> Result<Parsed, ChainError> {
    let invalid = || ChainError::Invalid("not a transaction this wallet makes");
    let wire = read(bytes).ok_or_else(invalid)?;
    if wire.versioned {
        return Err(ChainError::Unsupported("versioned transactions"));
    }
    let ([signature], [1, 0, readonly]) = (wire.signatures.as_slice(), wire.header) else {
        return Err(invalid());
    };
    let keys = &wire.keys;
    let readonly = usize::from(readonly);
    if keys.is_empty() || readonly >= keys.len() {
        return Err(invalid());
    }
    let meta = |index: u8| -> Result<Meta, ChainError> {
        let at = usize::from(index);
        Ok(Meta {
            key: *keys.get(at).ok_or_else(invalid)?,
            signer: at == 0,
            writable: at < keys.len() - readonly,
        })
    };
    let instructions = wire
        .instructions
        .iter()
        .map(|compiled| {
            Ok(Instruction {
                program: meta(compiled.program)?.key,
                accounts: compiled
                    .accounts
                    .iter()
                    .map(|index| meta(*index))
                    .collect::<Result<Vec<Meta>, ChainError>>()?,
                data: compiled.data.clone(),
            })
        })
        .collect::<Result<Vec<Instruction>, ChainError>>()?;
    if compile(keys[0], wire.blockhash, &instructions)? != wire.message {
        return Err(invalid());
    }
    VerifyingKey::from_bytes(&keys[0])
        .and_then(|key| key.verify_strict(wire.message, &Signature::from_bytes(signature)))
        .map_err(|_| ChainError::Mismatch("the signature"))?;
    Ok(Parsed {
        signature: *signature,
        payer: keys[0],
        blockhash: wire.blockhash,
        message: wire.message.to_vec(),
        instructions,
    })
}

fn write_len(out: &mut Vec<u8>, len: usize) -> Result<(), ChainError> {
    if len > MAX_LEN {
        return Err(ChainError::Invalid("a transaction part is too long"));
    }
    let mut rest = len;
    loop {
        let low = (rest & 0x7f).to_le_bytes()[0];
        rest >>= 7;
        if rest == 0 {
            out.push(low);
            return Ok(());
        }
        out.push(low | 0x80);
    }
}

fn read_len(reader: &mut Reader<'_>) -> Option<usize> {
    let mut value = 0;
    for shift in [0, 7, 14] {
        let byte = reader.byte()?;
        value |= usize::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return (value <= MAX_LEN && (shift == 0 || byte != 0)).then_some(value);
        }
    }
    None
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let (head, rest) = self.0.split_at_checked(len)?;
        self.0 = rest;
        Some(head)
    }

    fn byte(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.take(N)?.try_into().ok()
    }
}

#[cfg(fuzzing)]
pub(crate) fn fuzz(data: &[u8]) {
    let _ = parse(data);
    let _ = read(data);
    let _ = read_any(data);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lengths(len: usize) -> Vec<u8> {
        let mut out = Vec::new();
        write_len(&mut out, len).unwrap();
        out
    }

    #[test]
    fn lengths_use_the_compact_form() {
        for (len, bytes) in [
            (0, vec![0]),
            (0x7f, vec![0x7f]),
            (0x80, vec![0x80, 0x01]),
            (0x3fff, vec![0xff, 0x7f]),
            (0x4000, vec![0x80, 0x80, 0x01]),
            (0xffff, vec![0xff, 0xff, 0x03]),
        ] {
            assert_eq!(lengths(len), bytes, "{len}");
            assert_eq!(read_len(&mut Reader(&bytes)), Some(len));
        }
        assert!(write_len(&mut Vec::new(), 0x1_0000).is_err());
        for bad in [
            vec![0x80, 0x00],
            vec![0x80, 0x80, 0x00],
            vec![0xff, 0xff, 0x04],
            vec![0x80, 0x80, 0x80, 0x01],
            vec![0x80],
        ] {
            assert_eq!(read_len(&mut Reader(&bad)), None, "{bad:?}");
        }
    }

    fn meta(key: Key, signer: bool, writable: bool) -> Meta {
        Meta {
            key,
            signer,
            writable,
        }
    }

    fn sample(payer: Key, other: Key, program: Key) -> [Instruction; 2] {
        [
            Instruction {
                program,
                accounts: vec![meta(other, false, false), meta(payer, true, false)],
                data: vec![7],
            },
            Instruction {
                program,
                accounts: vec![meta(other, false, true)],
                data: Vec::new(),
            },
        ]
    }

    #[test]
    fn accounts_are_ordered_by_role_and_shared() {
        let (payer, other, program) = ([1; 32], [2; 32], [9; 32]);
        let message = compile(payer, [5; 32], &sample(payer, other, program)).unwrap();
        assert_eq!(&message[..4], [1, 0, 1, 3]);
        assert_eq!(&message[4..36], &payer);
        assert_eq!(&message[36..68], &other);
        assert_eq!(&message[68..100], &program);
        assert_eq!(&message[100..132], &[5; 32]);
        assert_eq!(&message[132..], [2, 2, 2, 1, 0, 1, 7, 2, 1, 1, 0]);
        let key = SigningKey::from_bytes(&[3; 32]);
        let signer = key.verifying_key().to_bytes();
        let message = compile(signer, [5; 32], &sample(signer, other, program)).unwrap();
        let (bytes, signature) = sign(&message, &key);
        let parsed = parse(&bytes).unwrap();
        assert_eq!(parsed.signature, signature);
        assert_eq!(parsed.payer, signer);
        assert_eq!(parsed.instructions[1].accounts, [meta(other, false, true)]);
        assert_eq!(
            parsed.instructions[0].accounts,
            [meta(other, false, true), meta(signer, true, true)]
        );
    }

    #[test]
    fn a_transfer_has_exactly_one_signer() {
        let program = [9; 32];
        let two = [Instruction {
            program,
            accounts: vec![meta([2; 32], true, false)],
            data: Vec::new(),
        }];
        assert!(compile([1; 32], [0; 32], &two).is_err());
        let writable_program = [Instruction {
            program,
            accounts: vec![meta(program, false, true)],
            data: Vec::new(),
        }];
        assert!(compile([1; 32], [0; 32], &writable_program).is_err());
        let huge = [Instruction {
            program,
            accounts: Vec::new(),
            data: vec![0; MAX_TX],
        }];
        assert!(compile([1; 32], [0; 32], &huge).is_err());
    }

    #[test]
    fn only_untouched_transactions_from_their_signer_parse() {
        let key = SigningKey::from_bytes(&[3; 32]);
        let signer = key.verifying_key().to_bytes();
        let instructions = [Instruction {
            program: [9; 32],
            accounts: vec![meta([2; 32], false, true)],
            data: vec![1, 2, 3],
        }];
        let message = compile(signer, [5; 32], &instructions).unwrap();
        let (bytes, _) = sign(&message, &key);
        assert!(parse(&bytes).is_ok());
        let mut edited = bytes.clone();
        let last = edited.len() - 1;
        edited[last] ^= 1;
        assert!(matches!(parse(&edited), Err(ChainError::Mismatch(_))));
        let mut longer = bytes.clone();
        longer.push(0);
        assert!(parse(&longer).is_err());
        let mut versioned = bytes[..65].to_vec();
        versioned.push(VERSIONED);
        versioned.extend_from_slice(&bytes[65..]);
        versioned.push(0);
        assert!(read(&versioned).is_some_and(|wire| wire.versioned));
        assert!(matches!(parse(&versioned), Err(ChainError::Unsupported(_))));
        let mut unknown = bytes.clone();
        unknown[65] |= VERSIONED;
        assert!(read(&unknown).is_none());
        let mut two = bytes;
        two[0] = 2;
        assert!(parse(&two).is_err());
        assert!(parse(&[]).is_err());
    }

    fn v1(mask: u32, addresses: u8, signatures: usize) -> Vec<u8> {
        let mut bytes = vec![V1, 1, 0, 1];
        bytes.extend_from_slice(&mask.to_le_bytes());
        bytes.extend_from_slice(&[5; 32]);
        bytes.extend_from_slice(&[1, addresses]);
        for key in 0..addresses {
            bytes.extend_from_slice(&[key; 32]);
        }
        bytes.extend(std::iter::repeat_n(
            9,
            4 * usize::try_from(mask.count_ones()).unwrap(),
        ));
        bytes.extend_from_slice(&[2, 2, 4, 0, 0, 1, 7, 7, 7, 7]);
        bytes.extend(std::iter::repeat_n(3, 64 * signatures));
        bytes
    }

    #[test]
    fn version_1_transactions_carry_their_signatures_at_the_end() {
        let bytes = v1(0b1111, 3, 1);
        let wire = read_any(&bytes).unwrap();
        assert_eq!(wire.keys, [[0; 32], [1; 32], [2; 32]]);
        assert_eq!(wire.blockhash, [5; 32]);
        assert_eq!(wire.signatures, [[3; 64]]);
        assert_eq!(wire.message, &bytes[..bytes.len() - 64]);
        assert_eq!(wire.instructions[0].program, 2);
        assert_eq!(wire.instructions[0].accounts, [0, 1]);
        assert_eq!(wire.instructions[0].data, [7; 4]);
        assert!(read(&bytes).is_none());
        assert!(read_any(&v1(0b10_0000, 3, 1)).is_none());
        assert!(read_any(&v1(0, 65, 1)).is_none());
        assert!(read_any(&v1(0, 3, 2)).is_none());
        assert!(read_any(&v1(0, 3, 0)).is_none());
        assert!(read_any(&v1(0b1_1111, 3, 1)).is_some());
    }
}
