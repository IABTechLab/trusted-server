use config::{Config, ConfigError, Environment, File, FileFormat};
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
pub struct Synthetic {
    pub counter_store: String,
    pub opid_store: String,
    pub secret_key: String,
    pub template: String,
}

#[derive(Debug, Deserialize)]
#[allow(unused)]
pub struct Server {
    pub domain: String,
    pub cookie_domain: String,
}

#[derive(Debug, Deserialize)]
#[allow(unused)]
pub struct Settings {
    pub server: Server,
    pub ad_server: AdServer,
    pub prebid: Prebid,
    pub synthetic: Synthetic,
}

impl Settings {
    pub fn new() -> Result<Self, ConfigError> {
        let toml_bytes = include_bytes!("../../../trusted-server.toml");
        let toml_str = str::from_utf8(toml_bytes)
            .map_err(|e| ConfigError::Message(format!("Invalid UTF-8 in config: {}", e)))?;

        let builder = Config::builder()
            .add_source(File::from_str(toml_str, FileFormat::Toml))
            .add_source(Environment::with_prefix("TRUSTED_SERVER").separator("__"));

        let config = builder.build()?;

        // Validate that secret key is not the default
        if let Ok(secret_key) = config.get_string("synthetic.secret_key") {
            if secret_key == "trusted-server" {
                return Err(ConfigError::Message(
                    "Secret key must be changed from default. Set TRUSTED_SERVER__SYNTHETIC__SECRET_KEY environment variable.".into()
                ));
            }
        }

        config.try_deserialize()
    }
}
