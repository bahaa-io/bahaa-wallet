use bahaa_types::Lock;

use crate::programs::{CORE, METADATA};
use crate::rpc::Account;
use crate::tx::Key;

pub(crate) const METADATA_V1: u8 = 4;
const CORE_ASSET: u8 = 1;
const CORE_COLLECTION: u8 = 5;
const MAX_STRING: usize = 200;
const CREATOR: usize = 34;
const MAX_CREATORS: usize = 5;
const NO_AUTHORITY: u8 = 0;
const ADDRESS_AUTHORITY: u8 = 1;
const COLLECTION_AUTHORITY: u8 = 2;
const USES: usize = 17;
const COLLECTION_DETAILS: u8 = 1;
const PROGRAMMABLE_V1: u8 = 0;
const TOKEN_RECORD: u8 = 11;
const UNLOCKED: u8 = 0;
const LISTED: u8 = 2;
const SALE: u8 = 0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Metadata {
    pub name: String,
    pub symbol: String,
    pub uri: String,
    pub standard: Option<u8>,
    pub collection: Option<(bool, Key)>,
    pub rules: Option<Key>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Core {
    pub owner: Key,
    pub collection: Option<Key>,
    pub name: String,
    pub uri: String,
}

pub(crate) struct Reader<'a>(pub(crate) &'a [u8]);

impl<'a> Reader<'a> {
    pub(crate) fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let (head, rest) = self.0.split_at_checked(count)?;
        self.0 = rest;
        Some(head)
    }

    pub(crate) fn byte(&mut self) -> Option<u8> {
        self.take(1).map(|byte| byte[0])
    }

    pub(crate) fn key(&mut self) -> Option<Key> {
        self.take(32)?.try_into().ok()
    }

    pub(crate) fn flag(&mut self) -> Option<bool> {
        match self.byte()? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }

    fn present(&mut self) -> Option<bool> {
        if self.0.is_empty() {
            Some(false)
        } else {
            self.flag()
        }
    }

    fn count(&mut self) -> Option<usize> {
        usize::try_from(u32::from_le_bytes(self.take(4)?.try_into().ok()?)).ok()
    }

    fn text(&mut self) -> Option<String> {
        let len = self.count().filter(|len| *len <= MAX_STRING)?;
        let raw = self.take(len)?;
        Some(
            String::from_utf8_lossy(raw)
                .trim_end_matches('\0')
                .to_owned(),
        )
    }
}

pub(crate) fn metadata(account: Option<&Account>, mint: &Key) -> Option<Metadata> {
    let account = account.filter(|account| account.owner == METADATA)?;
    let mut read = Reader(&account.data);
    if read.byte()? != METADATA_V1 {
        return None;
    }
    read.key()?;
    if read.key()? != *mint {
        return None;
    }
    let (name, symbol, uri) = (read.text()?, read.text()?, read.text()?);
    read.take(2)?;
    if read.present()? {
        let creators = read.count().filter(|count| *count <= MAX_CREATORS)?;
        read.take(creators * CREATOR)?;
    }
    if !read.0.is_empty() {
        read.take(2)?;
    }
    if read.present()? {
        read.byte()?;
    }
    let standard = if read.present()? {
        Some(read.byte()?)
    } else {
        None
    };
    let collection = if read.present()? {
        Some((read.flag()?, read.key()?))
    } else {
        None
    };
    Some(Metadata {
        name,
        symbol,
        uri,
        standard,
        collection,
        rules: rules(&mut read),
    })
}

fn rules(read: &mut Reader<'_>) -> Option<Key> {
    if read.present()? {
        read.take(USES)?;
    }
    if read.present()? {
        if read.byte()? > COLLECTION_DETAILS {
            return None;
        }
        read.take(8)?;
    }
    if !read.present()? || read.byte()? != PROGRAMMABLE_V1 || !read.flag()? {
        return None;
    }
    read.key()
}

pub(crate) fn token_lock(account: Option<&Account>) -> Option<Lock> {
    match token_state(account) {
        Some((UNLOCKED, role)) if role != Some(SALE) => None,
        Some((UNLOCKED | LISTED, _)) => Some(Lock::Listed),
        _ => Some(Lock::Frozen),
    }
}

fn token_state(account: Option<&Account>) -> Option<(u8, Option<u8>)> {
    let account = account.filter(|account| account.owner == METADATA)?;
    let mut read = Reader(&account.data);
    if read.byte()? != TOKEN_RECORD {
        return None;
    }
    read.byte()?;
    let state = read.byte()?;
    if read.flag()? {
        read.take(8)?;
    }
    if read.flag()? {
        read.key()?;
    }
    let role = if read.flag()? {
        Some(read.byte()?)
    } else {
        None
    };
    Some((state, role))
}

pub(crate) fn core(account: Option<&Account>) -> Option<Core> {
    let account = account.filter(|account| account.owner == CORE)?;
    let mut read = Reader(&account.data);
    if read.byte()? != CORE_ASSET {
        return None;
    }
    let owner = read.key()?;
    let collection = match read.byte()? {
        NO_AUTHORITY => None,
        ADDRESS_AUTHORITY => {
            read.key()?;
            None
        }
        COLLECTION_AUTHORITY => Some(read.key()?),
        _ => return None,
    };
    Some(Core {
        owner,
        collection,
        name: read.text()?,
        uri: read.text()?,
    })
}

pub(crate) fn core_collection(account: Option<&Account>) -> Option<String> {
    let account = account.filter(|account| account.owner == CORE)?;
    let mut read = Reader(&account.data);
    if read.byte()? != CORE_COLLECTION {
        return None;
    }
    read.key()?;
    read.text()
}
