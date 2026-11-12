use crate::abi::{abi_text, parse_nft_transfer};
use crate::details::{Gateways, Located, locate, read_details};

pub fn details(data: &[u8]) {
    let _ = read_details(data);
    let Ok(link) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(gateways) = Gateways::builtin() else {
        return;
    };
    if let Some(Located::Inline(bytes)) = locate(link, &gateways) {
        let _ = read_details(&bytes);
    }
}

pub fn abi(data: &[u8]) {
    let _ = parse_nft_transfer(data);
    let _ = abi_text(data);
}
