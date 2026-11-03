use std::sync::Mutex;
use std::time::{Duration, Instant};

use bahaa_chain::{
    Balance, Chain, ChainError, Decimals, Decoded, FeeQuote, Replace, Signed, Transfer, TxId,
    TxStatus, amount, on_network,
};
use bahaa_config::{ChainKind, Coin, Network};
use bahaa_crypto::{DerivationPath, Seed};
use bahaa_types::{Address, AssetId, MAX_DECIMALS, SolanaPath, to_plain_text};
use ed25519_dalek::SigningKey;
use futures_util::future::try_join_all;
use ruint::aliases::U256;

mod calls;
mod incoming;
mod send;

pub use calls::{SolanaCall, swap_instruction};
pub use send::SolanaPlan;

use crate::programs::{METADATA, Shape, TOKEN, TOKEN_2022, metadata_account, token_account};
use crate::rpc::{self, Account, Rpc, Solana};
use crate::tx::{self, Key};

pub const QUOTE_LIFE: Duration = Duration::from_secs(45);

const TOKEN_READ: usize = 109;
const MINT_READ: usize = 46;
const MAX_NAME: usize = 40;
const MAX_SYMBOL: usize = 11;
const MAX_TEXT: usize = 200;
const METADATA_V1: u8 = 4;

#[derive(Debug)]
pub struct SolanaChain {
    network: Network,
    rpc: Rpc,
    decimals: Decimals,
    rent: Mutex<Option<(Instant, send::Rent)>>,
}

impl SolanaChain {
    pub fn new(network: Network) -> Result<Self, ChainError> {
        let ChainKind::Solana { genesis } = network.kind else {
            return Err(ChainError::Invalid("not a Solana network"));
        };
        let rpc = Rpc::new(
            network.endpoints.clone(),
            Solana {
                genesis: rpc::text(&genesis),
            },
        )?;
        Ok(Self {
            network,
            rpc,
            decimals: Decimals::default(),
            rent: Mutex::new(None),
        })
    }

    async fn accounts(
        &self,
        keys: &[Key],
        length: Option<usize>,
    ) -> Result<Vec<Option<Account>>, ChainError> {
        let reply = self
            .rpc
            .call("getMultipleAccounts", rpc::accounts_params(keys, length))
            .await?;
        rpc::accounts(&reply, keys.len())
    }

    fn known_decimals(&self, mint: &Key) -> Option<u8> {
        self.decimals.get(&self.network, &Address::Solana(*mint))
    }

    fn symbol(&self, mint: &Key) -> String {
        bahaa_chain::symbol(&self.network, &Address::Solana(*mint))
    }

    async fn token_decimals(&self, mint: &Key) -> Result<u8, ChainError> {
        if let Some(decimals) = self.known_decimals(mint) {
            return Ok(decimals);
        }
        let found = self.accounts(&[*mint], Some(MINT_READ)).await?;
        let decimals = mint_decimals(found[0].as_ref())?;
        self.decimals.learn(Address::Solana(*mint), decimals);
        Ok(decimals)
    }
}

impl Chain for SolanaChain {
    type Plan = SolanaPlan;

    fn network(&self) -> &Network {
        &self.network
    }

    fn derive(&self, seed: &Seed, account: u32) -> Result<Address, ChainError> {
        derive(seed, account, SolanaPath::default())
    }

    async fn balances(
        &self,
        address: &Address,
        assets: &[AssetId],
    ) -> Result<Vec<Balance>, ChainError> {
        let owner = bytes(address)?;
        on_network(&self.network.id, assets)?;
        let mints = assets
            .iter()
            .filter_map(AssetId::contract)
            .map(bytes)
            .collect::<Result<Vec<Key>, _>>()?;
        let mut keys = vec![owner];
        for mint in &mints {
            keys.push(token_account(&owner, mint)?);
        }
        let (found, decimals) = futures_util::future::try_join(
            self.accounts(&keys, Some(TOKEN_READ)),
            try_join_all(mints.iter().map(|mint| self.token_decimals(mint))),
        )
        .await?;
        let mut tokens = mints.iter().zip(&found[1..]).zip(decimals);
        assets
            .iter()
            .map(|asset| {
                let amount = if asset.contract().is_none() {
                    amount(
                        U256::from(found[0].as_ref().map_or(0, |account| account.lamports)),
                        self.network.native.decimals,
                    )?
                } else {
                    let ((mint, account), decimals) =
                        tokens.next().ok_or_else(ChainError::malformed)?;
                    amount(U256::from(held(account.as_ref(), &owner, mint)?), decimals)?
                };
                Ok(Balance {
                    asset: asset.clone(),
                    amount,
                    staked: None,
                })
            })
            .collect()
    }

