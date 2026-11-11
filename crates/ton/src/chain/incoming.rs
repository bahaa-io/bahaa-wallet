use bahaa_chain::{ChainError, Incoming, Outgoing, ScanCursor, Scanned, TxId, WalkEnd, covered};
use bahaa_config::Protocol;
use bahaa_types::{Address, AssetId};

use super::{TonChain, amount, hex};
use crate::transaction::Transaction;

const MAX_SCANNED: usize = 64;

struct History {
    address: Address,
    asset: AssetId,
    decimals: u8,
    tokens: bool,
}

struct Found {
    received: Vec<Incoming>,
    sent: Vec<Outgoing>,
}

impl TonChain {
    pub fn can_scan(&self) -> bool {
        self.network.endpoints.iter().any(|endpoint| {
            matches!(
                endpoint.protocol,
                Protocol::TonLiteservers | Protocol::TonApi
            )
        })
    }

    pub async fn scan(
        &self,
        owner: &Address,
        assets: &[AssetId],
        cursor: Option<ScanCursor>,
    ) -> Result<Option<Scanned>, ChainError> {
        let histories = self.histories(owner, assets).await?;
        if histories.is_empty() {
            return Ok(None);
        }
        let stop = cursor.map_or(0, |cursor| cursor.next);
        let mut found = Found {
            received: Vec::new(),
            sent: Vec::new(),
        };
        let mut walked = Vec::with_capacity(histories.len());
        for history in &histories {
            walked.push(
                self.walk(owner, history, &histories, stop, &mut found)
                    .await?,
            );
        }
        let Found {
            received: found,
            sent,
        } = found;
        let checked = bahaa_chain::now();
        let next = covered(cursor, &walked, checked);
        if cursor.is_some_and(|cursor| cursor.next == next.next)
            && found.is_empty()
            && sent.is_empty()
        {
            return Ok(None);
        }
        Ok(Some(Scanned {
            cursor: next,
            found,
            sent,
            watch: None,
        }))
    }

    async fn histories(
        &self,
        owner: &Address,
        assets: &[AssetId],
    ) -> Result<Vec<History>, ChainError> {
        let mut histories = Vec::new();
        for asset in assets
            .iter()
            .filter(|asset| asset.network() == &self.network.id)
        {
            match asset.contract() {
                None => histories.push(History {
                    address: *owner,
                    asset: asset.clone(),
                    decimals: self.network.native.decimals,
                    tokens: false,
                }),
                Some(master) => {
                    let address = match self.jetton_wallet(owner, master).await {
                        Ok(address) => address,
                        Err(ChainError::Invalid(_) | ChainError::Rejected(_)) => continue,
                        Err(err) => return Err(err),
                    };
                    histories.push(History {
                        address,
                        asset: asset.clone(),
                        decimals: self.token_decimals(master).await?,
                        tokens: true,
                    });
                }
            }
        }
        Ok(histories)
    }

    async fn walk(
        &self,
        owner: &Address,
        history: &History,
        histories: &[History],
        stop: u64,
        found: &mut Found,
    ) -> Result<(Option<u64>, WalkEnd), ChainError> {
        let Some(mut next) = self.http.account(&history.address).await?.last else {
            return Ok((None, WalkEnd::Missing));
        };
        let mut newest = None;
        let mut oldest = None;
        let mut scanned = 0;
        loop {
            let list = match self.http.transactions(&history.address, next).await {
                Ok(list) => list,
                Err(err) => {
                    return match oldest {
                        Some(time) => Ok((newest, WalkEnd::Capped(time))),
                        None => Err(err),
                    };
                }
            };
            for transaction in list {
                let time = u64::from(transaction.now);
                newest.get_or_insert(time);
                if time < stop {
                    return Ok((newest, WalkEnd::Connected));
                }
                if let Some(entry) = receipt(owner, history, &transaction)? {
                    found.received.push(entry);
                }
                if !history.tokens {
                    found
                        .sent
                        .extend(departures(owner, history, histories, &transaction)?);
                }
                scanned += 1;
                oldest = Some(time);
                next = transaction.previous;
                if next.1 == 0 {
                    return Ok((newest, WalkEnd::Start(time)));
                }
                if scanned >= MAX_SCANNED {
                    return Ok((newest, WalkEnd::Capped(time)));
                }
            }
        }
    }
}

fn receipt(
    owner: &Address,
    history: &History,
    transaction: &Transaction,
) -> Result<Option<Incoming>, ChainError> {
    let credit = if history.tokens {
        &transaction.tokens
    } else {
        &transaction.credit
    };
    let Some(credit) = credit else {
        return Ok(None);
    };
    Ok(Some(Incoming {
        id: TxId::new(hex::encode(&transaction.hash))?,
        index: 0,
        from: credit.from,
        to: *owner,
        asset: history.asset.clone(),
        amount: amount(credit.units, history.decimals)?,
        block: transaction.lt,
        time: u64::from(transaction.now),
        memo: credit.memo.clone(),
    }))
}

fn departures(
    owner: &Address,
    history: &History,
    histories: &[History],
    transaction: &Transaction,
) -> Result<Vec<Outgoing>, ChainError> {
    let (Some(external), Some(true)) = (transaction.external, transaction.succeeded()) else {
        return Ok(Vec::new());
    };
    let id = TxId::new(hex::encode(&external))?;
    let mut sent = Vec::new();
    for (index, message) in transaction.sent.iter().enumerate() {
        let index = u64::try_from(index).map_err(|_| ChainError::Invalid("too many messages"))?;
        let moved = match message.token {
            Some((units, to)) => histories
                .iter()
                .find(|known| known.tokens && known.address == message.to)
                .map(|wallet| (to, wallet.asset.clone(), units, wallet.decimals)),
            None => (message.plain && !message.value.is_zero()).then(|| {
                (
                    message.to,
                    history.asset.clone(),
                    message.value,
                    history.decimals,
                )
            }),
        };
        if let Some((to, asset, units, decimals)) = moved {
            sent.push(Outgoing {
                id: id.clone(),
                index,
                from: *owner,
                to,
                asset,
                amount: amount(units, decimals)?,
                block: transaction.lt,
                time: u64::from(transaction.now),
                memo: message.memo.clone(),
            });
        }
    }
    Ok(sent)
}
