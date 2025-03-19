use config::{Config, ConfigError, File, FileFormat};
use serde::Deserialize;
use std::str;

#[derive(Debug, Deserialize)]
#[allow(unused)]
struct AdServer {
    backend: String,
    server_url: String,
}

#[derive(Debug, Deserialize)]
#[allow(unused)]
struct Prebid {
    server_url: String,
}

#[derive(Debug, Deserialize)]
#[allow(unused)]
struct Synthetic {
    counter_store: String,
    opid_store: String,
    secret_key: String,
}

#[derive(Debug, Deserialize)]
#[allow(unused)]
pub(crate) struct Settings {
    ad_server: AdServer,
    prebid: Prebid,
    synthetic: Synthetic,
}

impl Settings {
    pub(crate) fn new() -> Result<Self, ConfigError> {
        let tom_bytes = include_bytes!("../potsi.toml");
        let toml_str = str::from_utf8(tom_bytes).unwrap();

        let s = Config::builder()
            .add_source(File::from_str(toml_str, FileFormat::Toml))
            .build()?;

        // You can deserialize (and thus freeze) the entire configuration as
        s.try_deserialize()
    }
}
