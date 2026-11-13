use bahaa_types::NftKind;
use ruint::aliases::U256;

const WORD: usize = 32;
const MAX_TEXT: usize = 256;
const TRANSFER: [u8; 4] = [0xa9, 0x05, 0x9c, 0xbb];
const TRANSFER_LEN: usize = 4 + WORD + WORD;
const SAFE_TRANSFER: [u8; 4] = [0x42, 0x84, 0x2e, 0x0e];
const SAFE_TRANSFER_UNITS: [u8; 4] = [0xf2, 0x42, 0x43, 0x2a];
const NO_DATA: usize = 5 * WORD;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NftMove {
    pub kind: NftKind,
    pub from: [u8; 20],
    pub to: [u8; 20],
    pub id: U256,
    pub units: U256,
}

pub const TRANSFER_EVENT: &str = "ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";

pub fn transfer_data(to: &[u8; 20], units: U256) -> Vec<u8> {
    let mut data = Vec::with_capacity(TRANSFER_LEN);
    data.extend_from_slice(&TRANSFER);
    data.extend_from_slice(&[0u8; 12]);
    data.extend_from_slice(to);
    data.extend_from_slice(&units.to_be_bytes::<32>());
    data
}

pub fn parse_transfer(data: &[u8]) -> Option<([u8; 20], U256)> {
    if data.len() != TRANSFER_LEN || data[..4] != TRANSFER || data[4..16] != [0u8; 12] {
        return None;
    }
    let to: [u8; 20] = data[16..36].try_into().ok()?;
    Some((to, U256::from_be_slice(&data[36..])))
}

pub fn nft_transfer_data(moved: &NftMove) -> Option<Vec<u8>> {
    let mut data = Vec::with_capacity(4 + 6 * WORD);
    let address = |data: &mut Vec<u8>, address: &[u8; 20]| {
        data.extend_from_slice(&[0u8; 12]);
        data.extend_from_slice(address);
    };
    match moved.kind {
        NftKind::Erc721 if moved.units == U256::from(1u8) => data.extend_from_slice(&SAFE_TRANSFER),
        NftKind::Erc1155 => data.extend_from_slice(&SAFE_TRANSFER_UNITS),
        _ => return None,
    }
    address(&mut data, &moved.from);
    address(&mut data, &moved.to);
    data.extend_from_slice(&moved.id.to_be_bytes::<32>());
    if moved.kind == NftKind::Erc1155 {
        data.extend_from_slice(&moved.units.to_be_bytes::<32>());
        data.extend_from_slice(&U256::from(NO_DATA).to_be_bytes::<32>());
        data.extend_from_slice(&[0u8; WORD]);
    }
    Some(data)
}

pub fn parse_nft_transfer(data: &[u8]) -> Option<NftMove> {
    let selector = data.get(..4)?;
    let kind = if selector == SAFE_TRANSFER {
        NftKind::Erc721
    } else if selector == SAFE_TRANSFER_UNITS {
        NftKind::Erc1155
    } else {
        return None;
    };
    let words: Vec<&[u8]> = data[4..].chunks(WORD).collect();
    let address = |word: &[u8]| -> Option<[u8; 20]> {
        (word.len() == WORD && word[..12] == [0u8; 12]).then(|| word[12..].try_into().ok())?
    };
    let units = match (kind, words.as_slice()) {
        (NftKind::Erc721, [_, _, _]) => U256::from(1u8),
        (NftKind::Erc1155, [_, _, _, units, offset, length])
            if U256::from_be_slice(offset) == U256::from(NO_DATA)
                && length.iter().all(|byte| *byte == 0) =>
        {
            U256::from_be_slice(units)
        }
        _ => return None,
    };
    if data.len() % WORD != 4 {
        return None;
    }
    let moved = NftMove {
        kind,
        from: address(words[0])?,
        to: address(words[1])?,
        id: U256::from_be_slice(words[2]),
        units,
    };
    (nft_transfer_data(&moved)? == data).then_some(moved)
}

