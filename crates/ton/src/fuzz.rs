use bahaa_types::Address;

use crate::tl::{self, Answer};
use crate::{
    blocks, cells, comment, emulate, fees, lite, metadata, proof, transaction, vm, wallet,
};

const OWNER: &str = "UQBEgrmXc1WWtt4qIAR-hvQHBlsYAULFCqunQ3Ollcsrb-T_";
const PARAMS: [i32; 6] = [18, 20, 21, 24, 25, 43];

pub fn answer(data: &[u8]) {
    lite::fuzz_answer(data);
    let _ = tl::candidate_block(data);
    let _ = tl::masterchain_info(data);
    let _ = tl::block_header(data);
    let _ = tl::send_status(data);
    if let Some(Answer::Ok(proof)) = tl::block_proof(data) {
        let _ = blocks::walk(&proof.from, &proof.to, &proof);
        for link in &proof.steps {
            let _ = blocks::follow(&link.from, link);
        }
    }
    if let Some(Answer::Ok(libraries)) = tl::library_result(data) {
        for (_, boc) in &libraries {
            let _ = vm::library_hash(boc);
        }
    }
    if let Some(Answer::Ok(config)) = tl::config_info(data) {
        if let Some(found) = proof::config(
            &config.block,
            &config.state_proof,
            &config.config_proof,
            &PARAMS,
        ) && let Some(config) = fees::Config::new(found)
        {
            let _ = config.settings();
            let _ = config.storage();
            let _ = config.encoded();
        }
        boc(&config.state_proof);
        boc(&config.config_proof);
    }
    if let Some(Answer::Ok(list)) = tl::transaction_list(data) {
        transaction::fuzz(&list.transactions);
    }
    if let Some(Answer::Ok(state)) = tl::account_state(data) {
        let _ = proof::account(
            &state.block,
            &state.shard_block,
            &state.shard_proof,
            &state.proof,
            0,
            &[0; 32],
        );
        boc(&state.state);
        boc(&state.proof);
        boc(&state.shard_proof);
    }
}

pub fn boc(data: &[u8]) {
    blocks::fuzz(data);
    transaction::fuzz(data);
    let _ = metadata::content(data);
    let _ = wallet::parse(data);
    let _ = wallet::read_address(data);
    let _ = emulate::destination(data);
    let _ = vm::library_hash(data);
    if let Ok((owner, _)) = Address::parse_ton(OWNER) {
        let _ = cells::account(data, &owner);
    }
    if let Some(bag) = cells::bag(data) {
        for root in bag.roots.iter().take(4) {
            let _ = comment::text(root);
            let _ = proof::lookup(root, &[0; 32], 256);
            let _ = fees::param(root, 21);
            let _ = fees::leaves(root, 32);
            let _ = fees::size_without_root(root);
            let _ = wallet::normalized(root);
        }
    }
}

pub fn json(data: &[u8]) {
    let _ = metadata::file(data);
    if let Ok(list) = serde_json::from_slice(data) {
        lite::fuzz_list(&list);
    }
}
