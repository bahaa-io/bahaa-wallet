use bahaa_chain::ChainError;
use bahaa_types::{Address, TonContract};
use num_bigint::BigUint;
use ruint::aliases::U256;
use tonlib_core::cell::{ArcCell, BagOfCells, Cell, CellBuilder};
use tonlib_core::tlb_types::block::coins::{CurrencyCollection, Grams};
use tonlib_core::tlb_types::block::message::{CommonMsgInfo, ExtInMsgInfo, IntMsgInfo, Message};
use tonlib_core::tlb_types::block::msg_address::{MsgAddrNone, MsgAddressExt};
use tonlib_core::tlb_types::block::state_init::StateInit;
use tonlib_core::tlb_types::tlb::TLB;
use tonlib_core::wallet::mnemonic::KeyPair;
use tonlib_core::wallet::ton_wallet::TonWallet;
use tonlib_core::wallet::version_helper::VersionHelper;
use tonlib_core::wallet::versioned::v4::{WalletDataV4, WalletExtMsgBodyV4};
use tonlib_core::wallet::versioned::v5::{WalletDataV5, WalletExtMsgBodyV5};
use tonlib_core::wallet::wallet_version::WalletVersion;
use tonlib_core::{TonAddress, TonHash};

pub(crate) const SEND_MODE: u8 = 3;
pub(crate) const TOKEN_TRANSFER: u32 = 0x0f8a_7ea5;
pub(crate) const FORWARD_NANO: u8 = 1;
pub(crate) const V4R2_WALLET_ID: i32 = 0x29a9_a317;
const V4R2_GAS: u64 = 3_308;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Token {
    pub wallet: Address,
    pub units: U256,
    pub query_id: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Order {
    pub wallet: Address,
    pub contract: TonContract,
    pub wallet_id: i32,
    pub seqno: u32,
    pub valid_until: u32,
    pub to: Address,
    pub nano: U256,
    pub bounce: bool,
    pub token: Option<Token>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Parsed {
    pub order: Order,
    pub id: [u8; 32],
    pub unsigned: [u8; 32],
    pub signature: [u8; 64],
    pub public_key: Option<[u8; 32]>,
}

pub(crate) fn wallet_id(global_id: i32) -> i32 {
    global_id ^ i32::MIN
}

fn version(contract: TonContract) -> WalletVersion {
    match contract {
        TonContract::W5 => WalletVersion::V5R1,
        TonContract::V4R2 => WalletVersion::V4R2,
    }
}

pub(crate) fn address(
    contract: TonContract,
    public_key: &[u8; 32],
    wallet_id: i32,
) -> Result<Address, ChainError> {
    let keys = KeyPair {
        public_key: public_key.to_vec(),
        secret_key: Vec::new(),
    };
    let wallet = TonWallet::new_with_params(version(contract), keys, 0, wallet_id)
        .map_err(|_| ChainError::Invalid("cannot build the wallet address"))?;
    from_ton(&wallet.address).ok_or(ChainError::Invalid("cannot build the wallet address"))
}

pub(crate) fn contract_of(code: &[u8; 32]) -> Option<TonContract> {
    [TonContract::W5, TonContract::V4R2]
        .into_iter()
        .find(|contract| code_hash(*contract).as_ref() == Some(code))
}

pub(crate) fn code_hash(contract: TonContract) -> Option<[u8; 32]> {
    let code = VersionHelper::get_code(version(contract)).ok()?;
    Cell::cell_hash(code).as_slice().try_into().ok()
}

pub(crate) fn seqno(
    contract: TonContract,
    data: Option<&ArcCell>,
    wallet_id: i32,
) -> Result<u32, ChainError> {
    let unreadable = || ChainError::Invalid("the wallet's data is unreadable");
    let data = data.ok_or_else(unreadable)?;
    let (seqno, accepts) = match contract {
        TonContract::W5 => {
            let data = WalletDataV5::from_cell(data).map_err(|_| unreadable())?;
            (
                data.seqno,
                data.wallet_id == wallet_id && data.signature_allowed,
            )
        }
        TonContract::V4R2 => {
            let data = WalletDataV4::from_cell(data).map_err(|_| unreadable())?;
            (data.seqno, data.wallet_id == wallet_id)
        }
    };
    accepts.then_some(seqno).ok_or(ChainError::Invalid(
        "this wallet does not take signed transfers for this network",
    ))
}

pub(crate) fn transfer_gas(contract: TonContract) -> u64 {
    match contract {
        TonContract::W5 => crate::fees::TRANSFER_GAS,
        TonContract::V4R2 => V4R2_GAS,
    }
}

pub(crate) fn unsigned(order: &Order) -> Result<Cell, ChainError> {
    let broken = |_| ChainError::Invalid("cannot build the wallet message");
    let transfer = outgoing(order, None)?.to_arc();
    match order.contract {
        TonContract::W5 => WalletExtMsgBodyV5 {
            wallet_id: order.wallet_id,
            valid_until: order.valid_until,
            msg_seqno: order.seqno,
            msgs_modes: vec![SEND_MODE],
            msgs: vec![transfer],
        }
        .to_cell(),
        TonContract::V4R2 => WalletExtMsgBodyV4 {
            subwallet_id: order.wallet_id,
            valid_until: order.valid_until,
            msg_seqno: order.seqno,
            opcode: 0,
            msgs_modes: vec![SEND_MODE],
            msgs: vec![transfer],
        }
        .to_cell(),
    }
    .map_err(broken)
}

pub(crate) fn delivered(order: &Order) -> Result<Vec<u8>, ChainError> {
    BagOfCells::from_root(outgoing(order, Some(&order.wallet))?)
        .serialize(false)
        .map_err(|_| ChainError::Invalid("cannot build the wallet message"))
}

fn outgoing(order: &Order, source: Option<&Address>) -> Result<Cell, ChainError> {
    let broken = |_| ChainError::Invalid("cannot build the wallet message");
    let (destination, bounce, body) = match &order.token {
        None => (
            order.to,
            order.bounce,
            CellBuilder::new().build().map_err(broken)?,
        ),
        Some(token) => (token.wallet, true, token_body(order, token)?),
    };
    let src = match source {
        None => MsgAddrNone {}.into(),
        Some(address) => to_ton(address)?.to_msg_address_int().into(),
    };
    Message::new(
        CommonMsgInfo::Int(IntMsgInfo {
            ihr_disabled: true,
            bounce,
            bounced: false,
            src,
            dest: to_ton(&destination)?.to_msg_address_int().into(),
            value: CurrencyCollection::new(BigUint::from_bytes_be(&order.nano.to_be_bytes::<32>())),
            ihr_fee: Grams::new(BigUint::default()),
            fwd_fee: Grams::new(BigUint::default()),
            created_lt: 0,
            created_at: 0,
        }),
        body.to_arc(),
    )
    .to_cell()
    .map_err(broken)
}

fn token_body(order: &Order, token: &Token) -> Result<Cell, ChainError> {
    let broken = |_| ChainError::Invalid("cannot build the token transfer");
    let mut body = CellBuilder::new();
    body.store_u32(32, TOKEN_TRANSFER).map_err(broken)?;
    body.store_u64(64, token.query_id).map_err(broken)?;
    body.store_coins(&BigUint::from_bytes_be(&token.units.to_be_bytes::<32>()))
        .map_err(broken)?;
    body.store_address(&to_ton(&order.to)?).map_err(broken)?;
    body.store_address(&to_ton(&order.wallet)?)
        .map_err(broken)?;
    body.store_bit(false).map_err(broken)?;
    body.store_coins(&BigUint::from(FORWARD_NANO))
        .map_err(broken)?;
    body.store_bit(false).map_err(broken)?;
    body.build().map_err(broken)
}

fn token_transfer(body: &Cell, wallet: Address) -> Option<(Address, Token)> {
    let mut parser = body.parser();
    if parser.load_u32(32).ok()? != TOKEN_TRANSFER {
        return None;
    }
    let query_id = parser.load_u64(64).ok()?;
    let units = U256::try_from_be_slice(&parser.load_coins().ok()?.to_bytes_be())?;
    let to = from_ton(&parser.load_address().ok()?)?;
    Some((
        to,
        Token {
            wallet,
            units,
            query_id,
        },
    ))
}

pub(crate) fn transfer_size(order: &Order) -> Result<(u64, u64), ChainError> {
    Ok(crate::fees::size_without_root(
        &outgoing(order, None)?.to_arc(),
    ))
}

pub(crate) fn external(
    order: &Order,
    signature: &[u8; 64],
    deploy_key: Option<&[u8; 32]>,
) -> Result<Cell, ChainError> {
    let broken = |_| ChainError::Invalid("cannot build the wallet message");
    let body = VersionHelper::sign_msg(version(order.contract), &unsigned(order)?, signature)
        .map_err(broken)?;
    let mut message = Message::new(
        CommonMsgInfo::ExtIn(ExtInMsgInfo {
            src: MsgAddressExt::None(MsgAddrNone {}),
            dest: to_ton(&order.wallet)?.to_msg_address_int(),
            import_fee: Grams::new(BigUint::default()),
        }),
        body.to_arc(),
    );
    if let Some(key) = deploy_key {
        let keys = KeyPair {
            public_key: key.to_vec(),
            secret_key: Vec::new(),
        };
        let code = VersionHelper::get_code(version(order.contract))
            .map_err(broken)?
            .clone();
        let data = VersionHelper::get_data(version(order.contract), &keys, order.wallet_id)
            .map_err(broken)?;
        message.with_state_init(StateInit::new(code, data.to_arc()));
    }
    message
        .to_cell()
        .map_err(|_| ChainError::Invalid("cannot build the wallet message"))
}

pub(crate) fn parse(boc: &[u8]) -> Result<Parsed, ChainError> {
    let invalid = || ChainError::Invalid("not a signed wallet transfer");
    let root = BagOfCells::parse(boc)
        .map_err(|_| invalid())?
        .single_root()
        .map_err(|_| invalid())?;
    let message = Message::from_cell(&root).map_err(|_| invalid())?;
    let rebuilt = message.to_cell().map_err(|_| invalid())?;
    if rebuilt.cell_hash() != Cell::cell_hash(&root) {
        return Err(invalid());
    }
    let CommonMsgInfo::ExtIn(info) = &message.info else {
        return Err(invalid());
    };
    let wallet = from_ton(&TonAddress::from_msg_address(info.dest.clone()).map_err(|_| invalid())?)
        .ok_or_else(invalid)?;
    let body = &message.body.value;
    let ((Some(request), None) | (None, Some(request))) = (
        request(body, TonContract::W5),
        request(body, TonContract::V4R2),
    ) else {
        return Err(invalid());
    };
    let ([transfer], [SEND_MODE]) = (request.msgs.as_slice(), request.modes.as_slice()) else {
        return Err(ChainError::Invalid("not a single plain transfer"));
    };
    let inner = Message::from_cell(transfer).map_err(|_| invalid())?;
    let CommonMsgInfo::Int(sent) = &inner.info else {
        return Err(invalid());
    };
    let simple =
        inner.init.is_none() && sent.ihr_disabled && !sent.bounced && sent.value.other.is_none();
    if !simple {
        return Err(ChainError::Invalid("not a single plain transfer"));
    }
    let destination =
        from_ton(&TonAddress::from_msg_address(sent.dest.clone()).map_err(|_| invalid())?)
            .ok_or_else(invalid)?;
    let payload = &inner.body.value;
    let (to, token) = if payload.bit_len() == 0 && payload.references().is_empty() {
        (destination, None)
    } else {
        let (to, token) = token_transfer(payload, destination)
            .ok_or(ChainError::Invalid("not a single plain transfer"))?;
        (to, Some(token))
    };
    let nano =
        U256::try_from_be_slice(&sent.value.grams.amount.to_bytes_be()).ok_or_else(invalid)?;
    let order = Order {
        wallet,
        contract: request.contract,
        wallet_id: request.wallet_id,
        seqno: request.seqno,
        valid_until: request.valid_until,
        to,
        nano,
        bounce: sent.bounce,
        token,
    };
    let unsigned_cell = unsigned(&order)?;
    let resigned =
        VersionHelper::sign_msg(version(order.contract), &unsigned_cell, &request.signature)
            .map_err(|_| invalid())?;
    if resigned.cell_hash() != Cell::cell_hash(body) {
        return Err(invalid());
    }
    let public_key = match &message.init {
        None => None,
        Some(init) => Some(deployed_key(&init.value, &order)?),
    };
    Ok(Parsed {
        order,
        id: normalized(&root).ok_or_else(invalid)?,
        unsigned: hash_of(&unsigned_cell.to_arc())?,
        signature: request.signature,
        public_key,
    })
}

struct Request {
    contract: TonContract,
    wallet_id: i32,
    seqno: u32,
    valid_until: u32,
    modes: Vec<u8>,
    msgs: Vec<ArcCell>,
    signature: [u8; 64],
}

fn request(body: &ArcCell, contract: TonContract) -> Option<Request> {
    let mut parser = body.parser();
    let (wallet_id, seqno, valid_until, modes, msgs, signature) = match contract {
        TonContract::W5 => {
            let read = WalletExtMsgBodyV5::read(&mut parser).ok()?;
            let signature: [u8; 64] = parser.load_bytes(64).ok()?.try_into().ok()?;
            (
                read.wallet_id,
                read.msg_seqno,
                read.valid_until,
                read.msgs_modes,
                read.msgs,
                signature,
            )
        }
        TonContract::V4R2 => {
            let signature: [u8; 64] = parser.load_bytes(64).ok()?.try_into().ok()?;
            let read = WalletExtMsgBodyV4::read(&mut parser).ok()?;
            (
                read.subwallet_id,
                read.msg_seqno,
                read.valid_until,
                read.msgs_modes,
                read.msgs,
                signature,
            )
        }
    };
    parser.ensure_empty().ok()?;
    Some(Request {
        contract,
        wallet_id,
        seqno,
        valid_until,
        modes,
        msgs,
        signature,
    })
}

pub(crate) fn normalized(message: &Cell) -> Option<[u8; 32]> {
    let parsed = Message::from_cell(message).ok()?;
    let CommonMsgInfo::ExtIn(info) = &parsed.info else {
        return None;
    };
    let mut cell = CellBuilder::new();
    cell.store_u8(2, 0b10).ok()?.store_u8(2, 0).ok()?;
    cell.store_address(&TonAddress::from_msg_address(info.dest.clone()).ok()?)
        .ok()?;
    cell.store_u8(4, 0).ok()?;
    cell.store_bit(false).ok()?.store_bit(true).ok()?;
    cell.store_reference(&parsed.body.value).ok()?;
    hash_of(&cell.build().ok()?.to_arc()).ok()
}

fn deployed_key(init: &StateInit, order: &Order) -> Result<[u8; 32], ChainError> {
    let invalid = || ChainError::Invalid("the deployment is not this wallet's contract");
    let code = init.code.as_ref().ok_or_else(invalid)?;
    let data = init.data.as_ref().ok_or_else(invalid)?;
    let plain = init.split_depth.is_none() && init.tick_tock.is_none() && init.library.is_none();
    let hash: [u8; 32] = Cell::cell_hash(&code.0)
        .as_slice()
        .try_into()
        .map_err(|_| invalid())?;
    if !plain || contract_of(&hash) != Some(order.contract) {
        return Err(invalid());
    }
    let key = match order.contract {
        TonContract::W5 => {
            let wallet = WalletDataV5::from_cell(&data.0).map_err(|_| invalid())?;
            let fresh = wallet.seqno == 0
                && wallet.wallet_id == order.wallet_id
                && wallet.signature_allowed
                && wallet.extensions.is_none();
            fresh.then_some(wallet.public_key).ok_or_else(invalid)?
        }
        TonContract::V4R2 => {
            let wallet = WalletDataV4::from_cell(&data.0).map_err(|_| invalid())?;
            let fresh = wallet.seqno == 0
                && wallet.wallet_id == order.wallet_id
                && wallet.plugins.is_none();
            fresh.then_some(wallet.public_key).ok_or_else(invalid)?
        }
    };
    key.as_slice().try_into().map_err(|_| invalid())
}

fn hash_of(cell: &ArcCell) -> Result<[u8; 32], ChainError> {
    Cell::cell_hash(cell)
        .as_slice()
        .try_into()
        .map_err(|_| ChainError::Invalid("not a signed wallet transfer"))
}

pub(crate) fn read_address(boc: &[u8]) -> Option<Address> {
    let root = BagOfCells::parse(boc).ok()?.single_root().ok()?;
    let mut parser = root.parser();
    let address = parser.load_address().ok()?;
    parser.ensure_empty().ok()?;
    from_ton(&address)
}

fn to_ton(address: &Address) -> Result<TonAddress, ChainError> {
    match address {
        Address::Ton { workchain, hash } => {
            Ok(TonAddress::new(i32::from(*workchain), TonHash::from(*hash)))
        }
        _ => Err(ChainError::Invalid("not a TON address")),
    }
}

fn from_ton(address: &TonAddress) -> Option<Address> {
    Some(Address::Ton {
        workchain: i8::try_from(address.workchain).ok()?,
        hash: address.hash_part.as_slice().try_into().ok()?,
    })
}

#[cfg(test)]
mod tests {
    use tonlib_core::wallet::versioned::{DEFAULT_WALLET_ID_V5R1, DEFAULT_WALLET_ID_V5R1_TESTNET};

    use super::*;

    fn bytes32(hex: &str) -> [u8; 32] {
        let bytes: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).unwrap())
            .collect();
        bytes.try_into().unwrap()
    }

    #[test]
    fn wallet_ids_come_from_the_network_global_id() {
        assert_eq!(wallet_id(-239), DEFAULT_WALLET_ID_V5R1);
        assert_eq!(wallet_id(-3), DEFAULT_WALLET_ID_V5R1_TESTNET);
    }

    #[test]
    fn matches_real_mainnet_v5r1_wallets() {
        let wallets = [
            (
                "91b95e79b57d1e702ce4f03614daf5a4c33380c2c44e2588d0fdb8f2343727bc",
                "0:2fdc85e117595e1553968958a3128cfc4969de100505afd539d0dfcedd5a27a4",
            ),
            (
                "80c69399198063101a2d03ce081e849ba5581fb560156780b9d116f96c7f3104",
                "0:13fd9907ed89660d9b90f925bd54a6b029cafb368a172888f1ffc6fd690624e5",
            ),
        ];
        for (public_key, expected) in wallets {
            let built = address(TonContract::W5, &bytes32(public_key), wallet_id(-239)).unwrap();
            assert_eq!(built.to_string(), expected);
        }
        let testnet = address(TonContract::W5, &bytes32(wallets[0].0), wallet_id(-3)).unwrap();
        assert_ne!(testnet.to_string(), wallets[0].1);
    }

    #[test]
    fn addresses_survive_the_cell_round_trip() {
        let owner = Address::Ton {
            workchain: 0,
            hash: bytes32("f8b8635d0786b77f9da660e65fd31d670a675dd4302facba692f46b571eb5360"),
        };
        let from_node = bytes32_vec(
            "b5ee9c72010101010024000043801f170c6ba0f0d6eff3b4cc1ccbfa63ace14cebba8605f5974d25e8d6ae3d6a6c10",
        );
        assert_eq!(read_address(&from_node), Some(owner));
        assert_eq!(read_address(&[0xb5, 0xee]), None);
    }

    fn bytes32_vec(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn a_relay_cannot_change_the_id_of_a_signed_message() {
        let order = Order {
            wallet: address(TonContract::W5, &[7; 32], wallet_id(-239)).unwrap(),
            contract: TonContract::W5,
            wallet_id: wallet_id(-239),
            seqno: 3,
            valid_until: 1_800_000_000,
            to: Address::Ton {
                workchain: 0,
                hash: [0x5c; 32],
            },
            nano: U256::from(5u8),
            bounce: false,
            token: None,
        };
        let plain = external(&order, &[9; 64], None).unwrap();
        let id = normalized(&plain).unwrap();
        let deploying = external(&order, &[9; 64], Some(&[7; 32])).unwrap();
        assert_eq!(normalized(&deploying), Some(id));
        let message = Message::from_cell(&plain.clone().to_arc()).unwrap();
        let CommonMsgInfo::ExtIn(info) = &message.info else {
            panic!("an external message expected");
        };
        let mut relayed = CellBuilder::new();
        relayed.store_u8(2, 0b10).unwrap();
        relayed.store_u8(2, 0b01).unwrap().store_u16(9, 8).unwrap();
        relayed.store_u8(8, 0xab).unwrap();
        relayed
            .store_address(&TonAddress::from_msg_address(info.dest.clone()).unwrap())
            .unwrap();
        relayed.store_u8(4, 1).unwrap().store_u8(8, 5).unwrap();
        relayed.store_bit(false).unwrap().store_bit(false).unwrap();
        relayed.store_cell(&message.body.value).unwrap();
        assert_eq!(normalized(&relayed.build().unwrap()), Some(id));
        let later = external(&Order { seqno: 4, ..order }, &[9; 64], None).unwrap();
        assert_ne!(normalized(&later), Some(id));
        let body = WalletExtMsgBodyV5::from_cell(&unsigned(&order).unwrap().to_arc()).unwrap();
        assert_eq!(normalized(&body.msgs[0]), None);
        assert_eq!(
            parse(&BagOfCells::from_root(plain).serialize(true).unwrap())
                .unwrap()
                .id,
            id
        );
    }

    #[test]
    fn reads_a_real_signed_v4r2_transfer_byte_for_byte() {
        let body = BagOfCells::parse(&bytes32_vec("b5ee9c7201010201008700019c9dcd3a68926ad6fb9d094c5b72901bfc359ada50f22b648c6c2223c767135d397c7489c121071e45a5316a94a533d80c41450049ebeed406c419fea99117f40629a9a31767ad328900000013000301006842007847b4630eb08d9f486fe846d5496878556dfd5a084f82a9a3fb01224e67c84c200989680000000000000000000000000000"))
            .unwrap()
            .single_root()
            .unwrap();
        let wallet = Address::Ton {
            workchain: 0,
            hash: [0x11; 32],
        };
        let message = Message::new(
            CommonMsgInfo::ExtIn(ExtInMsgInfo {
                src: MsgAddressExt::None(MsgAddrNone {}),
                dest: to_ton(&wallet).unwrap().to_msg_address_int(),
                import_fee: Grams::new(BigUint::default()),
            }),
            body,
        )
        .to_cell()
        .unwrap();
        let parsed = parse(&BagOfCells::from_root(message).serialize(true).unwrap()).unwrap();
        assert_eq!(parsed.order.contract, TonContract::V4R2);
        assert_eq!(
            (
                parsed.order.wallet_id,
                parsed.order.seqno,
                parsed.order.valid_until
            ),
            (V4R2_WALLET_ID, 19, 1_739_403_913)
        );
        assert_eq!(parsed.order.nano, U256::from(20_000_000u32));
        assert_eq!(
            parsed.order.to.to_string(),
            "0:f08f68c61d611b3e90dfd08daa92d0f0aadbfab4109f055347f602449ccf9098"
        );
        assert!(!parsed.order.bounce);
        assert_eq!(parsed.public_key, None);
    }
}