    async fn prepare(&self, transfer: &Transfer) -> Result<(SolanaPlan, FeeQuote), ChainError> {
        Box::pin(self.plan(transfer)).await
    }

    fn planned<'a>(&self, plan: &'a SolanaPlan) -> &'a Transfer {
        &plan.transfer
    }

    fn replace(
        &self,
        _original: &Signed,
        _account: u32,
        _how: Replace,
    ) -> impl Future<Output = Result<(SolanaPlan, FeeQuote), ChainError>> + Send {
        std::future::ready(Err(ChainError::Unsupported(
            "replacing a transfer, it expires on its own within a minute",
        )))
    }

    fn sign(&self, plan: &SolanaPlan, seed: &Seed) -> Result<Signed, ChainError> {
        let key = signer(seed, plan.transfer.account, &plan.transfer.from)?;
        let (bytes, signature) = tx::sign(&plan.message, &key);
        Ok(Signed {
            id: tx::id(&signature)?,
            bytes,
        })
    }

    fn decode(&self, signed: &Signed) -> Result<Decoded, ChainError> {
        let parsed = tx::parse(&signed.bytes)?;
        if tx::id(&parsed.signature)? != signed.id {
            return Err(ChainError::Mismatch("the transaction id"));
        }
        let shape = Shape::read(&parsed)?;
        let (asset, decimals) = match shape.token {
            None => (
                AssetId::Native(self.network.id.clone()),
                self.network.native.decimals,
            ),
            Some((mint, decimals)) => {
                let known = self.known_decimals(&mint).ok_or(ChainError::Invalid(
                    "unknown token, prepare the transfer first",
                ))?;
                if known != decimals {
                    return Err(ChainError::Mismatch("the token decimals"));
                }
                (
                    AssetId::Token(self.network.id.clone(), Address::Solana(mint)),
                    decimals,
                )
            }
        };
        Ok(Decoded {
            from: Address::Solana(shape.from),
            to: Address::Solana(shape.to),
            asset,
            amount: amount(U256::from(shape.units), decimals)?,
            everything: false,
            memo: shape.memo,
            sequence: None,
            expires_at: None,
        })
    }

    async fn broadcast(&self, signed: &Signed) -> Result<TxId, ChainError> {
        self.push(signed).await
    }

    async fn status(
        &self,
        id: &TxId,
        _from: &Address,
        _expires_at: Option<u64>,
    ) -> Result<TxStatus, ChainError> {
        self.lookup(id).await
    }

    async fn token(&self, contract: &Address) -> Result<Coin, ChainError> {
        let mint = bytes(contract)?;
        let found = self
            .accounts(&[mint, metadata_account(&mint)?], None)
            .await?;
        let decimals = mint_decimals(found[0].as_ref())?;
        let (name, symbol) = metadata(found[1].as_ref(), &mint)
            .ok_or(ChainError::Invalid("this token has no name on chain"))?;
        Ok(Coin {
            symbol,
            name,
            decimals,
            price: None,
            color: None,
        })
    }

    fn know(&self, contract: &Address, decimals: u8) {
        self.decimals.learn(*contract, decimals);
    }
}

pub fn derive(seed: &Seed, account: u32, path: SolanaPath) -> Result<Address, ChainError> {
    Ok(Address::Solana(
        key(seed, account, path)?.verifying_key().to_bytes(),
    ))
}

fn signer(seed: &Seed, account: u32, from: &Address) -> Result<SigningKey, ChainError> {
    SolanaPath::ALL
        .into_iter()
        .filter_map(|path| key(seed, account, path).ok())
        .find(|key| Address::Solana(key.verifying_key().to_bytes()) == *from)
        .ok_or(ChainError::Invalid(
            "this seed does not own the sending account",
        ))
}

fn key(seed: &Seed, account: u32, path: SolanaPath) -> Result<SigningKey, ChainError> {
    let path: DerivationPath = match path {
        SolanaPath::Bip44Change => format!("m/44'/501'/{account}'/0'"),
        SolanaPath::Bip44 => format!("m/44'/501'/{account}'"),
    }
    .parse()
    .map_err(|_| ChainError::Invalid("account index out of range"))?;
    Ok(bahaa_crypto::ed25519(seed, &path)?)
}

fn bytes(address: &Address) -> Result<Key, ChainError> {
    match address {
        Address::Solana(key) => Ok(*key),
        _ => Err(ChainError::Invalid("not a Solana address")),
    }
}

