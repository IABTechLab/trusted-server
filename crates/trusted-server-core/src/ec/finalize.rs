//! EC response finalization.
//!
//! Centralizes post-routing EC behavior so all handlers get consistent cookie
//! and KV semantics.

use std::collections::HashSet;

use edgezero_core::body::Body as EdgeBody;
use http::Response;

use crate::constants::EC_RESPONSE_HEADERS;
use crate::settings::Settings;

use super::EcContext;
use super::consent::ec_consent_withdrawn;
use super::cookies::{expire_ec_cookie, set_ec_cookie};
use super::kv::KvIdentityGraph;
use super::log_id;
use super::prebid_eids::ingest_eid_cookies;
use super::provider::apply_provider_response_headers;
use super::registry::PartnerRegistry;

/// Finalizes EC response behavior for all routes.
///
/// Applies the resolved consent gate, last-seen updates, cookie
/// reconciliation, Prebid EID ingestion, and cookie writes for new EC generation.
///
/// When the request carries an explicit withdrawal signal (a storage opt-out or
/// a TCF record refusing storage) and the client presented a cookie, the browser
/// response clears the EC cookie immediately and the EC identity-graph KV
/// tombstone is the authoritative revocation marker. A request that is merely
/// not permitted (pre-consent or fail-closed) strips EC response headers but
/// leaves an already-issued cookie intact. There is no separate consent KV
/// store to clean up.
///
/// `eids_cookie` should be the raw value of the `ts-eids` cookie extracted
/// from the request *before* routing consumes it.
pub fn ec_finalize_response(
    settings: &Settings,
    ec_context: &EcContext,
    kv: Option<&KvIdentityGraph>,
    registry: &PartnerRegistry,
    eids_cookie: Option<&str>,
    sharedid_cookie: Option<&str>,
    response: &mut Response<EdgeBody>,
) {
    // Apply any response headers the active provider asked for during
    // generation (for example to request more client evidence). This is empty
    // unless a provider produced headers, so it is safe on every path. Each
    // one was checked against core's reserved response surface at capture
    // time in `EcContext::generate_with_provider`, so nothing here can set a
    // managed `ts-` cookie, an `x-ts-` header, or a framing or hop-by-hop
    // header. They accumulate with whatever the origin returned rather than
    // replacing it, for the reasons on
    // `provider::apply_provider_response_headers`.
    apply_provider_response_headers(
        response.headers_mut(),
        ec_context.response_headers().iter().cloned(),
    );

    let ec_permitted = ec_context.ec_allowed();

    if !ec_permitted {
        // Always strip EC-specific response headers when EC is not permitted for
        // this request, covering both an explicit withdrawal and fail-closed
        // cases such as missing geo or undecodable consent input.
        clear_ec_headers_on_response(response, Some(registry));

        // Only expire the browser cookie and tombstone the identity-graph row
        // when the request carries an explicit withdrawal signal. A pre-consent
        // or fail-closed state (consent is simply not granted) strips headers
        // but must not destroy an already-issued identifier, or a returning user
        // would be permanently withdrawn before they ever get to consent.
        if ec_consent_withdrawn(ec_context.consent()) && ec_context.cookie_was_present() {
            expire_ec_cookie(settings, response);

            // Compute once for the authoritative identity-graph tombstones.
            let keys_to_withdraw = withdrawal_kv_keys(ec_context);

            // The identity-graph tombstone is the authoritative withdrawal marker
            // for subsequent EC behavior.
            if let Some(graph) = kv {
                apply_withdrawal_tombstones(&keys_to_withdraw, |kv_key| {
                    if let Err(err) = graph.write_withdrawal_tombstone(kv_key) {
                        log::error!(
                            "Failed to write withdrawal tombstone for EC ID '{}': {err:?}",
                            log_id(kv_key),
                        );
                    }
                });
            }
        }

        return;
    }

    // Returning user: EC is permitted and came from the request.
    if ec_context.ec_was_present() && !ec_context.ec_generated() && ec_permitted {
        // Key EID ingestion by the provider's canonical form of the identifier,
        // the key the identity-graph row is stored under, so an ingested EID
        // lands on the live row rather than creating a second one keyed by the
        // value the browser carries.
        if let (Some(graph), Some(kv_key)) = (kv, ec_context.ec_kv_key()) {
            ingest_eid_cookies(eids_cookie, sharedid_cookie, &kv_key, graph, registry);
        }

        // Ordinary returning-user page views no longer refresh the browser
        // cookie, emit the EC header, or update KV TTL.
        return;
    }

    // Newly generated EC in this request. Do not emit a generated EC when
    // there is no KV graph: that would mint a browser cookie with no backing
    // identity-graph row, producing a phantom ID on later requests.
    if ec_context.ec_generated() {
        let (Some(graph), Some(kv_key)) = (kv, ec_context.ec_kv_key()) else {
            log::info!(
                "Skipping generated EC response write because the KV graph or the \
                 identity-graph key is unavailable"
            );
            return;
        };

        ingest_eid_cookies(eids_cookie, sharedid_cookie, &kv_key, graph, registry);
        set_ec_cookie_on_response(settings, ec_context, response);
    }
}

