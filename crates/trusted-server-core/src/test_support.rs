#[cfg(test)]
pub mod tests {
    use crate::ec::provider::{EcProviderSelection, HMAC_PROVIDER_KEY, HOST_SIGNALS_PROVIDER_KEY};
    use crate::redacted::Redacted;
    use crate::settings::{
        Ec, EcProviderBlock, HmacProviderConfig, HostSignalsProviderConfig, Settings,
    };

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

            [integrations.prebid]
            enabled = true
            external_bundle_url = "https://assets.example/prebid/trusted-prebid.js"

            [integrations.prebid.bundle]
            adapters = ["exampleBidder"]

            [integrations.nextjs]
            enabled = false
            rewrite_attributes = ["href", "link", "url"]

            [ec]
            provider = "hmac"

            [ec.hmac]
            passphrase = "test-secret-key-32-bytes-minimum"

            [request_signing]
            config_store_id = "test-config-store-id"
            secret_store_id = "test-secret-store-id"
            "#
        .to_owned()
    }

    /// The crate test configuration with its whole `[ec]` section replaced by
    /// `ec_section`, which carries its own `[ec]` header and any provider
    /// blocks.
    ///
    /// # Panics
    ///
    /// Panics if the embedded TOML configuration no longer has an `[ec]`
    /// section followed by a `[request_signing]` section.
    #[must_use]
    pub fn crate_test_settings_str_with_ec_section(ec_section: &str) -> String {
        let base = crate_test_settings_str();
        let (before, rest) = base
            .split_once("[ec]")
            .expect("should find the [ec] section in the test settings");
        let (_, after) = rest
            .split_once("[request_signing]")
            .expect("should find the [request_signing] section in the test settings");
        format!("{before}{ec_section}\n\n[request_signing]{after}")
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

    /// Selects the built-in HMAC provider under `name` with `passphrase`,
    /// replacing whatever Edge Cookie provider the settings carried.
    ///
    /// A `name` other than `hmac` is a label, so the block names the
    /// implementation it configures.
    pub fn select_hmac_provider(ec: &mut Ec, name: &str, passphrase: &str) {
        let mut block = EcProviderBlock::from(HmacProviderConfig {
            passphrase: Redacted::new(passphrase.to_owned()),
        });
        if name != HMAC_PROVIDER_KEY {
            block.implementation = Some(HMAC_PROVIDER_KEY.to_owned());
        }
        ec.provider = Some(EcProviderSelection::from(name));
        ec.provider_blocks.clear();
        ec.provider_blocks.insert(name.to_owned(), block);
    }

    /// Selects the built-in host-signal provider under its own name with
    /// `passphrase`, replacing whatever Edge Cookie provider the settings
    /// carried.
    pub fn select_host_signals_provider(ec: &mut Ec, passphrase: &str) {
        ec.provider = Some(EcProviderSelection::from(HOST_SIGNALS_PROVIDER_KEY));
        ec.provider_blocks.clear();
        ec.provider_blocks.insert(
            HOST_SIGNALS_PROVIDER_KEY.to_owned(),
            EcProviderBlock::from(HostSignalsProviderConfig {
                passphrase: Redacted::new(passphrase.to_owned()),
            }),
        );
    }

    /// The passphrase the block `name` holds.
    ///
    /// # Panics
    ///
    /// Panics if `name` has no block, or if its block configures another
    /// provider.
    #[must_use]
    pub fn hmac_passphrase<'a>(ec: &'a Ec, name: &str) -> &'a str {
        ec.provider_blocks
            .get(name)
            .and_then(EcProviderBlock::hmac_settings)
            .unwrap_or_else(|| panic!("settings should configure the hmac provider under `{name}`"))
            .passphrase
            .expose()
    }

    /// A valid EC ID in `{64-hex}.{6-alnum}` format for use in tests.
    pub const VALID_SYNTHETIC_ID: &str =
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.Ab1234";
}
