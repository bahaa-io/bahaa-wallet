const DIGITS: &[u8; 16] = b"0123456789abcdef";

pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

pub fn decode(text: &str) -> Option<Vec<u8>> {
    let digits = text.as_bytes();
    if !digits.len().is_multiple_of(2) {
        return None;
    }
    let (pairs, _) = digits.as_chunks::<2>();
    pairs
        .iter()
        .map(|[high, low]| Some((nibble(*high)? << 4) | nibble(*low)?))
        .collect()
}

pub fn decode_array<const N: usize>(text: &str) -> Option<[u8; N]> {
    let mut out = [0u8; N];
    decode_into(text, &mut out)?;
    Some(out)
}

pub fn decode_into(text: &str, out: &mut [u8]) -> Option<()> {
    let (pairs, rest) = text.as_bytes().as_chunks::<2>();
    if !rest.is_empty() || pairs.len() != out.len() {
        return None;
    }
    for (byte, [high, low]) in out.iter_mut().zip(pairs) {
        *byte = (nibble(*high)? << 4) | nibble(*low)?;
    }
    Some(())
}

fn nibble(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        b'A'..=b'F' => Some(digit - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_and_decodes_exactly() {
        assert_eq!(encode(&[0x00, 0x0f, 0xa9, 0xff]), "000fa9ff");
        assert_eq!(decode("000fA9ff"), Some(vec![0x00, 0x0f, 0xa9, 0xff]));
        assert_eq!(decode(""), Some(Vec::new()));
        assert_eq!(decode_array::<2>("beef"), Some([0xbe, 0xef]));
    }

    #[test]
    fn refuses_anything_but_pairs_of_hex_digits() {
        for text in ["0", "0x00", "+f", "-1", " 0", "0g", "é0"] {
            assert_eq!(decode(text), None, "{text:?}");
        }
        assert_eq!(decode_array::<3>("beef"), None);
        assert_eq!(decode_into("bee", &mut [0; 2]), None);
        assert_eq!(decode_into("beef", &mut [0; 3]), None);
        assert_eq!(decode_array::<32>(&"+f".repeat(32)), None);
    }
}
