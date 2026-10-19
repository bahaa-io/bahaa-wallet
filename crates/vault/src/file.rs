use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::VaultError;

const MAX_FILE: u64 = 2 * 1024 * 1024;

pub fn read(path: &Path) -> Result<Vec<u8>, VaultError> {
    let file = File::open(path).map_err(VaultError::from)?;
    let size = file.metadata().map_err(VaultError::from)?.len();
    if size > MAX_FILE {
        return Err(VaultError::TooLarge);
    }
    let mut bytes = Vec::with_capacity(usize::try_from(size).map_err(|_| VaultError::TooLarge)?);
    file.take(MAX_FILE + 1)
        .read_to_end(&mut bytes)
        .map_err(VaultError::from)?;
    if u64::try_from(bytes.len()).map_or(true, |len| len > MAX_FILE) {
        return Err(VaultError::TooLarge);
    }
    Ok(bytes)
}

pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), VaultError> {
    let temporary = temporary_path(path)?;
    let written = write_new(&temporary, bytes)
        .and_then(|()| fs::rename(&temporary, path).map_err(VaultError::from));
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    written?;
    sync_directory(path)
}

fn temporary_path(path: &Path) -> Result<PathBuf, VaultError> {
    let name = path
        .file_name()
        .ok_or(VaultError::Io(std::io::ErrorKind::InvalidInput))?
        .to_string_lossy();
    let mut random = [0u8; 8];
    getrandom::fill(&mut random).map_err(|_| VaultError::Randomness)?;
    let suffix = u64::from_le_bytes(random);
    Ok(path.with_file_name(format!(".{name}.{suffix:016x}.tmp")))
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), VaultError> {
    let mut file = private(&mut OpenOptions::new())
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(VaultError::from)?;
    file.write_all(bytes).map_err(VaultError::from)?;
    file.sync_all().map_err(VaultError::from)
}

#[cfg(unix)]
fn private(options: &mut OpenOptions) -> &mut OpenOptions {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600)
}

#[cfg(not(unix))]
fn private(options: &mut OpenOptions) -> &mut OpenOptions {
    options
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), VaultError> {
    let directory = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    File::open(directory)
        .and_then(|dir| dir.sync_all())
        .map_err(VaultError::from)
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), VaultError> {
    Ok(())
}