fn held(account: Option<&Account>, owner: &Key, mint: &Key) -> Result<u64, ChainError> {
    let Some(account) = account.filter(|account| account.owner == TOKEN) else {
        return Ok(0);
    };
    let data = &account.data;
    if data.len() < TOKEN_READ || data[..32] != mint[..] {
        return Err(ChainError::Network(
            "an endpoint sent a token account for another token".to_owned(),
        ));
    }
    if data[32..64] != owner[..] || !matches!(data[108], 1 | 2) {
        return Ok(0);
    }
    data[64..72]
        .try_into()
        .map(u64::from_le_bytes)
        .map_err(|_| ChainError::malformed())
}

fn mint_decimals(account: Option<&Account>) -> Result<u8, ChainError> {
    let not_token = ChainError::Invalid("this address is not a token");
    let account = account.ok_or_else(|| not_token.clone())?;
    if account.owner == TOKEN_2022 {
        return Err(ChainError::Unsupported("Token-2022 tokens yet"));
    }
    match account.data.get(44..MINT_READ) {
        Some([decimals, 1]) if account.owner == TOKEN && *decimals <= MAX_DECIMALS => Ok(*decimals),
        _ => Err(not_token),
    }
}

fn metadata(account: Option<&Account>, mint: &Key) -> Option<(String, String)> {
    let account = account.filter(|account| account.owner == METADATA)?;
    let (kind, rest) = account.data.split_first()?;
    let rest = rest.get(32..)?;
    if *kind != METADATA_V1 || rest.get(..32)? != mint {
        return None;
    }
    let mut rest = rest.get(32..)?;
    let mut text = |max: usize| -> Option<String> {
        let len = usize::try_from(u32::from_le_bytes(rest.get(..4)?.try_into().ok()?))
            .ok()
            .filter(|len| *len <= MAX_TEXT)?;
        let raw = rest.get(4..4 + len)?;
        rest = rest.get(4 + len..)?;
        let clean = to_plain_text(String::from_utf8_lossy(raw).trim_end_matches('\0'), max);
        (!clean.is_empty()).then_some(clean)
    };
    let name = text(MAX_NAME)?;
    let symbol = text(MAX_SYMBOL)?;
    Some((name, symbol))
}

#[cfg(fuzzing)]
pub(crate) fn fuzz(value: &serde_json::Value) {
    if let Ok(found) = rpc::accounts(value, value["value"].as_array().map_or(0, Vec::len)) {
        for account in &found {
            let _ = held(account.as_ref(), &[0; 32], &[0; 32]);
            let _ = mint_decimals(account.as_ref());
            let _ = metadata(account.as_ref(), &[0; 32]);
        }
    }
    incoming::fuzz(value);
    send::fuzz(value);
}

#[cfg(fuzzing)]
pub(crate) fn fuzz_raw(data: &[u8]) {
    calls::fuzz(data);
}

#[cfg(test)]
mod tests {
    use bahaa_crypto::{Mnemonic, ed25519_seed};

    use super::*;

    #[test]
    fn derives_the_accounts_phantom_derives() {
        let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let seed = Mnemonic::parse(phrase).unwrap().to_seed("");
        let phantom = SolanaPath::Bip44Change;
        assert_eq!(
            derive(&seed, 0, phantom).unwrap().to_string(),
            "HAgk14JpMQLgt6rVgv7cBQFJWFto5Dqxi472uT3DKpqk"
        );
        assert_eq!(
            derive(&seed, 0, SolanaPath::Bip44).unwrap().to_string(),
            "GjJyeC1r2RgkuoCWMyPYkCWSGSGLcz266EaAkLA27AhL"
        );
        let other = Mnemonic::parse(
            "shoot island position soft burden budget tooth cruel issue economy destroy above",
        )
        .unwrap()
        .to_seed("");
        assert_eq!(
            derive(&other, 1, phantom).unwrap().to_string(),
            "4ejVGsCvYtbePCNKns8rRpYHboAJ1rsJBxgnMGo4EVba"
        );
        assert_eq!(
            derive(&other, 0, SolanaPath::Bip44).unwrap().to_string(),
            "2bUBiBNZyD29gP1oV6de7nxowMLoDBtopMMTGgMvjG5m"
        );
        assert_eq!(
            derive(&other, 1, SolanaPath::Bip44).unwrap().to_string(),
            "Hj1BkTvb59CqjHjKK6xuToQAVR7Lm4HuXBxfYReypRVG"
        );
        let single = ed25519_seed(&[7; 32]);
        assert_eq!(
            derive(&single, 0, phantom).unwrap(),
            derive(&single, 3, SolanaPath::Bip44).unwrap()
        );
    }

