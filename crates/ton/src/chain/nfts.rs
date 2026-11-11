use bahaa_chain::{ChainError, Found, Transfer};
use bahaa_types::{
    Address, Amount, AssetId, Collection, Lock, MAX_NFT_LINK, MAX_NFT_TEXT, Nft, NftId, NftKind,
    Trust, is_plain_text, to_plain_text,
};
use futures_util::{StreamExt, TryStreamExt, stream};
use ruint::aliases::U256;

use super::TonChain;
use super::tokens::{Contracts, Parties, query_id};
use crate::cells::Account;
use crate::http::{Item, Listed};
use crate::metadata::{self, Content};
use crate::vm::Arg;
use crate::wallet::{self, Order, read_address, read_optional_address};

const AT_ONCE: usize = 8;

struct Held {
    listed: Listed,
    state: Account,
    index: U256,
    collection: Option<Address>,
    content: Vec<u8>,
    soulbound: bool,
}

struct Owner {
    address: Address,
    state: Account,
    name: Option<String>,
}

impl TonChain {
    pub async fn nfts(&self, owner: &Address) -> Result<Found, ChainError> {
        crate::cells::parts(owner)?;
        let (listed, capped) = self.http.nfts(owner).await?;
        let held: Vec<Held> =
            stream::iter(listed.into_iter().map(|listed| self.held(owner, listed)))
                .buffered(AT_ONCE)
                .try_collect::<Vec<_>>()
                .await?
                .into_iter()
                .flatten()
                .collect();
        let mut wanted: Vec<Address> = held.iter().filter_map(|held| held.collection).collect();
        wanted.sort_unstable();
        wanted.dedup();
        let collections: Vec<Owner> =
            stream::iter(wanted.into_iter().map(|address| self.collection(address)))
                .buffered(AT_ONCE)
                .try_collect()
                .await?;
        let mut items = Vec::with_capacity(held.len());
        for held in held {
            let collection = held
                .collection
                .and_then(|address| collections.iter().find(|owner| owner.address == address));
            items.push(self.item(held, collection).await?);
        }
        Ok(Found { items, capped })
    }

    async fn held(&self, owner: &Address, listed: Listed) -> Result<Option<Held>, ChainError> {
        let state = self.http.account(&listed.address).await?;
        let Some(stack) = refusal_as_none(self.http.run_on(&state, "get_nft_data", &[]).await)?
        else {
            return Ok(None);
        };
        let [
            init,
            Item::Num(index),
            Item::Cell(collection),
            Item::Cell(holder),
            Item::Cell(content),
        ] = stack.as_slice()
        else {
            return Ok(None);
        };
        let initialized = match init {
            Item::Negative => true,
            Item::Num(flag) => !flag.is_zero(),
            Item::Cell(_) => false,
        };
        if !initialized || read_address(holder) != Some(*owner) {
            return Ok(None);
        }
        let Ok(collection) = read_optional_address(collection) else {
            return Ok(None);
        };
        let soulbound =
            refusal_as_none(self.http.run_on(&state, "get_authority_address", &[]).await)?
                .is_some();
        Ok(Some(Held {
            listed,
            state,
            index: *index,
            collection,
            content: content.clone(),
            soulbound,
        }))
    }

    pub(super) async fn item_order(
        &self,
        transfer: &Transfer,
        item: &Address,
        order: &mut Order,
        wallet: &Account,
        contracts: Contracts<'_>,
    ) -> Result<u64, ChainError> {
        if transfer.to == transfer.from {
            return Err(ChainError::Invalid("send the NFT to another account"));
        }
        let listed = Listed {
            address: *item,
            trust: Trust::Unknown,
        };
        let held = self
            .held(&transfer.from, listed)
            .await?
            .ok_or(ChainError::Invalid("this wallet doesn't hold that NFT"))?;
        if held.soulbound {
            return Err(ChainError::Invalid(
                "this NFT is bound to its owner and can't be sent",
            ));
        }
        order.bounce = true;
        order.item = Some(wallet::Item {
            address: *item,
            query_id: query_id(order),
        });
        let parties = Parties {
            ours: (item, &held.state.boc),
            theirs: None,
            wallet: &wallet.boc,
        };
        self.attachment(order, &parties, &contracts).await
    }

    async fn collection(&self, address: Address) -> Result<Owner, ChainError> {
        let state = self.http.account(&address).await?;
        let data = refusal_as_none(self.http.run_on(&state, "get_collection_data", &[]).await)?;
        let name = match data.as_deref() {
            Some([Item::Num(_), Item::Cell(content), Item::Cell(_)]) => {
                metadata::content(content).and_then(|content| text(content.name, MAX_NFT_TEXT))
            }
            _ => None,
        };
        Ok(Owner {
            address,
            state,
            name,
        })
    }

    async fn item(&self, held: Held, collection: Option<&Owner>) -> Result<Nft, ChainError> {
        let mut trust = held.listed.trust;
        let mut content = metadata::content(&held.content);
        let mut belongs = None;
        if let Some(owner) = collection {
            let index = Arg::Number(held.index);
            let found = refusal_as_none(
                self.http
                    .run_on(&owner.state, "get_nft_address_by_index", &[index])
                    .await,
            )?;
            if matches!(found.as_deref(), Some([Item::Cell(boc)]) if read_address(boc) == Some(held.listed.address))
            {
                let full = refusal_as_none(
                    self.http
                        .run_on(
                            &owner.state,
                            "get_nft_content",
                            &[index, Arg::Cell(&held.content)],
                        )
                        .await,
                )?;
                content = match full.as_deref() {
                    Some([Item::Cell(boc)]) => metadata::content(boc),
                    _ => None,
                };
                belongs = Some(Collection {
                    address: Some(owner.address),
                    name: owner.name.clone(),
                });
            } else {
                trust = Trust::Flagged;
            }
        } else if held.collection.is_some() {
            trust = Trust::Flagged;
        }
        let Content { uri, name, .. } = content.unwrap_or_default();
        Ok(Nft {
            asset: AssetId::Nft(
                self.network.id.clone(),
                Box::new(
                    NftId::new(held.listed.address, None).map_err(|_| ChainError::malformed())?,
                ),
            ),
            kind: NftKind::TonItem,
            amount: Amount::new(U256::from(1u8), 0).map_err(|_| ChainError::overflow())?,
            collection: belongs,
            name: text(name, MAX_NFT_TEXT),
            link: uri.filter(|uri| is_plain_text(uri, MAX_NFT_LINK)),
            trust,
            lock: held.soulbound.then_some(Lock::Soulbound),
        })
    }
}

fn refusal_as_none(
    ran: Result<Option<Vec<Item>>, ChainError>,
) -> Result<Option<Vec<Item>>, ChainError> {
    match ran {
        Err(ChainError::Rejected(_)) => Ok(None),
        other => other,
    }
}

fn text(text: Option<String>, max: usize) -> Option<String> {
    text.map(|text| to_plain_text(&text, max))
        .filter(|text| !text.is_empty())
}
