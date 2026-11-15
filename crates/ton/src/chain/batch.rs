use bahaa_chain::{ChainError, FeeItem, FeeLine, FeeQuote, Pace, Signed, Transfer, enough};
use bahaa_crypto::Seed;
use bahaa_types::{Address, Amount, AssetId, NftId, TonContract, Trust};
use ed25519_dalek::{Signer, SigningKey};
use ruint::aliases::U256;
use tonlib_core::TonHash;
use tonlib_core::cell::{ArcCell, BagOfCells, Cell};
use tonlib_core::tlb_types::tlb::TLB;
use tonlib_core::wallet::mnemonic::KeyPair;
use tonlib_core::wallet::version_helper::VersionHelper;
use tonlib_core::wallet::versioned::v4::WalletDataV4;
use tonlib_core::wallet::versioned::v5::WalletDataV5;
use tycho_types::boc::Boc;
use tycho_types::cell::HashBytes;
use tycho_types::dict::Dict;
use tycho_types::models::{
    Account as Stored, AccountState, CurrencyCollection, IntAddr, OptionalAccount, StateInit,
    StdAddr, StorageExtra, StorageInfo, StorageUsed,
};
use tycho_types::num::{Tokens, VarUint56};

use super::send::{CONFIRM_BLOCKS, LIFETIME, prices};
use super::tokens::Contracts;
use super::{TonChain, amount};
use crate::batch::{self, Batch, Outbound, max_messages};
use crate::cells::{Account, Status};
use crate::comment;
use crate::emulate::{Emulated, Emulator, Hop};
use crate::fees::Settings;
use crate::http::{Item, Listed};
use crate::wallet::{
    self, Envelope, Forwarding, NFT_TRANSFER, Order, TOKEN_TRANSFER, read_address,
};

