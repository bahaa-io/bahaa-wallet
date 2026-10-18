use bahaa_chain::ChainError;
use bahaa_types::Address;
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
use tonlib_core::wallet::versioned::v5::{WalletDataV5, WalletExtMsgBodyV5};
use tonlib_core::wallet::wallet_version::WalletVersion;
use tonlib_core::{TonAddress, TonHash};

pub(crate) const SEND_MODE: u8 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Order {
    pub wallet: Address,
    pub wallet_id: i32,
    pub seqno: u32,
    pub valid_until: u32,
    pub to: Address,
    pub nano: U256,
    pub bounce: bool,
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

pub(crate) fn v5r1(public_key: &[u8; 32], wallet_id: i32) -> Result<Address, ChainError> {
    let keys = KeyPair {
        public_key: public_key.to_vec(),
        secret_key: Vec::new(),
    };
    let wallet = TonWallet::new_with_params(WalletVersion::V5R1, keys, 0, wallet_id)
        .map_err(|_| ChainError::Invalid("cannot build the wallet address"))?;
    from_ton(&wallet.address).ok_or(ChainError::Invalid("cannot build the wallet address"))
}

pub(crate) fn v5r1_code_hash() -> Option<[u8; 32]> {
    let code = VersionHelper::get_code(WalletVersion::V5R1).ok()?;
    Cell::cell_hash(code).as_slice().try_into().ok()
}

pub(crate) fn wallet_data(data: &ArcCell) -> Option<WalletDataV5> {
    WalletDataV5::from_cell(data).ok()
}

pub(crate) fn unsigned(order: &Order) -> Result<Cell, ChainError> {
    let broken = |_| ChainError::Invalid("cannot build the wallet message");
    let transfer = Message::new(
        CommonMsgInfo::Int(IntMsgInfo {
            ihr_disabled: true,
            bounce: order.bounce,
            bounced: false,
            src: MsgAddrNone {}.into(),
            dest: to_ton(&order.to)?.to_msg_address_int().into(),
            value: CurrencyCollection::new(BigUint::from_bytes_be(&order.nano.to_be_bytes::<32>())),
            ihr_fee: Grams::new(BigUint::default()),
            fwd_fee: Grams::new(BigUint::default()),
            created_lt: 0,
            created_at: 0,
        }),
        CellBuilder::new().build().map_err(broken)?.to_arc(),
    )
    .to_cell()
    .map_err(|_| ChainError::Invalid("cannot build the wallet message"))?
    .to_arc();
    WalletExtMsgBodyV5 {
        wallet_id: order.wallet_id,
        valid_until: order.valid_until,
        msg_seqno: order.seqno,
        msgs_modes: vec![SEND_MODE],
        msgs: vec![transfer],
    }
    .to_cell()
    .map_err(broken)
}

pub(crate) fn transfer_size(order: &Order) -> Result<(u64, u64), ChainError> {
    let body = WalletExtMsgBodyV5::from_cell(&unsigned(order)?.to_arc())
        .map_err(|_| ChainError::Invalid("cannot build the wallet message"))?;
    let [transfer] = body.msgs.as_slice() else {
        return Err(ChainError::Invalid("cannot build the wallet message"));
    };
    Ok(crate::fees::size_without_root(transfer))
}

pub(crate) fn external(
    order: &Order,
    signature: &[u8; 64],
    deploy_key: Option<&[u8; 32]>,
) -> Result<Cell, ChainError> {
    let broken = |_| ChainError::Invalid("cannot build the wallet message");
    let body = VersionHelper::sign_msg(WalletVersion::V5R1, &unsigned(order)?, signature)
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
        let code = VersionHelper::get_code(WalletVersion::V5R1)
            .map_err(broken)?
            .clone();
        let data =
            VersionHelper::get_data(WalletVersion::V5R1, &keys, order.wallet_id).map_err(broken)?;
        message.with_state_init(StateInit::new(code, data.to_arc()));
    }
    message
        .to_cell()
        .map_err(|_| ChainError::Invalid("cannot build the wallet message"))
}

