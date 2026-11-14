use zeroize::Zeroizing;

use crate::request::ConnectRequest;
use crate::{SessionKey, bridge, read_link, read_manifest, read_request, read_wallets};

pub fn inputs(data: &[u8]) {
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = read_link(text);
        let _ = ConnectRequest::read(text);
    }
    let _ = read_request(data);
    let _ = read_manifest(data);
    let _ = read_wallets(data);
    let _ = SessionKey::from_secret(&Zeroizing::new([1; 32])).open(&[2; 32], data);
    bridge::fuzz(data);
}
