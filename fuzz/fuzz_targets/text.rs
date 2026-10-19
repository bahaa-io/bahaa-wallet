#![no_main]

use bahaa_crypto::{Mnemonic, TonMnemonic};
use bahaa_types::{Address, Amount, ChainFamily, Price, TonForm, is_plain_text, to_plain_text};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&decimals, rest)) = data.split_first() else {
        return;
    };
    let Ok(text) = std::str::from_utf8(rest) else {
        return;
    };
    for family in [ChainFamily::Evm, ChainFamily::Tron, ChainFamily::Ton] {
        if let Ok(address) = Address::parse(family, text) {
            assert_eq!(Address::parse(family, &address.to_string()), Ok(address));
            for (bounceable, testnet) in [(true, false), (false, false), (true, true), (false, true)] {
                let form = TonForm { bounceable, testnet };
                if let Some(friendly) = address.ton_friendly(form) {
                    assert_eq!(Address::parse_ton(&friendly), Ok((address, Some(form))));
                }
            }
        }
    }
    if let Ok(amount) = Amount::parse(text, decimals) {
        assert_eq!(Amount::parse(&amount.to_string(), decimals), Ok(amount));
    }
    if let Ok(price) = Price::parse(text) {
        assert_eq!(Price::parse(&price.to_string()), Ok(price));
    }
    let max = usize::from(decimals % 200) + 1;
    let plain = to_plain_text(text, max);
    assert!(plain.is_empty() || is_plain_text(&plain, max), "{plain:?}");
    let _ = is_plain_text(text, max);
    let _ = Mnemonic::parse(text);
    let _ = TonMnemonic::parse(text);
});
