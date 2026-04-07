#[cfg(test)]
pub mod tests {
    use crate::settings::Settings;

    /// A well-formed synthetic ID for use in tests: 64 lowercase hex chars + `'.'` + 6 alphanumeric.
    pub const VALID_SYNTHETIC_ID: &str =
        "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d8e9f0a1b2.Ab12z9";

    #[must_use]
    pub fn crate_test_settings_str() -> String {
        r#"
            [[handlers]]
            path = "^/secure"
            username = "user"
            password = "pass"

            [[handlers]]
            path = "^/admin"
            username = "admin"
            password = "admin-pass"

            [publisher]
            domain = "test-publisher.com"
            cookie_domain = ".test-publisher.com"
            origin_backend = "publisher_origin"
            origin_url = "https://origin.test-publisher.com"
            proxy_secret = "unit-test-proxy-secret"

            [integrations.prebid]
            enabled = true
            server_url = "https://test-prebid.com/openrtb2/auction"  

            [integrations.nextjs]
            enabled = false
            rewrite_attributes = ["href", "link", "url"]

            [edge_cookie]
            secret_key = "test-secret-key"
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
}
