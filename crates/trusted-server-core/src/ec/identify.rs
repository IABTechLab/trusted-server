//! Identity lookup endpoint (`GET /_ts/api/v1/identify`).
//!
//! Partners authenticate with a Bearer token and receive only their own
//! synced UID for the active EC ID.

use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use http::header::{self, HeaderValue};
use http::{Request, Response, StatusCode};
use url::Url;

use super::auth::authenticate_bearer;
use crate::error::TrustedServerError;
use crate::openrtb::{Eid, Uid};
use crate::settings::Settings;

use super::EcContext;
use super::kv::KvIdentityGraph;
use super::log_id;
use super::registry::PartnerRegistry;

/// Handles `GET /_ts/api/v1/identify`.
///
/// Requires Bearer token authentication. Returns only the requesting
/// partner's UID for the active EC ID.
///
/// # Errors
///
/// Returns [`TrustedServerError`] for response serialization issues.
///
/// # Panics
///
/// Panics if response builder produces an invalid status or body, which cannot
/// happen with the hardcoded values used here.
pub fn handle_identify(
    settings: &Settings,
    kv: &KvIdentityGraph,
    registry: &PartnerRegistry,
    req: &Request<EdgeBody>,
    ec_context: &EcContext,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    let allowed_origin = match classify_origin(req, settings) {
        CorsDecision::Denied => {
            return Ok(apply_identify_cache_headers(
                Response::builder()
                    .status(StatusCode::FORBIDDEN)
                    .body(EdgeBody::empty())
                    .expect("should build forbidden response"),
            ));
        }
        CorsDecision::NoOrigin => None,
        CorsDecision::Allowed(origin) => Some(origin),
    };

    // Authenticate via Bearer token.
    let Some(partner) = authenticate_bearer(registry, req) else {
        return json_response_with_origin(
            StatusCode::UNAUTHORIZED,
            &serde_json::json!({ "error": "invalid_token" }),
            allowed_origin.as_deref(),
        );
    };

    if !ec_context.ec_allowed() {
        return json_response_with_origin(
            StatusCode::FORBIDDEN,
            &serde_json::json!({ "consent": "denied" }),
            allowed_origin.as_deref(),
        );
    }

    let Some(ec_id) = ec_context.ec_value() else {
        let response = apply_identify_cache_headers(
            Response::builder()
                .status(StatusCode::NO_CONTENT)
                .body(EdgeBody::empty())
                .expect("should build no-content response"),
        );
        return Ok(apply_cors_headers_if_allowed(
            response,
            allowed_origin.as_deref(),
        ));
    };

    let mut degraded = false;
    let mut uid: Option<String> = None;
    let mut cluster_size: Option<u32> = None;

    match kv.get(ec_id) {
        Ok(Some((entry, generation))) => {
            if !entry.consent.ok {
                // Tombstone entries preserve the withdrawal signal for 24 hours.
                // Do not extract IDs or evaluate cluster size because that would
                // write back with the live-entry TTL.
                log::trace!("Identify found tombstone for '{}'", log_id(ec_id));
            } else {
                // Extract only this partner's UID.
                if let Some(partner_uid) = entry.ids.get(&partner.source_domain)
                    && !partner_uid.uid.is_empty()
                {
                    uid = Some(partner_uid.uid.clone());
                }

                // Evaluate cluster size lazily for identify responses. Existing
                // stored cluster_size values are reused without a prefix-list call.
                match kv.evaluate_cluster(ec_id, &entry, generation) {
                    Ok(size) => {
                        cluster_size = size;
                    }
                    Err(err) => {
                        log::warn!("Cluster evaluation failed for '{}': {err:?}", log_id(ec_id));
                    }
                }
            }
        }
        Ok(None) => {}
        Err(err) => {
            log::warn!(
                "Identify KV read failed for EC ID '{}': {err:?}",
                log_id(ec_id)
            );
            degraded = true;
        }
    }

    let eid = uid.as_ref().map(|u| Eid {
        source: partner.source_domain.clone(),
        uids: vec![Uid {
            id: u.clone(),
            atype: Some(partner.openrtb_atype),
            ext: None,
        }],
    });

    let body = IdentifyResponse {
        ec: ec_id.to_owned(),
        consent: "ok".to_owned(),
        degraded,
        source_domain: partner.source_domain.clone(),
        uid,
        eid,
        cluster_size,
    };

    json_response_with_origin(StatusCode::OK, &body, allowed_origin.as_deref())
}

