//! Identity lookup endpoint (`GET /identify`).

use std::collections::HashMap;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use error_stack::{Report, ResultExt};
use fastly::http::{header, StatusCode};
use fastly::{Request, Response};
use url::Url;

use crate::consent::allows_ec_creation;
use crate::constants::{
    HEADER_X_TS_EC, HEADER_X_TS_EC_CONSENT, HEADER_X_TS_EIDS, HEADER_X_TS_EIDS_TRUNCATED,
};
use crate::error::TrustedServerError;
use crate::openrtb::{Eid, Uid};
use crate::settings::Settings;

use super::kv::KvIdentityGraph;
use super::kv_types::KvEntry;
use super::partner::PartnerStore;
use super::EcContext;

const MAX_EXPOSE_PARTNER_HEADERS: usize = 20;
const MAX_EIDS_HEADER_BYTES: usize = 4096;

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

    if let Some(ec_hash) = ec_context.ec_hash() {
        match kv.get(ec_hash) {
            Ok(Some((entry, _generation))) => match resolve_partner_ids(partner_store, &entry) {
                Ok(values) => {
                    resolved = values;
                }
                Err(err) => {
                    log::warn!("Identify partner resolution failed: {err:?}");
                    degraded = true;
                }
            },
            Ok(None) => {}
            Err(err) => {
                log::warn!("Identify KV read failed for hash '{ec_hash}': {err:?}");
                degraded = true;
            }
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
}

struct ResolvedPartnerId {
    partner_id: String,
    uid: String,
    synced: u64,
    source_domain: String,
    openrtb_atype: u8,
}

fn resolve_partner_ids(
    partner_store: &PartnerStore,
    entry: &KvEntry,
) -> Result<Vec<ResolvedPartnerId>, Report<TrustedServerError>> {
    let mut resolved = Vec::new();

    for (partner_id, partner_uid) in &entry.ids {
        if partner_uid.uid.is_empty() {
            continue;
        }

        let Some(partner) = partner_store.get(partner_id)? else {
            continue;
        };
        if !partner.bidstream_enabled {
            continue;
        }

        resolved.push(ResolvedPartnerId {
            partner_id: partner_id.clone(),
            uid: partner_uid.uid.clone(),
            synced: partner_uid.synced,
            source_domain: partner.source_domain,
            openrtb_atype: partner.openrtb_atype,
        });
    }

    resolved.sort_by(|a, b| b.synced.cmp(&a.synced));
    Ok(resolved)
}

fn to_eids(resolved: &[ResolvedPartnerId]) -> Vec<Eid> {
    resolved
        .iter()
        .map(|item| Eid {
            source: item.source_domain.clone(),
            uids: vec![Uid {
                id: item.uid.clone(),
                atype: Some(item.openrtb_atype),
                ext: None,
            }],
        })
        .collect()
}

fn build_eids_header(
    resolved: &[ResolvedPartnerId],
) -> Result<(String, bool), Report<TrustedServerError>> {
    for size in (0..=resolved.len()).rev() {
        let eids = to_eids(&resolved[..size]);
        let json = serde_json::to_vec(&eids).change_context(TrustedServerError::Configuration {
            message: "Failed to serialize identify eids header payload".to_owned(),
        })?;
        let encoded = BASE64.encode(json);

        if encoded.len() <= MAX_EIDS_HEADER_BYTES {
            return Ok((encoded, size != resolved.len()));
        }
    }

    Ok((BASE64.encode("[]"), true))
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
    response.set_header(header::VARY, "Origin");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consent::jurisdiction::Jurisdiction;
    use crate::consent::types::{ConsentContext, ConsentSource};
    use crate::test_support::tests::create_test_settings;

    fn make_ec_context(jurisdiction: Jurisdiction, ec_value: Option<&str>) -> EcContext {
        EcContext {
            ec_value: ec_value.map(str::to_owned),
            cookie_ec_value: ec_value.map(str::to_owned),
            ec_was_present: ec_value.is_some(),
            ec_generated: false,
            consent: ConsentContext {
                jurisdiction,
                source: ConsentSource::Cookie,
                ..ConsentContext::default()
            },
            client_ip: None,
            geo_info: None,
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
