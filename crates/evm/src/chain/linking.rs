use alloy_primitives::keccak256;
use bahaa_chain::abi::{self, Arg};
use bahaa_chain::{Call, ChainError, FeeQuote, Link, NameRecord, PlannedCall, Step, enough, now};
use bahaa_config::{Feature, Network};
use bahaa_types::{Address, ChainFamily};
use futures_util::future::{try_join, try_join3};
use ruint::aliases::U256;
use serde_json::json;

use super::calls::{Gas, Pricing, call_json, simulated};
use super::ens::{MAINNET, UNIVERSAL, dns_encoded, namehash, normalize, selector};
use super::names::{
    ETHEREUM_COIN, EVM_COIN, L2_REVERSE, READERS, call, contract, ens_coins, for_address,
    multicall, reverse_node, space_id_home, unstoppable_chains, unstoppable_codes,
    unstoppable_normalize,
};
use super::{EvmCall, EvmChain, bytes, hex};

const REGISTRY: &str = "00000000000C2E074eC69A0dFb2997BA6C7d2e1e";
const WRAPPER: &str = "D4416b13d2b3a9aBae7AcD5D6C2BbDBE25686401";
const V2_REGISTRY: &str = "f60aba43e1f7f3dab1186ff03b561401c9df85e2";
const ETH_REGISTRAR: &str = "57f1887a8BF19b14fC0dF6Fd9B2acc9Af147eA85";
const REVERSE: &str = "a58E81fe9b61B5c3fE2AFD33CF304c454AbFc7Cb";
const DEFAULT_REVERSE: &str = "283F227c4Bd38ecE252C4Ae7ECE650B0e913f1f9";
const BASE_REGISTRY: &str = "b94704422c2a1e396835a571837aa5ae53285a95";
const BASE_REGISTRAR: &str = "03c4738ee98ae44591e1a4a4f3cab6641d95dd9a";
const ETH_ENDING: &str = ".eth";
const BASE_ENDING: &str = ".base.eth";
const BASE: u64 = 8453;
const REGISTERED_IN_V2: u8 = 2;
const GENERIC_KEY: &str = "token.EVM.address";
const MAX_NAME: usize = 255;
const MAX_VALUE: usize = 256;
const MAX_KEYS: usize = 64;
const NOT_REGISTERED: &str = "this name isn't registered";
const REFUSED: &str = "the name's contract refused this change";

#[derive(Clone, Debug)]
pub struct EvmLinking {
    pub record: NameRecord,
    pub link: Link,
    pub calls: Vec<EvmCall>,
    pub fee: FeeQuote,
}

struct Named {
    record: NameRecord,
    keeper: Keeper,
}

enum Keeper {
    Ens(Records),
    SpaceId {
        registry: [u8; 20],
        resolver: [u8; 20],
        node: [u8; 32],
        reverse: [u8; 20],
    },
    Unstoppable(Entries),
}

struct Records {
    base: bool,
    home: u64,
    v2: bool,
    resolver: [u8; 20],
    node: [u8; 32],
    dns: Vec<u8>,
    coins: Vec<u64>,
    values: Vec<Vec<u8>>,
    checked: Vec<u64>,
}

struct Entries {
    registry: [u8; 20],
    token: U256,
    labels: Vec<String>,
    lists: Vec<Vec<String>>,
    keys: Vec<String>,
    values: Vec<String>,
}

struct Holder {
    owner: [u8; 20],
    exists: bool,
    registry: [u8; 20],
    lists: Vec<Vec<String>>,
}

struct Change {
    writes: Vec<([u8; 20], Vec<u8>)>,
    reads: Vec<([u8; 20], Vec<u8>)>,
    expect: Expect,
}

enum Expect {
    Records {
        base: bool,
        coins: Vec<u64>,
        checked: Vec<u64>,
        want: Option<[u8; 20]>,
    },
    Address(Option<[u8; 20]>),
    Keys {
        lists: Vec<Vec<String>>,
        keys: Vec<String>,
        want: Option<[u8; 20]>,
    },
    Name(String),
    Reverse {
        resolver: [u8; 20],
        name: String,
    },
}

impl EvmChain {
    pub async fn find_name(
        &self,
        name: &str,
        covered: &[Network],
    ) -> Result<Option<NameRecord>, ChainError> {
        Ok(self.named(name, covered).await?.map(|named| named.record))
    }

    pub async fn prepare_link(
        &self,
        account: u32,
        from: &Address,
        name: &str,
        link: Link,
        covered: &[Network],
    ) -> Result<EvmLinking, ChainError> {
        let sender = bytes(from)?;
        let named = self
            .named(name, covered)
            .await?
            .ok_or(ChainError::Invalid(NOT_REGISTERED))?;
        if named.record.expires_at.is_some_and(|at| at <= now()) {
            return Err(ChainError::Invalid("this name has expired"));
        }
        let change = self.change(&named, link, &sender).await?;
        let (gas, outputs) = self.rehearse(&sender, &change).await?;
        if !change.expect.met(&outputs) {
            return Err(ChainError::Rejected(
                "the name wouldn't point where asked after this change".to_owned(),
            ));
        }
        let nothing = self.native(U256::ZERO)?;
        let steps = change
            .writes
            .into_iter()
            .map(|(to, data)| PlannedCall {
                step: Step::Name {
                    name: named.record.name.clone(),
                },
                account,
                from: *from,
                call: Call {
                    to: Address::Evm(to),
                    value: nothing,
                    data,
                },
            })
            .collect();
        let (native, nonce, market) = try_join3(
            self.native_balance(&sender),
            self.nonce(&sender),
            self.fees(),
        )
        .await?;
        let (base, tip) = market.normal();
        let (calls, fees) = self
            .finish(steps, &gas, nonce, true, &Pricing::new(base, tip)?)
            .await?;
        let coin = &self.network.native;
        enough(&coin.symbol, fees.max, native, coin.decimals)?;
        Ok(EvmLinking {
            record: named.record,
            link,
            calls,
            fee: self.fee_quote(fees)?,
        })
    }

