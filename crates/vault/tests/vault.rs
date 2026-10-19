#![allow(clippy::unwrap_used)]

use std::fmt::Write;
use std::path::PathBuf;

use bahaa_vault::{KdfParams, Vault, VaultError};
use secrecy::ExposeSecret;

const FAST: KdfParams = KdfParams {
    memory_kib: 64,
    iterations: 1,
    lanes: 1,
};
const PASSWORD: &[u8] = b"correct horse battery staple";
const PAYLOAD: &[u8] = b"entropy and other secrets, 32 b.";

fn sealed() -> Vec<u8> {
    Vault::create(PASSWORD, FAST)
        .unwrap()
        .seal(PAYLOAD)
        .unwrap()
}

fn scratch_dir() -> PathBuf {
    let mut random = [0u8; 8];
    getrandom::fill(&mut random).unwrap();
    let name: String = random.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    });
    let dir = std::env::temp_dir().join(format!("bahaa-vault-test-{name}"));
    std::fs::create_dir(&dir).unwrap();
    dir
}

#[test]
fn opens_what_it_sealed() {
    let (vault, payload) = Vault::open(&sealed(), PASSWORD).unwrap();
    assert_eq!(payload.expose_secret(), PAYLOAD);
    assert_eq!(vault.params(), FAST);
}

#[test]
fn refuses_wrong_and_empty_passwords() {
    assert_eq!(
        Vault::open(&sealed(), b"wrong").unwrap_err(),
        VaultError::WrongPassword
    );
    assert_eq!(
        Vault::open(&sealed(), b"").unwrap_err(),
        VaultError::EmptyPassword
    );
    assert_eq!(
        Vault::create(b"", FAST).unwrap_err(),
        VaultError::EmptyPassword
    );
}

#[test]
fn every_byte_of_the_file_is_protected() {
    let file = sealed();
    for position in 0..file.len() {
        let mut tampered = file.clone();
        tampered[position] ^= 1;
        assert!(
            Vault::open(&tampered, PASSWORD).is_err(),
            "byte {position} was not protected"
        );
    }
}

#[test]
fn rejects_truncated_or_padded_files() {
    let file = sealed();
    for len in [0, 8, 37, 38, 110, 138, file.len() - 1] {
        assert_eq!(
            Vault::open(&file[..len], PASSWORD).unwrap_err(),
            VaultError::Format,
            "{len}"
        );
    }
    let mut longer = file.clone();
    longer.push(0);
    assert_eq!(
        Vault::open(&longer, PASSWORD).unwrap_err(),
        VaultError::Format
    );
    assert_eq!(
        Vault::open(b"not a vault at all, just some text", PASSWORD).unwrap_err(),
        VaultError::Format
    );
}

#[test]
fn hostile_settings_are_refused_before_any_work() {
    let mut file = sealed();
    file[10..14].copy_from_slice(&(4u32 * 1024 * 1024).to_be_bytes());
    assert_eq!(
        Vault::open(&file, PASSWORD).unwrap_err(),
        VaultError::Params
    );
    let mut file = sealed();
    file[14..18].copy_from_slice(&1000u32.to_be_bytes());
    assert_eq!(
        Vault::open(&file, PASSWORD).unwrap_err(),
        VaultError::Params
    );
}

#[test]
fn changing_the_password_keeps_the_secrets() {
    let (mut vault, payload) = Vault::open(&sealed(), PASSWORD).unwrap();
    vault.change_password(b"a new password", FAST).unwrap();
    let file = vault.seal(payload.expose_secret()).unwrap();
    assert_eq!(
        Vault::open(&file, PASSWORD).unwrap_err(),
        VaultError::WrongPassword
    );
    let (_, again) = Vault::open(&file, b"a new password").unwrap();
    assert_eq!(again.expose_secret(), PAYLOAD);
}

#[test]
fn every_seal_looks_different() {
    let vault = Vault::create(PASSWORD, FAST).unwrap();
    let first = vault.seal(PAYLOAD).unwrap();
    let second = vault.seal(PAYLOAD).unwrap();
    assert_ne!(first, second);
    assert_eq!(
        Vault::open(&second, PASSWORD).unwrap().1.expose_secret(),
        PAYLOAD
    );
}

#[test]
fn the_payload_has_a_size_limit() {
    let vault = Vault::create(PASSWORD, FAST).unwrap();
    assert_eq!(
        vault.seal(&vec![0; 1024 * 1024 + 1]).unwrap_err(),
        VaultError::TooLarge
    );
    assert!(vault.seal(&[]).is_ok());
}

#[test]
fn debug_output_never_shows_secrets() {
    let (vault, payload) = Vault::open(&sealed(), PASSWORD).unwrap();
    let shown = format!("{vault:?} {payload:?}");
    assert!(shown.contains("redacted") && shown.contains("REDACTED"));
    assert!(!shown.contains("entropy"));
}

#[test]
fn recommended_settings_work() {
    let vault = Vault::create(PASSWORD, KdfParams::RECOMMENDED).unwrap();
    let file = vault.seal(PAYLOAD).unwrap();
    assert_eq!(
        Vault::open(&file, PASSWORD).unwrap().0.params(),
        KdfParams::RECOMMENDED
    );
}

#[test]
fn files_are_written_atomically_and_privately() {
    let dir = scratch_dir();
    let path = dir.join("vault.bin");
    assert_eq!(
        bahaa_vault::read(&path).unwrap_err(),
        VaultError::Io(std::io::ErrorKind::NotFound)
    );
    bahaa_vault::write_atomic(&path, b"first").unwrap();
    bahaa_vault::write_atomic(&path, b"second").unwrap();
    assert_eq!(bahaa_vault::read(&path).unwrap(), b"second");
    let names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, ["vault.bin"]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn subkeys_are_stable_per_vault_and_label() {
    let mut vault = Vault::create(PASSWORD, FAST).unwrap();
    let file = vault.seal(PAYLOAD).unwrap();
    let index = vault.subkey(b"index").unwrap();
    assert_ne!(*index, *vault.subkey(b"data").unwrap());
    let (reopened, _) = Vault::open(&file, PASSWORD).unwrap();
    assert_eq!(*reopened.subkey(b"index").unwrap(), *index);
    vault.change_password(b"another password", FAST).unwrap();
    let moved = vault.seal(PAYLOAD).unwrap();
    let (moved, _) = Vault::open(&moved, b"another password").unwrap();
    assert_eq!(*moved.subkey(b"index").unwrap(), *index);
    let other = Vault::create(PASSWORD, FAST).unwrap();
    assert_ne!(*other.subkey(b"index").unwrap(), *index);
}
