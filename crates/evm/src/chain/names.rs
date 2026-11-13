use alloy_primitives::keccak256;
use bahaa_chain::abi::{self, Arg};
use bahaa_chain::{ChainError, now};
use futures_util::future::try_join;
use ruint::aliases::U256;

use super::EvmChain;
use super::ens::{namehash, normalize, selector};

struct SpaceId {
    chain: u64,
    ending: &'static str,
    registry: &'static str,
    registrar: &'static str,
    elsewhere: &'static str,
}

const SPACE_ID: [SpaceId; 2] = [
    SpaceId {
        chain: 56,
        ending: ".bnb",
        registry: "08CEd32a7f3eeC915Ba84415e9C07a7286977956",
        registrar: "E3b1D32e43Ce8d658368e2CBFF95D57Ef39Be8a6",
        elsewhere: "this is a BNB Chain name, it only points to BNB Chain addresses",
    },
    SpaceId {
        chain: 42_161,
        ending: ".arb",
        registry: "4a067EE58e73ac5E4a43722E008DFdf65B2bF348",
        registrar: "5d482d501b369f5ba034dec5c5fb7a50d2d6ca20",
        elsewhere: "this is an Arbitrum name, it only points to Arbitrum addresses",
    },
];
const READERS: [(u64, &str); 3] = [
    (137, "91EDd8708062bd4233f4Dd0FCE15A7cb4d500091"),
    (1, "578853aa776Eef10CeE6c4dd2B5862bdcE767A8B"),
    (8453, "78c4b414e1abdf0de267deda01dffd4cd0817a16"),
];
const ON_POLYGON: &str = "888 agent ai4 altimist anime arculus austin awaken binanceus bitcoin bitget blockchain brave calicoin carbon cashme cgai chipper clay coin collect crypto dao dejay derad dfz digitalfuture doga donut dream dsci emir farms go grow hi hub imtoken kingdom klever kresus kryptic lfg ltc manga marketer metropolis miku ministry mobix moon nft nibi og openx pack pastor pbdx pilot pog polygon presearch pudgy pundi raiin realm secret stepn super supernova tball tea tribe twin ubu unstoppable vanity wallet web3 wifi witg wrkx x xmr xz1 yellow zil";
const ON_BASE: &str = "amped anyone ask ath bald basenji bay bch benji bitscrunch boomer bunni caw chip chomp depin digibyte enigma ethermail goblin gotchi her horizen learn lunar mooncat mumu mycircle npc ohm onchain pendle podcast pokt privacy propykeys quantum rad smobler south spend u udtest verge xec xyo zano";
const POLYGON_THEN_ETHEREUM: [u64; 2] = [137, 1];
const BASE: [u64; 1] = [8453];
const MAX_RECORD: usize = 256;
const INVALID: ChainError = ChainError::Invalid("this is not a valid name");

pub fn space_id_home(name: &str) -> Option<(u64, &'static str)> {
    let lower = name.trim().to_lowercase();
    SPACE_ID
        .iter()
        .find(|system| lower.ends_with(system.ending))
        .map(|system| (system.chain, system.elsewhere))
}

pub fn unstoppable_chains(name: &str) -> Option<&'static [u64]> {
    let lower = name.trim().to_ascii_lowercase();
    let (_, ending) = lower.rsplit_once('.')?;
    let listed = |list: &str| list.split(' ').any(|listed| listed == ending);
    if listed(ON_POLYGON) {
        Some(&POLYGON_THEN_ETHEREUM)
    } else if listed(ON_BASE) {
        Some(&BASE)
    } else {
        None
    }
}

pub fn unstoppable_normalize(name: &str) -> Result<String, ChainError> {
    let lower = name.trim().to_ascii_lowercase();
    let fine = lower.contains('.')
        && lower.split('.').all(|label| {
            !label.is_empty()
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        });
    if fine { Ok(lower) } else { Err(INVALID) }
}

impl EvmChain {
    pub async fn space_id_address(&self, name: &str) -> Result<Option<[u8; 20]>, ChainError> {
        let system = SPACE_ID
            .iter()
            .find(|system| system.chain == self.chain_id)
            .ok_or(ChainError::Unsupported("SPACE ID names"))?;
        let name = normalize(name)?;
        let label = name
            .strip_suffix(system.ending)
            .and_then(|rest| rest.rsplit('.').next())
            .filter(|label| !label.is_empty())
            .ok_or(ChainError::Invalid(system.elsewhere))?;
        let node = namehash(&name);
        let (resolver, expiry) = try_join(
            self.read_with(
                &contract(system.registry)?,
                &call("resolver(bytes32)", node),
                None,
            ),
            self.read_with(
                &contract(system.registrar)?,
                &call("nameExpires(uint256)", keccak256(label).0),
                None,
            ),
        )
        .await?;
        let resolver = abi::address(&resolver, 0).ok_or_else(ChainError::malformed)?;
        let expiry = abi::uint(&expiry, 0).ok_or_else(ChainError::malformed)?;
        if resolver == [0; 20] || expiry <= U256::from(now()) {
            return Ok(None);
        }
        let address = self
            .read_with(&resolver, &call("addr(bytes32)", node), None)
            .await?;
        let address = abi::address(&address, 0).ok_or_else(ChainError::malformed)?;
        Ok((address != [0; 20]).then_some(address))
    }

