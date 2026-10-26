//! Pull sync URL helpers.
//!
//! Pull partners are called as generic pull identity providers
//! (`identity::generic_pull`), dispatched by the writer's phase 4
//! flush. These helpers validate and build the partner URL they call.

use url::Url;

/// Returns the pull URL when it is HTTPS and its host is allowlisted.
///
/// The host comparison ignores case and a trailing dot on both sides.
/// Returns `None`, with a log line that names no partner, for a missing,
/// invalid or non-HTTPS URL and for a host outside `allowed_domains`.
pub(crate) fn validated_pull_sync_url(
    pull_sync_url: Option<&str>,
    allowed_domains: &[String],
) -> Option<Url> {
    let pull_sync_url = pull_sync_url?;
    let parsed = match Url::parse(pull_sync_url) {
        Ok(url) => url,
        Err(err) => {
            log::error!("Pull sync: invalid pull_sync_url '{pull_sync_url}': {err}");
            return None;
        }
    };

    if parsed.scheme() != "https" {
        log::error!(
            "Pull sync: pull_sync_url must use HTTPS, got scheme '{}'",
            parsed.scheme()
        );
        return None;
    }

    let Some(hostname) = parsed.host_str() else {
        log::error!("Pull sync: pull_sync_url has no hostname: {pull_sync_url}");
        return None;
    };

    let hostname = hostname.trim_end_matches('.').to_ascii_lowercase();
    if !allowed_domains.iter().any(|domain| {
        domain
            .trim()
            .trim_end_matches('.')
            .eq_ignore_ascii_case(&hostname)
    }) {
        log::error!("Pull sync: URL host '{hostname}' not in pull_sync_allowed_domains");
        return None;
    }

    Some(parsed)
}

/// Appends the `ec_id` query parameter to a validated pull URL.
pub(crate) fn build_pull_request_url(mut base_url: Url, ec_id: &str) -> Url {
    base_url.query_pairs_mut().append_pair("ec_id", ec_id);
    base_url
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ec::registry::PartnerConfig;
    use crate::redacted::Redacted;

    fn pull_partner() -> PartnerConfig {
        PartnerConfig {
            name: "SSP X".to_owned(),
            api_key_hash: Some("deadbeef".to_owned()),
            bidstream_enabled: true,
            source_domain: "ssp.example.com".to_owned(),
            openrtb_atype: 3,
            batch_rate_limit: 60,
            pull_sync_enabled: true,
            pull_sync_url: Some("https://sync.partner.test/pull".to_owned()),
            pull_sync_allowed_domains: vec!["sync.partner.test".to_owned()],
            ts_pull_token: Some(Redacted::new("token".to_owned())),
        }
    }

    #[test]
    fn validated_pull_sync_url_rejects_http_scheme() {
        let mut partner = pull_partner();
        partner.pull_sync_url = Some("http://sync.partner.test/pull".to_owned());

        let validated = validated_pull_sync_url(
            partner.pull_sync_url.as_deref(),
            &partner.pull_sync_allowed_domains,
        );
        assert!(
            validated.is_none(),
            "should reject pull_sync_url with HTTP scheme"
        );
    }

    #[test]
    fn validated_pull_sync_url_rejects_non_allowlisted_host() {
        let mut partner = pull_partner();
        partner.pull_sync_url = Some("https://evil.test/pull".to_owned());

        let validated = validated_pull_sync_url(
            partner.pull_sync_url.as_deref(),
            &partner.pull_sync_allowed_domains,
        );
        assert!(
            validated.is_none(),
            "should reject runtime pull_sync_url host outside allowlist"
        );
    }

    #[test]
    fn validated_pull_sync_url_accepts_normalized_allowlist_match() {
        let mut partner = pull_partner();
        partner.pull_sync_url = Some("https://SYNC.PARTNER.TEST./pull".to_owned());
        partner.pull_sync_allowed_domains = vec!["sync.partner.test".to_owned()];

        let validated = validated_pull_sync_url(
            partner.pull_sync_url.as_deref(),
            &partner.pull_sync_allowed_domains,
        );
        assert!(
            validated.is_some(),
            "should accept allowlist match after hostname normalization"
        );
    }

    #[test]
    fn build_pull_request_url_appends_ec_id() {
        let url = Url::parse("https://sync.partner.test/pull?x=1").expect("should parse URL");
        let result = build_pull_request_url(url, "ecid123");

        let query = result.query().expect("should have query string");
        assert!(query.contains("x=1"), "should preserve existing query");
        assert!(query.contains("ec_id=ecid123"), "should append ec_id");
        assert!(
            !query.contains("ip="),
            "should not forward client IP to partners"
        );
    }
}
