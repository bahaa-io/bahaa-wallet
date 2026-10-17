use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;

pub fn unix_now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}
