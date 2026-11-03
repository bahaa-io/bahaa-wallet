use bahaa_chain::ChainError;
use bahaa_types::Address;
use num_bigint::BigUint;
use ruint::aliases::U256;
use tonlib_core::TonAddress;
use tonlib_core::cell::CellBuilder;

use crate::wallet::{serialized, to_ton};

pub struct Body(CellBuilder);

impl Default for Body {
    fn default() -> Self {
        Self(CellBuilder::new())
    }
}

fn broken<T>(_: T) -> ChainError {
    ChainError::Invalid("cannot build the message body")
}

impl Body {
    pub fn uint(mut self, bits: usize, value: u64) -> Result<Self, ChainError> {
        self.0.store_u64(bits, value).map_err(broken)?;
        Ok(self)
    }

    pub fn coins(mut self, units: U256) -> Result<Self, ChainError> {
        self.0
            .store_coins(&BigUint::from_bytes_be(&units.to_be_bytes::<32>()))
            .map_err(broken)?;
        Ok(self)
    }

    pub fn address(mut self, address: Option<&Address>) -> Result<Self, ChainError> {
        let address = address.map(to_ton).transpose()?.unwrap_or(TonAddress::NULL);
        self.0.store_address(&address).map_err(broken)?;
        Ok(self)
    }

    pub fn bit(mut self, value: bool) -> Result<Self, ChainError> {
        self.0.store_bit(value).map_err(broken)?;
        Ok(self)
    }

    pub fn reference(mut self, mut child: Self) -> Result<Self, ChainError> {
        let cell = child.0.build().map_err(broken)?;
        self.0.store_reference(&cell.to_arc()).map_err(broken)?;
        Ok(self)
    }

    pub fn boc(mut self) -> Result<Vec<u8>, ChainError> {
        serialized(&self.0.build().map_err(broken)?)
    }
}

pub fn read_jetton_amount(boc: &[u8]) -> Option<U256> {
    let root = crate::cells::bag(boc)?.single_root().ok()?;
    let mut parser = root.parser();
    if parser.load_u32(32).ok()? != crate::wallet::TOKEN_TRANSFER {
        return None;
    }
    parser.load_u64(64).ok()?;
    U256::try_from_be_slice(&parser.load_coins().ok()?.to_bytes_be())
}