/// Handles `OPTIONS /_ts/api/v1/identify` CORS preflight.
///
/// # Errors
///
/// Returns [`TrustedServerError`] when response construction fails.
///
/// # Panics
///
/// Panics if response builder produces an invalid status or body, which cannot
/// happen with the hardcoded values used here.
pub fn cors_preflight_identify(
    settings: &Settings,
    req: &Request<EdgeBody>,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    let response = match classify_origin(req, settings) {
        CorsDecision::Denied => Response::builder()
            .status(StatusCode::FORBIDDEN)
            .body(EdgeBody::empty())
            .expect("should build forbidden response"),
        CorsDecision::NoOrigin => Response::builder()
            .status(StatusCode::OK)
            .body(EdgeBody::empty())
            .expect("should build ok response"),
        CorsDecision::Allowed(origin) => {
            let mut response = Response::builder()
                .status(StatusCode::OK)
                .body(EdgeBody::empty())
                .expect("should build ok response");
            apply_cors_headers(&mut response, &origin);
            response
        }
    };

    Ok(apply_identify_cache_headers(response))
}

#[derive(serde::Serialize)]
struct IdentifyResponse {
    ec: String,
    consent: String,
    degraded: bool,
    source_domain: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    uid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    eid: Option<Eid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cluster_size: Option<u32>,
}

fn json_response_with_origin<T: serde::Serialize>(
    status: StatusCode,
    body: &T,
    allowed_origin: Option<&str>,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    let body_str = serde_json::to_string(body).change_context(TrustedServerError::EdgeCookie {
        message: "Failed to serialize identify response".to_owned(),
    })?;

    let response = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(EdgeBody::from(body_str))
        .expect("should build identify response");
    let response = apply_identify_cache_headers(response);

    Ok(apply_cors_headers_if_allowed(response, allowed_origin))
}

enum CorsDecision {
    NoOrigin,
    Allowed(String),
    Denied,
}

fn classify_origin(req: &Request<EdgeBody>, settings: &Settings) -> CorsDecision {
    let Some(origin) = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
    else {
        return CorsDecision::NoOrigin;
    };

    let Ok(origin_url) = Url::parse(origin) else {
        return CorsDecision::Denied;
    };

    if origin_url.scheme() != "https" {
        return CorsDecision::Denied;
    }

    let Some(host) = origin_url.host_str() else {
        return CorsDecision::Denied;
    };

    let publisher_host = settings
        .publisher
        .domain
        .trim_end_matches('.')
        .to_ascii_lowercase();

    if origin_authority_contains_uppercase_host(origin) {
        return CorsDecision::Denied;
    }

    let host = host.to_ascii_lowercase();
    if host == publisher_host || host.ends_with(&format!(".{publisher_host}")) {
        return CorsDecision::Allowed(origin.to_owned());
    }

    CorsDecision::Denied
}

fn origin_authority_contains_uppercase_host(origin: &str) -> bool {
    let Some(after_scheme) = origin.strip_prefix("https://") else {
        return false;
    };
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host_port)| host_port);
    let host = host_port
        .split_once(':')
        .map_or(host_port, |(host, _)| host);

    host.bytes().any(|byte| byte.is_ascii_uppercase())
}

fn apply_identify_cache_headers(mut response: Response<EdgeBody>) -> Response<EdgeBody> {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    response.headers_mut().insert(
        header::VARY,
        HeaderValue::from_static("Origin, Authorization"),
    );
    response
}

fn apply_cors_headers_if_allowed(
    mut response: Response<EdgeBody>,
    allowed_origin: Option<&str>,
) -> Response<EdgeBody> {
    if let Some(origin) = allowed_origin {
        apply_cors_headers(&mut response, origin);
    }
    response
}

