use bahaa_store::Received;
use bahaa_types::{Address, Amount};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Caution {
    LooksLike(Address),
    Dust,
    DustSender,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Arrival {
    pub received: Received,
    pub cautions: Vec<Caution>,
}

pub(crate) fn arrival(received: Received, known: &[Address], value: Option<Amount>) -> Arrival {
    let mut cautions = Vec::new();
    if let Some(resembled) = resembled(&received.from, known) {
        cautions.push(Caution::LooksLike(resembled));
    }
    if received.amount.is_zero() || value.is_some_and(|value| value.is_zero()) {
        cautions.push(Caution::Dust);
    }
    Arrival { received, cautions }
}

pub(crate) fn recipient(to: &Address, known: &[Address], arrivals: &[Arrival]) -> Vec<Caution> {
    let mut cautions = Vec::new();
    if let Some(resembled) = resembled(to, known) {
        cautions.push(Caution::LooksLike(resembled));
    }
    let flagged = arrivals
        .iter()
        .any(|arrival| arrival.received.from == *to && !arrival.cautions.is_empty());
    if flagged && !known.contains(to) {
        cautions.push(Caution::DustSender);
    }
    cautions
}

fn resembled(address: &Address, known: &[Address]) -> Option<Address> {
    if known.contains(address) {
        return None;
    }
    known
        .iter()
        .find(|candidate| address.looks_like(candidate))
        .copied()
}

#[cfg(test)]
mod tests {
    use bahaa_types::ChainFamily;

    use super::*;

    fn evm(text: &str) -> Address {
        Address::parse(ChainFamily::Evm, text).unwrap()
    }

    const MINE: &str = "0x55d398326f99059fF775485246999027B3197955";
    const LOOKALIKE: &str = "0x55d3000000000000000000000000000000007955";
    const STRANGER: &str = "0x1111111111111111111111111111111111111111";

    fn received(from: &str, amount: &str) -> Received {
        Received {
            id: "ab".repeat(32),
            index: 0,
            network: "bsc".parse().unwrap(),
            from: evm(from),
            to: evm(MINE),
            asset: "bsc".parse().unwrap(),
            amount: Amount::parse(amount, 18).unwrap(),
            block: 1,
            time: 1,
        }
    }

    fn cents(text: &str) -> Option<Amount> {
        Amount::parse(text, 2).ok()
    }

    #[test]
    fn transfers_from_look_alikes_or_worth_nothing_are_flagged() {
        let known = [evm(MINE)];
        let cautions = |from: &str, amount: &str, value: Option<Amount>| {
            arrival(received(from, amount), &known, value).cautions
        };
        assert_eq!(cautions(STRANGER, "1", cents("2.50")), []);
        assert_eq!(cautions(STRANGER, "1", None), []);
        assert_eq!(cautions(MINE, "0", cents("0")), [Caution::Dust]);
        assert_eq!(cautions(STRANGER, "0.000001", cents("0")), [Caution::Dust]);
        assert_eq!(cautions(STRANGER, "0.000001", cents("0.01")), []);
        assert_eq!(cautions(STRANGER, "0", None), [Caution::Dust]);
        assert_eq!(
            cautions(LOOKALIKE, "5", cents("12.00")),
            [Caution::LooksLike(evm(MINE))]
        );
        assert_eq!(
            cautions(LOOKALIKE, "0.000001", cents("0")),
            [Caution::LooksLike(evm(MINE)), Caution::Dust]
        );
    }

    #[test]
    fn sending_to_a_look_alike_or_a_dust_sender_is_flagged() {
        let known = [evm(MINE)];
        let arrivals = [
            arrival(received(LOOKALIKE, "0.000001"), &known, cents("0")),
            arrival(received(STRANGER, "1"), &known, cents("2.50")),
        ];
        assert_eq!(recipient(&evm(MINE), &known, &arrivals), []);
        assert_eq!(recipient(&evm(STRANGER), &known, &arrivals), []);
        assert_eq!(
            recipient(&evm(LOOKALIKE), &known, &arrivals),
            [Caution::LooksLike(evm(MINE)), Caution::DustSender]
        );
        let dusty = [arrival(received(STRANGER, "0"), &known, None)];
        assert_eq!(
            recipient(&evm(STRANGER), &known, &dusty),
            [Caution::DustSender]
        );
        assert_eq!(recipient(&evm(STRANGER), &[evm(STRANGER)], &dusty), []);
        let both = [evm(MINE), evm(LOOKALIKE)];
        assert_eq!(recipient(&evm(LOOKALIKE), &both, &[]), []);
        assert_eq!(
            arrival(received(LOOKALIKE, "1"), &both, cents("2.50")).cautions,
            []
        );
    }
}
