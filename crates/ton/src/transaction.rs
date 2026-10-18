use ruint::aliases::U256;
use tonlib_core::cell::{ArcCell, BagOfCells, Cell, CellParser};

use crate::cells::var_uint;
use crate::wallet;

pub(crate) const MAX_LISTED: u32 = 16;
const TRANSACTION: u8 = 0b0111;
const ORDINARY: u8 = 0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Transaction {
    pub lt: u64,
    pub previous: ([u8; 32], u64),
    pub now: u32,
    pub external: Option<[u8; 32]>,
    pub fees: U256,
    description: ArcCell,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Sent { forward: U256 },
    Failed(&'static str),
}

pub(crate) fn linked(
    boc: &[u8],
    account: &[u8; 32],
    from: ([u8; 32], u64),
) -> Option<Vec<Transaction>> {
    let roots = BagOfCells::parse(boc).ok()?.roots;
    if roots.is_empty() || roots.len() > usize::try_from(MAX_LISTED).ok()? {
        return None;
    }
    let mut expected = from;
    let mut list = Vec::with_capacity(roots.len());
    for root in &roots {
        if Cell::cell_hash(root).as_slice() != expected.0 {
            return None;
        }
        let transaction = read(root, account)?;
        if transaction.lt != expected.1 {
            return None;
        }
        expected = transaction.previous;
        list.push(transaction);
    }
    Some(list)
}

fn read(root: &ArcCell, account: &[u8; 32]) -> Option<Transaction> {
    let mut parser = root.parser();
    if parser.load_u8(4).ok()? != TRANSACTION || parser.load_bits(256).ok()? != account {
        return None;
    }
    let lt = parser.load_u64(64).ok()?;
    let previous_hash = parser.load_bits(256).ok()?.try_into().ok()?;
    let previous_lt = parser.load_u64(64).ok()?;
    let now = parser.load_u32(32).ok()?;
    parser.skip_bits(15 + 2 + 2).ok()?;
    let messages = parser.next_reference().ok()?;
    let fees = var_uint(&mut parser, 4)?;
    if parser.load_bit().ok()? {
        parser.next_reference().ok()?;
    }
    parser.next_reference().ok()?;
    let description = parser.next_reference().ok()?;
    parser.ensure_empty().ok()?;
    let mut messages = messages.parser();
    let external = if messages.load_bit().ok()? {
        wallet::normalized(messages.next_reference().ok()?.as_ref())
    } else {
        None
    };
    Some(Transaction {
        lt,
        previous: (previous_hash, previous_lt),
        now,
        external,
        fees,
        description,
    })
}

impl Transaction {
    pub(crate) fn outcome(&self) -> Option<Outcome> {
        let mut parser = self.description.parser();
        if parser.load_u8(4).ok()? != ORDINARY {
            return None;
        }
        parser.load_bit().ok()?;
        if parser.load_bit().ok()? {
            var_uint(&mut parser, 4)?;
            maybe_grams(&mut parser)?;
            status_change(&mut parser)?;
        }
        if parser.load_bit().ok()? {
            maybe_grams(&mut parser)?;
            var_uint(&mut parser, 4)?;
            if parser.load_bit().ok()? {
                parser.next_reference().ok()?;
            }
        }
        if !parser.load_bit().ok()? {
            return Some(Outcome::Failed("the wallet could not run the transfer"));
        }
        let computed = parser.load_bit().ok()?;
        parser.skip_bits(2).ok()?;
        var_uint(&mut parser, 4)?;
        parser.next_reference().ok()?;
        let action = if parser.load_bit().ok()? {
            Some(parser.next_reference().ok()?)
        } else {
            None
        };
        let aborted = parser.load_bit().ok()?;
        if !computed {
            return Some(Outcome::Failed("the wallet contract stopped with an error"));
        }
        let Some(action) = action else {
            return Some(Outcome::Failed("the wallet did not carry out the transfer"));
        };
        let mut parser = action.parser();
        let success = parser.load_bit().ok()?;
        parser.skip_bits(2).ok()?;
        status_change(&mut parser)?;
        let forward = maybe_grams(&mut parser)?;
        let collected = maybe_grams(&mut parser)?;
        parser.skip_bits(32).ok()?;
        if parser.load_bit().ok()? {
            parser.skip_bits(32).ok()?;
        }
        parser.skip_bits(32).ok()?;
        let skipped = parser.load_u16(16).ok()?;
        let created = parser.load_u16(16).ok()?;
        if !success || aborted {
            return Some(Outcome::Failed(
                "the network could not carry out the transfer",
            ));
        }
        if skipped != 0 || created != 1 {
            return Some(Outcome::Failed(
                "the transfer was skipped, most likely for lack of funds",
            ));
        }
        Some(Outcome::Sent {
            forward: forward.checked_sub(collected)?,
        })
    }
}

fn maybe_grams(parser: &mut CellParser<'_>) -> Option<U256> {
    if parser.load_bit().ok()? {
        var_uint(parser, 4)
    } else {
        Some(U256::ZERO)
    }
}

fn status_change(parser: &mut CellParser<'_>) -> Option<()> {
    if parser.load_bit().ok()? {
        parser.load_bit().ok()?;
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use tonlib_core::cell::CellBuilder;

    use super::*;

    const REAL: &str = include_str!("../tests/fixtures/transaction.txt");

    fn unhex(text: &str) -> Vec<u8> {
        let text = text.trim();
        (0..text.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
            .collect()
    }

    fn bytes32(text: &str) -> [u8; 32] {
        unhex(text).try_into().unwrap()
    }

    const ACCOUNT: &str = "13fd9907ed89660d9b90f925bd54a6b029cafb368a172888f1ffc6fd690624e5";
    const HASH: &str = "5bf15add946afef16f4406a82138e7ab3d1f2f2a6a2c765595421669f0cbd7e9";
    const PREVIOUS: &str = "39591d328ba81fa3c7c398b11e58f95eef3e966580ec582dd2e8e950981d295c";
    const NORMALIZED: &str = "88d7201d27158e7cbaf4cceaef8ce46f460e9dc1e084490b2485eb6e6112bd54";

    #[test]
    fn reads_a_real_wallet_transfer_and_its_cost() {
        let boc = unhex(REAL);
        let list = linked(
            &boc,
            &bytes32(ACCOUNT),
            (bytes32(HASH), 109_578_663_000_001),
        )
        .unwrap();
        let [transaction] = list.as_slice() else {
            panic!("one transaction expected");
        };
        assert_eq!(transaction.lt, 109_578_663_000_001);
        assert_eq!(
            transaction.previous,
            (bytes32(PREVIOUS), 108_313_447_000_005)
        );
        assert_eq!(transaction.now, 1_791_848_664);
        assert_eq!(transaction.external, Some(bytes32(NORMALIZED)));
        assert_eq!(transaction.fees, U256::from(561_530u32));
        assert_eq!(
            transaction.outcome(),
            Some(Outcome::Sent {
                forward: U256::from(79_468u32)
            })
        );
    }

    #[test]
    fn a_list_must_continue_the_history_it_was_asked_for() {
        let boc = unhex(REAL);
        let (account, hash) = (bytes32(ACCOUNT), bytes32(HASH));
        assert!(linked(&boc, &account, (hash, 109_578_663_000_001)).is_some());
        assert_eq!(linked(&boc, &account, (hash, 109_578_663_000_002)), None);
        assert_eq!(linked(&boc, &account, ([0; 32], 109_578_663_000_001)), None);
        assert_eq!(linked(&boc, &[0; 32], (hash, 109_578_663_000_001)), None);
        let mut tampered = boc.clone();
        let middle = tampered.len() / 2;
        tampered[middle] ^= 1;
        assert_eq!(
            linked(&tampered, &account, (hash, 109_578_663_000_001)),
            None
        );
        let root = BagOfCells::parse(&boc).unwrap().single_root().unwrap();
        let twice = BagOfCells::new(&[root.clone(), root])
            .serialize(false)
            .unwrap();
        assert_eq!(linked(&twice, &account, (hash, 109_578_663_000_001)), None);
        let empty = BagOfCells::new(&[]).serialize(false).unwrap_or_default();
        assert_eq!(linked(&empty, &account, (hash, 109_578_663_000_001)), None);
    }

    fn compute(success: bool) -> CellBuilder {
        let mut details = CellBuilder::new();
        details.store_u8(8, 0).unwrap();
        let mut phase = CellBuilder::new();
        phase.store_bit(true).unwrap().store_bit(success).unwrap();
        phase.store_u8(2, 0).unwrap().store_u8(4, 0).unwrap();
        phase
            .store_reference(&details.build().unwrap().to_arc())
            .unwrap();
        phase
    }

    fn action(success: bool, skipped: u16, created: u16) -> ArcCell {
        let mut phase = CellBuilder::new();
        phase.store_bit(success).unwrap().store_u8(2, 0).unwrap();
        phase.store_bit(false).unwrap();
        phase.store_bit(true).unwrap().store_u8(4, 1).unwrap();
        phase.store_u8(8, 90).unwrap();
        phase.store_bit(true).unwrap().store_u8(4, 1).unwrap();
        phase.store_u8(8, 30).unwrap();
        phase.store_u32(32, 0).unwrap().store_bit(false).unwrap();
        phase.store_u16(16, 1).unwrap().store_u16(16, 0).unwrap();
        phase.store_u16(16, skipped).unwrap();
        phase.store_u16(16, created).unwrap();
        phase.build().unwrap().to_arc()
    }

    fn described(
        compute: Option<CellBuilder>,
        action: Option<ArcCell>,
        aborted: bool,
    ) -> Transaction {
        let mut description = CellBuilder::new();
        description
            .store_u8(4, ORDINARY)
            .unwrap()
            .store_bit(false)
            .unwrap();
        description.store_bit(true).unwrap().store_u8(4, 0).unwrap();
        description
            .store_bit(false)
            .unwrap()
            .store_bit(false)
            .unwrap();
        description.store_bit(false).unwrap();
        match compute {
            Some(mut compute) => {
                description.store_cell(&compute.build().unwrap()).unwrap();
            }
            None => {
                description
                    .store_bit(false)
                    .unwrap()
                    .store_u8(2, 0)
                    .unwrap();
            }
        }
        match action {
            Some(action) => {
                description
                    .store_bit(true)
                    .unwrap()
                    .store_reference(&action)
                    .unwrap();
            }
            None => {
                description.store_bit(false).unwrap();
            }
        }
        description.store_bit(aborted).unwrap();
        Transaction {
            lt: 1,
            previous: ([0; 32], 0),
            now: 0,
            external: None,
            fees: U256::ZERO,
            description: description.build().unwrap().to_arc(),
        }
    }

    #[test]
    fn a_transfer_counts_only_when_the_wallet_sent_it() {
        let sent = described(Some(compute(true)), Some(action(true, 0, 1)), false);
        assert_eq!(
            sent.outcome(),
            Some(Outcome::Sent {
                forward: U256::from(60u8)
            })
        );
        let failures = [
            described(None, None, true),
            described(Some(compute(false)), None, true),
            described(Some(compute(true)), None, false),
            described(Some(compute(true)), Some(action(false, 0, 0)), true),
            described(Some(compute(true)), Some(action(true, 0, 1)), true),
            described(Some(compute(true)), Some(action(true, 1, 0)), false),
            described(Some(compute(true)), Some(action(true, 0, 2)), false),
            described(Some(compute(true)), Some(action(false, 0, 1)), false),
            described(Some(compute(true)), Some(action(true, 1, 1)), false),
            described(Some(compute(false)), Some(action(true, 0, 1)), false),
        ];
        for failure in failures {
            assert!(
                matches!(failure.outcome(), Some(Outcome::Failed(_))),
                "{:?}",
                failure.outcome()
            );
        }
    }
}
