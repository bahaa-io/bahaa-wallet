pub fn json(bytes: &[u8]) {
    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) {
        crate::chain::fuzz(&value);
    }
}

pub fn raw(bytes: &[u8]) {
    crate::tx::fuzz(bytes);
}
