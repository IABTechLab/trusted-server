//! Pull sync partner helpers.
//!
//! Pull partners run as [`GenericPullProvider`](super::identity::generic_pull)
//! identity providers, dispatched by the after-send identity flush. This
//! module keeps the partner URL checks they use and the completeness test
//! behind the pull-complete marker.
//!
//! Pull partners fill missing partner UIDs only, unless an operator makes one
//! a source owner.

use url::Url;

use super::kv_types::KvEntry;
use super::registry::{PartnerConfig, PartnerRegistry};

/// Returns whether a consented live entry holds an ID for every pull-enabled
/// partner. Always returns `false` when there are no pull-enabled partners.
#[must_use]
pub(crate) fn entry_is_pull_complete(entry: &KvEntry, registry: &PartnerRegistry) -> bool {
    let pull_partners = registry.pull_enabled_partners();
    !pull_partners.is_empty()
        && entry.consent.ok
        && pull_partners
            .iter()
            .all(|partner| !is_partner_pull_eligible(partner, Some(entry)))
}

fn is_partner_pull_eligible(partner: &PartnerConfig, kv_entry: Option<&KvEntry>) -> bool {
    kv_entry
        .and_then(|entry| entry.ids.get(&partner.source_domain))
        .is_none()
}

pub(crate) fn validated_pull_sync_url(partner: &PartnerConfig) -> Option<Url> {
    let pull_sync_url = partner.pull_sync_url.as_deref()?;
    let parsed = match Url::parse(pull_sync_url) {
        Ok(url) => url,
        Err(err) => {
            log::error!(
                "Pull sync: partner '{}' has invalid pull_sync_url '{}': {err}",
                partner.source_domain,
                pull_sync_url
            );
            return None;
        }
    };

    if parsed.scheme() != "https" {
        log::error!(
            "Pull sync: partner '{}' pull_sync_url must use HTTPS, got scheme '{}'",
            partner.source_domain,
            parsed.scheme()
        );
        return None;
    }

    let Some(hostname) = parsed.host_str() else {
        log::error!(
            "Pull sync: partner '{}' pull_sync_url has no hostname: {}",
            partner.source_domain,
            pull_sync_url
        );
        return None;
    };

    let hostname = hostname.trim_end_matches('.').to_ascii_lowercase();
    if !partner.pull_sync_allowed_domains.iter().any(|domain| {
        domain
            .trim()
            .trim_end_matches('.')
            .eq_ignore_ascii_case(&hostname)
    }) {
        log::error!(
            "Pull sync: partner '{}' URL host '{}' not in pull_sync_allowed_domains",
            partner.source_domain,
            hostname
        );
        return None;
    }

    Some(parsed)
}

pub(crate) fn build_pull_request_url(mut base_url: Url, ec_id: &str) -> Url {
    base_url.query_pairs_mut().append_pair("ec_id", ec_id);
    base_url
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ec::kv_types::KvEntry;
    use crate::redacted::Redacted;

    fn pull_partner(ttl_sec: u64) -> PartnerConfig {
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
            pull_sync_ttl_sec: ttl_sec,
            pull_sync_rate_limit: 20,
            ts_pull_token: Some(Redacted::new("token".to_owned())),
        }
    }

    #[test]
    fn partner_is_eligible_when_missing_from_entry() {
        let partner = pull_partner(3600);
        let entry = KvEntry::minimal("other_partner", "uid-1", 100);

        assert!(
            is_partner_pull_eligible(&partner, Some(&entry)),
            "should dispatch when partner has no stored UID"
        );
    }

    #[test]
    fn partner_is_not_eligible_when_already_present() {
        let partner = pull_partner(3600);
        let entry = KvEntry::minimal("ssp.example.com", "uid-1", 1000);

        assert!(
            !is_partner_pull_eligible(&partner, Some(&entry)),
            "should skip dispatch when partner already has a stored UID"
        );
    }

    fn pull_enabled_ec_partner(source_domain: &str) -> crate::settings::EcPartner {
        crate::settings::EcPartner {
            name: format!("Partner {source_domain}"),
            source_domain: source_domain.to_owned(),
            openrtb_atype: crate::settings::EcPartner::default_openrtb_atype(),
            bidstream_enabled: true,
            api_token: None,
            batch_rate_limit: crate::settings::EcPartner::default_batch_rate_limit(),
            pull_sync_enabled: true,
            pull_sync_url: Some(format!("https://sync.{source_domain}/pull")),
            pull_sync_allowed_domains: vec![format!("sync.{source_domain}")],
            pull_sync_ttl_sec: crate::settings::EcPartner::default_pull_sync_ttl_sec(),
            pull_sync_rate_limit: crate::settings::EcPartner::default_pull_sync_rate_limit(),
            ts_pull_token: Some(Redacted::new(
                "example-token-value-0123456789abcdef".to_owned(),
            )),
        }
    }

    #[test]
    fn completeness_requires_all_pull_partner_ids() {
        let registry = PartnerRegistry::from_config(&[
            pull_enabled_ec_partner("a.example.com"),
            pull_enabled_ec_partner("b.example.com"),
        ])
        .expect("should build registry");
        let mut entry = KvEntry::minimal("a.example.com", "uid-a", 1_000);

        assert!(
            !entry_is_pull_complete(&entry, &registry),
            "entry missing a pull partner ID should be incomplete"
        );

        entry.ids.insert(
            "b.example.com".to_owned(),
            crate::ec::kv_types::KvPartnerId::new("uid-b".to_owned()),
        );

        assert!(
            entry_is_pull_complete(&entry, &registry),
            "entry with every pull partner ID should be complete"
        );
    }

    #[test]
    fn validated_pull_sync_url_rejects_http_scheme() {
        let mut partner = pull_partner(3600);
        partner.pull_sync_url = Some("http://sync.partner.test/pull".to_owned());

        let validated = validated_pull_sync_url(&partner);
        assert!(
            validated.is_none(),
            "should reject pull_sync_url with HTTP scheme"
        );
    }

    #[test]
    fn validated_pull_sync_url_rejects_non_allowlisted_host() {
        let mut partner = pull_partner(3600);
        partner.pull_sync_url = Some("https://evil.test/pull".to_owned());

        let validated = validated_pull_sync_url(&partner);
        assert!(
            validated.is_none(),
            "should reject runtime pull_sync_url host outside allowlist"
        );
    }

    #[test]
    fn validated_pull_sync_url_accepts_normalized_allowlist_match() {
        let mut partner = pull_partner(3600);
        partner.pull_sync_url = Some("https://SYNC.PARTNER.TEST./pull".to_owned());
        partner.pull_sync_allowed_domains = vec!["sync.partner.test".to_owned()];

        let validated = validated_pull_sync_url(&partner);
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

    // -----------------------------------------------------------------------
    // Snapshot-driven eligibility and request-wide aggregation
    // -----------------------------------------------------------------------
}
