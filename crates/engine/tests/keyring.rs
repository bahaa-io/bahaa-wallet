#![allow(clippy::unwrap_used)]

use std::fmt::Write;
use std::path::PathBuf;

use bahaa_crypto::{ExposeSecret, Mnemonic, SeedKind, TonPhraseError, WordCount};
use bahaa_engine::{Keyring, KeyringError, NewPhrase, PhraseKind, WalletKind};
use bahaa_types::TonContract;
use bahaa_vault::{KdfParams, Vault};

const FAST: KdfParams = KdfParams {
    memory_kib: 64,
    iterations: 1,
    lanes: 1,
};
const PASSWORD: &[u8] = b"correct horse battery staple";
const HARDHAT: &str = "test test test test test test test test test test test junk";
const TON: &str = "dose ice enrich trigger test dove century still betray gas diet dune use other base gym mad law immense village world example praise game";
const BOTH: &str = "floor doctor sight adult spare hope fitness limit together ski damp photo car season parade sure fade long diesel toward resource try mobile junk";

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).unwrap();
        let name = random.iter().fold(String::new(), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        });
        Self(std::env::temp_dir().join(format!("bahaa-engine-test-{name}")))
    }

    fn vault(&self) -> PathBuf {
        self.0.join("data").join("vault.bin")
    }

    fn keyring(&self) -> Keyring {
        Keyring::new(self.vault(), FAST)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn reopened(scratch: &Scratch) -> Keyring {
    let mut keyring = scratch.keyring();
    keyring.unlock(PASSWORD).unwrap();
    keyring
}

#[test]
fn creates_an_empty_vault_and_reopens_it() {
    let scratch = Scratch::new();
    let mut keyring = scratch.keyring();
    assert!(!keyring.exists());
    assert_eq!(keyring.unlock(PASSWORD).unwrap_err(), KeyringError::NoVault);
    keyring.create(PASSWORD).unwrap();
    assert!(keyring.is_unlocked());
    assert_eq!(keyring.wallets().unwrap(), []);
    keyring.lock();
    assert!(!keyring.is_unlocked());
    assert_eq!(keyring.wallets().unwrap_err(), KeyringError::Locked);
    assert_eq!(reopened(&scratch).wallets().unwrap(), []);
    assert_eq!(
        scratch.keyring().create(PASSWORD).unwrap_err(),
        KeyringError::AlreadyExists
    );
}

#[test]
fn a_generated_wallet_survives_a_restart_with_the_same_seed() {
    let scratch = Scratch::new();
    let mut keyring = scratch.keyring();
    keyring.create(PASSWORD).unwrap();
    let (id, phrase) = keyring.generate(NewPhrase::default(), "").unwrap();
    assert_eq!(phrase.expose_secret().split(' ').count(), 12);
    let (long, longer) = keyring.generate(NewPhrase::TwentyFour, "").unwrap();
    assert_eq!(longer.expose_secret().split(' ').count(), 24);
    keyring.remove(long).unwrap();
    let expected = Mnemonic::parse(phrase.expose_secret()).unwrap().to_seed("");
    drop(keyring);
    let keyring = reopened(&scratch);
    let wallets = keyring.wallets().unwrap();
    assert_eq!(wallets.len(), 1);
    assert_eq!(
        (wallets[0].id, wallets[0].words, wallets[0].passphrase),
        (id, WordCount::Twelve, false)
    );
    assert_eq!(keyring.seed(id).unwrap().expose(), expected.expose());
}

#[test]
fn any_standard_length_can_be_imported_even_though_new_phrases_are_12_or_24() {
    let scratch = Scratch::new();
    let mut keyring = scratch.keyring();
    keyring.create(PASSWORD).unwrap();
    let eighteen = "legal winner thank year wave sausage worth useful legal winner thank year wave sausage worth useful legal will";
    let id = keyring.import(eighteen, "").unwrap();
    let wallets = keyring.wallets().unwrap();
    assert_eq!((wallets[0].id, wallets[0].words), (id, WordCount::Eighteen));
    assert_eq!(NewPhrase::default(), NewPhrase::Twelve);
}

#[test]
fn imports_once_but_a_passphrase_makes_a_different_wallet() {
    let scratch = Scratch::new();
    let mut keyring = scratch.keyring();
    keyring.create(PASSWORD).unwrap();
    let plain = keyring.import(HARDHAT, "").unwrap();
    assert_eq!(
        keyring.import(&HARDHAT.to_uppercase(), "").unwrap_err(),
        KeyringError::Duplicate
    );
    let hidden = keyring.import(HARDHAT, "extra words").unwrap();
    assert_ne!(plain, hidden);
    let keyring = reopened(&scratch);
    let wallets = keyring.wallets().unwrap();
    assert_eq!(
        wallets.iter().map(|w| w.passphrase).collect::<Vec<_>>(),
        [false, true]
    );
    assert_ne!(
        keyring.seed(plain).unwrap().expose(),
        keyring.seed(hidden).unwrap().expose()
    );
}

#[test]
fn bad_phrases_are_explained() {
    let scratch = Scratch::new();
    let mut keyring = scratch.keyring();
    keyring.create(PASSWORD).unwrap();
    let typo = "test test test test test test test test test test test junky";
    assert!(matches!(
        keyring.import(typo, ""),
        Err(KeyringError::Mnemonic(_))
    ));
    assert!(matches!(
        keyring.import(&"test ".repeat(11), ""),
        Err(KeyringError::Mnemonic(_))
    ));
    assert_eq!(keyring.wallets().unwrap(), []);
}

#[test]
fn removed_wallets_stay_removed_and_ids_are_never_reused() {
    let scratch = Scratch::new();
    let mut keyring = scratch.keyring();
    keyring.create(PASSWORD).unwrap();
    let first = keyring.import(HARDHAT, "").unwrap();
    keyring.remove(first).unwrap();
    assert_eq!(
        keyring.remove(first).unwrap_err(),
        KeyringError::UnknownWallet
    );
    let mut keyring = reopened(&scratch);
    assert_eq!(keyring.wallets().unwrap(), []);
    let second = keyring.import(HARDHAT, "").unwrap();
    assert!(second > first);
}

#[test]
fn locked_keyrings_refuse_secret_work() {
    let scratch = Scratch::new();
    let mut keyring = scratch.keyring();
    keyring.create(PASSWORD).unwrap();
    let id = keyring.import(HARDHAT, "").unwrap();
    keyring.lock();
    assert!(matches!(keyring.seed(id), Err(KeyringError::Locked)));
    assert!(matches!(
        keyring.generate(NewPhrase::Twelve, ""),
        Err(KeyringError::Locked)
    ));
    assert!(matches!(
        keyring.import(HARDHAT, "x"),
        Err(KeyringError::Locked)
    ));
    assert!(matches!(
        keyring.reveal(id, PASSWORD),
        Err(KeyringError::Locked)
    ));
    assert_eq!(
        keyring.unlock(b"wrong").unwrap_err(),
        KeyringError::WrongPassword
    );
}

#[test]
fn revealing_a_phrase_asks_for_the_password_again() {
    let scratch = Scratch::new();
    let mut keyring = scratch.keyring();
    keyring.create(PASSWORD).unwrap();
    let id = keyring.import(HARDHAT, "").unwrap();
    assert!(matches!(
        keyring.reveal(id, b"guess"),
        Err(KeyringError::WrongPassword)
    ));
    assert_eq!(
        keyring.reveal(id, PASSWORD).unwrap().expose_secret(),
        HARDHAT
    );
}

#[test]
fn changing_the_password_keeps_every_wallet() {
    let scratch = Scratch::new();
    let mut keyring = scratch.keyring();
    keyring.create(PASSWORD).unwrap();
    let id = keyring.import(HARDHAT, "").unwrap();
    assert_eq!(
        keyring.change_password(b"guess", b"new one").unwrap_err(),
        KeyringError::WrongPassword
    );
    keyring.change_password(PASSWORD, b"new one").unwrap();
    let mut fresh = scratch.keyring();
    assert_eq!(
        fresh.unlock(PASSWORD).unwrap_err(),
        KeyringError::WrongPassword
    );
    fresh.unlock(b"new one").unwrap();
    assert_eq!(fresh.wallets().unwrap()[0].id, id);
}

#[test]
fn a_vault_with_damaged_contents_is_refused() {
    let scratch = Scratch::new();
    std::fs::create_dir_all(scratch.vault().parent().unwrap()).unwrap();
    let vault = Vault::create(PASSWORD, FAST).unwrap();
    for payload in [
        &b""[..],
        b"\x02",
        b"\x01\x00\x00\x00\x02\x00\x01\x00\x00\x00\x05\x10",
    ] {
        bahaa_vault::write_atomic(&scratch.vault(), &vault.seal(payload).unwrap()).unwrap();
        assert_eq!(
            scratch.keyring().unlock(PASSWORD).unwrap_err(),
            KeyringError::Damaged,
            "{payload:?}"
        );
    }
}

#[test]
fn debug_output_never_shows_secrets() {
    let scratch = Scratch::new();
    let mut keyring = scratch.keyring();
    keyring.create(PASSWORD).unwrap();
    keyring.import(HARDHAT, "").unwrap();
    let shown = format!("{keyring:?}");
    assert!(!shown.contains("test"));
    assert!(shown.contains("redacted"));
    keyring.lock();
    assert_eq!(format!("{keyring:?}"), "Keyring(locked)");
}

#[cfg(unix)]
#[test]
fn the_vault_folder_and_file_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = Scratch::new();
    scratch.keyring().create(PASSWORD).unwrap();
    let mode =
        |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(scratch.vault().parent().unwrap()), 0o700);
    assert_eq!(mode(&scratch.vault()), 0o600);
}

