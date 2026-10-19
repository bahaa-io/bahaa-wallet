#![allow(clippy::unwrap_used, clippy::unused_async_trait_impl)]

use std::collections::HashMap;
use std::fmt::Write;
use std::sync::Mutex;
use std::time::Duration;

use bahaa_chain::{
    Balance, Chain, ChainError, Decoded, FeeItem, FeeLine, FeeQuote, PoolError, Signed, Transfer,
    TxId, TxStatus, check_signed,
};
use bahaa_config::{Coin, Network, Registry};
use bahaa_crypto::{Mnemonic, Seed};
use bahaa_types::{Address, Amount, AssetId};

const HARDHAT: &str = "test test test test test test test test test test test junk";
const GAS: u64 = 21_000;

struct FakeChain {
    network: Network,
    ledger: Mutex<HashMap<Address, Amount>>,
    sent: Mutex<HashMap<TxId, Amount>>,
}

struct Plan {
    transfer: Transfer,
    fee: Amount,
}

fn native(text: &str) -> Amount {
    Amount::parse(text, 18).unwrap()
}

impl FakeChain {
    fn new() -> Self {
        let registry = Registry::builtin().unwrap();
        let network = registry.get(&"ethereum".parse().unwrap()).unwrap().clone();
        Self {
            network,
            ledger: Mutex::new(HashMap::new()),
            sent: Mutex::new(HashMap::new()),
        }
    }

    fn fund(&self, address: Address, amount: Amount) {
        self.ledger.lock().unwrap().insert(address, amount);
    }

    fn balance(&self, address: &Address) -> Amount {
        self.ledger
            .lock()
            .unwrap()
            .get(address)
            .copied()
            .unwrap_or(native("0"))
    }
}

impl Chain for FakeChain {
    type Plan = Plan;

    fn network(&self) -> &Network {
        &self.network
    }

    fn derive(&self, seed: &Seed, account: u32) -> Result<Address, ChainError> {
        let mut bytes = [0u8; 20];
        bytes.copy_from_slice(&seed.expose()[..20]);
        bytes[..4].copy_from_slice(&account.to_be_bytes());
        Ok(Address::Evm(bytes))
    }

    async fn balances(
        &self,
        address: &Address,
        assets: &[AssetId],
    ) -> Result<Vec<Balance>, ChainError> {
        Ok(assets
            .iter()
            .map(|asset| Balance {
                asset: asset.clone(),
                amount: self.balance(address),
                staked: None,
            })
            .collect())
    }

    async fn prepare(&self, transfer: &Transfer) -> Result<(Plan, FeeQuote), ChainError> {
        if transfer.asset.contract().is_some() {
            return Err(ChainError::Unsupported("tokens"));
        }
        let price = Amount::parse("0.000000001", 18).unwrap();
        let fee = native("0.000021");
        let quote = FeeQuote::new(
            fee,
            fee,
            vec![FeeLine {
                item: FeeItem::Gas { units: GAS, price },
                amount: fee,
            }],
        )?;
        let available = self.balance(&transfer.from);
        let mut transfer = transfer.clone();
        if transfer.max {
            transfer.amount = available.checked_sub(fee).unwrap_or(fee);
        }
        let needed = transfer.amount.checked_add(fee).unwrap();
        if available < needed || transfer.amount.is_zero() {
            return Err(ChainError::Insufficient {
                symbol: self.network.native.symbol.clone(),
                needed,
                available,
            });
        }
        Ok((Plan { transfer, fee }, quote))
    }

