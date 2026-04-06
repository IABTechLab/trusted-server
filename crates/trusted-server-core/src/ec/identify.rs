//! Identity lookup endpoint (`GET /identify`).

use std::collections::HashMap;

use error_stack::{Report, ResultExt};
use fastly::http::{header, StatusCode};
use fastly::{Request, Response};
use url::Url;

use crate::consent::allows_ec_creation;
use crate::constants::{
    HEADER_X_TS_EC, HEADER_X_TS_EC_CONSENT, HEADER_X_TS_EIDS, HEADER_X_TS_EIDS_TRUNCATED,
};
use crate::error::TrustedServerError;
use crate::openrtb::Eid;
use crate::settings::Settings;

use super::eids::{build_eids_header, resolve_partner_ids, to_eids};
use super::kv::KvIdentityGraph;
use super::partner::PartnerStore;
use super::EcContext;

const MAX_EXPOSE_PARTNER_HEADERS: usize = 20;

/// Handles `GET /identify`.
///
/// # Errors
///
/// Returns [`TrustedServerError`] for response serialization issues.
pub fn handle_identify(
    settings: &Settings,
    kv: &KvIdentityGraph,
    partner_store: &PartnerStore,
    req: &Request,
    ec_context: &EcContext,
) -> Result<Response, Report<TrustedServerError>> {
    let cors = classify_origin(req, settings);
    if matches!(cors, CorsDecision::Denied) {
        return Ok(Response::from_status(StatusCode::FORBIDDEN));
    }

    if !allows_ec_creation(ec_context.consent()) {
        let mut response = json_response(
            StatusCode::FORBIDDEN,
            &serde_json::json!({ "consent": "denied" }),
        )?;
        if let CorsDecision::Allowed(origin) = cors {
            apply_cors_headers(&mut response, &origin);
        }
        return Ok(response);
    }

    let Some(ec_id) = ec_context.ec_value() else {
        let mut response = Response::from_status(StatusCode::NO_CONTENT);
        if let CorsDecision::Allowed(origin) = cors {
            apply_cors_headers(&mut response, &origin);
        }
        return Ok(response);
    };

    let mut degraded = false;
    let mut resolved = Vec::new();
    let mut cluster_size: Option<u32> = None;

    match kv.get(ec_id) {
        Ok(Some((entry, generation))) => {
            // Resolve partner IDs.
            match resolve_partner_ids(partner_store, &entry) {
                Ok(values) => {
                    resolved = values;
                }
                Err(err) => {
                    log::warn!("Identify partner resolution failed: {err:?}");
                    degraded = true;
                }
            }

            // Evaluate cluster size (lazy, TTL-gated).
            match kv.evaluate_cluster(ec_id, &entry, generation, settings.ec.cluster_recheck_secs) {
                Ok(size) => {
                    cluster_size = size;
                }
                Err(err) => {
                    log::warn!("Cluster evaluation failed for '{ec_id}': {err:?}");
                    // Non-fatal — cluster_size stays None, response is still useful.
                }
            }
        }
        Ok(None) => {}
        Err(err) => {
            log::warn!("Identify KV read failed for EC ID '{ec_id}': {err:?}");
            degraded = true;
        }
    }

    let mut uids = HashMap::new();
    for item in &resolved {
        uids.insert(item.partner_id.clone(), item.uid.clone());
    }

    let eids = to_eids(&resolved);
    let body = IdentifyResponse {
        ec: ec_id.to_owned(),
        consent: "ok".to_owned(),
        degraded,
        uids,
        eids,
        cluster_size,
    };

    let mut response = json_response(StatusCode::OK, &body)?;
    response.set_header(HEADER_X_TS_EC, ec_id);
    response.set_header(HEADER_X_TS_EC_CONSENT, "ok");

    let mut expose_headers = vec![
        "x-ts-ec".to_owned(),
        "x-ts-eids".to_owned(),
        "x-ts-ec-consent".to_owned(),
        "x-ts-eids-truncated".to_owned(),
    ];

    for item in resolved.iter().take(MAX_EXPOSE_PARTNER_HEADERS) {
        let header_name = format!("x-ts-{}", item.partner_id);
        response.set_header(&header_name, &item.uid);
        expose_headers.push(header_name);
    }

    let (encoded_eids, truncated) = build_eids_header(&resolved)?;
    response.set_header(HEADER_X_TS_EIDS, encoded_eids);
    if truncated {
        response.set_header(HEADER_X_TS_EIDS_TRUNCATED, "true");
    }

    if let Some(size) = cluster_size {
        response.set_header("x-ts-cluster-size", size.to_string());
        expose_headers.push("x-ts-cluster-size".to_owned());
    }

    if let CorsDecision::Allowed(origin) = cors {
        apply_cors_headers(&mut response, &origin);
        response.set_header(
            header::ACCESS_CONTROL_EXPOSE_HEADERS,
            expose_headers.join(", "),
        );
    }

    Ok(response)
}

/// Handles `OPTIONS /identify` CORS preflight.
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
    uids: HashMap<String, String>,
    eids: Vec<Eid>,
    /// Network cluster size. `None` when not yet evaluated or unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    cluster_size: Option<u32>,
}

