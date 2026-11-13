use bahaa_chain::ChainError;
use bahaa_config::Feature;
use bahaa_types::Address;
use ed25519_dalek::{Signature, VerifyingKey};
use serde_json::json;
use sha2::{Digest, Sha256};

use super::SolanaChain;
use crate::programs::{
    NAME_SERVICE, NAME_TOKENIZER, RECORD_V2_CLASS, SNS_ROOT, TOKEN, derived, token_account,
};
use crate::rpc::{self, Account};
use crate::token::{Mint, held};
use crate::tx::Key;

const PREFIX: &[u8] = b"SPL Name Service";
const HEADER: usize = 96;
const ACTIVE_NFT: u8 = 2;
const NFT_MINT: usize = 66;
const SOLANA_VALIDATION: u16 = 1;
const V1_RECORD: &[u8] = b"\x01SOL";
const V2_RECORD: &[u8] = b"\x02SOL";
const SUBDOMAIN: u8 = 0;
const SNS: &str = ".sns";
const MAX_NAME: usize = 64;

impl SolanaChain {
    pub async fn sns_address(&self, name: &str) -> Result<Option<Address>, ChainError> {
        let domain = domain_key(name)?;
        let keys = [
            derived(&[b"nft_record", &domain], &NAME_TOKENIZER)?,
            name_key(V1_RECORD, &domain, &[0; 32])?,
            name_key(V2_RECORD, &domain, &RECORD_V2_CLASS)?,
            domain,
        ];
        let found = self.accounts(&keys, None).await?;
        let Some(registry) = found[3]
            .as_ref()
            .filter(|account| account.owner == NAME_SERVICE && account.data.len() >= HEADER)
        else {
            return Ok(None);
        };
        let owner: Key = registry.data[32..64]
            .try_into()
            .map_err(|_| ChainError::malformed())?;
        let nft = found[0].as_ref().filter(|account| {
            account.owner == NAME_TOKENIZER && account.data.first() == Some(&ACTIVE_NFT)
        });
        let target = match nft {
            Some(nft) => self.holder(&nft.data).await?,
            None => match v2_target(found[2].as_ref(), &owner)? {
                Some(target) => target,
                None => v1_target(found[1].as_ref(), &keys[1], &owner).unwrap_or(owner),
            },
        };
        if VerifyingKey::from_bytes(&target).is_err() {
            return Err(ChainError::Invalid(
                "this name belongs to a program account, not a wallet",
            ));
        }
        Ok(Some(Address::Solana(target)))
    }

    async fn holder(&self, nft: &[u8]) -> Result<Key, ChainError> {
        let mint: Key = nft
            .get(NFT_MINT..NFT_MINT + 32)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(ChainError::malformed)?;
        let asset = self
            .rpc
            .call_with(
                Some(Feature::Nfts),
                "getAsset",
                json!({"id": rpc::text(&mint)}),
            )
            .await?;
        let owner = rpc::key(&asset["ownership"]["owner"])?;
        let account = token_account(&owner, &TOKEN, &mint)?;
        let found = self.accounts(&[mint, account], None).await?;
        let single = Mint::read(found[0].as_ref(), &mint, bahaa_chain::now())
            .is_ok_and(|read| read.program == TOKEN && read.decimals == 0)
            && supply(found[0].as_ref()) == Some(1);
        if !single || held(found[1].as_ref(), &TOKEN, &owner, &mint)? != 1 {
            return Err(ChainError::Invalid(
                "this name is held as an NFT and its holder can't be confirmed",
            ));
        }
        Ok(owner)
    }
}

pub fn normalize(name: &str) -> Result<String, ChainError> {
    let lower = name.trim().to_lowercase();
    let invalid = ChainError::Invalid("this is not a valid Solana name");
    let label = lower.strip_suffix(SNS).ok_or(invalid.clone())?;
    let parts: Vec<&str> = label.split('.').collect();
    let fine = matches!(parts.len(), 1 | 2)
        && lower.len() <= MAX_NAME
        && parts.iter().all(|part| {
            !part.is_empty() && part.chars().all(|c| !c.is_whitespace() && !c.is_control())
        });
    if fine { Ok(lower) } else { Err(invalid) }
}

fn domain_key(name: &str) -> Result<Key, ChainError> {
    let name = normalize(name)?;
    let label = name.strip_suffix(SNS).ok_or_else(ChainError::malformed)?;
    match label.split_once('.') {
        None => name_key(label.as_bytes(), &SNS_ROOT, &[0; 32]),
        Some((sub, parent)) => {
            let parent = name_key(parent.as_bytes(), &SNS_ROOT, &[0; 32])?;
            let mut child = vec![SUBDOMAIN];
            child.extend_from_slice(sub.as_bytes());
            name_key(&child, &parent, &[0; 32])
        }
    }
}

fn name_key(label: &[u8], parent: &Key, class: &Key) -> Result<Key, ChainError> {
    let mut hash = Sha256::new();
    hash.update(PREFIX);
    hash.update(label);
    let hashed: [u8; 32] = hash.finalize().into();
    derived(&[&hashed, class, parent], &NAME_SERVICE)
}