#[test]
fn a_ton_phrase_becomes_a_ton_only_wallet_that_survives_a_restart() {
    let scratch = Scratch::new();
    let mut keyring = scratch.keyring();
    keyring.create(PASSWORD).unwrap();
    let id = keyring.import(TON, "").unwrap();
    assert_eq!(
        keyring.import(&TON.to_uppercase(), "").unwrap_err(),
        KeyringError::Duplicate
    );
    assert_eq!(keyring.seed(id).unwrap().kind(), SeedKind::Ton);
    let keyring = reopened(&scratch);
    let wallets = keyring.wallets().unwrap();
    assert_eq!(
        (
            wallets[0].id,
            wallets[0].words,
            wallets[0].passphrase,
            wallets[0].kind,
            wallets[0].ton
        ),
        (
            id,
            WordCount::TwentyFour,
            false,
            WalletKind::Ton,
            TonContract::W5
        )
    );
    assert_eq!(keyring.reveal(id, PASSWORD).unwrap().expose_secret(), TON);
    assert_eq!(keyring.seed(id).unwrap().expose().len(), 32);
}

#[test]
fn ton_phrases_take_no_passphrase_and_bad_words_say_why() {
    let scratch = Scratch::new();
    let mut keyring = scratch.keyring();
    keyring.create(PASSWORD).unwrap();
    assert_eq!(
        keyring.import(TON, "extra").unwrap_err(),
        KeyringError::Mnemonic(bahaa_crypto::MnemonicError::Checksum)
    );
    assert_eq!(
        keyring
            .import_as(TON, "extra", PhraseKind::Ton)
            .unwrap_err(),
        KeyringError::TonPassphrase
    );
    let swapped = TON.replacen("dose ice", "ice dose", 1);
    assert_eq!(
        keyring
            .import_as(&swapped, "", PhraseKind::Ton)
            .unwrap_err(),
        KeyringError::TonPhrase(TonPhraseError::NotTon)
    );
    assert_eq!(
        keyring.import(&swapped, "").unwrap_err(),
        KeyringError::Mnemonic(bahaa_crypto::MnemonicError::Checksum)
    );
    assert_eq!(keyring.wallets().unwrap(), []);
}

