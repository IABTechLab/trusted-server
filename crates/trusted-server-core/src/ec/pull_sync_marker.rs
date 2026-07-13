//! Short-lived browser marker for complete pull-sync partner state.

use core::fmt;

use edgezero_core::body::Body as EdgeBody;
use hmac::{Hmac, Mac as _};
use http::{header, HeaderValue, Response};
use sha2::{Digest as _, Sha256};

use crate::constants::COOKIE_TS_EC_PULL_COMPLETE;
use crate::redacted::Redacted;
use crate::settings::Settings;

use super::kv_types::KvEntry;
use super::registry::PartnerRegistry;
use super::{current_timestamp, EcKvSnapshot};

type HmacSha256 = Hmac<Sha256>;

const MARKER_VERSION: &str = "v1";
const MARKER_KEY_LABEL: &[u8] = b"trusted-server/ec-pull-complete/key/v1";
const MARKER_MAX_AGE_SECS: u64 = 60 * 60;
const MAX_MARKER_LENGTH: usize = 256;

/// Request-local validation state for the pull-sync completeness marker.
#[derive(Clone, Default)]
pub(crate) enum PullSyncMarkerState {
    /// No marker cookie was present.
    #[default]
    Absent,
    /// A marker was present but has not been checked against the active EC and partner set.
    Unvalidated(Redacted<String>),
    /// A present marker failed validation or was disproved by authoritative KV state.
    Invalid,
    /// The marker is valid until the given Unix timestamp.
    Valid { expires_at: u64 },
}

impl fmt::Debug for PullSyncMarkerState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Absent => formatter.write_str("Absent"),
            Self::Unvalidated(_) => formatter.write_str("Unvalidated(<redacted>)"),
            Self::Invalid => formatter.write_str("Invalid"),
            Self::Valid { expires_at } => formatter
                .debug_struct("Valid")
                .field("expires_at", expires_at)
                .finish(),
        }
    }
}

impl PullSyncMarkerState {
    /// Creates marker state from an optional request cookie value.
    #[must_use]
    pub(crate) fn from_cookie(value: Option<String>) -> Self {
        value
            .map(Redacted::new)
            .map_or(Self::Absent, Self::Unvalidated)
    }

    /// Returns whether a marker cookie was present on the request.
    #[must_use]
    pub(crate) fn was_present(&self) -> bool {
        !matches!(self, Self::Absent)
    }

    /// Returns whether the marker is currently valid.
    #[must_use]
    pub(crate) fn is_valid(&self) -> bool {
        matches!(self, Self::Valid { .. })
    }

    /// Invalidates state bound to a replaced active EC ID.
    pub(crate) fn invalidate_for_replaced_ec(&mut self) {
        if self.was_present() {
            *self = Self::Invalid;
        }
    }
}

/// Validates an unvalidated marker against the active EC and current pull-partner set.
pub(crate) fn validate_marker_state(
    state: &mut PullSyncMarkerState,
    settings: &Settings,
    registry: &PartnerRegistry,
    ec_id: Option<&str>,
) {
    let PullSyncMarkerState::Unvalidated(value) = state else {
        return;
    };
    let valid_until = ec_id.and_then(|ec_id| {
        validate_marker(
            value.expose(),
            settings,
            registry,
            ec_id,
            current_timestamp(),
        )
    });
    *state = valid_until.map_or(PullSyncMarkerState::Invalid, |expires_at| {
        PullSyncMarkerState::Valid { expires_at }
    });
}

/// Reconciles browser marker state against finalized authoritative KV state.
pub(crate) fn reconcile_marker(
    settings: &Settings,
    registry: &PartnerRegistry,
    ec_id: Option<&str>,
    snapshot: &EcKvSnapshot,
    state: &mut PullSyncMarkerState,
    response: &mut Response<EdgeBody>,
) {
    let pull_partners = sorted_pull_partner_domains(registry);
    if pull_partners.is_empty() {
        expire_if_present(state, response);
        return;
    }

    let Some(ec_id) = ec_id else {
        expire_if_present(state, response);
        return;
    };

    if !matches!(snapshot, EcKvSnapshot::NotRead) && !snapshot.belongs_to(ec_id) {
        expire_if_present(state, response);
        return;
    }

    match snapshot {
        EcKvSnapshot::Present { .. } => {
            let entry = snapshot
                .entry_for(ec_id)
                .expect("snapshot binding should be checked before marker reconciliation");
            if !entry.consent.ok {
                expire_if_present(state, response);
            } else if entry_has_all_pull_partner_ids(entry, &pull_partners) {
                if !state.is_valid() {
                    set_marker(settings, registry, ec_id, state, response);
                }
            } else {
                expire_if_present(state, response);
            }
        }
        EcKvSnapshot::Missing { .. } => {
            expire_if_present(state, response);
        }
        EcKvSnapshot::Failed { .. } | EcKvSnapshot::NotRead => {
            if matches!(state, PullSyncMarkerState::Invalid) {
                expire_if_present(state, response);
            }
        }
    }
}

