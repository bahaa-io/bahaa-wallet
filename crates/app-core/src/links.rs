use std::fmt::Write as _;

use bahaa_engine::{Address, AssetId, ChainFamily, ChainKind, Network, Registry, TonForm};
use ruint::aliases::U256;

use crate::{format, text};

const EVM: &str = "ethereum:";
const TON: &str = "ton://transfer/";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Payment {
    pub network: Option<Network>,
    pub to: Address,
    pub token: Option<Address>,
    pub units: Option<U256>,
    pub memo: Option<String>,
}

pub(crate) fn read(text: &str, registry: &Registry) -> Option<Result<Payment, String>> {
    let text = text.trim();
    if let Some(rest) = text.strip_prefix(EVM) {
        return Some(evm(rest, registry));
    }
    text.strip_prefix(TON).map(|rest| ton(rest, registry))
}

pub(crate) fn request(
    network: &Network,
    to: &Address,
    asset: &AssetId,
    units: Option<U256>,
    memo: Option<&str>,
) -> String {
    let shown = format::address(to, network);
    if units.is_none() && memo.is_none() {
        return shown;
    }
    match network.kind {
        ChainKind::Evm { chain_id, .. } => {
            let units = units.unwrap_or_default();
            match asset.contract() {
                None => format!("{EVM}{to}@{chain_id}?value={units}"),
                Some(token) => {
                    format!("{EVM}{token}@{chain_id}/transfer?address={to}&uint256={units}")
                }
            }
        }
        ChainKind::Ton { .. } => {
            let mut link = format!("{TON}{shown}");
            let mut separator = '?';
            let mut add = |key: &str, value: &str| {
                let _ = write!(link, "{separator}{key}={value}");
                separator = '&';
            };
            if let Some(units) = units {
                add("amount", &units.to_string());
            }
            if let Some(memo) = memo {
                add("text", &encode(memo));
            }
            if let Some(token) = asset.contract() {
                let form = TonForm {
                    bounceable: true,
                    testnet: network.testnet,
                };
                add(
                    "jetton",
                    &token
                        .ton_friendly(form)
                        .unwrap_or_else(|| token.to_string()),
                );
            }
            link
        }
        ChainKind::Tron { .. } => shown,
    }
}

fn evm(rest: &str, registry: &Registry) -> Result<Payment, String> {
    let rest = rest.strip_prefix("pay-").unwrap_or(rest);
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (target, function) = path.split_once('/').unwrap_or((path, ""));
    let (target, chain) = match target.split_once('@') {
        Some((target, chain)) => (target, Some(chain)),
        None => (target, None),
    };
    let target = Address::parse(ChainFamily::Evm, target).map_err(|err| err.to_string())?;
    let network = chain
        .map(|chain| {
            let id: u64 = chain.parse().map_err(|_| text::LINK_INVALID.to_owned())?;
            registry
                .networks()
                .find(|network| matches!(network.kind, ChainKind::Evm { chain_id, .. } if chain_id == id))
                .cloned()
                .ok_or_else(|| text::LINK_UNKNOWN_NETWORK.to_owned())
        })
        .transpose()?;
    let params = pairs(query)?;
    let value = |key: &str| {
        params
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    };
    match function {
        "" => Ok(Payment {
            network,
            to: target,
            token: None,
            units: value("value").map(|value| integer(value)).transpose()?,
            memo: None,
        }),
        "transfer" => {
            let to = value("address").ok_or_else(|| text::LINK_INVALID.to_owned())?;
            Ok(Payment {
                network,
                to: Address::parse(ChainFamily::Evm, to).map_err(|err| err.to_string())?,
                token: Some(target),
                units: value("uint256").map(|value| integer(value)).transpose()?,
                memo: None,
            })
        }
        _ => Err(text::LINK_UNSUPPORTED.to_owned()),
    }
}

fn ton(rest: &str, registry: &Registry) -> Result<Payment, String> {
    let (target, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (to, form) = Address::parse_ton(target).map_err(|err| err.to_string())?;
    let network = form
        .map(|form| {
            registry
                .networks()
                .find(|network| {
                    network.family() == ChainFamily::Ton && network.testnet == form.testnet
                })
                .cloned()
                .ok_or_else(|| text::LINK_UNKNOWN_NETWORK.to_owned())
        })
        .transpose()?;
    let mut payment = Payment {
        network,
        to,
        token: None,
        units: None,
        memo: None,
    };
    for (key, value) in pairs(query)? {
        match key.as_str() {
            "amount" => payment.units = Some(integer(&value)?),
            "text" => payment.memo = Some(value),
            "jetton" => {
                let (token, _) = Address::parse_ton(&value).map_err(|err| err.to_string())?;
                payment.token = Some(token);
            }
            "bin" | "init" => return Err(text::LINK_UNSUPPORTED.to_owned()),
            _ => {}
        }
    }
    Ok(payment)
}

fn pairs(query: &str) -> Result<Vec<(String, String)>, String> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            Ok((decode(key)?, decode(value)?))
        })
        .collect()
}

fn integer(text: &str) -> Result<U256, String> {
    let invalid = || text::LINK_INVALID.to_owned();
    let (mantissa, exponent) = match text.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, exponent.parse::<usize>().map_err(|_| invalid())?),
        None => (text, 0),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let shift = exponent.checked_sub(fraction.len()).ok_or_else(invalid)?;
    let digits = format!("{whole}{fraction}");
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid());
    }
    let base = U256::from_str_radix(&digits, 10).map_err(|_| invalid())?;
    let scale = U256::from(10u8)
        .checked_pow(U256::from(shift))
        .ok_or_else(invalid)?;
    base.checked_mul(scale).ok_or_else(invalid)
}