pub(crate) fn parse(boc: &[u8]) -> Result<Parsed, ChainError> {
    let invalid = || ChainError::Invalid("not a signed V5R1 transfer");
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
    let mut parser = body.parser();
    let request = WalletExtMsgBodyV5::read(&mut parser).map_err(|_| invalid())?;
    let signature: [u8; 64] = parser
        .load_bytes(64)
        .map_err(|_| invalid())?
        .try_into()
        .map_err(|_| invalid())?;
    parser.ensure_empty().map_err(|_| invalid())?;
    let ([transfer], [SEND_MODE]) = (request.msgs.as_slice(), request.msgs_modes.as_slice()) else {
        return Err(ChainError::Invalid("not a single plain transfer"));
    };
    let inner = Message::from_cell(transfer).map_err(|_| invalid())?;
    let CommonMsgInfo::Int(sent) = &inner.info else {
        return Err(invalid());
    };
    let plain = inner.init.is_none()
        && inner.body.value.bit_len() == 0
        && inner.body.value.references().is_empty()
        && sent.ihr_disabled
        && !sent.bounced
        && sent.value.other.is_none();
    if !plain {
        return Err(ChainError::Invalid("not a single plain transfer"));
    }
    let to = from_ton(&TonAddress::from_msg_address(sent.dest.clone()).map_err(|_| invalid())?)
        .ok_or_else(invalid)?;
    let nano =
        U256::try_from_be_slice(&sent.value.grams.amount.to_bytes_be()).ok_or_else(invalid)?;
    let order = Order {
        wallet,
        wallet_id: request.wallet_id,
        seqno: request.msg_seqno,
        valid_until: request.valid_until,
        to,
        nano,
        bounce: sent.bounce,
    };
    let unsigned_cell = unsigned(&order)?;
    let resigned = VersionHelper::sign_msg(WalletVersion::V5R1, &unsigned_cell, &signature)
        .map_err(|_| invalid())?;
    if resigned.cell_hash() != Cell::cell_hash(body) {
        return Err(invalid());
    }
    let public_key = match &message.init {
        None => None,
        Some(init) => Some(deployed_key(&init.value, order.wallet_id)?),
    };
    Ok(Parsed {
        order,
        id: hash_of(&root)?,
        unsigned: hash_of(&unsigned_cell.to_arc())?,
        signature,
        public_key,
    })
}

fn deployed_key(init: &StateInit, wallet_id: i32) -> Result<[u8; 32], ChainError> {
    let invalid = || ChainError::Invalid("the deployment is not a V5R1 wallet");
    let code = init.code.as_ref().ok_or_else(invalid)?;
    let data = init.data.as_ref().ok_or_else(invalid)?;
    let plain = init.split_depth.is_none() && init.tick_tock.is_none() && init.library.is_none();
    if !plain || Cell::cell_hash(&code.0).as_slice() != v5r1_code_hash().ok_or_else(invalid)? {
        return Err(invalid());
    }
    let wallet = wallet_data(&data.0).ok_or_else(invalid)?;
    if wallet.seqno != 0
        || wallet.wallet_id != wallet_id
        || !wallet.signature_allowed
        || wallet.extensions.is_some()
    {
        return Err(invalid());
    }
    wallet
        .public_key
        .as_slice()
        .try_into()
        .map_err(|_| invalid())
}

fn hash_of(cell: &ArcCell) -> Result<[u8; 32], ChainError> {
    Cell::cell_hash(cell)
        .as_slice()
        .try_into()
        .map_err(|_| ChainError::Invalid("not a signed V5R1 transfer"))
}

pub(crate) fn address_boc(address: &Address) -> Result<Vec<u8>, ChainError> {
    let broken = |_| ChainError::Invalid("cannot encode the address");
    let mut builder = CellBuilder::new();
    builder.store_address(&to_ton(address)?).map_err(broken)?;
    let cell = builder.build().map_err(broken)?;
    BagOfCells::from_root(cell).serialize(false).map_err(broken)
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
        for (public_key, address) in wallets {
            let built = v5r1(&bytes32(public_key), wallet_id(-239)).unwrap();
            assert_eq!(built.to_string(), address);
        }
        let testnet = v5r1(&bytes32(wallets[0].0), wallet_id(-3)).unwrap();
        assert_ne!(testnet.to_string(), wallets[0].1);
    }

    #[test]
    fn addresses_survive_the_cell_round_trip() {
        let owner = Address::Ton {
            workchain: 0,
            hash: bytes32("f8b8635d0786b77f9da660e65fd31d670a675dd4302facba692f46b571eb5360"),
        };
        let boc = address_boc(&owner).unwrap();
        assert_eq!(read_address(&boc), Some(owner));
        let from_node = bytes32_vec(
            "b5ee9c72010101010024000043801f170c6ba0f0d6eff3b4cc1ccbfa63ace14cebba8605f5974d25e8d6ae3d6a6c10",
        );
        assert_eq!(read_address(&from_node), Some(owner));
        assert_eq!(read_address(&[0xb5, 0xee]), None);
        assert!(matches!(
            address_boc(&Address::Evm([1; 20])),
            Err(ChainError::Invalid(_))
        ));
    }

    fn bytes32_vec(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).unwrap())
            .collect()
    }
}
