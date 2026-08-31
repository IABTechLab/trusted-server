//! Client-cycle Edge Cookie resolution endpoint (`POST /_ts/api/v1/ec/resolve`).
//!
//! A client-side Edge Cookie provider defers on the organic page request
//! (deriving no identifier at the edge) and lets the page do the work in the
//! browser. When the page has its result it posts the value here, and this
//! endpoint hands it to the configured provider's
//! [`resolve_from_client`](super::provider::EdgeCookieProvider::resolve_from_client)
//! to create the Edge Cookie.
//!
//! The endpoint is provider-agnostic: it bounds the body, gates on the
//! permission model (the same gate as organic generation), calls the provider,
//! and sets the cookie on its own response so the value is live for every
//! subsequent first-party request. Whether the posted value is trustworthy is
//! the provider's responsibility. The payload arrives from the browser, so a
//! real provider verifies it (for example an OWID signature) before creating one.

use edgezero_core::body::Body as EdgeBody;
use error_stack::Report;
use http::{HeaderValue, Request, Response, StatusCode, header};

use crate::error::TrustedServerError;
use crate::settings::Settings;

use super::EcContext;
use super::cookies::{
    ec_id_has_only_allowed_chars, set_provider_ec_cookie, set_resolved_marker_cookie,
};
use super::kv::KvIdentityGraph;
use super::kv_types::KvEntry;
use super::provider::{ClientResolveInput, apply_provider_response_headers, build_provider};

/// Maximum size of a resolve request body.
///
/// Client-cycle payloads (a random value, or a signed envelope such as a
/// vendor JSON payload) are small; this bound guards against an oversized body
/// before it is read into memory.
const MAX_BODY_SIZE: usize = 64 * 1024;