    fn planned<'a>(&self, plan: &'a Plan) -> &'a Transfer {
        &plan.transfer
    }

    async fn replace(
        &self,
        _original: &Signed,
        _account: u32,
        _how: bahaa_chain::Replace,
    ) -> Result<(Plan, FeeQuote), ChainError> {
        Err(ChainError::Unsupported("replacing a transfer"))
    }

    fn sign(&self, plan: &Plan, seed: &Seed) -> Result<Signed, ChainError> {
        if self.derive(seed, plan.transfer.account)? != plan.transfer.from {
            return Err(ChainError::Invalid(
                "this seed does not own the sending account",
            ));
        }
        let (Address::Evm(from), Address::Evm(to)) = (plan.transfer.from, plan.transfer.to) else {
            return Err(ChainError::Invalid("not an EVM address"));
        };
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&from);
        bytes.extend_from_slice(&to);
        bytes.extend_from_slice(&plan.transfer.amount.units().to_be_bytes::<32>());
        let hex = bytes[..8].iter().fold(String::new(), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        });
        let id = TxId::new(hex)?;
        self.sent.lock().unwrap().insert(id.clone(), plan.fee);
        Ok(Signed { id, bytes })
    }

    fn decode(&self, signed: &Signed) -> Result<Decoded, ChainError> {
        let bad = || ChainError::Invalid("not a fake transaction");
        let from: [u8; 20] = signed
            .bytes
            .get(..20)
            .ok_or_else(bad)?
            .try_into()
            .map_err(|_| bad())?;
        let to: [u8; 20] = signed
            .bytes
            .get(20..40)
            .ok_or_else(bad)?
            .try_into()
            .map_err(|_| bad())?;
        let units: [u8; 32] = signed
            .bytes
            .get(40..72)
            .ok_or_else(bad)?
            .try_into()
            .map_err(|_| bad())?;
        Ok(Decoded {
            from: Address::Evm(from),
            to: Address::Evm(to),
            asset: AssetId::Native(self.network.id.clone()),
            amount: Amount::new(ruint::aliases::U256::from_be_bytes(units), 18)
                .map_err(|_| bad())?,
            everything: false,
            sequence: None,
            expires_at: None,
            memo: None,
        })
    }

    async fn broadcast(&self, signed: &Signed) -> Result<TxId, ChainError> {
        let decoded = self.decode(signed)?;
        let fee = *self
            .sent
            .lock()
            .unwrap()
            .get(&signed.id)
            .ok_or(ChainError::Rejected("unknown".to_owned()))?;
        let mut ledger = self.ledger.lock().unwrap();
        let from = ledger.get(&decoded.from).copied().unwrap_or(native("0"));
        let spent = decoded.amount.checked_add(fee).unwrap();
        ledger.insert(
            decoded.from,
            from.checked_sub(spent)
                .map_err(|_| ChainError::Rejected("insufficient funds".to_owned()))?,
        );
        let to = ledger.get(&decoded.to).copied().unwrap_or(native("0"));
        ledger.insert(decoded.to, to.checked_add(decoded.amount).unwrap());
        Ok(signed.id.clone())
    }

    async fn status(
        &self,
        id: &TxId,
        _from: &Address,
        _expires_at: Option<u64>,
    ) -> Result<TxStatus, ChainError> {
        Ok(match self.sent.lock().unwrap().get(id) {
            Some(fee) => TxStatus::Confirmed {
                block: Some(1),
                fee: Some(*fee),
                transaction: None,
            },
            None => TxStatus::NotFound,
        })
    }

    async fn token(&self, _contract: &Address) -> Result<Coin, ChainError> {
        Err(ChainError::Unsupported("tokens"))
    }

    fn know(&self, _contract: &Address, _decimals: u8) {}
}

async fn send<C: Chain>(chain: &C, transfer: &Transfer, seed: &Seed) -> Result<TxId, ChainError> {
    let (plan, _quote) = chain.prepare(transfer).await?;
    let signed = chain.sign(&plan, seed)?;
    check_signed(chain.planned(&plan), &chain.decode(&signed)?)?;
    chain.broadcast(&signed).await
}

fn assert_send<T: Send>(value: T) -> T {
    value
}

fn setup() -> (FakeChain, Seed, Transfer) {
    let chain = FakeChain::new();
    let seed = Mnemonic::parse(HARDHAT).unwrap().to_seed("");
    let from = chain.derive(&seed, 0).unwrap();
    let to = chain.derive(&seed, 1).unwrap();
    chain.fund(from, native("1"));
    let transfer = Transfer {
        account: 0,
        from,
        to,
        asset: AssetId::Native(chain.network().id.clone()),
        amount: native("0.25"),
        max: false,
        memo: None,
    };
    (chain, seed, transfer)
}

#[tokio::test]
async fn a_transfer_goes_through_every_step() {
    let (chain, seed, transfer) = setup();
    let (plan, quote) = chain.prepare(&transfer).await.unwrap();
    assert_eq!(quote.max().to_string(), "0.000021");
    assert!(matches!(
        quote.lines()[0].item,
        FeeItem::Gas { units: GAS, .. }
    ));
    let signed = chain.sign(&plan, &seed).unwrap();
    let decoded = chain.decode(&signed).unwrap();
    check_signed(&transfer, &decoded).unwrap();
    let id = chain.broadcast(&signed).await.unwrap();
    assert!(matches!(
        chain.status(&id, &transfer.from, None).await.unwrap(),
        TxStatus::Confirmed { block: Some(1), .. }
    ));
    let balances = chain
        .balances(&transfer.from, std::slice::from_ref(&transfer.asset))
        .await
        .unwrap();
    assert_eq!(balances[0].amount.to_string(), "0.749979");
    assert_eq!(chain.balance(&transfer.to).to_string(), "0.25");
}

#[tokio::test]
async fn any_chain_works_through_the_same_generic_code() {
    let (chain, seed, transfer) = setup();
    let id = assert_send(send(&chain, &transfer, &seed)).await.unwrap();
    assert_eq!(
        chain.status(&id, &transfer.from, None).await.unwrap(),
        TxStatus::Confirmed {
            block: Some(1),
            fee: Some(native("0.000021")),
            transaction: None,
        }
    );
}