fn v2_target(record: Option<&Account>, owner: &Key) -> Result<Option<Key>, ChainError> {
    let Some(data) = record
        .filter(|account| account.owner == NAME_SERVICE)
        .and_then(|account| account.data.get(HEADER..))
    else {
        return Ok(None);
    };
    let broken = ChainError::Invalid("this name's Solana address record is broken");
    let word = |at: usize| {
        data.get(at..at + 2)
            .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
    };
    let length = data
        .get(4..8)
        .map(|bytes| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        .ok_or(broken.clone())?;
    if length != 32 {
        return Err(broken);
    }
    if word(0) != Some(SOLANA_VALIDATION) || word(2) != Some(SOLANA_VALIDATION) {
        return Err(broken);
    }
    let field = |at: usize| -> Result<Key, ChainError> {
        data.get(8 + at..8 + at + 32)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(broken.clone())
    };
    let (staleness, proof, content) = (field(0)?, field(32)?, field(64)?);
    if staleness != *owner {
        return Ok(None);
    }
    if proof != content {
        return Err(broken);
    }
    Ok(Some(content))
}

fn v1_target(record: Option<&Account>, key: &Key, owner: &Key) -> Option<Key> {
    let payload = record
        .filter(|account| account.owner == NAME_SERVICE)?
        .data
        .get(HEADER..HEADER + 96)?;
    let target: Key = payload[..32].try_into().ok()?;
    let signature = Signature::from_slice(&payload[32..]).ok()?;
    let mut signed = target.to_vec();
    signed.extend_from_slice(key);
    let message = bahaa_types::hex::encode(&signed);
    VerifyingKey::from_bytes(owner)
        .ok()?
        .verify_strict(message.as_bytes(), &signature)
        .ok()
        .map(|()| target)
}

fn supply(account: Option<&Account>) -> Option<u64> {
    account?
        .data
        .get(36..44)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u64::from_le_bytes)
}

#[cfg(fuzzing)]
pub(crate) fn fuzz(account: Option<&Account>) {
    let _ = v2_target(account, &[0; 32]);
    let _ = v1_target(account, &[0; 32], &[0; 32]);
    let _ = supply(account);
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signer, SigningKey};

    use super::*;

    fn key(text: &str) -> Key {
        bs58::decode(text).into_vec().unwrap().try_into().unwrap()
    }

    fn record(owner: Key, data: Vec<u8>) -> Account {
        let mut full = vec![0; HEADER];
        full.extend(data);
        Account {
            lamports: 1,
            owner,
            executable: false,
            space: u64::try_from(full.len()).unwrap(),
            data: full,
        }
    }

    fn v2(staleness: Key, proof: Key, content: Key, validation: u16) -> Account {
        let mut data = validation.to_le_bytes().to_vec();
        data.extend(validation.to_le_bytes());
        data.extend(32u32.to_le_bytes());
        data.extend(staleness);
        data.extend(proof);
        data.extend(content);
        record(NAME_SERVICE, data)
    }

    #[test]
    fn names_derive_the_accounts_the_sdk_derives() {
        assert_eq!(
            domain_key("bonfida.sns").unwrap(),
            key("Crf8hzfthWGbGbLTVCiqRqV5MVnbpHB1L9KQMd6gsinb")
        );
        assert_eq!(
            domain_key("dex.bonfida.sns").unwrap(),
            key("HoFfFXqFHAC8RP3duuQNzag1ieUwJRBv1HtRNiWFq4Qu")
        );
        assert_eq!(normalize(" Bonfida.SNS ").unwrap(), "bonfida.sns");
        for bad in [
            "bonfida.sol",
            "bonfida",
            "a.b.c.sns",
            ".sns",
            "bon fida.sns",
        ] {
            assert!(normalize(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_v2_record_counts_only_while_its_owner_signed_it() {
        let (owner, target) = ([1; 32], [2; 32]);
        assert_eq!(
            v2_target(Some(&v2(owner, target, target, 1)), &owner),
            Ok(Some(target))
        );
        assert_eq!(
            v2_target(Some(&v2([9; 32], target, target, 1)), &owner),
            Ok(None)
        );
        assert!(v2_target(Some(&v2(owner, [3; 32], target, 1)), &owner).is_err());
        assert!(v2_target(Some(&v2(owner, target, target, 2)), &owner).is_err());
        assert_eq!(v2_target(None, &owner), Ok(None));
        assert_eq!(
            v2_target(Some(&record([7; 32], vec![1, 0, 1, 0])), &owner),
            Ok(None)
        );
    }

    #[test]
    fn a_v1_record_needs_the_owners_signature_over_its_hex_text() {
        let signer = SigningKey::from_bytes(&[5; 32]);
        let owner = signer.verifying_key().to_bytes();
        let (target, at) = ([6; 32], [8; 32]);
        let mut joined = target.to_vec();
        joined.extend(at);
        let signature = signer.sign(bahaa_types::hex::encode(&joined).as_bytes());
        let mut payload = target.to_vec();
        payload.extend(signature.to_bytes());
        let good = record(NAME_SERVICE, payload.clone());
        assert_eq!(v1_target(Some(&good), &at, &owner), Some(target));
        assert_eq!(v1_target(Some(&good), &[9; 32], &owner), None);
        assert_eq!(v1_target(Some(&good), &at, &[7; 32]), None);
        let mut forged = payload;
        forged[40] ^= 1;
        assert_eq!(
            v1_target(Some(&record(NAME_SERVICE, forged)), &at, &owner),
            None
        );
    }
}
