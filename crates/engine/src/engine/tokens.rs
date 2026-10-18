use std::sync::Arc;

use bahaa_chain::ChainError;
use bahaa_config::Coin;
use bahaa_store::CustomToken;
use bahaa_types::{Address, AssetId, to_plain_text};

use super::{EngineError, Event, Notice, Reply, Worker};
use crate::KeyringError;

const MAX_NAME: usize = 40;

pub(super) struct TokenRead {
    session: u64,
    chain: usize,
    asset: AssetId,
    result: Result<Coin, ChainError>,
    reply: Reply<CustomToken>,
}

impl Worker {
    pub(super) fn tokens(&self) -> Result<Vec<CustomToken>, EngineError> {
        if !self.unlocked() {
            return Err(KeyringError::Locked.into());
        }
        let mut tokens = self.store.tokens()?;
        tokens.sort_by(|a, b| {
            (a.coin.symbol.to_lowercase(), a.asset.to_string())
                .cmp(&(b.coin.symbol.to_lowercase(), b.asset.to_string()))
        });
        Ok(tokens)
    }

    pub(super) fn add_token(&mut self, asset: AssetId, reply: Reply<CustomToken>) {
        let (chain, contract) = match self.token_target(&asset) {
            Ok(target) => target,
            Err(err) => {
                let _ = reply.send(Err(err));
                return;
            }
        };
        let chains = Arc::clone(&self.chains);
        let notices = self.notices_tx.clone();
        let session = self.session;
        tokio::spawn(async move {
            let result = chains[chain].token(&contract).await;
            let read = TokenRead {
                session,
                chain,
                asset,
                result,
                reply,
            };
            let _ = notices.send(Notice::TokenRead(Box::new(read))).await;
        });
    }

    fn token_target(&self, asset: &AssetId) -> Result<(usize, Address), EngineError> {
        if !self.unlocked() {
            return Err(KeyringError::Locked.into());
        }
        let AssetId::Token(network, contract) = asset else {
            return Err(ChainError::Invalid("a network's own coin is always shown").into());
        };
        let chain = self
            .chain_index(network)
            .ok_or(EngineError::UnknownNetwork)?;
        let network = self.chains[chain].network();
        if network.family() != contract.family() {
            return Err(ChainError::Invalid("this address is for another kind of network").into());
        }
        if network
            .tokens
            .iter()
            .any(|token| token.contract == *contract)
        {
            return Err(ChainError::Invalid("this token is already listed").into());
        }
        Ok((chain, *contract))
    }

    pub(super) fn token_read(&mut self, read: TokenRead) {
        let TokenRead {
            session,
            chain,
            asset,
            result,
            reply,
        } = read;
        let saved = if session == self.session && self.unlocked() {
            result
                .map_err(EngineError::from)
                .and_then(|coin| self.save_token(chain, asset, coin))
        } else {
            Err(KeyringError::Locked.into())
        };
        let _ = reply.send(saved);
    }

    fn save_token(
        &mut self,
        chain: usize,
        asset: AssetId,
        coin: Coin,
    ) -> Result<CustomToken, EngineError> {
        let network = self.chains[chain].network();
        let spoofed = std::iter::once(&network.native)
            .chain(network.tokens.iter().map(|token| &token.coin))
            .any(|listed| listed.symbol.eq_ignore_ascii_case(&coin.symbol));
        if spoofed {
            return Err(ChainError::Invalid(
                "a listed coin on this network already uses this symbol",
            )
            .into());
        }
        let name = to_plain_text(&coin.name, MAX_NAME);
        let token = CustomToken {
            coin: Coin {
                name: if name.is_empty() {
                    coin.symbol.clone()
                } else {
                    name
                },
                price: None,
                ..coin
            },
            asset,
        };
        self.store.save_token(&token)?;
        if let Some(contract) = token.asset.contract() {
            self.chains[chain].know(contract, token.coin.decimals);
        }
        self.tokens_changed()?;
        self.restart_refresh();
        Ok(token)
    }

    pub(super) fn remove_token(&mut self, asset: &AssetId) -> Result<bool, EngineError> {
        if !self.unlocked() {
            return Err(KeyringError::Locked.into());
        }
        let removed = self.store.remove_token(asset)?;
        if removed {
            self.held.retain(|held| held.asset != *asset);
            self.tokens_changed()?;
            self.publish_portfolio();
            self.restart_refresh();
        }
        Ok(removed)
    }

    fn tokens_changed(&mut self) -> Result<(), EngineError> {
        self.custom = Arc::new(self.store.tokens()?);
        let _ = self.events.send(Event::TokensChanged(self.tokens()?));
        Ok(())
    }
}