    async fn named(&self, name: &str, covered: &[Network]) -> Result<Option<Named>, ChainError> {
        if let Some(homes) = unstoppable_chains(name) {
            if !homes.contains(&self.chain_id) {
                return Err(ChainError::Unsupported("names kept on another network"));
            }
            return self.unstoppable_record(name, covered).await;
        }
        if let Some((home, elsewhere)) = space_id_home(name) {
            if home != self.chain_id {
                return Err(ChainError::Invalid(elsewhere));
            }
            return self.space_id_record(name).await;
        }
        let name = normalize(name)?;
        match (self.chain_id, name.ends_with(BASE_ENDING)) {
            (MAINNET, false) | (BASE, true) => self.ens_record(name, covered).await,
            _ => Err(ChainError::Unsupported("names kept on another network")),
        }
    }

    async fn ens_record(
        &self,
        name: String,
        covered: &[Network],
    ) -> Result<Option<Named>, ChainError> {
        let base = self.chain_id == BASE;
        let node = namehash(&name);
        let label = name
            .strip_suffix(if base { BASE_ENDING } else { ETH_ENDING })
            .filter(|label| !label.contains('.'))
            .map(str::to_owned);
        let (owner, resolver, expires_at, v2) = if base {
            let (owner, resolver, expiry) = self
                .v1_state((BASE_REGISTRY, BASE_REGISTRAR), node, label.as_deref())
                .await?;
            (owner, resolver, expiry, false)
        } else {
            self.ethereum_state(node, label.as_deref()).await?
        };
        if owner == [0; 20] {
            return Ok(None);
        }
        let home = if base { EVM_COIN | BASE } else { ETHEREUM_COIN };
        let mut checked = vec![ETHEREUM_COIN, home];
        let mut coins = vec![ETHEREUM_COIN, EVM_COIN, home];
        for coin in covered
            .iter()
            .filter(|network| network.family() == ChainFamily::Evm)
            .filter_map(|network| ens_coins(network)?.first().copied())
        {
            push_new(&mut checked, coin);
            push_new(&mut coins, coin);
        }
        let mut records = Records {
            base,
            home,
            v2,
            resolver,
            node,
            dns: dns_encoded(&name)?,
            values: vec![Vec::new(); coins.len()],
            coins,
            checked,
        };
        if resolver != [0; 20] {
            let (to, data) = records.read();
            let reply = if base {
                Some(self.read_with(&to, &data, None).await?)
            } else {
                self.read_offchain(&to, data).await?
            };
            if let Some(reply) = reply {
                records.values = read_records(base, &reply, records.coins.len())
                    .ok_or_else(ChainError::malformed)?;
            }
        }
        let wallet = effective(&records.coins, &records.values, home);
        let elsewhere = records
            .checked
            .iter()
            .any(|coin| effective(&records.coins, &records.values, *coin) != wallet);
        Ok(Some(Named {
            record: NameRecord {
                name,
                network: self.network.id.clone(),
                owner: Address::Evm(owner),
                wallet: wallet.map(Address::Evm),
                elsewhere,
                expires_at,
                renewable: false,
                reversible: true,
            },
            keeper: Keeper::Ens(records),
        }))
    }

    async fn ethereum_state(
        &self,
        node: [u8; 32],
        label: Option<&str>,
    ) -> Result<([u8; 20], [u8; 20], Option<u64>, bool), ChainError> {
        let v2 = contract(V2_REGISTRY)?;
        let state = async {
            match label {
                Some(label) => self
                    .read_with(&v2, &call("getState(uint256)", keccak256(label).0), None)
                    .await
                    .map(Some),
                None => Ok(None),
            }
        };
        let (v1, state) =
            try_join(self.v1_state((REGISTRY, ETH_REGISTRAR), node, label), state).await?;
        let registered = state
            .as_deref()
            .and_then(|state| abi::uint(state, 0))
            .is_some_and(|status| status == U256::from(REGISTERED_IN_V2));
        if let (true, Some(state), Some(label)) = (registered, state, label) {
            let owner = abi::address(&state, 2).ok_or_else(ChainError::malformed)?;
            let expiry = abi::uint(&state, 1)
                .and_then(|at| u64::try_from(at).ok())
                .ok_or_else(ChainError::malformed)?;
            let mut asked = selector("getResolver(string)").to_vec();
            asked.extend(abi::encode(&[Arg::Bytes(label.as_bytes())]));
            let resolver = self.read_with(&v2, &asked, None).await?;
            let resolver = abi::address(&resolver, 0).ok_or_else(ChainError::malformed)?;
            return Ok((owner, resolver, Some(expiry), true));
        }
        let (mut owner, resolver, expiry) = v1;
        if owner == contract(WRAPPER)? {
            let holder = self
                .read_with(&owner, &call("ownerOf(uint256)", node), None)
                .await?;
            owner = abi::address(&holder, 0).ok_or_else(ChainError::malformed)?;
        }
        Ok((owner, resolver, expiry, false))
    }

