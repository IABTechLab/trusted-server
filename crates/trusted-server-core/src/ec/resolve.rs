//! Client-cycle Edge Cookie resolution endpoint (`POST /_ts/api/v1/ec/resolve`).
//!
//! A client-side Edge Cookie module defers on the organic page request
//! (deriving no identifier at the edge) and lets the page do the work in the
//! browser. When the page has its result it posts the value here, and this
//! endpoint hands it to the configured module's
//! [`resolve_from_client`](super::module::EdgeCookieModule::resolve_from_client)
//! to create the Edge Cookie.
//!
//! The endpoint is module-agnostic: it bounds the body, gates on the
//! permission model (the same gate as organic generation), calls the module,
//! and sets the cookie on its own response so the value is live for every
//! subsequent first-party request. Whether the posted value is trustworthy is
//! the module's responsibility. The payload arrives from the browser, so a
//! real module verifies it (for example an OWID signature) before creating one.

use edgezero_core::body::Body as EdgeBody;
use error_stack::Report;
use http::{HeaderValue, Request, Response, StatusCode, header};

use crate::error::TrustedServerError;
use crate::settings::Settings;

use super::EcContext;
use super::cookies::{ec_id_has_only_allowed_chars, set_ec_cookie, set_resolved_marker_cookie};
use super::kv::KvIdentityGraph;
use super::kv_types::KvEntry;
use super::module::{ClientResolveInput, apply_module_response_headers};

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
/// `text/plain` or `application/json` body. Gates on the configured module's
/// required permissions, then asks the module to create an Edge Cookie from
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
/// content type, `413` for an oversized body, `400` when the module creates an
/// identifier outside the identifier bounds, `409` when the request already
/// carries a different identity (a resolve must not silently replace one), and
/// `503` when the identity-graph write fails. When the permission gate is
/// closed, no module is configured, no graph is available, or the module
/// creates nothing, the response is `204` with no cookie. Every response this
/// handler builds carries `Cache-Control: no-store`; a module or
/// configuration error propagates to the adapter's error response instead,
/// and so does a module asking for a response header inside core's reserved
/// surface, which is a broken module contract rather than a bad request.
///
/// # Errors
///
/// Returns [`TrustedServerError`] when the module fails to process the
/// payload, or asks for a response header inside core's reserved surface
/// (see
/// [`reserved_response_effect`](crate::ec::module::reserved_response_effect)).
/// A payload that is merely unverified or absent yields a `204` rather than an
/// error.
pub async fn handle_ec_resolve(
    settings: &Settings,
    req: Request<EdgeBody>,
    ec_context: &EcContext,
    kv: Option<&KvIdentityGraph>,
    services: &crate::platform::RuntimeServices,
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

    // Gate: the configured module's required permissions must be set for
    // this request, the same gate as the organic generation path. A
    // client-driven resolve does not bypass the permission model.
    if !ec_context.ec_allowed() {
        log::info!("EC resolve skipped: required permissions not set");
        return Ok(status_only(StatusCode::NO_CONTENT));
    }

    // The module this request resolved when its context was read, so the
    // endpoint answers with the instance the page's own request selected. The
    // client value is verified from the posted body below, not from request info.
    let Some(module) = ec_context.selected_module() else {
        log::info!("EC resolve skipped: no Edge Cookie module configured");
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

    let generated = module.resolve_from_client(&input, services).await?;
    log::debug!(
        "EC resolve handled (module={}): id {}",
        module.id(),
        if generated.id.is_some() {
            "created"
        } else {
            "not created"
        },
    );

    // Check every response header the module asked for against core's
    // reserved surface, exactly as the organic generation path does in
    // `EcContext::generate_with_module`. A module may set its own cookies
    // and headers, but not a managed `ts-` cookie, a header in the `x-ts-`
    // namespace, or a framing or hop-by-hop header. Without this a
    // browser-side module could set `ts-ec` itself and walk straight past
    // the identifier bounds, the conflict check and the row-before-cookie
    // rule below. The check sits before the identifier is read because a
    // module can return headers with no identifier at all, which is the
    // 204 path, and that path applies headers too.
    for (name, value) in &generated.response_headers {
        if let Some(effect) = super::module::reserved_response_effect(name, value) {
            return Err(Report::new(TrustedServerError::EdgeCookie {
                message: format!(
                    "Module `{}` returned a response header `{name}` that {effect}",
                    module.id(),
                ),
            }));
        }
    }

    let generated_id = generated
        .id
        .map(|value| super::module::apply_module_code(module.as_ref(), &value));
    let Some(ec_id) = generated_id else {
        let mut response = status_only(StatusCode::NO_CONTENT);
        apply_module_response_headers(response.headers_mut(), generated.response_headers);
        return Ok(response);
    };

    // The same identifier bounds as the organic generation path: reject, never
    // rewrite. A module that created an out-of-bounds identifier is a bad
    // request from the client's perspective, because the posted payload
    // produced an unusable identity.
    if !ec_id_has_only_allowed_chars(&ec_id) {
        log::error!(
            "EC resolve rejected: module `{}` created an identifier outside the bounds",
            module.id(),
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
            "EC resolve rejected: request already carries a different identity (module={})",
            module.id(),
        );
        return Ok(status_only(StatusCode::CONFLICT));
    }

    // Persist the identity-graph row before setting the cookie, keyed by the
    // module's canonical form, exactly like the organic generation path.
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
    let kv_key = super::module::module_kv_key(module.as_ref(), &ec_id);
    if let Err(err) = graph.create_or_revive(&kv_key, &entry) {
        log::error!("EC resolve failed to write the identity-graph row: {err:?}");
        return Ok(status_only(StatusCode::SERVICE_UNAVAILABLE));
    }

    let mut response = status_only(StatusCode::OK);

    // Apply any response headers the module asked for (for example to request
    // more client evidence on a later request). Empty for the demo module.
    // They accumulate with what this handler already set rather than replacing
    // it, for the reasons on `module::apply_module_response_headers`; here
    // that keeps the `Cache-Control: no-store` every identity response must
    // carry, which a replacing write would drop.
    apply_module_response_headers(response.headers_mut(), generated.response_headers);

    set_ec_cookie(settings, &mut response, &ec_id);
    // The Edge Cookie is HttpOnly, so the page script cannot see it; the
    // non-HttpOnly marker tells the script the resolve succeeded so it does
    // not post again on every page view.
    set_resolved_marker_cookie(settings, &mut response);

    Ok(response)
}

/// Whether the request's `Origin` is one this deployment authorizes to set
/// identity.
///
/// The comparison is the same-origin test of RFC 6454 §5, so two origins
/// match only when their scheme, host and port triples (RFC 6454 §4) are
/// equal, with a missing port standing for the scheme's default. See
/// [`origins_match`]. `https://{publisher.domain}` is always accepted and
/// `[ec] resolve_allowed_origins` adds further origins. A subdomain of the publisher is not accepted unless it is listed,
/// because the Edge Cookie is scoped to the parent domain, so a delegated or
/// compromised sibling host would otherwise be able to fix an identity that
/// lands on the apex and every sibling with it.
///
/// This is defense in depth rather than the primary control. The primary
/// control is the module's own verification of the value it is handed.
fn origin_is_publisher(req: &Request<EdgeBody>, settings: &Settings) -> bool {
    let Some(origin) = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let origin = origin.trim();

    let default_origin = format!("https://{}", settings.publisher.domain);
    if origins_match(origin, &default_origin) {
        return true;
    }
    settings
        .ec
        .resolve_allowed_origins
        .iter()
        .any(|allowed| origins_match(origin, allowed))
}

/// Whether two serialized origins are the same origin under RFC 6454.
///
/// Each side is read by [`parse_serialized_origin`], so a value that is not a
/// serialized origin, such as one carrying a path, a query or a fragment,
/// never matches. The two are then compared as the scheme, host and port
/// triple of RFC 6454 §4, which is the same-origin test of §5. The scheme and
/// host are case-insensitive and a missing port stands for the scheme's
/// default, so a configured `https://www.example.com:443` matches the
/// `https://www.example.com` a browser sends, while `http://` or another port
/// never matches. The opaque `null` origin (RFC 6454 §6.2) is never the same
/// as anything.
fn origins_match(candidate: &str, allowed: &str) -> bool {
    match (
        parse_serialized_origin(candidate),
        parse_serialized_origin(allowed),
    ) {
        (Ok(candidate), Ok(allowed)) => candidate == allowed,
        _ => false,
    }
}

/// Why a value is not a serialized origin.
///
/// Each reason reads as the end of a sentence about the value, so a message
/// can name the value and then say what is wrong with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, derive_more::Display)]
pub(crate) enum NotAnOrigin {
    /// The value is empty.
    #[display("is empty")]
    Empty,
    /// The value has no scheme, another scheme, or is the opaque `null`.
    #[display("does not begin with `http://` or `https://`")]
    Scheme,
    /// A user name or password comes before the host.
    #[display("carries a user name or password")]
    Userinfo,
    /// Something follows the host and port, a lone trailing slash included.
    #[display("has a path, and a trailing slash is one")]
    Path,
    /// A query follows the host and port.
    #[display("has a query")]
    Query,
    /// A fragment follows the host and port.
    #[display("has a fragment")]
    Fragment,
    /// What follows the scheme is not a host with an optional port.
    #[display("does not name a host with an optional port")]
    Host,
}

impl core::error::Error for NotAnOrigin {}

/// Parses a serialized origin (RFC 6454 §6.1) into its scheme, host and port
/// triple (RFC 6454 §4).
///
/// A serialized origin is `http://` or `https://` followed by a host and an
/// optional port, and nothing else. The scheme and host are case-insensitive
/// and a missing port stands for the scheme's default. The resolve endpoint
/// reads both the request's `Origin` and each
/// `[ec] resolve_allowed_origins` entry with this parse, and the
/// settings refuse an entry it rejects, so an entry that loads is one a
/// request can match.
///
/// # Errors
///
/// Returns the [`NotAnOrigin`] reason for any other value.
pub(crate) fn parse_serialized_origin(value: &str) -> Result<url::Origin, NotAnOrigin> {
    if value.is_empty() {
        return Err(NotAnOrigin::Empty);
    }
    let authority = ["https://", "http://"]
        .into_iter()
        .find_map(|prefix| {
            let (scheme, rest) = value.split_at_checked(prefix.len())?;
            scheme.eq_ignore_ascii_case(prefix).then_some(rest)
        })
        .ok_or(NotAnOrigin::Scheme)?;
    // The host and port end at the first of these, and an origin has nothing
    // after them. The URL parser reads a backslash as a slash in these schemes.
    match authority
        .chars()
        .find(|c| matches!(c, '/' | '\\' | '?' | '#' | '@'))
    {
        Some('@') => return Err(NotAnOrigin::Userinfo),
        Some('?') => return Err(NotAnOrigin::Query),
        Some('#') => return Err(NotAnOrigin::Fragment),
        Some(_) => return Err(NotAnOrigin::Path),
        None => {}
    }
    // The URL parser drops surrounding spaces and any tab or newline, so they
    // are refused here rather than silently read as a different value.
    if authority
        .chars()
        .any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(NotAnOrigin::Host);
    }
    let url = url::Url::parse(value).map_err(|_| NotAnOrigin::Host)?;
    Ok(url.origin())
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
    use crate::ec::module::{
        CLIENT_FIXED_MODULE_KEY, EcModuleSelection, EdgeCookieModule, GeneratedEdgeCookie,
        IdentityInput,
    };
    use crate::evidence::RequestInfo;
    use crate::platform::test_support::{noop_services, noop_services_with_ec_module};
    use crate::test_support::tests::create_test_settings;
    use http::Method;
    use std::sync::Arc;

    fn settings_with_client_fixed() -> Settings {
        let mut settings = create_test_settings();
        settings.ec.module = Some(EcModuleSelection::from(CLIENT_FIXED_MODULE_KEY));
        settings
    }

    // The fixed word shared by the `client_fixed` module and its page script.
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

    /// A context whose gate is set by hand, carrying the module the settings
    /// select, resolved as a request's own context resolves it.
    fn gated(settings: &Settings, ec_allowed: bool) -> EcContext {
        with_selected_module(
            settings,
            EcContext::new_for_test_gated(None, ConsentContext::default(), ec_allowed),
        )
    }

    fn with_selected_module(settings: &Settings, context: EcContext) -> EcContext {
        match crate::ec::module::request_module(&settings.ec, &noop_services())
            .expect("should resolve the selected module")
        {
            Some(module) => context.with_module_for_test(module),
            None => context,
        }
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

    /// A **test-only** module modeling a client-generated, first-party
    /// identifier (a `UUID`) that the browser creates and posts back for the server
    /// to set. It is not a production module and exists only to exercise the
    /// client-set Edge Cookie value path from end to end. The edge defers, the
    /// page posts a value, and it must round-trip as the cookie and the KV key.
    ///
    /// A `UUID` has no separator, so the built-in [`normalize_id_for_kv`] default
    /// would append a trailing dot and corrupt it, which is why this module
    /// (like any opaque-identifier module) returns the value unchanged.
    ///
    /// [`normalize_id_for_kv`]: EdgeCookieModule::normalize_id_for_kv
    #[derive(Debug)]
    struct TestIdModule;

    #[async_trait::async_trait(?Send)]
    impl EdgeCookieModule for TestIdModule {
        fn id(&self) -> &'static str {
            "testid"
        }

        fn code(&self) -> crate::ec::module::ModuleCode {
            crate::module_code!("t0id")
        }

        async fn generate(
            &self,
            _request_info: &dyn RequestInfo,
            _input: &IdentityInput<'_>,
            _services: &crate::platform::RuntimeServices,
        ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>> {
            // The identifier is created in the browser, so the edge derives nothing.
            Ok(GeneratedEdgeCookie::default())
        }

        async fn resolve_from_client(
            &self,
            input: &ClientResolveInput<'_>,
            _services: &crate::platform::RuntimeServices,
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

    /// The identifier [`ResolveHeaderModule`] creates when asked to.
    const HEADER_MODULE_ID: &str = "5c3a1b70-2f4d-4a19-9c6e-7b0d18e4a221";

    /// A **test-only** module that returns caller-chosen response headers
    /// and identifier from the client-resolve path, so a test can drive one
    /// module response effect at a time through this endpoint, with and
    /// without an identifier.
    #[derive(Debug)]
    struct ResolveHeaderModule {
        headers: &'static [(&'static str, &'static str)],
        id: Option<&'static str>,
    }

    #[async_trait::async_trait(?Send)]
    impl EdgeCookieModule for ResolveHeaderModule {
        fn id(&self) -> &'static str {
            "resolve-header"
        }

        fn code(&self) -> crate::ec::module::ModuleCode {
            crate::module_code!("t0rh")
        }

        async fn generate(
            &self,
            _request_info: &dyn RequestInfo,
            _input: &IdentityInput<'_>,
            _services: &crate::platform::RuntimeServices,
        ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>> {
            Ok(GeneratedEdgeCookie::default())
        }

        async fn resolve_from_client(
            &self,
            _input: &ClientResolveInput<'_>,
            _services: &crate::platform::RuntimeServices,
        ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>> {
            Ok(GeneratedEdgeCookie {
                id: self.id.map(str::to_owned),
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

    /// Drives one resolve request through [`ResolveHeaderModule`], returning
    /// whatever the handler produced.
    async fn resolve_with_header_module(
        headers: &'static [(&'static str, &'static str)],
        id: Option<&'static str>,
        graph: Option<&crate::ec::kv::KvIdentityGraph>,
    ) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
        resolve_request_with_header_module(headers, id, post(HEADER_MODULE_ID), graph).await
    }

    /// Drives `request` through [`ResolveHeaderModule`], threaded the way a
    /// composition root threads a module.
    async fn resolve_request_with_header_module(
        headers: &'static [(&'static str, &'static str)],
        id: Option<&'static str>,
        request: Request<EdgeBody>,
        graph: Option<&crate::ec::kv::KvIdentityGraph>,
    ) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
        let mut settings = create_test_settings();
        settings.ec.module = Some(EcModuleSelection::from("resolve-header"));
        let services = noop_services_with_ec_module(Arc::new(ResolveHeaderModule { headers, id }));
        let organic = Request::builder()
            .method(Method::GET)
            .uri("https://edge.example.com/")
            .body(EdgeBody::empty())
            .expect("should build organic request");
        let ec = EcContext::read_from_request(&settings, &organic, &services)
            .expect("should read EC context");
        handle_ec_resolve(&settings, request, &ec, graph, &services).await
    }

    #[tokio::test]
    async fn resolve_rejects_a_reserved_response_effect_when_nothing_is_created() {
        // The 204 path applies module headers too, so without the check a
        // browser-side module could set the managed identity cookie while
        // returning no identifier, walking past the identifier bounds, the
        // conflict check and the row-before-cookie rule.
        let outcome =
            resolve_with_header_module(&[("set-cookie", "ts-ec=forged-value; Path=/")], None, None)
                .await;

        let err = outcome.expect_err("a managed cookie effect should fail the request");
        assert!(
            format!("{err:?}").contains("ts-` namespace"),
            "the failure should name the reserved effect, got {err:?}"
        );
    }

    #[tokio::test]
    async fn resolve_rejects_a_reserved_response_effect_on_the_created_path() {
        // The same check has to cover the 200 path, where the module does
        // create and core is about to write its own cookie and headers.
        let graph = in_memory_graph();
        let outcome = resolve_with_header_module(
            &[("x-ts-ec", "forged-value")],
            Some(HEADER_MODULE_ID),
            Some(&graph),
        )
        .await;

        let err = outcome.expect_err("a reserved header effect should fail the request");
        assert!(
            format!("{err:?}").contains("x-ts-` namespace"),
            "the failure should name the reserved effect, got {err:?}"
        );
    }

    #[tokio::test]
    async fn resolve_accumulates_module_response_headers_with_its_own() {
        // The module's own cookies must add to what the handler already set,
        // never replace it. Replacing would keep only the last of a module's
        // own cookies, and would drop the `Cache-Control: no-store` core writes
        // onto every identity response (a module cannot set `cache-control`
        // itself, so core's directive is what has to survive).
        let graph = in_memory_graph();
        let response = resolve_with_header_module(
            &[
                ("set-cookie", "vendor-ev=abc; Path=/"),
                ("set-cookie", "vendor-state=xyz; Path=/"),
            ],
            Some(HEADER_MODULE_ID),
            Some(&graph),
        )
        .await
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
            "a module header must not drop the no-store an identity response carries, got {cache_control:?}"
        );
    }

    #[tokio::test]
    async fn resolve_rejects_an_identifier_outside_the_cookie_bounds() {
        // An identifier the cookie cannot carry is refused, never rewritten,
        // and leaves neither a cookie nor an identity-graph row behind.
        let graph = in_memory_graph();
        let response = resolve_with_header_module(&[], Some("has a space"), Some(&graph))
            .await
            .expect("should handle resolve");
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "an identifier outside the cookie-safe bounds should be refused with 400"
        );
        assert!(
            response.headers().get(header::SET_COOKIE).is_none(),
            "a refused identifier must set no cookie"
        );
        assert!(
            graph
                .get("t0rh~has a space")
                .expect("should read the graph")
                .is_none(),
            "a refused identifier must leave no identity-graph row"
        );
    }

    #[tokio::test]
    async fn resolve_answers_503_when_the_identity_graph_write_fails() {
        // The row is written before the cookie, so when the write fails no
        // cookie is set, because withdrawal could not reach an identity with
        // no row.
        let graph = crate::ec::kv::KvIdentityGraph::failing("test-ec-store");
        let response = resolve_with_header_module(&[], Some(HEADER_MODULE_ID), Some(&graph))
            .await
            .expect("should handle resolve");
        assert_eq!(
            response.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "a failed identity-graph write should answer 503"
        );
        assert!(
            response.headers().get(header::SET_COOKIE).is_none(),
            "a failed identity-graph write must set no cookie"
        );
    }

    #[tokio::test]
    async fn resolve_refuses_an_advertised_oversized_body_before_reading_it() {
        // A `Content-Length` over the limit is refused before the body is read,
        // whatever the body turns out to hold.
        let mut request = post(HEADER_MODULE_ID);
        request
            .headers_mut()
            .insert(header::CONTENT_LENGTH, HeaderValue::from(MAX_BODY_SIZE + 1));
        let graph = in_memory_graph();
        let response =
            resolve_request_with_header_module(&[], Some(HEADER_MODULE_ID), request, Some(&graph))
                .await
                .expect("should handle resolve");
        assert_eq!(
            response.status(),
            StatusCode::PAYLOAD_TOO_LARGE,
            "an advertised length over the limit should be refused with 413"
        );
        assert!(
            response.headers().get(header::SET_COOKIE).is_none(),
            "a refused body must set no cookie"
        );
    }

    #[tokio::test]
    async fn client_set_value_round_trips_through_the_ec_scenario() {
        // A client-generated first-party UUID, the value the browser posts back.
        const TEST_ID: &str = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";

        let module: Arc<dyn EdgeCookieModule> = Arc::new(TestIdModule);
        let mut settings = create_test_settings();
        settings.ec.module = Some(EcModuleSelection::from("testid"));
        let services = noop_services_with_ec_module(Arc::clone(&module));

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
        ec.generate_if_needed(&settings, None, &services)
            .await
            .expect("should run generation");
        assert!(
            ec.ec_value().is_none(),
            "a client-set module defers creation to the browser"
        );

        // 2. The page generates its identifier and posts it to the resolve
        //    endpoint; the server persists the identity-graph row and sets the
        //    value as the EC cookie.
        const CODED_TEST_ID: &str = "t0id~3f2504e0-4f89-41d3-9a0c-0305e82c3301";
        let graph = in_memory_graph();
        let response = handle_ec_resolve(&settings, post(TEST_ID), &ec, Some(&graph), &services)
            .await
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

        // 3. A later request carries the EC cookie, and the identifier reads
        //    back verbatim because the module's own `accepts_id` decides its
        //    shape.
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

    #[tokio::test]
    async fn resolve_sets_cookie_marker_and_no_store_when_word_matches_and_allowed() {
        let settings = settings_with_client_fixed();
        let graph = in_memory_graph();
        let response = handle_ec_resolve(
            &settings,
            post(FIXED_WORD),
            &gated(&settings, true),
            Some(&graph),
            &noop_services(),
        )
        .await
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

    /// A resolve that creates nothing answers 204 and sets no cookie, whatever
    /// stopped it.
    #[tokio::test]
    async fn resolve_answers_204_with_no_cookie_when_it_creates_nothing() {
        let mut no_module = create_test_settings();
        no_module.ec.module = None;
        for (case, settings, body, allowed, has_graph) in [
            (
                "the permission gate is closed",
                settings_with_client_fixed(),
                FIXED_WORD,
                false,
                true,
            ),
            ("no module is configured", no_module, "123", true, true),
            (
                "there is no identity graph to hold the row, so a cookie would be a \
                 phantom identity",
                settings_with_client_fixed(),
                FIXED_WORD,
                true,
                false,
            ),
            (
                "the posted value fails verification",
                settings_with_client_fixed(),
                "not-the-word",
                true,
                true,
            ),
        ] {
            let graph = in_memory_graph();
            let response = handle_ec_resolve(
                &settings,
                post(body),
                &gated(&settings, allowed),
                has_graph.then_some(&graph),
                &noop_services(),
            )
            .await
            .expect("should handle resolve");

            assert_eq!(response.status(), StatusCode::NO_CONTENT, "{case}");
            assert!(
                response.headers().get(header::SET_COOKIE).is_none(),
                "{case}: should set no cookie"
            );
        }
    }

    /// A context read the way a composition root builds one, with the selected
    /// built-in module threaded through the services, at a location whose
    /// rules set storage without a signal so the gate is open.
    async fn resolve_with_threaded_module(settings: &Settings, body: &str) -> Response<EdgeBody> {
        let module = crate::ec::module::build_reusable_module(&settings.ec, None, None)
            .expect("should build the selected module")
            .expect("a built-in module is reusable");
        let services = noop_services_with_ec_module(module);
        let organic = Request::builder()
            .method(Method::GET)
            .uri("https://test-publisher.com/")
            .body(EdgeBody::empty())
            .expect("should build organic request");
        let geo = crate::platform::GeoInfo {
            city: String::new(),
            country: "US".to_owned(),
            continent: String::new(),
            latitude: 0.0,
            longitude: 0.0,
            metro_code: 0,
            region: Some("CA".to_owned()),
            asn: None,
        };
        let ec = EcContext::read_from_request_with_geo(settings, &organic, &services, Some(&geo))
            .expect("should read EC context");
        assert!(
            ec.ec_allowed(),
            "the gate must be open for this to test anything"
        );

        handle_ec_resolve(
            settings,
            post(body),
            &ec,
            Some(&in_memory_graph()),
            &services,
        )
        .await
        .expect("a valid selection should not be an error")
    }

    #[tokio::test]
    async fn resolve_answers_204_for_a_threaded_module_that_creates_nothing() {
        let response = resolve_with_threaded_module(&create_test_settings(), FIXED_WORD).await;

        assert_eq!(
            response.status(),
            StatusCode::NO_CONTENT,
            "a module that creates nothing from a client post should answer 204"
        );
    }

    #[tokio::test]
    async fn resolve_sets_the_cookie_for_a_threaded_client_fixed_module() {
        let response =
            resolve_with_threaded_module(&settings_with_client_fixed(), FIXED_WORD).await;

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "the fixed word should create the Edge Cookie"
        );
        assert!(
            response
                .headers()
                .get_all(header::SET_COOKIE)
                .iter()
                .any(|value| value
                    .to_str()
                    .is_ok_and(|cookie| cookie.starts_with("ts-ec="))),
            "should set the Edge Cookie"
        );
    }

    /// A request the endpoint cannot take is refused, and sets no cookie.
    #[tokio::test]
    async fn resolve_refuses_a_request_it_cannot_take() {
        let settings = settings_with_client_fixed();
        let oversized = "x".repeat(MAX_BODY_SIZE + 1);
        let own = Some("https://test-publisher.com");
        let text = Some("text/plain");
        for (case, origin, content_type, body, status) in [
            (
                "no Origin, because identity is set only from the publisher's own site",
                None,
                text,
                FIXED_WORD,
                StatusCode::FORBIDDEN,
            ),
            (
                "a foreign origin",
                Some("https://attacker.example"),
                text,
                FIXED_WORD,
                StatusCode::FORBIDDEN,
            ),
            (
                "a sibling subdomain, which would set identity that lands on the apex",
                Some("https://www.test-publisher.com"),
                text,
                FIXED_WORD,
                StatusCode::FORBIDDEN,
            ),
            (
                "plain http, because the scheme is part of the origin",
                Some("http://test-publisher.com"),
                text,
                FIXED_WORD,
                StatusCode::FORBIDDEN,
            ),
            (
                "a port, because the port is part of the origin",
                Some("https://test-publisher.com:8443"),
                text,
                FIXED_WORD,
                StatusCode::FORBIDDEN,
            ),
            (
                "a form body, because only text and JSON are resolve payloads",
                own,
                Some("application/x-www-form-urlencoded"),
                FIXED_WORD,
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
            ),
            (
                "a body over the limit",
                own,
                text,
                oversized.as_str(),
                StatusCode::PAYLOAD_TOO_LARGE,
            ),
        ] {
            let graph = in_memory_graph();
            let response = handle_ec_resolve(
                &settings,
                post_with(origin, content_type, body),
                &gated(&settings, true),
                Some(&graph),
                &noop_services(),
            )
            .await
            .expect("should handle resolve");

            assert_eq!(response.status(), status, "{case}");
            assert!(
                response.headers().get(header::SET_COOKIE).is_none(),
                "{case}: should set no cookie"
            );
        }
    }

    #[tokio::test]
    async fn resolve_accepts_a_configured_extra_origin() {
        let mut settings = settings_with_client_fixed();
        settings
            .ec
            .resolve_allowed_origins
            .push("https://www.test-publisher.com".to_owned());
        let graph = in_memory_graph();
        let request = post_with(
            Some("https://www.test-publisher.com"),
            Some("text/plain"),
            FIXED_WORD,
        );
        let response = handle_ec_resolve(
            &settings,
            request,
            &gated(&settings, true),
            Some(&graph),
            &noop_services(),
        )
        .await
        .expect("should handle resolve");
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "an operator should be able to authorize the origin their pages are served from"
        );
    }

    #[test]
    fn origins_match_follows_rfc_6454() {
        // Same triple, so the same origin (RFC 6454 §5), however it is written.
        for (candidate, allowed) in [
            ("https://www.example.com", "https://www.example.com:443"),
            ("https://www.example.com", "HTTPS://WWW.EXAMPLE.COM"),
            ("http://www.example.com", "http://www.example.com:80"),
            (
                "https://www.example.com:8443",
                "https://www.example.com:8443",
            ),
        ] {
            assert!(
                origins_match(candidate, allowed),
                "{candidate} and {allowed} should be the same origin"
            );
        }
        // A different scheme, host or port is a different origin, and a value
        // that is not a serialized origin (RFC 6454 §6.1) never matches.
        for (candidate, allowed) in [
            ("http://www.example.com", "https://www.example.com"),
            ("https://www.example.com:8443", "https://www.example.com"),
            ("https://sub.www.example.com", "https://www.example.com"),
            ("https://www.example.com/", "https://www.example.com"),
            ("https://www.example.com/path", "https://www.example.com"),
            ("https://www.example.com?x=1", "https://www.example.com"),
            ("null", "https://www.example.com"),
            ("null", "null"),
            ("www.example.com", "www.example.com"),
        ] {
            assert!(
                !origins_match(candidate, allowed),
                "{candidate} and {allowed} should not be the same origin"
            );
        }
    }

    #[test]
    fn origins_match_refuses_what_the_settings_refuse() {
        // The URL parser reads each of these as a tuple origin, but none is a
        // serialized origin, so none matches even itself.
        for value in [
            "https://user@www.example.com",
            "https://@www.example.com",
            "ftp://www.example.com",
            "wss://www.example.com",
            "https://www.example.com/.",
            "https://www.example.com\\",
            "https:www.example.com",
        ] {
            assert!(
                !origins_match(value, value),
                "{value} is not a serialized origin, so it should match nothing"
            );
        }
    }

    #[tokio::test]
    async fn resolve_conflicts_when_a_different_identity_already_exists() {
        let settings = settings_with_client_fixed();
        let graph = in_memory_graph();
        let existing = format!("{}.ABC123", "e".repeat(64));
        let ec_context = with_selected_module(
            &settings,
            EcContext::new_for_test_gated(Some(existing), ConsentContext::default(), true),
        );
        let response = handle_ec_resolve(
            &settings,
            post(FIXED_WORD),
            &ec_context,
            Some(&graph),
            &noop_services(),
        )
        .await
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
}
