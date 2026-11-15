use bahaa_chain::{ChainError, FeeQuote, Link, NameRecord};
use bahaa_types::{Address, Amount};
use ruint::aliases::U256;
use sha2::{Digest, Sha256};
use tonlib_core::cell::{BagOfCells, Cell, CellBuilder};

use super::batch::{Meaning, Requested, TonBatch};
use super::dns::{SMART_CONTRACT, WALLET, normalize, read_record};
use super::nfts::{nft_data, refusal_as_none};
use super::send::prices;
use super::{TonChain, amount};
use crate::batch::Outbound;
use crate::cells::{self, Account};
use crate::emulate::{Emulated, Emulator, Hop};
use crate::fees::Settings;
use crate::http::Item;
use crate::vm::Arg;
use crate::wallet;

pub(crate) const CHANGE_DNS_RECORD: u32 = 0x4eb1_f0f9;
const PROBE: u64 = 100_000_000;
const LIFE: u64 = 300;
const YEAR: u64 = 365 * 24 * 3600;
const TON_ENDING: &str = ".ton";
const REFUSED: &str = "the name's contract refused this change";

#[derive(Clone, Debug)]
pub struct Linking {
    pub record: NameRecord,
    pub link: Link,
    pub attach: Amount,
    pub returned: bool,
    pub batch: TonBatch,
    pub fee: FeeQuote,
}

impl TonChain {
    pub async fn name_record(&self, item: &Address) -> Result<NameRecord, ChainError> {
        cells::parts(item)?;
        let state = self.http.account(item).await?;
        self.record_at(item, &state).await
    }

    pub async fn find_name(&self, name: &str) -> Result<Option<NameRecord>, ChainError> {
        Ok(self.named(name).await?.map(|(record, _, _)| record))
    }

    pub async fn prepare_link(
        &self,
        account: u32,
        from: &Address,
        name: &str,
        link: Link,
    ) -> Result<Linking, ChainError> {
        let (record, item, state) = self
            .named(name)
            .await?
            .ok_or(ChainError::Invalid("this name isn't registered"))?;
        let item = &item;
        if record.owner != *from {
            return Err(ChainError::Invalid("this name belongs to another address"));
        }
        let (expected, changes) = match link {
            Link::To(to) if record.wallet == Some(to) => {
                return Err(ChainError::Invalid(
                    "the name already points to this address",
                ));
            }
            Link::To(to) => {
                cells::parts(&to)?;
                (Some(to), true)
            }
            Link::Clear if record.wallet.is_none() => {
                return Err(ChainError::Invalid("this name isn't linked to an address"));
            }
            Link::Clear => (None, true),
            Link::Renew if !record.renewable => {
                return Err(ChainError::Unsupported("renewing names that never expire"));
            }
            Link::Renew => (record.wallet, false),
            Link::Primary => return Err(ChainError::Unsupported("primary names on TON")),
        };
        let now = bahaa_chain::now();
        let body = changes
            .then(|| record_body(now, expected.as_ref()))
            .transpose()?;
        let attach = self
            .link_attach(&state, (from, item), body.as_deref(), expected)
            .await?;
        let requested = Requested {
            account,
            from: *from,
            messages: vec![Outbound {
                to: *item,
                bounce: true,
                nano: U256::from(attach),
                body,
                init: None,
            }],
            valid_until: Some(now + LIFE),
        };
        let (batch, fee) = self.prepare_batch(&requested).await?;
        let returned = !record.renewable;
        Ok(Linking {
            record,
            link,
            attach: amount(U256::from(attach), self.network.native.decimals)?,
            returned,
            batch,
            fee,
        })
    }

    async fn named(
        &self,
        name: &str,
    ) -> Result<Option<(NameRecord, Address, Account)>, ChainError> {
        let name = normalize(name)?;
        let Some(item) = self.dns_walk(&name).await?.map(|walked| walked.resolver) else {
            return Ok(None);
        };
        let state = self.http.account(&item).await?;
        let record = self.record_at(&item, &state).await?;
        if record.name != name {
            return Err(ChainError::Unsupported(
                "changing names kept by a contract that isn't their NFT",
            ));
        }
        Ok(Some((record, item, state)))
    }