/// Sets the EC cookie on response when an EC ID is available.
pub fn set_ec_cookie_on_response(
    settings: &Settings,
    ec_context: &EcContext,
    response: &mut Response<EdgeBody>,
) {
    if let Some(ec_id) = ec_context.ec_value() {
        set_ec_cookie(settings, response, ec_id);
    }
}

/// Removes EC-specific response headers.
///
/// In addition to the fixed [`EC_RESPONSE_HEADERS`], this also strips dynamic
/// `X-ts-<source_domain>` headers for registered partners. Other `x-ts-*`
/// headers are intentionally preserved because they may be set by non-EC middleware.
fn clear_ec_headers_on_response(
    response: &mut Response<EdgeBody>,
    registry: Option<&PartnerRegistry>,
) {
    for header in EC_RESPONSE_HEADERS {
        response.headers_mut().remove(*header);
    }

    if let Some(registry) = registry {
        for partner in registry.all() {
            response
                .headers_mut()
                .remove(partner_response_header(&partner.source_domain).as_str());
        }
    }
}

fn partner_response_header(source_domain: &str) -> String {
    format!("x-ts-{source_domain}")
}

/// Clears EC cookie and removes EC-specific response headers.
///
/// Used when the request carries an explicit withdrawal signal.
pub fn clear_ec_on_response(settings: &Settings, response: &mut Response<EdgeBody>) {
    expire_ec_cookie(settings, response);
    clear_ec_headers_on_response(response, None);
}

/// The identity-graph keys a withdrawal must tombstone.
///
/// Both the `ts-ec` cookie the request carried and the active identifier are
/// turned into keys by the provider that owns them, so the tombstone lands on
/// the row the live identifier is stored under rather than on the raw cookie
/// value. An identifier no provider this deployment reads owns produces no key
/// and is dropped, which is the same filtering the previous shape check did.
/// The two collapse to one key when they are the same identity written two
/// ways.
fn withdrawal_kv_keys(ec_context: &EcContext) -> HashSet<String> {
    let mut keys = HashSet::new();

    if let Some(cookie_kv_key) = ec_context.cookie_ec_kv_key() {
        keys.insert(cookie_kv_key);
    }

    if let Some(active_kv_key) = ec_context.ec_kv_key() {
        keys.insert(active_kv_key);
    }

    keys
}

fn apply_withdrawal_tombstones<F>(kv_keys: &HashSet<String>, mut write_tombstone: F)
where
    F: FnMut(&str),
{
    for kv_key in kv_keys {
        write_tombstone(kv_key);
    }
}

#[cfg(test)]
mod tests {
    use http::HeaderValue;

    use super::*;
    use crate::consent::jurisdiction::Jurisdiction;
    use crate::consent::types::{ConsentContext, ConsentSource};
    use crate::redacted::Redacted;
    use crate::settings::EcPartner;
    use crate::test_support::tests::create_test_settings;

    fn empty_response() -> Response<EdgeBody> {
        Response::builder()
            .status(200)
            .body(EdgeBody::empty())
            .expect("should build test response")
    }

    fn set_header(response: &mut Response<EdgeBody>, name: &str, value: &str) {
        response.headers_mut().insert(
            http::header::HeaderName::from_bytes(name.as_bytes())
                .expect("should parse header name"),
            HeaderValue::from_str(value).expect("should parse header value"),
        );
    }