/// Expires the completeness marker regardless of KV state.
pub(crate) fn expire_marker(state: &mut PullSyncMarkerState, response: &mut Response<EdgeBody>) {
    append_cookie(response, &format_marker_cookie("", 0));
    *state = PullSyncMarkerState::Absent;
}

/// Returns whether a live entry contains every pull-enabled partner ID.
#[must_use]
pub(crate) fn entry_is_pull_complete(entry: &KvEntry, registry: &PartnerRegistry) -> bool {
    let pull_partners = sorted_pull_partner_domains(registry);
    !pull_partners.is_empty() && entry_has_all_pull_partner_ids(entry, &pull_partners)
}

fn entry_has_all_pull_partner_ids(entry: &KvEntry, pull_partners: &[String]) -> bool {
    entry.consent.ok
        && pull_partners
            .iter()
            .all(|source_domain| entry.ids.contains_key(source_domain))
}

fn set_marker(
    settings: &Settings,
    registry: &PartnerRegistry,
    ec_id: &str,
    state: &mut PullSyncMarkerState,
    response: &mut Response<EdgeBody>,
) {
    let now = current_timestamp();
    let expires_at = now.saturating_add(MARKER_MAX_AGE_SECS);
    let Some(value) = create_marker(settings, registry, ec_id, expires_at) else {
        return;
    };
    append_cookie(response, &format_marker_cookie(&value, MARKER_MAX_AGE_SECS));
    *state = PullSyncMarkerState::Valid { expires_at };
}

fn expire_if_present(state: &mut PullSyncMarkerState, response: &mut Response<EdgeBody>) {
    if state.was_present() {
        expire_marker(state, response);
    }
}

fn append_cookie(response: &mut Response<EdgeBody>, value: &str) {
    match HeaderValue::from_str(value) {
        Ok(value) => {
            response.headers_mut().append(header::SET_COOKIE, value);
        }
        Err(err) => {
            log::warn!("Skipping pull-sync marker cookie: invalid header value: {err}");
        }
    }
}

fn format_marker_cookie(value: &str, max_age: u64) -> String {
    format!(
        "{COOKIE_TS_EC_PULL_COMPLETE}={value}; Path=/; Secure; SameSite=Lax; Max-Age={max_age}; HttpOnly"
    )
}

#[cfg(test)]
pub(crate) fn create_marker_for_test(
    settings: &Settings,
    registry: &PartnerRegistry,
    ec_id: &str,
) -> String {
    create_marker_for_test_with_expiry(
        settings,
        registry,
        ec_id,
        current_timestamp().saturating_add(MARKER_MAX_AGE_SECS),
    )
}

#[cfg(test)]
pub(crate) fn create_marker_for_test_with_expiry(
    settings: &Settings,
    registry: &PartnerRegistry,
    ec_id: &str,
    expires_at: u64,
) -> String {
    create_marker(settings, registry, ec_id, expires_at)
        .expect("should create marker for non-empty test registry")
}

fn create_marker(
    settings: &Settings,
    registry: &PartnerRegistry,
    ec_id: &str,
    expires_at: u64,
) -> Option<String> {
    let fingerprint = partner_set_fingerprint(registry)?;
    let payload = marker_payload(ec_id, expires_at, &fingerprint);
    let key = marker_key(settings);
    let mut mac = HmacSha256::new_from_slice(&key).expect("should create marker HMAC");
    mac.update(payload.as_bytes());
    let tag = hex::encode(mac.finalize().into_bytes());
    Some(format!("{MARKER_VERSION}.{expires_at}.{fingerprint}.{tag}"))
}

