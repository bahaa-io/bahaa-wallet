use bahaa_chain::{ChainError, MAX_MEMO};
use bahaa_types::is_plain_text;
use tonlib_core::cell::{Cell, CellBuilder};

pub(crate) const COMMENT: u32 = 0;
const FIRST_BYTES: usize = 123;
const MORE_BYTES: usize = 127;
const MAX_CELLS: usize = 16;

pub(crate) fn build(text: &str) -> Result<Cell, ChainError> {
    let broken = |_| ChainError::Invalid("cannot build the memo");
    let bytes = text.as_bytes();
    let (first, rest) = bytes.split_at(bytes.len().min(FIRST_BYTES));
    let mut tail: Option<Cell> = None;
    for chunk in rest.chunks(MORE_BYTES).rev() {
        let mut cell = CellBuilder::new();
        cell.store_slice(chunk).map_err(broken)?;
        if let Some(next) = tail.take() {
            cell.store_reference(&next.to_arc()).map_err(broken)?;
        }
        tail = Some(cell.build().map_err(broken)?);
    }
    let mut root = CellBuilder::new();
    root.store_u32(32, COMMENT).map_err(broken)?;
    root.store_slice(first).map_err(broken)?;
    if let Some(next) = tail {
        root.store_reference(&next.to_arc()).map_err(broken)?;
    }
    root.build().map_err(broken)
}

pub(crate) fn bytes(cell: &Cell) -> Option<Vec<u8>> {
    if cell.parser().load_u32(32).ok()? != COMMENT {
        return None;
    }
    let mut out = Vec::new();
    snake(cell, 32, &mut out, 0)?;
    Some(out)
}

pub(crate) fn text(cell: &Cell) -> Option<String> {
    let text = String::from_utf8(bytes(cell)?).ok()?;
    (!text.is_empty() && is_plain_text(&text, MAX_MEMO)).then_some(text)
}

fn snake(cell: &Cell, skip: usize, out: &mut Vec<u8>, depth: usize) -> Option<()> {
    if depth >= MAX_CELLS {
        return None;
    }
    let mut parser = cell.parser();
    parser.skip_bits(skip).ok()?;
    let bits = parser.remaining_bits();
    if !bits.is_multiple_of(8) {
        return None;
    }
    out.extend(parser.load_bytes(bits / 8).ok()?);
    match cell.references() {
        [] => Some(()),
        [next] => snake(next, 0, out, depth + 1),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_of_any_length_read_back_exactly() {
        for written in [
            "1",
            "deposit 4417",
            &format!("{}a", "é".repeat(61)),
            &"é".repeat(62),
            &"z".repeat(120),
            "سلام دنیا",
            &"é".repeat(120),
        ] {
            let cell = build(written).unwrap();
            assert_eq!(text(&cell).as_deref(), Some(written), "{written}");
        }
        let long = build(&"é".repeat(120)).unwrap();
        assert_eq!(long.references().len(), 1);
        assert_eq!(long.references()[0].references().len(), 0);
    }

    #[test]
    fn what_is_not_a_plain_comment_is_left_out() {
        let mut other = CellBuilder::new();
        other.store_u32(32, 7).unwrap().store_slice(b"hi").unwrap();
        assert_eq!(text(&other.build().unwrap()), None);
        let mut odd = CellBuilder::new();
        odd.store_u32(32, COMMENT).unwrap().store_bit(true).unwrap();
        assert_eq!(bytes(&odd.build().unwrap()), None);
        let mut hidden = CellBuilder::new();
        hidden.store_u32(32, COMMENT).unwrap();
        hidden.store_slice("pay\u{202e}lla".as_bytes()).unwrap();
        assert_eq!(text(&hidden.build().unwrap()), None);
        let mut binary = CellBuilder::new();
        binary.store_u32(32, COMMENT).unwrap();
        binary.store_slice(&[0xff, 0xfe]).unwrap();
        assert_eq!(text(&binary.build().unwrap()), None);
        let mut deep = CellBuilder::new().build().unwrap();
        for _ in 0..20 {
            let mut cell = CellBuilder::new();
            cell.store_slice(b"a").unwrap();
            cell.store_reference(&deep.to_arc()).unwrap();
            deep = cell.build().unwrap();
        }
        let mut root = CellBuilder::new();
        root.store_u32(32, COMMENT).unwrap();
        root.store_reference(&deep.to_arc()).unwrap();
        assert_eq!(bytes(&root.build().unwrap()), None);
        let empty = build("").unwrap();
        assert_eq!(text(&empty), None);
        assert_eq!(bytes(&empty), Some(Vec::new()));
    }
}