/// Handles `POST /_ts/api/v1/ec/resolve`.
///
/// The request must carry an `Origin` on the publisher's domain (this endpoint
/// sets identity state, so a foreign page must not be able to drive it) and a
/// `text/plain` or `application/json` body. Gates on the configured provider's
/// required permissions, then asks the provider to create an Edge Cookie from
/// the posted payload. A created identifier is persisted to the identity graph
/// before the cookie is set, so withdrawal reaches a client-set identity the
/// same way it reaches an edge-created one. With no graph available this
/// endpoint sets no cookie at all, which is stricter than the organic
/// generation path, where the identifier is committed and only the row write
/// is skipped. On
/// success the EC cookie and its `non-HttpOnly` resolved marker are set and the
/// status is `200`.
///
/// Rejections: `403` for a missing or foreign `Origin`, `415` for another
/// content type, `413` for an oversized body, `400` when the provider creates an
/// identifier outside the identifier bounds, `409` when the request already
/// carries a different identity (a resolve must not silently replace one), and
/// `503` when the identity-graph write fails. When the permission gate is
/// closed, no provider is configured, no graph is available, or the provider
/// creates nothing, the response is `204` with no cookie. Every response this
/// handler builds carries `Cache-Control: no-store`; a provider or
/// configuration error propagates to the adapter's error response instead,
/// and so does a provider asking for a response header inside core's reserved
/// surface, which is a broken provider contract rather than a bad request.
///
/// # Errors
///
/// Returns [`TrustedServerError`] when the provider fails to process the
/// payload, or asks for a response header inside core's reserved surface
/// (see
/// [`reserved_response_effect`](crate::ec::provider::reserved_response_effect)).
/// A payload that is merely unverified or absent yields a `204` rather than an
/// error.
pub fn handle_ec_resolve(
    settings: &Settings,
    req: Request<EdgeBody>,
    ec_context: &EcContext,
    kv: Option<&KvIdentityGraph>,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    // This endpoint sets identity state from a page script, so the posted
    // request must originate from the publisher's own site. Browsers always
    // send `Origin` on cross-origin POSTs and on same-origin `fetch` POSTs,
    // so its absence means a non-browser caller, which has no business here.
    if !origin_is_publisher(&req, settings) {
        log::warn!("EC resolve rejected: missing or foreign Origin");
        return Ok(status_only(StatusCode::FORBIDDEN));
    }

    // The page script posts plain text; a vendor payload may be JSON. Anything
    // else is not a resolve payload.
    if !content_type_is_allowed(&req) {
        return Ok(status_only(StatusCode::UNSUPPORTED_MEDIA_TYPE));
    }

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
            "created"
        } else {
            "not created"
        },
    );

    // Check every response header the provider asked for against core's
    // reserved surface, exactly as the organic generation path does in
    // `EcContext::generate_with_provider`. A provider may set its own cookies
    // and headers, but not a managed `ts-` cookie, a header in the `x-ts-`
    // namespace, or a framing or hop-by-hop header. Without this a
    // browser-side provider could set `ts-ec` itself and walk straight past
    // the identifier bounds, the conflict check and the row-before-cookie
    // rule below. The check sits before the identifier is read because a
    // provider can return headers with no identifier at all, which is the
    // 204 path, and that path applies headers too.
    for (name, value) in &generated.response_headers {
        if let Some(effect) = super::provider::reserved_response_effect(name, value) {
            return Err(Report::new(TrustedServerError::EdgeCookie {
                message: format!(
                    "Provider `{}` returned a response header `{name}` that {effect}",
                    provider.id(),
                ),
            }));
        }
    }

    let generated_id = generated
        .id
        .map(|value| super::provider::apply_provider_code(provider.as_ref(), &value));
    let Some(ec_id) = generated_id else {
        let mut response = status_only(StatusCode::NO_CONTENT);
        apply_provider_response_headers(response.headers_mut(), generated.response_headers);
        return Ok(response);
    };

    // The same identifier bounds as the organic generation path: reject, never
    // rewrite. A provider that created an out-of-bounds identifier is a bad
    // request from the client's perspective, because the posted payload
    // produced an unusable identity.
    if !ec_id_has_only_allowed_chars(&ec_id) {
        log::error!(
            "EC resolve rejected: provider `{}` created an identifier outside the bounds",
            provider.id(),
        );
        return Ok(status_only(StatusCode::BAD_REQUEST));
    }

    // A resolve must not silently replace an identity the request already
    // carries. The page script does not post when an identity exists, so a
    // different identifier here is a conflict to surface, not paper
    // over.
    if let Some(existing) = ec_context.ec_value()
        && existing != ec_id
    {
        log::warn!(
            "EC resolve rejected: request already carries a different identity (provider={})",
            provider.id(),
        );
        return Ok(status_only(StatusCode::CONFLICT));
    }

    // Persist the identity-graph row before setting the cookie, keyed by the
    // provider's canonical form, exactly like the organic generation path.
    // Without a row, withdrawal could never reach this identity, so with no
    // graph available this endpoint creates no cookie at all, which is stricter
    // than the organic generation path, where the identifier is committed and
    // only the row write is skipped.
    let Some(graph) = kv else {
        log::warn!("EC resolve skipped: no identity graph available, so no cookie is created");
        return Ok(status_only(StatusCode::NO_CONTENT));
    };
    let now = super::current_timestamp();
    let mut entry = KvEntry::new(
        ec_context.consent(),
        ec_context.geo_info(),
        now,
        &settings.publisher.domain,
    );
    entry.device = ec_context
        .device_signals()
        .map(super::device::DeviceSignals::to_kv_device);
    let kv_key = super::provider::provider_kv_key(provider.as_ref(), &ec_id);
    if let Err(err) = graph.create_or_revive(&kv_key, &entry) {
        log::error!("EC resolve failed to write the identity-graph row: {err:?}");
        return Ok(status_only(StatusCode::SERVICE_UNAVAILABLE));
    }

    let mut response = status_only(StatusCode::OK);

    // Apply any response headers the provider asked for (for example to request
    // more client evidence on a later request). Empty for the demo provider.
    // They accumulate with what this handler already set rather than replacing
    // it, for the reasons on `provider::apply_provider_response_headers`; here
    // that keeps the `Cache-Control: no-store` every identity response must
    // carry, which a replacing write would drop.
    apply_provider_response_headers(response.headers_mut(), generated.response_headers);

    set_provider_ec_cookie(settings, &mut response, &ec_id);
    // The Edge Cookie is HttpOnly, so the page script cannot see it; the
    // non-HttpOnly marker tells the script the resolve succeeded so it does
    // not post again on every page view.
    set_resolved_marker_cookie(settings, &mut response);

    Ok(response)
}

