use bahaa_types::Address;
use ruint::aliases::U256;
use tonlib_core::TonAddress;
use tonlib_core::cell::{ArcCell, BagOfCells, Cell, CellParser};

use crate::cells::{from_ton, var_uint};
use crate::wallet;

pub(crate) const MAX_LISTED: u32 = 16;
const TRANSACTION: u8 = 0b0111;
const ORDINARY: u8 = 0;
const COMMENT: u32 = 0;
const ENCRYPTED_COMMENT: u32 = 0x2167_da4b;
const INTERNAL_TRANSFER: u32 = 0x178d_4519;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Transaction {
    pub hash: [u8; 32],
    pub lt: u64,
    pub previous: ([u8; 32], u64),
    pub now: u32,
    pub external: Option<[u8; 32]>,
    pub credit: Option<Credit>,
    pub tokens: Option<Credit>,
    pub fees: U256,
    phases: Option<Phases>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Credit {
    pub from: Address,
    pub units: U256,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Sent { forward: U256 },
    Failed(&'static str),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Phases {
    compute: Option<bool>,
    action: Option<ArcCell>,
    aborted: bool,
    bounced: bool,
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
        let transaction = read(root, account)?;
        if transaction.hash != expected.0 || transaction.lt != expected.1 {
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
    let message = if messages.load_bit().ok()? {
        Some(messages.next_reference().ok()?)
    } else {
        None
    };
    let phases = phases(&description);
    let credit = match (&message, &phases) {
        (Some(message), Some(phases)) if !phases.bounced => credit(message, account),
        _ => None,
    };
    let tokens = match (&message, &phases) {
        (Some(message), Some(phases))
            if phases.compute == Some(true) && !phases.aborted && !phases.bounced =>
        {
            tokens(message, account)
        }
        _ => None,
    };
    Some(Transaction {
        hash: Cell::cell_hash(root).as_slice().try_into().ok()?,
        lt,
        previous: (previous_hash, previous_lt),
        now,
        external: message.as_deref().and_then(wallet::normalized),
        credit,
        tokens,
        fees,
        phases,
    })
}

fn phases(description: &Cell) -> Option<Phases> {
    let mut parser = description.parser();
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
    let compute = if parser.load_bit().ok()? {
        let success = parser.load_bit().ok()?;
        parser.skip_bits(2).ok()?;
        var_uint(&mut parser, 4)?;
        parser.next_reference().ok()?;
        Some(success)
    } else {
        if parser.load_u8(2).ok()? == 0b11 {
            parser.load_bit().ok()?;
        }
        None
    };
    let action = if parser.load_bit().ok()? {
        Some(parser.next_reference().ok()?)
    } else {
        None
    };
    Some(Phases {
        compute,
        action,
        aborted: parser.load_bit().ok()?,
        bounced: parser.load_bit().ok()?,
    })
}

fn credit(message: &Cell, account: &[u8; 32]) -> Option<Credit> {
    let mut parser = message.parser();
    let (from, nano) = internal(&mut parser, account)?;
    if parser.load_bit().ok()? {
        return None;
    }
    let body = body(&mut parser)?;
    (nano > U256::ZERO && plain(&body)).then_some(Credit { from, units: nano })
}

fn tokens(message: &Cell, account: &[u8; 32]) -> Option<Credit> {
    let mut parser = message.parser();
    let (source, _) = internal(&mut parser, account)?;
    if parser.load_bit().ok()? {
        if parser.load_bit().ok()? {
            parser.next_reference().ok()?;
        } else {
            skip_state_init(&mut parser)?;
        }
    }
    let body = body(&mut parser)?;
    let mut parser = body.parser();
    if parser.load_u32(32).ok()? != INTERNAL_TRANSFER {
        return None;
    }
    parser.skip_bits(64).ok()?;
    let units = var_uint(&mut parser, 4)?;
    let from = match parser.load_address() {
        Ok(address) if address != TonAddress::NULL => from_ton(&address)?,
        _ => source,
    };
    (units > U256::ZERO).then_some(Credit { from, units })
}

fn internal(parser: &mut CellParser<'_>, account: &[u8; 32]) -> Option<(Address, U256)> {
    if parser.load_bit().ok()? {
        return None;
    }
    parser.skip_bits(2).ok()?;
    if parser.load_bit().ok()? {
        return None;
    }
    let from = from_ton(&parser.load_address().ok()?)?;
    if parser.load_address().ok()?.hash_part.as_slice() != account {
        return None;
    }
    let nano = var_uint(parser, 4)?;
    if parser.load_bit().ok()? {
        parser.next_reference().ok()?;
    }
    var_uint(parser, 4)?;
    var_uint(parser, 4)?;
    parser.skip_bits(64 + 32).ok()?;
    Some((from, nano))
}

fn skip_state_init(parser: &mut CellParser<'_>) -> Option<()> {
    if parser.load_bit().ok()? {
        parser.skip_bits(5).ok()?;
    }
    if parser.load_bit().ok()? {
        parser.skip_bits(2).ok()?;
    }
    for _ in 0..3 {
        if parser.load_bit().ok()? {
            parser.next_reference().ok()?;
        }
    }
    Some(())
}

fn body(parser: &mut CellParser<'_>) -> Option<ArcCell> {
    if parser.load_bit().ok()? {
        parser.next_reference().ok()
    } else {
        parser.load_remaining().ok().map(Cell::to_arc)
    }
}

fn plain(body: &Cell) -> bool {
    (body.bit_len() == 0 && body.references().is_empty())
        || body
            .parser()
            .load_u32(32)
            .is_ok_and(|op| op == COMMENT || op == ENCRYPTED_COMMENT)
}

impl Transaction {
    pub(crate) fn outcome(&self) -> Option<Outcome> {
        let phases = self.phases.as_ref()?;
        let Some(computed) = phases.compute else {
            return Some(Outcome::Failed("the wallet could not run the transfer"));
        };
        if !computed {
            return Some(Outcome::Failed("the wallet contract stopped with an error"));
        }
        let Some(action) = &phases.action else {
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
        if !success || phases.aborted {
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
    use tonlib_core::{TonAddress, TonHash};

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
        description.store_bit(false).unwrap();
        Transaction {
            hash: [0; 32],
            lt: 1,
            previous: ([0; 32], 0),
            now: 0,
            external: None,
            credit: None,
            tokens: None,
            fees: U256::ZERO,
            phases: phases(&description.build().unwrap()),
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

    const OURS: [u8; 32] = [0x3c; 32];

    #[derive(Clone, Copy)]
    enum Body {
        Empty,
        Inline(u32),
        Referenced(Option<u32>),
        Nested,
        Transfer { units: u8, from: Option<[u8; 32]> },
    }

    #[derive(Clone, Copy)]
    enum Init {
        Absent,
        Referenced,
        Inline,
    }

    #[derive(Clone, Copy)]
    struct Sent {
        bounced: bool,
        nano: u8,
        to: [u8; 32],
        extra: bool,
        init: Init,
        body: Body,
    }

    const PLAIN: Sent = Sent {
        bounced: false,
        nano: 7,
        to: OURS,
        extra: false,
        init: Init::Absent,
        body: Body::Empty,
    };

    fn message(sent: Sent) -> ArcCell {
        let empty = CellBuilder::new().build().unwrap().to_arc();
        let mut cell = CellBuilder::new();
        cell.store_bit(false).unwrap().store_bit(true).unwrap();
        cell.store_bit(false)
            .unwrap()
            .store_bit(sent.bounced)
            .unwrap();
        cell.store_address(&TonAddress::new(0, TonHash::from([0x5a; 32])))
            .unwrap();
        cell.store_address(&TonAddress::new(0, TonHash::from(sent.to)))
            .unwrap();
        cell.store_u8(4, 1).unwrap().store_u8(8, sent.nano).unwrap();
        cell.store_bit(sent.extra).unwrap();
        if sent.extra {
            cell.store_reference(&empty).unwrap();
        }
        cell.store_u8(4, 0).unwrap().store_u8(4, 0).unwrap();
        cell.store_u64(64, 0).unwrap().store_u32(32, 0).unwrap();
        match sent.init {
            Init::Absent => {
                cell.store_bit(false).unwrap();
            }
            Init::Referenced => {
                cell.store_bit(true).unwrap().store_bit(true).unwrap();
                cell.store_reference(&empty).unwrap();
            }
            Init::Inline => {
                cell.store_bit(true).unwrap().store_bit(false).unwrap();
                cell.store_u8(2, 0).unwrap().store_bit(true).unwrap();
                cell.store_reference(&empty).unwrap();
                cell.store_bit(true)
                    .unwrap()
                    .store_reference(&empty)
                    .unwrap();
                cell.store_bit(false).unwrap();
            }
        }
        match sent.body {
            Body::Empty => {
                cell.store_bit(false).unwrap();
            }
            Body::Inline(op) => {
                cell.store_bit(false).unwrap().store_u32(32, op).unwrap();
            }
            Body::Referenced(op) => {
                let mut body = CellBuilder::new();
                if let Some(op) = op {
                    body.store_u32(32, op).unwrap();
                }
                cell.store_bit(true).unwrap();
                cell.store_reference(&body.build().unwrap().to_arc())
                    .unwrap();
            }
            Body::Transfer { units, from } => {
                let mut body = CellBuilder::new();
                body.store_u32(32, INTERNAL_TRANSFER).unwrap();
                body.store_u64(64, 0).unwrap();
                body.store_u8(4, 1).unwrap().store_u8(8, units).unwrap();
                match from {
                    Some(owner) => {
                        body.store_address(&TonAddress::new(0, TonHash::from(owner)))
                            .unwrap();
                    }
                    None => {
                        body.store_u8(2, 0).unwrap();
                    }
                }
                body.store_u8(2, 0).unwrap().store_u8(4, 0).unwrap();
                body.store_bit(false).unwrap();
                cell.store_bit(true).unwrap();
                cell.store_reference(&body.build().unwrap().to_arc())
                    .unwrap();
            }
            Body::Nested => {
                let mut body = CellBuilder::new();
                body.store_reference(&empty).unwrap();
                cell.store_bit(true).unwrap();
                cell.store_reference(&body.build().unwrap().to_arc())
                    .unwrap();
            }
        }
        cell.build().unwrap().to_arc()
    }

    fn received(sent: Sent, bounced_back: bool) -> Option<Credit> {
        transaction(sent, false, bounced_back).credit
    }

    fn transaction(sent: Sent, computed: bool, bounced_back: bool) -> Transaction {
        processed(sent, computed, !computed, bounced_back)
    }

    fn processed(sent: Sent, computed: bool, aborted: bool, bounced_back: bool) -> Transaction {
        let mut messages = CellBuilder::new();
        messages.store_bit(true).unwrap();
        messages.store_reference(&message(sent)).unwrap();
        messages.store_bit(false).unwrap();
        let mut description = CellBuilder::new();
        description
            .store_u8(4, ORDINARY)
            .unwrap()
            .store_bit(false)
            .unwrap();
        description
            .store_bit(false)
            .unwrap()
            .store_bit(false)
            .unwrap();
        if computed {
            description
                .store_bit(true)
                .unwrap()
                .store_bit(true)
                .unwrap();
            description.store_u8(2, 0).unwrap().store_u8(4, 0).unwrap();
            description
                .store_reference(&CellBuilder::new().build().unwrap().to_arc())
                .unwrap();
            description
                .store_bit(false)
                .unwrap()
                .store_bit(aborted)
                .unwrap();
        } else {
            description
                .store_bit(false)
                .unwrap()
                .store_u8(2, 0)
                .unwrap();
            description
                .store_bit(false)
                .unwrap()
                .store_bit(aborted)
                .unwrap();
        }
        description.store_bit(bounced_back).unwrap();
        description.store_u8(8, 0).unwrap();
        let mut root = CellBuilder::new();
        root.store_u8(4, TRANSACTION)
            .unwrap()
            .store_slice(&OURS)
            .unwrap();
        root.store_u64(64, 9)
            .unwrap()
            .store_slice(&[0; 32])
            .unwrap();
        root.store_u64(64, 0)
            .unwrap()
            .store_u32(32, 1_700_000_000)
            .unwrap();
        root.store_u16(15, 0).unwrap().store_u8(4, 0).unwrap();
        root.store_reference(&messages.build().unwrap().to_arc())
            .unwrap();
        root.store_u8(4, 0).unwrap().store_bit(false).unwrap();
        root.store_reference(&CellBuilder::new().build().unwrap().to_arc())
            .unwrap();
        root.store_reference(&description.build().unwrap().to_arc())
            .unwrap();
        read(&root.build().unwrap().to_arc(), &OURS).unwrap()
    }

    #[test]
    fn only_plain_transfers_the_wallet_keeps_are_received() {
        let credit = Some(Credit {
            from: Address::Ton {
                workchain: 0,
                hash: [0x5a; 32],
            },
            units: U256::from(7u8),
        });
        let plain = [
            PLAIN,
            Sent {
                body: Body::Inline(COMMENT),
                ..PLAIN
            },
            Sent {
                body: Body::Referenced(Some(COMMENT)),
                ..PLAIN
            },
            Sent {
                body: Body::Referenced(Some(ENCRYPTED_COMMENT)),
                ..PLAIN
            },
            Sent {
                body: Body::Referenced(None),
                ..PLAIN
            },
            Sent {
                extra: true,
                ..PLAIN
            },
        ];
        for (at, sent) in plain.into_iter().enumerate() {
            assert_eq!(received(sent, false), credit, "{at}");
        }
        let others = [
            Sent {
                body: Body::Inline(0x7362_d09c),
                ..PLAIN
            },
            Sent {
                body: Body::Referenced(Some(0xd532_76db)),
                ..PLAIN
            },
            Sent {
                bounced: true,
                ..PLAIN
            },
            Sent { nano: 0, ..PLAIN },
            Sent {
                to: [1; 32],
                ..PLAIN
            },
            Sent {
                init: Init::Referenced,
                ..PLAIN
            },
            Sent {
                body: Body::Nested,
                ..PLAIN
            },
        ];
        for (at, sent) in others.into_iter().enumerate() {
            assert_eq!(received(sent, false), None, "{at}");
        }
        assert_eq!(received(PLAIN, true), None);
    }

    #[test]
    fn every_skipped_compute_reason_is_read_to_its_end() {
        for reason in [(2, 0b00), (2, 0b01), (2, 0b10), (3, 0b110)] {
            let mut description = CellBuilder::new();
            description
                .store_u8(4, ORDINARY)
                .unwrap()
                .store_bit(false)
                .unwrap();
            description
                .store_bit(false)
                .unwrap()
                .store_bit(false)
                .unwrap();
            description.store_bit(false).unwrap();
            description.store_u8(reason.0, reason.1).unwrap();
            description
                .store_bit(false)
                .unwrap()
                .store_bit(true)
                .unwrap();
            description
                .store_bit(false)
                .unwrap()
                .store_bit(false)
                .unwrap();
            assert_eq!(
                phases(&description.build().unwrap()),
                Some(Phases {
                    compute: None,
                    action: None,
                    aborted: true,
                    bounced: false
                }),
                "{reason:?}"
            );
        }
    }

    #[test]
    fn token_receipts_are_internal_transfers_the_wallet_accepted() {
        let transfer = |units: u8, from: Option<[u8; 32]>| Sent {
            body: Body::Transfer { units, from },
            ..PLAIN
        };
        let tokens = |sent: Sent, computed: bool, bounced_back: bool| {
            transaction(sent, computed, bounced_back).tokens
        };
        let credit = |hash: [u8; 32]| {
            Some(Credit {
                from: Address::Ton { workchain: 0, hash },
                units: U256::from(9u8),
            })
        };
        let sent = transfer(9, Some([0x6a; 32]));
        assert_eq!(tokens(sent, true, false), credit([0x6a; 32]));
        assert_eq!(tokens(transfer(9, None), true, false), credit([0x5a; 32]));
        for init in [Init::Inline, Init::Referenced] {
            assert_eq!(
                tokens(Sent { init, ..sent }, true, false),
                credit([0x6a; 32])
            );
        }
        assert_eq!(transaction(sent, true, false).credit, None);
        assert_eq!(tokens(PLAIN, true, false), None);
        for refused in [
            tokens(sent, false, false),
            tokens(sent, true, true),
            processed(sent, true, true, false).tokens,
            processed(sent, false, false, false).tokens,
            tokens(transfer(0, Some([0x6a; 32])), true, false),
            tokens(
                Sent {
                    bounced: true,
                    ..sent
                },
                true,
                false,
            ),
            tokens(
                Sent {
                    to: [1; 32],
                    ..sent
                },
                true,
                false,
            ),
        ] {
            assert_eq!(refused, None);
        }
    }
}
