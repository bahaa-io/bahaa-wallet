use std::fmt;

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{AeadInOut, KeyInit};
use chacha20poly1305::{Tag, XChaCha20Poly1305, XNonce};
use secrecy::{ExposeSecret, ExposeSecretMut, SecretBox};
use thiserror::Error;
use zeroize::Zeroizing;

const MAGIC: &[u8; 8] = b"BAHAAVLT";
const VERSION: u8 = 1;
const KDF_ARGON2ID: u8 = 1;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const KEY_LEN: usize = 32;
const TAG_LEN: usize = 16;
const HEADER_LEN: usize = 8 + 1 + 1 + 4 * 3 + SALT_LEN;
const WRAP_LEN: usize = NONCE_LEN + KEY_LEN + TAG_LEN;
const MAX_PAYLOAD: usize = 1024 * 1024;
const MAX_MEMORY_KIB: u32 = 2 * 1024 * 1024;
const MAX_ITERATIONS: u32 = 64;
const MAX_LANES: u32 = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KdfParams {
    pub memory_kib: u32,
    pub iterations: u32,
    pub lanes: u32,
}

impl KdfParams {
    pub const RECOMMENDED: Self = Self {
        memory_kib: 256 * 1024,
        iterations: 3,
        lanes: 1,
    };

    fn argon2(self) -> Result<Argon2<'static>, VaultError> {
        if self.memory_kib > MAX_MEMORY_KIB
            || self.iterations > MAX_ITERATIONS
            || self.lanes > MAX_LANES
        {
            return Err(VaultError::Params);
        }
        let params = Params::new(self.memory_kib, self.iterations, self.lanes, Some(KEY_LEN))
            .map_err(|_| VaultError::Params)?;
        Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum VaultError {
    #[error("the password is empty")]
    EmptyPassword,
    #[error("wrong password")]
    WrongPassword,
    #[error("this is not a Bahaa vault, or it was made by a newer version")]
    Format,
    #[error("the vault file is damaged")]
    Damaged,
    #[error("the vault's key settings are out of the supported range")]
    Params,
    #[error("the vault holds at most {MAX_PAYLOAD} bytes")]
    TooLarge,
    #[error("the system random number generator failed")]
    Randomness,
    #[error("could not read or write the vault file: {0}")]
    Io(std::io::ErrorKind),
}

impl From<std::io::Error> for VaultError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err.kind())
    }
}

pub struct Vault {
    params: KdfParams,
    salt: [u8; SALT_LEN],
    wrap: [u8; WRAP_LEN],
    key: SecretBox<[u8]>,
}

impl Vault {
    pub fn create(password: &[u8], params: KdfParams) -> Result<Self, VaultError> {
        let mut key = SecretBox::new(vec![0u8; KEY_LEN].into_boxed_slice());
        fill(key.expose_secret_mut())?;
        let mut vault = Self {
            params,
            salt: [0; SALT_LEN],
            wrap: [0; WRAP_LEN],
            key,
        };
        vault.change_password(password, params)?;
        Ok(vault)
    }

    pub fn open(file: &[u8], password: &[u8]) -> Result<(Self, SecretBox<[u8]>), VaultError> {
        if password.is_empty() {
            return Err(VaultError::EmptyPassword);
        }
        let parsed = Parsed::read(file)?;
        let header = &file[..HEADER_LEN];
        let kek = derive(password, &parsed.salt, parsed.params)?;
        let (wrap_nonce, wrapped) = parsed.wrap.split_at(NONCE_LEN);
        let key = decrypt(&kek[..], wrap_nonce, header, wrapped)
            .map_err(|()| VaultError::WrongPassword)?;
        let payload = decrypt(key.expose_secret(), parsed.data_nonce, header, parsed.data)
            .map_err(|()| VaultError::Damaged)?;
        let vault = Self {
            params: parsed.params,
            salt: parsed.salt,
            wrap: parsed.wrap,
            key,
        };
        Ok((vault, payload))
    }

    pub fn seal(&self, payload: &[u8]) -> Result<Vec<u8>, VaultError> {
        if payload.len() > MAX_PAYLOAD {
            return Err(VaultError::TooLarge);
        }
        let header = self.header();
        let mut nonce = [0u8; NONCE_LEN];
        fill(&mut nonce)?;
        let sealed = encrypt(self.key.expose_secret(), &nonce, &header, payload)?;
        let length = u32::try_from(payload.len()).map_err(|_| VaultError::TooLarge)?;
        let mut file = Vec::with_capacity(HEADER_LEN + WRAP_LEN + NONCE_LEN + 4 + sealed.len());
        file.extend_from_slice(&header);
        file.extend_from_slice(&self.wrap);
        file.extend_from_slice(&nonce);
        file.extend_from_slice(&length.to_be_bytes());
        file.extend_from_slice(&sealed);
        Ok(file)
    }

