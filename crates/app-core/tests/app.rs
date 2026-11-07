#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bahaa_app_core::{
    Action, App, AppConfig, ChainFamily, Channel, Currency, Direction, EngineConfig, Family, Field,
    Go, KdfParams, Kind, Level, Page, PageId, Pick, Registry, Request, Tab, Toggle, Tone, Value,
    View, Words,
};
use bahaa_testkit::Reply;
use serde_json::{Value as Json, json};
use tokio::sync::{mpsc, watch};

const PASSWORD: &str = "correct horse battery";
const HARDHAT: &str = "test test test test test test test test test test test junk";
const HARDHAT_0: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";
const FRIEND: &str = "0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC";
const BOTH: &str = "floor doctor sight adult spare hope fitness limit together ski damp photo car season parade sure fade long diesel toward resource try mobile junk";

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).unwrap();
        Self(std::env::temp_dir().join(format!(
            "bahaa-app-core-{:016x}",
            u64::from_le_bytes(random)
        )))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn offline(keep: impl Fn(ChainFamily) -> bool) -> Registry {
    let mut networks: Vec<_> = Registry::builtin()
        .unwrap()
        .networks()
        .filter(|network| keep(network.family()))
        .cloned()
        .collect();
    for network in &mut networks {
        for endpoint in &mut network.endpoints {
            endpoint.url = "http://127.0.0.1:9/".parse().unwrap();
        }
    }
    Registry::with_networks(networks).unwrap()
}

async fn bsc_node() -> String {
    let url = bahaa_testkit::serve(|request| {
        let result = match request.json()["method"].as_str() {
            Some("eth_chainId") => json!("0x38"),
            Some("eth_getBalance") => json!("0x14d1120d7b160000"),
            Some("eth_call") => json!(format!("0x{:064x}", 0)),
            _ => Json::Null,
        };
        Reply::json(200, &json!({"jsonrpc": "2.0", "id": 1, "result": result}))
    })
    .await;
    url.to_string()
}

fn start(scratch: &Scratch) -> (App, mpsc::Receiver<Request>, watch::Receiver<Arc<View>>) {
    start_with(scratch, offline(|_| true), false)
}

fn engine_config(scratch: &Scratch, registry: Registry) -> EngineConfig {
    EngineConfig {
        kdf: KdfParams {
            memory_kib: 64,
            iterations: 1,
            lanes: 1,
        },
        market: None,
        registry: Some(registry),
        swaps: None,
        ..EngineConfig::new(scratch.0.join("data"))
    }
}

fn start_with(
    scratch: &Scratch,
    registry: Registry,
    updates: bool,
) -> (App, mpsc::Receiver<Request>, watch::Receiver<Arc<View>>) {
    start_config(engine_config(scratch, registry), updates)
}

fn start_config(
    engine: EngineConfig,
    updates: bool,
) -> (App, mpsc::Receiver<Request>, watch::Receiver<Arc<View>>) {
    let (app, requests) = App::start(AppConfig { engine, updates }).unwrap();
    let view = app.view();
    (app, requests, view)
}

async fn until(view: &mut watch::Receiver<Arc<View>>, done: impl Fn(&View) -> bool) -> Arc<View> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            {
                let current = view.borrow_and_update();
                if done(&current) {
                    return Arc::clone(&current);
                }
            }
            view.changed().await.unwrap();
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out at {:#?}", view.borrow().page))
}

async fn at(view: &mut watch::Receiver<Arc<View>>, page: PageId) -> Arc<View> {
    until(view, |current| current.page.id == page).await
}

fn word_slots(page: &Page) -> Vec<usize> {
    page.inputs()
        .map(|input| input.label.trim_start_matches("Word #").parse().unwrap())
        .collect()
}

fn typed(view: &View, field: Field) -> String {
    view.page
        .inputs()
        .find(|input| input.field == field)
        .map(|input| input.value.text().to_owned())
        .unwrap_or_default()
}

fn set_password(app: &App) {
    app.act(Action::Type(Field::NewPassword, PASSWORD.to_owned()));
    app.act(Action::Type(Field::Confirm, PASSWORD.to_owned()));
    app.act(Action::Submit);
}

