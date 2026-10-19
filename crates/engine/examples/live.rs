#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::error::Error;
use std::fs;
use std::io::{self, BufRead, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bahaa_config::{Network, Registry};
use bahaa_crypto::{ExposeSecret, SecretString};
use bahaa_engine::{
    Account, CustomToken, Draft, Engine, EngineConfig, Event, NewPhrase, PhraseKind, SendRequest,
    SentStatus, SentTx, Spend, WalletId,
};
use bahaa_market::Status;
use bahaa_types::{Address, Amount, AssetId, ChainFamily, NetworkId, TonContract, TonForm};
use tokio::sync::broadcast::{self, error::RecvError};

type Outcome<T = ()> = Result<T, Box<dyn Error>>;

const DIR: &str = ".bahaa-live";
const PASSWORD: &str = "password";
const VAULT: &str = "vault.bin";
const LETTERS: &str = "abcdefgh";
const SETTLE: Duration = Duration::from_mins(20);
const FRESH: Duration = Duration::from_mins(4);
const PRICES: Duration = Duration::from_secs(30);
const LONGEST_LOCK: u16 = 1_440;

const USAGE: &str = "commands:
  create                              new test vault with wallets a and b
  addresses                           every wallet's receiving addresses
  status                              refresh, then balances, sends and receipts
  send <wallet> <asset> <to> <amount|max>
                                      quote, sign, broadcast and wait until settled
  watch <seconds>                     print what the engine reports
  track                               wait until every open send is settled
  testnets <on|off>                   include the test networks
  tokens | token add <asset> | token remove <asset>
  ton-contract <wallet> <w5|v4r2>     choose the contract of a TON phrase
  reveal <wallet>                     print a phrase, run it in your own terminal only
  import [bip39|ton]                  read a phrase from the keyboard
assets look like bsc, bsc/USDT or bsc/0x55d3...; <to> is a wallet letter or an address";

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(err) = run(&args).await {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

async fn run(args: &[String]) -> Outcome {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    if words == ["create"] {
        return create().await;
    }
    let live = Live::open().await?;
    let result = match words.as_slice() {
        ["addresses"] => live.addresses().await,
        ["status"] => live.status().await,
        ["send", from, asset, to, amount] => live.send(from, asset, to, amount).await,
        ["watch", seconds] => live.watch(Duration::from_secs(seconds.parse()?)).await,
        ["track"] => live.track().await,
        ["testnets", choice @ ("on" | "off")] => live.testnets(*choice == "on").await,
        ["tokens"] => live.tokens().await,
        ["token", "add", asset] => live.add_token(asset).await,
        ["token", "remove", asset] => live.remove_token(asset).await,
        ["ton-contract", wallet, contract] => live.ton_contract(wallet, contract).await,
        ["reveal", wallet] => live.reveal(wallet).await,
        ["import"] => live.import(None).await,
        ["import", "bip39"] => live.import(Some(PhraseKind::Bip39)).await,
        ["import", "ton"] => live.import(Some(PhraseKind::Ton)).await,
        _ => Err(USAGE.into()),
    };
    live.engine.shutdown().await?;
    result
}

fn folder() -> Outcome<PathBuf> {
    Ok(PathBuf::from(std::env::var("HOME")?).join(DIR))
}

async fn create() -> Outcome {
    let dir = folder()?;
    if dir.join(VAULT).exists() {
        return Err(format!("{} already holds a test vault", dir.display()).into());
    }
    private_dir(&dir)?;
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes).map_err(|_| "no randomness")?;
    let password = hex(&bytes);
    private_file(&dir.join(PASSWORD))?.write_all(password.as_bytes())?;
    let engine = Engine::start(EngineConfig::new(&dir))?;
    engine.create(SecretString::from(password)).await?;
    let mut settings = engine.settings().await?;
    settings.lock_after_minutes = LONGEST_LOCK;
    engine.set_settings(settings).await?;
    for _ in 0..2 {
        engine
            .generate(NewPhrase::Twelve, SecretString::from(""))
            .await?;
    }
    engine.shutdown().await?;
    println!("created {}", dir.display());
    let live = Live::open().await?;
    let result = live.addresses().await;
    live.engine.shutdown().await?;
    result
}

fn private_dir(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

fn private_file(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

struct Live {
    engine: Engine,
    password: SecretString,
    registry: Registry,
}

impl Live {
    async fn open() -> Outcome<Self> {
        let dir = folder()?;
        let password = fs::read_to_string(dir.join(PASSWORD))
            .map_err(|_| "no test vault yet, run create first")?;
        let engine = Engine::start(EngineConfig::new(&dir))?;
        engine
            .unlock(SecretString::from(password.trim().to_owned()))
            .await?;
        Ok(Self {
            engine,
            password: SecretString::from(password.trim().to_owned()),
            registry: Registry::builtin()?,
        })
    }

    fn secret(&self) -> SecretString {
        SecretString::from(self.password.expose_secret().to_owned())
    }

    async fn wallets(&self) -> Outcome<Vec<WalletId>> {
        let mut ids: Vec<WalletId> = self
            .engine
            .wallets()
            .await?
            .iter()
            .map(|info| info.id)
            .collect();
        ids.sort();
        Ok(ids)
    }

    async fn label(&self, id: WalletId) -> Outcome<char> {
        let at = self
            .wallets()
            .await?
            .iter()
            .position(|known| *known == id)
            .ok_or("unknown wallet")?;
        Ok(LETTERS.chars().nth(at).unwrap_or('?'))
    }

    async fn wallet(&self, letter: &str) -> Outcome<WalletId> {
        let at = LETTERS
            .find(letter)
            .filter(|_| letter.len() == 1)
            .ok_or("wallets are named by a single letter")?;
        Ok(*self.wallets().await?.get(at).ok_or("no such wallet")?)
    }

    fn network(&self, id: &NetworkId) -> Outcome<&Network> {
        Ok(self.registry.get(id).ok_or("unknown network")?)
    }

    async fn custom(&self) -> Outcome<Vec<CustomToken>> {
        Ok(self.engine.tokens().await?)
    }

    async fn symbol(&self, asset: &AssetId) -> Outcome<String> {
        let network = self.network(asset.network())?;
        let Some(contract) = asset.contract() else {
            return Ok(network.native.symbol.clone());
        };
        if let Some(token) = network.tokens.iter().find(|t| t.contract == *contract) {
            return Ok(token.coin.symbol.clone());
        }
        Ok(self
            .custom()
            .await?
            .into_iter()
            .find(|token| token.asset == *asset)
            .map_or_else(|| "?".to_owned(), |token| token.coin.symbol))
    }

    async fn asset(&self, text: &str) -> Outcome<(AssetId, u8)> {
        let (net, token) = match text.split_once('/') {
            Some((net, token)) => (net, Some(token)),
            None => (text, None),
        };
        let network = self.network(&net.parse()?)?;
        let Some(token) = token else {
            return Ok((AssetId::Native(network.id.clone()), network.native.decimals));
        };
        if let Some(listed) = network
            .tokens
            .iter()
            .find(|listed| listed.coin.symbol.eq_ignore_ascii_case(token))
        {
            return Ok((
                AssetId::Token(network.id.clone(), listed.contract),
                listed.coin.decimals,
            ));
        }
        let custom = self.custom().await?;
        let found = custom.iter().find(|custom| {
            custom.asset.network() == &network.id
                && (custom.coin.symbol.eq_ignore_ascii_case(token)
                    || custom.asset.to_string() == text)
        });
        match found {
            Some(custom) => Ok((custom.asset.clone(), custom.coin.decimals)),
            None => Err("unknown token, add it with token add first".into()),
        }
    }

    async fn accounts(&self) -> Outcome<Vec<Account>> {
        Ok(self.engine.accounts().await?)
    }

    async fn address_of(&self, wallet: WalletId, network: &Network) -> Outcome<Address> {
        Ok(self
            .accounts()
            .await?
            .into_iter()
            .find(|account| {
                account.wallet == wallet && account.serves(&network.id, network.family())
            })
            .ok_or("this wallet has no account on that network")?
            .address)
    }

    async fn destination(&self, text: &str, network: &Network) -> Outcome<Address> {
        if text.len() == 1 {
            return self.address_of(self.wallet(text).await?, network).await;
        }
        Ok(Address::parse(network.family(), text)?)
    }

    fn shown(address: &Address, network: &Network) -> String {
        address
            .ton_friendly(TonForm {
                bounceable: false,
                testnet: network.testnet,
            })
            .unwrap_or_else(|| address.to_string())
    }

    async fn addresses(&self) -> Outcome {
        let testnets = self.engine.settings().await?.show_testnets;
        for account in self.accounts().await? {
            let label = self.label(account.wallet).await?;
            match &account.network {
                Some(id) => {
                    let network = self.network(id)?;
                    if network.testnet && !testnets {
                        continue;
                    }
                    let shown = Self::shown(&account.address, network);
                    println!("{label}  {:<5} {shown}  ({id})", family(account.family));
                }
                None => println!("{label}  {:<5} {}", family(account.family), account.address),
            }
        }
        Ok(())
    }

    async fn fresh(&self) -> Outcome {
        let started = now();
        let mut portfolio = self.engine.portfolio();
        self.engine.refresh();
        tokio::time::timeout(FRESH, async {
            loop {
                let done = {
                    let current = portfolio.borrow_and_update();
                    !current.networks.is_empty()
                        && current
                            .networks
                            .iter()
                            .all(|health| health.checked_at >= started)
                };
                if done || portfolio.changed().await.is_err() {
                    return;
                }
            }
        })
        .await
        .map_err(|_| "the refresh did not finish in time")?;
        let _ = tokio::time::timeout(PRICES, async {
            loop {
                let priced = {
                    let current = portfolio.borrow_and_update();
                    matches!(current.prices, Status::Live | Status::Polling)
                        && current
                            .holdings
                            .iter()
                            .any(|holding| holding.value.is_some())
                };
                if priced || portfolio.changed().await.is_err() {
                    return;
                }
            }
        })
        .await;
        Ok(())
    }

    async fn status(&self) -> Outcome {
        self.fresh().await?;
        let portfolio = self.engine.portfolio().borrow().clone();
        println!(
            "balances ({}, prices {:?})",
            portfolio.fiat.code(),
            portfolio.prices
        );
        for holding in &portfolio.holdings {
            if holding.amount.is_zero() && holding.staked.is_none() {
                continue;
            }
            let label = self.label(holding.wallet).await?;
            let value = holding
                .value
                .map(|value| format!("  = {value} {}", portfolio.fiat.code()))
                .unwrap_or_default();
            let staked = holding
                .staked
                .map(|staked| format!("  staked {staked}"))
                .unwrap_or_default();
            println!(
                "  {label}  {:<12} {} {}{staked}{value}",
                holding.asset.network(),
                holding.amount,
                holding.symbol
            );
        }
        if let Some(total) = portfolio.total {
            println!("  total {total} {}", portfolio.fiat.code());
        }
        for health in &portfolio.networks {
            if let Some(problem) = &health.problem {
                println!("  {} problem: {problem}", health.network);
            }
            if let Some(problem) = &health.history_problem {
                println!("  {} history problem: {problem}", health.network);
            }
        }
        println!("sent");
        for tx in self.engine.history().await? {
            self.print_sent(&tx).await?;
        }
        println!("received");
        let incoming = self.engine.incoming().await?;
        for arrival in &incoming.transfers {
            let received = &arrival.received;
            println!(
                "  {:<12} {} {} from {} block {} tx {}{}",
                received.network,
                received.amount,
                self.symbol(&received.asset).await?,
                received.from,
                received.block,
                received.id,
                if arrival.cautions.is_empty() {
                    String::new()
                } else {
                    format!("  cautions {:?}", arrival.cautions)
                }
            );
        }
        println!("sent from other apps");
        for sent in self.engine.outgoing().await? {
            println!(
                "  {:<12} {} {} to {} block {} tx {}",
                sent.network,
                sent.amount,
                self.symbol(&sent.asset).await?,
                sent.to,
                sent.block,
                sent.id
            );
        }
        println!("history covered");
        for scan in &incoming.coverage {
            println!(
                "  {:<12} {} since {}",
                scan.network, scan.address, scan.complete_since
            );
        }
        Ok(())
    }

    async fn print_sent(&self, tx: &SentTx) -> Outcome {
        let network = self.network(&tx.network)?;
        let paid = tx
            .fee_paid
            .map_or_else(|| "?".to_owned(), |fee| fee.to_string());
        println!(
            "  {:<12} {:?}  {} {} to {}  fee {paid} (max {})  block {}\n    {}",
            tx.network,
            tx.status,
            tx.amount,
            self.symbol(&tx.asset).await?,
            Self::shown(&tx.to, network),
            tx.fee_max,
            tx.block
                .map_or_else(|| "-".to_owned(), |block| block.to_string()),
            network.explorer.tx_url(&tx.id)
        );
        Ok(())
    }

    async fn print_draft(&self, draft: &Draft, network: &Network) -> Outcome {
        let transfer = &draft.transfer;
        let symbol = self.symbol(&transfer.asset).await?;
        let native = &network.native.symbol;
        println!(
            "draft: {} {symbol}{} from {} to {}",
            transfer.amount,
            if transfer.max { " (max)" } else { "" },
            Self::shown(&transfer.from, network),
            Self::shown(&transfer.to, network)
        );
        for line in draft.fee.lines() {
            println!("  fee {:?}: {} {native}", line.item, line.amount);
        }
        println!(
            "  fee expected {} {native}, at most {} {native}, valid {}s",
            draft.fee.expected(),
            draft.fee.max(),
            draft.valid_for.as_secs()
        );
        if !draft.cautions.is_empty() {
            println!("  cautions {:?}", draft.cautions);
        }
        Ok(())
    }

    async fn send(&self, from: &str, asset: &str, to: &str, amount: &str) -> Outcome {
        let wallet = self.wallet(from).await?;
        let (asset, decimals) = self.asset(asset).await?;
        let network = self.network(asset.network())?.clone();
        let to = self.destination(to, &network).await?;
        let amount = if amount == "max" {
            Spend::Max
        } else {
            Spend::Exact(Amount::parse(amount, decimals)?)
        };
        let draft = self
            .engine
            .prepare_send(SendRequest {
                wallet,
                asset,
                to,
                amount,
            })
            .await?;
        self.print_draft(&draft, &network).await?;
        let mut events = self.engine.events();
        let sent = self.engine.confirm_send(draft.id, self.secret()).await?;
        println!("sent {:?}: {}", sent.status, sent.id);
        println!("  {}", network.explorer.tx_url(&sent.id));
        let done = settle(&mut events, &sent).await?;
        println!("settled");
        self.print_sent(&done).await?;
        let paid = done.fee_paid.map(|paid| {
            let expected = draft.fee.expected();
            match paid.checked_sub(expected) {
                Ok(over) => format!("{over} above the expected fee"),
                Err(_) => format!(
                    "{} below the expected fee",
                    expected.checked_sub(paid).unwrap_or(expected)
                ),
            }
        });
        if let Some(paid) = paid {
            println!("  paid {paid}");
        }
        Ok(())
    }

    async fn watch(&self, length: Duration) -> Outcome {
        let mut events = self.engine.events();
        self.engine.refresh();
        let _ = tokio::time::timeout(length, async {
            loop {
                match events.recv().await {
                    Ok(event) => self.print_event(&event).await,
                    Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => return,
                }
            }
        })
        .await;
        Ok(())
    }

    async fn print_event(&self, event: &Event) {
        match event {
            Event::TxUpdated(tx) => {
                let _ = self.print_sent(tx).await;
            }
            Event::Received(arrivals) => {
                for arrival in arrivals {
                    let received = &arrival.received;
                    println!(
                        "received {} on {} from {} tx {} {:?}",
                        received.amount,
                        received.network,
                        received.from,
                        received.id,
                        arrival.cautions
                    );
                }
            }
            Event::SentElsewhere(sent) => {
                for sent in sent {
                    println!(
                        "sent from another app {} on {} to {} tx {}",
                        sent.amount, sent.network, sent.to, sent.id
                    );
                }
            }
            other => println!("{other:?}"),
        }
    }

    async fn track(&self) -> Outcome {
        let mut events = self.engine.events();
        let open: Vec<SentTx> = self
            .engine
            .history()
            .await?
            .into_iter()
            .filter(|tx| tx.status.is_open())
            .collect();
        for tx in &open {
            println!("waiting for {}", tx.id);
            let done = settle(&mut events, tx).await?;
            self.print_sent(&done).await?;
        }
        if open.is_empty() {
            println!("nothing is open");
        }
        Ok(())
    }

    async fn testnets(&self, on: bool) -> Outcome {
        let mut settings = self.engine.settings().await?;
        settings.show_testnets = on;
        self.engine.set_settings(settings).await?;
        self.addresses().await
    }

    async fn tokens(&self) -> Outcome {
        for token in self.custom().await? {
            println!(
                "{}  {} ({}) {} decimals",
                token.asset, token.coin.symbol, token.coin.name, token.coin.decimals
            );
        }
        Ok(())
    }

    async fn add_token(&self, asset: &str) -> Outcome {
        let token = self.engine.add_token(asset.parse()?).await?;
        println!(
            "added {} ({}) {} decimals",
            token.coin.symbol, token.coin.name, token.coin.decimals
        );
        Ok(())
    }

    async fn remove_token(&self, asset: &str) -> Outcome {
        let removed = self.engine.remove_token(asset.parse()?).await?;
        println!("{}", if removed { "removed" } else { "not added" });
        Ok(())
    }

    async fn ton_contract(&self, wallet: &str, contract: &str) -> Outcome {
        let contract = match contract {
            "w5" => TonContract::W5,
            "v4r2" => TonContract::V4R2,
            _ => return Err("the contract is w5 or v4r2".into()),
        };
        self.engine
            .set_ton_contract(self.wallet(wallet).await?, contract)
            .await?;
        self.addresses().await
    }

    async fn reveal(&self, wallet: &str) -> Outcome {
        let phrase = self
            .engine
            .reveal(self.wallet(wallet).await?, self.secret())
            .await?;
        println!("{}", phrase.expose_secret());
        Ok(())
    }

    async fn import(&self, kind: Option<PhraseKind>) -> Outcome {
        print!("phrase: ");
        io::stdout().flush()?;
        let mut line = String::new();
        io::stdin().lock().read_line(&mut line)?;
        let phrase = SecretString::from(line.trim().to_owned());
        line.clear();
        let empty = SecretString::from("");
        let id = match kind {
            None => self.engine.import(phrase, empty).await?,
            Some(kind) => self.engine.import_as(phrase, empty, kind).await?,
        };
        println!("imported as wallet {}", self.label(id).await?);
        self.addresses().await
    }
}

async fn settle(events: &mut broadcast::Receiver<Event>, sent: &SentTx) -> Outcome<SentTx> {
    let mut last = sent.status.clone();
    let waited = tokio::time::timeout(SETTLE, async {
        loop {
            match events.recv().await {
                Ok(Event::TxUpdated(tx)) if tx.id == sent.id && tx.network == sent.network => {
                    if tx.status != last && tx.status != SentStatus::Sending {
                        println!("  now {:?}", tx.status);
                        last = tx.status.clone();
                    }
                    if !tx.status.is_open() {
                        return Ok(*tx);
                    }
                }
                Ok(_) | Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => return Err("the engine stopped"),
            }
        }
    })
    .await;
    match waited {
        Ok(result) => Ok(result?),
        Err(_) => Err("still not settled, run track later".into()),
    }
}

fn family(family: ChainFamily) -> &'static str {
    match family {
        ChainFamily::Evm => "evm",
        ChainFamily::Tron => "tron",
        ChainFamily::Ton => "ton",
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = std::fmt::Write::write_fmt(&mut out, format_args!("{byte:02x}"));
        out
    })
}