    async fn v1_state(
        &self,
        (registry, registrar): (&str, &str),
        node: [u8; 32],
        label: Option<&str>,
    ) -> Result<([u8; 20], [u8; 20], Option<u64>), ChainError> {
        let registry = contract(registry)?;
        let registrar = contract(registrar)?;
        let expiry = async {
            match label {
                Some(label) => self
                    .read_with(
                        &registrar,
                        &call("nameExpires(uint256)", keccak256(label).0),
                        None,
                    )
                    .await
                    .map(Some),
                None => Ok(None),
            }
        };
        let (owner, resolver, expiry) = try_join3(
            self.read_with(&registry, &call("owner(bytes32)", node), None),
            self.read_with(&registry, &call("resolver(bytes32)", node), None),
            expiry,
        )
        .await?;
        let word = |reply: &[u8]| abi::address(reply, 0).ok_or_else(ChainError::malformed);
        let expiry = expiry
            .map(|reply| {
                abi::uint(&reply, 0)
                    .and_then(|at| u64::try_from(at).ok())
                    .ok_or_else(ChainError::malformed)
            })
            .transpose()?
            .filter(|at| *at > 0);
        Ok((word(&owner)?, word(&resolver)?, expiry))
    }

    async fn space_id_record(&self, name: &str) -> Result<Option<Named>, ChainError> {
        let found = self.space_id_name(name).await?;
        if found.owner == [0; 20] {
            return Ok(None);
        }
        let expires_at = u64::try_from(found.expiry).map_err(|_| ChainError::malformed())?;
        Ok(Some(Named {
            record: NameRecord {
                name: normalize(name)?,
                network: self.network.id.clone(),
                owner: Address::Evm(found.owner),
                wallet: found.address.map(Address::Evm),
                elsewhere: false,
                expires_at: Some(expires_at),
                renewable: false,
                reversible: true,
            },
            keeper: Keeper::SpaceId {
                registry: found.system.registry()?,
                resolver: found.resolver,
                node: found.node,
                reverse: contract(found.system.reverse)?,
            },
        }))
    }

    async fn unstoppable_record(
        &self,
        name: &str,
        covered: &[Network],
    ) -> Result<Option<Named>, ChainError> {
        let (_, reader) = READERS
            .iter()
            .find(|(chain, _)| *chain == self.chain_id)
            .ok_or(ChainError::Unsupported("Unstoppable Domains names"))?;
        let name = unstoppable_normalize(name)?;
        let token = namehash(&name);
        let mut codes = vec![("ETH", "ETH")];
        for code in covered
            .iter()
            .filter(|network| network.family() == ChainFamily::Evm)
            .filter_map(unstoppable_codes)
        {
            if !codes.contains(&code) {
                codes.push(code);
            }
        }
        let mut calls = vec![
            call("ownerOf(uint256)", token),
            call("exists(uint256)", token),
            call("registryOf(uint256)", token),
        ];
        calls.extend(codes.iter().map(|(network, coin)| {
            let mut data = selector("getAddressKeys(string,string)").to_vec();
            data.extend(abi::encode(&[
                Arg::Bytes(network.as_bytes()),
                Arg::Bytes(coin.as_bytes()),
            ]));
            data
        }));
        let reply = self
            .read_with(&contract(reader)?, &multicall(&calls), None)
            .await?;
        let Holder {
            owner,
            exists,
            registry,
            lists,
        } = read_holder(&reply, calls.len()).ok_or_else(ChainError::malformed)?;
        if owner == [0; 20] {
            return if exists {
                Err(ChainError::Invalid("this name has expired"))
            } else {
                Ok(None)
            };
        }
        let mut keys: Vec<String> = Vec::new();
        for key in lists.iter().flatten() {
            if !keys.contains(key) {
                keys.push(key.clone());
            }
        }
        if keys.len() > MAX_KEYS || !keys.iter().any(|key| key == GENERIC_KEY) {
            return Err(ChainError::malformed());
        }
        let token = U256::from_be_bytes(token);
        let values = strings(
            &self
                .read_with(&registry, &get_many(&keys, token), None)
                .await?,
        )
        .filter(|values| values.len() == keys.len())
        .ok_or_else(ChainError::malformed)?;
        let wallet = first_set(&lists[0], &keys, &values);
        let elsewhere = lists
            .iter()
            .any(|list| first_set(list, &keys, &values) != wallet);
        let labels = name.split('.').map(str::to_owned).collect();
        Ok(Some(Named {
            record: NameRecord {
                name,
                network: self.network.id.clone(),
                owner: Address::Evm(owner),
                wallet: wallet.map(Address::Evm),
                elsewhere,
                expires_at: None,
                renewable: false,
                reversible: true,
            },
            keeper: Keeper::Unstoppable(Entries {
                registry,
                token,
                labels,
                lists,
                keys,
                values,
            }),
        }))
    }

