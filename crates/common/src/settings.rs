use config::{Config, ConfigError, File, FileFormat};
use serde::Deserialize;
use std::str;

#[derive(Debug, Deserialize)]
#[allow(unused)]
pub struct AdServer {
    pub ad_partner_url: String,
    pub sync_url: String,
}

#[derive(Debug, Deserialize)]
#[allow(unused)]
pub struct Prebid {
    pub server_url: String,
}

#[derive(Debug, Deserialize)]
#[allow(unused)]
pub struct GamAdUnit {
    pub name: String,
    pub size: String,
}

#[derive(Debug, Deserialize)]
#[allow(unused)]
pub struct Gam {
    pub publisher_id: String,
    pub server_url: String,
    pub ad_units: Vec<GamAdUnit>,
}

#[derive(Debug, Deserialize)]
#[allow(unused)]
pub struct Synthetic {
    pub counter_store: String,
    pub opid_store: String,
    pub secret_key: String,
    pub template: String,
}

#[derive(Debug, Deserialize)]
#[allow(unused)]
pub struct Settings {
    pub ad_server: AdServer,
    pub prebid: Prebid,
    pub gam: Gam,
    pub synthetic: Synthetic,
}

impl Settings {
    pub fn new() -> Result<Self, ConfigError> {
        let toml_bytes = include_bytes!("../../../trusted-server.toml");
        let toml_str = str::from_utf8(toml_bytes).unwrap();

        let s = Config::builder()
            .add_source(File::from_str(toml_str, FileFormat::Toml))
            .build()?;

        // You can deserialize (and thus freeze) the entire configuration as
        s.try_deserialize()
    }
}
