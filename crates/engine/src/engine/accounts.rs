use bahaa_chain::ChainError;
use bahaa_store::MAX_ACCOUNTS;

use super::{EngineError, Worker};
use crate::portfolio::Account;
use crate::{KeyringError, WalletId, WalletKind};

impl Worker {
    pub(super) async fn add_account(
        &mut self,
        wallet: WalletId,
    ) -> Result<Vec<Account>, EngineError> {
        let kind = self.kind_of(wallet).await?;
        if kind == WalletKind::Ton {
            return Err(ChainError::Invalid("a TON recovery phrase has one account").into());
        }
        let mut indexes = self.store.account_indexes(wallet.value())?;
        if indexes.len() >= MAX_ACCOUNTS {
            return Err(ChainError::Invalid("a wallet keeps at most twenty accounts").into());
        }
        let next = indexes.last().map_or(0, |last| last.saturating_add(1));
        indexes.push(next);
        self.store.save_account_indexes(wallet.value(), &indexes)?;
        self.derive_accounts().await;
        Ok(self
            .accounts
            .iter()
            .filter(|account| account.wallet == wallet && account.index == next)
            .cloned()
            .collect())
    }

    pub(super) async fn remove_account(
        &mut self,
        wallet: WalletId,
        index: u32,
    ) -> Result<(), EngineError> {
        self.kind_of(wallet).await?;
        let mut indexes = self.store.account_indexes(wallet.value())?;
        if !indexes.contains(&index) {
            return Err(ChainError::Invalid("there is no such account").into());
        }
        if indexes.len() == 1 {
            return Err(ChainError::Invalid("a wallet keeps at least one account").into());
        }
        indexes.retain(|kept| *kept != index);
        self.store.save_account_indexes(wallet.value(), &indexes)?;
        self.derive_accounts().await;
        Ok(())
    }

    async fn kind_of(&mut self, wallet: WalletId) -> Result<WalletKind, EngineError> {
        self.ensure_unlocked()?;
        let wallets = self.keyring(|keyring| keyring.wallets()).await?;
        wallets
            .into_iter()
            .find(|info| info.id == wallet)
            .map(|info| info.kind)
            .ok_or_else(|| KeyringError::UnknownWallet.into())
    }
}
