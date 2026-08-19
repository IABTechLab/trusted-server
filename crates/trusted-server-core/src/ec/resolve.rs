//! Client-cycle Edge Cookie resolution endpoint (`POST /_ts/api/v1/ec/resolve`).
//!
//! A client-side Edge Cookie provider defers on the organic page request
//! (deriving no identifier at the edge) and lets the page do the work in the
//! browser. When the page has its result it posts the value here, and this
//! endpoint hands it to the configured provider's
//! [`resolve_from_client`](super::provider::EdgeCookieProvider::resolve_from_client)
//! to mint the Edge Cookie.
//!
//! The endpoint is provider-agnostic: it bounds the body, gates on the
//! permission model (the same gate as organic generation), calls the provider,
//! and sets the cookie on its own response so the value is live for every
//! subsequent first-party request. Whether the posted value is trustworthy is
//! the provider's responsibility. The payload arrives from the browser, so a
//! real provider verifies it (for example an OWID signature) before minting.

use edgezero_core::body::Body as EdgeBody;
use error_stack::Report;
use http::{Request, Response, StatusCode, header};

use crate::error::TrustedServerError;
use crate::settings::Settings;

use super::EcContext;
use super::cookies::set_provider_ec_cookie;
use super::provider::{ClientResolveInput, build_provider};

/// Maximum size of a resolve request body.
///
/// Client-cycle payloads (a random value, or a signed envelope such as a
/// vendor JSON payload) are small; this bound guards against an oversized body
/// before it is read into memory.
const MAX_BODY_SIZE: usize = 64 * 1024;

/// Handles `POST /_ts/api/v1/ec/resolve`.
///
/// Gates on the configured provider's required permissions, then asks the
/// provider to mint an Edge Cookie from the posted payload. On success the EC
/// cookie is set on this response (so the browser carries it on subsequent
/// first-party requests) and the status is `200`. When the gate is closed, no
/// provider is configured, or the provider produces no identifier, the response
/// is `204` with no cookie. An oversized body is rejected with `413`.
///
/// # Errors
///
/// Returns [`TrustedServerError`] when the provider fails to process the
/// payload. A payload that is merely unverified or absent yields a `204`
/// rather than an error.
pub fn handle_ec_resolve(
    settings: &Settings,
    req: Request<EdgeBody>,
    ec_context: &EcContext,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    // Gate: the configured provider's required permissions must be set for
    // this request, the same gate as the organic generation path. A
    // client-driven resolve does not bypass the permission model.
    if !ec_context.ec_allowed() {
        log::info!("EC resolve skipped: required permissions not set");
        return Ok(status_only(StatusCode::NO_CONTENT));
    }

    // Rebuild the provider with the same host signals captured on the context, so
    // a provider that needs a service the host cannot supply fails here. The
    // client value is verified from the posted body below, not from request info.
    let Some(provider) = build_provider(
        &settings.ec,
        ec_context.host_signals(),
        ec_context.ec_provider(),
    )?
    else {
        log::info!("EC resolve skipped: no Edge Cookie provider configured");
        return Ok(status_only(StatusCode::NO_CONTENT));
    };

    // Bound the body before reading it into memory.
    if content_length_exceeds_limit(&req, MAX_BODY_SIZE) {
        return Ok(status_only(StatusCode::PAYLOAD_TOO_LARGE));
    }
    let payload = req.into_body().into_bytes().unwrap_or_default();
    if payload.len() > MAX_BODY_SIZE {
        return Ok(status_only(StatusCode::PAYLOAD_TOO_LARGE));
    }

    let input = ClientResolveInput {
        payload: payload.as_ref(),
        permissions: Some(ec_context.permissions()),
        consent: Some(ec_context.consent()),
    };

    let generated = provider.resolve_from_client(&input)?;
    log::debug!(
        "EC resolve handled (provider={}): id {}",
        provider.id(),
        if generated.id.is_some() {
            "minted"
        } else {
            "not minted"
        },
    );

    let mut response = status_only(if generated.id.is_some() {
        StatusCode::OK
    } else {
        StatusCode::NO_CONTENT
    });

    // Apply any response headers the provider asked for (for example to request
    // more client evidence on a later request). Empty for the demo provider.
    for (name, value) in generated.response_headers {
        response.headers_mut().insert(name, value);
    }

    if let Some(ec_id) = generated.id {
        set_provider_ec_cookie(settings, &mut response, &ec_id);
    }

    Ok(response)
}