    async fn change(
        &self,
        named: &Named,
        link: Link,
        sender: &[u8; 20],
    ) -> Result<Change, ChainError> {
        let record = &named.record;
        let owns = record.owner == Address::Evm(*sender);
        let target = match link {
            Link::Renew => {
                return Err(ChainError::Unsupported(
                    "renewing this name here, renew it where it was registered",
                ));
            }
            Link::To(_) | Link::Clear if !owns => {
                return Err(ChainError::Invalid("this name belongs to another address"));
            }
            Link::Primary if record.wallet != Some(Address::Evm(*sender)) => {
                return Err(ChainError::Invalid(
                    "point the name to this account before making it the account's name",
                ));
            }
            Link::To(to) => Some(bytes(&to)?),
            Link::Clear | Link::Primary => None,
        };
        let name = &record.name;
        match (&named.keeper, link) {
            (Keeper::Ens(records), Link::Primary) => ens_primary(records.base, name, sender),
            (Keeper::Ens(records), _) => records.change(target),
            (
                Keeper::SpaceId {
                    registry, reverse, ..
                },
                Link::Primary,
            ) => {
                let resolver = self
                    .read_with(reverse, &selector("defaultResolver()"), None)
                    .await?;
                let resolver = abi::address(&resolver, 0).ok_or_else(ChainError::malformed)?;
                Ok(space_id_primary(
                    (*registry, *reverse, resolver),
                    name,
                    sender,
                ))
            }
            (Keeper::SpaceId { resolver, node, .. }, _) => {
                space_id_change(*resolver, *node, target, record.wallet.is_some())
            }
            (Keeper::Unstoppable(_), Link::Primary) if !owns => Err(ChainError::Invalid(
                "only the account that holds this name can make it its name",
            )),
            (Keeper::Unstoppable(keys), Link::Primary) => Ok(keys.primary(name, sender)),
            (Keeper::Unstoppable(keys), _) => keys.change(target),
        }
    }

    async fn rehearse(
        &self,
        from: &[u8; 20],
        change: &Change,
    ) -> Result<(Vec<Gas>, Vec<Vec<u8>>), ChainError> {
        let nothing = self.native(U256::ZERO)?;
        let mut calls = change
            .writes
            .iter()
            .map(|(to, data)| {
                call_json(
                    from,
                    &Call {
                        to: Address::Evm(*to),
                        value: nothing,
                        data: data.clone(),
                    },
                )
            })
            .collect::<Result<Vec<_>, ChainError>>()?;
        calls.extend(
            change
                .reads
                .iter()
                .map(|(to, data)| json!({"from": hex(from), "to": hex(to), "data": hex(data)})),
        );
        let count = calls.len();
        let params =
            json!([{"blockStateCalls": [{"calls": calls}], "validation": false}, "latest"]);
        let reply = self
            .rpc
            .call_offered(Feature::Simulate, "eth_simulateV1", params)
            .await?
            .ok_or(ChainError::Unsupported(
                "checking this change before it is signed",
            ))?;
        let ran = simulated(&reply, count)?;
        let (writes, reads) = ran.split_at(change.writes.len());
        if writes.iter().any(|call| !call.ok) {
            return Err(ChainError::Rejected(REFUSED.to_owned()));
        }
        if reads.iter().any(|call| !call.ok) {
            return Err(ChainError::Rejected(
                "the name can't be read back after this change".to_owned(),
            ));
        }
        let gas = writes
            .iter()
            .map(|call| Gas {
                used: call.used,
                limit: call.peak.saturating_add(call.peak / 4),
            })
            .collect();
        Ok((gas, reads.iter().map(|call| call.output.clone()).collect()))
    }
}

impl Records {
    fn read(&self) -> ([u8; 20], Vec<u8>) {
        let reads: Vec<Vec<u8>> = self
            .coins
            .iter()
            .map(|coin| {
                let mut data = selector("addr(bytes32,uint256)").to_vec();
                data.extend_from_slice(&self.node);
                abi::push_uint(&mut data, U256::from(*coin));
                data
            })
            .collect();
        let batch = multicall(&reads);
        if self.base {
            return (self.resolver, batch);
        }
        let mut data = selector("resolve(bytes,bytes)").to_vec();
        data.extend(abi::encode_bytes(&[&self.dns, &batch]));
        (UNIVERSAL, data)
    }

    fn change(&self, target: Option<[u8; 20]>) -> Result<Change, ChainError> {
        if self.resolver == [0; 20] {
            return Err(ChainError::Unsupported(
                "names without a resolver yet, set one where the name is managed",
            ));
        }
        let mut written: Vec<u64> = Vec::new();
        if target.is_some() {
            for coin in [ETHEREUM_COIN, EVM_COIN, self.home] {
                push_new(&mut written, coin);
            }
        }
        for (coin, value) in self.coins.iter().zip(&self.values) {
            if filled(value) {
                push_new(&mut written, *coin);
            }
        }
        if let Some(to) = target
            && self
                .checked
                .iter()
                .all(|coin| effective(&self.coins, &self.values, *coin) == Some(to))
        {
            return Err(ChainError::Invalid(
                "the name already points to this address",
            ));
        }
        if written.is_empty() {
            return Err(ChainError::Invalid("this name isn't linked to an address"));
        }
        let sets: Vec<Vec<u8>> = written.iter().map(|coin| self.set(*coin, target)).collect();
        Ok(Change {
            writes: vec![(self.resolver, multicall(&sets))],
            reads: vec![self.read()],
            expect: Expect::Records {
                base: self.base,
                coins: self.coins.clone(),
                checked: self.checked.clone(),
                want: target,
            },
        })
    }

