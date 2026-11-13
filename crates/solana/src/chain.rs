use std::collections::{BTreeSet, HashMap};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use bahaa_chain::{
    Balance, Chain, ChainError, Decimals, Decoded, FeeQuote, Replace, Signed, Speed, Transfer,
    TxId, TxStatus, amount, now, on_network,
};
use bahaa_config::{ChainKind, Coin, Network};
use bahaa_crypto::{DerivationPath, Seed};
use bahaa_types::{Address, Amount, AssetId, NftId, SolanaPath, to_plain_text};
use ed25519_dalek::SigningKey;
use futures_util::future::try_join;
use ruint::aliases::U256;

mod calls;
mod incoming;
mod nfts;
mod send;

pub use calls::{SolanaCall, swap_instruction};
pub use send::SolanaPlan;

use crate::programs::{Moved, Shape, TOKEN, TOKEN_2022, asset_id, metadata_account, token_account};
use crate::rpc::{self, Account, Rpc, Solana};
use crate::token::{Mint, Scale, TOKEN_READ, held};
use crate::tx::{self, Key};

pub const QUOTE_LIFE: Duration = Duration::from_secs(45);

const MAX_NAME: usize = 40;
const MAX_SYMBOL: usize = 11;

#[derive(Debug)]
pub struct SolanaChain {
    network: Network,
    rpc: Rpc,
    decimals: Decimals,
    rent: Mutex<Option<(Instant, send::Rent)>>,
    nft_mints: Mutex<BTreeSet<Key>>,
    seen: Mutex<HashMap<Key, Mint>>,
    quoted: Mutex<HashMap<Key, Mint>>,
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
            nft_mints: Mutex::new(BTreeSet::new()),
            seen: Mutex::new(HashMap::new()),
            quoted: Mutex::new(HashMap::new()),
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

    async fn slice(
        &self,
        key: &Key,
        offset: usize,
        length: usize,
    ) -> Result<Option<Vec<u8>>, ChainError> {
        let reply = self
            .rpc
            .call("getAccountInfo", rpc::slice_params(key, offset, length))
            .await?;
        match rpc::account(&reply["value"])? {
            Some(account) if account.data.len() == length => Ok(Some(account.data)),
            Some(_) => Err(ChainError::malformed()),
            None => Ok(None),
        }
    }

    fn symbol(&self, mint: &Key) -> String {
        bahaa_chain::symbol(&self.network, &Address::Solana(*mint))
    }

    async fn mints(&self, keys: &[Key], fresh: bool) -> Result<Vec<Mint>, ChainError> {
        let wanted: Vec<Key> = {
            let seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
            keys.iter()
                .filter(|key| fresh || seen.get(*key).is_none_or(|mint| mint.scale.varies()))
                .copied()
                .collect()
        };
        let found = if wanted.is_empty() {
            Vec::new()
        } else {
            self.accounts(&wanted, None).await?
        };
        let now = now();
        let mut read = HashMap::new();
        for (key, account) in wanted.iter().zip(&found) {
            let mint = Mint::read(account.as_ref(), key, now)?;
            let address = Address::Solana(*key);
            if self
                .decimals
                .get(&self.network, &address)
                .is_some_and(|known| known != mint.decimals)
            {
                return Err(ChainError::Mismatch("the token decimals"));
            }
            self.decimals.learn(address, mint.decimals);
            read.insert(*key, mint);
        }
        let mut seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
        seen.extend(read);
        keys.iter()
            .map(|key| seen.get(key).cloned().ok_or_else(ChainError::malformed))
            .collect()
    }

    pub async fn raw_units(&self, asset: &AssetId, amount: &Amount) -> Result<u64, ChainError> {
        self.scale_of(asset, true).await?.0.raw(amount.units())
    }

    pub async fn shown_amount(&self, asset: &AssetId, raw: u64) -> Result<Amount, ChainError> {
        let (scale, decimals) = self.scale_of(asset, true).await?;
        amount(scale.shown(raw)?, decimals)
    }

    async fn scale_of(&self, asset: &AssetId, fresh: bool) -> Result<(Scale, u8), ChainError> {
        let Some(contract) = asset.contract() else {
            return Ok((Scale::PLAIN, self.network.native.decimals));
        };
        let key = bytes(contract)?;
        if !fresh && let Ok(quoted) = self.quoted(&key) {
            return Ok((quoted.scale, quoted.decimals));
        }
        let found = self
            .mints(&[key], true)
            .await?
            .into_iter()
            .next()
            .ok_or_else(ChainError::malformed)?;
        self.quote_mint(&found, key);
        Ok((found.scale, found.decimals))
    }

