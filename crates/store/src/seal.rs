use std::fmt;
use std::fmt::Write as _;

use chacha20poly1305::aead::{AeadInOut, KeyInit};
use chacha20poly1305::{Tag, XChaCha20Poly1305, XNonce};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::StoreError;

const FORMAT: u8 = 1;
const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;
const KEY_LEN: usize = 32;

pub struct StoreKey {
    index: Zeroizing<[u8; KEY_LEN]>,
    data: Zeroizing<[u8; KEY_LEN]>,
}

impl StoreKey {
    pub fn new(index: Zeroizing<[u8; KEY_LEN]>, data: Zeroizing<[u8; KEY_LEN]>) -> Self {
        Self { index, data }
    }

    pub(crate) fn name(&self, table: &str, key: &str) -> Result<String, StoreError> {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&self.index[..]).map_err(broken)?;
        mac.update(table.as_bytes());
        mac.update(&[0]);
        mac.update(key.as_bytes());
        Ok(mac
            .finalize()
            .into_bytes()
            .iter()
            .fold(String::with_capacity(64), |mut out, byte| {
                let _ = write!(out, "{byte:02x}");
                out
            }))
    }

    pub(crate) fn seal(
        &self,
        table: &str,
        name: &str,
        plain: &[u8],
    ) -> Result<Vec<u8>, StoreError> {
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce).map_err(|_| StoreError::Invalid("no randomness"))?;
        let mut buffer = Zeroizing::new(plain.to_vec());
        let tag = self
            .cipher()?
            .encrypt_inout_detached(
                &XNonce::from(nonce),
                &bound(table, name),
                buffer.as_mut_slice().into(),
            )
            .map_err(broken)?;
        let mut sealed = Vec::with_capacity(1 + NONCE_LEN + buffer.len() + TAG_LEN);
        sealed.push(FORMAT);
        sealed.extend_from_slice(&nonce);
        sealed.extend_from_slice(&buffer);
        sealed.extend_from_slice(&tag);
        Ok(sealed)
    }

    pub(crate) fn open(
        &self,
        table: &str,
        name: &str,
        sealed: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, StoreError> {
        let unreadable = || StoreError::Corrupt("an encrypted entry cannot be opened".to_owned());
        let (format, rest) = sealed.split_first().ok_or_else(unreadable)?;
        if *format != FORMAT || rest.len() < NONCE_LEN + TAG_LEN {
            return Err(unreadable());
        }
        let (nonce, rest) = rest.split_at(NONCE_LEN);
        let (body, tag) = rest.split_at(rest.len() - TAG_LEN);
        let nonce = XNonce::try_from(nonce).map_err(|_| unreadable())?;
        let tag = Tag::try_from(tag).map_err(|_| unreadable())?;
        let mut buffer = Zeroizing::new(body.to_vec());
        self.cipher()?
            .decrypt_inout_detached(
                &nonce,
                &bound(table, name),
                buffer.as_mut_slice().into(),
                &tag,
            )
            .map_err(|_| unreadable())?;
        Ok(buffer)
    }

    fn cipher(&self) -> Result<XChaCha20Poly1305, StoreError> {
        XChaCha20Poly1305::new_from_slice(&self.data[..]).map_err(broken)
    }
}

impl fmt::Debug for StoreKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StoreKey([redacted])")
    }
}

fn bound(table: &str, name: &str) -> Vec<u8> {
    let mut aad = Vec::with_capacity(table.len() + 1 + name.len());
    aad.extend_from_slice(table.as_bytes());
    aad.push(0);
    aad.extend_from_slice(name.as_bytes());
    aad
}

fn broken<E>(_: E) -> StoreError {
    StoreError::Invalid("the store key is unusable")
}
