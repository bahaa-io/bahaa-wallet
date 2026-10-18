#![allow(clippy::unwrap_used)]

mod common;

use std::time::Duration;

use bahaa_config::Registry;
use bahaa_crypto::SecretString;
use bahaa_engine::{Caution, Contact, Engine, EngineError, Event, KeyringError, StoreError};
use bahaa_types::{Address, ChainFamily};
use common::{HARDHAT, HARDHAT_0, HARDHAT_1, Scratch, TRON_0, config};
use tokio::sync::broadcast;
use url::Url;

const LIKE_OURS: &str = "0xf39f000000000000000000000000000000002266";
const LIKE_FRIEND: &str = "0x70990000000000000000000000000000000079c8";

fn registry() -> Registry {
    let dead = Url::parse("http://127.0.0.1:9/").unwrap();
    Registry::with_networks(vec![
        common::network("bsc", dead.clone()),
        common::tron_network(dead),
    ])
    .unwrap()
}

fn evm(text: &str) -> Address {
    Address::parse(ChainFamily::Evm, text).unwrap()
}

fn contact(name: &str, address: &str) -> Contact {
    Contact {
        network: "bsc".parse().unwrap(),
        address: evm(address),
        name: name.to_owned(),
    }
}

async fn changed(events: &mut broadcast::Receiver<Event>) -> Vec<Contact> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(Event::ContactsChanged(contacts)) = events.recv().await {
                return contacts;
            }
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn contacts_are_saved_sorted_removed_and_kept() {
    let scratch = Scratch::new();
    let engine = Engine::start(config(&scratch, registry(), None)).unwrap();
    let mut events = engine.events();
    engine.create(SecretString::from("pw")).await.unwrap();
    engine
        .save_contact(contact("zoe", HARDHAT_1))
        .await
        .unwrap();
    assert_eq!(changed(&mut events).await, [contact("zoe", HARDHAT_1)]);
    engine
        .save_contact(contact("Amir", HARDHAT_0))
        .await
        .unwrap();
    assert_eq!(
        changed(&mut events).await,
        [contact("Amir", HARDHAT_0), contact("zoe", HARDHAT_1)]
    );
    engine
        .save_contact(contact("Zoe Smith", HARDHAT_1))
        .await
        .unwrap();
    assert_eq!(
        changed(&mut events).await,
        [contact("Amir", HARDHAT_0), contact("Zoe Smith", HARDHAT_1)]
    );
    assert_eq!(
        engine.contacts().await.unwrap(),
        [contact("Amir", HARDHAT_0), contact("Zoe Smith", HARDHAT_1)]
    );
    assert_eq!(
        engine
            .remove_contact("bsc".parse().unwrap(), evm(HARDHAT_0))
            .await,
        Ok(true)
    );
    assert_eq!(
        changed(&mut events).await,
        [contact("Zoe Smith", HARDHAT_1)]
    );
    assert_eq!(
        engine
            .remove_contact("bsc".parse().unwrap(), evm(HARDHAT_0))
            .await,
        Ok(false)
    );
    engine.shutdown().await.unwrap();
    let engine = Engine::start(config(&scratch, registry(), None)).unwrap();
    assert_eq!(
        engine.contacts().await,
        Err(EngineError::Keyring(KeyringError::Locked))
    );
    assert_eq!(
        engine.save_contact(contact("Amir", HARDHAT_0)).await,
        Err(EngineError::Keyring(KeyringError::Locked))
    );
    engine.unlock(SecretString::from("pw")).await.unwrap();
    assert_eq!(
        engine.contacts().await.unwrap(),
        [contact("Zoe Smith", HARDHAT_1)]
    );
}

#[tokio::test]
async fn a_contact_must_fit_its_network_and_have_a_plain_name() {
    let scratch = Scratch::new();
    let engine = Engine::start(config(&scratch, registry(), None)).unwrap();
    engine.create(SecretString::from("pw")).await.unwrap();
    let elsewhere = Contact {
        network: "ethereum".parse().unwrap(),
        ..contact("Amir", HARDHAT_0)
    };
    assert_eq!(
        engine.save_contact(elsewhere).await,
        Err(EngineError::UnknownNetwork)
    );
    let mixed = Contact {
        address: Address::parse(ChainFamily::Tron, TRON_0).unwrap(),
        ..contact("Amir", HARDHAT_0)
    };
    assert!(matches!(
        engine.save_contact(mixed).await,
        Err(EngineError::Chain(bahaa_chain::ChainError::Invalid(_)))
    ));
    for name in ["", "a\u{202e}b", &"x".repeat(41)] {
        assert!(matches!(
            engine.save_contact(contact(name, HARDHAT_0)).await,
            Err(EngineError::Store(StoreError::Invalid(_)))
        ));
    }
    let tron = Contact {
        network: "tron".parse().unwrap(),
        address: Address::parse(ChainFamily::Tron, TRON_0).unwrap(),
        name: "Tron friend".to_owned(),
    };
    engine.save_contact(tron.clone()).await.unwrap();
    assert_eq!(engine.contacts().await.unwrap(), [tron]);
}

#[tokio::test]
async fn any_address_can_be_checked_before_it_is_trusted() {
    let scratch = Scratch::new();
    let engine = Engine::start(config(&scratch, registry(), None)).unwrap();
    engine.create(SecretString::from("pw")).await.unwrap();
    engine
        .import(SecretString::from(HARDHAT), SecretString::from(""))
        .await
        .unwrap();
    assert_eq!(
        engine.cautions(evm(LIKE_OURS)).await.unwrap(),
        [Caution::LooksLike(evm(HARDHAT_0))]
    );
    assert_eq!(engine.cautions(evm(LIKE_FRIEND)).await.unwrap(), []);
    engine
        .save_contact(contact("Friend", HARDHAT_1))
        .await
        .unwrap();
    assert_eq!(
        engine.cautions(evm(LIKE_FRIEND)).await.unwrap(),
        [Caution::LooksLike(evm(HARDHAT_1))]
    );
    assert_eq!(engine.cautions(evm(HARDHAT_1)).await.unwrap(), []);
    engine.lock().await.unwrap();
    assert_eq!(
        engine.cautions(evm(LIKE_OURS)).await,
        Err(EngineError::Keyring(KeyringError::Locked))
    );
}
