//! First-party reconciliation after the publisher CMP changes its real consent signal.
//!
//! The browser transports consent through the ordinary consent cookies. This endpoint
//! accepts no client-supplied identity, vendor token, or inferred permission grant.

use edgezero_core::body::Body as EdgeBody;
use http::{HeaderValue, Method, Request, Response, StatusCode, header};
use serde::Deserialize;
use url::Url;

use super::kv::KvIdentityGraph;
use crate::http_util::RequestInfo;
use crate::platform::RuntimeServices;
use crate::response_privacy::enforce_terminal_private_cache_privacy;

pub const CONSENT_UPDATE_PATH: &str = "/_ts/consent";
// Two browser-cookie-sized signals plus their JSON representation. This is a
// consent-only transport, not another general auction or identity payload.
pub const MAX_CONSENT_UPDATE_BODY_BYTES: usize = 16 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ConsentSnapshot {
    #[serde(deserialize_with = "required_nullable_gpp")]
    gpp_string: Option<String>,
    applicable_sections: Vec<i32>,
}

fn required_nullable_gpp<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    Option::deserialize(deserializer)
}

#[derive(Clone, Copy, Debug)]
struct ConsentUpdateResponse;

#[derive(Clone, Copy, Debug)]
struct PreparedConsentUpdate;

fn response(status: StatusCode) -> Response<EdgeBody> {
    let mut response = Response::new(EdgeBody::empty());
    *response.status_mut() = status;
    enforce_terminal_private_cache_privacy(&mut response);
    response
}

/// Marks an accepted consent update whose finalization could not confirm revocation.
/// Ordinary publisher/error responses retain their existing status semantics.
pub(crate) fn mark_reconciliation_failed(response: &mut Response<EdgeBody>) {
    if response
        .extensions()
        .get::<ConsentUpdateResponse>()
        .is_some()
    {
        *response.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
    }
}

fn same_origin(services: &RuntimeServices, req: &Request<EdgeBody>) -> bool {
    let Some(origin) = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let Ok(origin) = Url::parse(origin) else {
        return false;
    };
    if !origin.username().is_empty()
        || origin.password().is_some()
        || origin.path() != "/"
        || origin.query().is_some()
        || origin.fragment().is_some()
    {
        return false;
    }
    let info = RequestInfo::from_request(req, services.client_info());
    let Ok(expected) = Url::parse(&format!("{}://{}", info.scheme, info.host)) else {
        return false;
    };
    // The serving origin may be a reviewed demo origin; use normalized trusted
    // ingress, not a hard-coded publisher origin or a viewer-selected header.
    origin.origin() == expected.origin()
}

