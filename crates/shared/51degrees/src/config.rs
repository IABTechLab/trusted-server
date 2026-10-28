//! The settings every 51Degrees module shares, from `[shared.51degrees]`.
//!
//! The table's presence is what runs the 51Degrees modules, so a configuration
//! without it runs none of them, and each module's own settings stay in the
//! table that selects it. The cache the modules' answers are held in is
//! selected and sized in `[cache.51degrees-answers]`, as every declared cache
//! is, see [`crate::engine_cache`].

use error_stack::Report;
use serde::Deserialize;
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::redacted::Redacted;
use trusted_server_core::settings::{SHARED_TYPE, Settings};
use validator::Validate;

/// This vendor's table under `[shared]`, `51degrees`, taken from this crate's
/// folder.
#[must_use]
pub fn vendor() -> &'static str {
    trusted_server_core::module_name::short_form(SHARED_TYPE, trusted_server_core::module_name!())
}

/// `[shared.51degrees]`, being the cloud endpoint, the license key and the
/// timeout the 51Degrees modules share.
#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct SharedConfig {
    /// The full URL of the JSON endpoint, including any resource key the
    /// deployment requires in the path, for example
    /// `https://cloud.example.com/api/v4/<resource key>.json`, or
    /// `http://127.0.0.1:8080/api/v4/json` for a self-hosted container, which
    /// carries no key.
    ///
    /// Server side calls take the last path segment off this endpoint, which is
    /// the segment a multi-tenant service carries the resource key in, and
    /// present [`license_key`](Self::license_key) instead. The origin of the
    /// endpoint is where the client hints middleware delegates the browser's
    /// hints to.
    ///
    /// Name the domains a resource key may be used from when creating it in the
    /// 51Degrees configurator. A key with no domain list works from anywhere, so
    /// anyone who reads it can spend the allowance against the account.
    #[validate(url)]
    pub endpoint: String,

    /// How long to wait for the service before giving up, in milliseconds.
    /// The call sits in front of the page on a first view, so the default is
    /// short.
    #[serde(default = "default_timeout_ms")]
    #[validate(range(min = 1, max = 10_000))]
    pub timeout_ms: u32,

    /// The license key every server side call authenticates with.
    ///
    /// A server side call presents the license key and never the resource key. A
    /// resource key is public, travelling to the browser inside a script URL, and
    /// a key restricted to named domains answers `401` to a server, which sends
    /// no `Referer` or `Origin`. A resource key also answers with everything it
    /// carries whatever the call names.
    ///
    /// A secret, named in the settings by its entry in the secret store. Required
    /// whenever the table is present, because every 51Degrees module presents
    /// it, see [`license_key`](crate::license::license_key).
    #[serde(default)]
    pub license_key: Option<Redacted<String>>,
}

const fn default_timeout_ms() -> u32 {
    500
}

/// Reads `[shared.51degrees]`, or `None` when the configuration has none.
///
/// # Errors
///
/// When the table carries a setting it does not know, or one that fails
/// validation.
pub fn read(settings: &Settings) -> Result<Option<SharedConfig>, Report<TrustedServerError>> {
    settings.shared_config::<SharedConfig>(vendor())
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusted_server_core::html_processor::test_support::page_settings;

    #[test]
    fn the_table_is_named_by_this_crates_folder() {
        assert_eq!(vendor(), "51degrees");
    }

    #[test]
    fn a_configuration_without_the_table_reads_as_none() {
        assert!(
            read(&page_settings(""))
                .expect("should read settings with no table")
                .is_none()
        );
    }

    #[test]
    fn the_table_is_read_with_its_defaults() {
        let settings = page_settings(
            r#"
                [shared.51degrees]
                endpoint = "https://cloud.example.com/api/v4/json"
                license_key = "a-license-key"
            "#,
        );

        let shared = read(&settings)
            .expect("should read the table")
            .expect("the table is present");

        assert_eq!(shared.endpoint, "https://cloud.example.com/api/v4/json");
        assert_eq!(shared.timeout_ms, 500);
        assert_eq!(
            shared.license_key.as_ref().map(|key| key.expose().as_str()),
            Some("a-license-key")
        );
    }

    /// A misspelt key fails the deploy check naming the table and the key,
    /// where it would otherwise be ignored.
    #[test]
    fn a_key_the_table_does_not_know_is_refused() {
        let settings = page_settings(
            r#"
                [shared.51degrees]
                endpoint = "https://cloud.example.com/api/v4/json"
                license_key = "a-license-key"
                no_such_setting = true
            "#,
        );

        let message = format!(
            "{:?}",
            read(&settings).expect_err("the table carries a key it does not know")
        );

        assert!(
            message.contains("[shared.51degrees]") && message.contains("no_such_setting"),
            "the message has to name the table and the key: {message}"
        );
    }

    #[test]
    fn a_timeout_out_of_range_is_refused() {
        let settings = page_settings(
            r#"
                [shared.51degrees]
                endpoint = "https://cloud.example.com/api/v4/json"
                license_key = "a-license-key"
                timeout_ms = 0
            "#,
        );

        let message = format!(
            "{:?}",
            read(&settings).expect_err("a zero timeout waits for nothing")
        );

        assert!(message.contains("[shared.51degrees]"), "{message}");
    }

    #[test]
    fn an_endpoint_that_is_not_a_url_is_refused() {
        let settings = page_settings(
            r#"
                [shared.51degrees]
                endpoint = "not a url"
                license_key = "a-license-key"
            "#,
        );

        assert!(read(&settings).is_err());
    }
}
