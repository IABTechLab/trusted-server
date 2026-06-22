// Strips lines whose first non-whitespace token is `//`.
//
// Known limitations (both produce false positives, never false negatives):
// - String literals: `"fastly::Request"` in a test assertion would trigger a
//   spurious failure even though it is not a real Fastly dependency.
// - Block comments: `/* fastly::Request */` is not stripped. A banned pattern
//   inside a block comment causes a spurious failure; one hidden *outside* a
//   block comment is still caught by the non-comment portions of the line.
//
// False positives are safe for a guard test — they cause a noisy failure that
// forces investigation rather than letting a real regression slip through
// silently. False negatives are not possible with the current banned-pattern
// list because none of the migrated files use block comments in practice.
fn strip_line_comments(source: &str) -> String {
    source
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            !trimmed.starts_with("//")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn fastly_sdk_pattern() -> regex::Regex {
    regex::Regex::new(r"\b(?:use\s+fastly\b|fastly::)")
        .expect("should compile migration guard regex")
}

fn checked_sources() -> &'static [(&'static str, &'static str)] {
    &[
        (
            "asset_image_optimizer.rs",
            include_str!("asset_image_optimizer.rs"),
        ),
        ("auction/config.rs", include_str!("auction/config.rs")),
        ("auction/context.rs", include_str!("auction/context.rs")),
        ("auction/endpoints.rs", include_str!("auction/endpoints.rs")),
        ("auction/formats.rs", include_str!("auction/formats.rs")),
        ("auction/mod.rs", include_str!("auction/mod.rs")),
        (
            "auction/orchestrator.rs",
            include_str!("auction/orchestrator.rs"),
        ),
        ("auction/provider.rs", include_str!("auction/provider.rs")),
        (
            "auction/test_support.rs",
            include_str!("auction/test_support.rs"),
        ),
        ("auction/types.rs", include_str!("auction/types.rs")),
        (
            "auction_config_types.rs",
            include_str!("auction_config_types.rs"),
        ),
        ("auth.rs", include_str!("auth.rs")),
        (
            "consent/extraction.rs",
            include_str!("consent/extraction.rs"),
        ),
        ("consent/gpp.rs", include_str!("consent/gpp.rs")),
        (
            "consent/jurisdiction.rs",
            include_str!("consent/jurisdiction.rs"),
        ),
        ("consent/mod.rs", include_str!("consent/mod.rs")),
        ("consent/tcf.rs", include_str!("consent/tcf.rs")),
        ("consent/types.rs", include_str!("consent/types.rs")),
        (
            "consent/us_privacy.rs",
            include_str!("consent/us_privacy.rs"),
        ),
        ("consent_config.rs", include_str!("consent_config.rs")),
        ("constants.rs", include_str!("constants.rs")),
        ("cookies.rs", include_str!("cookies.rs")),
        ("creative.rs", include_str!("creative.rs")),
        ("ec/auth.rs", include_str!("ec/auth.rs")),
        ("ec/batch_sync.rs", include_str!("ec/batch_sync.rs")),
        ("ec/consent.rs", include_str!("ec/consent.rs")),
        ("ec/cookies.rs", include_str!("ec/cookies.rs")),
        ("ec/device.rs", include_str!("ec/device.rs")),
        ("ec/eids.rs", include_str!("ec/eids.rs")),
        ("ec/finalize.rs", include_str!("ec/finalize.rs")),
        ("ec/generation.rs", include_str!("ec/generation.rs")),
        ("ec/identify.rs", include_str!("ec/identify.rs")),
        ("ec/kv_types.rs", include_str!("ec/kv_types.rs")),
        ("ec/mod.rs", include_str!("ec/mod.rs")),
        ("ec/partner.rs", include_str!("ec/partner.rs")),
        ("ec/prebid_eids.rs", include_str!("ec/prebid_eids.rs")),
        ("ec/pull_sync.rs", include_str!("ec/pull_sync.rs")),
        ("ec/registry.rs", include_str!("ec/registry.rs")),
        ("edge_cookie.rs", include_str!("edge_cookie.rs")),
        ("error.rs", include_str!("error.rs")),
        ("geo.rs", include_str!("geo.rs")),
        ("host_header.rs", include_str!("host_header.rs")),
        ("host_rewrite.rs", include_str!("host_rewrite.rs")),
        ("html_processor.rs", include_str!("html_processor.rs")),
        ("http_util.rs", include_str!("http_util.rs")),
        (
            "integrations/adserver_mock.rs",
            include_str!("integrations/adserver_mock.rs"),
        ),
        ("integrations/aps.rs", include_str!("integrations/aps.rs")),
        (
            "integrations/datadome.rs",
            include_str!("integrations/datadome.rs"),
        ),
        (
            "integrations/didomi.rs",
            include_str!("integrations/didomi.rs"),
        ),
        (
            "integrations/google_tag_manager.rs",
            include_str!("integrations/google_tag_manager.rs"),
        ),
        ("integrations/gpt.rs", include_str!("integrations/gpt.rs")),
        (
            "integrations/lockr.rs",
            include_str!("integrations/lockr.rs"),
        ),
        ("integrations/mod.rs", include_str!("integrations/mod.rs")),
        (
            "integrations/nextjs/html_post_process.rs",
            include_str!("integrations/nextjs/html_post_process.rs"),
        ),
        (
            "integrations/nextjs/mod.rs",
            include_str!("integrations/nextjs/mod.rs"),
        ),
        (
            "integrations/nextjs/rsc.rs",
            include_str!("integrations/nextjs/rsc.rs"),
        ),
        (
            "integrations/nextjs/rsc_placeholders.rs",
            include_str!("integrations/nextjs/rsc_placeholders.rs"),
        ),
        (
            "integrations/nextjs/script_rewriter.rs",
            include_str!("integrations/nextjs/script_rewriter.rs"),
        ),
        (
            "integrations/nextjs/shared.rs",
            include_str!("integrations/nextjs/shared.rs"),
        ),
        (
            "integrations/permutive.rs",
            include_str!("integrations/permutive.rs"),
        ),
        (
            "integrations/prebid.rs",
            include_str!("integrations/prebid.rs"),
        ),
        (
            "integrations/registry.rs",
            include_str!("integrations/registry.rs"),
        ),
        (
            "integrations/sourcepoint.rs",
            include_str!("integrations/sourcepoint.rs"),
        ),
        (
            "integrations/testlight.rs",
            include_str!("integrations/testlight.rs"),
        ),
        ("lib.rs", include_str!("lib.rs")),
        ("models.rs", include_str!("models.rs")),
        ("openrtb.rs", include_str!("openrtb.rs")),
        ("platform/error.rs", include_str!("platform/error.rs")),
        ("platform/http.rs", include_str!("platform/http.rs")),
        (
            "platform/image_optimizer.rs",
            include_str!("platform/image_optimizer.rs"),
        ),
        ("platform/kv.rs", include_str!("platform/kv.rs")),
        ("platform/mod.rs", include_str!("platform/mod.rs")),
        (
            "platform/test_support.rs",
            include_str!("platform/test_support.rs"),
        ),
        ("platform/traits.rs", include_str!("platform/traits.rs")),
        ("platform/types.rs", include_str!("platform/types.rs")),
        ("proxy.rs", include_str!("proxy.rs")),
        ("publisher.rs", include_str!("publisher.rs")),
        ("redacted.rs", include_str!("redacted.rs")),
        (
            "request_signing/discovery.rs",
            include_str!("request_signing/discovery.rs"),
        ),
        (
            "request_signing/endpoints.rs",
            include_str!("request_signing/endpoints.rs"),
        ),
        (
            "request_signing/jwks.rs",
            include_str!("request_signing/jwks.rs"),
        ),
        (
            "request_signing/mod.rs",
            include_str!("request_signing/mod.rs"),
        ),
        (
            "request_signing/rotation.rs",
            include_str!("request_signing/rotation.rs"),
        ),
        (
            "request_signing/signing.rs",
            include_str!("request_signing/signing.rs"),
        ),
        ("rsc_flight.rs", include_str!("rsc_flight.rs")),
        ("s3_sigv4.rs", include_str!("s3_sigv4.rs")),
        ("settings.rs", include_str!("settings.rs")),
        ("settings_data.rs", include_str!("settings_data.rs")),
        ("storage/kv_store.rs", include_str!("storage/kv_store.rs")),
        ("storage/mod.rs", include_str!("storage/mod.rs")),
        (
            "streaming_processor.rs",
            include_str!("streaming_processor.rs"),
        ),
        (
            "streaming_replacer.rs",
            include_str!("streaming_replacer.rs"),
        ),
        ("test_support.rs", include_str!("test_support.rs")),
        ("tsjs.rs", include_str!("tsjs.rs")),
    ]
}

fn allowlisted_sources() -> &'static [(&'static str, &'static str)] {
    &[
        ("ec/kv.rs", include_str!("ec/kv.rs")),
        ("ec/rate_limiter.rs", include_str!("ec/rate_limiter.rs")),
    ]
}

#[test]
fn fastly_sdk_references_remain_only_in_deferred_ec_modules() {
    let banned = fastly_sdk_pattern();

    for &(path, source) in checked_sources() {
        let uncommented = strip_line_comments(source);
        assert!(
            !banned.is_match(&uncommented),
            "{path} should not reference the Fastly SDK outside the explicit EC KV/ERL allowlist"
        );
    }
}

#[test]
fn deferred_ec_fastly_allowlist_still_tracks_actual_residual_dependencies() {
    let banned = fastly_sdk_pattern();

    for &(path, source) in allowlisted_sources() {
        let uncommented = strip_line_comments(source);
        assert!(
            banned.is_match(&uncommented),
            "{path} should remain an intentional temporary Fastly SDK dependency"
        );
    }
}