fn validate_marker(
    value: &str,
    settings: &Settings,
    registry: &PartnerRegistry,
    ec_id: &str,
    now: u64,
) -> Option<u64> {
    if value.len() > MAX_MARKER_LENGTH {
        return None;
    }

    let mut segments = value.split('.');
    let version = segments.next()?;
    let expires = segments.next()?;
    let fingerprint = segments.next()?;
    let tag = segments.next()?;
    if segments.next().is_some() || version != MARKER_VERSION {
        return None;
    }

    let expires_at = expires.parse::<u64>().ok()?;
    if expires_at <= now || expires_at > now.saturating_add(MARKER_MAX_AGE_SECS) {
        return None;
    }

    let expected_fingerprint = partner_set_fingerprint(registry)?;
    if fingerprint != expected_fingerprint {
        return None;
    }

    let tag = hex::decode(tag).ok()?;
    if tag.len() != 32 {
        return None;
    }

    let payload = marker_payload(ec_id, expires_at, fingerprint);
    let key = marker_key(settings);
    let mut mac = HmacSha256::new_from_slice(&key).expect("should create marker HMAC");
    mac.update(payload.as_bytes());
    mac.verify_slice(&tag).ok()?;
    Some(expires_at)
}

fn marker_key(settings: &Settings) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(settings.ec.passphrase.expose().as_bytes())
        .expect("should create marker key HMAC");
    mac.update(MARKER_KEY_LABEL);
    mac.finalize().into_bytes().into()
}

fn marker_payload(ec_id: &str, expires_at: u64, fingerprint: &str) -> String {
    format!("{MARKER_VERSION}\0{ec_id}\0{expires_at}\0{fingerprint}")
}

fn partner_set_fingerprint(registry: &PartnerRegistry) -> Option<String> {
    let domains = sorted_pull_partner_domains(registry);
    if domains.is_empty() {
        return None;
    }

    let mut hasher = Sha256::new();
    hasher.update(b"trusted-server/ec-pull-partners/v1\0");
    for domain in domains {
        hasher.update((domain.len() as u64).to_be_bytes());
        hasher.update(domain.as_bytes());
    }
    Some(hex::encode(hasher.finalize()))
}