#[tokio::test]
async fn refuses_to_spend_more_than_the_balance() {
    let (chain, _, mut transfer) = setup();
    transfer.amount = native("1");
    let err = chain.prepare(&transfer).await.err().unwrap();
    assert_eq!(
        err.to_string(),
        "not enough ETH: this needs 1.000021, the balance is 1"
    );
}

#[tokio::test]
async fn a_signed_transaction_that_differs_from_the_request_is_caught() {
    let (chain, seed, transfer) = setup();
    let (plan, _) = chain.prepare(&transfer).await.unwrap();
    let decoded = chain.decode(&chain.sign(&plan, &seed).unwrap()).unwrap();
    let wrong_amount = Decoded {
        amount: native("2.5"),
        ..decoded.clone()
    };
    assert_eq!(
        check_signed(&transfer, &wrong_amount),
        Err(ChainError::Mismatch("the amount"))
    );
    let wrong_recipient = Decoded {
        to: transfer.from,
        ..decoded.clone()
    };
    assert_eq!(
        check_signed(&transfer, &wrong_recipient),
        Err(ChainError::Mismatch("the recipient"))
    );
    let wrong_asset = Decoded {
        asset: "bsc".parse().unwrap(),
        ..decoded
    };
    assert_eq!(
        check_signed(&transfer, &wrong_asset),
        Err(ChainError::Mismatch("the asset"))
    );
    let sweeping = Decoded {
        everything: true,
        ..chain.decode(&chain.sign(&plan, &seed).unwrap()).unwrap()
    };
    assert_eq!(
        check_signed(&transfer, &sweeping),
        Err(ChainError::Mismatch("the amount"))
    );
    let asked = Transfer {
        max: true,
        ..transfer
    };
    assert_eq!(check_signed(&asked, &sweeping), Ok(()));
}

#[tokio::test]
async fn a_max_transfer_sends_the_balance_less_the_fee() {
    let (chain, seed, mut transfer) = setup();
    transfer.max = true;
    transfer.amount = Amount::default();
    let (plan, quote) = chain.prepare(&transfer).await.unwrap();
    let planned = chain.planned(&plan);
    assert_eq!(planned.amount, native("0.999979"));
    assert_eq!(planned.amount.checked_add(quote.max()), Ok(native("1")));
    assert!(planned.max);
    send(&chain, &transfer, &seed).await.unwrap();
    assert_eq!(chain.balance(&transfer.from), native("0"));
    assert!(matches!(
        chain.prepare(&transfer).await,
        Err(ChainError::Insufficient { .. })
    ));
}

#[tokio::test]
async fn only_the_owning_seed_can_sign() {
    let (chain, _, transfer) = setup();
    let (plan, _) = chain.prepare(&transfer).await.unwrap();
    let stranger = Mnemonic::from_entropy(&[9; 16]).unwrap().to_seed("");
    assert!(matches!(
        chain.sign(&plan, &stranger),
        Err(ChainError::Invalid(_))
    ));
}

#[test]
fn fee_quotes_must_add_up() {
    let line = |text: &str| FeeLine {
        item: FeeItem::Network,
        amount: native(text),
    };
    assert!(FeeQuote::new(native("2"), native("1.5"), vec![line("1"), line("0.5")]).is_ok());
    assert!(FeeQuote::new(native("2"), native("1.5"), vec![line("1")]).is_err());
    assert!(FeeQuote::new(native("1"), native("1.5"), vec![line("1.5")]).is_err());
    let other = Amount::parse("1", 6).unwrap();
    assert!(FeeQuote::new(other, native("1"), vec![line("1")]).is_err());
}

#[test]
fn transaction_ids_are_plain() {
    for text in [
        "a2c0feb8b0139faba029fa15cc8433f81d5dfbf65627760e43b73270f574c599",
        "xJVJ1Tn1WJqpKw8rBAKKjhwsU7kYGE9YbIbgxyO141U=",
        "0x3c4e20",
    ] {
        assert_eq!(TxId::new(text).unwrap().as_str(), text);
    }
    for text in [
        "",
        "abc def",
        "abc\u{1b}[31m",
        "abc\u{202e}",
        &"a".repeat(129),
    ] {
        assert!(TxId::new(text).is_err(), "{text:?}");
    }
}

#[test]
fn pool_errors_become_chain_errors() {
    let fatal: ChainError =
        PoolError::Fatal(ChainError::Rejected("nonce too low".to_owned())).into();
    assert_eq!(fatal, ChainError::Rejected("nonce too low".to_owned()));
    let exhausted: ChainError =
        PoolError::Exhausted(ChainError::Network("timeout".to_owned())).into();
    assert!(matches!(exhausted, ChainError::Network(_)));
    let unserved: ChainError =
        PoolError::Unserved(ChainError::Network("archive only".to_owned())).into();
    assert_eq!(
        unserved,
        ChainError::Network("network problem: archive only".to_owned())
    );
    let waiting: ChainError = PoolError::<ChainError>::Unavailable(Duration::from_secs(12)).into();
    assert!(waiting.to_string().contains("12 s"));
}