    async fn record_at(&self, item: &Address, state: &Account) -> Result<NameRecord, ChainError> {
        let data = refusal_as_none(self.http.run_on(state, "get_nft_data", &[]).await)?;
        let owner = data
            .as_deref()
            .and_then(nft_data)
            .map(|(_, _, holder, _)| holder)
            .ok_or(ChainError::Invalid("this NFT has no owner yet"))?;
        let name = self.item_name(state).await?;
        let walked = self
            .dns_walk(&name)
            .await?
            .filter(|walked| walked.resolver == *item)
            .ok_or(ChainError::Invalid(
                "this NFT doesn't hold the name it shows",
            ))?;
        let expires_at = if name.ends_with(TON_ENDING) {
            let renewed =
                refusal_as_none(self.http.run_on(state, "get_last_fill_up_time", &[]).await)?;
            match renewed.as_deref() {
                Some([Item::Num(at)]) => Some(
                    u64::try_from(*at)
                        .ok()
                        .and_then(|at| at.checked_add(YEAR))
                        .ok_or_else(ChainError::malformed)?,
                ),
                _ => return Err(ChainError::malformed()),
            }
        } else {
            None
        };
        Ok(NameRecord {
            name,
            network: self.network.id.clone(),
            owner,
            wallet: walked.wallet,
            elsewhere: false,
            renewable: expires_at.is_some(),
            reversible: false,
            expires_at,
        })
    }

    async fn item_name(&self, state: &Account) -> Result<String, ChainError> {
        let label = refusal_as_none(self.http.run_on(state, "get_domain", &[]).await)?;
        if let Some([Item::Cell(boc)]) = label.as_deref()
            && let Some(name) = cell_bytes(boc).and_then(|label| ton_name(&label))
        {
            return Ok(name);
        }
        let full = refusal_as_none(self.http.run_on(state, "get_full_domain", &[]).await)?;
        if let Some([Item::Cell(boc)]) = full.as_deref()
            && let Some(name) = cell_bytes(boc).and_then(|full| full_name(&full))
        {
            return Ok(name);
        }
        Err(ChainError::Unsupported(
            "linking NFTs that aren't TON names",
        ))
    }

    async fn link_attach(
        &self,
        state: &Account,
        (from, item): (&Address, &Address),
        body: Option<&[u8]>,
        expected: Option<Address>,
    ) -> Result<u64, ChainError> {
        let config = self.http.config().await?;
        let (settings, _) = prices(&config)?;
        let params = config.encoded().ok_or_else(|| {
            ChainError::Network("the network config cannot be encoded".to_owned())
        })?;
        let clock = u32::try_from(bahaa_chain::now())
            .map_err(|_| ChainError::Invalid("the clock is out of range"))?;
        let run = (params.as_slice(), settings, clock);
        let probe = self.item_hop(run, state, (from, item), body, PROBE).await?;
        if !probe.success {
            return Err(ChainError::Rejected(REFUSED.to_owned()));
        }
        let attach = probe.fees.checked_mul(2).ok_or_else(ChainError::overflow)?;
        let hop = self
            .item_hop(run, state, (from, item), body, attach)
            .await?;
        let after = cells::account(&hop.state, item).ok_or_else(ChainError::malformed)?;
        if !hop.success || self.wallet_record(&after).await? != expected {
            return Err(ChainError::Rejected(REFUSED.to_owned()));
        }
        Ok(attach)
    }

    async fn item_hop(
        &self,
        (params, settings, clock): (&[(i32, Vec<u8>)], Settings, u32),
        state: &Account,
        (from, item): (&Address, &Address),
        body: Option<&[u8]>,
        nano: u64,
    ) -> Result<Hop, ChainError> {
        let message = incoming(from, item, nano, body)?;
        let mut libraries = Vec::new();
        for _ in 0..=crate::vm::MAX_LIBRARIES {
            let ran = {
                let emulator =
                    Emulator::new(params, &libraries, settings, clock).ok_or_else(|| {
                        ChainError::Network("the network config cannot be emulated".to_owned())
                    })?;
                emulator.run(item, &state.boc, &message)?
            };
            match ran {
                Emulated::Done(hop) => return Ok(hop),
                Emulated::Missing(hash) => libraries.push(self.http.library(&hash).await?),
            }
        }
        Err(ChainError::Network(
            "a contract needs too many libraries".to_owned(),
        ))
    }

    async fn wallet_record(&self, state: &Account) -> Result<Option<Address>, ChainError> {
        let category = U256::from_be_bytes::<32>(Sha256::digest(WALLET.as_bytes()).into());
        let answer = self
            .http
            .run_on(
                state,
                "dnsresolve",
                &[Arg::Bytes(&[0]), Arg::Number(category)],
            )
            .await?;
        Ok(match answer.as_deref() {
            Some([Item::Num(_), Item::Cell(boc)]) => read_record(boc, SMART_CONTRACT),
            _ => None,
        })
    }
}

