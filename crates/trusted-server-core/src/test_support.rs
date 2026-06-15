#[cfg(test)]
pub mod tests {
    use crate::settings::Settings;

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

            [integrations.nextjs]
            enabled = false
            rewrite_attributes = ["href", "link", "url"]

            [ec]
            passphrase = "test-secret-key-32-bytes-minimum"
            [request_signing]
            config_store_id = "test-config-store-id"
            secret_store_id = "test-secret-store-id"
            "#
        .to_string()
    }

    #[must_use]
    /// Creates test settings from embedded TOML configuration.
    ///
    /// # Panics
    ///
    /// Panics if the embedded TOML configuration is invalid.
    pub fn create_test_settings() -> Settings {
        let toml_str = crate_test_settings_str();
        Settings::from_toml(&toml_str).expect("Invalid config")
    }

    /// A valid EC ID in `{64-hex}.{6-alnum}` format for use in tests.
    pub const VALID_SYNTHETIC_ID: &str =
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.Ab1234";
}