    fn set(&self, coin: u64, target: Option<[u8; 20]>) -> Vec<u8> {
        let value = target.as_ref().map_or(&[][..], |to| to.as_slice());
        let node = Arg::Uint(U256::from_be_bytes(self.node));
        let coin = U256::from(coin);
        let (signature, args) = match (self.v2, target) {
            (true, _) => (
                "setAddress(bytes,uint256,bytes)",
                vec![Arg::Bytes(&self.dns), Arg::Uint(coin), Arg::Bytes(value)],
            ),
            (false, None) if coin == U256::from(ETHEREUM_COIN) => (
                "setAddr(bytes32,address)",
                vec![node, Arg::Uint(U256::ZERO)],
            ),
            (false, _) => (
                "setAddr(bytes32,uint256,bytes)",
                vec![node, Arg::Uint(coin), Arg::Bytes(value)],
            ),
        };
        let mut data = selector(signature).to_vec();
        data.extend(abi::encode(&args));
        data
    }
}

impl Entries {
    fn change(&self, target: Option<[u8; 20]>) -> Result<Change, ChainError> {
        let mut written: Vec<String> = Vec::new();
        if target.is_some() {
            written.push(GENERIC_KEY.to_owned());
        }
        for (key, value) in self.keys.iter().zip(&self.values) {
            if !value.trim().is_empty() && !written.contains(key) {
                written.push(key.clone());
            }
        }
        if let Some(to) = target
            && self
                .lists
                .iter()
                .all(|list| first_set(list, &self.keys, &self.values) == Some(to))
        {
            return Err(ChainError::Invalid(
                "the name already points to this address",
            ));
        }
        if written.is_empty() {
            return Err(ChainError::Invalid("this name isn't linked to an address"));
        }
        let text = target.map_or_else(String::new, |to| Address::Evm(to).to_string());
        let keys: Vec<Vec<u8>> = written.iter().map(|key| key.as_bytes().to_vec()).collect();
        let values = vec![text.into_bytes(); keys.len()];
        let mut data = selector("setMany(string[],string[],uint256)").to_vec();
        data.extend(abi::encode(&[
            Arg::List(&keys),
            Arg::List(&values),
            Arg::Uint(self.token),
        ]));
        Ok(Change {
            writes: vec![(self.registry, data)],
            reads: vec![(self.registry, get_many(&self.keys, self.token))],
            expect: Expect::Keys {
                lists: self.lists.clone(),
                keys: self.keys.clone(),
                want: target,
            },
        })
    }

    fn primary(&self, name: &str, sender: &[u8; 20]) -> Change {
        let labels: Vec<Vec<u8>> = self
            .labels
            .iter()
            .map(|label| label.as_bytes().to_vec())
            .collect();
        let mut data = selector("setReverse(string[])").to_vec();
        data.extend(abi::encode(&[Arg::List(&labels)]));
        Change {
            writes: vec![(self.registry, data)],
            reads: vec![(self.registry, for_address("reverseNameOf(address)", sender))],
            expect: Expect::Name(name.to_owned()),
        }
    }
}

impl Expect {
    fn met(&self, outputs: &[Vec<u8>]) -> bool {
        match (self, outputs) {
            (
                Self::Records {
                    base,
                    coins,
                    checked,
                    want,
                },
                [output],
            ) => read_records(*base, output, coins.len()).is_some_and(|values| {
                checked
                    .iter()
                    .all(|coin| effective(coins, &values, *coin) == *want)
            }),
            (Self::Address(want), [output]) => abi::address(output, 0)
                .is_some_and(|found| Some(found).filter(|found| *found != [0; 20]) == *want),
            (Self::Keys { lists, keys, want }, [output]) => strings(output)
                .filter(|values| values.len() == keys.len())
                .is_some_and(|values| {
                    lists
                        .iter()
                        .all(|list| first_set(list, keys, &values) == *want)
                }),
            (Self::Name(name), [_, ..]) => outputs
                .iter()
                .all(|output| abi::abi_string(output, MAX_NAME).as_deref() == Some(name)),
            (Self::Reverse { resolver, name }, [pointed, named]) => {
                abi::address(pointed, 0) == Some(*resolver)
                    && abi::abi_string(named, MAX_NAME).as_deref() == Some(name)
            }
            _ => false,
        }
    }
}

fn ens_primary(base: bool, name: &str, sender: &[u8; 20]) -> Result<Change, ChainError> {
    let named = for_address("nameForAddr(address)", sender);
    let (writes, reads) = if base {
        let registrar = contract(L2_REVERSE)?;
        (vec![(registrar, set_name(name))], vec![(registrar, named)])
    } else {
        let default = contract(DEFAULT_REVERSE)?;
        let mut reverse = selector("reverse(bytes,uint256)").to_vec();
        reverse.extend(abi::encode(&[
            Arg::Bytes(sender),
            Arg::Uint(U256::from(ETHEREUM_COIN)),
        ]));
        (
            vec![
                (contract(REVERSE)?, set_name(name)),
                (default, set_name(name)),
            ],
            vec![(UNIVERSAL, reverse), (default, named)],
        )
    };
    Ok(Change {
        writes,
        reads,
        expect: Expect::Name(name.to_owned()),
    })
}