pub(crate) fn record_change(root: &Cell) -> Option<Meaning> {
    let mut parser = root.parser();
    if parser.load_u32(32).ok()? != CHANGE_DNS_RECORD {
        return None;
    }
    parser.load_u64(64).ok()?;
    let mut key = [0u8; 32];
    parser.load_slice(&mut key).ok()?;
    if key != <[u8; 32]>::from(Sha256::digest(WALLET.as_bytes())) {
        return None;
    }
    let wallet = match root.references() {
        [] => None,
        [value] => Some(read_record(
            &wallet::serialized(value).ok()?,
            SMART_CONTRACT,
        )?),
        _ => return None,
    };
    Some(Meaning::Record { wallet })
}

fn record_body(query_id: u64, wallet: Option<&Address>) -> Result<Vec<u8>, ChainError> {
    let broken = |_| ChainError::Invalid("cannot build the name record change");
    let mut body = CellBuilder::new();
    body.store_u32(32, CHANGE_DNS_RECORD).map_err(broken)?;
    body.store_u64(64, query_id).map_err(broken)?;
    body.store_slice(&Sha256::digest(WALLET.as_bytes()))
        .map_err(broken)?;
    if let Some(wallet) = wallet {
        let mut value = CellBuilder::new();
        value.store_u16(16, SMART_CONTRACT).map_err(broken)?;
        value
            .store_address(&wallet::to_ton(wallet)?)
            .map_err(broken)?;
        value.store_u8(8, 0).map_err(broken)?;
        body.store_reference(&value.build().map_err(broken)?.to_arc())
            .map_err(broken)?;
    }
    wallet::serialized(&body.build().map_err(broken)?)
}

fn incoming(
    from: &Address,
    item: &Address,
    nano: u64,
    body: Option<&[u8]>,
) -> Result<Vec<u8>, ChainError> {
    let broken = |_| ChainError::Invalid("cannot build the name record change");
    let body = match body {
        Some(boc) => cells::root(boc)
            .map(|root| (*root).clone())
            .ok_or_else(ChainError::malformed)?,
        None => CellBuilder::new().build().map_err(broken)?,
    };
    let message = wallet::internal(item, true, U256::from(nano), body, None, Some(from))?;
    BagOfCells::from_root(message)
        .serialize(false)
        .map_err(|_| ChainError::Invalid("cannot build the name record change"))
}

fn cell_bytes(boc: &[u8]) -> Option<Vec<u8>> {
    let cell = cells::root(boc)?;
    let bits = cell.bit_len();
    (bits % 8 == 0 && cell.references().is_empty()).then(|| cell.data()[..bits / 8].to_vec())
}

fn ton_name(label: &[u8]) -> Option<String> {
    let label = std::str::from_utf8(label).ok()?;
    normalize(&format!("{label}{TON_ENDING}"))
        .ok()
        .filter(|name| name.matches('.').count() == 1)
}

fn full_name(full: &[u8]) -> Option<String> {
    let labels: Vec<&str> = full
        .split(|byte| *byte == 0)
        .filter(|label| !label.is_empty())
        .map(std::str::from_utf8)
        .collect::<Result<_, _>>()
        .ok()?;
    let name: Vec<&str> = labels.into_iter().rev().collect();
    (name.len() >= 2)
        .then(|| normalize(&name.join(".")).ok())
        .flatten()
}

#[cfg(fuzzing)]
pub(crate) fn fuzz(data: &[u8]) {
    if let Some(root) = cells::root(data) {
        let _ = record_change(&root);
    }
    let _ = cell_bytes(data).map(|bytes| (ton_name(&bytes), full_name(&bytes)));
    let _ = ton_name(data);
    let _ = full_name(data);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_change_reads_back_as_what_was_built() {
        let wallet = Address::parse_ton(
            "0:4bab343ae759aa67cfe7a0af4ab3d2258484fd593142c57e9cad3c064039f4dd",
        )
        .unwrap()
        .0;
        let set = record_body(7, Some(&wallet)).unwrap();
        let root = cells::root(&set).unwrap();
        assert_eq!(
            record_change(&root),
            Some(Meaning::Record {
                wallet: Some(wallet)
            })
        );
        let cleared = record_body(7, None).unwrap();
        assert_eq!(
            record_change(&cells::root(&cleared).unwrap()),
            Some(Meaning::Record { wallet: None })
        );
        let comment = crate::comment::build("hi").unwrap();
        assert_eq!(record_change(&comment), None);
    }

    #[test]
    fn item_domains_become_names() {
        assert_eq!(ton_name(b"foundation").as_deref(), Some("foundation.ton"));
        assert_eq!(ton_name(b"a.b"), None);
        assert_eq!(ton_name(b"bad name"), None);
        assert_eq!(full_name(b"me\0t\0gosal\0").as_deref(), Some("gosal.t.me"));
        assert_eq!(full_name(b"gosal\0"), None);
        assert_eq!(full_name(b"me\0t\0\xff\0"), None);
    }
}
