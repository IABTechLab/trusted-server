//! Identity lookup endpoint (`GET /_ts/api/v1/identify`).
//!
//! Partners authenticate with a Bearer token and receive only their own
//! synced UID for the active EC ID.

use error_stack::{Report, ResultExt};
use fastly::http::{header, StatusCode};
use fastly::{Request, Response};
use url::Url;

use super::auth::authenticate_bearer;
use super::consent::ec_consent_granted;
use crate::constants::HEADER_X_TS_EC;
use crate::error::TrustedServerError;
use crate::openrtb::{Eid, Uid};
use crate::settings::Settings;

use super::kv::KvIdentityGraph;
use super::log_id;
use super::registry::PartnerRegistry;
use super::EcContext;

/// Handles `GET /_ts/api/v1/identify`.
///
/// Requires Bearer token authentication. Returns only the requesting
/// partner's UID for the active EC ID.
///
/// # Errors
///
/// Returns [`TrustedServerError`] for response serialization issues.
pub fn handle_identify(
    settings: &Settings,
    kv: &KvIdentityGraph,
    registry: &PartnerRegistry,
    req: &Request,
    ec_context: &EcContext,
) -> Result<Response, Report<TrustedServerError>> {
    let allowed_origin = match classify_origin(req, settings) {
        CorsDecision::Denied => return Ok(Response::from_status(StatusCode::FORBIDDEN)),
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

    if !ec_consent_granted(ec_context.consent()) {
        return json_response_with_origin(
            StatusCode::FORBIDDEN,
            &serde_json::json!({ "consent": "denied" }),
            allowed_origin.as_deref(),
        );
    }

    let Some(ec_id) = ec_context.ec_value() else {
        let response = Response::from_status(StatusCode::NO_CONTENT);
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
            // Extract only this partner's UID.
            if let Some(partner_uid) = entry.ids.get(&partner.id) {
                if !partner_uid.uid.is_empty() {
                    uid = Some(partner_uid.uid.clone());
                }
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
        partner_id: partner.id.clone(),
        uid,
        eid,
        cluster_size,
    };

    let mut response = json_response_with_origin(StatusCode::OK, &body, allowed_origin.as_deref())?;
    response.set_header(HEADER_X_TS_EC, ec_id);

    Ok(response)
}

/// Handles `OPTIONS /_ts/api/v1/identify` CORS preflight.
///
/// # Errors
///
/// Returns [`TrustedServerError`] when response construction fails.
pub fn cors_preflight_identify(
    settings: &Settings,
    req: &Request,
) -> Result<Response, Report<TrustedServerError>> {
    let mut response = match classify_origin(req, settings) {
        CorsDecision::Denied => Response::from_status(StatusCode::FORBIDDEN),
        CorsDecision::NoOrigin => Response::from_status(StatusCode::OK),
        CorsDecision::Allowed(origin) => {
            let mut response = Response::from_status(StatusCode::OK);
            apply_cors_headers(&mut response, &origin);
            response
        }
    };

    response.set_body(Vec::new());
    Ok(response)
}

#[derive(serde::Serialize)]
struct IdentifyResponse {
    ec: String,
    consent: String,
    degraded: bool,
    partner_id: String,
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
) -> Result<Response, Report<TrustedServerError>> {
    let body = serde_json::to_string(body).change_context(TrustedServerError::EdgeCookie {
        message: "Failed to serialize identify response".to_owned(),
    })?;

    let response = Response::from_status(status)
        .with_content_type(fastly::mime::APPLICATION_JSON)
        .with_body(body);

    Ok(apply_cors_headers_if_allowed(response, allowed_origin))
}

enum CorsDecision {
    NoOrigin,
    Allowed(String),
    Denied,
}

fn classify_origin(req: &Request, settings: &Settings) -> CorsDecision {
    let Some(origin) = req.get_header(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
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

    let host = host.to_ascii_lowercase();
    let publisher_host = settings
        .publisher
        .domain
        .trim_end_matches('.')
        .to_ascii_lowercase();

    if host == publisher_host || host.ends_with(&format!(".{publisher_host}")) {
        return CorsDecision::Allowed(origin.to_owned());
    }

    CorsDecision::Denied
}

fn apply_cors_headers_if_allowed(mut response: Response, allowed_origin: Option<&str>) -> Response {
    if let Some(origin) = allowed_origin {
        apply_cors_headers(&mut response, origin);
    }
    response
}

fn apply_cors_headers(response: &mut Response, origin: &str) {
    response.set_header(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
    response.set_header(header::ACCESS_CONTROL_ALLOW_CREDENTIALS, "true");
    response.set_header(header::ACCESS_CONTROL_ALLOW_METHODS, "GET, OPTIONS");
    response.set_header(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        "Authorization, X-ts-ec",
    );
    response.set_header(header::ACCESS_CONTROL_MAX_AGE, "600");
    response.set_header(header::VARY, "Origin");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consent::jurisdiction::Jurisdiction;
    use crate::consent::types::{ConsentContext, ConsentSource};
    use crate::ec::registry::PartnerRegistry;
    use crate::redacted::Redacted;
    use crate::settings::EcPartner;
    use crate::test_support::tests::create_test_settings;

    fn make_ec_context(jurisdiction: Jurisdiction, ec_value: Option<&str>) -> EcContext {
        let consent = ConsentContext {
            jurisdiction,
            source: ConsentSource::Cookie,
            ..ConsentContext::default()
        };
        EcContext::new_for_test(ec_value.map(str::to_owned), consent)
    }

    fn make_test_partner(id: &str, api_token: &str) -> EcPartner {
        EcPartner {
            id: id.to_owned(),
            name: format!("Partner {id}"),
            source_domain: format!("{id}.example.com"),
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
        let mut req = Request::new("GET", "https://edge.test-publisher.com/identify");
        req.set_header("origin", "https://www.test-publisher.com");

        let decision = classify_origin(&req, &settings);
        assert!(
            matches!(decision, CorsDecision::Allowed(_)),
            "should allow publisher subdomain origin"
        );
    }

    #[test]
    fn classify_origin_rejects_mismatch() {
        let settings = create_test_settings();
        let mut req = Request::new("GET", "https://edge.test-publisher.com/identify");
        req.set_header("origin", "https://evil.com");

        let decision = classify_origin(&req, &settings);
        assert!(
            matches!(decision, CorsDecision::Denied),
            "should deny mismatched origin"
        );
    }

    #[test]
    fn classify_origin_rejects_http_scheme() {
        let settings = create_test_settings();
        let mut req = Request::new("GET", "https://edge.test-publisher.com/identify");
        req.set_header("origin", "http://www.test-publisher.com");

        let decision = classify_origin(&req, &settings);
        assert!(
            matches!(decision, CorsDecision::Denied),
            "should deny non-https publisher origin"
        );
    }

    #[test]
    fn classify_origin_allows_absent_origin_header() {
        let settings = create_test_settings();
        let req = Request::new("GET", "https://edge.test-publisher.com/identify");

        let decision = classify_origin(&req, &settings);
        assert!(
            matches!(decision, CorsDecision::NoOrigin),
            "should allow no-origin requests"
        );
    }

    #[test]
    fn handle_identify_rejects_missing_bearer_token() {
        let settings = create_test_settings();
        let kv = KvIdentityGraph::new("missing_store");
        let registry = PartnerRegistry::empty();
        let req = Request::new("GET", "https://edge.test-publisher.com/identify");
        let ec_context = make_ec_context(Jurisdiction::NonRegulated, None);

        let mut response = handle_identify(&settings, &kv, &registry, &req, &ec_context)
            .expect("should construct unauthorized response");

        assert_eq!(
            response.get_header_str(header::ACCESS_CONTROL_ALLOW_ORIGIN),
            None,
            "should omit CORS headers when Origin is absent"
        );

        assert_eq!(
            response.get_status(),
            StatusCode::UNAUTHORIZED,
            "should return 401 without Bearer token"
        );
        let body = serde_json::from_slice::<serde_json::Value>(&response.take_body_bytes())
            .expect("should decode JSON body");
        assert_eq!(
            body["error"], "invalid_token",
            "should return invalid_token error"
        );
    }

    #[test]
    fn handle_identify_rejects_invalid_bearer_token() {
        let settings = create_test_settings();
        let kv = KvIdentityGraph::new("missing_store");
        let partners = vec![make_test_partner("ssp_x", "real-token")];
        let registry = PartnerRegistry::from_config(&partners).expect("should build registry");
        let mut req = Request::new("GET", "https://edge.test-publisher.com/identify");
        req.set_header("authorization", "Bearer wrong-token");
        let ec_context = make_ec_context(Jurisdiction::NonRegulated, None);

        let response = handle_identify(&settings, &kv, &registry, &req, &ec_context)
            .expect("should construct unauthorized response");

        assert_eq!(
            response.get_status(),
            StatusCode::UNAUTHORIZED,
            "should return 401 for invalid Bearer token"
        );
    }

    #[test]
    fn handle_identify_denied_consent_returns_403() {
        let settings = create_test_settings();
        let kv = KvIdentityGraph::new("missing_store");
        let partners = vec![make_test_partner("ssp_x", "my-token")];
        let registry = PartnerRegistry::from_config(&partners).expect("should build registry");
        let mut req = Request::new("GET", "https://edge.test-publisher.com/identify");
        req.set_header("authorization", "Bearer my-token");
        let ec_context = make_ec_context(Jurisdiction::Unknown, None);

        let mut response = handle_identify(&settings, &kv, &registry, &req, &ec_context)
            .expect("should construct denied response");

        assert_eq!(
            response.get_status(),
            StatusCode::FORBIDDEN,
            "should return 403 when consent denies EC"
        );
        let body = serde_json::from_slice::<serde_json::Value>(&response.take_body_bytes())
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
        let kv = KvIdentityGraph::new("missing_store");
        let partners = vec![make_test_partner("ssp_x", "my-token")];
        let registry = PartnerRegistry::from_config(&partners).expect("should build registry");
        let mut req = Request::new("GET", "https://edge.test-publisher.com/identify");
        req.set_header("authorization", "Bearer my-token");
        let ec_context = make_ec_context(Jurisdiction::NonRegulated, None);

        let response = handle_identify(&settings, &kv, &registry, &req, &ec_context)
            .expect("should construct no-content response");

        assert_eq!(
            response.get_status(),
            StatusCode::NO_CONTENT,
            "should return 204 when EC is unavailable"
        );
    }

    #[test]
    fn handle_identify_kv_failure_sets_degraded_true() {
        let settings = create_test_settings();
        let kv = KvIdentityGraph::new("missing_store");
        let partners = vec![make_test_partner("ssp_x", "my-token")];
        let registry = PartnerRegistry::from_config(&partners).expect("should build registry");
        let mut req = Request::new("GET", "https://edge.test-publisher.com/identify");
        req.set_header("authorization", "Bearer my-token");
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let ec_context = make_ec_context(Jurisdiction::NonRegulated, Some(&ec_id));

        let mut response = handle_identify(&settings, &kv, &registry, &req, &ec_context)
            .expect("should construct degraded identify response");

        assert_eq!(
            response.get_status(),
            StatusCode::OK,
            "should return 200 on degraded KV read"
        );
        let body = serde_json::from_slice::<serde_json::Value>(&response.take_body_bytes())
            .expect("should decode identify response JSON");

        assert_eq!(body["ec"], ec_id, "should echo EC in body");
        assert_eq!(body["partner_id"], "ssp_x", "should echo partner ID");
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
        let kv = KvIdentityGraph::new("missing_store");
        let partners = vec![make_test_partner("ssp_x", "my-token")];
        let registry = PartnerRegistry::from_config(&partners).expect("should build registry");
        let mut req = Request::new("GET", "https://edge.test-publisher.com/identify");
        req.set_header("authorization", "Bearer my-token");
        req.set_header("origin", "https://evil.example");
        let ec_context = make_ec_context(Jurisdiction::NonRegulated, None);

        let response = handle_identify(&settings, &kv, &registry, &req, &ec_context)
            .expect("should construct forbidden response");

        assert_eq!(
            response.get_status(),
            StatusCode::FORBIDDEN,
            "should reject GET from non-publisher origin"
        );
    }

    #[test]
    fn handle_identify_allows_browser_origin_and_reflects_cors_headers() {
        let settings = create_test_settings();
        let kv = KvIdentityGraph::new("missing_store");
        let partners = vec![make_test_partner("ssp_x", "my-token")];
        let registry = PartnerRegistry::from_config(&partners).expect("should build registry");
        let mut req = Request::new("GET", "https://edge.test-publisher.com/identify");
        req.set_header("authorization", "Bearer my-token");
        req.set_header("origin", "https://www.test-publisher.com");
        let ec_context = make_ec_context(Jurisdiction::NonRegulated, None);

        let response = handle_identify(&settings, &kv, &registry, &req, &ec_context)
            .expect("should construct no-content response with CORS headers");

        assert_eq!(
            response.get_status(),
            StatusCode::NO_CONTENT,
            "should preserve identify response status for allowed browser origin"
        );
        assert_eq!(
            response.get_header_str(header::ACCESS_CONTROL_ALLOW_ORIGIN),
            Some("https://www.test-publisher.com"),
            "should reflect allowed browser origin on GET responses"
        );
        assert_eq!(
            response.get_header_str(header::VARY),
            Some("Origin"),
            "should vary on Origin for browser-direct identify responses"
        );
    }

    #[test]
    fn identify_preflight_denies_mismatched_origin() {
        let settings = create_test_settings();
        let mut req = Request::new("OPTIONS", "https://edge.test-publisher.com/identify");
        req.set_header("origin", "https://evil.example");

        let response =
            cors_preflight_identify(&settings, &req).expect("should construct preflight response");

        assert_eq!(
            response.get_status(),
            StatusCode::FORBIDDEN,
            "should reject preflight from non-publisher origin"
        );
    }

    #[test]
    fn identify_preflight_allows_publisher_origin() {
        let settings = create_test_settings();
        let mut req = Request::new("OPTIONS", "https://edge.test-publisher.com/identify");
        req.set_header("origin", "https://www.test-publisher.com");

        let response =
            cors_preflight_identify(&settings, &req).expect("should construct preflight response");

        assert_eq!(
            response.get_status(),
            StatusCode::OK,
            "should allow preflight from publisher origin"
        );
    }
}