pub fn abi_string(data: &[u8], max: usize) -> Option<String> {
    let offset = small(data.get(..WORD)?)?;
    let length = small(data.get(offset..offset.checked_add(WORD)?)?)?;
    if length > max {
        return None;
    }
    let start = offset.checked_add(WORD)?;
    let text = data.get(start..start.checked_add(length)?)?;
    String::from_utf8(text.to_vec()).ok()
}

pub fn abi_text(data: &[u8]) -> Option<String> {
    if data.len() == WORD {
        let end = data.iter().position(|byte| *byte == 0).unwrap_or(WORD);
        if data[end..].iter().any(|byte| *byte != 0) {
            return None;
        }
        return String::from_utf8(data[..end].to_vec()).ok();
    }
    abi_string(data, MAX_TEXT)
}

pub fn push_address(data: &mut Vec<u8>, address: &[u8; 20]) {
    data.extend_from_slice(&[0u8; 12]);
    data.extend_from_slice(address);
}

pub fn push_uint(data: &mut Vec<u8>, value: U256) {
    data.extend_from_slice(&value.to_be_bytes::<WORD>());
}

pub fn encode_bytes(items: &[&[u8]]) -> Vec<u8> {
    let mut data = Vec::new();
    let mut offset = items.len() * WORD;
    for item in items {
        push_uint(&mut data, U256::from(offset));
        offset += WORD + padded(item.len());
    }
    for item in items {
        push_uint(&mut data, U256::from(item.len()));
        data.extend_from_slice(item);
        data.resize(data.len() + padded(item.len()) - item.len(), 0);
    }
    data
}

pub fn push_bytes_list(data: &mut Vec<u8>, items: &[Vec<u8>]) {
    push_uint(data, U256::from(items.len()));
    let items: Vec<&[u8]> = items.iter().map(Vec::as_slice).collect();
    data.extend(encode_bytes(&items));
}

pub fn uint(data: &[u8], index: usize) -> Option<U256> {
    let start = index.checked_mul(WORD)?;
    let word: [u8; WORD] = data.get(start..start.checked_add(WORD)?)?.try_into().ok()?;
    Some(U256::from_be_bytes(word))
}

pub fn address(data: &[u8], index: usize) -> Option<[u8; 20]> {
    let word = uint(data, index)?.to_be_bytes::<WORD>();
    word[..12]
        .iter()
        .all(|byte| *byte == 0)
        .then(|| word[12..].try_into().ok())
        .flatten()
}

pub fn tail(data: &[u8], index: usize) -> Option<&[u8]> {
    let offset = usize::try_from(uint(data, index)?).ok()?;
    if offset % WORD != 0 {
        return None;
    }
    data.get(offset..)
}

pub fn length(data: &[u8], index: usize) -> Option<usize> {
    usize::try_from(uint(tail(data, index)?, 0)?).ok()
}

pub fn bytes_at(data: &[u8], index: usize) -> Option<&[u8]> {
    let tail = tail(data, index)?;
    let length = usize::try_from(uint(tail, 0)?).ok()?;
    tail.get(WORD..WORD.checked_add(length)?)
}

pub fn bytes_list(data: &[u8], index: usize) -> Option<Vec<&[u8]>> {
    let list = tail(data, index)?;
    let count = usize::try_from(uint(list, 0)?).ok()?;
    let body = list.get(WORD..)?;
    if count > body.len() / WORD {
        return None;
    }
    (0..count).map(|item| bytes_at(body, item)).collect()
}

fn padded(length: usize) -> usize {
    length.div_ceil(WORD) * WORD
}

