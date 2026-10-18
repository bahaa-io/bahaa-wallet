use std::time::{SystemTime, UNIX_EPOCH};

use bahaa_chain::{ChainError, Decoded, FeeItem, FeeLine, FeeQuote, Signed, Transfer, TxId};
use bahaa_crypto::Seed;
use bahaa_types::{Address, AssetId};
use ed25519_dalek::{Signature, Signer};
use ruint::aliases::U256;
use tonlib_core::cell::BagOfCells;

use super::{TonChain, amount, key};
use crate::cells::{Account, Status};
use crate::fees::{Settings, TRANSFER_GAS, size_without_root};
use crate::wallet::{self, Order};

pub(super) const LIFETIME: u64 = 300;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TonPlan {
    pub(super) transfer: Transfer,
    pub(super) order: Order,
    pub(super) deploy: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Estimate {
    pub compute: u64,
    pub storage: u64,
    pub forwarding: u64,
}

impl Estimate {
    fn expected(&self) -> Option<u64> {
        self.compute
            .checked_add(self.storage)?
            .checked_add(self.forwarding)
    }
}

impl TonChain {
    pub(super) async fn plan(
        &self,
        transfer: &Transfer,
    ) -> Result<(TonPlan, FeeQuote), ChainError> {
        self.sendable(transfer)?;
        let wallet = self.http.account(&transfer.from).await?;
        let (seqno, deploy) = self.sequence(&wallet)?;
        let recipient = self.http.account(&transfer.to).await?;
        let bounce = bounces(&recipient.status);
        let settings = self.http.settings().await?;
        let now = now();
        let order = Order {
            wallet: transfer.from,
            wallet_id: self.wallet_id,
            seqno,
            valid_until: u32::try_from(now + LIFETIME)
                .map_err(|_| ChainError::Invalid("the clock is out of range"))?,
            to: transfer.to,
            nano: transfer.amount.units(),
            bounce,
        };
        let fees = estimate(&order, deploy, &wallet, &settings, now)?;
        let expected = fees.expected().ok_or_else(overflow)?;
        let max = expected.checked_mul(2).ok_or_else(overflow)?;
        let needed = transfer
            .amount
            .units()
            .checked_add(U256::from(max))
            .ok_or_else(overflow)?;
        let decimals = self.network.native.decimals;
        if wallet.balance < needed {
            return Err(ChainError::Insufficient {
                symbol: self.network.native.symbol.clone(),
                needed: amount(needed, decimals)?,
                available: amount(wallet.balance, decimals)?,
            });
        }
        let gram = |nano: u64| amount(U256::from(nano), decimals);
        let lines = vec![
            FeeLine {
                item: FeeItem::Compute { gas: TRANSFER_GAS },
                amount: gram(fees.compute)?,
            },
            FeeLine {
                item: FeeItem::Storage,
                amount: gram(fees.storage)?,
            },
            FeeLine {
                item: FeeItem::Forwarding,
                amount: gram(fees.forwarding)?,
            },
        ];
        Ok((
            TonPlan {
                transfer: transfer.clone(),
                order,
                deploy,
            },
            FeeQuote::new(gram(max)?, gram(expected)?, lines)?,
        ))
    }

    fn sendable(&self, transfer: &Transfer) -> Result<(), ChainError> {
        if transfer.asset.network() != &self.network.id {
            return Err(ChainError::Invalid("the asset belongs to another network"));
        }
        if transfer.asset.contract().is_some() {
            return Err(ChainError::Unsupported("sending TON tokens yet"));
        }
        if transfer.amount.is_zero() {
            return Err(ChainError::Invalid("send more than zero"));
        }
        if transfer.amount.decimals() != self.network.native.decimals {
            return Err(ChainError::Invalid(
                "the amount uses the wrong number of decimals",
            ));
        }
        let ton = |address: &Address| matches!(address, Address::Ton { .. });
        if !ton(&transfer.from) || !ton(&transfer.to) {
            return Err(ChainError::Invalid("not a TON address"));
        }
        Ok(())
    }

    fn sequence(&self, wallet: &Account) -> Result<(u32, bool), ChainError> {
        match &wallet.status {
            Status::Missing | Status::Uninit => Ok((0, true)),
            Status::Frozen => Err(ChainError::Invalid("this wallet is frozen and cannot send")),
            Status::Active { code, data } => {
                if Some(*code) != wallet::v5r1_code_hash() {
                    return Err(ChainError::Unsupported(
                        "sending from wallets other than V5R1 yet",
                    ));
                }
                let data = data
                    .as_ref()
                    .and_then(wallet::wallet_data)
                    .ok_or(ChainError::Invalid("the wallet's data is unreadable"))?;
                if data.wallet_id != self.wallet_id || !data.signature_allowed {
                    return Err(ChainError::Invalid(
                        "this wallet does not take signed transfers for this network",
                    ));
                }
                Ok((data.seqno, false))
            }
        }
    }

    pub(super) fn sign_plan(&self, plan: &TonPlan, seed: &Seed) -> Result<Signed, ChainError> {
        let key = key(seed, plan.transfer.account)?;
        let public = key.verifying_key().to_bytes();
        if wallet::v5r1(&public, self.wallet_id)? != plan.transfer.from
            || plan.order.wallet != plan.transfer.from
        {
            return Err(ChainError::Invalid(
                "this seed does not own the sending account",
            ));
        }
        let digest = wallet::unsigned(&plan.order)?.cell_hash();
        let signature = key.sign(digest.as_slice());
        key.verifying_key()
            .verify_strict(digest.as_slice(), &signature)
            .map_err(|_| ChainError::Invalid("signing failed"))?;
        let message = wallet::external(
            &plan.order,
            &signature.to_bytes(),
            plan.deploy.then_some(&public),
        )?;
        let id = wallet::normalized(&message)
            .map(|id| hex(&id))
            .ok_or(ChainError::Invalid("cannot encode the wallet message"))?;
        let bytes = BagOfCells::from_root(message)
            .serialize(true)
            .map_err(|_| ChainError::Invalid("cannot encode the wallet message"))?;
        Ok(Signed {
            id: TxId::new(id)?,
            bytes,
        })
    }

    pub(super) fn decode_signed(&self, signed: &Signed) -> Result<Decoded, ChainError> {
        let parsed = wallet::parse(&signed.bytes)?;
        if hex(&parsed.id) != signed.id.as_str() {
            return Err(ChainError::Mismatch("the transaction id"));
        }
        if parsed.order.wallet_id != self.wallet_id {
            return Err(ChainError::Mismatch("the network"));
        }
        if let Some(public) = parsed.public_key {
            let key = ed25519_dalek::VerifyingKey::from_bytes(&public)
                .map_err(|_| ChainError::Invalid("the deployment has an unusable key"))?;
            key.verify_strict(&parsed.unsigned, &Signature::from_bytes(&parsed.signature))
                .map_err(|_| ChainError::Invalid("the signature does not match the wallet"))?;
            if wallet::v5r1(&public, self.wallet_id)? != parsed.order.wallet {
                return Err(ChainError::Invalid("the deployment is for another wallet"));
            }
        }
        Ok(Decoded {
            from: parsed.order.wallet,
            to: parsed.order.to,
            asset: AssetId::Native(self.network.id.clone()),
            amount: amount(parsed.order.nano, self.network.native.decimals)?,
            sequence: Some(u64::from(parsed.order.seqno)),
            expires_at: Some(u64::from(parsed.order.valid_until)),
        })
    }
}

fn bounces(status: &Status) -> bool {
    !matches!(status, Status::Uninit | Status::Missing)
}

pub(crate) fn estimate(
    order: &Order,
    deploy: bool,
    wallet: &Account,
    settings: &Settings,
    now: u64,
) -> Result<Estimate, ChainError> {
    let external = wallet::external(order, &[0; 64], deploy.then_some(&[0; 32]))?;
    let import = settings
        .forward_fee(size_without_root(&external))
        .ok_or_else(overflow)?;
    let forward = settings
        .forward_fee(wallet::transfer_size(order)?)
        .ok_or_else(overflow)?;
    let elapsed = now.saturating_sub(u64::from(wallet.last_paid));
    let due = u64::try_from(wallet.due).map_err(|_| overflow())?;
    let storage = settings
        .storage_fee((wallet.bits, wallet.cells), elapsed)
        .and_then(|fee| fee.checked_add(due))
        .ok_or_else(overflow)?;
    Ok(Estimate {
        compute: settings.gas_fee(TRANSFER_GAS).ok_or_else(overflow)?,
        storage,
        forwarding: import.checked_add(forward).ok_or_else(overflow)?,
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
        out
    })
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

fn overflow() -> ChainError {
    ChainError::Invalid("fee overflow")
}

#[cfg(test)]
mod tests {
    use bahaa_chain::Chain;
    use bahaa_config::Registry;
    use bahaa_crypto::Mnemonic;
    use bahaa_types::Amount;
    use num_bigint::BigUint;
    use tonlib_core::cell::{Cell, CellBuilder};
    use tonlib_core::tlb_types::block::coins::{CurrencyCollection, Grams};
    use tonlib_core::tlb_types::block::message::{
        CommonMsgInfo, ExtInMsgInfo, IntMsgInfo, Message,
    };
    use tonlib_core::tlb_types::block::msg_address::{MsgAddrNone, MsgAddressExt};
    use tonlib_core::tlb_types::tlb::TLB;
    use tonlib_core::wallet::version_helper::VersionHelper;
    use tonlib_core::wallet::versioned::v5::WalletExtMsgBodyV5;
    use tonlib_core::wallet::wallet_version::WalletVersion;
    use tonlib_core::{TonAddress, TonHash};

    use super::*;

    fn chain(id: &str) -> TonChain {
        let registry = Registry::builtin().unwrap();
        TonChain::new(registry.get(&id.parse().unwrap()).unwrap().clone()).unwrap()
    }

    fn seed() -> Seed {
        Mnemonic::parse("test test test test test test test test test test test junk")
            .unwrap()
            .to_seed("")
    }

    fn plan(chain: &TonChain, deploy: bool) -> TonPlan {
        let from = chain.derive(&seed(), 0).unwrap();
        let to = Address::Ton {
            workchain: 0,
            hash: [0x5c; 32],
        };
        let amount = Amount::parse("1.25", 9).unwrap();
        TonPlan {
            transfer: Transfer {
                account: 0,
                from,
                to,
                asset: "ton".parse().unwrap(),
                amount,
            },
            order: Order {
                wallet: from,
                wallet_id: chain.wallet_id,
                seqno: if deploy { 0 } else { 7 },
                valid_until: 1_792_400_000,
                to,
                nano: amount.units(),
                bounce: !deploy,
            },
            deploy,
        }
    }

    #[test]
    fn signed_transfers_decode_back_to_what_was_planned() {
        let ton = chain("ton");
        for deploy in [false, true] {
            let plan = plan(&ton, deploy);
            let signed = ton.sign(&plan, &seed()).unwrap();
            let decoded = ton.decode(&signed).unwrap();
            bahaa_chain::check_signed(&plan.transfer, &decoded).unwrap();
            assert_eq!(
                (decoded.sequence, decoded.expires_at),
                (Some(u64::from(plan.order.seqno)), Some(1_792_400_000))
            );
            let parsed = wallet::parse(&signed.bytes).unwrap();
            assert_eq!(parsed.order, plan.order);
            assert_eq!(parsed.public_key.is_some(), deploy);
        }
    }

    #[test]
    fn foreign_seeds_networks_and_ids_are_refused() {
        let ton = chain("ton");
        let stranger = Mnemonic::from_entropy(&[1; 16]).unwrap().to_seed("");
        assert!(matches!(
            ton.sign(&plan(&ton, false), &stranger),
            Err(ChainError::Invalid(_))
        ));
        let signed = ton.sign(&plan(&ton, true), &seed()).unwrap();
        assert_eq!(
            chain("ton-testnet").decode(&signed),
            Err(ChainError::Mismatch("the network"))
        );
        let mut renamed = signed.clone();
        renamed.id = TxId::new("ab".repeat(32)).unwrap();
        assert_eq!(
            ton.decode(&renamed),
            Err(ChainError::Mismatch("the transaction id"))
        );
        let mut tampered = signed.clone();
        let at = tampered.bytes.len() - 20;
        tampered.bytes[at] ^= 1;
        assert!(ton.decode(&tampered).is_err());
    }

    fn with_body(body: &WalletExtMsgBodyV5, wallet: &Address) -> Vec<u8> {
        let Address::Ton { hash, .. } = wallet else {
            panic!("not a TON wallet");
        };
        let unsigned = body.to_cell().unwrap();
        let signed = VersionHelper::sign_msg(WalletVersion::V5R1, &unsigned, &[0; 64]).unwrap();
        let message = Message::new(
            CommonMsgInfo::ExtIn(ExtInMsgInfo {
                src: MsgAddressExt::None(MsgAddrNone {}),
                dest: TonAddress::new(0, TonHash::from(*hash)).to_msg_address_int(),
                import_fee: Grams::new(BigUint::default()),
            }),
            signed.to_arc(),
        );
        BagOfCells::from_root(message.to_cell().unwrap())
            .serialize(true)
            .unwrap()
    }

    fn transfer(bounce: bool, body: Cell) -> tonlib_core::cell::ArcCell {
        Message::new(
            CommonMsgInfo::Int(IntMsgInfo {
                ihr_disabled: true,
                bounce,
                bounced: false,
                src: MsgAddrNone {}.into(),
                dest: TonAddress::new(0, TonHash::from([0x5c; 32]))
                    .to_msg_address_int()
                    .into(),
                value: CurrencyCollection::new(BigUint::from(5u8)),
                ihr_fee: Grams::new(BigUint::default()),
                fwd_fee: Grams::new(BigUint::default()),
                created_lt: 0,
                created_at: 0,
            }),
            body.to_arc(),
        )
        .to_cell()
        .unwrap()
        .to_arc()
    }

    fn account_with(status: Status) -> Account {
        Account {
            balance: U256::from(5u8),
            bits: 0,
            cells: 0,
            last_paid: 0,
            due: U256::ZERO,
            status,
            last: None,
        }
    }

    #[test]
    fn the_wallet_state_decides_seqno_deployment_and_bounce() {
        let ton = chain("ton");
        let data = |wallet_id: i32, seqno: u32| {
            let mut cell = CellBuilder::new();
            cell.store_bit(true).unwrap().store_u32(32, seqno).unwrap();
            cell.store_i32(32, wallet_id)
                .unwrap()
                .store_slice(&[7; 32])
                .unwrap();
            cell.store_bit(false).unwrap();
            Some(cell.build().unwrap().to_arc())
        };
        let v5r1 = wallet::v5r1_code_hash().unwrap();
        let active = |code: [u8; 32], data| account_with(Status::Active { code, data });
        assert_eq!(ton.sequence(&account_with(Status::Missing)), Ok((0, true)));
        assert_eq!(ton.sequence(&account_with(Status::Uninit)), Ok((0, true)));
        assert!(matches!(
            ton.sequence(&account_with(Status::Frozen)),
            Err(ChainError::Invalid(_))
        ));
        assert_eq!(
            ton.sequence(&active(v5r1, data(ton.wallet_id, 41))),
            Ok((41, false))
        );
        assert!(matches!(
            ton.sequence(&active([1; 32], data(ton.wallet_id, 41))),
            Err(ChainError::Unsupported(_))
        ));
        assert!(matches!(
            ton.sequence(&active(v5r1, data(ton.wallet_id ^ 1, 41))),
            Err(ChainError::Invalid(_))
        ));
        assert!(matches!(
            ton.sequence(&active(v5r1, None)),
            Err(ChainError::Invalid(_))
        ));
        assert!(!bounces(&Status::Missing));
        assert!(!bounces(&Status::Uninit));
        assert!(bounces(&Status::Frozen));
        assert!(bounces(&Status::Active {
            code: v5r1,
            data: None
        }));
    }

    #[test]
    fn a_deployment_must_be_signed_by_the_key_it_deploys() {
        let ton = chain("ton");
        let mut signed = ton.sign(&plan(&ton, true), &seed()).unwrap();
        let mut parsed = wallet::parse(&signed.bytes).unwrap();
        parsed.signature[0] ^= 1;
        let message =
            wallet::external(&parsed.order, &parsed.signature, parsed.public_key.as_ref()).unwrap();
        signed.id = TxId::new(hex(&wallet::normalized(&message).unwrap())).unwrap();
        signed.bytes = BagOfCells::from_root(message).serialize(true).unwrap();
        assert!(matches!(ton.decode(&signed), Err(ChainError::Invalid(_))));
    }

    #[test]
    fn a_deployment_must_be_for_the_wallet_it_is_sent_to() {
        let ton = chain("ton");
        let ours = plan(&ton, true);
        let stranger = key(&Mnemonic::from_entropy(&[9; 16]).unwrap().to_seed(""), 0).unwrap();
        let theirs = stranger.verifying_key().to_bytes();
        let digest = wallet::unsigned(&ours.order).unwrap().cell_hash();
        let signature = stranger.sign(digest.as_slice()).to_bytes();
        let message = wallet::external(&ours.order, &signature, Some(&theirs)).unwrap();
        let signed = Signed {
            id: TxId::new(hex(&wallet::normalized(&message).unwrap())).unwrap(),
            bytes: BagOfCells::from_root(message).serialize(true).unwrap(),
        };
        assert!(matches!(ton.decode(&signed), Err(ChainError::Invalid(_))));
    }

    #[test]
    fn only_a_single_plain_transfer_in_mode_three_is_accepted() {
        let ton = chain("ton");
        let wallet = ton.derive(&seed(), 0).unwrap();
        let empty = CellBuilder::new().build().unwrap();
        let mut note = CellBuilder::new();
        note.store_u32(32, 0).unwrap().store_slice(b"hi").unwrap();
        let note = note.build().unwrap();
        let body = |msgs: Vec<tonlib_core::cell::ArcCell>, modes: Vec<u8>| WalletExtMsgBodyV5 {
            wallet_id: ton.wallet_id,
            valid_until: 1_792_400_000,
            msg_seqno: 1,
            msgs_modes: modes,
            msgs,
        };
        let plain = with_body(&body(vec![transfer(true, empty.clone())], vec![3]), &wallet);
        assert!(wallet::parse(&plain).is_ok());
        let cases = [
            body(
                vec![transfer(true, empty.clone()), transfer(true, empty.clone())],
                vec![3, 3],
            ),
            body(vec![transfer(true, empty.clone())], vec![128]),
            body(vec![transfer(true, note)], vec![3]),
            body(Vec::new(), Vec::new()),
        ];
        for case in cases {
            assert!(wallet::parse(&with_body(&case, &wallet)).is_err());
        }
    }
}
