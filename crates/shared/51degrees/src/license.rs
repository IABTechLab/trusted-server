//! The license key every 51Degrees module presents.
//!
//! Every module takes its key from [`license_key`], through the builder that
//! registers them, so a missing key has one answer, which is that the
//! configuration is refused.

use error_stack::Report;
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::redacted::Redacted;
use trusted_server_core::settings::Settings;

use crate::config::{self, vendor};

/// The license key every 51Degrees server side call presents, from
/// `[shared.51degrees]`.
///
/// Fatal when absent, because a server side call presents the license key and
/// never the public resource key, and without one the deployment would serve
/// every page while answering nothing. A configuration without
/// `[shared.51degrees]` runs no 51Degrees module, so it never calls this.
///
/// A configuration error is served from every route on a host that cannot
/// start without its settings, so the key has to be in the secret store before
/// a deploy that carries the table.
///
/// # Errors
///
/// A configuration error when there is no `[shared.51degrees]`, when it
/// cannot be read, or when it has no `license_key` or an empty one.
pub fn license_key(settings: &Settings) -> Result<Redacted<String>, Report<TrustedServerError>> {
    let vendor = vendor();
    let Some(shared) = config::read(settings)? else {
        return Err(Report::new(TrustedServerError::Configuration {
            message: format!(
                "There is no [shared.{vendor}], so there is no 51Degrees license key for a \
                 51Degrees module to present. Add [shared.{vendor}] with its `endpoint` and \
                 `license_key`."
            ),
        }));
    };
    shared
        .license_key
        .filter(|key| !key.expose().trim().is_empty())
        .ok_or_else(|| {
            Report::new(TrustedServerError::Configuration {
                message: format!(
                    "[shared.{vendor}] has no `license_key`. Every 51Degrees module presents a \
                     license key on its server side calls, and never the public resource key in \
                     `endpoint`, so the table needs one. Set `license_key`, or remove \
                     [shared.{vendor}] and every 51Degrees module the configuration selects."
                ),
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusted_server_core::html_processor::test_support::page_settings;

    #[test]
    fn the_configured_key_is_the_one_every_module_presents() {
        let settings = page_settings(
            r#"
                [shared.51degrees]
                endpoint = "https://cloud.example.com/api/v4/json"
                license_key = "a-license-key"
            "#,
        );

        let key = license_key(&settings).expect("a configured key should be read");

        assert_eq!(key.expose(), "a-license-key");
    }

    #[test]
    fn a_shared_table_with_no_key_stops_the_deployment_naming_the_setting() {
        for table in [
            r#"endpoint = "https://cloud.example.com/api/v4/json""#,
            r#"endpoint = "https://cloud.example.com/api/v4/json"
               license_key = "  ""#,
        ] {
            let settings = page_settings(&format!(
                r#"
                    [shared.51degrees]
                    {table}
                "#
            ));

            let message = format!(
                "{:?}",
                license_key(&settings).expect_err("there is no key to present")
            );

            assert!(
                message.contains("[shared.51degrees] has no `license_key`"),
                "the message has to name the setting to fix: {message}"
            );
        }
    }

    #[test]
    fn a_configuration_without_the_shared_table_has_no_key_to_give() {
        let settings = page_settings("");

        let message = format!(
            "{:?}",
            license_key(&settings).expect_err("51Degrees is not configured")
        );

        assert!(
            message.contains("There is no [shared.51degrees]"),
            "the message has to say why there is no key rather than blame the setting: \
             {message}"
        );
    }
}
