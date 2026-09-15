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

            [geo]
            # A gdpr-eu country, where every permission requires a signal. This
            # reproduces the prior no-default floor, so existing tests are
            # unaffected by the now-required default.
            # Tests run with no geo provider, so single-jurisdiction operation
            # is acknowledged the same way a deployment would.
            assume_single_jurisdiction = true

            [integration]
            provider = ["prebid"]

            [integration.prebid]
            external_bundle_url = "https://assets.example/prebid/trusted-prebid.js"

            [integration.prebid.bundle]
            adapters = ["exampleBidder"]

            [ec]
            provider = "hmac"

            [ec.providers.hmac]
            passphrase = "test-secret-key-32-bytes-minimum"

            [request_signing]
            config_store_id = "test-config-store-id"
            secret_store_id = "test-secret-store-id"
            "#
        .to_owned()
    }

    /// The shared fixture TOML with `integration_id` named in
    /// `[integration] provider` as well, for a test that appends that
    /// integration's own block.
    #[must_use]
    pub fn crate_test_settings_str_running(integration_id: &str) -> String {
        crate_test_settings_str().replace(
            "provider = [\"prebid\"]",
            &format!("provider = [\"prebid\", \"{integration_id}\"]"),
        )
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