/// Builds a bodiless response with the given status.
fn status_only(status: StatusCode) -> Response<EdgeBody> {
    let mut response = Response::new(EdgeBody::empty());
    *response.status_mut() = status;
    response
}

/// Returns `true` when the request advertises a `Content-Length` over `limit`.
fn content_length_exceeds_limit(req: &Request<EdgeBody>, limit: usize) -> bool {
    req.headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|len| len > limit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consent::types::ConsentContext;
    use crate::ec::provider::{EdgeCookieProvider, GeneratedEdgeCookie, IdentityInput};
    use crate::evidence::RequestInfo;
    use crate::platform::test_support::noop_services_with_ec_provider;
    use crate::test_support::tests::create_test_settings;
    use http::Method;
    use std::sync::Arc;

    fn settings_with_client_fixed() -> Settings {
        let mut settings = create_test_settings();
        settings.ec.provider = Some("client-fixed".to_owned());
        settings
    }

    // The fixed word shared by the client-fixed provider and its page script.
    const FIXED_WORD: &str = "an-ec";

    fn post(body: &str) -> Request<EdgeBody> {
        Request::builder()
            .method(Method::POST)
            .uri("https://edge.example.com/_ts/api/v1/ec/resolve")
            .body(EdgeBody::from(body.to_owned()))
            .expect("should build resolve request")
    }

    fn gated(ec_allowed: bool) -> EcContext {
        EcContext::new_for_test_gated(None, ConsentContext::default(), ec_allowed)
    }

    /// Returns whether `value` is a canonical UUID (`8-4-4-4-12` lowercase hex).
    fn is_uuid(value: &str) -> bool {
        let groups = [8, 4, 4, 4, 12];
        let parts: Vec<&str> = value.split('-').collect();
        parts.len() == groups.len()
            && parts.iter().zip(groups).all(|(part, len)| {
                part.len() == len
                    && part
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
    }

    /// A **test-only** provider modeling a client-generated, first-party
    /// identifier (a `UUID`) that the browser mints and posts back for the server
    /// to set. It is not a production provider and exists only to exercise the
    /// client-set Edge Cookie value path from end to end. The edge defers, the
    /// page posts a value, and it must round-trip as the cookie and the KV key.
    ///
    /// A `UUID` has no separator, so the built-in [`normalize_id_for_kv`] default
    /// would append a trailing dot and corrupt it, which is why this provider
    /// (like any opaque-identifier provider) returns the value unchanged.
    ///
    /// [`normalize_id_for_kv`]: EdgeCookieProvider::normalize_id_for_kv
    #[derive(Debug)]
    struct TestIdProvider;

    impl EdgeCookieProvider for TestIdProvider {
        fn id(&self) -> &'static str {
            "testid"
        }

        fn generate(
            &self,
            _request_info: &dyn RequestInfo,
            _input: &IdentityInput<'_>,
        ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>> {
            // The identifier is minted in the browser, so the edge derives nothing.
            Ok(GeneratedEdgeCookie::default())
        }

        fn resolve_from_client(
            &self,
            input: &ClientResolveInput<'_>,
        ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>> {
            // The page posts the identifier it generated. Accept a well-formed UUID.
            let value = core::str::from_utf8(input.payload)
                .unwrap_or_default()
                .trim();
            if is_uuid(value) {
                Ok(GeneratedEdgeCookie {
                    id: Some(value.to_owned()),
                    response_headers: Vec::new(),
                })
            } else {
                Ok(GeneratedEdgeCookie::default())
            }
        }

        fn accepts_id(&self, value: &str) -> bool {
            is_uuid(value)
        }

        fn normalize_id_for_kv(&self, value: &str) -> String {
            value.to_owned()
        }
    }

    #[test]
    fn client_set_value_round_trips_through_the_ec_scenario() {
        // A client-generated first-party UUID, the value the browser posts back.
        const TEST_ID: &str = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";

        let provider: Arc<dyn EdgeCookieProvider> = Arc::new(TestIdProvider);
        let mut settings = create_test_settings();
        settings.ec.provider = Some("testid".to_owned());
        let services = noop_services_with_ec_provider(Arc::clone(&provider));

        // 1. Organic first visit: no EC yet, and the edge defers because the
        //    identifier is generated in the browser, not derived server-side.
        let organic = Request::builder()
            .method(Method::GET)
            .uri("https://edge.example.com/")
            .body(EdgeBody::empty())
            .expect("should build organic request");
        let mut ec = EcContext::read_from_request(&settings, &organic, &services)
            .expect("should read EC context");
        assert!(
            ec.ec_value().is_none(),
            "no EC should exist on the first visit"
        );
        ec.generate_if_needed(&settings, None)
            .expect("should run generation");
        assert!(
            ec.ec_value().is_none(),
            "a client-set provider defers minting to the browser"
        );

        // 2. The page generates its identifier and posts it to the resolve
        //    endpoint; the server sets it as the EC value.
        let response =
            handle_ec_resolve(&settings, post(TEST_ID), &ec).expect("should handle resolve");
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "a valid client-set value should return 200"
        );
        let set_cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .expect("should set the EC cookie")
            .to_str()
            .expect("should be utf-8");
        assert!(
            set_cookie.contains(TEST_ID),
            "the EC cookie should carry the full client-set identifier, got {set_cookie}"
        );

        // 3. A later request carries the EC cookie; the server reads the
        //    identifier back verbatim. This is the step the built-in shape check
        //    used to drop.
        let ret = Request::builder()
            .method(Method::GET)
            .uri("https://edge.example.com/")
            .header("cookie", format!("ts-ec={TEST_ID}"))
            .body(EdgeBody::empty())
            .expect("should build return request");
        let ec2 = EcContext::read_from_request(&settings, &ret, &services)
            .expect("should read EC context");
        assert_eq!(
            ec2.ec_value(),
            Some(TEST_ID),
            "the client-set identifier should round-trip as the EC value"
        );
    }

    #[test]
    fn resolve_sets_cookie_when_word_matches_and_allowed() {
        let settings = settings_with_client_fixed();
        let response = handle_ec_resolve(&settings, post(FIXED_WORD), &gated(true))
            .expect("should handle resolve");

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "a verified value should return 200"
        );
        let set_cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .expect("should set the EC cookie")
            .to_str()
            .expect("should be utf-8");
        assert!(
            set_cookie.contains(FIXED_WORD),
            "should set the verified known word as the EC cookie, got {set_cookie}"
        );
        assert!(
            set_cookie.contains("HttpOnly"),
            "the EC cookie should be HttpOnly"
        );
        assert!(
            set_cookie.contains("Secure"),
            "the EC cookie should be Secure"
        );
    }

    #[test]
    fn resolve_returns_204_when_not_allowed() {
        let settings = settings_with_client_fixed();
        let response = handle_ec_resolve(&settings, post(FIXED_WORD), &gated(false))
            .expect("should handle resolve");

        assert_eq!(
            response.status(),
            StatusCode::NO_CONTENT,
            "a closed permission gate should return 204"
        );
        assert!(
            response.headers().get(header::SET_COOKIE).is_none(),
            "a closed gate should set no cookie"
        );
    }

    #[test]
    fn resolve_returns_204_when_no_provider_configured() {
        let mut settings = create_test_settings();
        settings.ec.provider = None;
        let response =
            handle_ec_resolve(&settings, post("123"), &gated(true)).expect("should handle resolve");

        assert_eq!(
            response.status(),
            StatusCode::NO_CONTENT,
            "no configured provider should return 204"
        );
        assert!(
            response.headers().get(header::SET_COOKIE).is_none(),
            "no configured provider should set no cookie"
        );
    }

    #[test]
    fn resolve_rejects_oversized_body() {
        let settings = settings_with_client_fixed();
        let big = "x".repeat(MAX_BODY_SIZE + 1);
        let response =
            handle_ec_resolve(&settings, post(&big), &gated(true)).expect("should handle resolve");

        assert_eq!(
            response.status(),
            StatusCode::PAYLOAD_TOO_LARGE,
            "an oversized body should be rejected with 413"
        );
    }

    #[test]
    fn resolve_sets_no_cookie_for_unmatched_word() {
        let settings = settings_with_client_fixed();
        let response = handle_ec_resolve(&settings, post("not-the-word"), &gated(true))
            .expect("should handle resolve");

        assert_eq!(
            response.status(),
            StatusCode::NO_CONTENT,
            "a value that fails verification should yield no cookie and a 204"
        );
        assert!(
            response.headers().get(header::SET_COOKIE).is_none(),
            "an unmatched value should set no cookie"
        );
    }
}
