use crate::abi::{abi_text, parse_nft_transfer};
use crate::details::{DETAILS_MEDIA, Gateways, Located, PICTURE_MEDIA, locate, read_details};
use crate::picture::read_picture;

pub fn details(data: &[u8]) {
    let _ = read_details(data);
    let Ok(link) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(gateways) = Gateways::builtin() else {
        return;
    };
    if let Some(Located::Inline(bytes)) = locate(link, &gateways, &DETAILS_MEDIA) {
        let _ = read_details(&bytes);
    }
    if let Some(Located::Inline(bytes)) = locate(link, &gateways, &PICTURE_MEDIA) {
        let _ = read_picture(&bytes);
    }
}

pub fn picture(data: &[u8]) {
    let _ = read_picture(data);
}

pub fn abi(data: &[u8]) {
    let _ = parse_nft_transfer(data);
    let _ = abi_text(data);
}
