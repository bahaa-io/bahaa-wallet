use std::env::consts::{ARCH, EXE_SUFFIX, OS};
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::Path;
use std::time::Duration;

use bahaa_app_core::to_plain_text;
use ed25519_dalek::{Signature, VerifyingKey};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::hints;

const RELEASES: &str = "https://api.github.com/repos/bahaa-io/bahaa-wallet/releases?per_page=30";
const SUMS: &str = "SHA256SUMS";
const SIGNATURE: &str = "SHA256SUMS.sig";
const RELEASE_KEY: [u8; 32] = [
    0xc8, 0x56, 0x87, 0x34, 0xfc, 0xbd, 0x2d, 0xc4, 0x4e, 0xab, 0x8b, 0xbb, 0x1d, 0x27, 0x75, 0x10,
    0x11, 0xa7, 0xb6, 0xfe, 0x5a, 0xa3, 0xb7, 0x9a, 0xc1, 0x95, 0x49, 0x46, 0x7e, 0x57, 0xfb, 0x28,
];
const LIST_LIMIT: usize = 4 << 20;
const SMALL_LIMIT: usize = 64 << 10;
const BINARY_LIMIT: usize = 256 << 20;
const NOTE_LINES: usize = 200;
const NOTE_WIDTH: usize = 400;
const CHECK_TIMEOUT: Duration = Duration::from_secs(20);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_mins(10);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Release {
    pub(crate) version: Version,
    pub(crate) notes: Vec<String>,
    binary: String,
    digest: String,
}

struct Candidate {
    version: Version,
    notes: Vec<String>,
    binary: String,
    sums: String,
    signature: String,
}

pub(crate) enum Done {
    Checked {
        asked: bool,
        result: Result<Option<Release>, &'static str>,
    },
    Installed(Result<Version, &'static str>),
}

#[derive(Deserialize)]
struct Listed {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    draft: bool,
    prerelease: bool,
    assets: Vec<Attached>,
}

#[derive(Deserialize)]
struct Attached {
    name: String,
    browser_download_url: String,
}

pub(crate) fn current() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or_else(|_| Version::new(0, 0, 0))
}

pub(crate) async fn check(beta: bool) -> Result<Option<Release>, &'static str> {
    let client = client(CHECK_TIMEOUT)?;
    let body = fetch(&client, RELEASES, LIST_LIMIT).await?;
    let listed: Vec<Listed> =
        serde_json::from_slice(&body).map_err(|_| hints::UPDATE_UNREADABLE)?;
    let Some(candidate) = newest(listed, beta, &current(), &asset()) else {
        return Ok(None);
    };
    let sums = fetch(&client, &candidate.sums, SMALL_LIMIT).await?;
    let signature = fetch(&client, &candidate.signature, SMALL_LIMIT).await?;
    Ok(Some(Release {
        digest: checksum(&sums, &signature, &RELEASE_KEY, &asset())?,
        version: candidate.version,
        notes: candidate.notes,
        binary: candidate.binary,
    }))
}

pub(crate) async fn install(release: Release) -> Result<Version, &'static str> {
    let binary = fetch(&client(DOWNLOAD_TIMEOUT)?, &release.binary, BINARY_LIMIT).await?;
    if !format!("{:x}", Sha256::digest(&binary)).eq_ignore_ascii_case(&release.digest) {
        return Err(hints::UPDATE_DAMAGED);
    }
    tokio::task::spawn_blocking(move || replace(&binary))
        .await
        .map_err(|_| hints::UPDATE_REPLACE)??;
    Ok(release.version)
}

fn asset() -> String {
    format!("bahaa-wallet-tui-{OS}-{ARCH}{EXE_SUFFIX}")
}

fn newest(listed: Vec<Listed>, beta: bool, current: &Version, asset: &str) -> Option<Candidate> {
    listed
        .into_iter()
        .filter(|listed| !listed.draft && (beta || !listed.prerelease))
        .filter_map(|listed| {
            let version = Version::parse(listed.tag_name.trim_start_matches('v')).ok()?;
            let url = |name: &str| {
                listed
                    .assets
                    .iter()
                    .find(|attached| attached.name == name)
                    .map(|attached| attached.browser_download_url.clone())
            };
            Some(Candidate {
                binary: url(asset)?,
                sums: url(SUMS)?,
                signature: url(SIGNATURE)?,
                notes: notes(listed.body.as_deref().unwrap_or_default()),
                version,
            })
        })
        .filter(|candidate| candidate.version > *current)
        .max_by(|a, b| a.version.cmp(&b.version))
}

fn notes(body: &str) -> Vec<String> {
    body.lines()
        .map(|line| to_plain_text(line, NOTE_WIDTH))
        .filter(|line| !line.is_empty())
        .take(NOTE_LINES)
        .collect()
}

fn checksum(
    sums: &[u8],
    signature: &[u8],
    key: &[u8; 32],
    asset: &str,
) -> Result<String, &'static str> {
    let key = VerifyingKey::from_bytes(key).map_err(|_| hints::UPDATE_UNSIGNED)?;
    let signature = Signature::from_slice(signature).map_err(|_| hints::UPDATE_UNSIGNED)?;
    key.verify_strict(sums, &signature)
        .map_err(|_| hints::UPDATE_UNSIGNED)?;
    let text = std::str::from_utf8(sums).map_err(|_| hints::UPDATE_UNSIGNED)?;
    text.lines()
        .find_map(|line| {
            let (digest, name) = line.split_once(' ')?;
            (name.trim_start_matches([' ', '*']) == asset).then(|| digest.to_owned())
        })
        .ok_or(hints::UPDATE_MISSING)
}