fn apply_cors_headers(response: &mut Response<EdgeBody>, origin: &str) {
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_str(origin).expect("should be valid origin header value"),
    );
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
        HeaderValue::from_static("true"),
    );
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, OPTIONS"),
    );
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("Authorization"),
    );
    response.headers_mut().insert(
        header::ACCESS_CONTROL_MAX_AGE,
        HeaderValue::from_static("600"),
    );
    response.headers_mut().insert(
        header::VARY,
        HeaderValue::from_static("Origin, Authorization"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consent::types::{ConsentContext, ConsentSource};
    use crate::ec::registry::PartnerRegistry;
    use crate::redacted::Redacted;
    use crate::settings::EcPartner;
    use crate::test_support::tests::create_test_settings;

    const VALID_API_TOKEN: &str = "identify-test-token-32-bytes-min";

    fn assert_no_store(response: &Response<EdgeBody>) {
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some("no-store"),
            "identify responses should not be cached"
        );
    }

    fn make_ec_context(ec_allowed: bool, ec_value: Option<&str>) -> EcContext {
        let consent = ConsentContext {
            source: ConsentSource::Cookie,
            ..ConsentContext::default()
        };
        EcContext::new_for_test_gated(ec_value.map(str::to_owned), consent, ec_allowed)
    }

    fn make_test_partner(source_domain: &str, api_token: &str) -> EcPartner {
        EcPartner {
            name: format!("Partner {source_domain}"),
            source_domain: source_domain.to_owned(),
            openrtb_atype: EcPartner::default_openrtb_atype(),
            bidstream_enabled: true,
            api_token: Redacted::new(api_token.to_owned()),
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
    fn classify_origin_accepts_publisher_subdomain() {
        let settings = create_test_settings();
        let req = Request::builder()
            .method("GET")
            .uri("https://edge.test-publisher.com/identify")
            .header("origin", "https://www.test-publisher.com")
            .body(EdgeBody::empty())
            .expect("should build test request");

        let decision = classify_origin(&req, &settings);
        assert!(
            matches!(decision, CorsDecision::Allowed(_)),
            "should allow publisher subdomain origin"
        );
    }

    #[test]
    fn classify_origin_rejects_mismatch() {
        let settings = create_test_settings();
        let req = Request::builder()
            .method("GET")
            .uri("https://edge.test-publisher.com/identify")
            .header("origin", "https://evil.com")
            .body(EdgeBody::empty())
            .expect("should build test request");

        let decision = classify_origin(&req, &settings);
        assert!(
            matches!(decision, CorsDecision::Denied),
            "should deny mismatched origin"
        );
    }

    #[test]
    fn classify_origin_rejects_mixed_case_publisher_host() {
        let settings = create_test_settings();
        let req = Request::builder()
            .method("GET")
            .uri("https://edge.test-publisher.com/identify")
            .header("origin", "https://Foo.test-publisher.com")
            .body(EdgeBody::empty())
            .expect("should build test request");

        let decision = classify_origin(&req, &settings);
        assert!(
            matches!(decision, CorsDecision::Denied),
            "should deny mixed-case origin hosts instead of reflecting a value browsers reject"
        );
    }

    #[test]
    fn classify_origin_rejects_http_scheme() {
        let settings = create_test_settings();
        let req = Request::builder()
            .method("GET")
            .uri("https://edge.test-publisher.com/identify")
            .header("origin", "http://www.test-publisher.com")
            .body(EdgeBody::empty())
            .expect("should build test request");

        let decision = classify_origin(&req, &settings);
        assert!(
            matches!(decision, CorsDecision::Denied),
            "should deny non-https publisher origin"
        );
    }

    #[test]
    fn classify_origin_allows_absent_origin_header() {
        let settings = create_test_settings();
        let req = Request::builder()
            .method("GET")
            .uri("https://edge.test-publisher.com/identify")
            .body(EdgeBody::empty())
            .expect("should build test request");

        let decision = classify_origin(&req, &settings);
        assert!(
            matches!(decision, CorsDecision::NoOrigin),
            "should allow no-origin requests"
        );
    }

    #[test]
    fn handle_identify_rejects_missing_bearer_token() {
        let settings = create_test_settings();
        let kv = KvIdentityGraph::failing("missing_store");
        let registry = PartnerRegistry::empty();
        let req = Request::builder()
            .method("GET")
            .uri("https://edge.test-publisher.com/identify")
            .body(EdgeBody::empty())
            .expect("should build test request");
        let ec_context = make_ec_context(true, None);

        let response = handle_identify(&settings, &kv, &registry, &req, &ec_context)
            .expect("should construct unauthorized response");

        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .and_then(|v| v.to_str().ok()),
            None,
            "should omit CORS headers when Origin is absent"
        );

        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "should return 401 without Bearer token"
        );
        assert_no_store(&response);
        let body = serde_json::from_slice::<serde_json::Value>(
            &response.into_body().into_bytes().unwrap_or_default(),
        )
        .expect("should decode JSON body");
        assert_eq!(
            body["error"], "invalid_token",
            "should return invalid_token error"
        );
    }

    #[test]
    fn handle_identify_rejects_invalid_bearer_token() {
        let settings = create_test_settings();
        let kv = KvIdentityGraph::failing("missing_store");
        let partners = vec![make_test_partner("ssp.example.com", VALID_API_TOKEN)];
        let registry = PartnerRegistry::from_config(&partners).expect("should build registry");
        let req = Request::builder()
            .method("GET")
            .uri("https://edge.test-publisher.com/identify")
            .header("authorization", "Bearer wrong-token")
            .body(EdgeBody::empty())
            .expect("should build test request");
        let ec_context = make_ec_context(true, None);

        let response = handle_identify(&settings, &kv, &registry, &req, &ec_context)
            .expect("should construct unauthorized response");

        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "should return 401 for invalid Bearer token"
        );
        assert_no_store(&response);
    }

    #[test]
    fn handle_identify_denied_consent_returns_403() {
        let settings = create_test_settings();
        let kv = KvIdentityGraph::failing("missing_store");
        let partners = vec![make_test_partner("ssp.example.com", VALID_API_TOKEN)];
        let registry = PartnerRegistry::from_config(&partners).expect("should build registry");
        let req = Request::builder()
            .method("GET")
            .uri("https://edge.test-publisher.com/identify")
            .header("authorization", format!("Bearer {VALID_API_TOKEN}"))
            .body(EdgeBody::empty())
            .expect("should build test request");
        let ec_context = make_ec_context(false, None);

        let response = handle_identify(&settings, &kv, &registry, &req, &ec_context)
            .expect("should construct denied response");

        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "should return 403 when consent denies EC"
        );
        assert_no_store(&response);
        let body = serde_json::from_slice::<serde_json::Value>(
            &response.into_body().into_bytes().unwrap_or_default(),
        )
        .expect("should decode JSON body");
        assert_eq!(
            body,
            serde_json::json!({ "consent": "denied" }),
            "should return denied consent payload"
        );
    }

    #[test]
    fn handle_identify_without_ec_returns_204() {
        let settings = create_test_settings();
        let kv = KvIdentityGraph::failing("missing_store");
        let partners = vec![make_test_partner("ssp.example.com", VALID_API_TOKEN)];
        let registry = PartnerRegistry::from_config(&partners).expect("should build registry");
        let req = Request::builder()
            .method("GET")
            .uri("https://edge.test-publisher.com/identify")
            .header("authorization", format!("Bearer {VALID_API_TOKEN}"))
            .body(EdgeBody::empty())
            .expect("should build test request");
        let ec_context = make_ec_context(true, None);

        let response = handle_identify(&settings, &kv, &registry, &req, &ec_context)
            .expect("should construct no-content response");

        assert_eq!(
            response.status(),
            StatusCode::NO_CONTENT,
            "should return 204 when EC is unavailable"
        );
        assert_no_store(&response);
    }

    #[test]
    fn handle_identify_kv_failure_sets_degraded_true() {
        let settings = create_test_settings();
        let kv = KvIdentityGraph::failing("missing_store");
        let partners = vec![make_test_partner("ssp.example.com", VALID_API_TOKEN)];
        let registry = PartnerRegistry::from_config(&partners).expect("should build registry");
        let req = Request::builder()
            .method("GET")
            .uri("https://edge.test-publisher.com/identify")
            .header("authorization", format!("Bearer {VALID_API_TOKEN}"))
            .body(EdgeBody::empty())
            .expect("should build test request");
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let ec_context = make_ec_context(true, Some(&ec_id));

        let response = handle_identify(&settings, &kv, &registry, &req, &ec_context)
            .expect("should construct degraded identify response");

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "should return 200 on degraded KV read"
        );
        assert_no_store(&response);
        assert!(
            response.headers().get("x-ts-ec").is_none(),
            "should not emit x-ts-ec header"
        );
        let body = serde_json::from_slice::<serde_json::Value>(
            &response.into_body().into_bytes().unwrap_or_default(),
        )
        .expect("should decode identify response JSON");

        assert_eq!(body["ec"], ec_id, "should echo EC in body");
        assert_eq!(
            body["source_domain"], "ssp.example.com",
            "should echo source domain"
        );
        assert_eq!(
            body["degraded"],
            serde_json::Value::Bool(true),
            "should mark response as degraded when KV read fails"
        );
        assert!(
            body.get("uid").is_none(),
            "uid should be omitted when KV read fails"
        );
        assert!(
            body.get("eid").is_none(),
            "eid should be omitted when KV read fails"
        );
    }

    #[test]
    fn handle_identify_denies_mismatched_browser_origin() {
        let settings = create_test_settings();
        let kv = KvIdentityGraph::failing("missing_store");
        let partners = vec![make_test_partner("ssp.example.com", VALID_API_TOKEN)];
        let registry = PartnerRegistry::from_config(&partners).expect("should build registry");
        let req = Request::builder()
            .method("GET")
            .uri("https://edge.test-publisher.com/identify")
            .header("authorization", format!("Bearer {VALID_API_TOKEN}"))
            .header("origin", "https://evil.example")
            .body(EdgeBody::empty())
            .expect("should build test request");
        let ec_context = make_ec_context(true, None);

        let response = handle_identify(&settings, &kv, &registry, &req, &ec_context)
            .expect("should construct forbidden response");

        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "should reject GET from non-publisher origin"
        );
        assert_no_store(&response);
    }

    #[test]
    fn handle_identify_allows_browser_origin_and_reflects_cors_headers() {
        let settings = create_test_settings();
        let kv = KvIdentityGraph::failing("missing_store");
        let partners = vec![make_test_partner("ssp.example.com", VALID_API_TOKEN)];
        let registry = PartnerRegistry::from_config(&partners).expect("should build registry");
        let req = Request::builder()
            .method("GET")
            .uri("https://edge.test-publisher.com/identify")
            .header("authorization", format!("Bearer {VALID_API_TOKEN}"))
            .header("origin", "https://www.test-publisher.com")
            .body(EdgeBody::empty())
            .expect("should build test request");
        let ec_context = make_ec_context(true, None);

        let response = handle_identify(&settings, &kv, &registry, &req, &ec_context)
            .expect("should construct no-content response with CORS headers");

        assert_eq!(
            response.status(),
            StatusCode::NO_CONTENT,
            "should preserve identify response status for allowed browser origin"
        );
        assert_no_store(&response);
        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .and_then(|v| v.to_str().ok()),
            Some("https://www.test-publisher.com"),
            "should reflect allowed browser origin on GET responses"
        );
        assert_eq!(
            response
                .headers()
                .get(header::VARY)
                .and_then(|v| v.to_str().ok()),
            Some("Origin, Authorization"),
            "should vary on identity request inputs for browser-direct identify responses"
        );
    }

    #[test]
    fn identify_preflight_denies_mismatched_origin() {
        let settings = create_test_settings();
        let req = Request::builder()
            .method("OPTIONS")
            .uri("https://edge.test-publisher.com/identify")
            .header("origin", "https://evil.example")
            .body(EdgeBody::empty())
            .expect("should build test request");

        let response =
            cors_preflight_identify(&settings, &req).expect("should construct preflight response");

        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "should reject preflight from non-publisher origin"
        );
        assert_no_store(&response);
    }

    #[test]
    fn identify_preflight_allows_publisher_origin() {
        let settings = create_test_settings();
        let req = Request::builder()
            .method("OPTIONS")
            .uri("https://edge.test-publisher.com/identify")
            .header("origin", "https://www.test-publisher.com")
            .body(EdgeBody::empty())
            .expect("should build test request");

        let response =
            cors_preflight_identify(&settings, &req).expect("should construct preflight response");

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "should allow preflight from publisher origin"
        );
        assert_no_store(&response);
        assert_eq!(
            response
                .headers()
                .get(header::VARY)
                .and_then(|v| v.to_str().ok()),
            Some("Origin, Authorization"),
            "should vary on identity request inputs for preflight"
        );
    }
}
