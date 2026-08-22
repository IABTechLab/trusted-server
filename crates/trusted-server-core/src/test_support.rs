#[cfg(test)]
pub mod tests {
    use crate::settings::Settings;
    use serde::Deserialize as _;

    /// Decode the one server-sealed lexical TSJS boot transport from generated HTML.
    ///
    /// # Panics
    ///
    /// Panics if the script omits the sealed transport or contains invalid JSON.
    #[must_use]
    pub fn bootstrap_transport(script: &str) -> serde_json::Value {
        let encoded = script
            .split_once("const __TSJS_SERVER_BOOT_TRANSPORT_V1__=")
            .expect("bootstrap should contain the sealed transport")
            .1;
        let mut deserializer = serde_json::Deserializer::from_str(encoded);
        let decoded = String::deserialize(&mut deserializer)
            .expect("transport should be one JSON string literal");
        serde_json::from_str(&decoded).expect("decoded transport should be exact JSON")
    }

    #[must_use]
    pub fn crate_test_settings_str() -> String {
        r#"
            [[handlers]]
            path = "^/secure"
            username = "user"
            password = "pass"

            [[handlers]]
            path = "^/_ts/admin"
            username = "admin"
            password = "admin-pass"

            [publisher]
            domain = "test-publisher.com"
            cookie_domain = ".test-publisher.com"
            origin_url = "https://origin.test-publisher.com"
            proxy_secret = "unit-test-proxy-secret"

            [integrations.prebid]
            enabled = true
            server_url = "https://test-prebid.com/openrtb2/auction"
            external_bundle_url = "https://assets.example/prebid/trusted-prebid.js"

            [integrations.nextjs]
            enabled = false
            rewrite_attributes = ["href", "link", "url"]

            [ec]
            passphrase = "test-secret-key-32-bytes-minimum"
            [request_signing]
            config_store_id = "test-config-store-id"
            secret_store_id = "test-secret-store-id"
            "#
        .to_owned()
    }

    #[must_use]
    /// Creates test settings from embedded TOML configuration.
    ///
    /// # Panics
    ///
    /// Panics if the embedded TOML configuration is invalid.
    pub fn create_test_settings() -> Settings {
        let toml_str = crate_test_settings_str();
        let mut settings = Settings::from_toml(&toml_str).expect("Invalid config");
        settings.proxy.allowed_domains = vec!["*.example".to_string(), "*.example.com".to_string()];
        settings
    }

    /// A valid EC ID in `{64-hex}.{6-alnum}` format for use in tests.
    pub const VALID_SYNTHETIC_ID: &str =
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.Ab1234";
}