/// Whether the request's `Origin` names the publisher's domain (exactly, or a
/// subdomain of it).
fn origin_is_publisher(req: &Request<EdgeBody>, settings: &Settings) -> bool {
    let Some(origin) = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let Some(host) = origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://"))
    else {
        return false;
    };
    let host = host.split(':').next().unwrap_or(host);
    let publisher = settings.publisher.domain.as_str();
    host.eq_ignore_ascii_case(publisher)
        || host
            .to_ascii_lowercase()
            .ends_with(&format!(".{}", publisher.to_ascii_lowercase()))
}

/// Whether the request's `Content-Type` is one a resolve payload may use.
fn content_type_is_allowed(req: &Request<EdgeBody>) -> bool {
    req.headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.split(';').next().unwrap_or(value).trim())
        .is_some_and(|media_type| {
            media_type.eq_ignore_ascii_case("text/plain")
                || media_type.eq_ignore_ascii_case("application/json")
        })
}

/// Builds a bodiless response with the given status. Identity-resolution
/// responses must never be cached by the browser or an intermediary.
fn status_only(status: StatusCode) -> Response<EdgeBody> {
    let mut response = Response::new(EdgeBody::empty());
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
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
    use crate::ec::provider::{
        CLIENT_FIXED_PROVIDER_KEY, EcProviderSelection, EdgeCookieProvider, GeneratedEdgeCookie,
        IdentityInput,
    };
    use crate::evidence::RequestInfo;
    use crate::platform::test_support::noop_services_with_ec_provider;
    use crate::test_support::tests::create_test_settings;
    use http::Method;
    use std::sync::Arc;

    fn settings_with_client_fixed() -> Settings {
        let mut settings = create_test_settings();
        settings.ec.provider = Some(EcProviderSelection::from(CLIENT_FIXED_PROVIDER_KEY));
        settings
    }

    // The fixed word shared by the client-fixed provider and its page script.
    const FIXED_WORD: &str = "an-ec";

    fn post(body: &str) -> Request<EdgeBody> {
        post_with(Some("https://test-publisher.com"), Some("text/plain"), body)
    }

    fn post_with(
        origin: Option<&str>,
        content_type: Option<&str>,
        body: &str,
    ) -> Request<EdgeBody> {
        let mut builder = Request::builder()
            .method(Method::POST)
            .uri("https://test-publisher.com/_ts/api/v1/ec/resolve");
        if let Some(origin) = origin {
            builder = builder.header(header::ORIGIN, origin);
        }
        if let Some(content_type) = content_type {
            builder = builder.header(header::CONTENT_TYPE, content_type);
        }
        builder
            .body(EdgeBody::from(body.to_owned()))
            .expect("should build resolve request")
    }

    fn in_memory_graph() -> crate::ec::kv::KvIdentityGraph {
        crate::ec::kv::KvIdentityGraph::in_memory("test-ec-store")
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
    /// identifier (a `UUID`) that the browser creates and posts back for the server
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

        fn code(&self) -> crate::ec::provider::ProviderCode {
            crate::provider_code!("t0id")
        }

        fn generate(
            &self,
            _request_info: &dyn RequestInfo,
            _input: &IdentityInput<'_>,
        ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>> {
            // The identifier is created in the browser, so the edge derives nothing.
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

    /// The identifier [`ResolveHeaderProvider`] creates when asked to.
    const HEADER_PROVIDER_ID: &str = "5c3a1b70-2f4d-4a19-9c6e-7b0d18e4a221";

    /// A **test-only** provider that returns caller-chosen response headers
    /// from the client-resolve path, so a test can drive one provider response
    /// effect at a time through this endpoint, with and without an identifier.
    #[derive(Debug)]
    struct ResolveHeaderProvider {
        headers: &'static [(&'static str, &'static str)],
        mint: bool,
    }

    impl EdgeCookieProvider for ResolveHeaderProvider {
        fn id(&self) -> &'static str {
            "resolve-header"
        }

        fn code(&self) -> crate::ec::provider::ProviderCode {
            crate::provider_code!("t0rh")
        }

        fn generate(
            &self,
            _request_info: &dyn RequestInfo,
            _input: &IdentityInput<'_>,
        ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>> {
            Ok(GeneratedEdgeCookie::default())
        }

        fn resolve_from_client(
            &self,
            _input: &ClientResolveInput<'_>,
        ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>> {
            Ok(GeneratedEdgeCookie {
                id: self.mint.then(|| HEADER_PROVIDER_ID.to_owned()),
                response_headers: self
                    .headers
                    .iter()
                    .map(|(name, value)| {
                        (
                            http::HeaderName::from_bytes(name.as_bytes())
                                .expect("should parse header name"),
                            HeaderValue::from_static(value),
                        )
                    })
                    .collect(),
            })
        }

        fn accepts_id(&self, value: &str) -> bool {
            !value.is_empty()
        }

        fn normalize_id_for_kv(&self, value: &str) -> String {
            value.to_owned()
        }
    }

    /// Drives one resolve request through [`ResolveHeaderProvider`], returning
    /// whatever the handler produced.
    fn resolve_with_header_provider(
        headers: &'static [(&'static str, &'static str)],
        mint: bool,
        graph: Option<&crate::ec::kv::KvIdentityGraph>,
    ) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
        let mut settings = create_test_settings();
        settings.ec.provider = Some(EcProviderSelection::from("resolve-header"));
        let services =
            noop_services_with_ec_provider(Arc::new(ResolveHeaderProvider { headers, mint }));
        let organic = Request::builder()
            .method(Method::GET)
            .uri("https://edge.example.com/")
            .body(EdgeBody::empty())
            .expect("should build organic request");
        let ec = EcContext::read_from_request(&settings, &organic, &services)
            .expect("should read EC context");
        handle_ec_resolve(&settings, post(HEADER_PROVIDER_ID), &ec, graph)
    }

    #[test]
    fn resolve_rejects_a_reserved_response_effect_when_nothing_is_minted() {
        // The 204 path applied provider headers with no check at all, so a
        // browser-side provider could set the managed identity cookie while
        // returning no identifier, walking past the identifier bounds, the
        // conflict check and the row-before-cookie rule below it.
        let outcome = resolve_with_header_provider(
            &[("set-cookie", "ts-ec=forged-value; Path=/")],
            false,
            None,
        );

        let err = outcome.expect_err("a managed cookie effect should fail the request");
        assert!(
            format!("{err:?}").contains("ts-` namespace"),
            "the failure should name the reserved effect, got {err:?}"
        );
    }

    #[test]
    fn resolve_rejects_a_reserved_response_effect_on_the_minted_path() {
        // The same check has to cover the 200 path, where the provider does
        // create and core is about to write its own cookie and headers.
        let graph = in_memory_graph();
        let outcome =
            resolve_with_header_provider(&[("x-ts-ec", "forged-value")], true, Some(&graph));

        let err = outcome.expect_err("a reserved header effect should fail the request");
        assert!(
            format!("{err:?}").contains("x-ts-` namespace"),
            "the failure should name the reserved effect, got {err:?}"
        );
    }

    #[test]
    fn resolve_accumulates_provider_response_headers_with_its_own() {
        // The provider's own cookies must add to what the handler already set,
        // never replace it. Replacing collapsed a provider's own cookie list to
        // whichever came last, and would drop the `Cache-Control: no-store` core
        // writes onto every identity response (a provider cannot set
        // `cache-control` itself, so core's directive is what has to survive).
        let graph = in_memory_graph();
        let response = resolve_with_header_provider(
            &[
                ("set-cookie", "vendor-ev=abc; Path=/"),
                ("set-cookie", "vendor-state=xyz; Path=/"),
            ],
            true,
            Some(&graph),
        )
        .expect("should handle resolve");

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "a created identifier should return 200"
        );

        let cookies: Vec<&str> = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|value| value.to_str().expect("should render set-cookie as utf-8"))
            .collect();
        for expected in ["vendor-ev=abc", "vendor-state=xyz", "ts-ec=", "ts-ecr=1"] {
            assert!(
                cookies.iter().any(|cookie| cookie.starts_with(expected)),
                "`{expected}` should survive on the response, got {cookies:?}"
            );
        }

        let cache_control: Vec<&str> = response
            .headers()
            .get_all(header::CACHE_CONTROL)
            .iter()
            .map(|value| {
                value
                    .to_str()
                    .expect("should render cache-control as utf-8")
            })
            .collect();
        assert!(
            cache_control.contains(&"no-store"),
            "a provider header must not drop the no-store an identity response carries, got {cache_control:?}"
        );
    }

    #[test]
    fn client_set_value_round_trips_through_the_ec_scenario() {
        // A client-generated first-party UUID, the value the browser posts back.
        const TEST_ID: &str = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";

        let provider: Arc<dyn EdgeCookieProvider> = Arc::new(TestIdProvider);
        let mut settings = create_test_settings();
        settings.ec.provider = Some(EcProviderSelection::from("testid"));
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
            "a client-set provider defers creation to the browser"
        );

        // 2. The page generates its identifier and posts it to the resolve
        //    endpoint; the server persists the identity-graph row and sets the
        //    value as the EC cookie.
        const CODED_TEST_ID: &str = "t0id~3f2504e0-4f89-41d3-9a0c-0305e82c3301";
        let graph = in_memory_graph();
        let response = handle_ec_resolve(&settings, post(TEST_ID), &ec, Some(&graph))
            .expect("should handle resolve");
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
            set_cookie.contains(CODED_TEST_ID),
            "the EC cookie should carry the coded client-set identifier, got {set_cookie}"
        );
        assert!(
            graph
                .get(CODED_TEST_ID)
                .expect("should read the graph")
                .is_some(),
            "the resolve should persist the identity-graph row, so withdrawal can reach it"
        );

        // 3. A later request carries the EC cookie; the server reads the
        //    identifier back verbatim. This is the step the built-in shape check
        //    used to drop.
        let ret = Request::builder()
            .method(Method::GET)
            .uri("https://edge.example.com/")
            .header("cookie", format!("ts-ec={CODED_TEST_ID}"))
            .body(EdgeBody::empty())
            .expect("should build return request");
        let ec2 = EcContext::read_from_request(&settings, &ret, &services)
            .expect("should read EC context");
        assert_eq!(
            ec2.ec_value(),
            Some(CODED_TEST_ID),
            "the client-set identifier should round-trip as the coded EC value"
        );
    }

    #[test]
    fn resolve_sets_cookie_marker_and_no_store_when_word_matches_and_allowed() {
        let settings = settings_with_client_fixed();
        let graph = in_memory_graph();
        let response = handle_ec_resolve(&settings, post(FIXED_WORD), &gated(true), Some(&graph))
            .expect("should handle resolve");

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "a verified value should return 200"
        );
        let cookies: Vec<String> = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|value| value.to_str().expect("should be utf-8").to_owned())
            .collect();
        let ec_cookie = cookies
            .iter()
            .find(|cookie| cookie.starts_with("ts-ec="))
            .expect("should set the EC cookie");
        assert!(
            ec_cookie.contains("cfix~an-ec"),
            "should set the coded verified word as the EC cookie, got {ec_cookie}"
        );
        assert!(
            ec_cookie.contains("HttpOnly"),
            "the EC cookie should be HttpOnly"
        );
        assert!(
            ec_cookie.contains("Secure"),
            "the EC cookie should be Secure"
        );
        let marker = cookies
            .iter()
            .find(|cookie| cookie.starts_with("ts-ecr=1"))
            .expect("should set the resolved marker cookie");
        assert!(
            !marker.contains("HttpOnly"),
            "the marker must be readable by the page script, so not HttpOnly"
        );
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store"),
            "identity responses must never be cached"
        );
        assert!(
            graph
                .get("cfix~an-ec")
                .expect("should read the graph")
                .is_some(),
            "the resolve should persist the identity-graph row under the coded key"
        );
    }

    #[test]
    fn resolve_returns_204_when_not_allowed() {
        let settings = settings_with_client_fixed();
        let graph = in_memory_graph();
        let response = handle_ec_resolve(&settings, post(FIXED_WORD), &gated(false), Some(&graph))
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
        let graph = in_memory_graph();
        let response = handle_ec_resolve(&settings, post("123"), &gated(true), Some(&graph))
            .expect("should handle resolve");

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
        let graph = in_memory_graph();
        let response = handle_ec_resolve(&settings, post(&big), &gated(true), Some(&graph))
            .expect("should handle resolve");

        assert_eq!(
            response.status(),
            StatusCode::PAYLOAD_TOO_LARGE,
            "an oversized body should be rejected with 413"
        );
    }

    #[test]
    fn resolve_rejects_a_missing_or_foreign_origin() {
        let settings = settings_with_client_fixed();
        let graph = in_memory_graph();
        for origin in [None, Some("https://attacker.example")] {
            let request = post_with(origin, Some("text/plain"), FIXED_WORD);
            let response = handle_ec_resolve(&settings, request, &gated(true), Some(&graph))
                .expect("should handle resolve");
            assert_eq!(
                response.status(),
                StatusCode::FORBIDDEN,
                "an identity-setting POST must come from the publisher's own site"
            );
            assert!(
                response.headers().get(header::SET_COOKIE).is_none(),
                "a rejected origin must set no cookie"
            );
        }
    }

    #[test]
    fn resolve_accepts_a_publisher_subdomain_origin() {
        let settings = settings_with_client_fixed();
        let graph = in_memory_graph();
        let request = post_with(
            Some("https://www.test-publisher.com"),
            Some("text/plain"),
            FIXED_WORD,
        );
        let response = handle_ec_resolve(&settings, request, &gated(true), Some(&graph))
            .expect("should handle resolve");
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "a subdomain of the publisher should be accepted"
        );
    }

    #[test]
    fn resolve_rejects_an_unexpected_content_type() {
        let settings = settings_with_client_fixed();
        let graph = in_memory_graph();
        let request = post_with(
            Some("https://test-publisher.com"),
            Some("application/x-www-form-urlencoded"),
            FIXED_WORD,
        );
        let response = handle_ec_resolve(&settings, request, &gated(true), Some(&graph))
            .expect("should handle resolve");
        assert_eq!(
            response.status(),
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "only text/plain and application/json bodies are resolve payloads"
        );
    }

    #[test]
    fn resolve_conflicts_when_a_different_identity_already_exists() {
        let settings = settings_with_client_fixed();
        let graph = in_memory_graph();
        let existing = format!("{}.ABC123", "e".repeat(64));
        let ec_context =
            EcContext::new_for_test_gated(Some(existing), ConsentContext::default(), true);
        let response = handle_ec_resolve(&settings, post(FIXED_WORD), &ec_context, Some(&graph))
            .expect("should handle resolve");
        assert_eq!(
            response.status(),
            StatusCode::CONFLICT,
            "a resolve must not silently replace an existing identity"
        );
        assert!(
            response.headers().get(header::SET_COOKIE).is_none(),
            "a conflict must set no cookie"
        );
    }

    #[test]
    fn resolve_mints_nothing_without_an_identity_graph() {
        let settings = settings_with_client_fixed();
        let response = handle_ec_resolve(&settings, post(FIXED_WORD), &gated(true), None)
            .expect("should handle resolve");
        assert_eq!(
            response.status(),
            StatusCode::NO_CONTENT,
            "with no graph there is no row to persist, so nothing is created"
        );
        assert!(
            response.headers().get(header::SET_COOKIE).is_none(),
            "a cookie without a graph row would be a phantom identity"
        );
    }

    #[test]
    fn resolve_sets_no_cookie_for_unmatched_word() {
        let settings = settings_with_client_fixed();
        let graph = in_memory_graph();
        let response =
            handle_ec_resolve(&settings, post("not-the-word"), &gated(true), Some(&graph))
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