fn sorted_pull_partner_domains(registry: &PartnerRegistry) -> Vec<String> {
    let mut domains = registry
        .pull_enabled_partners()
        .into_iter()
        .map(|partner| partner.source_domain.clone())
        .collect::<Vec<_>>();
    domains.sort();
    domains
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ec::kv_types::KvEntry;
    use crate::redacted::Redacted;
    use crate::settings::{EcPartner, Settings};
    use crate::test_support::tests::create_test_settings;

    const EC_ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.ABC123";

    fn pull_partner(source_domain: &str) -> EcPartner {
        EcPartner {
            name: format!("Partner {source_domain}"),
            source_domain: source_domain.to_owned(),
            openrtb_atype: EcPartner::default_openrtb_atype(),
            bidstream_enabled: true,
            api_token: Redacted::new(format!("token-{source_domain}-32-bytes-minimum-value")),
            batch_rate_limit: EcPartner::default_batch_rate_limit(),
            pull_sync_enabled: true,
            pull_sync_url: Some(format!("https://sync.{source_domain}/pull")),
            pull_sync_allowed_domains: vec![format!("sync.{source_domain}")],
            pull_sync_ttl_sec: EcPartner::default_pull_sync_ttl_sec(),
            pull_sync_rate_limit: EcPartner::default_pull_sync_rate_limit(),
            ts_pull_token: Some(Redacted::new("pull-token".to_owned())),
        }
    }

    fn settings_and_registry(domains: &[&str]) -> (Settings, PartnerRegistry) {
        let mut settings = create_test_settings();
        settings.ec.partners = domains.iter().map(|domain| pull_partner(domain)).collect();
        let registry = PartnerRegistry::from_config(&settings.ec.partners)
            .expect("should build pull partner registry");
        (settings, registry)
    }

    fn empty_response() -> Response<EdgeBody> {
        Response::builder()
            .status(200)
            .body(EdgeBody::empty())
            .expect("should build response")
    }

    fn live_snapshot(ec_id: &str, domains: &[&str]) -> EcKvSnapshot {
        let mut entry = KvEntry::tombstone(1_000);
        entry.consent.ok = true;
        for domain in domains {
            entry.ids.insert(
                (*domain).to_owned(),
                crate::ec::kv_types::KvPartnerId {
                    uid: format!("uid-{domain}"),
                },
            );
        }
        EcKvSnapshot::Present {
            ec_id: ec_id.to_owned(),
            entry: Box::new(entry),
            generation: Some(1),
        }
    }

    fn marker_cookies(response: &Response<EdgeBody>) -> Vec<&str> {
        response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .collect()
    }

    fn assert_expired(state: &PullSyncMarkerState, response: &Response<EdgeBody>) {
        assert!(matches!(state, PullSyncMarkerState::Absent));
        assert_eq!(
            marker_cookies(response),
            vec!["ts-ec-pull-complete=; Path=/; Secure; SameSite=Lax; Max-Age=0; HttpOnly"],
            "should emit the exact host-only expiration cookie"
        );
    }

    #[test]
    fn marker_round_trip_binds_ec_and_partner_set() {
        let (settings, registry) = settings_and_registry(&["a.example.com", "b.example.com"]);
        let now = 1_000;
        let marker =
            create_marker(&settings, &registry, EC_ID, now + 3_600).expect("should create marker");

        assert_eq!(
            validate_marker(&marker, &settings, &registry, EC_ID, now),
            Some(now + 3_600),
            "should validate the issued marker"
        );
        assert!(
            validate_marker(&marker, &settings, &registry, "wrong", now).is_none(),
            "should reject a marker for another EC"
        );
    }

    #[test]
    fn marker_fingerprint_is_order_independent_and_set_sensitive() {
        let (settings, first) = settings_and_registry(&["a.example.com", "b.example.com"]);
        let (_, reordered) = settings_and_registry(&["b.example.com", "a.example.com"]);
        let (_, changed) = settings_and_registry(&["a.example.com", "c.example.com"]);
        let marker = create_marker(&settings, &first, EC_ID, 4_600).expect("should create marker");

        assert!(
            validate_marker(&marker, &settings, &reordered, EC_ID, 1_000).is_some(),
            "config ordering should not change the marker"
        );
        assert!(
            validate_marker(&marker, &settings, &changed, EC_ID, 1_000).is_none(),
            "partner-set changes should invalidate the marker"
        );
    }

    #[test]
    fn fingerprint_changes_for_enable_disable_add_and_remove() {
        let (_, enabled_one) = settings_and_registry(&["a.example.com"]);
        let (_, enabled_two) = settings_and_registry(&["a.example.com", "b.example.com"]);
        let mut disabled_config = pull_partner("a.example.com");
        disabled_config.pull_sync_enabled = false;
        let disabled = PartnerRegistry::from_config(&[disabled_config])
            .expect("should build disabled registry");
        let removed = PartnerRegistry::empty();

        let base = partner_set_fingerprint(&enabled_one);
        assert_ne!(
            base,
            partner_set_fingerprint(&enabled_two),
            "adding a pull partner should change the fingerprint"
        );
        assert_ne!(
            base,
            partner_set_fingerprint(&disabled),
            "disabling a pull partner should change the fingerprint"
        );
        assert_ne!(
            base,
            partner_set_fingerprint(&removed),
            "removing a pull partner should change the fingerprint"
        );
        assert_ne!(
            partner_set_fingerprint(&disabled),
            partner_set_fingerprint(&enabled_one),
            "enabling a partner should change the fingerprint"
        );
    }

    #[test]
    fn reconcile_rejects_snapshot_bound_to_different_ec() {
        let (settings, registry) = settings_and_registry(&["a.example.com"]);
        let snapshot = live_snapshot("different-ec", &["a.example.com"]);
        let mut valid_state = PullSyncMarkerState::Valid { expires_at: 4_600 };
        let mut valid_response = empty_response();

        reconcile_marker(
            &settings,
            &registry,
            Some(EC_ID),
            &snapshot,
            &mut valid_state,
            &mut valid_response,
        );
        assert_expired(&valid_state, &valid_response);

        let mut absent_state = PullSyncMarkerState::Absent;
        let mut absent_response = empty_response();
        reconcile_marker(
            &settings,
            &registry,
            Some(EC_ID),
            &snapshot,
            &mut absent_state,
            &mut absent_response,
        );
        assert!(marker_cookies(&absent_response).is_empty());
        assert!(matches!(absent_state, PullSyncMarkerState::Absent));
    }

    #[test]
    fn authoritative_incomplete_states_expire_valid_marker() {
        let (settings, registry) = settings_and_registry(&["a.example.com"]);
        let mut tombstone = KvEntry::tombstone(1_000);
        tombstone.ids.insert(
            "a.example.com".to_owned(),
            crate::ec::kv_types::KvPartnerId {
                uid: "stale".to_owned(),
            },
        );
        let snapshots = [
            live_snapshot(EC_ID, &[]),
            EcKvSnapshot::Missing {
                ec_id: EC_ID.to_owned(),
            },
            EcKvSnapshot::Present {
                ec_id: EC_ID.to_owned(),
                entry: Box::new(tombstone),
                generation: Some(1),
            },
        ];

        for snapshot in snapshots {
            let mut state = PullSyncMarkerState::Valid { expires_at: 4_600 };
            let mut response = empty_response();
            reconcile_marker(
                &settings,
                &registry,
                Some(EC_ID),
                &snapshot,
                &mut state,
                &mut response,
            );
            assert_expired(&state, &response);
        }
    }

    #[test]
    fn non_authoritative_states_preserve_valid_fixed_expiry_marker() {
        let (settings, registry) = settings_and_registry(&["a.example.com"]);
        let snapshots = [
            EcKvSnapshot::Failed {
                ec_id: EC_ID.to_owned(),
            },
            EcKvSnapshot::NotRead,
        ];

        for snapshot in snapshots {
            let mut state = PullSyncMarkerState::Valid { expires_at: 4_600 };
            let mut response = empty_response();
            reconcile_marker(
                &settings,
                &registry,
                Some(EC_ID),
                &snapshot,
                &mut state,
                &mut response,
            );
            assert!(matches!(
                state,
                PullSyncMarkerState::Valid { expires_at: 4_600 }
            ));
            assert!(marker_cookies(&response).is_empty());
        }
    }

    #[test]
    fn invalid_marker_is_cleared_without_authoritative_snapshot() {
        let (settings, registry) = settings_and_registry(&["a.example.com"]);
        let mut state = PullSyncMarkerState::Invalid;
        let mut response = empty_response();

        reconcile_marker(
            &settings,
            &registry,
            Some(EC_ID),
            &EcKvSnapshot::NotRead,
            &mut state,
            &mut response,
        );

        assert_expired(&state, &response);
    }

    #[test]
    fn marker_rejects_expired_overlong_and_tampered_values() {
        let (settings, registry) = settings_and_registry(&["a.example.com"]);
        let marker =
            create_marker(&settings, &registry, EC_ID, 4_600).expect("should create marker");
        let mut tampered = marker.clone();
        tampered.push('0');

        assert!(
            validate_marker(&marker, &settings, &registry, EC_ID, 4_600).is_none(),
            "expired marker should fail"
        );
        assert!(
            validate_marker(&marker, &settings, &registry, EC_ID, 999).is_none(),
            "marker more than one hour in the future should fail"
        );
        assert!(
            validate_marker(&tampered, &settings, &registry, EC_ID, 1_000).is_none(),
            "tampering should fail"
        );
        assert!(
            validate_marker(
                &"x".repeat(MAX_MARKER_LENGTH + 1),
                &settings,
                &registry,
                EC_ID,
                1_000
            )
            .is_none(),
            "overlong marker should fail"
        );
    }

    #[test]
    fn marker_rejects_wrong_passphrase_and_empty_partner_set() {
        let (settings, registry) = settings_and_registry(&["a.example.com"]);
        let marker =
            create_marker(&settings, &registry, EC_ID, 4_600).expect("should create marker");
        let mut changed_settings = settings.clone();
        changed_settings.ec.passphrase =
            Redacted::new("different-secret-key-32-bytes-minimum".to_owned());
        let empty = PartnerRegistry::empty();

        assert!(
            validate_marker(&marker, &changed_settings, &registry, EC_ID, 1_000).is_none(),
            "passphrase rotation should invalidate the marker"
        );
        assert!(
            create_marker(&settings, &empty, EC_ID, 4_600).is_none(),
            "empty partner sets should not produce a marker"
        );
    }

    #[test]
    fn marker_cookie_is_host_only_and_secure() {
        let cookie = format_marker_cookie("value", MARKER_MAX_AGE_SECS);
        assert_eq!(
            cookie,
            "ts-ec-pull-complete=value; Path=/; Secure; SameSite=Lax; Max-Age=3600; HttpOnly"
        );
        assert!(!cookie.contains("Domain="), "marker should be host-only");
    }

    #[test]
    fn completeness_requires_all_pull_partner_ids() {
        let (_, registry) = settings_and_registry(&["a.example.com", "b.example.com"]);
        let mut entry = KvEntry::minimal("a.example.com", "uid-a", 1_000);
        assert!(!entry_is_pull_complete(&entry, &registry));

        entry.ids.insert(
            "b.example.com".to_owned(),
            crate::ec::kv_types::KvPartnerId {
                uid: "uid-b".to_owned(),
            },
        );
        assert!(entry_is_pull_complete(&entry, &registry));
    }
}
