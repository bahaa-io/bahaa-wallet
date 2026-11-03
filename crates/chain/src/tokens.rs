use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use bahaa_config::Network;
use bahaa_types::Address;

#[derive(Debug, Default)]
pub struct Decimals(Mutex<HashMap<Address, u8>>);

impl Decimals {
    pub fn get(&self, network: &Network, contract: &Address) -> Option<u8> {
        network
            .token(contract)
            .map(|coin| coin.decimals)
            .or_else(|| {
                self.0
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get(contract)
                    .copied()
            })
    }

    pub fn learn(&self, contract: Address, decimals: u8) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(contract, decimals);
    }
}

pub fn symbol(network: &Network, contract: &Address) -> String {
    network
        .token(contract)
        .map_or_else(|| "this token".to_owned(), |coin| coin.symbol.clone())
}
