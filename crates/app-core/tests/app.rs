#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bahaa_app_core::{
    Action, App, AppConfig, ChainFamily, Channel, Currency, Direction, EngineConfig, Field, Go,
    KdfParams, Kind, Level, Page, PageId, Pick, Registry, Request, Tab, Toggle, Tone, Value, View,
    Words,
};
use serde_json::{Value as Json, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
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
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let Some(call) = read_call(&mut socket).await else {
                    return;
                };
                let result = match call["method"].as_str() {
                    Some("eth_chainId") => json!("0x38"),
                    Some("eth_getBalance") => json!("0x14d1120d7b160000"),
                    Some("eth_call") => json!(format!("0x{:064x}", 0)),
                    _ => Json::Null,
                };
                let text = json!({"jsonrpc": "2.0", "id": 1, "result": result}).to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                    text.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    url
}

async fn read_call(socket: &mut tokio::net::TcpStream) -> Option<Json> {
    let mut request = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let read = socket.read(&mut buffer).await.ok()?;
        if read == 0 {
            return None;
        }
        request.extend_from_slice(&buffer[..read]);
        let text = String::from_utf8_lossy(&request).to_string();
        let Some(end) = text.find("\r\n\r\n") else {
            continue;
        };
        let length = text
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length: ")
                    .and_then(|value| value.trim().parse::<usize>().ok())
            })
            .unwrap_or(0);
        if request.len() >= end + 4 + length {
            return serde_json::from_slice(&request[end + 4..end + 4 + length]).ok();
        }
    }
}

fn start(scratch: &Scratch) -> (App, mpsc::Receiver<Request>, watch::Receiver<Arc<View>>) {
    start_with(scratch, offline(|_| true), false)
}

fn start_with(
    scratch: &Scratch,
    registry: Registry,
    updates: bool,
) -> (App, mpsc::Receiver<Request>, watch::Receiver<Arc<View>>) {
    let config = AppConfig {
        engine: EngineConfig {
            kdf: KdfParams {
                memory_kib: 64,
                iterations: 1,
                lanes: 1,
            },
            market: None,
            registry: Some(registry),
            ..EngineConfig::new(scratch.0.join("data"))
        },
        updates,
    };
    let (app, requests) = App::start(config).unwrap();
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
    assert_eq!(home.tabs.len(), 6);
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
    let (app, requests, mut view) = start_with(scratch, registry, updates);
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
        current.page.id == PageId::Networks && !current.page.notes.is_empty()
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
    let mut registry = offline(|family| family == ChainFamily::Evm);
    let mut networks: Vec<_> = registry.networks().cloned().collect();
    for network in &mut networks {
        if network.id.as_str() == "bsc" {
            network.endpoints.truncate(1);
            network.endpoints[0].url = node.parse().unwrap();
        }
    }
    registry = Registry::with_networks(networks).unwrap();
    let (app, _requests, mut view) = imported_on(&scratch, registry, false).await;
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
    app.act(Action::Back);
    at(&mut view, PageId::SendFrom).await;
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