#[test]
fn words_that_fit_both_standards_wait_for_the_users_choice() {
    let scratch = Scratch::new();
    let mut keyring = scratch.keyring();
    keyring.create(PASSWORD).unwrap();
    assert_eq!(
        keyring.import(BOTH, "").unwrap_err(),
        KeyringError::Ambiguous
    );
    let multi = keyring.import_as(BOTH, "", PhraseKind::Bip39).unwrap();
    let ton = keyring.import_as(BOTH, "", PhraseKind::Ton).unwrap();
    assert_ne!(multi, ton);
    let kinds: Vec<WalletKind> = keyring.wallets().unwrap().iter().map(|w| w.kind).collect();
    assert_eq!(kinds, [WalletKind::Bip39, WalletKind::Ton]);
    assert_eq!(keyring.seed(multi).unwrap().kind(), SeedKind::Bip39);
    assert_eq!(keyring.seed(ton).unwrap().kind(), SeedKind::Ton);
    let with_passphrase = keyring.import(BOTH, "extra").unwrap();
    assert_eq!(
        keyring.seed(with_passphrase).unwrap().kind(),
        SeedKind::Bip39
    );
}

#[test]
fn every_wallet_keeps_the_ton_contract_it_was_found_on() {
    let scratch = Scratch::new();
    let mut keyring = scratch.keyring();
    keyring.create(PASSWORD).unwrap();
    let ton = keyring.import(TON, "").unwrap();
    let multi = keyring.import(HARDHAT, "").unwrap();
    keyring.import(BOTH, "extra").unwrap();
    keyring.set_ton_contract(ton, TonContract::V4R2).unwrap();
    keyring.set_ton_contract(multi, TonContract::V4R2).unwrap();
    let keyring = reopened(&scratch);
    let kept: Vec<(WalletKind, TonContract)> = keyring
        .wallets()
        .unwrap()
        .iter()
        .map(|w| (w.kind, w.ton))
        .collect();
    assert_eq!(
        kept,
        [
            (WalletKind::Ton, TonContract::V4R2),
            (WalletKind::Bip39, TonContract::V4R2),
            (WalletKind::Bip39, TonContract::W5)
        ]
    );
}