    fn quote_mint(&self, mint: &Mint, key: Key) {
        self.quoted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key, mint.clone());
    }

    fn quoted(&self, mint: &Key) -> Result<Mint, ChainError> {
        self.quoted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(mint)
            .cloned()
            .ok_or(ChainError::Invalid(
                "unknown token, prepare the transfer first",
            ))
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
            keys.push(token_account(&owner, &TOKEN, mint)?);
            keys.push(token_account(&owner, &TOKEN_2022, mint)?);
        }
        let (found, details) = try_join(
            self.accounts(&keys, Some(TOKEN_READ)),
            self.mints(&mints, false),
        )
        .await?;
        let mut tokens = mints.iter().zip(found[1..].chunks(2)).zip(details);
        assets
            .iter()
            .map(|asset| {
                let amount = if asset.contract().is_none() {
                    amount(
                        U256::from(found[0].as_ref().map_or(0, |account| account.lamports)),
                        self.network.native.decimals,
                    )?
                } else {
                    let ((mint, pair), details) =
                        tokens.next().ok_or_else(ChainError::malformed)?;
                    let account = if details.program == TOKEN {
                        &pair[0]
                    } else {
                        &pair[1]
                    };
                    let units = held(account.as_ref(), &details.program, &owner, mint)?;
                    amount(details.scale.shown(units)?, details.decimals)?
                };
                Ok(Balance {
                    asset: asset.clone(),
                    amount,
                    staked: None,
                })
            })
            .collect()
    }

    async fn prepare_with(
        &self,
        transfer: &Transfer,
        speed: Speed,
    ) -> Result<(SolanaPlan, FeeQuote), ChainError> {
        Box::pin(self.plan(transfer, speed)).await
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
        let nft = |id: Key, units: U256| {
            NftId::new(Address::Solana(id), None)
                .map(|nft| {
                    (
                        AssetId::Nft(self.network.id.clone(), Box::new(nft)),
                        0,
                        units,
                    )
                })
                .map_err(|_| ChainError::malformed())
        };
        let known_nft = |mint: &Key| {
            self.nft_mints
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .contains(mint)
        };
        let units = U256::from(shape.units);
        let (asset, decimals, units) = match &shape.moved {
            Moved::Coin => (
                AssetId::Native(self.network.id.clone()),
                self.network.native.decimals,
                units,
            ),
            Moved::Core { asset, .. } | Moved::Programmable { mint: asset, .. } => {
                nft(*asset, units)?
            }
            Moved::Compressed(moving) => {
                nft(asset_id(&moving.leaf.tree, moving.leaf.nonce)?, units)?
            }
            Moved::Token {
                mint,
                decimals: 0,
                program: TOKEN,
                fee: None,
            } if known_nft(mint) => nft(*mint, units)?,
            Moved::Token {
                mint,
                decimals,
                program,
                ..
            } => {
                let quoted = self.quoted(mint)?;
                if quoted.decimals != *decimals {
                    return Err(ChainError::Mismatch("the token decimals"));
                }
                if quoted.program != *program {
                    return Err(ChainError::Mismatch("the token program"));
                }
                (
                    AssetId::Token(self.network.id.clone(), Address::Solana(*mint)),
                    *decimals,
                    quoted.scale.shown(shape.units)?,
                )
            }
        };
        Ok(Decoded {
            from: Address::Solana(shape.from),
            to: Address::Solana(shape.to),
            asset,
            amount: amount(units, decimals)?,
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
        let details = Mint::read(found[0].as_ref(), &mint, now())?;
        let (name, symbol) = details
            .named
            .as_ref()
            .and_then(|(name, symbol)| named(name, symbol))
            .or_else(|| metadata(found[1].as_ref(), &mint))
            .ok_or(ChainError::Invalid("this token has no name on chain"))?;
        Ok(Coin {
            symbol,
            name,
            decimals: details.decimals,
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

fn metadata(account: Option<&Account>, mint: &Key) -> Option<(String, String)> {
    let found = crate::metaplex::metadata(account, mint)?;
    named(&found.name, &found.symbol)
}

fn named(name: &str, symbol: &str) -> Option<(String, String)> {
    let clean = |text: &str, max: usize| {
        let clean = to_plain_text(text, max);
        (!clean.is_empty()).then_some(clean)
    };
    Some((clean(name, MAX_NAME)?, clean(symbol, MAX_SYMBOL)?))
}

#[cfg(fuzzing)]
pub(crate) fn fuzz(value: &serde_json::Value) {
    if let Ok(found) = rpc::accounts(value, value["value"].as_array().map_or(0, Vec::len)) {
        for account in &found {
            crate::token::fuzz(account.as_ref());
            let _ = metadata(account.as_ref(), &[0; 32]);
            let _ = crate::metaplex::token_lock(account.as_ref());
            if let Some(tree) = account.as_ref().and_then(|account| {
                crate::bubblegum::Tree::read(&account.owner, &account.data, account.space)
            }) {
                let _ = (tree.root_at(), tree.changes(), tree.needed());
                let _ = account.as_ref().map(|account| tree.recent(&account.data));
            }
        }
    }
    if let Ok(found) = rpc::accounts(value, value["value"].as_array().map_or(0, Vec::len)) {
        for account in &found {
            let _ = crate::metaplex::core(account.as_ref());
            let _ = crate::metaplex::core_collection(account.as_ref());
        }
    }
    incoming::fuzz(value);
    nfts::fuzz(value);
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
    use crate::metaplex::METADATA_V1;
    use crate::programs::METADATA;

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
            space: 679,
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
