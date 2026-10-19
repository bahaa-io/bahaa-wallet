#![allow(clippy::unwrap_used)]

use std::fmt::Write;
use std::path::PathBuf;
use std::time::Duration;

use bahaa_config::{Fiat, Registry, Settings};
use bahaa_crypto::{ExposeSecret, SecretString};
use bahaa_engine::{Engine, EngineConfig, EngineError, Event, KeyringError, NewPhrase, State};
use bahaa_store::StoreError;
use bahaa_vault::KdfParams;
use tokio::sync::watch;

const HARDHAT: &str = "test test test test test test test test test test test junk";

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).unwrap();
        let name = random.iter().fold(String::new(), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        });
        Self(std::env::temp_dir().join(format!("bahaa-engine-run-{name}")))
    }

    fn config(&self) -> EngineConfig {
        EngineConfig {
            data_dir: self.0.join("data"),
            kdf: KdfParams {
                memory_kib: 64,
                iterations: 1,
                lanes: 1,
            },
            market: None,
            registry: Some(Registry::with_networks(Vec::new()).unwrap()),
            auto_lock_unit: Duration::from_millis(50),
            refresh_every: Duration::from_secs(60),
            draft_ttl: Duration::from_secs(120),
            track_every: Duration::from_secs(4),
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn secret(text: &str) -> SecretString {
    SecretString::from(text)
}

async fn until<T: Clone>(watcher: &mut watch::Receiver<T>, done: impl Fn(&T) -> bool) -> T {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            {
                let current = watcher.borrow_and_update();
                if done(&current) {
                    return current.clone();
                }
            }
            watcher.changed().await.unwrap();
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn sets_up_a_new_wallet_and_announces_it() {
    let scratch = Scratch::new();
    let engine = Engine::start(scratch.config()).unwrap();
    let mut events = engine.events();
    assert_eq!(engine.state(), State::NoVault);
    engine.create(secret("pw")).await.unwrap();
    assert_eq!(engine.state(), State::Unlocked);
    assert_eq!(
        events.recv().await.unwrap(),
        Event::StateChanged(State::Unlocked)
    );
    let (id, phrase) = engine
        .generate(NewPhrase::TwentyFour, secret(""))
        .await
        .unwrap();
    assert_eq!(phrase.expose_secret().split(' ').count(), 24);
    let wallets = loop {
        if let Event::WalletsChanged(wallets) = events.recv().await.unwrap() {
            break wallets;
        }
    };
    assert_eq!(wallets[0].id, id);
}

#[tokio::test]
async fn everything_is_there_after_a_restart() {
    let scratch = Scratch::new();
    let engine = Engine::start(scratch.config()).unwrap();
    engine.create(secret("pw")).await.unwrap();
    let id = engine.import(secret(HARDHAT), secret("")).await.unwrap();
    let settings = Settings {
        fiat: Fiat::Irt,
        ..Settings::default()
    };
    engine.set_settings(settings.clone()).await.unwrap();
    engine.shutdown().await.unwrap();
    assert_eq!(engine.state(), State::Unlocked);
    assert_eq!(engine.wallets().await.unwrap_err(), EngineError::Stopped);
    let engine = Engine::start(scratch.config()).unwrap();
    assert_eq!(engine.state(), State::Locked);
    assert_eq!(engine.settings().await.unwrap(), settings);
    engine.unlock(secret("pw")).await.unwrap();
    assert_eq!(engine.wallets().await.unwrap()[0].id, id);
}

#[tokio::test]
async fn locking_closes_the_secrets() {
    let scratch = Scratch::new();
    let engine = Engine::start(scratch.config()).unwrap();
    engine.create(secret("pw")).await.unwrap();
    let id = engine.import(secret(HARDHAT), secret("")).await.unwrap();
    engine.lock().await.unwrap();
    assert_eq!(engine.state(), State::Locked);
    assert_eq!(
        engine.wallets().await.unwrap_err(),
        EngineError::Keyring(KeyringError::Locked)
    );
    assert_eq!(
        engine.reveal(id, secret("pw")).await.unwrap_err(),
        EngineError::Keyring(KeyringError::Locked)
    );
    assert_eq!(
        engine.unlock(secret("nope")).await.unwrap_err(),
        EngineError::Keyring(KeyringError::WrongPassword)
    );
    assert_eq!(engine.state(), State::Locked);
    engine.unlock(secret("pw")).await.unwrap();
    assert_eq!(
        engine
            .reveal(id, secret("pw"))
            .await
            .unwrap()
            .expose_secret(),
        HARDHAT
    );
}

#[tokio::test]
async fn locks_itself_after_the_idle_time() {
    let scratch = Scratch::new();
    let engine = Engine::start(scratch.config()).unwrap();
    engine.create(secret("pw")).await.unwrap();
    engine
        .set_settings(Settings {
            lock_after_minutes: 2,
            ..Settings::default()
        })
        .await
        .unwrap();
    for _ in 0..6 {
        tokio::time::sleep(Duration::from_millis(40)).await;
        engine.touch();
    }
    assert_eq!(engine.state(), State::Unlocked);
    for _ in 0..6 {
        tokio::time::sleep(Duration::from_millis(40)).await;
        engine.wallets().await.unwrap_or_default();
    }
    let mut state = engine.watch_state();
    until(&mut state, |state| *state == State::Locked).await;
}

#[tokio::test]
async fn changing_the_password_goes_through_the_engine() {
    let scratch = Scratch::new();
    let engine = Engine::start(scratch.config()).unwrap();
    engine.create(secret("pw")).await.unwrap();
    engine
        .change_password(secret("pw"), secret("better"))
        .await
        .unwrap();
    engine.lock().await.unwrap();
    assert!(engine.unlock(secret("pw")).await.is_err());
    engine.unlock(secret("better")).await.unwrap();
}

#[tokio::test]
async fn passwords_are_checked_without_changing_anything() {
    let scratch = Scratch::new();
    let engine = Engine::start(scratch.config()).unwrap();
    engine.create(secret("pw")).await.unwrap();
    engine.check_password(secret("pw")).await.unwrap();
    assert_eq!(
        engine.check_password(secret("nope")).await,
        Err(EngineError::Keyring(KeyringError::WrongPassword))
    );
    assert_eq!(engine.state(), State::Unlocked);
}

#[tokio::test]
async fn wallets_keep_their_names_across_restarts() {
    let scratch = Scratch::new();
    let id = {
        let engine = Engine::start(scratch.config()).unwrap();
        let mut events = engine.events();
        engine.create(secret("pw")).await.unwrap();
        let id = engine.import(secret(HARDHAT), secret("")).await.unwrap();
        assert_eq!(engine.wallets().await.unwrap()[0].name, None);
        engine
            .rename_wallet(id, "Savings".to_owned())
            .await
            .unwrap();
        let announced = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(Event::WalletsChanged(wallets)) = events.recv().await
                    && wallets[0].name.is_some()
                {
                    return wallets;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(announced[0].name.as_deref(), Some("Savings"));
        assert!(matches!(
            engine.rename_wallet(id, " ".to_owned()).await,
            Err(EngineError::Store(StoreError::Invalid(_)))
        ));
        engine.shutdown().await.unwrap();
        id
    };
    let engine = Engine::start(scratch.config()).unwrap();
    engine.unlock(secret("pw")).await.unwrap();
    let wallets = engine.wallets().await.unwrap();
    assert_eq!(
        (wallets[0].id, wallets[0].name.as_deref()),
        (id, Some("Savings"))
    );
    engine.remove(id).await.unwrap();
    assert_eq!(
        engine.rename_wallet(id, "Gone".to_owned()).await,
        Err(EngineError::Keyring(KeyringError::UnknownWallet))
    );
    engine.import(secret(HARDHAT), secret("")).await.unwrap();
    assert_eq!(engine.wallets().await.unwrap()[0].name, None);
}

#[tokio::test]
async fn the_network_list_is_shared_with_the_app() {
    let scratch = Scratch::new();
    let engine = Engine::start(scratch.config()).unwrap();
    assert_eq!(engine.registry().networks().count(), 0);
    let other = Scratch::new();
    let builtin = Engine::start(EngineConfig {
        registry: None,
        ..other.config()
    })
    .unwrap();
    assert!(builtin.registry().get(&"bsc".parse().unwrap()).is_some());
}

#[tokio::test]
async fn only_one_engine_may_use_the_data_folder() {
    let scratch = Scratch::new();
    let first = Engine::start(scratch.config()).unwrap();
    assert!(matches!(
        Engine::start(scratch.config()),
        Err(EngineError::Store(StoreError::AlreadyOpen))
    ));
    first.shutdown().await.unwrap();
    assert!(Engine::start(scratch.config()).is_ok());
}

#[cfg(unix)]
#[tokio::test]
async fn the_data_folder_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = Scratch::new();
    let config = scratch.config();
    let _engine = Engine::start(config.clone()).unwrap();
    let mode = std::fs::metadata(&config.data_dir)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o700);
}