#[test]
fn phrases_are_checked_with_the_rules_import_uses() {
    use bahaa_crypto::{MnemonicError, TonPhraseError};
    use bahaa_engine::check_phrase;
    assert_eq!(check_phrase(HARDHAT, ""), Ok(PhraseKind::Bip39));
    assert_eq!(check_phrase(HARDHAT, "extra words"), Ok(PhraseKind::Bip39));
    assert_eq!(check_phrase(TON, ""), Ok(PhraseKind::Ton));
    assert_eq!(check_phrase(BOTH, ""), Err(KeyringError::Ambiguous));
    assert_eq!(
        check_phrase(BOTH, "with a passphrase"),
        Ok(PhraseKind::Bip39)
    );
    assert_eq!(
        check_phrase("test test", ""),
        Err(KeyringError::Mnemonic(MnemonicError::WordCount(2)))
    );
    assert_eq!(
        check_phrase(&HARDHAT.replace("junk", "jjunk"), ""),
        Err(KeyringError::Mnemonic(MnemonicError::UnknownWord(12)))
    );
    assert_eq!(
        check_phrase(&HARDHAT.replace("junk", "test"), ""),
        Err(KeyringError::Mnemonic(MnemonicError::Checksum))
    );
    assert!(matches!(
        check_phrase(TON, "a passphrase"),
        Err(KeyringError::Mnemonic(_) | KeyringError::TonPhrase(TonPhraseError::NotTon))
    ));
}