fn space_id_primary(
    (registry, reverse, resolver): ([u8; 20], [u8; 20], [u8; 20]),
    name: &str,
    sender: &[u8; 20],
) -> Change {
    let node = reverse_node(sender);
    Change {
        writes: vec![(reverse, set_name(name))],
        reads: vec![
            (registry, call("resolver(bytes32)", node)),
            (resolver, call("name(bytes32)", node)),
        ],
        expect: Expect::Reverse {
            resolver,
            name: name.to_owned(),
        },
    }
}

fn space_id_change(
    resolver: [u8; 20],
    node: [u8; 32],
    target: Option<[u8; 20]>,
    linked: bool,
) -> Result<Change, ChainError> {
    if resolver == [0; 20] {
        return Err(ChainError::Unsupported(
            "names without a resolver yet, set one where the name is managed",
        ));
    }
    if target.is_none() && !linked {
        return Err(ChainError::Invalid("this name isn't linked to an address"));
    }
    let mut data = selector("setAddr(bytes32,address)").to_vec();
    data.extend_from_slice(&node);
    abi::push_address(&mut data, &target.unwrap_or([0; 20]));
    Ok(Change {
        writes: vec![(resolver, data)],
        reads: vec![(resolver, call("addr(bytes32)", node))],
        expect: Expect::Address(target),
    })
}

fn set_name(name: &str) -> Vec<u8> {
    let mut data = selector("setName(string)").to_vec();
    data.extend(abi::encode(&[Arg::Bytes(name.as_bytes())]));
    data
}

fn get_many(keys: &[String], token: U256) -> Vec<u8> {
    let keys: Vec<Vec<u8>> = keys.iter().map(|key| key.as_bytes().to_vec()).collect();
    let mut data = selector("getMany(string[],uint256)").to_vec();
    data.extend(abi::encode(&[Arg::List(&keys), Arg::Uint(token)]));
    data
}

fn read_records(base: bool, output: &[u8], count: usize) -> Option<Vec<Vec<u8>>> {
    let list = if base {
        output
    } else {
        abi::bytes_at(output, 0)?
    };
    let items = abi::bytes_list(list, 0).filter(|items| items.len() == count)?;
    items
        .iter()
        .map(|item| abi::bytes_at(item, 0).map(<[u8]>::to_vec))
        .collect()
}

fn read_holder(reply: &[u8], count: usize) -> Option<Holder> {
    let answers =
        abi::bytes_list(reply, 0).filter(|answers| answers.len() == count && count > 3)?;
    let lists = answers[3..]
        .iter()
        .map(|answer| strings(answer).filter(|list| list.len() <= MAX_KEYS))
        .collect::<Option<Vec<_>>>()?;
    Some(Holder {
        owner: abi::address(answers[0], 0)?,
        exists: abi::uint(answers[1], 0)? == U256::from(1u8),
        registry: abi::address(answers[2], 0)?,
        lists,
    })
}

fn strings(output: &[u8]) -> Option<Vec<String>> {
    abi::bytes_list(output, 0)?
        .into_iter()
        .map(|item| {
            (item.len() <= MAX_VALUE)
                .then(|| String::from_utf8(item.to_vec()).ok())
                .flatten()
        })
        .collect()
}

fn effective(coins: &[u64], values: &[Vec<u8>], coin: u64) -> Option<[u8; 20]> {
    let value = |wanted: u64| {
        coins
            .iter()
            .position(|known| *known == wanted)
            .and_then(|index| values.get(index))
            .filter(|value| filled(value))
    };
    let found = match value(coin) {
        Some(found) => found,
        None if coin == ETHEREUM_COIN => return None,
        None => value(EVM_COIN)?,
    };
    found.as_slice().try_into().ok()
}

fn first_set(list: &[String], keys: &[String], values: &[String]) -> Option<[u8; 20]> {
    let text = list.iter().find_map(|key| {
        let index = keys.iter().position(|known| known == key)?;
        values
            .get(index)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
    })?;
    match Address::parse(ChainFamily::Evm, text) {
        Ok(Address::Evm(address)) if address != [0; 20] => Some(address),
        _ => None,
    }
}

fn filled(value: &[u8]) -> bool {
    value.iter().any(|byte| *byte != 0)
}

fn push_new(list: &mut Vec<u64>, coin: u64) {
    if !list.contains(&coin) {
        list.push(coin);
    }
}

#[cfg(fuzzing)]
pub(super) fn fuzz(data: &[u8]) {
    let _ = read_records(true, data, 4);
    let _ = read_records(false, data, 4);
    let _ = read_holder(data, 5);
    let _ = strings(data);
}

#[cfg(test)]
mod tests {
    use bahaa_types::hex;

    use super::*;

    const TARGET: [u8; 20] = [0x11; 20];

    fn records(v2: bool, values: Vec<Vec<u8>>) -> Records {
        let coins = vec![ETHEREUM_COIN, EVM_COIN, EVM_COIN | 0x38, EVM_COIN | BASE];
        Records {
            base: false,
            home: ETHEREUM_COIN,
            v2,
            resolver: [0x23; 20],
            node: namehash("alice.eth"),
            dns: dns_encoded("alice.eth").unwrap(),
            values,
            checked: vec![ETHEREUM_COIN, EVM_COIN | 0x38, EVM_COIN | BASE],
            coins,
        }
    }

