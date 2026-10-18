use std::collections::BTreeSet;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};
use tycho_types::boc::Boc;
use tycho_types::cell::{Cell, Load};
use tycho_types::merkle::MerkleProof;
use tycho_types::models::{
    BlockInfo, BlockchainConfig, ConfigParam28, ShardFees, ShardHashes, ValidatorDescription,
    ValidatorSet,
};

use crate::tl::{self, BlockId, BlockProof, Link, Signed};

const MASTERCHAIN: i32 = -1;
const BLOCK: u32 = 0x11ef_55aa;
const BLOCK_EXTRA: u32 = 0x4a33_f6fd;
const MC_BLOCK_EXTRA: u16 = 0xcca5;
const NODE_ID: u32 = 0x4813_b4c6;
const FRESH: Duration = Duration::from_secs(3);
const MAX_BEHIND: u32 = 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unproven {
    Chain,
    Source,
    Target,
    Validators,
    Signatures,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Reached {
    pub block: BlockId,
    pub key: bool,
    pub utime: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Walk {
    pub key: BlockId,
    pub reached: Option<Reached>,
}

#[derive(Debug)]
pub(crate) struct Trust(Mutex<Trusted>);

#[derive(Clone, Copy, Debug)]
struct Trusted {
    key: BlockId,
    latest: Option<(Instant, BlockId)>,
    newest: Option<u32>,
}

impl Trust {
    pub(crate) fn new(key: BlockId) -> Self {
        Self(Mutex::new(Trusted {
            key,
            latest: None,
            newest: None,
        }))
    }

    pub(crate) fn key(&self) -> BlockId {
        self.lock().key
    }

    pub(crate) fn recent(&self, last: Option<&BlockId>, now: Instant) -> Option<BlockId> {
        let (at, block) = self.lock().latest?;
        let fresh = now.saturating_duration_since(at) < FRESH;
        (fresh && last.is_none_or(|last| last.seqno >= block.seqno)).then_some(block)
    }

    pub(crate) fn reached(&self, key: BlockId) {
        let mut trusted = self.lock();
        if key.seqno > trusted.key.seqno {
            trusted.key = key;
        }
    }

    pub(crate) fn current(&self, utime: u32) -> bool {
        self.lock()
            .newest
            .is_none_or(|newest| newest <= utime.saturating_add(MAX_BEHIND))
    }

    pub(crate) fn authenticated(&self, block: BlockId, utime: u32, now: Instant) {
        let mut trusted = self.lock();
        trusted.newest = Some(trusted.newest.map_or(utime, |newest| newest.max(utime)));
        if trusted
            .latest
            .is_none_or(|(_, latest)| block.seqno >= latest.seqno)
        {
            trusted.latest = Some((now, block));
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Trusted> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

pub(crate) fn walk(
    known: &BlockId,
    target: &BlockId,
    proof: &BlockProof,
) -> Result<Walk, Unproven> {
    let mut key = *known;
    let mut at = *known;
    let mut last = None;
    for link in &proof.steps {
        let reached = follow(&at, link)?;
        if reached.key {
            key = reached.block;
        }
        at = reached.block;
        last = Some(reached);
    }
    let reached = last.filter(|reached| reached.block == *target);
    if proof.to != at || (reached.is_none() && (proof.complete || key == *known)) {
        return Err(Unproven::Chain);
    }
    Ok(Walk { key, reached })
}

pub(crate) fn follow(trusted: &BlockId, link: &Link) -> Result<Reached, Unproven> {
    if link.from != *trusted
        || !masterchain(&link.from)
        || !masterchain(&link.to)
        || link.to.seqno <= link.from.seqno
    {
        return Err(Unproven::Chain);
    }
    let source = opened(&link.config_proof, &link.from).ok_or(Unproven::Source)?;
    if header(&source, &link.from).is_none() {
        return Err(Unproven::Source);
    }
    let (validators, shuffle) = key_validators(&source).ok_or(Unproven::Source)?;
    let target = opened(&link.dest_proof, &link.to).ok_or(Unproven::Target)?;
    let info = header(&target, &link.to).ok_or(Unproven::Target)?;
    if info.key_block != link.to_key_block {
        return Err(Unproven::Target);
    }
    let signed = &link.signed;
    if signed.catchain_seqno != info.gen_catchain_seqno {
        return Err(Unproven::Validators);
    }
    let (subset, hash) = validators
        .compute_mc_subset(info.gen_catchain_seqno, shuffle)
        .ok_or(Unproven::Validators)?;
    if hash != info.gen_validator_list_hash_short || hash != signed.validator_set_hash {
        return Err(Unproven::Validators);
    }
    let message = message(&link.to, signed).ok_or(Unproven::Signatures)?;
    approved(&subset, &signed.signatures, &message)?;
    Ok(Reached {
        block: link.to,
        key: info.key_block,
        utime: info.gen_utime,
    })
}

fn masterchain(block: &BlockId) -> bool {
    block.workchain == MASTERCHAIN && block.shard == i64::MIN
}

fn opened(proof: &[u8], block: &BlockId) -> Option<Cell> {
    let proof = Boc::decode(proof)
        .ok()?
        .parse_exotic::<MerkleProof>()
        .ok()?;
    (proof.hash.0 == block.root_hash).then_some(proof.cell)
}

fn header(cell: &Cell, block: &BlockId) -> Option<BlockInfo> {
    if cell.as_slice().ok()?.load_u32().ok()? != BLOCK {
        return None;
    }
    let info = cell.reference_cloned(0)?.parse::<BlockInfo>().ok()?;
    let matches = info.version == 0
        && info.shard.is_masterchain()
        && info.master_ref.is_none()
        && i64::from(info.seqno) == i64::from(block.seqno);
    matches.then_some(info)
}

fn key_validators(cell: &Cell) -> Option<(ValidatorSet, bool)> {
    let extra = cell.reference_cloned(3)?;
    let mut slice = extra.as_slice().ok()?;
    if slice.load_u32().ok()? != BLOCK_EXTRA {
        return None;
    }
    slice.skip_first(256 + 256, 3).ok()?;
    if !slice.load_bit().ok()? {
        return None;
    }
    let custom = slice.load_reference_cloned().ok()?;
    let mut slice = custom.as_slice().ok()?;
    if slice.load_u16().ok()? != MC_BLOCK_EXTRA || !slice.load_bit().ok()? {
        return None;
    }
    ShardHashes::load_from(&mut slice).ok()?;
    ShardFees::load_from(&mut slice).ok()?;
    slice.skip_first(0, 1).ok()?;
    let config = BlockchainConfig::load_from(&mut slice).ok()?;
    if !slice.is_empty() {
        return None;
    }
    let validators = config.get_current_validator_set().ok()?;
    let catchain = config.get::<ConfigParam28>().ok()??;
    Some((validators, catchain.shuffle_mc_validators))
}

fn message(block: &BlockId, signed: &Signed) -> Option<Vec<u8>> {
    let Some(simplex) = &signed.simplex else {
        return Some(tl::block_signed(block));
    };
    if tl::candidate_block(&simplex.candidate)? != *block {
        return None;
    }
    let candidate: [u8; 32] = Sha256::digest(&simplex.candidate).into();
    Some(tl::finalized(&simplex.session_id, simplex.slot, &candidate))
}

fn approved(
    subset: &[ValidatorDescription],
    signatures: &[([u8; 32], Vec<u8>)],
    message: &[u8],
) -> Result<(), Unproven> {
    let known: Vec<([u8; 32], &ValidatorDescription)> = subset
        .iter()
        .map(|validator| (node_id(&validator.public_key.0), validator))
        .collect();
    let total: u128 = subset
        .iter()
        .map(|validator| u128::from(validator.weight))
        .sum();
    let mut seen = BTreeSet::new();
    let mut weight = 0u128;
    for (node, signature) in signatures {
        if !seen.insert(*node) {
            return Err(Unproven::Signatures);
        }
        let validator = known
            .iter()
            .find(|(id, _)| id == node)
            .map(|(_, validator)| *validator)
            .ok_or(Unproven::Signatures)?;
        let key =
            VerifyingKey::from_bytes(&validator.public_key.0).map_err(|_| Unproven::Signatures)?;
        let signature = Signature::from_slice(signature).map_err(|_| Unproven::Signatures)?;
        key.verify_strict(message, &signature)
            .map_err(|_| Unproven::Signatures)?;
        weight += u128::from(validator.weight);
    }
    if weight * 3 > total * 2 {
        Ok(())
    } else {
        Err(Unproven::Signatures)
    }
}

fn node_id(key: &[u8; 32]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(NODE_ID.to_le_bytes());
    hasher.update(key);
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tl::Answer;

    const FIXTURE: &str = include_str!("../tests/fixtures/blocks.json");

    fn recorded(name: &str) -> Link {
        let fixture: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        let text = fixture[name].as_str().unwrap();
        let bytes: Vec<u8> = (0..text.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
            .collect();
        let Some(Answer::Ok(proof)) = tl::block_proof(&bytes) else {
            panic!("{name} is not a block proof");
        };
        assert!(proof.complete);
        assert_eq!(proof.steps.len(), 1);
        proof.steps[0].clone()
    }

    #[test]
    fn validators_signed_each_recorded_link() {
        for (name, key, simplex) in [
            ("ordinary", true, false),
            ("simplex_key", true, true),
            ("simplex", false, true),
        ] {
            let link = recorded(name);
            assert_eq!(link.signed.simplex.is_some(), simplex, "{name}");
            let reached = follow(&link.from, &link).unwrap();
            assert_eq!((reached.block, reached.key), (link.to, key), "{name}");
        }
        let change = recorded("simplex_key");
        let reached = follow(&change.from, &change).unwrap();
        assert!(follow(&reached.block, &recorded("simplex")).is_ok());
    }

    #[test]
    fn a_link_must_start_at_the_trusted_block_and_move_forward() {
        let link = recorded("simplex");
        let other = recorded("ordinary");
        assert_eq!(follow(&other.from, &link), Err(Unproven::Chain));
        let mut backwards = link.clone();
        backwards.to.seqno = backwards.from.seqno;
        assert_eq!(follow(&backwards.from, &backwards), Err(Unproven::Chain));
        let mut basechain = link.clone();
        basechain.from.workchain = 0;
        assert_eq!(follow(&basechain.from, &basechain), Err(Unproven::Chain));
    }

    #[test]
    fn validators_come_only_from_the_trusted_key_block() {
        let link = recorded("simplex");
        let mut foreign = link.clone();
        foreign.config_proof = recorded("simplex_key").config_proof;
        assert_eq!(follow(&foreign.from, &foreign), Err(Unproven::Source));
        let mut not_key = link.clone();
        not_key.from = link.to;
        not_key.config_proof = link.dest_proof.clone();
        not_key.to.seqno = link.to.seqno + 1;
        assert_eq!(follow(&not_key.from, &not_key), Err(Unproven::Source));
        let mut torn = link.clone();
        let middle = torn.config_proof.len() / 2;
        torn.config_proof[middle] ^= 1;
        assert_eq!(follow(&torn.from, &torn), Err(Unproven::Source));
    }

    #[test]
    fn the_signed_block_header_must_match_the_link() {
        let link = recorded("simplex");
        let mut foreign = link.clone();
        foreign.dest_proof = recorded("simplex_key").dest_proof;
        assert_eq!(follow(&foreign.from, &foreign), Err(Unproven::Target));
        let mut flagged = link.clone();
        flagged.to_key_block = true;
        assert_eq!(follow(&flagged.from, &flagged), Err(Unproven::Target));
        let mut renumbered = link.clone();
        renumbered.to.seqno += 1;
        assert_eq!(follow(&renumbered.from, &renumbered), Err(Unproven::Target));
    }

    #[test]
    fn both_proofs_are_bound_to_their_block_hashes() {
        let link = recorded("simplex");
        let mut source = link.clone();
        source.from.root_hash[0] ^= 1;
        assert_eq!(follow(&source.from, &source), Err(Unproven::Source));
        let mut target = link.clone();
        target.to.root_hash[0] ^= 1;
        assert_eq!(follow(&target.from, &target), Err(Unproven::Target));
    }

    #[test]
    fn a_vote_for_a_neighbouring_block_does_not_prove_this_one() {
        let (link, neighbour) = (recorded("simplex"), recorded("neighbour"));
        assert_eq!(link.signed.catchain_seqno, neighbour.signed.catchain_seqno);
        assert!(follow(&neighbour.from, &neighbour).is_ok());
        let mut borrowed = link.clone();
        borrowed.signed = neighbour.signed;
        assert_eq!(follow(&borrowed.from, &borrowed), Err(Unproven::Signatures));
    }

    #[test]
    fn the_validator_set_must_be_the_one_the_block_names() {
        let link = recorded("simplex");
        let mut hashed = link.clone();
        hashed.signed.validator_set_hash ^= 1;
        assert_eq!(follow(&hashed.from, &hashed), Err(Unproven::Validators));
        let mut session = link.clone();
        session.signed.catchain_seqno += 1;
        assert_eq!(follow(&session.from, &session), Err(Unproven::Validators));
    }

    #[test]
    fn every_signature_must_be_valid_unique_and_known() {
        for name in ["ordinary", "simplex"] {
            let link = recorded(name);
            let mut forged = link.clone();
            forged.signed.signatures[0].1[10] ^= 1;
            assert_eq!(
                follow(&forged.from, &forged),
                Err(Unproven::Signatures),
                "{name}"
            );
            let mut repeated = link.clone();
            let first = repeated.signed.signatures[0].clone();
            repeated.signed.signatures.push(first);
            assert_eq!(
                follow(&repeated.from, &repeated),
                Err(Unproven::Signatures),
                "{name}"
            );
            let mut stranger = link.clone();
            stranger.signed.signatures[0].0[0] ^= 1;
            assert_eq!(
                follow(&stranger.from, &stranger),
                Err(Unproven::Signatures),
                "{name}"
            );
            let mut few = link.clone();
            let third = few.signed.signatures.len() / 3;
            few.signed.signatures.truncate(third);
            assert_eq!(follow(&few.from, &few), Err(Unproven::Signatures), "{name}");
        }
        let link = recorded("ordinary");
        let mut file = link.clone();
        file.to.file_hash[0] ^= 1;
        assert_eq!(follow(&file.from, &file), Err(Unproven::Signatures));
    }

    #[test]
    fn simplex_votes_must_finalize_this_very_block() {
        let link = recorded("simplex");
        let simplex = |link: &mut Link| link.signed.simplex.as_mut().unwrap().clone();
        let mut session = link.clone();
        let mut changed = simplex(&mut session);
        changed.session_id[0] ^= 1;
        session.signed.simplex = Some(changed);
        assert_eq!(follow(&session.from, &session), Err(Unproven::Signatures));
        let mut slot = link.clone();
        let mut changed = simplex(&mut slot);
        changed.slot += 1;
        slot.signed.simplex = Some(changed);
        assert_eq!(follow(&slot.from, &slot), Err(Unproven::Signatures));
        let mut other = link.clone();
        let mut changed = simplex(&mut other);
        changed.candidate[12] ^= 1;
        other.signed.simplex = Some(changed);
        assert_eq!(follow(&other.from, &other), Err(Unproven::Signatures));
        let mut longer = link.clone();
        let mut changed = simplex(&mut longer);
        changed.candidate.extend_from_slice(&[0; 4]);
        longer.signed.simplex = Some(changed);
        assert_eq!(follow(&longer.from, &longer), Err(Unproven::Signatures));
        let mut ordinary = link.clone();
        ordinary.signed.simplex = None;
        assert_eq!(follow(&ordinary.from, &ordinary), Err(Unproven::Signatures));
    }

    #[test]
    fn more_than_two_thirds_of_the_weight_must_sign() {
        use ed25519_dalek::{Signer, SigningKey};
        use tycho_types::cell::HashBytes;
        let keys: Vec<SigningKey> = (1..=4u8)
            .map(|seed| SigningKey::from_bytes(&[seed; 32]))
            .collect();
        let subset: Vec<ValidatorDescription> = keys
            .iter()
            .zip([2, 1, 1, 1])
            .map(|(key, weight)| ValidatorDescription {
                public_key: HashBytes(key.verifying_key().to_bytes()),
                weight,
                adnl_addr: None,
                mc_seqno_since: 0,
                prev_total_weight: 0,
            })
            .collect();
        let message = b"block";
        let signed = |chosen: &[usize]| -> Vec<([u8; 32], Vec<u8>)> {
            chosen
                .iter()
                .map(|at| {
                    let key = &keys[*at];
                    (
                        node_id(&key.verifying_key().to_bytes()),
                        key.sign(message).to_bytes().to_vec(),
                    )
                })
                .collect()
        };
        assert_eq!(
            approved(&subset, &signed(&[0, 1]), message),
            Err(Unproven::Signatures)
        );
        assert_eq!(
            approved(&subset, &signed(&[1, 2, 3]), message),
            Err(Unproven::Signatures)
        );
        assert_eq!(approved(&subset, &signed(&[0, 1, 2]), message), Ok(()));
        assert_eq!(approved(&subset, &signed(&[0, 1, 2, 3]), message), Ok(()));
        assert_eq!(
            approved(&subset[..3], &signed(&[1, 2]), message),
            Err(Unproven::Signatures)
        );
        assert_eq!(
            approved(&subset[1..], &signed(&[1, 2]), message),
            Err(Unproven::Signatures)
        );
        assert_eq!(approved(&subset[1..], &signed(&[1, 2, 3]), message), Ok(()));
        assert_eq!(
            approved(&subset, &signed(&[0, 1, 2]), b"other"),
            Err(Unproven::Signatures)
        );
    }

    fn answer(complete: bool, from: BlockId, to: BlockId, steps: Vec<Link>) -> BlockProof {
        BlockProof {
            complete,
            from,
            to,
            steps,
        }
    }

    #[test]
    fn a_walk_ends_at_the_target_or_at_a_newer_key_block() {
        let (change, latest) = (recorded("simplex_key"), recorded("simplex"));
        let whole = answer(
            true,
            change.from,
            latest.to,
            vec![change.clone(), latest.clone()],
        );
        let walked = walk(&change.from, &latest.to, &whole).unwrap();
        assert_eq!(walked.key, change.to);
        assert_eq!(walked.reached.map(|reached| reached.block), Some(latest.to));
        let partial = answer(false, change.from, change.to, vec![change.clone()]);
        assert_eq!(
            walk(&change.from, &latest.to, &partial),
            Ok(Walk {
                key: change.to,
                reached: None
            })
        );
        let claimed = answer(true, change.from, change.to, vec![change.clone()]);
        assert_eq!(
            walk(&change.from, &latest.to, &claimed),
            Err(Unproven::Chain)
        );
        let stalled = answer(false, latest.from, latest.from, Vec::new());
        assert_eq!(
            walk(&latest.from, &latest.to, &stalled),
            Err(Unproven::Chain)
        );
        let elsewhere = answer(true, change.to, latest.to, vec![latest.clone()]);
        assert_eq!(
            walk(&change.from, &latest.to, &elsewhere),
            Err(Unproven::Chain)
        );
        let misnamed = answer(true, latest.from, change.to, vec![latest.clone()]);
        assert_eq!(
            walk(&latest.from, &latest.to, &misnamed),
            Err(Unproven::Chain)
        );
        let skipped = answer(true, change.from, latest.to, vec![latest.clone()]);
        assert_eq!(
            walk(&change.from, &latest.to, &skipped),
            Err(Unproven::Chain)
        );
    }

    #[test]
    fn trust_only_moves_forward_and_recent_blocks_expire() {
        let (change, latest) = (recorded("simplex_key"), recorded("simplex"));
        let trust = Trust::new(change.from);
        trust.reached(change.to);
        trust.reached(change.from);
        assert_eq!(trust.key(), change.to);
        let start = Instant::now();
        assert_eq!(trust.recent(None, start), None);
        assert!(trust.current(0));
        trust.authenticated(latest.to, 1_792_330_000, start);
        trust.authenticated(change.to, 1_792_298_760, start);
        assert!(trust.current(1_792_330_000 - MAX_BEHIND));
        assert!(!trust.current(1_792_330_000 - MAX_BEHIND - 1));
        assert_eq!(trust.recent(None, start), Some(latest.to));
        assert_eq!(trust.recent(Some(&latest.to), start), Some(latest.to));
        assert_eq!(trust.recent(Some(&change.to), start), None);
        assert_eq!(trust.recent(None, start + FRESH), None);
    }
}
