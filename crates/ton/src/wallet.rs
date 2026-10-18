use bahaa_chain::ChainError;
use bahaa_types::Address;
use tonlib_core::cell::{BagOfCells, CellBuilder};
use tonlib_core::wallet::mnemonic::KeyPair;
use tonlib_core::wallet::ton_wallet::TonWallet;
use tonlib_core::wallet::wallet_version::WalletVersion;
use tonlib_core::{TonAddress, TonHash};

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
