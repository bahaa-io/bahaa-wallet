use std::fmt;

use crypto_box::aead::Aead;
use crypto_box::{Nonce, PublicKey, SalsaBox, SecretKey};
use zeroize::Zeroizing;

use crate::ConnectError;

const NONCE: usize = 24;

pub struct SessionKey {
    secret: SecretKey,
}

impl SessionKey {
    pub fn generate() -> Result<Self, ConnectError> {
        let mut bytes = Zeroizing::new([0u8; 32]);
        getrandom::fill(bytes.as_mut_slice())
            .map_err(|_| ConnectError::Invalid("the system can't make random numbers"))?;
        Ok(Self::from_secret(&bytes))
    }

    pub fn from_secret(bytes: &Zeroizing<[u8; 32]>) -> Self {
        Self {
            secret: SecretKey::from(**bytes),
        }
    }

    pub fn secret(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.secret.to_bytes())
    }

    pub fn id(&self) -> [u8; 32] {
        self.secret.public_key().to_bytes()
    }

    pub fn seal(&self, to: &[u8; 32], message: &[u8]) -> Result<Vec<u8>, ConnectError> {
        let mut nonce = [0u8; NONCE];
        getrandom::fill(&mut nonce)
            .map_err(|_| ConnectError::Invalid("the system can't make random numbers"))?;
        let sealed = SalsaBox::new(&PublicKey::from(*to), &self.secret)
            .encrypt(Nonce::from_slice(&nonce), message)
            .map_err(|_| ConnectError::Sealed)?;
        Ok([nonce.as_slice(), &sealed].concat())
    }

    pub fn open(&self, from: &[u8; 32], sealed: &[u8]) -> Result<Vec<u8>, ConnectError> {
        if sealed.len() <= NONCE {
            return Err(ConnectError::Sealed);
        }
        let (nonce, body) = sealed.split_at(NONCE);
        SalsaBox::new(&PublicKey::from(*from), &self.secret)
            .decrypt(Nonce::from_slice(nonce), body)
            .map_err(|_| ConnectError::Sealed)
    }
}

impl fmt::Debug for SessionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionKey")
            .field("id", &bahaa_types::hex::encode(&self.id()))
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_two_session_ends_can_open_a_message() {
        let wallet = SessionKey::generate().unwrap();
        let app = SessionKey::generate().unwrap();
        let stranger = SessionKey::generate().unwrap();
        let sealed = wallet.seal(&app.id(), b"{\"event\":\"connect\"}").unwrap();
        assert_eq!(
            app.open(&wallet.id(), &sealed).unwrap(),
            b"{\"event\":\"connect\"}"
        );
        assert!(stranger.open(&wallet.id(), &sealed).is_err());
        let mut tampered = sealed.clone();
        if let Some(last) = tampered.last_mut() {
            *last ^= 1;
        }
        assert!(app.open(&wallet.id(), &tampered).is_err());
        assert!(app.open(&wallet.id(), &sealed[..10]).is_err());
        let again = SessionKey::from_secret(&wallet.secret());
        assert_eq!(again.id(), wallet.id());
        assert!(
            !format!("{again:?}").contains(&bahaa_types::hex::encode(wallet.secret().as_slice()))
        );
    }

    #[test]
    fn matches_a_nacl_box_made_by_tweetnacl() {
        let wallet = SessionKey::from_secret(&Zeroizing::new([1u8; 32]));
        let app = SessionKey::from_secret(&Zeroizing::new([2u8; 32]));
        let mut nonce = [0u8; NONCE];
        nonce[0] = 7;
        let sealed = SalsaBox::new(&PublicKey::from(app.id()), &wallet.secret)
            .encrypt(Nonce::from_slice(&nonce), b"hello".as_slice())
            .unwrap();
        assert_eq!(bahaa_types::hex::encode(&sealed), TWEETNACL_BOX);
    }

    const TWEETNACL_BOX: &str = "067e0cc1330d627ec39eb43e867de4de613d32c6e7";
}
