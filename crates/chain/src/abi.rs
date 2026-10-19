use ruint::aliases::U256;

const WORD: usize = 32;
const MAX_TEXT: usize = 256;
const TRANSFER: [u8; 4] = [0xa9, 0x05, 0x9c, 0xbb];
const TRANSFER_LEN: usize = 4 + WORD + WORD;

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

pub fn abi_text(data: &[u8]) -> Option<String> {
    if data.len() == WORD {
        let end = data.iter().position(|byte| *byte == 0).unwrap_or(WORD);
        if data[end..].iter().any(|byte| *byte != 0) {
            return None;
        }
        return String::from_utf8(data[..end].to_vec()).ok();
    }
    let offset = small(data.get(..WORD)?)?;
    let length = small(data.get(offset..offset.checked_add(WORD)?)?)?;
    if length > MAX_TEXT {
        return None;
    }
    let start = offset.checked_add(WORD)?;
    let text = data.get(start..start.checked_add(length)?)?;
    String::from_utf8(text.to_vec()).ok()
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