fn client(timeout: Duration) -> Result<reqwest::Client, &'static str> {
    reqwest::Client::builder()
        .https_only(true)
        .timeout(timeout)
        .user_agent(concat!("bahaa-wallet/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| hints::UPDATE_UNREACHABLE)
}

async fn fetch(client: &reqwest::Client, url: &str, limit: usize) -> Result<Vec<u8>, &'static str> {
    let mut response = client
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|_| hints::UPDATE_UNREACHABLE)?;
    let most = u64::try_from(limit).unwrap_or(u64::MAX);
    if response
        .content_length()
        .is_some_and(|length| length > most)
    {
        return Err(hints::UPDATE_TOO_BIG);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| hints::UPDATE_UNREACHABLE)?
    {
        if body.len() + chunk.len() > limit {
            return Err(hints::UPDATE_TOO_BIG);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn replace(binary: &[u8]) -> Result<(), &'static str> {
    let mut random = [0u8; 8];
    getrandom::fill(&mut random).map_err(|_| hints::UPDATE_REPLACE)?;
    let path = std::env::temp_dir().join(format!(
        "bahaa-wallet-update-{:016x}{EXE_SUFFIX}",
        u64::from_le_bytes(random)
    ));
    let written = write_new(&path, binary).and_then(|()| self_replace::self_replace(&path));
    let _ = std::fs::remove_file(&path);
    written.map_err(|_| hints::UPDATE_REPLACE)
}

fn write_new(path: &Path, binary: &[u8]) -> std::io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o755);
    }
    let mut file = options.open(path)?;
    file.write_all(binary)?;
    file.sync_all()
}

#[cfg(test)]
pub(crate) mod tests {
    use ed25519_dalek::{Signer, SigningKey};

    use super::*;

    const LINUX: &str = "bahaa-wallet-tui-linux-x86_64";

    pub(crate) fn release(version: &str, notes: &[&str]) -> Release {
        Release {
            version: Version::parse(version).unwrap(),
            notes: notes.iter().map(|note| (*note).to_owned()).collect(),
            binary: String::new(),
            digest: String::new(),
        }
    }

    fn signer() -> SigningKey {
        SigningKey::from_bytes(&[7; 32])
    }

    fn listed(tag: &str, prerelease: bool, files: &[&str]) -> Listed {
        Listed {
            tag_name: format!("v{tag}"),
            body: Some(format!("- changes in {tag}\n\n- second line")),
            draft: false,
            prerelease,
            assets: files
                .iter()
                .map(|name| Attached {
                    name: (*name).to_owned(),
                    browser_download_url: format!("https://example.com/{tag}/{name}"),
                })
                .collect(),
        }
    }

    #[test]
    fn the_newest_complete_release_for_the_channel_wins() {
        let all = [SUMS, SIGNATURE, LINUX];
        let list = || {
            vec![
                listed("0.6.0-beta.1", true, &all),
                listed("0.5.1", false, &all),
                listed("0.5.2", false, &[SUMS, SIGNATURE]),
                listed("0.5.3", false, &[SUMS, LINUX]),
                listed("0.4.0", false, &all),
            ]
        };
        let current = Version::new(0, 5, 0);
        let stable = newest(list(), false, &current, LINUX).unwrap();
        assert_eq!(stable.version, Version::new(0, 5, 1));
        assert_eq!(stable.notes, ["- changes in 0.5.1", "- second line"]);
        assert_eq!(stable.sums, format!("https://example.com/0.5.1/{SUMS}"));
        let beta = newest(list(), true, &current, LINUX).unwrap();
        assert_eq!(beta.version.to_string(), "0.6.0-beta.1");
        assert!(newest(list(), false, &Version::new(0, 5, 1), LINUX).is_none());
    }

    #[test]
    fn only_checksums_signed_with_the_release_key_are_trusted() {
        let signer = signer();
        let key = signer.verifying_key().to_bytes();
        let sums =
            b"ab12  bahaa-wallet-tui-linux-x86_64\ncd34  bahaa-wallet-tui-windows-x86_64.exe\n";
        let signature = signer.sign(sums).to_bytes();
        assert_eq!(
            checksum(
                sums,
                &signature,
                &key,
                "bahaa-wallet-tui-windows-x86_64.exe"
            ),
            Ok("cd34".to_owned())
        );
        assert_eq!(
            checksum(sums, &signature, &key, "bahaa-wallet-tui-macos-aarch64"),
            Err(hints::UPDATE_MISSING)
        );
        let mut tampered = sums.to_vec();
        tampered[0] = b'f';
        assert_eq!(
            checksum(&tampered, &signature, &key, LINUX),
            Err(hints::UPDATE_UNSIGNED)
        );
        assert_eq!(
            checksum(sums, &signature, &RELEASE_KEY, LINUX),
            Err(hints::UPDATE_UNSIGNED)
        );
    }
}