/// Reads one bounded CMP snapshot before adapters construct the EC context.
///
/// Each request carries its own snapshot: a withdrawal must not accidentally
/// read a later grant from cookies by the time network dispatch occurs.
/// Only GPP consent headers are replaced; identity and other privacy signals remain.
///
/// # Errors
///
/// Returns a private HTTP error for unsupported methods, invalid origins,
/// oversized bodies, or malformed snapshots. Error responses are boxed so the
/// successful request path does not carry a large error variant.
pub async fn prepare_consent_update(
    services: &RuntimeServices,
    req: Request<EdgeBody>,
) -> Result<Request<EdgeBody>, Box<Response<EdgeBody>>> {
    if req.method() != Method::POST {
        let mut response = response(StatusCode::METHOD_NOT_ALLOWED);
        response
            .headers_mut()
            .insert(header::ALLOW, HeaderValue::from_static("POST"));
        return Err(Box::new(response));
    }
    if !same_origin(services, &req) {
        return Err(Box::new(response(StatusCode::FORBIDDEN)));
    }
    let (mut parts, body) = req.into_parts();
    let bytes = body
        .into_bytes_bounded(MAX_CONSENT_UPDATE_BODY_BYTES)
        .await
        .map_err(|_| response(StatusCode::PAYLOAD_TOO_LARGE))?;
    let snapshot: ConsentSnapshot =
        serde_json::from_slice(&bytes).map_err(|_| response(StatusCode::BAD_REQUEST))?;
    if let Some(gpp) = &snapshot.gpp_string {
        if gpp.is_empty()
            || !gpp
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._~-=".contains(&byte))
            || snapshot
                .applicable_sections
                .iter()
                .any(|id| !(1..=65535).contains(id))
            || crate::consent::gpp::decode_gpp_string(gpp).is_err()
        {
            return Err(Box::new(response(StatusCode::BAD_REQUEST)));
        }
    } else if !snapshot.applicable_sections.is_empty() {
        return Err(Box::new(response(StatusCode::BAD_REQUEST)));
    }
    let mut cookies = Vec::new();
    for header in parts.headers.get_all(header::COOKIE) {
        let value = header
            .to_str()
            .map_err(|_| response(StatusCode::BAD_REQUEST))?;
        for pair in value
            .split(';')
            .map(str::trim)
            .filter(|pair| !pair.is_empty())
        {
            let name = pair.split_once('=').map_or(pair, |(name, _)| name.trim());
            if name != "__gpp" && name != "__gpp_sid" {
                cookies.push(pair.to_owned());
            }
        }
    }
    if let Some(gpp) = snapshot.gpp_string {
        cookies.push(format!("__gpp={gpp}"));
        if !snapshot.applicable_sections.is_empty() {
            cookies.push(format!(
                "__gpp_sid={}",
                snapshot
                    .applicable_sections
                    .iter()
                    .map(i32::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            ));
        }
    }
    parts.headers.remove(header::COOKIE);
    if !cookies.is_empty() {
        let value = HeaderValue::from_str(&cookies.join("; "))
            .map_err(|_| response(StatusCode::BAD_REQUEST))?;
        parts.headers.insert(header::COOKIE, value);
    }
    let mut req = Request::from_parts(parts, EdgeBody::empty());
    req.extensions_mut().insert(PreparedConsentUpdate);
    Ok(req)
}

/// Acknowledges a prepared snapshot through the ordinary EC finalization path.
///
/// Adapters must prepare the snapshot before extracting EC/consent context, run
/// their usual request filters, and finalize exactly once before headers commit.
/// This endpoint never mints an identity or restores EIDs. Thus an older grant
/// response cannot recreate an identity after a concurrent withdrawal. Creation
/// remains on eligible publisher navigation; partner identify remains read-only.
pub fn handle_consent_update(
    req: &Request<EdgeBody>,
    graph: Option<&KvIdentityGraph>,
) -> Response<EdgeBody> {
    if req.extensions().get::<PreparedConsentUpdate>().is_none() {
        return response(StatusCode::BAD_REQUEST);
    }
    let mut response = response(if graph.is_some() {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    });
    response.extensions_mut().insert(ConsentUpdateResponse);
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consent::{ConsentContext, jurisdiction::Jurisdiction};
    use crate::ec::{EcContext, finalize::ec_finalize_response, registry::PartnerRegistry};
    use crate::platform::test_support::noop_services;
    use crate::test_support::tests::create_test_settings;

    const GPP: &str = "DBABLA~BVQqAAAAAgA.QA";
    const EC: &str = "aaaaaaaabbbbbbbbccccccccddddddddeeeeeeeeffffffff0000000011111111.abc123";

    fn request(
        method: Method,
        origin: Option<&str>,
        body: impl Into<serde_json::Value>,
    ) -> Request<EdgeBody> {
        let mut builder = Request::builder()
            .method(method)
            .uri("https://publisher.example/_ts/consent")
            .header(header::HOST, "publisher.example")
            .header("x-forwarded-proto", "https")
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(origin) = origin {
            builder = builder.header(header::ORIGIN, origin);
        }
        builder
            .body(EdgeBody::from(body.into().to_string()))
            .expect("fixture request")
    }

    fn snapshot() -> serde_json::Value {
        serde_json::json!({ "gppString": GPP, "applicableSections": [7] })
    }

    #[tokio::test]
    async fn snapshot_replaces_only_gpp_headers_and_preserves_identity_and_gpc() {
        let mut req = request(Method::POST, Some("https://publisher.example"), snapshot());
        req.headers_mut().insert(
            header::COOKIE,
            HeaderValue::from_str(&format!(
                "__gpp=synthetic-later-state; __gpp_sid=2; us_privacy=1YNN; ts-ec={EC}"
            ))
            .expect("fixture cookie"),
        );
        req.headers_mut()
            .insert("sec-gpc", HeaderValue::from_static("1"));
        let req = prepare_consent_update(&noop_services(), req)
            .await
            .expect("valid snapshot");
        let cookies = crate::cookies::handle_request_cookies(&req)
            .expect("cookies")
            .expect("jar");
        assert_eq!(cookies.get("__gpp").expect("GPP").value(), GPP);
        assert_eq!(cookies.get("__gpp_sid").expect("sections").value(), "7");
        assert_eq!(cookies.get("ts-ec").expect("identity").value(), EC);
        assert_eq!(cookies.get("us_privacy").expect("legacy").value(), "1YNN");
        assert_eq!(req.headers()["sec-gpc"], "1");
    }

    #[tokio::test]
    async fn missing_signal_is_not_synthesized_as_an_opt_out_or_grant() {
        let mut req = request(
            Method::POST,
            Some("https://publisher.example"),
            serde_json::json!({
                "gppString": null, "applicableSections": []
            }),
        );
        req.headers_mut().insert(
            header::COOKIE,
            HeaderValue::from_static("__gpp=stale; __gpp_sid=7; ordinary=retained"),
        );
        let req = prepare_consent_update(&noop_services(), req)
            .await
            .expect("missing snapshot");
        let jar = crate::cookies::handle_request_cookies(&req)
            .expect("cookies")
            .expect("jar");
        assert!(jar.get("__gpp").is_none());
        assert!(jar.get("__gpp_sid").is_none());
        assert!(jar.get("ordinary").is_some());
    }

    #[tokio::test]
    async fn rejects_missing_or_foreign_origins_and_non_origin_urls() {
        for origin in [
            None,
            Some("null"),
            Some("https://attacker.invalid"),
            Some("https://publisher.example/path"),
            Some("https://user@publisher.example"),
        ] {
            let response =
                prepare_consent_update(&noop_services(), request(Method::POST, origin, snapshot()))
                    .await
                    .expect_err("must reject origin");
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
    }

    #[tokio::test]
    async fn rejects_unclaimed_methods_locally() {
        for method in [Method::GET, Method::OPTIONS, Method::HEAD] {
            let response = prepare_consent_update(
                &noop_services(),
                request(method, Some("https://publisher.example"), snapshot()),
            )
            .await
            .expect_err("must reject method");
            assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
            assert_eq!(response.headers()[header::ALLOW], "POST");
        }
    }

    #[tokio::test]
    async fn rejects_malformed_or_inconsistent_snapshots() {
        for body in [
            serde_json::json!({}),
            serde_json::json!({"gppString": "invalid", "applicableSections": [7]}),
            serde_json::json!({"gppString": null, "applicableSections": [7]}),
            serde_json::json!({"gppString": GPP, "applicableSections": [-1]}),
            serde_json::json!({"gppString": GPP, "applicableSections": [70000]}),
            serde_json::json!({"gppString": GPP, "applicableSections": [7], "identity": "not-accepted"}),
        ] {
            let response = prepare_consent_update(
                &noop_services(),
                request(Method::POST, Some("https://publisher.example"), body),
            )
            .await
            .expect_err("must reject snapshot");
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }
    }

    #[tokio::test]
    async fn reconciliation_does_not_mint_identity_even_when_creation_is_permitted() {
        let settings = create_test_settings();
        let req = prepare_consent_update(
            &noop_services(),
            request(Method::POST, Some("https://publisher.example"), snapshot()),
        )
        .await
        .expect("valid snapshot");
        let graph = KvIdentityGraph::in_memory("consent-test");
        let mut ec = EcContext::new_for_test_with_ip(
            None,
            ConsentContext {
                jurisdiction: Jurisdiction::NonRegulated,
                ..Default::default()
            },
            Some("198.51.100.24".to_owned()),
        );
        let mut response = handle_consent_update(&req, Some(&graph));
        ec_finalize_response(
            &settings,
            &mut ec,
            Some(&graph),
            &PartnerRegistry::empty(),
            None,
            None,
            &mut response,
        )
        .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(ec.ec_value().is_none());
        assert!(!response.headers().contains_key(header::SET_COOKIE));
    }

    #[tokio::test]
    async fn real_opt_out_snapshot_tombstones_the_existing_row_despite_later_grant_cookies() {
        // Synthetic protocol fixtures only, never visitor consent or identities.
        const OPT_OUT: &str = "DBABRg~BVVVVVVVVWA.AA";
        let settings = create_test_settings();
        let services = noop_services();
        let graph = KvIdentityGraph::in_memory("consent-revoke-test");
        let mut original = EcContext::new_for_test_with_ip(
            None,
            ConsentContext {
                jurisdiction: Jurisdiction::NonRegulated,
                ..Default::default()
            },
            Some("198.51.100.24".to_owned()),
        );
        original
            .generate_if_needed(&settings, Some(&graph))
            .await
            .expect("fixture identity");
        let id = original.ec_value().expect("fixture ID").to_owned();
        let mut req = request(
            Method::POST,
            Some("https://publisher.example"),
            serde_json::json!({
                "gppString": OPT_OUT, "applicableSections": [9]
            }),
        );
        req.headers_mut().insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("ts-ec={id}; __gpp={GPP}; __gpp_sid=7"))
                .expect("fixture cookie"),
        );
        let req = prepare_consent_update(&services, req)
            .await
            .expect("valid opt-out");
        let geo = crate::geo::GeoInfo::country_region("US", Some("TN".into()));
        let mut ec = EcContext::read_from_request_with_geo(&settings, &req, &services, Some(&geo))
            .expect("consent context");
        assert_eq!(
            ec.consent()
                .gpp
                .as_ref()
                .and_then(|gpp| gpp.us_sale_opt_out),
            Some(true)
        );
        let mut response = handle_consent_update(&req, Some(&graph));
        ec_finalize_response(
            &settings,
            &mut ec,
            Some(&graph),
            &PartnerRegistry::empty(),
            None,
            None,
            &mut response,
        )
        .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(
            !graph
                .get(&id)
                .await
                .expect("read")
                .expect("tombstone")
                .0
                .consent
                .ok
        );

        // A concurrent older grant may arrive after withdrawal. This endpoint
        // does not generate/recover identities or ingest EIDs, so it cannot
        // restore the tombstoned row or issue a live EC cookie.
        let grant_req = prepare_consent_update(
            &services,
            request(Method::POST, Some("https://publisher.example"), snapshot()),
        )
        .await
        .expect("grant snapshot");
        let mut late = EcContext::new_for_test(
            Some(id.clone()),
            ConsentContext {
                jurisdiction: Jurisdiction::NonRegulated,
                ..Default::default()
            },
        );
        let mut response = handle_consent_update(&grant_req, Some(&graph));
        ec_finalize_response(
            &settings,
            &mut late,
            Some(&graph),
            &PartnerRegistry::empty(),
            None,
            None,
            &mut response,
        )
        .await;
        assert!(
            !graph
                .get(&id)
                .await
                .expect("read")
                .expect("tombstone")
                .0
                .consent
                .ok
        );
        assert!(
            response
                .headers()
                .get_all(header::SET_COOKIE)
                .iter()
                .all(|value| {
                    value.to_str().is_ok_and(|value| {
                        !value.starts_with("ts-ec=") || value.contains("Max-Age=0")
                    })
                })
        );
    }

    #[tokio::test]
    async fn bounds_snapshot_body_before_parsing() {
        let response = prepare_consent_update(&noop_services(), request(Method::POST, Some("https://publisher.example"), serde_json::json!({
            "gppString": "x".repeat(MAX_CONSENT_UPDATE_BODY_BYTES + 1), "applicableSections": [7]
        }))).await.expect_err("oversized snapshot");
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn unavailable_store_is_not_acknowledged_as_success() {
        let req = prepare_consent_update(
            &noop_services(),
            request(Method::POST, Some("https://publisher.example"), snapshot()),
        )
        .await
        .expect("valid snapshot");
        let mut response = handle_consent_update(&req, None);
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let mut settings = create_test_settings();
        settings
            .response_headers
            .insert("Cache-Control".into(), "public, max-age=300".into());
        crate::response_privacy::apply_response_headers_with_cache_privacy(
            &settings,
            &mut response,
        );
        let cache = response.headers()[header::CACHE_CONTROL]
            .to_str()
            .expect("cache policy");
        assert!(cache.contains("private") && cache.contains("no-store"));
    }

    #[tokio::test]
    async fn failed_revocation_returns_failure_while_expiring_the_browser_cookie() {
        let settings = create_test_settings();
        let req = prepare_consent_update(
            &noop_services(),
            request(Method::POST, Some("https://publisher.example"), snapshot()),
        )
        .await
        .expect("valid snapshot");
        let graph = KvIdentityGraph::failing("unavailable-test-store");
        let mut ec = EcContext::new_for_test(
            Some(EC.to_owned()),
            ConsentContext {
                jurisdiction: Jurisdiction::UsState("TN".to_owned()),
                gpc: true,
                ..Default::default()
            },
        );
        let mut response = handle_consent_update(&req, Some(&graph));
        ec_finalize_response(
            &settings,
            &mut ec,
            Some(&graph),
            &PartnerRegistry::empty(),
            None,
            None,
            &mut response,
        )
        .await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            response
                .headers()
                .get_all(header::SET_COOKIE)
                .iter()
                .any(|value| value
                    .to_str()
                    .is_ok_and(|value| value.starts_with("ts-ec=") && value.contains("Max-Age=0")))
        );
    }
}