    fn token(owner: &Key, mint: &Key, units: u64, state: u8) -> Account {
        let mut data = mint.to_vec();
        data.extend_from_slice(owner);
        data.extend_from_slice(&units.to_le_bytes());
        data.resize(TOKEN_READ, 0);
        data[108] = state;
        Account {
            lamports: 2_039_280,
            owner: TOKEN,
            executable: false,
            data,
        }
    }

    #[test]
    fn token_balances_come_only_from_this_wallets_account_and_gifts_there_are_ignored() {
        let (owner, mint) = ([1; 32], [2; 32]);
        assert_eq!(held(None, &owner, &mint), Ok(0));
        assert_eq!(
            held(Some(&token(&owner, &mint, 8_509_335, 1)), &owner, &mint),
            Ok(8_509_335)
        );
        assert_eq!(
            held(Some(&token(&owner, &mint, 5, 2)), &owner, &mint),
            Ok(5)
        );
        for bad in [
            token(&owner, &[3; 32], 5, 1),
            Account {
                data: vec![0; 64],
                ..token(&owner, &mint, 5, 1)
            },
        ] {
            assert!(held(Some(&bad), &owner, &mint).is_err());
        }
        assert_eq!(
            held(Some(&token(&[3; 32], &mint, 5, 1)), &owner, &mint),
            Ok(0)
        );
        assert_eq!(
            held(Some(&token(&owner, &mint, 5, 0)), &owner, &mint),
            Ok(0)
        );
        let gifted = Account {
            lamports: 1,
            owner: [0; 32],
            executable: false,
            data: Vec::new(),
        };
        assert_eq!(held(Some(&gifted), &owner, &mint), Ok(0));
        let elsewhere = Account {
            owner: TOKEN_2022,
            ..token(&owner, &mint, 5, 1)
        };
        assert_eq!(held(Some(&elsewhere), &owner, &mint), Ok(0));
    }

    fn mint(owner: Key, decimals: u8, initialized: u8) -> Account {
        let mut data = vec![0; MINT_READ];
        data[44] = decimals;
        data[45] = initialized;
        Account {
            lamports: 1,
            owner,
            executable: false,
            data,
        }
    }

    #[test]
    fn only_initialized_classic_mints_are_tokens() {
        assert_eq!(mint_decimals(Some(&mint(TOKEN, 6, 1))), Ok(6));
        assert!(matches!(
            mint_decimals(Some(&mint(TOKEN_2022, 6, 1))),
            Err(ChainError::Unsupported(_))
        ));
        for bad in [
            None,
            Some(mint(TOKEN, 6, 0)),
            Some(mint(TOKEN, 90, 1)),
            Some(mint([0; 32], 6, 1)),
        ] {
            assert!(
                matches!(mint_decimals(bad.as_ref()), Err(ChainError::Invalid(_))),
                "{bad:?}"
            );
        }
    }

    fn record(mint: &Key, name: &[u8], symbol: &[u8]) -> Account {
        let mut data = vec![METADATA_V1];
        data.extend_from_slice(&[9; 32]);
        data.extend_from_slice(mint);
        for text in [name, symbol] {
            data.extend_from_slice(&u32::try_from(text.len()).unwrap().to_le_bytes());
            data.extend_from_slice(text);
        }
        data.extend_from_slice(&[0; 40]);
        Account {
            lamports: 1,
            owner: METADATA,
            executable: false,
            data,
        }
    }

    #[test]
    fn token_names_come_from_the_metadata_record() {
        let mint = [2; 32];
        let padded = |text: &str, width: usize| {
            let mut bytes = text.as_bytes().to_vec();
            bytes.resize(width, 0);
            bytes
        };
        assert_eq!(
            metadata(
                Some(&record(&mint, &padded("Jupiter", 32), &padded("JUP", 10))),
                &mint
            ),
            Some(("Jupiter".to_owned(), "JUP".to_owned()))
        );
        assert_eq!(
            metadata(Some(&record(&[3; 32], b"Jupiter", b"JUP")), &mint),
            None
        );
        assert_eq!(metadata(Some(&record(&mint, b"", b"JUP")), &mint), None);
        assert_eq!(
            metadata(
                Some(&record(&mint, b"Jupiter\x07\xe2\x80\xae", b"JUP")),
                &mint
            ),
            Some(("Jupiter".to_owned(), "JUP".to_owned()))
        );
        let mut huge = record(&mint, b"Jupiter", b"JUP");
        huge.data[65..69].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(metadata(Some(&huge), &mint), None);
        let mut short = record(&mint, b"Jupiter", b"JUP");
        short.data.truncate(70);
        assert_eq!(metadata(Some(&short), &mint), None);
    }
}
