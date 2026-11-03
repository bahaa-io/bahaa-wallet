use ruint::aliases::U256;

const WORD: usize = 32;

pub(crate) fn push_address(data: &mut Vec<u8>, address: &[u8; 20]) {
    data.extend_from_slice(&[0u8; 12]);
    data.extend_from_slice(address);
}

pub(crate) fn push_uint(data: &mut Vec<u8>, value: U256) {
    data.extend_from_slice(&value.to_be_bytes::<WORD>());
}

pub(crate) fn push_bytes_list(data: &mut Vec<u8>, items: &[Vec<u8>]) {
    push_uint(data, U256::from(items.len()));
    let mut offset = items.len() * WORD;
    for item in items {
        push_uint(data, U256::from(offset));
        offset += WORD + padded(item.len());
    }
    for item in items {
        push_uint(data, U256::from(item.len()));
        data.extend_from_slice(item);
        data.resize(data.len() + padded(item.len()) - item.len(), 0);
    }
}

pub(crate) fn uint(data: &[u8], index: usize) -> Option<U256> {
    let start = index.checked_mul(WORD)?;
    let word: [u8; WORD] = data.get(start..start.checked_add(WORD)?)?.try_into().ok()?;
    Some(U256::from_be_bytes(word))
}

pub(crate) fn address(data: &[u8], index: usize) -> Option<[u8; 20]> {
    let word = uint(data, index)?.to_be_bytes::<WORD>();
    word[..12]
        .iter()
        .all(|byte| *byte == 0)
        .then(|| word[12..].try_into().ok())
        .flatten()
}

pub(crate) fn tail(data: &[u8], index: usize) -> Option<&[u8]> {
    let offset = usize::try_from(uint(data, index)?).ok()?;
    if offset % WORD != 0 {
        return None;
    }
    data.get(offset..)
}

pub(crate) fn length(data: &[u8], index: usize) -> Option<usize> {
    usize::try_from(uint(tail(data, index)?, 0)?).ok()
}

fn padded(length: usize) -> usize {
    length.div_ceil(WORD) * WORD
}