    fn answer(items: &[Vec<u8>]) -> Vec<u8> {
        let encoded: Vec<Vec<u8>> = items
            .iter()
            .map(|item| abi::encode_bytes(&[item]))
            .collect();
        let mut list = Vec::new();
        abi::push_uint(&mut list, U256::from(32u8));
        abi::push_bytes_list(&mut list, &encoded);
        list
    }

    fn universal(items: &[Vec<u8>]) -> Vec<u8> {
        let result = answer(items);
        let mut reply = Vec::new();
        abi::push_uint(&mut reply, U256::from(64u8));
        abi::push_address(&mut reply, &[0x39; 20]);
        reply.extend(abi::encode_bytes(&[&result])[32..].iter());
        reply
    }

    #[test]
    fn record_writes_match_an_independent_encoder() {
        let held = records(false, vec![Vec::new(); 4]);
        assert_eq!(
            hex::encode(&held.set(ETHEREUM_COIN, Some(TARGET))),
            "8b95dd71787192fc5378cc32aa956ddfdedbf26b24e8d78e40109add0eea2c1a012c3dec000000000000000000000000000000000000000000000000000000000000003c000000000000000000000000000000000000000000000000000000000000006000000000000000000000000000000000000000000000000000000000000000141111111111111111111111111111111111111111000000000000000000000000"
        );
        assert_eq!(
            hex::encode(&held.set(EVM_COIN, None)),
            "8b95dd71787192fc5378cc32aa956ddfdedbf26b24e8d78e40109add0eea2c1a012c3dec000000000000000000000000000000000000000000000000000000008000000000000000000000000000000000000000000000000000000000000000000000600000000000000000000000000000000000000000000000000000000000000000"
        );
        assert_eq!(
            hex::encode(&held.set(ETHEREUM_COIN, None)),
            "d5fa2b00787192fc5378cc32aa956ddfdedbf26b24e8d78e40109add0eea2c1a012c3dec0000000000000000000000000000000000000000000000000000000000000000"
        );
        assert_eq!(
            hex::encode(&records(true, vec![Vec::new(); 4]).set(ETHEREUM_COIN, Some(TARGET))),
            "b4436dde0000000000000000000000000000000000000000000000000000000000000060000000000000000000000000000000000000000000000000000000000000003c00000000000000000000000000000000000000000000000000000000000000a0000000000000000000000000000000000000000000000000000000000000000b05616c696365036574680000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000141111111111111111111111111111111111111111000000000000000000000000"
        );
        assert_eq!(
            hex::encode(&multicall(&[held.set(ETHEREUM_COIN, Some(TARGET))])),
            "ac9650d800000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000001000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000000000000000000a48b95dd71787192fc5378cc32aa956ddfdedbf26b24e8d78e40109add0eea2c1a012c3dec000000000000000000000000000000000000000000000000000000000000003c00000000000000000000000000000000000000000000000000000000000000600000000000000000000000000000000000000000000000000000000000000014111111111111111111111111111111111111111100000000000000000000000000000000000000000000000000000000000000000000000000000000"
        );
        assert_eq!(
            hex::encode(&set_name("alice.eth")),
            "c47f002700000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000009616c6963652e6574680000000000000000000000000000000000000000000000"
        );
    }

    #[test]
    fn unstoppable_writes_match_an_independent_encoder() {
        let entries = Entries {
            registry: [0xa9; 20],
            token: U256::from_be_bytes(namehash("brad.crypto")),
            labels: vec!["brad".to_owned(), "crypto".to_owned()],
            lists: vec![vec![
                "crypto.ETH.address".to_owned(),
                GENERIC_KEY.to_owned(),
            ]],
            keys: vec!["crypto.ETH.address".to_owned(), GENERIC_KEY.to_owned()],
            values: vec![
                "0x2222222222222222222222222222222222222222".to_owned(),
                String::new(),
            ],
        };
        let change = entries.change(Some(TARGET)).unwrap();
        assert_eq!(
            hex::encode(&change.writes[0].1),
            "ce92b33e00000000000000000000000000000000000000000000000000000000000000600000000000000000000000000000000000000000000000000000000000000140756e4e998dbffd803c21d23b06cd855cdc7a4b57706c95964a37e24b47c10fc90000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000000000000000004000000000000000000000000000000000000000000000000000000000000000800000000000000000000000000000000000000000000000000000000000000011746f6b656e2e45564d2e61646472657373000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000001263727970746f2e4554482e6164647265737300000000000000000000000000000000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000000000000000004000000000000000000000000000000000000000000000000000000000000000a0000000000000000000000000000000000000000000000000000000000000002a30783131313131313131313131313131313131313131313131313131313131313131313131313131313100000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000002a30783131313131313131313131313131313131313131313131313131313131313131313131313131313100000000000000000000000000000000000000000000"
        );
        assert_eq!(
            hex::encode(&entries.primary("brad.crypto", &[0x8a; 20]).writes[0].1),
            "663f7b2a000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000040000000000000000000000000000000000000000000000000000000000000008000000000000000000000000000000000000000000000000000000000000000046272616400000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000663727970746f0000000000000000000000000000000000000000000000000000"
        );
        assert!(entries.change(None).is_ok());
        let empty = Entries {
            values: vec![String::new(), String::new()],
            ..entries
        };
        assert!(empty.change(None).is_err());
    }

