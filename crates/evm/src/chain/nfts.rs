use std::collections::BTreeSet;

use bahaa_chain::{
    ChainError, Failure, FeeQuote, Found, NftMove, Pool, Speed, Transfer, abi_string, abi_text,
    amount, enough, http, nft_transfer_data,
};
use bahaa_config::{Endpoint, Feature, Protocol};
use bahaa_types::{
    Address, AssetId, Collection, MAX_NFT_LINK, MAX_NFT_TEXT, MAX_NFTS, Nft, NftId, NftKind, Trust,
    is_plain_text, to_plain_text,
};
use futures_util::{StreamExt, TryStreamExt, stream};
use reqwest::{Client, RequestBuilder, StatusCode};
use ruint::aliases::U256;
use serde_json::Value;

use super::{EvmChain, EvmPlan, NAME, Route, bytes, hex, word};

const AT_ONCE: usize = 8;
const READ_GAS: u64 = 10_000_000;
const BLOCKSCOUT_PAGES: usize = 6;
const ROUTESCAN_PAGES: usize = 3;
const ROUTESCAN_LIMIT: &str = "100";
const OWNER_OF: &str = "6352211e";
const BALANCE_OF_ID: &str = "00fdd58e";
const TOKEN_URI: &str = "c87b56dd";
const URI: &str = "0e89341c";
const SUPPORTS: &str = "01ffc9a7";
const ERC721: &str = "80ac58cd";
const ERC1155: &str = "d9b67a26";
const ID_PLACEHOLDER: &str = "{id}";

#[derive(Debug)]
pub(super) struct Listings {
    pool: Pool,
    http: Client,
}

impl Listings {
    pub(super) fn new(endpoints: Vec<Endpoint>) -> Result<Self, ChainError> {
        Ok(Self {
            pool: Pool::new(endpoints),
            http: http::client()?,
        })
    }

    async fn get(&self, request: RequestBuilder) -> Result<Value, Failure<ChainError>> {
        let (status, body) = http::exchange(request).await?;
        if matches!(
            status,
            StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
        ) {
            return Err(Failure::unsupported("an endpoint does not list NFTs"));
        }
        if !status.is_success() {
            return Err(Failure::retry("an NFT listing failed"));
        }
        serde_json::from_slice(&body).map_err(|_| Failure::retry("an NFT listing is not JSON"))
    }

    async fn blockscout(
        &self,
        endpoint: &Endpoint,
        owner: &[u8; 20],
    ) -> Result<Vec<Listed>, Failure<ChainError>> {
        let mut query = vec![("type".to_owned(), "ERC-721,ERC-1155".to_owned())];
        let mut listed = Vec::new();
        for _ in 0..BLOCKSCOUT_PAGES {
            let url = located(endpoint, &["addresses", &hex(owner), "nft"], &query)?;
            let reply = self.get(self.http.get(url)).await?;
            let items = reply["items"]
                .as_array()
                .ok_or_else(|| Failure::retry("an NFT listing is malformed"))?;
            listed.extend(items.iter().filter_map(blockscout_item));
            let Some(next) = reply["next_page_params"].as_object() else {
                break;
            };
            if listed.len() > MAX_NFTS {
                break;
            }
            query.truncate(1);
            for (key, value) in next {
                let value = match value {
                    Value::String(text) => text.clone(),
                    Value::Number(number) => number.to_string(),
                    _ => return Err(Failure::retry("an NFT listing is malformed")),
                };
                query.push((key.clone(), value));
            }
        }
        Ok(listed)
    }