    pub async fn unstoppable_address(
        &self,
        name: &str,
        network: &str,
        token: &str,
    ) -> Result<Option<String>, ChainError> {
        let (_, reader) = READERS
            .iter()
            .find(|(chain, _)| *chain == self.chain_id)
            .ok_or(ChainError::Unsupported("Unstoppable Domains names"))?;
        let id = namehash(&unstoppable_normalize(name)?);
        let mut lookup = selector("getAddress(string,string,uint256)").to_vec();
        lookup.extend(abi::encode(&[
            Arg::Bytes(network.as_bytes()),
            Arg::Bytes(token.as_bytes()),
            Arg::Uint(U256::from_be_bytes(id)),
        ]));
        let calls = [
            call("ownerOf(uint256)", id),
            call("exists(uint256)", id),
            lookup,
        ];
        let mut data = selector("multicall(bytes[])").to_vec();
        abi::push_uint(&mut data, U256::from(32));
        abi::push_bytes_list(&mut data, &calls);
        let reply = self.read_with(&contract(reader)?, &data, None).await?;
        let (owner, exists, record) = read_unstoppable(&reply).ok_or_else(ChainError::malformed)?;
        if owner == [0; 20] {
            return if exists {
                Err(ChainError::Invalid("this name has expired"))
            } else {
                Ok(None)
            };
        }
        let record = record.trim();
        if record.is_empty() {
            return Err(ChainError::Invalid(
                "this name has no address on this network",
            ));
        }
        Ok(Some(record.to_owned()))
    }
}

fn read_unstoppable(reply: &[u8]) -> Option<([u8; 20], bool, String)> {
    let answers = abi::bytes_list(reply, 0)?;
    let [owner, exists, record] = answers.as_slice() else {
        return None;
    };
    let exists = abi::uint(exists, 0)?;
    Some((
        abi::address(owner, 0)?,
        exists == U256::from(1),
        abi::abi_string(record, MAX_RECORD)?,
    ))
}

fn call(signature: &str, word: [u8; 32]) -> Vec<u8> {
    let mut data = selector(signature).to_vec();
    data.extend_from_slice(&word);
    data
}

fn contract(text: &str) -> Result<[u8; 20], ChainError> {
    bahaa_types::hex::decode_array(text).ok_or_else(ChainError::malformed)
}

#[cfg(fuzzing)]
pub(crate) fn fuzz(data: &[u8]) {
    let _ = read_unstoppable(data);
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = (
            space_id_home(text),
            unstoppable_chains(text),
            unstoppable_normalize(text),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_go_to_the_system_and_chain_that_holds_their_ending() {
        assert_eq!(unstoppable_chains(" Brad.Crypto"), Some(&[137, 1][..]));
        assert_eq!(unstoppable_chains("gurugur.u"), Some(&[8453][..]));
        for other in ["vitalik.eth", "shop.travel", "yatoa.com", "crypto"] {
            assert_eq!(unstoppable_chains(other), None, "{other}");
        }
        assert_eq!(space_id_home("SpaceID.BNB").map(|home| home.0), Some(56));
        assert_eq!(
            space_id_home("spaceid.arb").map(|home| home.0),
            Some(42_161)
        );
        assert_eq!(space_id_home("spaceid.eth"), None);
        assert_eq!(
            unstoppable_normalize(" Brad.Crypto ").unwrap(),
            "brad.crypto"
        );
        for bad in ["brad..crypto", "br\u{e4}d.crypto", "brad", "br ad.crypto"] {
            assert!(unstoppable_normalize(bad).is_err(), "{bad}");
        }
        assert_eq!(
            bahaa_types::hex::encode(&namehash("brad.crypto")),
            "756e4e998dbffd803c21d23b06cd855cdc7a4b57706c95964a37e24b47c10fc9"
        );
    }

    #[test]
    fn the_reader_answer_gives_owner_existence_and_record() {
        let mut owner = vec![0; 12];
        owner.extend([0x8a; 20]);
        let exists = U256::from(1).to_be_bytes::<32>().to_vec();
        let record = abi::encode_bytes(&[b"0x8aaD44321A86b170879d7A244c1e8d360c99DdA8"]);
        let mut reply = abi::encode(&[Arg::Uint(U256::from(32))]);
        abi::push_bytes_list(&mut reply, &[owner.clone(), exists.clone(), record.clone()]);
        assert_eq!(
            read_unstoppable(&reply),
            Some((
                [0x8a; 20],
                true,
                "0x8aaD44321A86b170879d7A244c1e8d360c99DdA8".to_owned()
            ))
        );
        let mut short = abi::encode(&[Arg::Uint(U256::from(32))]);
        abi::push_bytes_list(&mut short, &[owner, exists]);
        assert_eq!(read_unstoppable(&short), None);
    }
}
