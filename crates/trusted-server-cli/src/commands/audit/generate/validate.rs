//! Write-side validation for generated ad-template config.
//!
//! Everything the generator writes is derived from a live, page-controlled ad
//! stack, so the candidate document has to clear the same bar the runtime
//! applies at startup *before* it replaces the operator's file. A config the
//! runtime rejects is not a degraded ad stack — `build_state` fails and the
//! adapter answers every route from the startup error router, so an unloadable
//! `trusted-server.toml` is a full-site outage once pushed.

use trusted_server_core::settings::Settings;

use crate::error::{CliResult, cli_error};

/// Validates the candidate config text the generator is about to persist.
///
/// Runs [`Settings::from_toml`], which drives the identical
/// `finalize_deserialized` chain the runtime uses — serde (`deny_unknown_fields`
/// plus required fields), then `compile_slots` → `compile_unit_templates` →
/// `validate_runtime`, then the validator pass — with no I/O.
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
    let Err(candidate_error) = Settings::from_toml(candidate) else {
        return Ok(Vec::new());
    };

    if let Err(baseline_error) = Settings::from_toml(baseline) {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal config that loads cleanly, used as the valid baseline.
    fn baseline() -> String {
        crate::commands::config::init::EXAMPLE_CONFIG
            .replace("handler_password", "test-admin-password-32-bytes-minimum")
            .replace("ec_passphrase", "test-ec-passphrase-32-bytes-minimum")
            .replace(
                "publisher_proxy_secret",
                "test-proxy-secret-32-bytes-minimum",
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