    async fn routescan(
        &self,
        endpoint: &Endpoint,
        owner: &[u8; 20],
    ) -> Result<Vec<Listed>, Failure<ChainError>> {
        let mut listed = Vec::new();
        for (path, kind) in [
            ("erc721-holdings", NftKind::Erc721),
            ("erc1155-holdings", NftKind::Erc1155),
        ] {
            let mut next: Option<String> = None;
            for _ in 0..ROUTESCAN_PAGES {
                let mut query = vec![("limit".to_owned(), ROUTESCAN_LIMIT.to_owned())];
                query.extend(next.take().map(|token| ("next".to_owned(), token)));
                let url = located(endpoint, &["address", &hex(owner), path], &query)?;
                let reply = self.get(self.http.get(url)).await?;
                let items = reply["items"]
                    .as_array()
                    .ok_or_else(|| Failure::retry("an NFT listing is malformed"))?;
                listed.extend(items.iter().filter_map(|item| routescan_item(item, kind)));
                next = reply["link"]["nextToken"].as_str().map(str::to_owned);
                if next.is_none() || listed.len() > MAX_NFTS {
                    break;
                }
            }
        }
        Ok(listed)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Listed {
    contract: [u8; 20],
    id: U256,
    kind: NftKind,
}

struct Checked {
    name: Option<String>,
}

impl EvmChain {
    pub async fn nfts(&self, owner: &Address) -> Result<Found, ChainError> {
        let owner = bytes(owner)?;
        let (listed, capped) = self.listing(&owner).await?;
        let mut contracts: Vec<([u8; 20], NftKind)> = listed
            .iter()
            .map(|listed| (listed.contract, listed.kind))
            .collect();
        contracts.sort_unstable();
        contracts.dedup();
        let reads: Vec<_> = contracts
            .iter()
            .map(|(contract, kind)| self.collection(contract, *kind))
            .collect();
        let checked: Vec<Option<Checked>> =
            stream::iter(reads).buffered(AT_ONCE).try_collect().await?;
        let mut reads = Vec::new();
        for item in &listed {
            let found = contracts
                .binary_search(&(item.contract, item.kind))
                .ok()
                .and_then(|index| checked.get(index)?.as_ref());
            if let Some(found) = found {
                reads.push(self.held_nft(&owner, item, found));
            }
        }
        let items: Vec<Option<Nft>> = stream::iter(reads).buffered(AT_ONCE).try_collect().await?;
        Ok(Found {
            items: items.into_iter().flatten().collect(),
            capped,
        })
    }

    async fn listing(&self, owner: &[u8; 20]) -> Result<(Vec<Listed>, bool), ChainError> {
        let listings = &self.listings;
        Ok(listings
            .pool
            .call(Some(Feature::Nfts), |endpoint| async move {
                let found = match endpoint.protocol {
                    Protocol::Blockscout => listings.blockscout(&endpoint, owner).await?,
                    Protocol::Routescan => listings.routescan(&endpoint, owner).await?,
                    _ => return Err(Failure::unsupported("an endpoint does not list NFTs")),
                };
                let mut seen = BTreeSet::new();
                let mut listed: Vec<Listed> = found
                    .into_iter()
                    .filter(|item| seen.insert(*item))
                    .collect();
                let capped = listed.len() > MAX_NFTS;
                listed.truncate(MAX_NFTS);
                Ok((listed, capped))
            })
            .await?)
    }

    async fn collection(
        &self,
        contract: &[u8; 20],
        kind: NftKind,
    ) -> Result<Option<Checked>, ChainError> {
        if !self.supports(contract, kind).await? {
            return Ok(None);
        }
        let name = self
            .nft_read(contract, NAME)
            .await?
            .and_then(|data| abi_text(&data))
            .and_then(|name| text(&name));
        Ok(Some(Checked { name }))
    }

    async fn held_nft(
        &self,
        owner: &[u8; 20],
        listed: &Listed,
        checked: &Checked,
    ) -> Result<Option<Nft>, ChainError> {
        let units = self
            .units_held(listed.kind, &listed.contract, owner, listed.id)
            .await?;
        if units.is_zero() {
            return Ok(None);
        }
        let id = digits(listed.id);
        let link_call = match listed.kind {
            NftKind::Erc1155 => format!("{URI}{id}"),
            _ => format!("{TOKEN_URI}{id}"),
        };
        let link = self
            .nft_read(&listed.contract, &link_call)
            .await?
            .and_then(|data| abi_string(&data, MAX_NFT_LINK))
            .map(|link| link.replace(ID_PLACEHOLDER, &id))
            .filter(|link| !link.is_empty() && is_plain_text(link, MAX_NFT_LINK));
        let nft = NftId::new(Address::Evm(listed.contract), Some(listed.id))
            .map_err(|_| ChainError::malformed())?;
        Ok(Some(Nft {
            asset: AssetId::Nft(self.network.id.clone(), Box::new(nft)),
            kind: listed.kind,
            amount: amount(units, 0)?,
            collection: Some(Collection {
                address: Some(Address::Evm(listed.contract)),
                name: checked.name.clone(),
            }),
            name: None,
            link,
            trust: Trust::Unknown,
            lock: None,
        }))
    }

    async fn units_held(
        &self,
        kind: NftKind,
        contract: &[u8; 20],
        owner: &[u8; 20],
        id: U256,
    ) -> Result<U256, ChainError> {
        let id = digits(id);
        match kind {
            NftKind::Erc721 => {
                let held = self
                    .nft_read(contract, &format!("{OWNER_OF}{id}"))
                    .await?
                    .is_some_and(|data| {
                        data.len() == 32 && data[..12] == [0u8; 12] && data[12..] == owner[..]
                    });
                Ok(U256::from(u8::from(held)))
            }
            NftKind::Erc1155 => Ok(self
                .nft_read(contract, &format!("{BALANCE_OF_ID}{}{id}", word(owner)))
                .await?
                .filter(|data| data.len() == 32)
                .map_or(U256::ZERO, |data| U256::from_be_slice(&data))),
            _ => Ok(U256::ZERO),
        }
    }

    async fn supports(&self, contract: &[u8; 20], kind: NftKind) -> Result<bool, ChainError> {
        let interface = match kind {
            NftKind::Erc721 => ERC721,
            NftKind::Erc1155 => ERC1155,
            _ => return Ok(false),
        };
        let call = format!("{SUPPORTS}{interface}{}", "0".repeat(56));
        Ok(self
            .nft_read(contract, &call)
            .await?
            .is_some_and(|data| U256::try_from_be_slice(&data) == Some(U256::from(1u8))))
    }

    async fn kind_of(&self, contract: &[u8; 20]) -> Result<NftKind, ChainError> {
        for kind in [NftKind::Erc721, NftKind::Erc1155] {
            if self.supports(contract, kind).await? {
                return Ok(kind);
            }
        }
        Err(ChainError::Invalid("this is not an NFT contract"))
    }

    async fn nft_read(
        &self,
        contract: &[u8; 20],
        call: &str,
    ) -> Result<Option<Vec<u8>>, ChainError> {
        let data = alloy_primitives::hex::decode(call).map_err(|_| ChainError::malformed())?;
        match self.read_with(contract, &data, Some(READ_GAS)).await {
            Ok(found) => Ok((!found.is_empty()).then_some(found)),
            Err(ChainError::Rejected(_)) => Ok(None),
            Err(err) => Err(err),
        }
    }

    pub(super) async fn prepare_nft(
        &self,
        transfer: &Transfer,
        speed: Speed,
    ) -> Result<(EvmPlan, FeeQuote), ChainError> {
        let nft = transfer.check_nft(&self.network.id)?;
        if transfer.memo.is_some() {
            return Err(ChainError::Unsupported("memos on NFT transfers"));
        }
        let (from, to) = (bytes(&transfer.from)?, bytes(&transfer.to)?);
        if from == to {
            return Err(ChainError::Invalid("send the NFT to another account"));
        }
        let contract = bytes(nft.address())?;
        let id = nft.token().ok_or_else(ChainError::malformed)?;
        let kind = self.kind_of(&contract).await?;
        let (held, nonce, native) = futures_util::future::try_join3(
            self.units_held(kind, &contract, &from, id),
            self.nonce(&from),
            self.native_balance(&from),
        )
        .await?;
        if held.is_zero() {
            return Err(ChainError::Invalid("this wallet doesn't hold that NFT"));
        }
        let mut transfer = transfer.clone();
        if transfer.max {
            transfer.take_all(amount(held, 0)?)?;
        }
        let units = transfer.amount.units();
        if kind == NftKind::Erc721 && units != U256::from(1u8) {
            return Err(ChainError::Invalid(
                "an ERC-721 NFT is one of a kind, send just 1",
            ));
        }
        if units > held {
            return Err(ChainError::Invalid("this wallet holds fewer of that NFT"));
        }
        let moved = NftMove {
            kind,
            from,
            to,
            id,
            units,
        };
        let route = Route {
            call_to: contract,
            value: U256::ZERO,
            data: nft_transfer_data(&moved).ok_or_else(ChainError::malformed)?,
            token: None,
            decimals: 0,
        };
        let (tx, fees) =
            self.priced_tx(&from, nonce, &route, speed)
                .await
                .map_err(|err| match err {
                    ChainError::Rejected(_) => ChainError::Rejected(
                        "the NFT contract refuses this transfer, the recipient may not accept NFTs"
                            .to_owned(),
                    ),
                    other => other,
                })?;
        enough(
            &self.network.native.symbol,
            fees.max,
            native,
            self.network.native.decimals,
        )?;
        Ok((EvmPlan { transfer, tx }, self.fee_quote(fees)?))
    }
}

fn located(
    endpoint: &Endpoint,
    path: &[&str],
    query: &[(String, String)],
) -> Result<String, Failure<ChainError>> {
    let mut url = endpoint.url.clone();
    url.path_segments_mut()
        .map_err(|()| Failure::unsupported("an NFT endpoint URL cannot take a path"))?
        .pop_if_empty()
        .extend(path);
    url.query_pairs_mut().extend_pairs(query);
    Ok(url.into())
}

fn blockscout_item(item: &Value) -> Option<Listed> {
    let kind = match item["token_type"].as_str()? {
        "ERC-721" => NftKind::Erc721,
        "ERC-1155" => NftKind::Erc1155,
        _ => return None,
    };
    let token = &item["token"];
    let contract = token["address_hash"]
        .as_str()
        .or_else(|| token["address"].as_str())?;
    Some(Listed {
        contract: contract_bytes(contract)?,
        id: decimal(item["id"].as_str()?)?,
        kind,
    })
}

fn routescan_item(item: &Value, kind: NftKind) -> Option<Listed> {
    Some(Listed {
        contract: contract_bytes(item["tokenAddress"].as_str()?)?,
        id: decimal(item["tokenId"].as_str()?)?,
        kind,
    })
}

fn contract_bytes(text: &str) -> Option<[u8; 20]> {
    let digits = text.strip_prefix("0x")?;
    if digits.len() != 40 {
        return None;
    }
    bahaa_types::hex::decode_array(&digits.to_ascii_lowercase())
}

fn decimal(text: &str) -> Option<U256> {
    if text.is_empty() || text.len() > 78 || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    U256::from_str_radix(text, 10).ok()
}

fn digits(id: U256) -> String {
    format!("{id:064x}")
}

fn text(raw: &str) -> Option<String> {
    let clean = to_plain_text(raw, MAX_NFT_TEXT);
    (!clean.is_empty()).then_some(clean)
}

#[cfg(fuzzing)]
pub(super) fn fuzz(value: &Value) {
    let _ = blockscout_item(value);
    let _ = routescan_item(value, NftKind::Erc1155);
}
