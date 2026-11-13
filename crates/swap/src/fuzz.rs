use serde_json::Value;

use bahaa_chain::abi;

use crate::{across, chainflip, circle, jupiter, kyber, oneclick, relay, thorchain, velora};

pub fn raw(data: &[u8]) {
    for index in 0..4 {
        let _ = abi::uint(data, index);
        let _ = abi::address(data, index);
        let _ = abi::tail(data, index);
        let _ = abi::length(data, index);
        let _ = abi::bytes_at(data, index);
        let _ = abi::bytes_list(data, index);
    }
    chainflip::fuzz(data);
    jupiter::fuzz(data);
    kyber::fuzz(data);
    relay::fuzz(data);
    velora::fuzz(data);
}

pub fn json(data: &[u8]) {
    let Ok(value) = serde_json::from_slice::<Value>(data) else {
        return;
    };
    across::fuzz(&value);
    circle::fuzz(&value);
    oneclick::fuzz(&value);
    relay::fuzz_reply(&value);
    thorchain::fuzz(&value);
    velora::fuzz_reply(&value);
}