fn small(word: &[u8]) -> Option<usize> {
    let (high, low) = word.split_at(WORD - 8);
    if high.iter().any(|byte| *byte != 0) {
        return None;
    }
    usize::try_from(u64::from_be_bytes(low.try_into().ok()?)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(value: usize) -> Vec<u8> {
        let mut word = vec![0; WORD - 8];
        word.extend(u64::try_from(value).unwrap().to_be_bytes());
        word
    }

    fn encoded(text: &str) -> Vec<u8> {
        let mut data = word(WORD);
        data.extend(word(text.len()));
        let mut body = text.as_bytes().to_vec();
        body.resize(text.len().div_ceil(WORD) * WORD, 0);
        data.extend(body);
        data
    }

    fn moved(kind: NftKind, units: u64) -> NftMove {
        NftMove {
            kind,
            from: [0x11; 20],
            to: [0x22; 20],
            id: U256::from(4_901u32),
            units: U256::from(units),
        }
    }

    #[test]
    fn nft_transfers_are_the_standard_safe_calls_and_read_back_exactly() {
        let one = nft_transfer_data(&moved(NftKind::Erc721, 1)).unwrap();
        assert_eq!(
            (one[..4].to_vec(), one.len()),
            (SAFE_TRANSFER.to_vec(), 4 + 3 * WORD)
        );
        assert_eq!(parse_nft_transfer(&one), Some(moved(NftKind::Erc721, 1)));
        let many = nft_transfer_data(&moved(NftKind::Erc1155, 7)).unwrap();
        assert_eq!(
            (many[..4].to_vec(), many.len()),
            (SAFE_TRANSFER_UNITS.to_vec(), 4 + 6 * WORD)
        );
        assert_eq!(parse_nft_transfer(&many), Some(moved(NftKind::Erc1155, 7)));
        assert_eq!(nft_transfer_data(&moved(NftKind::Erc721, 2)), None);
        assert_eq!(nft_transfer_data(&moved(NftKind::TonItem, 1)), None);
        let mut longer = one.clone();
        longer.extend([0u8; WORD]);
        let mut dirty = one.clone();
        dirty[4] = 1;
        let mut extra = many.clone();
        extra[4 + 6 * WORD - 1] = 1;
        let mut offset = many.clone();
        offset[4 + 5 * WORD - 1] = 0xc0;
        let mut short = many;
        short.truncate(4 + 5 * WORD);
        for bad in [
            longer,
            dirty,
            extra,
            offset,
            short,
            one[..4 + 2 * WORD].to_vec(),
            Vec::new(),
        ] {
            assert_eq!(parse_nft_transfer(&bad), None, "{bad:?}");
        }
    }

    #[test]
    fn dynamic_strings_are_read() {
        assert_eq!(
            abi_text(&encoded("Tether USD")).as_deref(),
            Some("Tether USD")
        );
        assert_eq!(abi_text(&encoded("")).as_deref(), Some(""));
        let long = "x".repeat(40);
        assert_eq!(abi_text(&encoded(&long)), Some(long));
    }

    #[test]
    fn fixed_bytes32_symbols_are_read() {
        let mut maker = b"MKR".to_vec();
        maker.resize(WORD, 0);
        assert_eq!(abi_text(&maker).as_deref(), Some("MKR"));
        let mut gap = b"MK".to_vec();
        gap.resize(WORD, 0);
        gap[5] = b'R';
        assert_eq!(abi_text(&gap), None);
    }

    #[test]
    fn broken_or_huge_replies_are_refused() {
        let good = encoded("Tether USD");
        assert_eq!(abi_text(&good[..good.len() - WORD]), None);
        assert_eq!(abi_text(&[]), None);
        let mut far = good.clone();
        far[WORD - 1] = 0xff;
        assert_eq!(abi_text(&far), None);
        let mut high = good.clone();
        high[0] = 1;
        assert_eq!(abi_text(&high), None);
        let mut huge = word(WORD);
        huge.extend(word(MAX_TEXT + 1));
        huge.extend(vec![b'x'; MAX_TEXT + WORD]);
        assert_eq!(abi_text(&huge), None);
        let mut bad = encoded("ab");
        bad[2 * WORD] = 0xff;
        assert_eq!(abi_text(&bad), None);
    }
}