    fn get_header<'a>(response: &'a Response<EdgeBody>, name: &str) -> Option<&'a HeaderValue> {
        response.headers().get(name)
    }

    fn get_header_str<'a>(response: &'a Response<EdgeBody>, name: &str) -> Option<&'a str> {
        response.headers().get(name).and_then(|v| v.to_str().ok())
    }

    fn make_context(
        ec_value: Option<&str>,
        cookie_ec_value: Option<&str>,
        ec_was_present: bool,
        ec_generated: bool,
        jurisdiction: Jurisdiction,
        ec_allowed: bool,
    ) -> EcContext {
        let consent = ConsentContext {
            jurisdiction,
            source: ConsentSource::Cookie,
            ..Default::default()
        };

        make_context_with_consent(
            ec_value,
            cookie_ec_value,
            ec_was_present,
            ec_generated,
            consent,
            ec_allowed,
        )
    }

    fn make_context_with_consent(
        ec_value: Option<&str>,
        cookie_ec_value: Option<&str>,
        ec_was_present: bool,
        ec_generated: bool,
        consent: ConsentContext,
        ec_allowed: bool,
    ) -> EcContext {
        EcContext::new_for_test_with_cookie(
            ec_value.map(str::to_owned),
            cookie_ec_value.map(str::to_owned),
            ec_was_present,
            ec_generated,
            consent,
            ec_allowed,
        )
    }

    /// The identifier [`CanonicalizingProvider`] mints, as the browser carries
    /// it in the `ts-ec` cookie.
    const CANONICAL_COOKIE_VALUE: &str = "t0ca~MiXeD.CaseId";

    /// The identity-graph key generation writes that identifier's row under.
    /// Pinned to the mint path by
    /// `generate_keys_the_identity_graph_by_the_normalized_identifier` in the
    /// `ec` module tests.
    const CANONICAL_KV_KEY: &str = "t0ca~mixed.caseid";

    fn canonicalizing_context(
        ec_was_present: bool,
        ec_generated: bool,
        consent: ConsentContext,
        ec_allowed: bool,
    ) -> EcContext {
        make_context_with_consent(
            Some(CANONICAL_COOKIE_VALUE),
            Some(CANONICAL_COOKIE_VALUE),
            ec_was_present,
            ec_generated,
            consent,
            ec_allowed,
        )
        .with_provider_for_test(std::sync::Arc::new(
            crate::ec::tests::CanonicalizingProvider,
        ))
    }

    fn graph_with_live_canonical_row() -> KvIdentityGraph {
        let graph = KvIdentityGraph::in_memory("finalize-canonical-store");
        graph
            .create(
                CANONICAL_KV_KEY,
                &crate::ec::kv_types::KvEntry::minimal(
                    "ssp.example.com",
                    "partner-uid-123",
                    1_741_824_000,
                ),
            )
            .expect("should write the row generation keys by the canonical form");
        graph
    }

    fn sample_ec_id(suffix: &str) -> String {
        format!("{}.{suffix}", "a".repeat(64))
    }

    fn make_partner(source_domain: &str) -> EcPartner {
        EcPartner {
            name: format!("Partner {source_domain}"),
            source_domain: source_domain.to_owned(),
            openrtb_atype: EcPartner::default_openrtb_atype(),
            bidstream_enabled: true,
            api_token: Redacted::new(format!("token-{source_domain}-32-bytes-minimum-value")),
            batch_rate_limit: EcPartner::default_batch_rate_limit(),
            pull_sync_enabled: false,
            pull_sync_url: None,
            pull_sync_allowed_domains: vec![],
            pull_sync_ttl_sec: EcPartner::default_pull_sync_ttl_sec(),
            pull_sync_rate_limit: EcPartner::default_pull_sync_rate_limit(),
            ts_pull_token: None,
        }
    }

    #[test]
    fn withdrawal_kv_keys_returns_cookie_ec_only_when_active_missing() {
        let cookie_ec = sample_ec_id("cook1e");
        let ec_context = make_context(
            None,
            Some(&cookie_ec),
            true,
            false,
            Jurisdiction::Unknown,
            false,
        );

        let ids = withdrawal_kv_keys(&ec_context);

        assert_eq!(ids.len(), 1, "should include exactly one EC ID");
        assert!(
            ids.contains(&cookie_ec),
            "should include the cookie EC value"
        );
    }

    #[test]
    fn withdrawal_kv_keys_deduplicates_matching_cookie_and_active_ec() {
        let ec_id = sample_ec_id("same01");
        let ec_context = make_context(
            Some(&ec_id),
            Some(&ec_id),
            true,
            false,
            Jurisdiction::Unknown,
            false,
        );

        let ids = withdrawal_kv_keys(&ec_context);

        assert_eq!(ids.len(), 1, "should deduplicate identical EC IDs");
        assert!(ids.contains(&ec_id), "should retain the shared EC ID");
    }

    #[test]
    fn withdrawal_kv_keys_includes_both_cookie_and_active_when_different() {
        let active_ec = sample_ec_id("activ1");
        let cookie_ec = sample_ec_id("cook1e");
        let ec_context = make_context(
            Some(&active_ec),
            Some(&cookie_ec),
            true,
            false,
            Jurisdiction::Unknown,
            false,
        );

        let ids = withdrawal_kv_keys(&ec_context);

        assert_eq!(ids.len(), 2, "should include both distinct EC IDs");
        assert!(ids.contains(&active_ec), "should include active EC ID");
        assert!(ids.contains(&cookie_ec), "should include cookie EC ID");
    }

    #[test]
    fn withdrawal_kv_keys_filters_invalid_values() {
        let valid_ec = sample_ec_id("valid1");
        let ec_context = make_context(
            Some(&valid_ec),
            Some("not-an-ec-id"),
            true,
            false,
            Jurisdiction::Unknown,
            false,
        );

        let ids = withdrawal_kv_keys(&ec_context);

        assert_eq!(ids.len(), 1, "should ignore malformed EC values");
        assert!(ids.contains(&valid_ec), "should keep the valid EC ID");
    }

    #[test]
    fn apply_withdrawal_tombstones_invokes_writer_for_each_ec_id() {
        let first = sample_ec_id("first1");
        let second = sample_ec_id("second");
        let mut ids = HashSet::new();
        ids.insert(first.clone());
        ids.insert(second.clone());

        let mut written = Vec::new();
        apply_withdrawal_tombstones(&ids, |ec_id| written.push(ec_id.to_owned()));
        written.sort();

        let mut expected = vec![first, second];
        expected.sort();
        assert_eq!(written, expected, "should write a tombstone for each EC ID");
    }

    #[test]
    fn clear_ec_on_response_removes_headers_and_expires_cookie() {
        let settings = create_test_settings();
        let mut response = empty_response();
        set_header(&mut response, "x-ts-ec", "abc");
        set_header(&mut response, "x-ts-eids", "[]");
        set_header(&mut response, "x-ts-unrelated", "keep-me");

        clear_ec_on_response(&settings, &mut response);

        assert!(
            get_header(&response, "x-ts-ec").is_none(),
            "should remove x-ts-ec"
        );
        assert!(
            get_header(&response, "x-ts-eids").is_none(),
            "should remove x-ts-eids"
        );
        assert_eq!(
            get_header_str(&response, "x-ts-unrelated"),
            Some("keep-me"),
            "should preserve unrelated x-ts headers without a partner registry"
        );

        let set_cookie = get_header(&response, "set-cookie")
            .expect("should append Set-Cookie for expiry")
            .to_str()
            .expect("should render set-cookie as utf-8");

        assert!(
            set_cookie.contains("Max-Age=0"),
            "should expire the EC cookie"
        );
    }

    #[test]
    fn finalize_withdrawal_clears_cookie_and_headers() {
        let settings = create_test_settings();
        let ec_id = sample_ec_id("aBc123");
        let consent = ConsentContext {
            jurisdiction: Jurisdiction::UsState("CA".to_owned()),
            gpc: true,
            source: ConsentSource::Cookie,
            ..Default::default()
        };
        let ec_context =
            make_context_with_consent(Some(&ec_id), Some(&ec_id), true, false, consent, false);
        let mut response = empty_response();
        set_header(&mut response, "x-ts-ec", "stale");
        set_header(&mut response, "x-ts-eids", "[]");
        set_header(&mut response, "x-ts-ssp.example.com", "partner-uid-123");
        set_header(&mut response, "x-ts-unrelated", "keep-me");

        let partners = vec![make_partner("ssp.example.com")];
        let test_registry = PartnerRegistry::from_config(&partners).expect("should build registry");
        ec_finalize_response(
            &settings,
            &ec_context,
            None,
            &test_registry,
            None,
            None,
            &mut response,
        );

        assert!(
            get_header(&response, "x-ts-ec").is_none(),
            "withdrawal should clear x-ts-ec header"
        );
        assert!(
            get_header(&response, "x-ts-eids").is_none(),
            "withdrawal should clear x-ts-eids header"
        );
        assert!(
            get_header(&response, "x-ts-ssp.example.com").is_none(),
            "withdrawal should clear registered partner header"
        );
        assert_eq!(
            get_header_str(&response, "x-ts-unrelated"),
            Some("keep-me"),
            "withdrawal should preserve unrelated x-ts header"
        );
        let set_cookie = get_header(&response, "set-cookie")
            .expect("withdrawal should expire cookie")
            .to_str()
            .expect("set-cookie should be utf-8");
        assert!(
            set_cookie.contains("Max-Age=0"),
            "withdrawal should set Max-Age=0"
        );
    }

    #[test]
    fn finalize_returning_user_with_cookie_mismatch_sets_no_header_or_cookie() {
        let settings = create_test_settings();
        let active_ec = sample_ec_id("activ1");
        let cookie_ec = sample_ec_id("cook1e");
        let ec_context = make_context(
            Some(&active_ec),
            Some(&cookie_ec),
            true,
            false,
            Jurisdiction::NonRegulated,
            true,
        );
        let mut response = empty_response();

        let test_registry = PartnerRegistry::empty();
        ec_finalize_response(
            &settings,
            &ec_context,
            None,
            &test_registry,
            None,
            None,
            &mut response,
        );

        assert!(
            get_header(&response, "x-ts-ec").is_none(),
            "returning user should not set x-ts-ec"
        );
        assert!(
            get_header(&response, "set-cookie").is_none(),
            "returning user should not refresh or repair cookie"
        );
    }

    #[test]
    fn finalize_returning_user_sets_no_header_or_cookie() {
        let settings = create_test_settings();
        let ec_id = sample_ec_id("mtch01");
        let ec_context = make_context(
            Some(&ec_id),
            Some(&ec_id),
            true,
            false,
            Jurisdiction::NonRegulated,
            true,
        );
        let mut response = empty_response();

        let test_registry = PartnerRegistry::empty();
        ec_finalize_response(
            &settings,
            &ec_context,
            None,
            &test_registry,
            None,
            None,
            &mut response,
        );

        assert!(
            get_header(&response, "x-ts-ec").is_none(),
            "returning user should not set x-ts-ec"
        );
        assert!(
            get_header(&response, "set-cookie").is_none(),
            "returning user should not refresh cookie"
        );
    }

    #[test]
    fn finalize_generated_ec_without_kv_skips_cookie_and_header() {
        let settings = create_test_settings();
        let generated_ec = sample_ec_id("gen123");
        let ec_context = make_context(
            Some(&generated_ec),
            None,
            false,
            true,
            Jurisdiction::NonRegulated,
            true,
        );
        let mut response = empty_response();

        let test_registry = PartnerRegistry::empty();
        ec_finalize_response(
            &settings,
            &ec_context,
            None,
            &test_registry,
            None,
            None,
            &mut response,
        );

        assert!(
            get_header(&response, "x-ts-ec").is_none(),
            "generated EC without KV should not set response header"
        );
        assert!(
            get_header(&response, "set-cookie").is_none(),
            "generated EC without KV should not set cookie"
        );
    }

    #[test]
    fn finalize_denied_without_cookie_is_noop() {
        let settings = create_test_settings();
        let ec_context = make_context(None, None, false, false, Jurisdiction::Unknown, false);
        let mut response = empty_response();

        let test_registry = PartnerRegistry::empty();
        ec_finalize_response(
            &settings,
            &ec_context,
            None,
            &test_registry,
            None,
            None,
            &mut response,
        );

        assert!(
            get_header(&response, "x-ts-ec").is_none(),
            "should not set EC header"
        );
        assert!(
            get_header(&response, "set-cookie").is_none(),
            "should not mutate cookie when there is nothing to revoke"
        );
    }

    #[test]
    fn finalize_not_permitted_without_withdrawal_keeps_cookie() {
        // When EC is not permitted (here a fail-closed unknown jurisdiction with
        // no geo) but the request carries no explicit withdrawal signal, the
        // response strips EC headers yet must leave an already-issued cookie
        // intact. A pre-consent or transient fail-closed request must not
        // permanently withdraw a returning user before they get to consent.
        let settings = create_test_settings();
        let ec_id = sample_ec_id("unk001");
        let ec_context = make_context(
            Some(&ec_id),
            Some(&ec_id),
            true,
            false,
            Jurisdiction::Unknown,
            false,
        );
        let mut response = empty_response();
        set_header(&mut response, "x-ts-ec", &ec_id);
        set_header(&mut response, "x-ts-eids", "[]");

        let test_registry = PartnerRegistry::empty();
        ec_finalize_response(
            &settings,
            &ec_context,
            None,
            &test_registry,
            None,
            None,
            &mut response,
        );

        assert!(
            get_header(&response, "x-ts-ec").is_none(),
            "should strip EC header when EC is not permitted"
        );
        assert!(
            get_header(&response, "x-ts-eids").is_none(),
            "should strip EID header when EC is not permitted"
        );
        assert!(
            get_header(&response, "set-cookie").is_none(),
            "a not-permitted request without a withdrawal signal should keep the cookie"
        );
    }

    #[test]
    fn set_ec_cookie_on_response_writes_the_ts_ec_cookie() {
        // The positive case: when an EC value is present, the finalize path
        // writes the ts-ec cookie to the browser, carrying the EC id.
        let settings = create_test_settings();
        let ec_id = sample_ec_id("setck1");
        let ec_context = make_context(
            Some(&ec_id),
            None,
            false,
            true,
            Jurisdiction::NonRegulated,
            true,
        );
        let mut response = empty_response();

        set_ec_cookie_on_response(&settings, &ec_context, &mut response);

        let set_cookie =
            get_header_str(&response, "set-cookie").expect("an EC value should write a Set-Cookie");
        assert!(
            set_cookie.contains("ts-ec=") && set_cookie.contains(&ec_id),
            "should write the ts-ec cookie carrying the EC id, got: {set_cookie}"
        );
    }

    #[test]
    fn closed_consent_gate_writes_no_ec_cookie() {
        // The gate: with the consent gate closed (ec_allowed = false), no
        // ts-ec cookie is written, even when an EC value and a generated flag are
        // present. The consent gate is what suppresses the cookie.
        let settings = create_test_settings();
        let ec_id = sample_ec_id("gated1");
        let ec_context = make_context(
            Some(&ec_id),
            None,
            false,
            true,
            Jurisdiction::NonRegulated,
            false,
        );
        let mut response = empty_response();

        // Pass a KV graph so the missing-graph guard cannot be the reason the
        // cookie is suppressed; the closed gate must be doing the work.
        let kv = KvIdentityGraph::failing("test_store");
        let test_registry = PartnerRegistry::empty();
        ec_finalize_response(
            &settings,
            &ec_context,
            Some(&kv),
            &test_registry,
            None,
            None,
            &mut response,
        );

        assert!(
            get_header(&response, "set-cookie").is_none(),
            "a closed consent gate must not write a ts-ec cookie"
        );
    }

    #[test]
    fn withdrawal_tombstones_the_canonical_row_not_the_cookie_value() {
        // The tombstone is the authoritative revocation marker, so it has to
        // land on the key the live row uses. Written under the raw cookie
        // value it creates a second row nothing reads, and the revocation
        // never takes effect for a provider whose canonical form differs.
        let settings = create_test_settings();
        let graph = graph_with_live_canonical_row();
        let consent = ConsentContext {
            jurisdiction: Jurisdiction::UsState("CA".to_owned()),
            gpc: true,
            source: ConsentSource::Cookie,
            ..Default::default()
        };
        let ec_context = canonicalizing_context(true, false, consent, false);
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &ec_context,
            Some(&graph),
            &PartnerRegistry::empty(),
            None,
            None,
            &mut response,
        );

        let (live_row, _) = graph
            .get(CANONICAL_KV_KEY)
            .expect("should read the canonical row")
            .expect("the canonical row should still exist");
        assert!(
            !live_row.consent.ok,
            "withdrawal should tombstone the row the live identifier is keyed by"
        );
        assert!(
            graph
                .get(CANONICAL_COOKIE_VALUE)
                .expect("should read the graph")
                .is_none(),
            "withdrawal should not write a tombstone under the raw cookie value"
        );
    }

    #[test]
    fn eid_ingestion_keys_by_the_providers_canonical_form() {
        // An ingested EID must join the row the identifier already has. Keyed
        // by the raw cookie value the upsert finds no row and the partner ID
        // is dropped.
        let settings = create_test_settings();
        let graph = graph_with_live_canonical_row();
        let partners = vec![make_partner("sharedid.org")];
        let registry = PartnerRegistry::from_config(&partners).expect("should build registry");
        let consent = ConsentContext {
            jurisdiction: Jurisdiction::NonRegulated,
            source: ConsentSource::Cookie,
            ..Default::default()
        };
        let ec_context = canonicalizing_context(true, false, consent, true);
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &ec_context,
            Some(&graph),
            &registry,
            None,
            Some("shared-cookie-id"),
            &mut response,
        );

        let (row, _) = graph
            .get(CANONICAL_KV_KEY)
            .expect("should read the canonical row")
            .expect("the canonical row should still exist");
        assert_eq!(
            row.ids.get("sharedid.org").map(|id| id.uid.as_str()),
            Some("shared-cookie-id"),
            "the ingested EID should land on the row keyed by the canonical form"
        );
        assert!(
            graph
                .get(CANONICAL_COOKIE_VALUE)
                .expect("should read the graph")
                .is_none(),
            "EID ingestion should not create a row under the raw cookie value"
        );
    }

    /// A provider that sets one cookie of its own and one `Vary` entry, the
    /// two response effects a provider realistically asks for, so a test can
    /// watch both land on a response the origin already wrote headers to.
    #[derive(Debug)]
    struct EvidenceHeaderProvider;

    impl crate::ec::provider::EdgeCookieProvider for EvidenceHeaderProvider {
        fn id(&self) -> &'static str {
            "evidence-header"
        }

        fn code(&self) -> crate::ec::provider::ProviderCode {
            crate::provider_code!("t0eh")
        }

        fn generate(
            &self,
            _request_info: &dyn crate::evidence::RequestInfo,
            _input: &crate::ec::provider::IdentityInput<'_>,
        ) -> Result<
            crate::ec::provider::GeneratedEdgeCookie,
            error_stack::Report<crate::error::TrustedServerError>,
        > {
            Ok(crate::ec::provider::GeneratedEdgeCookie {
                id: Some("evidence-id".to_owned()),
                response_headers: vec![
                    (
                        http::header::SET_COOKIE,
                        HeaderValue::from_static("vendor-ev=abc; Path=/"),
                    ),
                    (http::header::VARY, HeaderValue::from_static("sec-ch-ua")),
                ],
            })
        }

        fn accepts_id(&self, value: &str) -> bool {
            !value.is_empty()
        }

        fn normalize_id_for_kv(&self, value: &str) -> String {
            value.to_owned()
        }
    }

    #[test]
    fn provider_response_headers_reach_the_response_without_dropping_the_origins() {
        // The response finalization runs on is the finished one, so it already
        // carries the publisher origin's own headers. A provider effect must
        // add to those, never replace them: replacing `Set-Cookie` would drop
        // the publisher's session and sign-in cookies, and replacing `Vary`
        // would break the caching the origin asked for.
        let settings = create_test_settings();
        let graph = KvIdentityGraph::in_memory("finalize-provider-headers-store");
        let consent = ConsentContext {
            jurisdiction: Jurisdiction::NonRegulated,
            source: ConsentSource::Cookie,
            ..Default::default()
        };
        let mut ec_context = make_context_with_consent(None, None, false, false, consent, true)
            .with_provider_for_test(std::sync::Arc::new(EvidenceHeaderProvider));
        ec_context
            .generate_if_needed(&settings, Some(&graph))
            .expect("should mint through the provider");

        // What the publisher's origin returned, before EC finalization runs.
        let mut response = empty_response();
        response.headers_mut().append(
            http::header::SET_COOKIE,
            HeaderValue::from_static("publisher_session=origin-value; Path=/; HttpOnly"),
        );
        response.headers_mut().append(
            http::header::VARY,
            HeaderValue::from_static("accept-encoding"),
        );

        ec_finalize_response(
            &settings,
            &ec_context,
            Some(&graph),
            &PartnerRegistry::empty(),
            None,
            None,
            &mut response,
        );

        let cookies: Vec<&str> = response
            .headers()
            .get_all(http::header::SET_COOKIE)
            .iter()
            .map(|value| value.to_str().expect("should render set-cookie as utf-8"))
            .collect();
        assert!(
            cookies
                .iter()
                .any(|cookie| cookie.starts_with("publisher_session=origin-value")),
            "the origin's own cookie must survive a provider effect, got {cookies:?}"
        );
        assert!(
            cookies
                .iter()
                .any(|cookie| cookie.starts_with("vendor-ev=abc")),
            "the provider's cookie must reach the response, got {cookies:?}"
        );
        assert!(
            cookies.iter().any(|cookie| cookie.starts_with("ts-ec=")),
            "core's own managed cookie must still be written, got {cookies:?}"
        );

        let vary: Vec<&str> = response
            .headers()
            .get_all(http::header::VARY)
            .iter()
            .map(|value| value.to_str().expect("should render vary as utf-8"))
            .collect();
        assert!(
            vary.contains(&"accept-encoding"),
            "the origin's Vary must survive a provider effect, got {vary:?}"
        );
        assert!(
            vary.contains(&"sec-ch-ua"),
            "the provider's Vary must reach the response, got {vary:?}"
        );
    }

    /// A provider standing in for the one a deployment switched *to*, with a
    /// different registered code from the provider that minted the live row.
    #[derive(Debug)]
    struct SwitchedProvider;

    impl crate::ec::provider::EdgeCookieProvider for SwitchedProvider {
        fn id(&self) -> &'static str {
            "switched"
        }

        fn code(&self) -> crate::ec::provider::ProviderCode {
            crate::provider_code!("t0sw")
        }

        fn generate(
            &self,
            _request_info: &dyn crate::evidence::RequestInfo,
            _input: &crate::ec::provider::IdentityInput<'_>,
        ) -> Result<
            crate::ec::provider::GeneratedEdgeCookie,
            error_stack::Report<crate::error::TrustedServerError>,
        > {
            Ok(crate::ec::provider::GeneratedEdgeCookie::default())
        }

        fn accepts_id(&self, value: &str) -> bool {
            !value.is_empty()
        }

        fn normalize_id_for_kv(&self, value: &str) -> String {
            value.to_ascii_lowercase()
        }
    }

    #[test]
    fn switching_provider_leaves_the_previous_providers_row_beyond_withdrawal() {
        // Pins what a provider switch really does, which the switching
        // section of the pluggable-providers spec now states plainly. The
        // retired provider's identifier is owned by nobody this deployment
        // reads, so a later withdrawal expires the browser cookie but cannot
        // tombstone the row, and the identifier is never adopted either. If
        // the deferred `legacy_providers` reader list ever lands, this test
        // is meant to fail, so that the spec sentence a deployer acts on is
        // revisited in the same change.
        let settings = create_test_settings();
        let graph = graph_with_live_canonical_row();
        // A TCF record consenting to nothing, under GDPR, so the request
        // carries an explicit refusal of storage. That is the narrow,
        // destructive kind of withdrawal, the one that tombstones rather than
        // merely suppressing, which is the behavior under test.
        let consent = ConsentContext {
            jurisdiction: Jurisdiction::Gdpr,
            tcf: Some(crate::consent::TcfConsent {
                version: 2,
                cmp_id: 0,
                cmp_version: 0,
                consent_screen: 0,
                consent_language: "EN".to_owned(),
                vendor_list_version: 0,
                tcf_policy_version: 2,
                created_ds: 0,
                last_updated_ds: 0,
                purpose_consents: vec![false; 24],
                purpose_legitimate_interests: vec![false; 24],
                vendor_consents: Vec::new(),
                vendor_legitimate_interests: Vec::new(),
                special_feature_opt_ins: vec![false; 12],
            }),
            source: ConsentSource::Cookie,
            ..Default::default()
        };
        // The browser still carries the identifier the previous provider
        // minted, but the deployment now runs a provider with a different
        // code, so read-back treats the cookie as absent and the active
        // identifier is empty.
        let ec_context = make_context_with_consent(
            None,
            Some(CANONICAL_COOKIE_VALUE),
            false,
            false,
            consent,
            false,
        )
        .with_provider_for_test(std::sync::Arc::new(SwitchedProvider));
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &ec_context,
            Some(&graph),
            &PartnerRegistry::empty(),
            None,
            None,
            &mut response,
        );

        assert!(
            ec_context.ec_value().is_none(),
            "the retired provider's identifier must never be adopted by the new one"
        );

        let (row, _) = graph
            .get(CANONICAL_KV_KEY)
            .expect("should read the previous provider's row")
            .expect("the previous provider's row should still exist");
        assert!(
            row.consent.ok,
            "withdrawal cannot reach a retired provider's row without the provider that owns the code"
        );

        let cookies: Vec<&str> = response
            .headers()
            .get_all(http::header::SET_COOKIE)
            .iter()
            .map(|value| value.to_str().expect("should render set-cookie as utf-8"))
            .collect();
        assert!(
            cookies
                .iter()
                .any(|cookie| cookie.starts_with("ts-ec=") && cookie.contains("Max-Age=0")),
            "withdrawal should still expire the browser cookie after a switch, got {cookies:?}"
        );
    }
}
