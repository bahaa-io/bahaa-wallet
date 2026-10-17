use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use rustls::ClientConfig;
use rustls_platform_verifier::ConfigVerifierExt;

pub struct Net {
    pub http: reqwest::Client,
    pub tls: Arc<ClientConfig>,
}

impl Net {
    pub fn new() -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("bahaa-probe/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(15))
            .build()?;
        let tls = Arc::new(ClientConfig::with_platform_verifier()?);
        Ok(Self { http, tls })
    }
}
