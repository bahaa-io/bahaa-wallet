use bahaa_chain::ChainError;
use bahaa_types::{Address, TonContract};
use ruint::aliases::U256;
use tonlib_core::TonAddress;
use tonlib_core::cell::{ArcCell, Cell, CellBuilder};
use tonlib_core::tlb_types::block::message::{CommonMsgInfo, Message};
use tonlib_core::tlb_types::block::state_init::StateInit;
use tonlib_core::tlb_types::tlb::TLB;

use crate::wallet::{self, Envelope, SEND_MODE, Seal};

const W5_MESSAGES: usize = 255;
const SIGN_DATA: u32 = 0x7556_9022;
const SNAKE_BYTES: usize = 127;
const V4R2_MESSAGES: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outbound {
    pub to: Address,
    pub bounce: bool,
    pub nano: U256,
    pub body: Option<Vec<u8>>,
    pub init: Option<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Batch {
    pub envelope: Envelope,
    pub messages: Vec<Outbound>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ParsedBatch {
    pub batch: Batch,
    pub seal: Seal,
}

pub fn cell_data_hash(
    schema_crc: u32,
    timestamp: u64,
    address: &Address,
    domain: &str,
    cell: &[u8],
) -> Option<[u8; 32]> {
    let labels: Vec<u8> = domain
        .split('.')
        .rev()
        .flat_map(|label| label.bytes().chain([0]))
        .collect();
    cell_message_hash(schema_crc, timestamp, address, &labels, cell)
}

fn cell_message_hash(
    schema_crc: u32,
    timestamp: u64,
    address: &Address,
    domain: &[u8],
    cell: &[u8],
) -> Option<[u8; 32]> {
    let payload = crate::cells::root(cell)?;
    let mut message = CellBuilder::new();
    message.store_u32(32, SIGN_DATA).ok()?;
    message.store_u32(32, schema_crc).ok()?;
    message.store_u64(64, timestamp).ok()?;
    message.store_address(&wallet::to_ton(address).ok()?).ok()?;
    message.store_reference(&snake(domain)?.to_arc()).ok()?;
    message.store_reference(&payload).ok()?;
    message.build().ok()?.cell_hash().as_slice().try_into().ok()
}

fn snake(bytes: &[u8]) -> Option<Cell> {
    let mut tail: Option<Cell> = None;
    for chunk in bytes.chunks(SNAKE_BYTES).rev() {
        let mut builder = CellBuilder::new();
        builder.store_slice(chunk).ok()?;
        if let Some(next) = tail {
            builder.store_reference(&next.to_arc()).ok()?;
        }
        tail = Some(builder.build().ok()?);
    }
    match tail {
        Some(cell) => Some(cell),
        None => CellBuilder::new().build().ok(),
    }
}

pub fn max_messages(contract: TonContract) -> usize {
    match contract {
        TonContract::W5 => W5_MESSAGES,
        TonContract::V4R2 => V4R2_MESSAGES,
    }
}

impl Outbound {
    pub(crate) fn canonical(self) -> Result<Self, ChainError> {
        crate::cells::parts(&self.to)?;
        let body = match &self.body {
            Some(boc) => {
                let cell = root(boc, "a message body isn't one cell")?;
                let empty = cell.bit_len() == 0 && cell.references().is_empty();
                (!empty).then(|| wallet::serialized(&cell)).transpose()?
            }
            None => None,
        };
        let init = match &self.init {
            Some(boc) => {
                let cell = root(boc, "a contract to deploy isn't one cell")?;
                StateInit::from_cell(&cell.clone().to_arc())
                    .map_err(|_| ChainError::Invalid("a contract to deploy can't be read"))?;
                Some(wallet::serialized(&cell)?)
            }
            None => None,
        };
        Ok(Self { body, init, ..self })
    }

    fn message(&self) -> Result<ArcCell, ChainError> {
        let body = match &self.body {
            Some(boc) => root(boc, "a message body isn't one cell")?,
            None => CellBuilder::new()
                .build()
                .map_err(|_| ChainError::Invalid("cannot build the wallet message"))?,
        };
        let init = match &self.init {
            Some(boc) => Some(
                StateInit::from_cell(&root(boc, "a contract to deploy isn't one cell")?.to_arc())
                    .map_err(|_| ChainError::Invalid("a contract to deploy can't be read"))?,
            ),
            None => None,
        };
        Ok(wallet::internal(&self.to, self.bounce, self.nano, body, init, None)?.to_arc())
    }
}

fn root(boc: &[u8], broken: &'static str) -> Result<Cell, ChainError> {
    crate::cells::bag(boc)
        .and_then(|bag| bag.single_root().ok())
        .map(|root| (*root).clone())
        .ok_or(ChainError::Invalid(broken))
}

pub(crate) fn unsigned(batch: &Batch) -> Result<Cell, ChainError> {
    let count = batch.messages.len();
    if count == 0 || count > max_messages(batch.envelope.contract) {
        return Err(ChainError::Invalid(
            "this wallet can't send that many messages at once",
        ));
    }
    let messages = batch
        .messages
        .iter()
        .map(Outbound::message)
        .collect::<Result<Vec<_>, _>>()?;
    wallet::signed_body(&batch.envelope, vec![SEND_MODE; count], messages)
}

pub(crate) fn parse(boc: &[u8]) -> Result<ParsedBatch, ChainError> {
    let invalid = || ChainError::Invalid("not a signed wallet request");
    let opened = wallet::open(boc)?;
    let request = &opened.request;
    if request.msgs.is_empty()
        || request.msgs.len() > max_messages(request.contract)
        || request.modes.iter().any(|mode| *mode != SEND_MODE)
    {
        return Err(invalid());
    }
    let messages = request
        .msgs
        .iter()
        .map(|cell| outbound(cell).ok_or_else(invalid))
        .collect::<Result<Vec<_>, _>>()?;
    let batch = Batch {
        envelope: Envelope {
            wallet: opened.wallet,
            contract: request.contract,
            wallet_id: request.wallet_id,
            seqno: request.seqno,
            valid_until: request.valid_until,
        },
        messages,
    };
    let seal = wallet::resealed(&opened, &batch.envelope, &unsigned(&batch)?)?;
    Ok(ParsedBatch { batch, seal })
}

fn outbound(cell: &ArcCell) -> Option<Outbound> {
    let message = Message::from_cell(cell).ok()?;
    let CommonMsgInfo::Int(info) = &message.info else {
        return None;
    };
    if !info.ihr_disabled || info.bounced || info.value.other.is_some() {
        return None;
    }
    let to = crate::cells::from_ton(&TonAddress::from_msg_address(info.dest.clone()).ok()?)?;
    let payload = &message.body.value;
    let body = if payload.bit_len() == 0 && payload.references().is_empty() {
        None
    } else {
        Some(wallet::serialized(payload).ok()?)
    };
    let init = match &message.init {
        Some(init) => Some(wallet::serialized(&init.value.to_cell().ok()?).ok()?),
        None => None,
    };
    Some(Outbound {
        to,
        bounce: info.bounce,
        nano: U256::try_from_be_slice(&info.value.grams.amount.to_bytes_be())?,
        body,
        init,
    })
}

#[cfg(test)]
mod tests {
    use bahaa_types::hex;
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use ed25519_dalek::{Signature, VerifyingKey};

    use super::*;

    #[test]
    fn a_real_cell_signature_from_the_demo_app_verifies() {
        let (address, _) = Address::parse_ton(
            "0:f4809e5ffac9dc42a6b1d94c5e74ad5fd86378de675c805f2274d0055cbc9378",
        )
        .unwrap();
        let cell = STANDARD
            .decode("te6cckEBAQEAEQAAHgAAAABIZWxsbywgVE9OIb7WCx4=")
            .unwrap();
        let hash = cell_message_hash(
            0x2ecc_d0c1,
            1_747_772_412,
            &address,
            b"ton-connect.github.io",
            &cell,
        )
        .unwrap();
        let key = VerifyingKey::from_bytes(
            &hex::decode_array::<32>(
                "22e795a07e832fc9084ca35a488a711f1dbedef637d4e886a6997d93ee2c2e37",
            )
            .unwrap(),
        )
        .unwrap();
        let signature = Signature::from_slice(&hex::decode("6ad083855374c201c2acb14aa4e7eef44603c8d356624c8fd3b6be3babd84bd8bc7390f0ed4484ab58a535b3088681e0006839eb07136470985b3a33bfa17c05").unwrap()).unwrap();
        assert!(key.verify_strict(&hash, &signature).is_ok());
        let current = cell_data_hash(
            0x2ecc_d0c1,
            1_747_772_412,
            &address,
            "ton-connect.github.io",
            &cell,
        )
        .unwrap();
        assert!(key.verify_strict(&current, &signature).is_err());
        assert_eq!(
            Some(current),
            cell_message_hash(
                0x2ecc_d0c1,
                1_747_772_412,
                &address,
                b"io\0github\0ton-connect\0",
                &cell
            )
        );
        let other = cell_message_hash(
            0x2ecc_d0c1,
            1_747_772_412,
            &address,
            b"ton-connect.github.com",
            &cell,
        )
        .unwrap();
        assert!(key.verify_strict(&other, &signature).is_err());
        assert_eq!(cell_data_hash(1, 1, &address, "a.b", b"not a cell"), None);
    }

    #[test]
    fn long_domains_continue_in_the_next_cell() {
        let long = snake(&[b'a'; 300]).unwrap();
        assert_eq!(long.bit_len(), 127 * 8);
        assert_eq!(long.references()[0].bit_len(), 127 * 8);
        assert_eq!(long.references()[0].references()[0].bit_len(), 46 * 8);
    }
}