#[tokio::test]
async fn a_new_wallet_is_created_backed_up_and_locked_again() {
    let scratch = Scratch::new();
    let (app, _requests, mut view) = start(&scratch);
    at(&mut view, PageId::Welcome).await;
    app.act(Action::Pick(Pick::Create(Words::Twelve)));
    at(&mut view, PageId::NewPassword).await;
    app.act(Action::Type(Field::NewPassword, "short".to_owned()));
    app.act(Action::Submit);
    until(&mut view, |current| current.page.error.is_some()).await;
    set_password(&app);
    let shown = at(&mut view, PageId::Phrase).await;
    let words: Vec<String> = shown
        .page
        .secret
        .as_ref()
        .unwrap()
        .words()
        .map(str::to_owned)
        .collect();
    assert_eq!(words.len(), 12);
    app.act(Action::Back);
    until(&mut view, |current| !current.toasts.is_empty()).await;
    assert_eq!(view.borrow().page.id, PageId::Phrase);
    app.act(Action::Submit);
    let check = at(&mut view, PageId::Verify).await;
    let slots = word_slots(&check.page);
    assert_eq!(slots.len(), 3);
    for (slot, _) in slots.iter().enumerate() {
        app.act(Action::Type(Field::Word(slot), "wrong".to_owned()));
    }
    app.act(Action::Submit);
    until(&mut view, |current| {
        current
            .page
            .error
            .as_deref()
            .is_some_and(|error| error.contains("doesn't match"))
    })
    .await;
    for (slot, position) in slots.iter().enumerate() {
        app.act(Action::Type(
            Field::Word(slot),
            words[position - 1].to_uppercase(),
        ));
    }
    app.act(Action::Submit);
    let home = at(&mut view, PageId::Portfolio).await;
    assert_eq!(home.tabs.len(), 7);
    assert_eq!(home.tabs[3].label, "Swap");
    assert!(home.tabs[0].selected);
    app.act(Action::Lock);
    at(&mut view, PageId::Unlock).await;
    assert_eq!(view.borrow().tabs.len(), 0);
    app.act(Action::Type(Field::Password, "not it".to_owned()));
    app.act(Action::Submit);
    let wrong = until(&mut view, |current| {
        current.page.error.as_deref() == Some("Wrong password")
    })
    .await;
    assert!(
        matches!(&wrong.page.inputs().next().unwrap().value, Value::Hidden(secret) if secret.expose().is_empty())
    );
    let typed = app.act(Action::Type(Field::Password, PASSWORD.to_owned()));
    let echoed = until(&mut view, |current| current.handled >= typed).await;
    assert!(
        matches!(&echoed.page.inputs().next().unwrap().value, Value::Hidden(secret) if secret.expose() == PASSWORD)
    );
    assert!(!format!("{:?}", echoed.page).contains(PASSWORD));
    app.act(Action::Submit);
    at(&mut view, PageId::Portfolio).await;
    app.shutdown().await.unwrap();
}

async fn imported(scratch: &Scratch) -> (App, mpsc::Receiver<Request>, watch::Receiver<Arc<View>>) {
    imported_on(scratch, offline(|_| true), false).await
}

async fn imported_on(
    scratch: &Scratch,
    registry: Registry,
    updates: bool,
) -> (App, mpsc::Receiver<Request>, watch::Receiver<Arc<View>>) {
    imported_with(engine_config(scratch, registry), updates).await
}

async fn imported_with(
    engine: EngineConfig,
    updates: bool,
) -> (App, mpsc::Receiver<Request>, watch::Receiver<Arc<View>>) {
    let (app, requests, mut view) = start_config(engine, updates);
    at(&mut view, PageId::Welcome).await;
    app.act(Action::Pick(Pick::Import));
    at(&mut view, PageId::Import).await;
    app.act(Action::Type(Field::Phrase, "test test".to_owned()));
    until(&mut view, |current| {
        current
            .page
            .notes
            .first()
            .is_some_and(|note| note.text.contains("12, 15, 18, 21 or 24"))
    })
    .await;
    app.act(Action::Type(Field::Phrase, HARDHAT.to_owned()));
    until(&mut view, |current| {
        current
            .page
            .notes
            .first()
            .is_some_and(|note| note.text.contains("12 words"))
    })
    .await;
    app.act(Action::Submit);
    at(&mut view, PageId::NewPassword).await;
    set_password(&app);
    let home = at(&mut view, PageId::Portfolio).await;
    assert!(
        home.toasts
            .iter()
            .any(|toast| toast.text == "Wallet imported")
    );
    (app, requests, view)
}

