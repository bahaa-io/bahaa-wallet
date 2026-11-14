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
