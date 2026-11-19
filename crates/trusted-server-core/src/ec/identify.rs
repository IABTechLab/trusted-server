//! Identity lookup endpoint (`GET /_ts/api/v1/identify`).
//!
//! Partners authenticate with a Bearer token and receive only their own
//! synced UID for the active EC ID.

use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use http::header::{self, HeaderValue};
use http::{Method, Request, Response, StatusCode};
use url::Url;

use super::auth::authenticate_bearer;
use super::consent::ec_consent_granted;
use crate::error::TrustedServerError;
use crate::openrtb::Eid;
use crate::settings::Settings;

use super::eid_types::UnixSeconds;
use super::eids::source_views;
use super::kv::KvIdentityGraph;
use super::log_id;
use super::registry::PartnerRegistry;
use super::{EcContext, current_timestamp};

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
pub async fn handle_identify(
    settings: &Settings,
    kv: &KvIdentityGraph,
    registry: &PartnerRegistry,
    req: &Request<EdgeBody>,
    ec_context: &EcContext,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    let allowed_origin = match classify_origin(req, settings) {
        CorsDecision::Denied => return Ok(identify_origin_denied()),
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
    let mut eid: Option<Eid> = None;
    let mut cluster_size: Option<u32> = None;

    match kv.get(ec_id).await {
        Ok(Some((entry, generation))) => {
            if !entry.consent.ok {
                // Tombstone entries preserve the withdrawal signal for 24 hours.
                // Do not extract IDs or evaluate cluster size because that would
                // write back with the live-entry TTL.
                log::trace!("Identify found tombstone for '{}'", log_id(ec_id));
            } else {
                // Extract only this partner's first live entry. A
                // provider-owned source is withheld while its owner lacks TCF
                // vendor consent, and a source a provider excludes where GDPR
                // applies is withheld for a GDPR visitor, as in the bidstream.
                let now = UnixSeconds::new(current_timestamp());
                let vendor_denied = partner.owner_vendor_consent_denied(ec_context.consent())
                    || partner.excluded_for(ec_context.consent());
                eid = entry
                    .ids
                    .get(&partner.source_domain)
                    .filter(|_| !vendor_denied)
                    .and_then(|record| {
                        source_views(&partner.source_domain, record, partner.openrtb_atype, now)
                            .next()
                    })
                    .map(Eid::from);

                // Evaluate cluster size lazily for identify responses. Existing
                // stored cluster_size values are reused without a prefix-list call.
                match kv.evaluate_cluster(ec_id, &entry, generation).await {
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

    let uid = eid
        .as_ref()
        .and_then(|eid| eid.uids.first())
        .map(|uid| uid.id.clone());

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

/// Builds the local `501 Not Implemented` response for `GET /_ts/api/v1/identify`
/// on a deployment without an EC record store.
///
/// Adapters return this instead of proxying the request to the publisher
/// origin, so a partner's Bearer token never leaves Trusted Server. The same
/// origin policy as [`handle_identify`] applies first: a denied browser origin
/// receives `403`, and an allowed one receives reflected CORS headers so it can
/// read the `501`.
///
/// # Panics
///
/// Panics if the response builder rejects the hardcoded status or headers,
/// which cannot happen.
#[must_use]
pub fn identify_not_supported(settings: &Settings, req: &Request<EdgeBody>) -> Response<EdgeBody> {
    let allowed_origin = match classify_origin(req, settings) {
        CorsDecision::Denied => return identify_origin_denied(),
        CorsDecision::NoOrigin => None,
        CorsDecision::Allowed(origin) => Some(origin),
    };
    let body = serde_json::json!({ "error": "ec_store_not_configured" });
    let response = apply_identify_cache_headers(
        Response::builder()
            .status(StatusCode::NOT_IMPLEMENTED)
            .header(header::CONTENT_TYPE, "application/json")
            .body(EdgeBody::from(body.to_string()))
            .expect("should build identify not-supported response"),
    );
    apply_cors_headers_if_allowed(response, allowed_origin.as_deref())
}

/// Builds the local `405 Method Not Allowed` response for a method other than
/// `GET` or `OPTIONS` on `/_ts/api/v1/identify`.
///
/// Adapters claim every method on the identify path and answer the others
/// with this response, so no request on the path reaches the publisher origin.
///
/// # Panics
///
/// Panics if the response builder rejects the hardcoded status or headers,
/// which cannot happen.
#[must_use]
pub fn identify_method_not_allowed() -> Response<EdgeBody> {
    let body = serde_json::json!({ "error": "method_not_allowed" });
    apply_identify_cache_headers(
        Response::builder()
            .status(StatusCode::METHOD_NOT_ALLOWED)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ALLOW, "GET, OPTIONS")
            .body(EdgeBody::from(body.to_string()))
            .expect("should build identify method-not-allowed response"),
    )
}

/// Answers an identify request locally, without the EC record store.
///
/// `OPTIONS` runs [`cors_preflight_identify`], `GET` returns
/// [`identify_not_supported`], and every other method returns
/// [`identify_method_not_allowed`].
///
/// # Errors
///
/// Returns [`TrustedServerError`] when preflight response construction fails.
pub fn answer_identify_locally(
    settings: &Settings,
    req: &Request<EdgeBody>,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    match *req.method() {
        Method::OPTIONS => cors_preflight_identify(settings, req),
        Method::GET => Ok(identify_not_supported(settings, req)),
        _ => Ok(identify_method_not_allowed()),
    }
}

fn identify_origin_denied() -> Response<EdgeBody> {
    apply_identify_cache_headers(
        Response::builder()
            .status(StatusCode::FORBIDDEN)
            .body(EdgeBody::empty())
            .expect("should build forbidden response"),
    )
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
    use crate::consent::jurisdiction::Jurisdiction;
    use crate::consent::types::{ConsentContext, ConsentSource};
    use crate::ec::eid_types::{AgentType, EidFields, Expiry, ProviderId, UidValue, UnixSeconds};
    use crate::ec::kv_types::{KvEid, KvEntry, KvPartnerId, KvUid};
    use crate::ec::registry::PartnerRegistry;
    use crate::redacted::Redacted;
    use crate::settings::EcPartner;
    use crate::test_support::tests::{consent_granting_vendors, create_test_settings};
    use std::num::NonZeroU16;

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

    fn local_identify_request(method: &str, origin: Option<&str>) -> Request<EdgeBody> {
        let mut builder = Request::builder()
            .method(method)
            .uri("https://edge.publisher.example.com/_ts/api/v1/identify")
            .header("authorization", format!("Bearer {VALID_API_TOKEN}"));
        if let Some(origin) = origin {
            builder = builder.header("origin", origin);
        }
        builder
            .body(EdgeBody::empty())
            .expect("should build local identify request")
    }

    fn local_identify_settings() -> Settings {
        let mut settings = create_test_settings();
        settings.publisher.domain = "publisher.example.com".to_owned();
        settings
    }

    #[test]
    fn identify_not_supported_reflects_allowed_origin_on_501() {
        let settings = local_identify_settings();
        let req = local_identify_request("GET", Some("https://www.publisher.example.com"));

        let response = identify_not_supported(&settings, &req);

        assert_eq!(
            response.status(),
            StatusCode::NOT_IMPLEMENTED,
            "should answer 501 to an allowed browser origin"
        );
        assert_no_store(&response);
        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .and_then(|v| v.to_str().ok()),
            Some("https://www.publisher.example.com"),
            "should reflect the allowed origin so the browser can read the 501"
        );
    }

    #[test]
    fn identify_not_supported_denies_mismatched_origin() {
        let settings = local_identify_settings();
        let req = local_identify_request("GET", Some("https://evil.example"));

        let response = identify_not_supported(&settings, &req);

        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "should apply the identify origin policy before the 501"
        );
        assert_no_store(&response);
        assert!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .is_none(),
            "should not reflect a denied origin"
        );
    }

    #[test]
    fn answer_identify_locally_dispatches_by_method() {
        let settings = local_identify_settings();

        let get = answer_identify_locally(&settings, &local_identify_request("GET", None))
            .expect("should answer GET locally");
        let options = answer_identify_locally(&settings, &local_identify_request("OPTIONS", None))
            .expect("should answer OPTIONS locally");

        assert_eq!(get.status(), StatusCode::NOT_IMPLEMENTED, "GET should 501");
        assert_eq!(options.status(), StatusCode::OK, "OPTIONS should preflight");
        for method in ["HEAD", "POST", "PUT", "PATCH", "DELETE"] {
            let response =
                answer_identify_locally(&settings, &local_identify_request(method, None))
                    .expect("should answer unsupported method locally");
            assert_eq!(
                response.status(),
                StatusCode::METHOD_NOT_ALLOWED,
                "{method} identify should answer 405"
            );
            assert_eq!(
                response
                    .headers()
                    .get(header::ALLOW)
                    .and_then(|v| v.to_str().ok()),
                Some("GET, OPTIONS"),
                "{method} identify should advertise the allowed methods"
            );
            assert_no_store(&response);
        }
    }

    #[test]
    fn identify_not_supported_returns_uncached_501() {
        let settings = local_identify_settings();
        let response = identify_not_supported(&settings, &local_identify_request("GET", None));

        assert_eq!(
            response.status(),
            StatusCode::NOT_IMPLEMENTED,
            "should answer 501 without a record store"
        );
        assert_no_store(&response);
        let body: serde_json::Value = serde_json::from_slice(
            &response
                .into_body()
                .into_bytes()
                .expect("should buffer body"),
        )
        .expect("should return a JSON body");
        assert_eq!(
            body,
            serde_json::json!({ "error": "ec_store_not_configured" }),
            "should name the missing record store"
        );
    }

    fn make_ec_context(jurisdiction: Jurisdiction, ec_value: Option<&str>) -> EcContext {
        let consent = ConsentContext {
            jurisdiction,
            source: ConsentSource::Cookie,
            ..ConsentContext::default()
        };
        EcContext::new_for_test(ec_value.map(str::to_owned), consent)
    }

    fn make_test_partner(source_domain: &str, api_token: &str) -> EcPartner {
        EcPartner {
            name: format!("Partner {source_domain}"),
            source_domain: source_domain.to_owned(),
            openrtb_atype: EcPartner::default_openrtb_atype(),
            bidstream_enabled: true,
            mirror_enabled: false,
            api_token: Some(Redacted::new(api_token.to_owned())),
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

    #[tokio::test(flavor = "current_thread")]
    async fn handle_identify_rejects_missing_bearer_token() {
        let settings = create_test_settings();
        let kv = KvIdentityGraph::failing("missing_store");
        let registry = PartnerRegistry::empty();
        let req = Request::builder()
            .method("GET")
            .uri("https://edge.test-publisher.com/identify")
            .body(EdgeBody::empty())
            .expect("should build test request");
        let ec_context = make_ec_context(Jurisdiction::NonRegulated, None);

        let response = handle_identify(&settings, &kv, &registry, &req, &ec_context)
            .await
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

    #[tokio::test(flavor = "current_thread")]
    async fn handle_identify_rejects_invalid_bearer_token() {
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
        let ec_context = make_ec_context(Jurisdiction::NonRegulated, None);

        let response = handle_identify(&settings, &kv, &registry, &req, &ec_context)
            .await
            .expect("should construct unauthorized response");

        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "should return 401 for invalid Bearer token"
        );
        assert_no_store(&response);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn handle_identify_denied_consent_returns_403() {
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
        let ec_context = make_ec_context(Jurisdiction::Unknown, None);

        let response = handle_identify(&settings, &kv, &registry, &req, &ec_context)
            .await
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

    #[tokio::test(flavor = "current_thread")]
    async fn handle_identify_without_ec_returns_204() {
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
        let ec_context = make_ec_context(Jurisdiction::NonRegulated, None);

        let response = handle_identify(&settings, &kv, &registry, &req, &ec_context)
            .await
            .expect("should construct no-content response");

        assert_eq!(
            response.status(),
            StatusCode::NO_CONTENT,
            "should return 204 when EC is unavailable"
        );
        assert_no_store(&response);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn handle_identify_kv_failure_sets_degraded_true() {
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
        let ec_context = make_ec_context(Jurisdiction::NonRegulated, Some(&ec_id));

        let response = handle_identify(&settings, &kv, &registry, &req, &ec_context)
            .await
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

    #[tokio::test(flavor = "current_thread")]
    async fn handle_identify_denies_mismatched_browser_origin() {
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
        let ec_context = make_ec_context(Jurisdiction::NonRegulated, None);

        let response = handle_identify(&settings, &kv, &registry, &req, &ec_context)
            .await
            .expect("should construct forbidden response");

        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "should reject GET from non-publisher origin"
        );
        assert_no_store(&response);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn handle_identify_allows_browser_origin_and_reflects_cors_headers() {
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
        let ec_context = make_ec_context(Jurisdiction::NonRegulated, None);

        let response = handle_identify(&settings, &kv, &registry, &req, &ec_context)
            .await
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

    fn provider_record(id: &str, expiry: Expiry) -> KvPartnerId {
        let uid = KvUid::new(
            UidValue::try_from(id).expect("should build uid"),
            AgentType::try_from(1).expect("should build atype"),
            None,
            Some(ProviderId::new("example-provider")),
            expiry,
        );
        let fields = EidFields {
            inserter: Some("inserter.example.com".into()),
            ..EidFields::default()
        };
        KvPartnerId::from_eids(vec![KvEid::new(fields, vec![uid])], UnixSeconds::new(0))
            .expect("should build record")
    }

    async fn identify_body_for_record(record: KvPartnerId) -> serde_json::Value {
        let registry =
            PartnerRegistry::from_config(&[make_test_partner("ssp.example.com", VALID_API_TOKEN)])
                .expect("should build registry");
        let consent = ConsentContext {
            jurisdiction: Jurisdiction::NonRegulated,
            source: ConsentSource::Cookie,
            ..ConsentContext::default()
        };
        identify_body_with(record, &registry, consent).await
    }

    async fn identify_body_with(
        record: KvPartnerId,
        registry: &PartnerRegistry,
        consent: ConsentContext,
    ) -> serde_json::Value {
        let settings = create_test_settings();
        let kv = KvIdentityGraph::in_memory("identify_store");
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let mut entry = KvEntry::minimal("other.example.com", "other-uid", 1);
        entry.ids.insert("ssp.example.com".to_owned(), record);
        kv.create(&ec_id, &entry)
            .await
            .expect("should seed KV entry");
        let req = Request::builder()
            .method("GET")
            .uri("https://edge.example.com/identify")
            .header("authorization", format!("Bearer {VALID_API_TOKEN}"))
            .body(EdgeBody::empty())
            .expect("should build test request");
        let ec_context = EcContext::new_for_test(Some(ec_id), consent);

        let response = handle_identify(&settings, &kv, registry, &req, &ec_context)
            .await
            .expect("should construct identify response");

        assert_eq!(response.status(), StatusCode::OK, "should return 200");
        serde_json::from_slice::<serde_json::Value>(
            &response.into_body().into_bytes().unwrap_or_default(),
        )
        .expect("should decode identify response JSON")
    }

    #[tokio::test(flavor = "current_thread")]
    async fn handle_identify_withholds_a_source_whose_owner_lacks_vendor_consent() {
        let mut registry =
            PartnerRegistry::from_config(&[make_test_partner("ssp.example.com", VALID_API_TOKEN)])
                .expect("should build registry");
        assert!(
            registry.mark_provider_owned("ssp.example.com", NonZeroU16::new(9999)),
            "should mark the source provider-owned"
        );
        let record = || provider_record("live", Expiry::Never);

        let denied =
            identify_body_with(record(), &registry, consent_granting_vendors(vec![1])).await;
        let granted =
            identify_body_with(record(), &registry, consent_granting_vendors(vec![9999])).await;

        assert!(
            denied.get("uid").is_none(),
            "should withhold the UID without the owner's vendor consent"
        );
        assert_eq!(
            granted["uid"], "live",
            "should return the UID with the owner's vendor consent"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn handle_identify_withholds_a_source_excluded_where_gdpr_applies() {
        let mut registry =
            PartnerRegistry::from_config(&[make_test_partner("ssp.example.com", VALID_API_TOKEN)])
                .expect("should build registry");
        assert!(
            registry.exclude_where_gdpr_applies("ssp.example.com"),
            "should mark the source excluded where GDPR applies"
        );
        let record = || provider_record("live", Expiry::Never);
        let non_regulated = ConsentContext {
            jurisdiction: Jurisdiction::NonRegulated,
            source: ConsentSource::Cookie,
            ..ConsentContext::default()
        };

        let gdpr = identify_body_with(record(), &registry, consent_granting_vendors(vec![1])).await;
        let elsewhere = identify_body_with(record(), &registry, non_regulated).await;

        assert!(
            gdpr.get("uid").is_none() && gdpr.get("eid").is_none(),
            "should withhold the excluded source from a GDPR visitor"
        );
        assert_eq!(
            elsewhere["uid"], "live",
            "should return the source outside GDPR"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn handle_identify_omits_expired_provider_uid() {
        let body =
            identify_body_for_record(provider_record("old", Expiry::At(UnixSeconds::new(1)))).await;

        assert!(
            body.get("uid").is_none(),
            "should omit expired provider uid"
        );
        assert!(
            body.get("eid").is_none(),
            "should omit eid without a live uid"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn handle_identify_returns_live_provider_eid_with_eid_fields() {
        let body = identify_body_for_record(provider_record("live", Expiry::Never)).await;

        assert_eq!(body["uid"], "live", "should return live provider uid");
        assert_eq!(
            body["eid"]["inserter"], "inserter.example.com",
            "should keep provider EID-level fields"
        );
        assert_eq!(
            body["eid"]["uids"][0]["atype"], 1,
            "should use the provider atype"
        );
    }
}