#[tokio::test]
async fn an_imported_wallet_can_be_renamed() {
    let scratch = Scratch::new();
    let (app, _requests, mut view) = imported(&scratch).await;
    app.act(Action::Go(Go::Tab(Tab::Settings)));
    until(&mut view, |current| {
        current.page.id == PageId::Settings
            && current
                .page
                .table
                .as_ref()
                .is_some_and(|table| table.rows.len() == 1)
    })
    .await;
    app.act(Action::Go(Go::Wallet(0)));
    at(&mut view, PageId::Wallet).await;
    app.act(Action::Type(Field::Name, "Main".to_owned()));
    app.act(Action::Submit);
    until(&mut view, |current| current.page.title == "Main").await;
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn receiving_shows_the_address_and_a_payment_link() {
    let scratch = Scratch::new();
    let (app, mut requests, mut view) = imported(&scratch).await;
    app.act(Action::Go(Go::Tab(Tab::Receive)));
    let list = at(&mut view, PageId::ReceiveTo).await;
    let rows = &list.page.table.as_ref().unwrap().rows;
    let evm = rows
        .iter()
        .position(|row| row.cells[0].text == "BNB Smart Chain")
        .unwrap();
    assert_eq!(rows[evm].cells[1].text, "0xf39F…b92266");
    app.act(rows[evm].open.clone().unwrap());
    let shown = at(&mut view, PageId::Receive).await;
    assert_eq!(
        shown.page.headline.as_deref(),
        Some("0xf39F d6e5 1aad 88F6 F4ce 6aB8 8272 79cf fFb9 2266")
    );
    assert_eq!(shown.page.qr.as_deref(), Some(HARDHAT_0));
    app.act(Action::Type(Field::Amount, "1.5".to_owned()));
    until(&mut view, |current| {
        current
            .page
            .qr
            .as_deref()
            .is_some_and(|qr| qr == format!("ethereum:{HARDHAT_0}@56?value=1500000000000000000"))
    })
    .await;
    app.act(Action::Copy);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), requests.recv())
            .await
            .unwrap(),
        Some(Request::Copy(HARDHAT_0.to_owned()))
    );
    app.act(Action::Back);
    at(&mut view, PageId::ReceiveTo).await;
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn contacts_and_settings_are_saved() {
    let scratch = Scratch::new();
    let (app, _requests, mut view) = imported(&scratch).await;
    app.act(Action::Go(Go::Tab(Tab::Contacts)));
    at(&mut view, PageId::Contacts).await;
    app.act(Action::Go(Go::NewContact));
    let form = at(&mut view, PageId::Contact).await;
    let bsc = form
        .page
        .groups()
        .next()
        .unwrap()
        .choices
        .iter()
        .position(|choice| choice.label == "BNB Smart Chain")
        .unwrap();
    app.act(Action::Pick(Pick::Network(bsc)));
    app.act(Action::Type(Field::Name, "Friend".to_owned()));
    app.act(Action::Type(Field::Address, FRIEND.to_owned()));
    app.act(Action::Submit);
    until(&mut view, |current| {
        current.page.id == PageId::Contacts
            && current.page.table.as_ref().is_some_and(|table| {
                table
                    .rows
                    .first()
                    .is_some_and(|row| row.cells[0].text == "Friend")
            })
    })
    .await;
    app.act(Action::Go(Go::Tab(Tab::Settings)));
    at(&mut view, PageId::Settings).await;
    app.act(Action::Pick(Pick::Currency(Currency::Eur)));
    until(&mut view, |current| {
        current
            .page
            .groups()
            .next()
            .unwrap()
            .choices
            .iter()
            .any(|choice| choice.label == "EUR" && choice.selected)
    })
    .await;
    app.act(Action::Go(Go::Tab(Tab::Send)));
    let empty = at(&mut view, PageId::SendFrom).await;
    assert_eq!(empty.page.form.len(), 0);
    assert_eq!(
        empty.page.buttons[0].action,
        Action::Go(Go::Tab(Tab::Receive))
    );
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn the_banner_opens_the_network_status() {
    let scratch = Scratch::new();
    let (app, _requests, mut view) = imported(&scratch).await;
    let down = until(&mut view, |current| {
        current
            .banner
            .as_ref()
            .is_some_and(|banner| banner.level == Level::Warning)
    })
    .await;
    let banner = down.banner.clone().unwrap();
    assert!(banner.text.starts_with("Can't reach"));
    app.act(banner.action.clone());
    app.act(banner.action);
    let status = until(&mut view, |current| {
        current.page.id == PageId::Networks
            && current
                .page
                .notes
                .iter()
                .any(|note| note.text.starts_with("BNB Smart Chain: "))
    })
    .await;
    let table = status.page.table.as_ref().unwrap();
    let bsc = table
        .rows
        .iter()
        .find(|row| row.cells[0].text == "BNB Smart Chain")
        .unwrap();
    assert!(matches!(bsc.cells[0].tone, Tone::Brand(_)));
    assert_eq!(bsc.cells[1].text, "Can't reach");
    assert!(
        status
            .page
            .notes
            .iter()
            .any(|note| note.text.starts_with("BNB Smart Chain: "))
    );
    app.act(Action::Back);
    at(&mut view, PageId::Portfolio).await;
    app.act(Action::Go(Go::Tab(Tab::Activity)));
    let activity = at(&mut view, PageId::Activity).await;
    let labels: Vec<(&str, bool)> = activity
        .page
        .groups()
        .map(|group| {
            (
                group.label.as_str(),
                group.choices.iter().any(|choice| choice.selected),
            )
        })
        .collect();
    assert_eq!(labels, [("Show", true), ("Spam", true)]);
    app.act(Action::Pick(Pick::Direction(Direction::Sent)));
    until(&mut view, |current| {
        current.page.groups().next().is_some_and(|group| {
            group
                .choices
                .iter()
                .any(|choice| choice.label == "Sent" && choice.selected)
        })
    })
    .await;
    app.shutdown().await.unwrap();
}

fn hint(view: &View, field: Field) -> Option<(Level, String)> {
    view.page
        .inputs()
        .find(|input| input.field == field)?
        .hint
        .as_ref()
        .map(|hint| (hint.level, hint.text.clone()))
}

#[tokio::test]
async fn sending_starts_from_a_list_and_checks_while_typing() {
    let scratch = Scratch::new();
    let node = bsc_node().await;
    let (app, _requests, mut view) = imported_on(&scratch, bsc_registry(&node), false).await;
    app.act(Action::Go(Go::Tab(Tab::Send)));
    let list = until(&mut view, |current| {
        current.page.id == PageId::SendFrom
            && current
                .page
                .table
                .as_ref()
                .is_some_and(|table| !table.rows.is_empty())
    })
    .await;
    let row = list.page.table.as_ref().unwrap().rows[0].clone();
    assert_eq!(
        row.cells
            .iter()
            .map(|cell| cell.text.as_str())
            .collect::<Vec<_>>(),
        ["BNB", "BNB Smart Chain", "1.5 BNB", "—"]
    );
    app.act(row.open.unwrap());
    let compose = at(&mut view, PageId::Compose).await;
    assert_eq!(compose.page.title, "Send BNB");
    app.act(Action::Type(Field::To, "0x12".to_owned()));
    until(&mut view, |current| {
        hint(current, Field::To).is_some_and(|(level, _)| level == Level::Error)
    })
    .await;
    app.act(Action::Type(Field::To, FRIEND.to_owned()));
    until(&mut view, |current| {
        hint(current, Field::To)
            == Some((Level::Success, "A valid BNB Smart Chain address".to_owned()))
    })
    .await;
    app.act(Action::Type(Field::Amount, "2".to_owned()));
    until(&mut view, |current| {
        hint(current, Field::Amount)
            == Some((Level::Error, "More than you have (1.5 BNB)".to_owned()))
    })
    .await;
    app.act(Action::Type(Field::Amount, "1".to_owned()));
    until(&mut view, |current| {
        current.page.id == PageId::Compose && hint(current, Field::Amount).is_none()
    })
    .await;
    app.act(Action::Toggle(Toggle::Max));
    until(&mut view, |current| {
        typed(current, Field::Amount) == "1.5"
            && hint(current, Field::Amount)
                == Some((
                    Level::Info,
                    "Your whole balance, less the network fee".to_owned(),
                ))
    })
    .await;
    app.act(Action::Type(Field::Amount, "1.2".to_owned()));
    until(&mut view, |current| {
        typed(current, Field::Amount) == "1.2" && hint(current, Field::Amount).is_none()
    })
    .await;
    app.act(Action::Back);
    at(&mut view, PageId::SendFrom).await;
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn lists_narrow_down_as_the_search_is_typed() {
    let scratch = Scratch::new();
    let node = bsc_node().await;
    let (app, _requests, mut view) = imported_on(&scratch, bsc_registry(&node), false).await;
    app.act(Action::Go(Go::Tab(Tab::Send)));
    until(&mut view, |current| {
        current.page.id == PageId::SendFrom
            && current
                .page
                .table
                .as_ref()
                .is_some_and(|table| !table.rows.is_empty())
    })
    .await;
    let rows = |current: &View| {
        current
            .page
            .table
            .as_ref()
            .map_or(0, |table| table.rows.len())
    };
    for words in ["bnb", "smart chain", "wallet 1"] {
        app.act(Action::Type(Field::Search, words.to_owned()));
        until(&mut view, |current| {
            typed(current, Field::Search) == words
                && rows(current) == 1
                && hint(current, Field::Search) == Some((Level::Info, "1 of 1".to_owned()))
        })
        .await;
    }
    app.act(Action::Type(Field::Search, "eth".to_owned()));
    let missed = until(&mut view, |current| rows(current) == 0).await;
    assert_eq!(
        missed.page.table.as_ref().unwrap().empty,
        "Nothing matches \u{201c}eth\u{201d}"
    );
    app.act(Action::Go(Go::Tab(Tab::Receive)));
    let listed = until(&mut view, |current| {
        current.page.id == PageId::ReceiveTo && rows(current) > 1
    })
    .await;
    let all = rows(&listed);
    let everything = format!("{all} of {all}");
    app.act(Action::Type(Field::Search, HARDHAT_0.to_lowercase()));
    until(&mut view, |current| {
        rows(current) == all
            && hint(current, Field::Search) == Some((Level::Info, everything.clone()))
    })
    .await;
    for words in [format!("{HARDHAT_0} arbitrum"), "usdt avalanche".to_owned()] {
        app.act(Action::Type(Field::Search, words));
        until(&mut view, |current| rows(current) == 1).await;
    }
    app.act(Action::Type(Field::Search, "tron".to_owned()));
    until(&mut view, |current| rows(current) == 0).await;
    app.shutdown().await.unwrap();
}

async fn next_request(requests: &mut mpsc::Receiver<Request>) -> Request {
    tokio::time::timeout(Duration::from_secs(5), requests.recv())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn settings_check_for_updates_hide_balances_and_open_developer_options() {
    let scratch = Scratch::new();
    let (app, mut requests, mut view) = imported_on(&scratch, offline(|_| true), true).await;
    assert_eq!(
        next_request(&mut requests).await,
        Request::CheckUpdates {
            beta: false,
            asked: false
        }
    );
    app.act(Action::Go(Go::Tab(Tab::Settings)));
    at(&mut view, PageId::Settings).await;
    app.act(Action::Pick(Pick::Channel(Channel::Beta)));
    assert_eq!(
        next_request(&mut requests).await,
        Request::CheckUpdates {
            beta: true,
            asked: true
        }
    );
    app.act(Action::Toggle(Toggle::HideBalances));
    until(&mut view, |current| {
        current.page.groups().any(|group| {
            group.label == "Privacy" && group.choices.iter().all(|choice| choice.selected)
        })
    })
    .await;
    app.act(Action::Go(Go::Tab(Tab::Portfolio)));
    until(&mut view, |current| {
        current.page.id == PageId::Portfolio && current.page.headline.as_deref() == Some("••••")
    })
    .await;
    app.act(Action::Go(Go::Tab(Tab::Settings)));
    at(&mut view, PageId::Settings).await;
    app.act(Action::Go(Go::Developer));
    at(&mut view, PageId::Developer).await;
    app.act(Action::Toggle(Toggle::Developer));
    until(&mut view, |current| {
        current
            .page
            .groups()
            .next()
            .is_some_and(|group| group.choices[0].selected)
    })
    .await;
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_private_key_is_checked_while_typing_and_becomes_a_one_account_wallet() {
    let scratch = Scratch::new();
    let (app, _requests, mut view) = start(&scratch);
    at(&mut view, PageId::Welcome).await;
    app.act(Action::Pick(Pick::ImportKey));
    at(&mut view, PageId::ImportKey).await;
    app.act(Action::Type(Field::Key, "0x12".to_owned()));
    until(&mut view, |current| {
        hint(current, Field::Key)
            == Some((
                Level::Error,
                "A private key is 64 hex characters".to_owned(),
            ))
    })
    .await;
    app.act(Action::Type(
        Field::Key,
        "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80".to_owned(),
    ));
    until(&mut view, |current| {
        hint(current, Field::Key) == Some((Level::Success, "A valid private key".to_owned()))
    })
    .await;
    app.act(Action::Submit);
    at(&mut view, PageId::NewPassword).await;
    set_password(&app);
    at(&mut view, PageId::Portfolio).await;
    app.act(Action::Go(Go::Tab(Tab::Settings)));
    let settings = until(&mut view, |current| {
        current.page.id == PageId::Settings
            && current
                .page
                .table
                .as_ref()
                .is_some_and(|table| !table.rows.is_empty())
    })
    .await;
    let row = &settings.page.table.as_ref().unwrap().rows[0];
    assert_eq!(row.cells[1].text, "Private key, EVM networks");
    app.act(row.open.clone().unwrap());
    let wallet = at(&mut view, PageId::Wallet).await;
    assert_eq!(wallet.page.groups().count(), 0);
    assert!(
        wallet
            .page
            .buttons
            .iter()
            .any(|button| button.label == "Show private key")
    );
    assert!(
        !wallet
            .page
            .buttons
            .iter()
            .any(|button| button.label == "Add account")
    );
    app.shutdown().await.unwrap();
}

fn bsc_registry(node: &str) -> Registry {
    let networks: Vec<_> = offline(|family| family == ChainFamily::Evm)
        .networks()
        .cloned()
        .map(|mut network| {
            if network.id.as_str() == "bsc" {
                network.endpoints.truncate(1);
                network.endpoints[0].url = node.parse().unwrap();
            }
            network
        })
        .collect();
    Registry::with_networks(networks).unwrap()
}

fn counted_switch(page: &Page) -> Option<bool> {
    page.groups()
        .find(|group| group.label == "Portfolio")
        .map(|group| group.choices[0].selected)
}

async fn watching(scratch: &Scratch) -> (App, mpsc::Receiver<Request>, watch::Receiver<Arc<View>>) {
    let registry = bsc_registry(&bsc_node().await);
    let (app, requests, mut view) = start_with(scratch, registry, false);
    at(&mut view, PageId::Welcome).await;
    app.act(Action::Pick(Pick::Watch));
    let page = at(&mut view, PageId::Watch).await;
    assert_eq!(counted_switch(&page.page), Some(true));
    app.act(Action::Type(Field::Address, "hello".to_owned()));
    until(&mut view, |current| {
        hint(current, Field::Address)
            == Some((
                Level::Error,
                "This is not an address on EVM networks".to_owned(),
            ))
    })
    .await;
    app.act(Action::Type(Field::Address, FRIEND.to_owned()));
    until(&mut view, |current| {
        hint(current, Field::Address)
            == Some((Level::Success, "An address on EVM networks".to_owned()))
    })
    .await;
    app.act(Action::Submit);
    at(&mut view, PageId::NewPassword).await;
    set_password(&app);
    let home = at(&mut view, PageId::Portfolio).await;
    assert!(
        home.toasts
            .iter()
            .any(|toast| toast.text == "Watching the address")
    );
    (app, requests, view)
}

async fn watch_wallet_page(app: &App, view: &mut watch::Receiver<Arc<View>>) -> Arc<View> {
    app.act(Action::Go(Go::Tab(Tab::Settings)));
    let settings = until(view, |current| {
        current.page.id == PageId::Settings
            && current
                .page
                .table
                .as_ref()
                .is_some_and(|table| !table.rows.is_empty())
    })
    .await;
    let row = &settings.page.table.as_ref().unwrap().rows[0];
    assert_eq!(row.cells[1].text, "Watch only, EVM networks");
    app.act(row.open.clone().unwrap());
    at(view, PageId::Wallet).await
}

#[tokio::test]
async fn a_watched_address_shows_its_balance_but_is_never_offered_for_sending() {
    let scratch = Scratch::new();
    let (app, _requests, mut view) = watching(&scratch).await;
    until(&mut view, |current| {
        current
            .page
            .table
            .as_ref()
            .is_some_and(|table| !table.rows.is_empty())
    })
    .await;
    app.act(Action::Go(Go::Tab(Tab::Send)));
    let send = at(&mut view, PageId::SendFrom).await;
    assert!(
        send.page
            .table
            .as_ref()
            .is_none_or(|table| table.rows.is_empty())
    );
    let wallet = watch_wallet_page(&app, &mut view).await;
    assert_eq!(counted_switch(&wallet.page), Some(true));
    assert!(
        !wallet
            .page
            .buttons
            .iter()
            .any(|button| button.label.starts_with("Show"))
    );
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_watched_wallet_left_out_of_the_total_is_one_pick_away() {
    let scratch = Scratch::new();
    let (app, _requests, mut view) = watching(&scratch).await;
    watch_wallet_page(&app, &mut view).await;
    app.act(Action::Toggle(Toggle::Counted));
    until(&mut view, |current| {
        current.page.id == PageId::Wallet && counted_switch(&current.page) == Some(false)
    })
    .await;
    app.act(Action::Go(Go::Tab(Tab::Portfolio)));
    let left_out = until(&mut view, |current| {
        current.page.id == PageId::Portfolio
            && current
                .page
                .notes
                .iter()
                .any(|note| note.text.starts_with("1 watch-only wallet"))
    })
    .await;
    assert!(
        left_out
            .page
            .table
            .as_ref()
            .is_some_and(|table| table.rows.is_empty())
    );
    assert!(left_out.page.groups().any(|group| group.label == "Wallets"));
    app.act(Action::Pick(Pick::Wallet(Some(0))));
    until(&mut view, |current| {
        current.page.id == PageId::Portfolio
            && current.page.notes.is_empty()
            && current
                .page
                .table
                .as_ref()
                .is_some_and(|table| !table.rows.is_empty())
    })
    .await;
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_ton_private_key_is_checked_as_ton_and_keeps_the_contract_switch() {
    let scratch = Scratch::new();
    let registry = offline(|family| family != ChainFamily::Ton);
    let (app, _requests, mut view) = start_with(&scratch, registry, false);
    at(&mut view, PageId::Welcome).await;
    app.act(Action::Pick(Pick::ImportKey));
    at(&mut view, PageId::ImportKey).await;
    app.act(Action::Type(
        Field::Key,
        "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60\
         d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"
            .to_owned(),
    ));
    until(&mut view, |current| {
        hint(current, Field::Key)
            == Some((
                Level::Error,
                "A private key is 64 hex characters".to_owned(),
            ))
    })
    .await;
    app.act(Action::Pick(Pick::Family(Family::Ton)));
    until(&mut view, |current| {
        hint(current, Field::Key) == Some((Level::Success, "A valid private key".to_owned()))
    })
    .await;
    app.act(Action::Submit);
    at(&mut view, PageId::NewPassword).await;
    set_password(&app);
    at(&mut view, PageId::Portfolio).await;
    app.act(Action::Go(Go::Tab(Tab::Settings)));
    let settings = until(&mut view, |current| {
        current.page.id == PageId::Settings
            && current
                .page
                .table
                .as_ref()
                .is_some_and(|table| !table.rows.is_empty())
    })
    .await;
    let row = &settings.page.table.as_ref().unwrap().rows[0];
    assert_eq!(row.cells[1].text, "Private key, TON");
    app.act(row.open.clone().unwrap());
    let wallet = at(&mut view, PageId::Wallet).await;
    let contract = wallet.page.groups().next().unwrap();
    assert_eq!(contract.label, "TON contract");
    assert!(contract.choices[0].selected);
    app.shutdown().await.unwrap();
}

async fn import_as_ton(app: &App, view: &mut watch::Receiver<Arc<View>>) {
    at(view, PageId::Welcome).await;
    app.act(Action::Pick(Pick::Import));
    at(view, PageId::Import).await;
    app.act(Action::Type(Field::Phrase, BOTH.to_owned()));
    let asked = until(view, |current| current.page.groups().next().is_some()).await;
    assert_eq!(asked.page.groups().next().unwrap().choices.len(), 2);
    app.act(Action::Submit);
    until(view, |current| current.page.error.is_some()).await;
    app.act(Action::Pick(Pick::Kind(Kind::Ton)));
    app.act(Action::Submit);
    at(view, PageId::NewPassword).await;
    set_password(app);
}

#[tokio::test]
async fn a_ton_import_waits_for_the_network_to_pick_the_contract() {
    let scratch = Scratch::new();
    let (app, _requests, mut view) = start(&scratch);
    import_as_ton(&app, &mut view).await;
    let failed = until(&mut view, |current| current.page.error.is_some()).await;
    assert_eq!(failed.page.id, PageId::NewPassword);
    assert!(
        failed
            .page
            .error
            .as_deref()
            .is_some_and(|error| error.starts_with("Network problem"))
    );
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn words_valid_both_ways_ask_which_wallet_made_them() {
    let scratch = Scratch::new();
    let registry = offline(|family| family != ChainFamily::Ton);
    let (app, _requests, mut view) = start_with(&scratch, registry, false);
    import_as_ton(&app, &mut view).await;
    at(&mut view, PageId::Portfolio).await;
    app.act(Action::Go(Go::Tab(Tab::Settings)));
    until(&mut view, |current| {
        current.page.table.as_ref().is_some_and(|table| {
            table
                .rows
                .first()
                .is_some_and(|row| row.cells[1].text == "TON wallet (W5)")
        })
    })
    .await;
    app.shutdown().await.unwrap();
}

const DEPOSIT: &str = "0x88346a0010f9c3914b9e6d507a7200bdcdede3a5";
const REQUEST_ID: &str = "0x1793720631f6df061cade369e4f053582da6700bb930246d73dc27ffad60ddb9";

fn keccak(raw: &str) -> String {
    use sha3::{Digest, Keccak256};
    use std::fmt::Write as _;
    let bytes: Vec<u8> = (2..raw.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&raw[at..at + 2], 16).unwrap())
        .collect();
    Keccak256::digest(bytes)
        .iter()
        .fold("0x".to_owned(), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

async fn spending_node() -> String {
    let sent = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let url = bahaa_testkit::serve(move |request| {
        let call = request.json();
        let first = call["params"][0].as_str().unwrap_or_default().to_owned();
        let mut sent = sent.lock().unwrap();
        let result = match call["method"].as_str() {
            Some("eth_chainId") => json!("0x38"),
            Some("eth_getBalance") => json!("0x14d1120d7b160000"),
            Some("eth_getTransactionCount") => json!("0x0"),
            Some("eth_feeHistory") => {
                json!({"oldestBlock": "0x1", "baseFeePerGas": ["0x3b9aca00", "0x3b9aca00"]})
            }
            Some("eth_maxPriorityFeePerGas") => json!("0x3b9aca00"),
            Some("eth_estimateGas") => json!("0x5208"),
            Some("eth_getCode") => json!("0x"),
            Some("eth_call") => json!(format!("0x{:064x}", 0)),
            Some("eth_sendRawTransaction") => {
                sent.push(keccak(&first));
                json!(keccak(&first))
            }
            Some("eth_getTransactionReceipt") if sent.contains(&first) => json!({
                "status": "0x1",
                "blockNumber": "0x10",
                "gasUsed": "0x5208",
                "effectiveGasPrice": "0x77359400"
            }),
            _ => Json::Null,
        };
        Reply::json(200, &json!({"jsonrpc": "2.0", "id": 1, "result": result}))
    })
    .await;
    url.to_string()
}

async fn relay() -> String {
    let polls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    bahaa_testkit::serve(move |request| {
        if request.path().starts_with("/intents/status/v3") {
            let done = polls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= 2;
            let status = if done { "success" } else { "pending" };
            return Reply::json(200, &json!({ "status": status }));
        }
        let amount = request.json()["amount"].as_str().unwrap().to_owned();
        let data = json!({"from": HARDHAT_0, "to": DEPOSIT, "data": "0x", "value": amount, "chainId": 56});
        Reply::json(
            200,
            &json!({
                "steps": [{
                    "id": "deposit",
                    "kind": "transaction",
                    "requestId": REQUEST_ID,
                    "depositAddress": DEPOSIT,
                    "items": [{"data": data}]
                }],
                "details": {
                    "currencyIn": {"amount": amount},
                    "currencyOut": {"amount": "29800000000000000", "minimumAmount": "29500000000000000"},
                    "timeEstimate": 2
                }
            }),
        )
    })
    .await
    .to_string()
}

fn cells(row: &bahaa_app_core::Row) -> Vec<&str> {
    row.cells.iter().map(|cell| cell.text.as_str()).collect()
}

fn opener(view: &View, wanted: &[&str]) -> Action {
    let table = view.page.table.as_ref().unwrap();
    let row = table.rows.iter().find(|row| cells(row) == wanted).unwrap();
    row.open.clone().unwrap()
}

async fn activity_finds_the_swap(app: &App, view: &mut watch::Receiver<Arc<View>>) {
    app.act(Action::Go(Go::Tab(Tab::Activity)));
    let rows = |current: &View| {
        current
            .page
            .table
            .as_ref()
            .map_or(0, |table| table.rows.len())
    };
    until(view, |current| {
        current.page.id == PageId::Activity && rows(current) == 1
    })
    .await;
    for words in [
        "0.1",
        "eth base",
        "0x88346a0010f9c3914b9e6d507a7200bdcdede3a5",
        "88346a",
        "bnb done",
    ] {
        app.act(Action::Type(Field::Search, words.to_owned()));
        until(view, |current| {
            typed(current, Field::Search) == words
                && rows(current) == 1
                && hint(current, Field::Search) == Some((Level::Info, "1 of 1".to_owned()))
        })
        .await;
    }
    for words in ["ton", "88", "0.2"] {
        app.act(Action::Type(Field::Search, words.to_owned()));
        until(view, |current| {
            typed(current, Field::Search) == words && rows(current) == 0
        })
        .await;
    }
}

async fn swap_app(scratch: &Scratch) -> (App, mpsc::Receiver<Request>, watch::Receiver<Arc<View>>) {
    let node = spending_node().await;
    let mut engine = engine_config(scratch, bsc_registry(&node));
    let mut providers = bahaa_engine::swap::Providers::builtin().unwrap();
    providers.relay.url = relay().await.parse().unwrap();
    providers.oneclick.url = "http://127.0.0.1:9/".parse().unwrap();
    providers.kyberswap.url = "http://127.0.0.1:9/".parse().unwrap();
    providers.velora.url = "http://127.0.0.1:9/".parse().unwrap();
    engine.swaps = Some(providers);
    engine.track_every = Duration::from_millis(50);
    imported_with(engine, false).await
}

async fn routes_shown(view: &mut watch::Receiver<Arc<View>>) -> Arc<View> {
    until(view, |current| {
        current.page.id == PageId::Swap && current.page.table.is_some()
    })
    .await
}

async fn swap_compose(app: &App, view: &mut watch::Receiver<Arc<View>>) {
    app.act(Action::Go(Go::Tab(Tab::Swap)));
    let sources = until(view, |current| {
        current.page.id == PageId::SwapFrom
            && current
                .page
                .table
                .as_ref()
                .is_some_and(|table| !table.rows.is_empty())
    })
    .await;
    app.act(opener(
        &sources,
        &["BNB", "BNB Smart Chain", "1.5 BNB", "—"],
    ));
    let targets = at(view, PageId::SwapTo).await;
    let rows = &targets.page.table.as_ref().unwrap().rows;
    assert!(
        rows.iter()
            .all(|row| cells(row) != ["BNB", "BNB Smart Chain"])
    );
    app.act(opener(&targets, &["ETH", "Base"]));
    at(view, PageId::Swap).await;
}

#[tokio::test]
async fn swapping_everything_shows_the_exact_amount_left_after_the_fee() {
    let scratch = Scratch::new();
    let (app, _requests, mut view) = swap_app(&scratch).await;
    swap_compose(&app, &mut view).await;
    app.act(Action::Toggle(Toggle::Max));
    let filled = until(&mut view, |current| typed(current, Field::Amount) == "1.5").await;
    assert_eq!(
        hint(&filled, Field::Amount),
        Some((
            Level::Info,
            "Your whole balance, less the network fee".to_owned()
        ))
    );
    app.act(Action::Submit);
    let routes = routes_shown(&mut view).await;
    assert_eq!(typed(&routes, Field::Amount), "1.499937");
    assert_eq!(
        hint(&routes, Field::Amount),
        Some((Level::Info, "Everything after the network fee".to_owned()))
    );
    app.act(Action::Submit);
    let review = at(&mut view, PageId::SwapReview).await;
    assert!(
        review
            .page
            .lines
            .iter()
            .any(|line| line.label == "You send" && line.value == "1.499937 BNB ≈ — from Wallet 1")
    );
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_swap_lists_every_route_reviews_the_exact_deposit_and_follows_the_provider() {
    let scratch = Scratch::new();
    let (app, _requests, mut view) = swap_app(&scratch).await;
    swap_compose(&app, &mut view).await;
    app.act(Action::Type(Field::Amount, "0.1".to_owned()));
    app.act(Action::Submit);
    let routes = routes_shown(&mut view).await;
    let table = routes.page.table.as_ref().unwrap();
    assert_eq!(
        cells(&table.rows[0]),
        [
            "Relay · best",
            "0.0298 ETH",
            "—",
            "—",
            "~2 s",
            "the provider"
        ]
    );
    assert!(
        routes
            .page
            .notes
            .iter()
            .any(|note| note.text.starts_with("NEAR Intents: "))
    );
    assert_eq!(routes.page.buttons[0].label, "Review the best route");
    app.act(Action::Submit);
    let review = at(&mut view, PageId::SwapReview).await;
    assert_eq!(review.page.title, "Swap 0.1 BNB to ETH on Base");
    let shown: Vec<(&str, &str)> = review
        .page
        .lines
        .iter()
        .map(|line| (line.label.as_str(), line.value.as_str()))
        .collect();
    for expected in [
        (
            "Provider",
            "Relay · it holds your money until it pays out, and refunds you if it fails",
        ),
        ("You send", "0.1 BNB ≈ — from Wallet 1"),
        (
            "You receive",
            "0.0298 ETH ≈ — · at least 0.0295 ETH, to Wallet 1 on Base",
        ),
        ("Rate", "1 BNB = 0.298 ETH"),
        ("Network fee", "0.000042 BNB ≈ —, paid from your balance"),
        (
            "Deposit to",
            "0x8834 6A00 10F9 c391 4b9e 6d50 7A72 00bD cdED E3a5",
        ),
    ] {
        assert!(shown.contains(&expected), "{expected:?} in {shown:?}");
    }
    app.act(Action::Back);
    at(&mut view, PageId::Swap).await;
    app.act(Action::Pick(Pick::Route(0)));
    at(&mut view, PageId::SwapReview).await;
    app.act(Action::Submit);
    at(&mut view, PageId::Confirm).await;
    app.act(Action::Type(Field::Password, PASSWORD.to_owned()));
    app.act(Action::Submit);
    let done = until(&mut view, |current| {
        current.page.id == PageId::Swapping
            && current
                .page
                .lines
                .first()
                .is_some_and(|line| line.value == "Done")
    })
    .await;
    assert_eq!(done.page.headline.as_deref(), Some("0.1 BNB → ETH"));
    assert!(
        done.toasts
            .iter()
            .any(|toast| toast.text == "Swap done: 0.1 BNB to ETH")
    );
    activity_finds_the_swap(&app, &mut view).await;
    app.shutdown().await.unwrap();
}
