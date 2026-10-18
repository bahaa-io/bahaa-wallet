use std::collections::{BTreeMap, BTreeSet};

use bahaa_types::{Address, AssetId, ChainFamily, MAX_DECIMALS, NetworkId, PriceId, is_plain_text};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

const MAX_NAME: usize = 40;
const MAX_SYMBOL: usize = 11;
const MAX_GAP_MS: u32 = 60_000;
const BUILTIN: &str = include_str!("networks.json");

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ConfigError {
    #[error("the network list is not valid JSON: {0}")]
    Json(String),
    #[error("network {network}: {problem}")]
    Network { network: String, problem: String },
    #[error("network id {0} is used twice")]
    DuplicateId(String),
    #[error("EVM chain id {0} is used twice")]
    DuplicateChainId(u64),
    #[error("{0}")]
    Settings(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FeeModel {
    Standard,
    OpStack,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Protocol {
    JsonRpc,
    TronHttp,
    TonLiteservers,
    TonCenter,
    TonApi,
}

impl Protocol {
    pub fn family(self) -> ChainFamily {
        match self {
            Self::JsonRpc => ChainFamily::Evm,
            Self::TronHttp => ChainFamily::Tron,
            Self::TonLiteservers | Self::TonCenter | Self::TonApi => ChainFamily::Ton,
        }
    }

    fn allows(self, feature: Feature) -> bool {
        match feature {
            Feature::Logs => self == Self::JsonRpc,
            Feature::History => matches!(self, Self::TronHttp | Self::TonCenter | Self::TonApi),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Feature {
    Logs,
    History,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Endpoint {
    pub protocol: Protocol,
    pub url: Url,
    #[serde(default)]
    pub features: Vec<Feature>,
    #[serde(default)]
    pub min_gap_ms: u32,
    #[serde(default)]
    pub history_gap_ms: u32,
}

impl Endpoint {
    pub fn has(&self, feature: Feature) -> bool {
        self.features.contains(&feature)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Coin {
    pub symbol: String,
    pub name: String,
    pub decimals: u8,
    #[serde(default)]
    pub price: Option<PriceId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub contract: Address,
    pub coin: Coin,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChainKind {
    Evm {
        chain_id: u64,
        fee: FeeModel,
    },
    Tron {
        genesis: [u8; 32],
    },
    Ton {
        global_id: i32,
        zero_state: [u8; 32],
        checkpoint: Checkpoint,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    pub seqno: u32,
    pub root_hash: [u8; 32],
    pub file_hash: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Explorer {
    pub tx: String,
    pub address: String,
}

impl Explorer {
    pub fn tx_url(&self, tx: &str) -> String {
        self.tx.replace("{tx}", tx)
    }

    pub fn address_url(&self, address: &str) -> String {
        self.address.replace("{address}", address)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Network {
    pub id: NetworkId,
    pub name: String,
    pub kind: ChainKind,
    pub testnet: bool,
    pub native: Coin,
    pub tokens: Vec<Token>,
    pub endpoints: Vec<Endpoint>,
    pub explorer: Explorer,
}

impl Network {
    pub fn family(&self) -> ChainFamily {
        match self.kind {
            ChainKind::Evm { .. } => ChainFamily::Evm,
            ChainKind::Tron { .. } => ChainFamily::Tron,
            ChainKind::Ton { .. } => ChainFamily::Ton,
        }
    }

    pub fn assets(&self) -> impl Iterator<Item = (AssetId, &Coin)> {
        std::iter::once((AssetId::Native(self.id.clone()), &self.native)).chain(
            self.tokens
                .iter()
                .map(|token| (AssetId::Token(self.id.clone(), token.contract), &token.coin)),
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Registry {
    networks: BTreeMap<NetworkId, Network>,
}

impl Registry {
    pub fn builtin() -> Result<Self, ConfigError> {
        Self::from_json(BUILTIN)
    }

    pub fn from_json(text: &str) -> Result<Self, ConfigError> {
        let specs: Vec<Spec> =
            serde_json::from_str(text).map_err(|err| ConfigError::Json(err.to_string()))?;
        let networks = specs
            .into_iter()
            .map(Spec::validate)
            .collect::<Result<Vec<_>, _>>()?;
        Self::with_networks(networks)
    }

    pub fn with_networks(list: Vec<Network>) -> Result<Self, ConfigError> {
        let mut networks = BTreeMap::new();
        let mut chain_ids = BTreeSet::new();
        for network in list {
            if let ChainKind::Evm { chain_id, .. } = network.kind
                && !chain_ids.insert(chain_id)
            {
                return Err(ConfigError::DuplicateChainId(chain_id));
            }
            if networks.contains_key(&network.id) {
                return Err(ConfigError::DuplicateId(network.id.to_string()));
            }
            networks.insert(network.id.clone(), network);
        }
        Ok(Self { networks })
    }

    pub fn get(&self, id: &NetworkId) -> Option<&Network> {
        self.networks.get(id)
    }

    pub fn networks(&self) -> impl Iterator<Item = &Network> {
        self.networks.values()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Spec {
    id: NetworkId,
    name: String,
    family: ChainFamily,
    #[serde(default)]
    chain_id: Option<u64>,
    #[serde(default)]
    fee: Option<FeeModel>,
    #[serde(default)]
    genesis: Option<String>,
    #[serde(default)]
    global_id: Option<i32>,
    #[serde(default)]
    zero_state: Option<String>,
    #[serde(default)]
    checkpoint: Option<CheckpointSpec>,
    testnet: bool,
    native: Coin,
    #[serde(default)]
    tokens: Vec<TokenSpec>,
    endpoints: Vec<Endpoint>,
    explorer: Explorer,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckpointSpec {
    seqno: u32,
    root_hash: String,
    file_hash: String,
}

impl CheckpointSpec {
    fn read(&self) -> Option<Checkpoint> {
        Some(Checkpoint {
            seqno: (self.seqno > 0 && i32::try_from(self.seqno).is_ok()).then_some(self.seqno)?,
            root_hash: block_id(Some(&self.root_hash))?,
            file_hash: block_id(Some(&self.file_hash))?,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenSpec {
    contract: String,
    symbol: String,
    name: String,
    decimals: u8,
    #[serde(default)]
    price: Option<PriceId>,
}

impl Spec {
    fn kind(&self) -> Result<ChainKind, &'static str> {
        let kind = match (self.family, self.chain_id, self.fee) {
            (ChainFamily::Evm, Some(chain_id), Some(fee)) if chain_id > 0 => {
                ChainKind::Evm { chain_id, fee }
            }
            (ChainFamily::Evm, _, _) => {
                return Err("EVM networks need a non-zero chain_id and a fee model");
            }
            (ChainFamily::Tron, None, None) => ChainKind::Tron {
                genesis: block_id(self.genesis.as_deref())
                    .ok_or("Tron networks need a 64 hex digit genesis")?,
            },
            (ChainFamily::Ton, None, None) => ChainKind::Ton {
                global_id: self.global_id.ok_or("TON networks need their global_id")?,
                zero_state: block_id(self.zero_state.as_deref())
                    .ok_or("TON networks need a 64 hex digit zero_state")?,
                checkpoint: self
                    .checkpoint
                    .as_ref()
                    .and_then(CheckpointSpec::read)
                    .ok_or("TON networks need a checkpoint key block with its two hashes")?,
            },
            _ => return Err("only EVM networks have chain_id and fee"),
        };
        let foreign = match self.family {
            ChainFamily::Evm => {
                self.genesis.is_some()
                    || self.global_id.is_some()
                    || self.zero_state.is_some()
                    || self.checkpoint.is_some()
            }
            ChainFamily::Tron => {
                self.global_id.is_some() || self.zero_state.is_some() || self.checkpoint.is_some()
            }
            ChainFamily::Ton => self.genesis.is_some(),
        };
        if foreign {
            return Err("a field belongs to another chain family");
        }
        Ok(kind)
    }

    fn validate(self) -> Result<Network, ConfigError> {
        let problem = |text: &str| ConfigError::Network {
            network: self.id.to_string(),
            problem: text.to_owned(),
        };
        if !is_plain_text(&self.name, MAX_NAME) {
            return Err(problem("name must be 1 to 40 printable characters"));
        }
        let kind = self.kind().map_err(problem)?;
        self.native
            .check()
            .map_err(|text| problem(&format!("native coin {text}")))?;
        if self.endpoints.is_empty() {
            return Err(problem("needs at least one endpoint"));
        }
        let mut urls = BTreeSet::new();
        for endpoint in &self.endpoints {
            if endpoint.protocol.family() != self.family {
                return Err(problem("an endpoint speaks another chain's protocol"));
            }
            if endpoint.url.scheme() != "https" || endpoint.url.host_str().is_none() {
                return Err(problem("endpoints must use https"));
            }
            if endpoint
                .features
                .iter()
                .any(|feature| !endpoint.protocol.allows(*feature))
            {
                return Err(problem(
                    "an endpoint claims a feature its protocol cannot have",
                ));
            }
            if endpoint.min_gap_ms > MAX_GAP_MS {
                return Err(problem("an endpoint's min_gap_ms is over one minute"));
            }
            if endpoint.history_gap_ms > MAX_GAP_MS {
                return Err(problem("an endpoint's history_gap_ms is over one minute"));
            }
            if !urls.insert(endpoint.url.as_str()) {
                return Err(problem("an endpoint is listed twice"));
            }
        }
        check_template(&self.explorer.tx, "{tx}")
            .and_then(|()| check_template(&self.explorer.address, "{address}"))
            .map_err(problem)?;
        let mut contracts = BTreeSet::new();
        let mut tokens = Vec::with_capacity(self.tokens.len());
        for spec in self.tokens {
            let contract = Address::parse(self.family, &spec.contract)
                .map_err(|err| problem(&format!("token {}: {err}", spec.symbol)))?;
            if !contracts.insert(contract) {
                return Err(problem("a token is listed twice"));
            }
            let coin = Coin {
                symbol: spec.symbol,
                name: spec.name,
                decimals: spec.decimals,
                price: spec.price,
            };
            coin.check()
                .map_err(|text| problem(&format!("token {text}")))?;
            tokens.push(Token { contract, coin });
        }
        Ok(Network {
            id: self.id,
            name: self.name,
            kind,
            testnet: self.testnet,
            native: self.native,
            tokens,
            endpoints: self.endpoints,
            explorer: self.explorer,
        })
    }
}

impl Coin {
    pub fn check(&self) -> Result<(), &'static str> {
        let symbol_ok = (1..=MAX_SYMBOL).contains(&self.symbol.len())
            && self.symbol.starts_with(|c: char| c.is_ascii_alphanumeric())
            && self
                .symbol
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.');
        if !symbol_ok {
            return Err("symbol must be 1 to 11 letters, digits or dots");
        }
        if !is_plain_text(&self.name, MAX_NAME) {
            return Err("name must be 1 to 40 printable characters");
        }
        if self.decimals > MAX_DECIMALS {
            return Err("has too many decimals");
        }
        Ok(())
    }
}

fn block_id(text: Option<&str>) -> Option<[u8; 32]> {
    let text = text?;
    if text.len() != 64 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 32];
    for (slot, at) in out.iter_mut().zip((0..text.len()).step_by(2)) {
        *slot = u8::from_str_radix(&text[at..at + 2], 16).ok()?;
    }
    Some(out)
}

fn check_template(template: &str, placeholder: &str) -> Result<(), &'static str> {
    let url = Url::parse(&template.replace(placeholder, "x"))
        .map_err(|_| "explorer links must be valid URLs")?;
    if url.scheme() != "https" || template.matches(placeholder).count() != 1 {
        return Err("explorer links must use https and contain their placeholder once");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    type Edit = Box<dyn Fn(&mut Value)>;

    fn builtin() -> Registry {
        Registry::builtin().unwrap()
    }

    fn network(id: &str) -> Network {
        builtin().get(&id.parse().unwrap()).unwrap().clone()
    }

    fn edited(id: &str, edit: impl Fn(&mut Value)) -> Result<Registry, ConfigError> {
        let mut list: Value = serde_json::from_str(BUILTIN).unwrap();
        let entry = list
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|entry| entry["id"] == id)
            .unwrap();
        edit(entry);
        Registry::from_json(&list.to_string())
    }

    #[test]
    fn builtin_registry_is_valid() {
        let registry = builtin();
        let ids: Vec<&str> = registry
            .networks()
            .map(|network| network.id.as_str())
            .collect();
        assert_eq!(
            ids,
            [
                "arbitrum",
                "avalanche",
                "base",
                "bsc",
                "bsc-testnet",
                "ethereum",
                "optimism",
                "polygon",
                "ton",
                "ton-testnet",
                "tron",
                "tron-nile"
            ]
        );
    }

    #[test]
    fn evm_networks_have_their_real_chain_ids_and_fee_models() {
        let expected = [
            ("bsc", 56, FeeModel::Standard),
            ("ethereum", 1, FeeModel::Standard),
            ("polygon", 137, FeeModel::Standard),
            ("arbitrum", 42161, FeeModel::Standard),
            ("avalanche", 43114, FeeModel::Standard),
            ("base", 8453, FeeModel::OpStack),
            ("optimism", 10, FeeModel::OpStack),
            ("bsc-testnet", 97, FeeModel::Standard),
        ];
        for (id, chain_id, fee) in expected {
            assert_eq!(network(id).kind, ChainKind::Evm { chain_id, fee }, "{id}");
        }
        for (id, start, end) in [("tron", 0x1e, 0xdc), ("tron-nile", 0xd6, 0xdc)] {
            let ChainKind::Tron { genesis } = network(id).kind else {
                panic!("{id} is not a Tron network");
            };
            assert_eq!(genesis[..8], [0; 8]);
            assert_eq!((genesis[8], genesis[31]), (start, end));
        }
        for (id, global_id, first) in [("ton", -239, 0x17), ("ton-testnet", -3, 0x82)] {
            let ChainKind::Ton {
                global_id: found,
                zero_state,
                ..
            } = network(id).kind
            else {
                panic!("{id} is not a TON network");
            };
            assert_eq!((found, zero_state[0]), (global_id, first));
        }
    }

    #[test]
    fn mainnet_coins_are_priced_and_testnet_coins_are_not() {
        for network in builtin().networks() {
            for (asset, coin) in network.assets() {
                assert_eq!(coin.price.is_some(), !network.testnet, "{asset}");
            }
        }
        assert_eq!(network("ton").native.symbol, "GRAM");
        assert_eq!(
            network("polygon").native.price.unwrap().to_string(),
            "crypto/POLYG"
        );
    }

    #[test]
    fn assets_are_keyed_by_network_and_contract() {
        let bsc: Vec<String> = network("bsc")
            .assets()
            .map(|(asset, _)| asset.to_string())
            .collect();
        assert_eq!(
            bsc,
            [
                "bsc",
                "bsc/0x55d398326f99059fF775485246999027B3197955",
                "bsc/0x8AC76a51cc950d9822D68b83fE1Ad97B32Cd580d"
            ]
        );
        let ton = network("ton");
        assert_eq!(ton.tokens[0].contract.family(), ChainFamily::Ton);
    }

    #[test]
    fn endpoints_follow_the_phase_zero_order() {
        let bsc = network("bsc");
        assert_eq!(
            bsc.endpoints[0].url.as_str(),
            "https://bsc-rpc.publicnode.com/"
        );
        assert!(bsc.endpoints[0].has(Feature::Logs));
        assert!(!bsc.endpoints[1].has(Feature::Logs));
        let tron = network("tron");
        assert!(tron.endpoints[0].has(Feature::History));
        assert!(!tron.endpoints[1].has(Feature::History));
        assert_eq!(tron.endpoints[0].min_gap_ms, 400);
        assert_eq!(tron.endpoints[0].history_gap_ms, 1000);
        assert_eq!(network("ton").endpoints[1].min_gap_ms, 1100);
        assert_eq!(bsc.endpoints[0].min_gap_ms, 0);
    }

    #[test]
    fn explorer_links_fill_their_placeholders() {
        let tron = network("tron");
        assert_eq!(
            tron.explorer.tx_url("abc"),
            "https://tronscan.org/#/transaction/abc"
        );
        assert_eq!(
            network("ton").explorer.address_url("EQx"),
            "https://tonviewer.com/EQx"
        );
    }

    #[test]
    fn rejects_inconsistent_networks() {
        let cases: Vec<(&str, Edit)> = vec![
            ("bsc", Box::new(|n| n["chain_id"] = Value::Null)),
            ("tron", Box::new(|n| n["chain_id"] = 1.into())),
            ("tron", Box::new(|n| n["genesis"] = Value::Null)),
            ("tron", Box::new(|n| n["genesis"] = "00ff".into())),
            (
                "tron",
                Box::new(|n| n["genesis"] = format!("+f{}", "0".repeat(62)).into()),
            ),
            ("bsc", Box::new(|n| n["genesis"] = "0".repeat(64).into())),
            ("ton", Box::new(|n| n["global_id"] = Value::Null)),
            ("ton", Box::new(|n| n["zero_state"] = "17a3".into())),
            ("ton", Box::new(|n| n["genesis"] = "0".repeat(64).into())),
            ("ton", Box::new(|n| n["checkpoint"] = Value::Null)),
            ("ton", Box::new(|n| n["checkpoint"]["seqno"] = 0.into())),
            (
                "ton",
                Box::new(|n| n["checkpoint"]["seqno"] = 3_000_000_000_u64.into()),
            ),
            (
                "ton",
                Box::new(|n| n["checkpoint"]["root_hash"] = "17a3".into()),
            ),
            (
                "ton",
                Box::new(|n| n["checkpoint"]["file_hash"] = Value::Null),
            ),
            ("ton", Box::new(|n| n["checkpoint"]["lt"] = 1.into())),
            (
                "tron",
                Box::new(|n| n["checkpoint"] = serde_json::json!({"seqno": 1})),
            ),
            ("tron", Box::new(|n| n["global_id"] = (-239).into())),
            (
                "bsc",
                Box::new(|n| n["endpoints"][0]["protocol"] = "tron-http".into()),
            ),
            (
                "bsc",
                Box::new(|n| n["endpoints"][0]["url"] = "http://bsc-rpc.publicnode.com".into()),
            ),
            (
                "bsc",
                Box::new(|n| n["endpoints"][1] = n["endpoints"][0].clone()),
            ),
            (
                "tron",
                Box::new(|n| n["endpoints"][0]["features"] = serde_json::json!(["logs"])),
            ),
            ("bsc", Box::new(|n| n["endpoints"] = serde_json::json!([]))),
            (
                "bsc",
                Box::new(|n| {
                    n["tokens"][0]["contract"] =
                        "0x55d398326f99059fF775485246999027B3197956".into();
                }),
            ),
            ("bsc", Box::new(|n| n["tokens"][1] = n["tokens"][0].clone())),
            (
                "tron",
                Box::new(|n| {
                    n["tokens"][0]["contract"] =
                        "0x55d398326f99059fF775485246999027B3197955".into();
                }),
            ),
            (
                "bsc",
                Box::new(|n| n["explorer"]["tx"] = "https://bscscan.com/tx/".into()),
            ),
            ("bsc", Box::new(|n| n["name"] = "BNB\u{202E}Chain".into())),
            ("bsc", Box::new(|n| n["native"]["symbol"] = "B N B".into())),
            ("bsc", Box::new(|n| n["native"]["decimals"] = 78.into())),
            ("bsc", Box::new(|n| n["chainid"] = 56.into())),
            (
                "bsc",
                Box::new(|n| n["endpoints"][0]["min_gap_ms"] = 60_001.into()),
            ),
            (
                "bsc",
                Box::new(|n| n["endpoints"][0]["history_gap_ms"] = 60_001.into()),
            ),
        ];
        for (index, (id, edit)) in cases.into_iter().enumerate() {
            assert!(edited(id, edit).is_err(), "case {index} was accepted");
        }
    }

    #[test]
    fn rejects_duplicate_ids_and_chain_ids() {
        let mut list: Value = serde_json::from_str(BUILTIN).unwrap();
        let copy = list[0].clone();
        list.as_array_mut().unwrap().push(copy);
        assert!(matches!(
            Registry::from_json(&list.to_string()),
            Err(ConfigError::DuplicateChainId(56))
        ));
        let mut list: Value = serde_json::from_str(BUILTIN).unwrap();
        let mut copy = list[7].clone();
        copy["id"] = "tron-copy".into();
        list.as_array_mut().unwrap().push(copy);
        assert!(Registry::from_json(&list.to_string()).is_ok());
        let mut list: Value = serde_json::from_str(BUILTIN).unwrap();
        let copy = list[7].clone();
        list.as_array_mut().unwrap().push(copy);
        assert!(matches!(
            Registry::from_json(&list.to_string()),
            Err(ConfigError::DuplicateId(_))
        ));
    }
}
