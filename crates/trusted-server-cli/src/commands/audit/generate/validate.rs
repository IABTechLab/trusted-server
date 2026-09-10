//! Write-side validation for generated ad-template config.
//!
//! Everything the generator writes is derived from a live, page-controlled ad
//! stack, so the candidate document must pass source-config validation
//! before it replaces the operator's file. A config the
//! runtime rejects is not a degraded ad stack — `build_state` fails and the
//! adapter answers every route from the startup error router, so an unloadable
//! `trusted-server.toml` is a full-site outage once pushed.

use trusted_server_core::config::TrustedServerAppConfig;

use crate::error::{CliResult, cli_error};

/// Validates the candidate config text the generator is about to persist.
///
/// Runs [`TrustedServerAppConfig::new`], the push-time validation path, after
/// deserializing the source TOML. This checks slot and template compilation,
/// provider configuration, and secret key references without I/O. Secret values
/// are resolved and validated separately at runtime; treating source key names
/// as resolved secrets would incorrectly reject otherwise valid baselines.
///
/// `baseline` is the config as it was read from disk. When the baseline is
/// *already* unloadable, this run cannot be blamed for it: the candidate is
/// accepted and the pre-existing error is returned as a warning instead. Without
/// that escape hatch a freshly bootstrapped config carrying placeholder secrets
/// could never be updated by `generate`.
///
/// # Errors
///
/// Returns a user-facing error when the candidate fails to load and the baseline
/// loaded cleanly — that is, when this run introduced the failure.
pub(super) fn check_candidate(candidate: &str, baseline: &str) -> CliResult<Vec<String>> {
    let Err(candidate_error) = validate_source_config(candidate) else {
        return Ok(Vec::new());
    };

    if let Err(baseline_error) = validate_source_config(baseline) {
        return Ok(vec![format!(
            "target config was already invalid before this run, so the generated \
             result could not be verified: {baseline_error}"
        )]);
    }

    cli_error(format!(
        "refusing to write: the generated config would fail to load, which would \
         take the service down once pushed: {candidate_error}"
    ))
}

fn validate_source_config(source: &str) -> CliResult<()> {
    let config: TrustedServerAppConfig =
        toml::from_str(source).map_err(|error| error.to_string())?;
    TrustedServerAppConfig::new(config.into_settings())
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A source config containing secret references, as an operator would edit it.
    fn baseline() -> String {
        crate::commands::config::init::EXAMPLE_CONFIG
            .replace("\"example.com\"", "\"publisher.example.com\"")
            .replace("\".example.com\"", "\".publisher.example.com\"")
            .replace(
                "password = \"handler_password\"",
                "password = \"test-admin-password-32-bytes-minimum\"",
            )
            .replace(
                "passphrase = \"ec_passphrase\"",
                "passphrase = \"test-ec-passphrase-32-bytes-minimum\"",
            )
            .replace(
                "proxy_secret = \"publisher_proxy_secret\"",
                "proxy_secret = \"test-proxy-secret-32-bytes-minimum\"",
            )
            .replace(
                "https://origin.example.com",
                "https://origin.publisher.example.com",
            )
    }

    #[test]
    fn valid_candidate_passes_without_warnings() {
        let config = baseline();

        let warnings = check_candidate(&config, &config).expect("should accept valid candidate");

        assert!(
            warnings.is_empty(),
            "a clean candidate should not warn, got {warnings:?}"
        );
    }

    #[test]
    fn candidate_this_run_broke_is_refused() {
        let good = baseline();
        // An empty div_id override is exactly what a div id normalized down to
        // nothing would produce, and `validate_runtime` rejects it.
        let broken = format!(
            "{good}\n[[creative_opportunities.slot]]\n\
             id = \"broken\"\ndiv_id = \"\"\n\
             page_patterns = [\"/\"]\n\
             formats = [{{ width = 300, height = 250 }}]\n"
        );

        let error = check_candidate(&broken, &good).expect_err("should refuse a broken candidate");

        assert!(
            format!("{error:?}").contains("refusing to write"),
            "error should name the refusal, got {error:?}"
        );
    }

    #[test]
    fn pre_existing_breakage_downgrades_to_a_warning() {
        // The operator's file was already unloadable; `generate` must still be
        // able to update it rather than blaming this run for the old error.
        let broken_baseline = "[creative_opportunities]\n";
        let broken_candidate = "[creative_opportunities]\n";

        let warnings = check_candidate(broken_candidate, broken_baseline)
            .expect("a pre-existing failure should not block the write");

        assert_eq!(warnings.len(), 1, "should surface exactly one warning");
        assert!(
            warnings[0].contains("already invalid"),
            "warning should name the pre-existing failure, got {:?}",
            warnings[0]
        );
    }
}