const REHEARSAL_KEY: [u8; 32] = [7; 32];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Requested {
    pub account: u32,
    pub from: Address,
    pub messages: Vec<Outbound>,
    pub valid_until: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Meaning {
    Coins {
        comment: Option<String>,
    },
    Token {
        master: Address,
        amount: Amount,
        to: Address,
    },
    ForeignToken,
    Nft {
        item: Address,
        to: Address,
    },
    ForeignNft,
    Call {
        op: Option<u32>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Wanted {
    Coins(Outbound),
    Token(Handover),
    Nft(Handover),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Handover {
    pub contract: Address,
    pub to: Address,
    pub units: U256,
    pub attach: Option<U256>,
    pub query_id: Option<u64>,
    pub response: Option<Address>,
    pub custom: Option<Vec<u8>>,
    pub forward: Option<U256>,
    pub payload: Option<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reading {
    pub to: Address,
    pub bounce: bool,
    pub amount: Amount,
    pub meaning: Meaning,
    pub deploys: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TonBatch {
    pub(super) account: u32,
    pub(super) batch: Batch,
    pub(super) deploy: bool,
    pub(super) readings: Vec<Reading>,
}

impl TonBatch {
    pub fn from(&self) -> &Address {
        &self.batch.envelope.wallet
    }

    pub fn messages(&self) -> &[Outbound] {
        &self.batch.messages
    }

    pub fn readings(&self) -> &[Reading] {
        &self.readings
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub public_key: [u8; 32],
    pub state_init: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedBatch {
    pub from: Address,
    pub messages: Vec<Outbound>,
    pub sequence: u64,
    pub expires_at: u64,
}

impl TonChain {
    pub async fn prepare_batch(
        &self,
        requested: &Requested,
    ) -> Result<(TonBatch, FeeQuote), ChainError> {
        let (batch, wallet, deploy, now) = self.planned(requested).await?;
        let sent = batch
            .messages
            .iter()
            .try_fold(U256::ZERO, |sum, message| sum.checked_add(message.nano))
            .ok_or_else(ChainError::overflow)?;
        let native = &self.network.native;
        enough(&native.symbol, sent, wallet.balance, native.decimals)?;
        let (hop, settings) = self.rehearse(&batch, &wallet, now).await?;
        if !hop.success {
            return Err(ChainError::Rejected(
                "your wallet would refuse this request".to_owned(),
            ));
        }
        if hop.out.len() != batch.messages.len() {
            return Err(ChainError::Rejected(
                "your wallet would leave out some of these messages".to_owned(),
            ));
        }
        let fee = hop
            .fees
            .checked_sub(hop.gas_fees)
            .and_then(|rest| rest.checked_add(hop.charged_gas_fees))
            .ok_or_else(ChainError::overflow)?;
        let max = fee.checked_mul(2).ok_or_else(ChainError::overflow)?;
        enough(
            &native.symbol,
            sent.checked_add(U256::from(max))
                .ok_or_else(ChainError::overflow)?,
            wallet.balance,
            native.decimals,
        )?;
        let readings = self.readings(&batch).await?;
        let gram = |nano: u64| amount(U256::from(nano), native.decimals);
        let lines = vec![
            FeeLine {
                item: FeeItem::Compute {
                    gas: settings.gas_for(hop.charged_gas_fees),
                },
                amount: gram(hop.charged_gas_fees)?,
            },
            FeeLine {
                item: FeeItem::Forwarding,
                amount: gram(fee.saturating_sub(hop.charged_gas_fees))?,
            },
        ];
        let quote = FeeQuote::new(gram(max)?, gram(fee)?, lines)?
            .paced(Pace::blocks(CONFIRM_BLOCKS, self.network.block_ms));
        Ok((
            TonBatch {
                account: requested.account,
                batch,
                deploy,
                readings,
            },
            quote,
        ))
    }

    pub async fn prepare_relayed(&self, requested: &Requested) -> Result<TonBatch, ChainError> {
        let (batch, _, deploy, _) = self.planned(requested).await?;
        if batch.envelope.contract != TonContract::W5 {
            return Err(ChainError::Unsupported(
                "signed messages for relayers on wallets other than W5",
            ));
        }
        let readings = self.readings(&batch).await?;
        Ok(TonBatch {
            account: requested.account,
            batch,
            deploy,
            readings,
        })
    }

    async fn planned(
        &self,
        requested: &Requested,
    ) -> Result<(Batch, Account, bool, u64), ChainError> {
        if requested.messages.is_empty() {
            return Err(ChainError::Invalid("the request has no messages"));
        }
        let messages = requested
            .messages
            .iter()
            .cloned()
            .map(Outbound::canonical)
            .collect::<Result<Vec<_>, _>>()?;
        let wallet = self.http.account(&requested.from).await?;
        let (seqno, deploy, contract) = self.sequence(&wallet, &requested.from)?;
        if messages.len() > max_messages(contract) {
            return Err(ChainError::Invalid(match contract {
                TonContract::V4R2 => "a V4R2 wallet sends at most 4 messages at once",
                TonContract::W5 => "a W5 wallet sends at most 255 messages at once",
            }));
        }
        let now = bahaa_chain::now();
        let until = match requested.valid_until {
            Some(until) if until <= now => {
                return Err(ChainError::Invalid("the app's request has expired"));
            }
            Some(until) => until.min(now + LIFETIME),
            None => now + LIFETIME,
        };
        let batch = Batch {
            envelope: Envelope {
                wallet: requested.from,
                contract,
                wallet_id: self.subwallet(contract),
                seqno,
                valid_until: u32::try_from(until)
                    .map_err(|_| ChainError::Invalid("the clock is out of range"))?,
            },
            messages,
        };
        Ok((batch, wallet, deploy, now))
    }

    pub async fn build_items(
        &self,
        account: u32,
        from: &Address,
        wanted: &[Wanted],
    ) -> Result<Vec<Outbound>, ChainError> {
        let wallet = self.http.account(from).await?;
        let (seqno, _, contract) = self.sequence(&wallet, from)?;
        let now = bahaa_chain::now();
        let clock =
            u32::try_from(now).map_err(|_| ChainError::Invalid("the clock is out of range"))?;
        let config = self.http.config().await?;
        let (settings, _) = prices(&config)?;
        let contracts = Contracts {
            config: &config,
            settings,
            now: clock,
        };
        let base = Order {
            wallet: *from,
            contract,
            wallet_id: self.subwallet(contract),
            seqno,
            valid_until: u32::try_from(now + LIFETIME)
                .map_err(|_| ChainError::Invalid("the clock is out of range"))?,
            to: *from,
            nano: U256::ZERO,
            everything: false,
            bounce: true,
            token: None,
            item: None,
            memo: None,
            call: None,
        };
        let mut built = Vec::with_capacity(wanted.len());
        for want in wanted {
            built.push(match want {
                Wanted::Coins(message) => message.clone(),
                Wanted::Token(handover) => {
                    self.handed(account, handover, true, &base, &wallet, contracts)
                        .await?
                }
                Wanted::Nft(handover) => {
                    self.handed(account, handover, false, &base, &wallet, contracts)
                        .await?
                }
            });
        }
        Ok(built)
    }

    async fn handed(
        &self,
        account: u32,
        handover: &Handover,
        token: bool,
        base: &Order,
        wallet: &Account,
        contracts: Contracts<'_>,
    ) -> Result<Outbound, ChainError> {
        let from = base.wallet;
        let mut order = Order {
            to: handover.to,
            ..base.clone()
        };
        let network = self.network.id.clone();
        let (asset, units) = if token {
            let decimals = self.token_decimals(&handover.contract).await?;
            (
                AssetId::Token(network, handover.contract),
                amount(handover.units, decimals)?,
            )
        } else {
            let item = NftId::new(handover.contract, None)
                .map_err(|_| ChainError::Invalid("not a TON NFT"))?;
            (
                AssetId::Nft(network, Box::new(item)),
                amount(U256::from(1u8), 0)?,
            )
        };
        let transfer = Transfer {
            account,
            from,
            to: handover.to,
            asset,
            amount: units,
            max: false,
            memo: None,
        };
        let (destination, query_id) = if token {
            self.token_order(&transfer, &handover.contract, &mut order, wallet, contracts)
                .await?;
            let moved = order.token.ok_or_else(ChainError::malformed)?;
            (moved.wallet, moved.query_id)
        } else {
            self.item_order(&transfer, &handover.contract, &mut order, wallet, contracts)
                .await?;
            let moved = order.item.ok_or_else(ChainError::malformed)?;
            (moved.address, moved.query_id)
        };
        let forward = handover.forward.unwrap_or(U256::from(1u8));
        let attach = match handover.attach {
            Some(attach) => attach,
            None => order
                .nano
                .checked_add(forward)
                .ok_or_else(ChainError::overflow)?,
        };
        let forwarding = Forwarding {
            query_id: handover.query_id.unwrap_or(query_id),
            to: &handover.to,
            response: handover.response.as_ref().unwrap_or(&from),
            custom: handover.custom.as_deref().map(cell).transpose()?,
            forward,
            payload: handover.payload.as_deref().map(cell).transpose()?,
        };
        let body = if token {
            wallet::jetton_body(handover.units, forwarding)?
        } else {
            wallet::nft_body(forwarding)?
        };
        Ok(Outbound {
            to: destination,
            bounce: true,
            nano: attach,
            body: Some(wallet::serialized(&body)?),
            init: None,
        })
    }

    pub fn sign_batch(&self, plan: &TonBatch, seed: &Seed) -> Result<Signed, ChainError> {
        let envelope = &plan.batch.envelope;
        self.sign_envelope(
            seed,
            (plan.account, &envelope.wallet),
            envelope,
            plan.deploy,
            &batch::unsigned(&plan.batch)?,
        )
    }

    pub fn sign_internal(&self, plan: &TonBatch, seed: &Seed) -> Result<Vec<u8>, ChainError> {
        let envelope = &plan.batch.envelope;
        if envelope.contract != TonContract::W5 {
            return Err(ChainError::Unsupported(
                "signed messages for relayers on wallets other than W5",
            ));
        }
        let key = self.owned_key(seed, (plan.account, &envelope.wallet), envelope)?;
        let public = key.verifying_key().to_bytes();
        let unsigned = wallet::internal_signed(&batch::unsigned(&plan.batch)?)?;
        let digest = unsigned.cell_hash();
        let signature = key.sign(digest.as_slice());
        key.verifying_key()
            .verify_strict(digest.as_slice(), &signature)
            .map_err(|_| ChainError::Invalid("signing failed"))?;
        let body = VersionHelper::sign_msg(
            wallet::version(envelope.contract),
            &unsigned,
            &signature.to_bytes(),
        )
        .map_err(|_| ChainError::Invalid("cannot build the signed message"))?;
        let init = plan
            .deploy
            .then(|| wallet::deployment(envelope.contract, &public, envelope.wallet_id))
            .transpose()?;
        let message = wallet::internal(&envelope.wallet, true, U256::ZERO, body, init, None)?;
        wallet::serialized(&message)
    }

    pub fn identity(
        &self,
        seed: &Seed,
        account: u32,
        from: &Address,
    ) -> Result<Identity, ChainError> {
        let contract = self.contract(from);
        let public = self
            .account_key(seed, (account, from), contract)?
            .verifying_key()
            .to_bytes();
        let init = wallet::deployment(contract, &public, self.subwallet(contract))?
            .to_cell()
            .map_err(|_| ChainError::Invalid("cannot build the wallet's contract"))?;
        Ok(Identity {
            public_key: public,
            state_init: wallet::serialized(&init)?,
        })
    }

    pub fn sign_hash(
        &self,
        seed: &Seed,
        account: u32,
        from: &Address,
        hash: &[u8; 32],
    ) -> Result<[u8; 64], ChainError> {
        let key = self.account_key(seed, (account, from), self.contract(from))?;
        let signature = key.sign(hash);
        key.verifying_key()
            .verify_strict(hash, &signature)
            .map_err(|_| ChainError::Invalid("signing failed"))?;
        Ok(signature.to_bytes())
    }

    pub fn decode_batch(&self, signed: &Signed) -> Result<DecodedBatch, ChainError> {
        let parsed = batch::parse(&signed.bytes)?;
        let envelope = parsed.batch.envelope;
        self.verified(&envelope, &parsed.seal, signed)?;
        Ok(DecodedBatch {
            from: envelope.wallet,
            messages: parsed.batch.messages,
            sequence: u64::from(envelope.seqno),
            expires_at: u64::from(envelope.valid_until),
        })
    }

    async fn rehearse(
        &self,
        batch: &Batch,
        wallet: &Account,
        now: u64,
    ) -> Result<(Hop, Settings), ChainError> {
        let config = self.http.config().await?;
        let (settings, _) = prices(&config)?;
        let params = config.encoded().ok_or_else(|| {
            ChainError::Network("the network config cannot be encoded".to_owned())
        })?;
        let (state, message) = rehearsal(batch, wallet, now)?;
        let clock =
            u32::try_from(now).map_err(|_| ChainError::Invalid("the clock is out of range"))?;
        let mut libraries = Vec::new();
        for _ in 0..=crate::vm::MAX_LIBRARIES {
            let ran = {
                let emulator =
                    Emulator::new(&params, &libraries, settings, clock).ok_or_else(|| {
                        ChainError::Network("the network config cannot be emulated".to_owned())
                    })?;
                emulator
                    .run_external(&batch.envelope.wallet, &state, &message)
                    .map_err(|_| {
                        ChainError::Rejected("your wallet can't pay to run this request".to_owned())
                    })?
            };
            match ran {
                Emulated::Done(hop) => return Ok((hop, settings)),
                Emulated::Missing(hash) => libraries.push(self.http.library(&hash).await?),
            }
        }
        Err(ChainError::Network(
            "a contract needs too many libraries".to_owned(),
        ))
    }

    async fn readings(&self, batch: &Batch) -> Result<Vec<Reading>, ChainError> {
        let decimals = self.network.native.decimals;
        let mut readings = Vec::with_capacity(batch.messages.len());
        for message in &batch.messages {
            readings.push(Reading {
                to: message.to,
                bounce: message.bounce,
                amount: amount(message.nano, decimals)?,
                meaning: self.meaning(&batch.envelope.wallet, message).await?,
                deploys: message.init.is_some(),
            });
        }
        Ok(readings)
    }

    async fn meaning(&self, owner: &Address, message: &Outbound) -> Result<Meaning, ChainError> {
        let Some(root) = message.body.as_deref().and_then(crate::cells::root) else {
            return Ok(Meaning::Coins { comment: None });
        };
        let op = root.parser().load_u32(32).ok();
        match op {
            Some(comment::COMMENT) => Ok(match comment::text(&root) {
                Some(text) => Meaning::Coins {
                    comment: Some(text),
                },
                None => Meaning::Call { op },
            }),
            Some(TOKEN_TRANSFER) => {
                let Some((units, to)) = token_transfer(&root) else {
                    return Ok(Meaning::Call { op });
                };
                let Some(stack) = self.http.run(&message.to, "get_wallet_data", &[]).await? else {
                    return Ok(Meaning::ForeignToken);
                };
                let [Item::Num(_), Item::Cell(holder), Item::Cell(minter), _] = stack.as_slice()
                else {
                    return Ok(Meaning::ForeignToken);
                };
                match (read_address(holder), read_address(minter)) {
                    (Some(holder), Some(master)) if holder == *owner => Ok(Meaning::Token {
                        master,
                        amount: amount(units, self.token_decimals(&master).await?)?,
                        to,
                    }),
                    _ => Ok(Meaning::ForeignToken),
                }
            }
            Some(NFT_TRANSFER) => {
                let Some(to) = nft_transfer(&root) else {
                    return Ok(Meaning::Call { op });
                };
                let listed = Listed {
                    address: message.to,
                    trust: Trust::Unknown,
                };
                Ok(match self.held(owner, listed).await? {
                    Some(_) => Meaning::Nft {
                        item: message.to,
                        to,
                    },
                    None => Meaning::ForeignNft,
                })
            }
            _ => Ok(Meaning::Call { op }),
        }
    }
}

#[cfg(fuzzing)]
pub(super) fn fuzz(data: &[u8]) {
    if let Some(root) = crate::cells::root(data) {
        let _ = token_transfer(&root);
        let _ = nft_transfer(&root);
    }
    let message = Outbound {
        to: Address::Ton {
            workchain: 0,
            hash: [1; 32],
        },
        bounce: true,
        nano: U256::from(1u8),
        body: Some(data.to_vec()),
        init: Some(data.to_vec()),
    };
    let _ = message.canonical();
}

fn cell(boc: &[u8]) -> Result<Cell, ChainError> {
    crate::cells::root(boc)
        .map(|root| (*root).clone())
        .ok_or(ChainError::Invalid(
            "a payload in the request isn't one cell",
        ))
}

fn token_transfer(root: &ArcCell) -> Option<(U256, Address)> {
    let mut parser = root.parser();
    parser.load_u32(32).ok()?;
    parser.load_u64(64).ok()?;
    let units = U256::try_from_be_slice(&parser.load_coins().ok()?.to_bytes_be())?;
    let to = crate::cells::from_ton(&parser.load_address().ok()?)?;
    Some((units, to))
}

fn nft_transfer(root: &ArcCell) -> Option<Address> {
    let mut parser = root.parser();
    parser.load_u32(32).ok()?;
    parser.load_u64(64).ok()?;
    crate::cells::from_ton(&parser.load_address().ok()?)
}

fn rehearsal(batch: &Batch, wallet: &Account, now: u64) -> Result<(Vec<u8>, Vec<u8>), ChainError> {
    let broken = || ChainError::Invalid("the request can't be tried on this wallet");
    let envelope = &batch.envelope;
    let key = SigningKey::from_bytes(&REHEARSAL_KEY);
    let public = key.verifying_key().to_bytes();
    let version = wallet::version(envelope.contract);
    let data = match &wallet.status {
        Status::Active {
            data: Some(data), ..
        } => rekeyed(envelope.contract, data, &public)?,
        Status::Missing | Status::Uninit => VersionHelper::get_data(
            version,
            &KeyPair {
                public_key: public.to_vec(),
                secret_key: Vec::new(),
            },
            envelope.wallet_id,
        )
        .map_err(|_| broken())?,
        _ => return Err(broken()),
    };
    let code = VersionHelper::get_code(version).map_err(|_| broken())?;
    let (workchain, hash) = crate::cells::parts(&envelope.wallet)?;
    let stored = Stored {
        address: IntAddr::Std(StdAddr::new(workchain, HashBytes(hash))),
        storage_stat: StorageInfo {
            used: StorageUsed {
                cells: VarUint56::new(wallet.cells.max(1)),
                bits: VarUint56::new(wallet.bits),
            },
            storage_extra: StorageExtra::None,
            last_paid: if wallet.last_paid == 0 {
                u32::try_from(now).map_err(|_| broken())?
            } else {
                wallet.last_paid
            },
            due_payment: u128::try_from(wallet.due)
                .ok()
                .filter(|due| *due > 0)
                .map(Tokens::new),
        },
        last_trans_lt: wallet.last.map_or(0, |(_, lt)| lt),
        balance: CurrencyCollection::new(u128::try_from(wallet.balance).map_err(|_| broken())?),
        state: AccountState::Active(StateInit {
            split_depth: None,
            special: None,
            code: Some(tycho(code)?),
            data: Some(tycho(&data)?),
            libraries: Dict::new(),
        }),
    };
    let state = Boc::encode(
        tycho_types::cell::CellBuilder::build_from(OptionalAccount(Some(stored)))
            .map_err(|_| broken())?,
    );
    let unsigned = batch::unsigned(batch)?;
    let signature = key.sign(unsigned.cell_hash().as_slice()).to_bytes();
    let message = wallet::serialized(&wallet::wrapped(envelope, &unsigned, &signature, None)?)?;
    Ok((state, message))
}

fn rekeyed(contract: TonContract, data: &ArcCell, public: &[u8; 32]) -> Result<Cell, ChainError> {
    let broken = |_| ChainError::Invalid("the wallet's data is unreadable");
    let key = TonHash::from(*public);
    match contract {
        TonContract::W5 => {
            let mut parsed = WalletDataV5::from_cell(data).map_err(broken)?;
            parsed.public_key = key;
            parsed.to_cell()
        }
        TonContract::V4R2 => {
            let mut parsed = WalletDataV4::from_cell(data).map_err(broken)?;
            parsed.public_key = key;
            parsed.to_cell()
        }
    }
    .map_err(broken)
}

fn tycho(cell: &Cell) -> Result<tycho_types::cell::Cell, ChainError> {
    let bytes = BagOfCells::from_root(cell.clone())
        .serialize(false)
        .map_err(|_| ChainError::Invalid("cannot encode the cell"))?;
    Boc::decode(bytes).map_err(|_| ChainError::Invalid("cannot encode the cell"))
}

#[cfg(test)]
mod tests {
    use bahaa_chain::Chain;
    use bahaa_types::TonContract;
    use ruint::aliases::U256;
    use tonlib_core::cell::CellBuilder;
    use tonlib_core::tlb_types::block::state_init::StateInit;
    use tonlib_core::tlb_types::tlb::TLB;

    use super::*;
    use crate::chain::send::tests::{carried, chain, seed, stored};
    use crate::emulate::{self, tests::NOW, tests::done};
    use crate::wallet::{Envelope, serialized};

    fn party(fill: u8) -> Address {
        Address::Ton {
            workchain: 0,
            hash: [fill; 32],
        }
    }

    fn messages() -> Vec<Outbound> {
        let comment = serialized(&crate::comment::build("for the app").unwrap()).unwrap();
        let mut code = CellBuilder::new();
        code.store_u32(32, 0xdead_beef).unwrap();
        let mut data = CellBuilder::new();
        data.store_u8(8, 7).unwrap();
        let init = StateInit::new(
            code.build().unwrap().to_arc(),
            data.build().unwrap().to_arc(),
        )
        .to_cell()
        .unwrap();
        let call = crate::Body::default()
            .uint(32, 0x1234_5678)
            .unwrap()
            .uint(64, 9)
            .unwrap()
            .boc()
            .unwrap();
        vec![
            Outbound {
                to: party(0x11),
                bounce: false,
                nano: U256::from(100_000_000u64),
                body: Some(comment),
                init: None,
            },
            Outbound {
                to: party(0x22),
                bounce: true,
                nano: U256::from(50_000_000u64),
                body: Some(call),
                init: Some(serialized(&init).unwrap()),
            },
            Outbound {
                to: party(0x33),
                bounce: false,
                nano: U256::from(1u64),
                body: None,
                init: None,
            },
        ]
    }

    fn plan(
        chain: &TonChain,
        contract: TonContract,
        (deploy, seqno): (bool, u32),
        messages: Vec<Outbound>,
    ) -> TonBatch {
        let from = chain.derive_as(&seed(), 0, contract).unwrap();
        TonBatch {
            account: 0,
            batch: Batch {
                envelope: Envelope {
                    wallet: from,
                    contract,
                    wallet_id: chain.subwallet(contract),
                    seqno,
                    valid_until: NOW + 300,
                },
                messages: messages
                    .into_iter()
                    .map(|message| message.canonical().unwrap())
                    .collect(),
            },
            deploy,
            readings: Vec::new(),
        }
    }

    #[test]
    fn a_signed_request_reads_back_message_by_message() {
        let ton = chain("ton");
        for contract in [TonContract::W5, TonContract::V4R2] {
            for deploy in [false, true] {
                let plan = plan(
                    &ton,
                    contract,
                    (deploy, if deploy { 0 } else { 5 }),
                    messages(),
                );
                let signed = ton.sign_batch(&plan, &seed()).unwrap();
                let decoded = ton.decode_batch(&signed).unwrap();
                assert_eq!(decoded.from, *plan.from());
                assert_eq!(decoded.messages, plan.messages());
                assert_eq!(decoded.sequence, if deploy { 0 } else { 5 });
                assert_eq!(decoded.expires_at, u64::from(NOW + 300));
                assert!(
                    ton.decode(&signed).is_err(),
                    "several messages aren't a plain transfer"
                );
            }
        }
    }

    #[test]
    fn v4r2_takes_four_messages_and_w5_more() {
        let ton = chain("ton");
        let many: Vec<Outbound> = (0..5).flat_map(|_| messages()).take(5).collect();
        let v4 = plan(&ton, TonContract::V4R2, (false, 5), many.clone());
        assert!(ton.sign_batch(&v4, &seed()).is_err());
        let w5 = plan(&ton, TonContract::W5, (false, 5), many);
        let signed = ton.sign_batch(&w5, &seed()).unwrap();
        assert_eq!(ton.decode_batch(&signed).unwrap().messages.len(), 5);
        let none = plan(&ton, TonContract::W5, (false, 5), Vec::new());
        assert!(ton.sign_batch(&none, &seed()).is_err());
    }

    #[test]
    fn bodies_and_contracts_must_be_single_cells() {
        let mut broken = messages().remove(0);
        broken.body = Some(vec![0xb5, 0xee, 0x9c, 0x72, 1, 1, 1, 1, 0, 3, 0, 1, 0, 5]);
        assert!(broken.clone().canonical().is_err());
        broken.body = Some(serialized(&CellBuilder::new().build().unwrap()).unwrap());
        assert_eq!(broken.canonical().unwrap().body, None);
        let mut odd = messages().remove(1);
        odd.init = odd.body.clone();
        assert!(odd.canonical().is_err());
    }

    #[test]
    fn the_real_wallets_send_every_message_of_a_request() {
        let ton = chain("ton");
        let emulator = emulate::tests::emulator();
        for contract in [TonContract::W5, TonContract::V4R2] {
            for deploy in [false, true] {
                let plan = plan(&ton, contract, (deploy, 0), messages());
                let signed = ton.sign_batch(&plan, &seed()).unwrap();
                let state = stored(&ton, plan.from(), contract, deploy);
                let hop = done(
                    emulator
                        .run_external(plan.from(), &state, &signed.bytes)
                        .unwrap(),
                );
                assert!(hop.success, "{contract:?} deploy {deploy}: {hop:?}");
                let mut sent: Vec<(Address, u128)> = hop
                    .out
                    .iter()
                    .map(|message| (emulate::destination(message).unwrap().0, carried(message).0))
                    .collect();
                sent.sort_by_key(|(to, _)| to.to_string());
                assert_eq!(
                    sent,
                    [
                        (party(0x11), 100_000_000),
                        (party(0x22), 50_000_000),
                        (party(0x33), 1)
                    ],
                    "{contract:?} deploy {deploy}"
                );
            }
        }
    }

    fn account(ton: &TonChain, contract: TonContract, status: &str, balance: u64) -> Account {
        let public = crate::chain::key(&seed(), 0)
            .unwrap()
            .verifying_key()
            .to_bytes();
        let keys = KeyPair {
            public_key: public.to_vec(),
            secret_key: Vec::new(),
        };
        let data =
            VersionHelper::get_data(wallet::version(contract), &keys, ton.subwallet(contract))
                .unwrap()
                .to_arc();
        Account {
            balance: U256::from(balance),
            bits: 1_300,
            cells: 3,
            last_paid: NOW - 86_400,
            due: U256::ZERO,
            status: match status {
                "active" => Status::Active {
                    code: wallet::code_hash(contract).unwrap(),
                    data: Some(data),
                },
                _ => Status::Uninit,
            },
            last: None,
            boc: Vec::new(),
        }
    }

    #[test]
    fn a_rehearsal_runs_the_request_on_the_wallets_own_code() {
        let ton = chain("ton");
        let emulator = emulate::tests::emulator();
        for contract in [TonContract::W5, TonContract::V4R2] {
            for status in ["active", "uninit"] {
                let plan = plan(&ton, contract, (status == "uninit", 0), messages());
                let wallet = account(&ton, contract, status, 2_000_000_000);
                let (state, message) = rehearsal(&plan.batch, &wallet, u64::from(NOW)).unwrap();
                let hop = done(
                    emulator
                        .run_external(plan.from(), &state, &message)
                        .unwrap(),
                );
                assert!(hop.success, "{contract:?} {status}: {hop:?}");
                assert_eq!(hop.out.len(), 3, "{contract:?} {status}");
                assert!(hop.fees > 0 && hop.gas_fees > 0, "{hop:?}");
                let poor = account(&ton, contract, status, 120_000_000);
                let (state, message) = rehearsal(&plan.batch, &poor, u64::from(NOW)).unwrap();
                let hop = done(
                    emulator
                        .run_external(plan.from(), &state, &message)
                        .unwrap(),
                );
                assert!(
                    hop.out.len() < 3,
                    "{contract:?} {status}: a message the balance can't cover is left out"
                );
            }
        }
    }

    #[test]
    fn an_account_shows_an_app_its_key_and_contract_and_signs_hashes() {
        let ton = chain("ton");
        for contract in [TonContract::W5, TonContract::V4R2] {
            let from = ton.derive_as(&seed(), 0, contract).unwrap();
            let identity = ton.identity(&seed(), 0, &from).unwrap();
            let init = crate::cells::root(&identity.state_init).unwrap();
            let hash: [u8; 32] = Cell::cell_hash(&init).as_slice().try_into().unwrap();
            assert_eq!(from, Address::Ton { workchain: 0, hash }, "{contract:?}");
            let signature = ton.sign_hash(&seed(), 0, &from, &[9; 32]).unwrap();
            ed25519_dalek::VerifyingKey::from_bytes(&identity.public_key)
                .unwrap()
                .verify_strict(&[9; 32], &ed25519_dalek::Signature::from_bytes(&signature))
                .unwrap();
            assert!(ton.identity(&seed(), 1, &from).is_err());
            assert!(ton.sign_hash(&seed(), 1, &from, &[9; 32]).is_err());
        }
    }

    #[test]
    fn token_and_nft_bodies_name_their_recipient() {
        let recipient = party(0x44);
        let token = crate::Body::default()
            .uint(32, u64::from(TOKEN_TRANSFER))
            .unwrap()
            .uint(64, 1)
            .unwrap()
            .coins(U256::from(5_000_000u64))
            .unwrap()
            .address(Some(&recipient))
            .unwrap()
            .boc()
            .unwrap();
        let root = crate::cells::root(&token).unwrap();
        assert_eq!(
            token_transfer(&root),
            Some((U256::from(5_000_000u64), recipient))
        );
        let nft = crate::Body::default()
            .uint(32, u64::from(NFT_TRANSFER))
            .unwrap()
            .uint(64, 1)
            .unwrap()
            .address(Some(&recipient))
            .unwrap()
            .boc()
            .unwrap();
        assert_eq!(
            nft_transfer(&crate::cells::root(&nft).unwrap()),
            Some(recipient)
        );
        let short = crate::Body::default()
            .uint(32, u64::from(TOKEN_TRANSFER))
            .unwrap()
            .boc()
            .unwrap();
        assert_eq!(token_transfer(&crate::cells::root(&short).unwrap()), None);
    }

    #[test]
    fn a_relayer_can_deliver_a_signed_w5_request() {
        use num_bigint::BigUint;
        use tonlib_core::tlb_types::block::coins::CurrencyCollection as Coins;
        use tonlib_core::tlb_types::block::message::{CommonMsgInfo, Message};
        let ton = chain("ton");
        let emulator = emulate::tests::emulator();
        let relayer = party(0x55);
        for deploy in [false, true] {
            let plan = plan(&ton, TonContract::W5, (deploy, 0), messages());
            let signed = ton.sign_internal(&plan, &seed()).unwrap();
            let root = crate::cells::root(&signed).unwrap();
            let mut message = Message::from_cell(&root).unwrap();
            assert_eq!(message.init.is_some(), deploy);
            let CommonMsgInfo::Int(info) = &mut message.info else {
                panic!("a signed request is an internal message");
            };
            assert_eq!(
                crate::cells::from_ton(
                    &tonlib_core::TonAddress::from_msg_address(info.dest.clone()).unwrap()
                ),
                Some(*plan.from())
            );
            info.src = wallet::to_ton(&relayer)
                .unwrap()
                .to_msg_address_int()
                .into();
            info.value = Coins::new(BigUint::from(500_000_000u64));
            let delivered = serialized(&message.to_cell().unwrap()).unwrap();
            let state = stored(&ton, plan.from(), TonContract::W5, deploy);
            let hop = done(emulator.run(plan.from(), &state, &delivered).unwrap());
            assert!(hop.success, "deploy {deploy}: {hop:?}");
            assert_eq!(hop.out.len(), 3, "deploy {deploy}");
        }
        let v4 = plan(&ton, TonContract::V4R2, (false, 0), messages());
        assert!(matches!(
            ton.sign_internal(&v4, &seed()),
            Err(ChainError::Unsupported(_))
        ));
    }
}