    #[test]
    fn linking_writes_the_general_records_and_every_record_that_would_win() {
        let mut held = records(
            false,
            vec![vec![0x22; 20], Vec::new(), vec![0x33; 20], Vec::new()],
        );
        let change = held.change(Some(TARGET)).unwrap();
        let sets = abi::bytes_list(&change.writes[0].1[4..], 0).unwrap();
        let coins: Vec<U256> = sets
            .iter()
            .map(|set| abi::uint(&set[4..], 1).unwrap())
            .collect();
        assert_eq!(
            coins,
            [60, EVM_COIN, EVM_COIN | 0x38].map(U256::from).to_vec()
        );
        let cleared = held.change(None).unwrap();
        assert_eq!(
            abi::bytes_list(&cleared.writes[0].1[4..], 0).unwrap().len(),
            2
        );
        held.values = vec![TARGET.to_vec(), TARGET.to_vec(), Vec::new(), Vec::new()];
        assert!(held.change(Some(TARGET)).is_err());
        held.values = vec![Vec::new(), vec![0; 20], Vec::new(), Vec::new()];
        assert!(held.change(None).is_err());
        held.resolver = [0; 20];
        assert!(held.change(Some(TARGET)).is_err());
    }

    #[test]
    fn a_chain_record_wins_over_the_default_but_ethereum_reads_only_its_own() {
        let coins = [ETHEREUM_COIN, EVM_COIN, EVM_COIN | 0x38];
        let values = vec![Vec::new(), vec![0x44; 20], vec![0x55; 20]];
        assert_eq!(
            effective(&coins, &values, EVM_COIN | 0x38),
            Some([0x55; 20])
        );
        assert_eq!(
            effective(&coins, &values, EVM_COIN | BASE),
            Some([0x44; 20])
        );
        assert_eq!(effective(&coins, &values, ETHEREUM_COIN), None);
        let odd = vec![vec![1; 32], Vec::new(), vec![0; 20]];
        assert_eq!(effective(&coins, &odd, ETHEREUM_COIN), None);
        assert_eq!(effective(&coins, &odd, EVM_COIN | 0x38), None);
        let keys = ["a".to_owned(), "b".to_owned()];
        let list = keys.to_vec();
        let text = |value: &str| vec![String::new(), value.to_owned()];
        assert_eq!(
            first_set(
                &list,
                &keys,
                &text("0x1111111111111111111111111111111111111111")
            ),
            Some(TARGET)
        );
        assert_eq!(first_set(&list, &keys, &text("not an address")), None);
        assert_eq!(
            first_set(
                &list,
                &keys,
                &text("0x0000000000000000000000000000000000000000")
            ),
            None
        );
    }

    #[test]
    fn read_backs_must_show_exactly_what_was_asked() {
        let after = vec![TARGET.to_vec(), TARGET.to_vec(), Vec::new(), Vec::new()];
        let expect = |base: bool, want: Option<[u8; 20]>| Expect::Records {
            base,
            coins: vec![ETHEREUM_COIN, EVM_COIN, EVM_COIN | 0x38, EVM_COIN | BASE],
            checked: vec![ETHEREUM_COIN, EVM_COIN | 0x38],
            want,
        };
        assert!(expect(false, Some(TARGET)).met(&[universal(&after)]));
        assert!(expect(true, Some(TARGET)).met(&[answer(&after)]));
        assert!(!expect(false, Some(TARGET)).met(&[answer(&after)]));
        assert!(!expect(false, Some([0x12; 20])).met(&[universal(&after)]));
        assert!(!expect(false, None).met(&[universal(&after)]));
        assert!(expect(false, None).met(&[universal(&vec![Vec::new(); 4])]));
        assert!(!expect(false, Some(TARGET)).met(&[universal(&after[..3])]));
        let mut word = vec![0; 12];
        word.extend(TARGET);
        assert!(Expect::Address(Some(TARGET)).met(&[word]));
        assert!(Expect::Address(None).met(&[vec![0; 32]]));
        let named = abi::encode(&[Arg::Bytes(b"alice.eth")]);
        let name = Expect::Name("alice.eth".to_owned());
        assert!(name.met(&[named.clone(), named.clone()]));
        assert!(!name.met(&[named.clone(), abi::encode(&[Arg::Bytes(b"bob.eth")])]));
        assert!(!name.met(&[]));
        let mut pointed = vec![0; 12];
        pointed.extend([0xd8; 20]);
        let reverse = Expect::Reverse {
            resolver: [0xd8; 20],
            name: "alice.eth".to_owned(),
        };
        assert!(reverse.met(&[pointed.clone(), named.clone()]));
        assert!(!reverse.met(&[named, pointed]));
    }

    #[test]
    fn reverse_nodes_are_the_registries_own() {
        let sender = hex::decode_array::<20>("d8da6bf26964af9d7eed9e03e53415d37aa96045").unwrap();
        let change = space_id_primary(([1; 20], [2; 20], [3; 20]), "spaceid.bnb", &sender);
        assert_eq!(
            hex::encode(&change.reads[0].1[4..]),
            "7aef81fbd30c83431369026d62ee533af8b69f246b63d75b40fe223346e6fa9a"
        );
    }
}