fn json_response<T: serde::Serialize>(
    status: StatusCode,
    body: &T,
) -> Result<Response, Report<TrustedServerError>> {
    let body = serde_json::to_string(body).change_context(TrustedServerError::Configuration {
        message: "Failed to serialize identify response".to_owned(),
    })?;

    Ok(Response::from_status(status)
        .with_content_type(fastly::mime::APPLICATION_JSON)
        .with_body(body))
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

fn apply_cors_headers(response: &mut Response, origin: &str) {
    response.set_header(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
    response.set_header(header::ACCESS_CONTROL_ALLOW_CREDENTIALS, "true");
    response.set_header(header::ACCESS_CONTROL_ALLOW_METHODS, "GET, OPTIONS");
    response.set_header(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        "Cookie, X-ts-ec, X-consent-advertising",
    );
    response.set_header(header::ACCESS_CONTROL_MAX_AGE, "600");
    response.set_header(header::VARY, "Origin");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consent::jurisdiction::Jurisdiction;
    use crate::consent::types::{ConsentContext, ConsentSource};
    use crate::ec::eids::{ResolvedPartnerId, MAX_EIDS_HEADER_BYTES};
    use crate::test_support::tests::create_test_settings;

    fn make_ec_context(jurisdiction: Jurisdiction, ec_value: Option<&str>) -> EcContext {
        let consent = ConsentContext {
            jurisdiction,
            source: ConsentSource::Cookie,
            ..ConsentContext::default()
        };
        EcContext::new_for_test(ec_value.map(str::to_owned), consent)
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
    fn eids_header_truncates_when_too_large() {
        let mut resolved = Vec::new();
        for idx in 0..64 {
            resolved.push(ResolvedPartnerId {
                partner_id: format!("p{idx}"),
                uid: "x".repeat(200),
                synced: 1000 - idx,
                source_domain: format!("s{idx}.example.com"),
                openrtb_atype: 3,
            });
        }

        let (header, truncated) =
            build_eids_header(&resolved).expect("should build capped eids header");
        assert!(truncated, "should truncate oversized eids header payload");
        assert!(
            header.len() <= MAX_EIDS_HEADER_BYTES,
            "should cap encoded header bytes"
        );
    }

    #[test]
    fn handle_identify_denied_consent_returns_403_json() {
        let settings = create_test_settings();
        let kv = KvIdentityGraph::new("missing_store");
        let partner_store = PartnerStore::new("missing_store");
        let mut req = Request::new("GET", "https://edge.test-publisher.com/identify");
        req.set_header("origin", "https://www.test-publisher.com");
        let ec_context = make_ec_context(Jurisdiction::Unknown, None);

        let mut response = handle_identify(&settings, &kv, &partner_store, &req, &ec_context)
            .expect("should construct denied response");

        assert_eq!(
            response.get_status(),
            StatusCode::FORBIDDEN,
            "should return 403 when consent denies EC"
        );
        assert_eq!(
            response
                .get_header(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .and_then(|v| v.to_str().ok()),
            Some("https://www.test-publisher.com"),
            "should include CORS allow-origin for approved publisher origin"
        );

        let body = serde_json::from_slice::<serde_json::Value>(&response.take_body_bytes())
            .expect("should decode denied JSON body");
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
        let partner_store = PartnerStore::new("missing_store");
        let mut req = Request::new("GET", "https://edge.test-publisher.com/identify");
        req.set_header("origin", "https://www.test-publisher.com");
        let ec_context = make_ec_context(Jurisdiction::NonRegulated, None);

        let response = handle_identify(&settings, &kv, &partner_store, &req, &ec_context)
            .expect("should construct no-content response");

        assert_eq!(
            response.get_status(),
            StatusCode::NO_CONTENT,
            "should return 204 when EC is unavailable"
        );
        assert_eq!(
            response
                .get_header(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .and_then(|v| v.to_str().ok()),
            Some("https://www.test-publisher.com"),
            "should include CORS allow-origin for approved publisher origin"
        );
    }

    #[test]
    fn handle_identify_kv_failure_sets_degraded_true() {
        let settings = create_test_settings();
        let kv = KvIdentityGraph::new("missing_store");
        let partner_store = PartnerStore::new("missing_store");
        let req = Request::new("GET", "https://edge.test-publisher.com/identify");
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let ec_context = make_ec_context(Jurisdiction::NonRegulated, Some(&ec_id));

        let mut response = handle_identify(&settings, &kv, &partner_store, &req, &ec_context)
            .expect("should construct degraded identify response");

        assert_eq!(
            response.get_status(),
            StatusCode::OK,
            "should return 200 on degraded KV read"
        );
        let body = serde_json::from_slice::<serde_json::Value>(&response.take_body_bytes())
            .expect("should decode identify response JSON");

        assert_eq!(body["ec"], ec_id, "should echo EC in body");
        assert_eq!(
            body["degraded"],
            serde_json::Value::Bool(true),
            "should mark response as degraded when KV read fails"
        );
        assert_eq!(
            body["uids"],
            serde_json::json!({}),
            "should emit empty uids"
        );
        assert_eq!(
            body["eids"],
            serde_json::json!([]),
            "should emit empty eids"
        );
        assert!(
            body.get("cluster_size").is_none(),
            "cluster_size should be omitted when KV read fails"
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
        assert_eq!(
            response
                .get_header(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .and_then(|v| v.to_str().ok()),
            Some("https://www.test-publisher.com"),
            "should include CORS allow-origin header"
        );
    }
}
