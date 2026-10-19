use std::collections::BTreeSet;

use crate::protocol::{self, Frame, Item};

pub fn frames(data: &[u8]) {
    if let Ok(items) = protocol::read_envelope(data) {
        quote_all(items);
    }
    if let Ok(frame) = serde_json::from_slice::<Frame>(data) {
        let _ = frame.ours();
        quote_all(frame.result);
    }
}

fn quote_all(items: Vec<Item>) {
    let wanted: BTreeSet<_> = items.iter().filter_map(Item::id).collect();
    let _ = protocol::quotes(items, "USD", &wanted);
}