    pub fn change_password(
        &mut self,
        password: &[u8],
        params: KdfParams,
    ) -> Result<(), VaultError> {
        if password.is_empty() {
            return Err(VaultError::EmptyPassword);
        }
        let mut salt = [0u8; SALT_LEN];
        fill(&mut salt)?;
        let kek = derive(password, &salt, params)?;
        self.params = params;
        self.salt = salt;
        let header = self.header();
        let mut nonce = [0u8; NONCE_LEN];
        fill(&mut nonce)?;
        let wrapped = encrypt(&kek[..], &nonce, &header, self.key.expose_secret())?;
        self.wrap[..NONCE_LEN].copy_from_slice(&nonce);
        self.wrap[NONCE_LEN..].copy_from_slice(&wrapped);
        Ok(())
    }

    pub fn params(&self) -> KdfParams {
        self.params
    }

    fn header(&self) -> [u8; HEADER_LEN] {
        let mut header = [0u8; HEADER_LEN];
        header[..8].copy_from_slice(MAGIC);
        header[8] = VERSION;
        header[9] = KDF_ARGON2ID;
        header[10..14].copy_from_slice(&self.params.memory_kib.to_be_bytes());
        header[14..18].copy_from_slice(&self.params.iterations.to_be_bytes());
        header[18..22].copy_from_slice(&self.params.lanes.to_be_bytes());
        header[22..].copy_from_slice(&self.salt);
        header
    }
}

impl fmt::Debug for Vault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Vault([redacted], {:?})", self.params)
    }
}

struct Parsed<'a> {
    params: KdfParams,
    salt: [u8; SALT_LEN],
    wrap: [u8; WRAP_LEN],
    data_nonce: &'a [u8],
    data: &'a [u8],
}

impl<'a> Parsed<'a> {
    fn read(file: &'a [u8]) -> Result<Self, VaultError> {
        let format = VaultError::Format;
        let (header, rest) = file.split_at_checked(HEADER_LEN).ok_or(format)?;
        if &header[..8] != MAGIC || header[8] != VERSION || header[9] != KDF_ARGON2ID {
            return Err(format);
        }
        let number = |at: usize| -> Result<u32, VaultError> {
            Ok(u32::from_be_bytes(
                header[at..at + 4].try_into().map_err(|_| format)?,
            ))
        };
        let params = KdfParams {
            memory_kib: number(10)?,
            iterations: number(14)?,
            lanes: number(18)?,
        };
        let (wrap, rest) = rest.split_at_checked(WRAP_LEN).ok_or(format)?;
        let (data_nonce, rest) = rest.split_at_checked(NONCE_LEN).ok_or(format)?;
        let (length, data) = rest.split_at_checked(4).ok_or(format)?;
        let length = u32::from_be_bytes(length.try_into().map_err(|_| format)?);
        let length = usize::try_from(length).map_err(|_| format)?;
        if length > MAX_PAYLOAD || data.len() != length + TAG_LEN {
            return Err(format);
        }
        Ok(Self {
            params,
            salt: header[22..].try_into().map_err(|_| format)?,
            wrap: wrap.try_into().map_err(|_| format)?,
            data_nonce,
            data,
        })
    }
}

fn derive(
    password: &[u8],
    salt: &[u8],
    params: KdfParams,
) -> Result<Zeroizing<[u8; KEY_LEN]>, VaultError> {
    let mut kek = Zeroizing::new([0u8; KEY_LEN]);
    params
        .argon2()?
        .hash_password_into(password, salt, &mut kek[..])
        .map_err(|_| VaultError::Params)?;
    Ok(kek)
}

fn cipher(key: &[u8]) -> Result<XChaCha20Poly1305, VaultError> {
    XChaCha20Poly1305::new_from_slice(key).map_err(|_| VaultError::Format)
}

fn encrypt(key: &[u8], nonce: &[u8], header: &[u8], plain: &[u8]) -> Result<Vec<u8>, VaultError> {
    let nonce = XNonce::try_from(nonce).map_err(|_| VaultError::Format)?;
    let mut buffer = Zeroizing::new(plain.to_vec());
    let tag = cipher(key)?
        .encrypt_inout_detached(&nonce, header, buffer.as_mut_slice().into())
        .map_err(|_| VaultError::TooLarge)?;
    let mut sealed = Vec::with_capacity(buffer.len() + TAG_LEN);
    sealed.extend_from_slice(&buffer);
    sealed.extend_from_slice(&tag);
    Ok(sealed)
}

fn decrypt(key: &[u8], nonce: &[u8], header: &[u8], sealed: &[u8]) -> Result<SecretBox<[u8]>, ()> {
    let nonce = XNonce::try_from(nonce).map_err(|_| ())?;
    let (body, tag) = sealed
        .split_at_checked(sealed.len().checked_sub(TAG_LEN).ok_or(())?)
        .ok_or(())?;
    let tag = Tag::try_from(tag).map_err(|_| ())?;
    let mut buffer = Zeroizing::new(body.to_vec());
    cipher(key)
        .map_err(|_| ())?
        .decrypt_inout_detached(&nonce, header, buffer.as_mut_slice().into(), &tag)
        .map_err(|_| ())?;
    let mut plain = SecretBox::new(vec![0u8; buffer.len()].into_boxed_slice());
    plain.expose_secret_mut().copy_from_slice(&buffer);
    Ok(plain)
}

fn fill(bytes: &mut [u8]) -> Result<(), VaultError> {
    getrandom::fill(bytes).map_err(|_| VaultError::Randomness)
}