fn encode(text: &str) -> String {
    text.bytes()
        .fold(String::with_capacity(text.len()), |mut out, byte| {
            if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                out.push(char::from(byte));
            } else {
                let _ = write!(out, "%{byte:02X}");
            }
            out
        })
}

fn decode(text: &str) -> Result<String, String> {
    let invalid = || text::LINK_INVALID.to_owned();
    let mut bytes = Vec::with_capacity(text.len());
    let mut rest = text.as_bytes();
    while let Some((&byte, tail)) = rest.split_first() {
        if byte == b'%' {
            let pair = tail.get(..2).ok_or_else(invalid)?;
            let pair = std::str::from_utf8(pair).map_err(|_| invalid())?;
            bytes.push(u8::from_str_radix(pair, 16).map_err(|_| invalid())?);
            rest = &tail[2..];
        } else {
            bytes.push(byte);
            rest = tail;
        }
    }
    String::from_utf8(bytes).map_err(|_| invalid())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BSC_USDT: &str = "0x55d398326f99059fF775485246999027B3197955";
    const OWNER: &str = "0xA749C5a714991dc2EF2cF36a238514adA4cbBf69";
    const TON_OWNER: &str = "UQCi1ivY6QriyKIQZrSTwhvf4qXI19qIPe4XAOYo-qMMUEiR";

    fn registry() -> Registry {
        Registry::builtin().unwrap()
    }

    fn evm_address(text: &str) -> Address {
        Address::parse(ChainFamily::Evm, text).unwrap()
    }

    #[test]
    fn eip681_links_name_the_network_the_recipient_and_the_amount() {
        let registry = registry();
        let native = read(&format!("ethereum:{OWNER}@56?value=2.014e18"), &registry)
            .unwrap()
            .unwrap();
        assert_eq!(native.network.unwrap().id.as_str(), "bsc");
        assert_eq!(native.to, evm_address(OWNER));
        assert_eq!(native.units, Some(U256::from(2_014_000_000_000_000_000u64)));
        let token = read(
            &format!("ethereum:pay-{BSC_USDT}@56/transfer?address={OWNER}&uint256=1000000"),
            &registry,
        )
        .unwrap()
        .unwrap();
        assert_eq!(token.token, Some(evm_address(BSC_USDT)));
        assert_eq!(token.to, evm_address(OWNER));
        assert_eq!(token.units, Some(U256::from(1_000_000u32)));
        let plain = read(&format!("ethereum:{OWNER}"), &registry)
            .unwrap()
            .unwrap();
        assert_eq!((plain.network, plain.units), (None, None));
    }

    #[test]
    fn broken_or_foreign_links_are_explained() {
        let registry = registry();
        let error = |link: &str| read(link, &registry).unwrap().unwrap_err();
        assert_eq!(
            error(&format!("ethereum:{OWNER}@999999")),
            text::LINK_UNKNOWN_NETWORK
        );
        assert_eq!(
            error(&format!("ethereum:{OWNER}/approve?x=1")),
            text::LINK_UNSUPPORTED
        );
        assert_eq!(
            error(&format!("ethereum:{OWNER}?value=1.5")),
            text::LINK_INVALID
        );
        assert_eq!(
            error(&format!("{TON}{TON_OWNER}?bin=te6c")),
            text::LINK_UNSUPPORTED
        );
        assert_eq!(
            error(&format!("{TON}{TON_OWNER}?text=%E2%28")),
            text::LINK_INVALID
        );
        assert!(read(OWNER, &registry).is_none());
    }

    #[test]
    fn ton_links_carry_amount_comment_and_token() {
        let registry = registry();
        let link = format!("{TON}{TON_OWNER}?amount=50000000&text=Bahaa%20memo%20%F0%9F%99%82");
        let payment = read(&link, &registry).unwrap().unwrap();
        assert_eq!(payment.network.unwrap().id.as_str(), "ton");
        assert_eq!(payment.units, Some(U256::from(50_000_000u32)));
        assert_eq!(payment.memo.as_deref(), Some("Bahaa memo 🙂"));
    }

    #[test]
    fn receive_requests_read_back_as_the_same_payment() {
        let registry = registry();
        let bsc = registry.get(&"bsc".parse().unwrap()).unwrap();
        let usdt = AssetId::Token(bsc.id.clone(), evm_address(BSC_USDT));
        let to = evm_address(OWNER);
        assert_eq!(request(bsc, &to, &usdt, None, None), OWNER);
        let link = request(bsc, &to, &usdt, Some(U256::from(5u8)), None);
        let back = read(&link, &registry).unwrap().unwrap();
        assert_eq!(
            (back.to, back.token, back.units),
            (to, Some(evm_address(BSC_USDT)), Some(U256::from(5u8)))
        );
        let ton = registry.get(&"ton".parse().unwrap()).unwrap();
        let (owner, _) = Address::parse_ton(TON_OWNER).unwrap();
        let native = AssetId::Native(ton.id.clone());
        let link = request(
            ton,
            &owner,
            &native,
            Some(U256::from(7u8)),
            Some("for coffee & cake"),
        );
        let back = read(&link, &registry).unwrap().unwrap();
        assert_eq!(back.memo.as_deref(), Some("for coffee & cake"));
        assert_eq!((back.to, back.units), (owner, Some(U256::from(7u8))));
    }
}
