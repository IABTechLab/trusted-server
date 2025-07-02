use std::str;

use config::{Config, ConfigError, Environment, File, FileFormat};
use serde::Deserialize;

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
pub struct Settings {
    pub ad_server: AdServer,
    pub prebid: Prebid,
    pub synthetic: Synthetic,
}

impl Settings {
    pub fn new() -> Result<Self, ConfigError> {
        let toml_bytes = include_bytes!("../../../trusted-server.toml");
        let toml_str = str::from_utf8(toml_bytes).unwrap();

        Self::from_toml(toml_str)
    }

    pub fn from_toml(toml_str: &str) -> Result<Self, ConfigError> {
        let environment = Environment::default()
            .prefix("TRUSTED_SERVER")
            .separator("__");

        let toml = File::from_str(toml_str, FileFormat::Toml);
        let config = Config::builder()
            .add_source(toml)
            .add_source(environment)
            .build()?;

        // You can deserialize (and thus freeze) the entire configuration as
        config.try_deserialize()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_settings_new() {
        // Test that Settings::new() loads successfully
        let settings = Settings::new();
        assert!(settings.is_ok(), "Settings should load from embedded TOML");

        let settings = settings.unwrap();
        // Verify basic structure is loaded
        assert!(!settings.ad_server.ad_partner_url.is_empty());
        assert!(!settings.ad_server.sync_url.is_empty());
        assert!(!settings.prebid.server_url.is_empty());
        assert!(!settings.synthetic.counter_store.is_empty());
        assert!(!settings.synthetic.opid_store.is_empty());
        assert!(!settings.synthetic.secret_key.is_empty());
        assert!(!settings.synthetic.template.is_empty());
    }

    #[test]
    fn test_settings_from_valid_toml() {
        let toml_str = r#"
            [ad_server]
            ad_partner_url = "https://example-ad.com/serve"
            sync_url = "https://example-ad.com/sync"

            [prebid]
            server_url = "https://prebid.example.com/openrtb2/auction"

            [synthetic]
            counter_store = "test-counter-store"
            opid_store = "test-opid-store"
            secret_key = "test-secret-key-1234567890"
            template = "{{client_ip}}:{{user_agent}}:{{first_party_id}}:{{auth_user_id}}:{{publisher_domain}}:{{accept_language}}"
            "#;

        let settings = Settings::from_toml(toml_str);
        assert!(settings.is_ok());

        let settings = settings.unwrap();
        assert_eq!(
            settings.ad_server.ad_partner_url,
            "https://example-ad.com/serve"
        );
        assert_eq!(settings.ad_server.sync_url, "https://example-ad.com/sync");
        assert_eq!(
            settings.prebid.server_url,
            "https://prebid.example.com/openrtb2/auction"
        );
        assert_eq!(settings.synthetic.counter_store, "test-counter-store");
        assert_eq!(settings.synthetic.opid_store, "test-opid-store");
        assert_eq!(settings.synthetic.secret_key, "test-secret-key-1234567890");
        assert!(settings.synthetic.template.contains("{{client_ip}}"));
    }

    #[test]
    fn test_settings_missing_required_fields() {
        let toml_str = r#"
            [ad_server]
            ad_partner_url = "https://example-ad.com/serve"
            # Missing sync_url

            [prebid]
            server_url = "https://prebid.example.com/openrtb2/auction"

            [synthetic]
            counter_store = "test-counter-store"
            opid_store = "test-opid-store"
            secret_key = "test-secret-key"
            template = "{{client_ip}}"
            "#;

        let settings = Settings::from_toml(toml_str);
        assert!(
            settings.is_err(),
            "Should fail when required fields are missing"
        );
    }

    #[test]
    fn test_settings_empty_toml() {
        let toml_str = "";
        let settings = Settings::from_toml(toml_str);

        assert!(settings.is_err(), "Should fail with empty TOML");
    }

    #[test]
    fn test_settings_invalid_toml_syntax() {
        let toml_str = r#"
            [ad_server
            ad_partner_url = "https://example-ad.com/serve"
            "#;

        let settings = Settings::from_toml(toml_str);
        assert!(settings.is_err(), "Should fail with invalid TOML syntax");
    }

    #[test]
    fn test_settings_partial_config() {
        let toml_str = r#"
            [ad_server]
            ad_partner_url = "https://example-ad.com/serve"
            sync_url = "https://example-ad.com/sync"
            "#;

        let settings = Settings::from_toml(toml_str);
        assert!(settings.is_err(), "Should fail when sections are missing");
    }

    #[test]
    fn test_settings_extra_fields() {
        let toml_str = r#"
        [ad_server]
        ad_partner_url = "https://example-ad.com/serve"
        sync_url = "https://example-ad.com/sync"
        extra_field = "should be ignored"

        [prebid]
        server_url = "https://prebid.example.com/openrtb2/auction"

        [synthetic]
        counter_store = "test-counter-store"
        opid_store = "test-opid-store"
        secret_key = "test-secret-key-1234567890"
        template = "{{client_ip}}"
        "#;

        let settings = Settings::from_toml(toml_str);
        assert!(settings.is_ok(), "Extra fields should be ignored");
    }

    #[test]
    fn test_set_env() {
        let toml_str = r#"
            [ad_server]
            # ad_partner_url will be set by env variable
            sync_url = "https://example-ad.com/sync"

            [prebid]
            server_url = "https://prebid.example.com/openrtb2/auction"

            [synthetic] 
            counter_store = "test-counter-stor  e"
            opid_store = "test-opid-store"
            secret_key = "test-secret-key-1234567890"
            template = "{{client_ip}}"
            "#;

        temp_env::with_var(
            "TRUSTED_SERVER__AD_SERVER__AD_PARTNER_URL",
            Some("https://change-ad.com/serve"),
            || {
                let settings = Settings::from_toml(toml_str);

                assert!(settings.is_ok(), "Settings should load from embedded TOML");
                assert_eq!(
                    settings.unwrap().ad_server.ad_partner_url,
                    "https://change-ad.com/serve"
                );
            },
        );
    }

    #[test]
    fn test_override_env() {
        let toml_str = r#"
            [ad_server]
            ad_partner_url = "https://example-ad.com/serve"
            sync_url = "https://example-ad.com/sync"

            [prebid]
            server_url = "https://prebid.example.com/openrtb2/auction"

            [synthetic] 
            counter_store = "test-counter-stor  e"
            opid_store = "test-opid-store"
            secret_key = "test-secret-key-1234567890"
            template = "{{client_ip}}"
            "#;

        temp_env::with_var(
            "TRUSTED_SERVER__AD_SERVER__AD_PARTNER_URL",
            Some("https://change-ad.com/serve"),
            || {
                let settings = Settings::from_toml(toml_str);

                assert!(settings.is_ok(), "Settings should load from embedded TOML");
                assert_eq!(
                    settings.unwrap().ad_server.ad_partner_url,
                    "https://change-ad.com/serve"
                );
            },
        );
    }
}
