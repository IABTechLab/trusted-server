use base64::{Engine as _, engine::general_purpose::STANDARD};
use edgezero_core::body::Body as EdgeBody;
use error_stack::Report;
use http::header;
use http::{Request, Response, StatusCode};
use sha2::{Digest as _, Sha256};
use subtle::ConstantTimeEq as _;

use crate::error::TrustedServerError;
use crate::settings::Settings;

const BASIC_AUTH_REALM: &str = r#"Basic realm="Trusted Server""#;

/// Marker recording that this request's `Authorization` header was consumed and
/// validated by a Trusted Server handler at the edge.
///
/// The shared template cache refuses every request carrying `Authorization`,
/// because an authorized response must never become a reader-neutral template.
/// That rule exists for credentials bound for the publisher origin, whose
/// response content TS cannot reason about.
///
/// A credential this edge terminated is a different case. [`enforce_basic_auth`]
/// runs as middleware ahead of routing, so a request that reaches a handler for a
/// gated path has necessarily already satisfied that same handler. Every reader
/// able to look up a template stored from such a request has authenticated
/// against the same credential, so reuse is not a cross-reader disclosure.
///
/// Absence of this marker on a request that still carries `Authorization` means
/// the credential is pass-through, and the template cache continues to refuse it.
///
/// # Invariants
///
/// The private field makes [`enforce_basic_auth`] the only code that can produce
/// this marker. It grants shared-template eligibility to a request that would
/// otherwise be refused, so being unforgeable outside this module is the whole
/// point: a caller cannot assert "already authenticated" without having actually
/// checked. [`enforce_basic_auth`] also clears any inherited marker before it
/// decides, so the value can never outlive the check that produced it.
#[derive(Debug, Clone)]
pub(crate) struct EdgeTerminatedAuthorization(());

impl EdgeTerminatedAuthorization {
    /// Builds the marker without performing a credential check.
    ///
    /// Test-only. Production code obtains this marker exclusively by passing
    /// [`enforce_basic_auth`], which is what makes it meaningful.
    #[cfg(test)]
    #[must_use]
    pub(crate) const fn for_test() -> Self {
        Self(())
    }
}

/// Enforces HTTP Basic authentication for configured handler paths.
///
/// Returns `Ok(None)` when the request does not target a protected handler or
/// when the supplied credentials are valid. Returns `Ok(Some(Response))` with
/// the auth challenge when credentials are missing or invalid.
///
/// Admin endpoints are protected by requiring a handler during settings
/// finalization; see [`Settings::from_toml`]. Credential checks use constant-time
/// comparison for both username and password, and evaluate both regardless of
/// individual match results to avoid timing oracles. Runtime requests within
/// the reserved admin namespace fail closed if no handler matches, providing
/// defense in depth for malformed and parameterized paths.
///
/// # Request mutation
///
/// Takes `req` mutably because it owns [`EdgeTerminatedAuthorization`]. Any
/// inherited marker is cleared on entry, and a fresh one is inserted only on the
/// success path, so the marker present after this call always describes this
/// call's own decision. Nothing else about the request is touched — in
/// particular the `Authorization` header is left in place and still reaches the
/// publisher origin.
///
/// That last point is a stated assumption: a credential this edge terminates is
/// treated as reader-neutral, which holds unless the origin *also* authenticates
/// on the same header. An origin that does so declares `Vary: Authorization`,
/// which the template-cache store refuses as an uncovered `Vary` name. An origin
/// that varies on `Authorization` without declaring it would defeat any HTTP
/// cache, and is out of scope here.
///
/// # Errors
///
/// Returns an error when handler configuration is invalid, such as an
/// un-compilable path regex.
pub fn enforce_basic_auth(
    settings: &Settings,
    req: &mut Request<EdgeBody>,
) -> Result<Option<Response<EdgeBody>>, Report<TrustedServerError>> {
    // Cleared before any early return so no inherited marker can survive a call
    // that did not itself validate a credential. Without this, a request marked
    // upstream and then routed to an unprotected path would keep an assertion
    // nothing checked.
    req.extensions_mut().remove::<EdgeTerminatedAuthorization>();

    // Scoped so the path borrow ends before the successful branch marks the
    // request. `handler` borrows `settings`, not `req`.
    let handler = {
        let path = req.uri().path();
        let Some(handler) = settings.handler_for_path(path)? else {
            if Settings::is_admin_path(path) {
                return Err(Report::new(TrustedServerError::Configuration {
                    message: format!("Admin path `{path}` has no configured handler"),
                }));
            }
            return Ok(None);
        };
        handler
    };

    let Some((username, password)) = extract_credentials(req) else {
        return Ok(Some(unauthorized_response()));
    };

    // Hash before comparing to normalise lengths — `ct_eq` on raw byte slices
    // short-circuits when lengths differ, which would leak credential length.
    // SHA-256 produces fixed-size digests so the comparison is truly constant-time.
    //
    // Note: constant-time guarantees are best-effort on WASM targets because the
    // runtime optimiser/JIT may re-introduce variable-time paths. This is an
    // inherent limitation of all constant-time code in managed runtimes.
    let username_match = Sha256::digest(handler.username.expose().as_bytes())
        .ct_eq(&Sha256::digest(username.as_bytes()));
    let password_match = Sha256::digest(handler.password.expose().as_bytes())
        .ct_eq(&Sha256::digest(password.as_bytes()));

    if bool::from(username_match & password_match) {
        // Record that TS itself consumed this credential, so the shared template
        // cache can distinguish it from a credential meant for the origin.
        req.extensions_mut().insert(EdgeTerminatedAuthorization(()));
        Ok(None)
    } else {
        log::warn!("Basic auth failed for path: {}", req.uri().path());
        Ok(Some(unauthorized_response()))
    }
}

fn extract_credentials(req: &Request<EdgeBody>) -> Option<(String, String)> {
    let mut header_values = req.headers().get_all(header::AUTHORIZATION).iter();
    let header_value = header_values.next()?.to_str().ok()?;
    if header_values.next().is_some() {
        return None;
    }

    let mut parts = header_value.splitn(2, ' ');
    let scheme = parts.next()?.trim();
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }

    let token = parts.next()?.trim();
    if token.is_empty() {
        return None;
    }

    let decoded = STANDARD.decode(token).ok()?;
    let credentials = String::from_utf8(decoded).ok()?;

    let mut credentials_parts = credentials.splitn(2, ':');
    let username = credentials_parts.next()?.to_owned();
    let password = credentials_parts.next()?.to_owned();

    Some((username, password))
}

fn unauthorized_response() -> Response<EdgeBody> {
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header(header::WWW_AUTHENTICATE, BASIC_AUTH_REALM)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(EdgeBody::from(b"Unauthorized".as_ref()))
        .expect("should build unauthorized response")
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD;
    use http::{HeaderValue, Method, header};

    use crate::test_support::tests::{crate_test_settings_str, create_test_settings};

    fn build_request(method: Method, uri: &str) -> Request<EdgeBody> {
        Request::builder()
            .method(method)
            .uri(uri)
            .body(EdgeBody::empty())
            .expect("should build request")
    }

    fn set_authorization(req: &mut Request<EdgeBody>, value: &str) {
        req.headers_mut().insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(value).expect("should build authorization header"),
        );
    }

    #[test]
    fn encoded_admin_separator_path_is_auth_gated() {
        // `^/_ts/admin` matches the raw path, so a percent-encoded separator
        // still consumes admin credentials. The publisher-fallback boundary
        // reserves the same paths so those credentials are never forwarded
        // upstream (see `ec::admin::deny_admin_diagnostic_fallback`).
        let settings = create_test_settings();

        for path in ["/_ts/admin%2Fec", "/_ts/admin%2fec"] {
            let mut req = build_request(Method::GET, &format!("https://example.com{path}"));

            let response = enforce_basic_auth(&settings, &mut req)
                .expect("should evaluate auth")
                .unwrap_or_else(|| panic!("should challenge {path}"));

            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "should require credentials for {path}"
            );
        }
    }

    #[test]
    fn valid_credentials_mark_the_request_as_edge_terminated() {
        let settings = create_test_settings();
        let mut req = build_request(Method::GET, "https://example.com/secure");
        let encoded = STANDARD.encode("user:pass");
        set_authorization(&mut req, &format!("Basic {encoded}"));

        assert!(
            enforce_basic_auth(&settings, &mut req)
                .expect("should evaluate auth")
                .is_none(),
            "valid credentials should be admitted"
        );
        assert!(
            req.extensions()
                .get::<EdgeTerminatedAuthorization>()
                .is_some(),
            "a credential this edge consumed should be marked so the template cache can share it"
        );
    }

    #[test]
    fn repeated_authorization_values_are_rejected_without_a_marker() {
        let settings = create_test_settings();
        let mut req = build_request(Method::GET, "https://example.com/secure");
        let encoded = STANDARD.encode("user:pass");
        set_authorization(&mut req, &format!("Basic {encoded}"));
        req.headers_mut().append(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer publisher-origin-credential"),
        );

        let response = enforce_basic_auth(&settings, &mut req)
            .expect("should evaluate auth")
            .expect("should challenge an ambiguous credential");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(
            req.extensions()
                .get::<EdgeTerminatedAuthorization>()
                .is_none(),
            "a repeated authorization field must remain pass-through rather than being marked safe"
        );
    }

    #[test]
    fn an_inherited_marker_is_cleared_on_an_unprotected_path() {
        // The marker asserts "this edge already checked a credential". A request
        // routed to a path no handler protects was never checked here, so a marker
        // it arrived with must not survive to grant shared-template eligibility.
        let settings = create_test_settings();
        let mut req = build_request(Method::GET, "https://example.com/open");
        set_authorization(&mut req, "Basic dXNlcjpwYXNz");
        req.extensions_mut()
            .insert(EdgeTerminatedAuthorization::for_test());

        assert!(
            enforce_basic_auth(&settings, &mut req)
                .expect("should evaluate auth")
                .is_none(),
            "an unprotected path should not challenge"
        );
        assert!(
            req.extensions()
                .get::<EdgeTerminatedAuthorization>()
                .is_none(),
            "a marker no credential check produced must not survive this call"
        );
    }

    #[test]
    fn an_inherited_marker_is_cleared_when_credentials_are_rejected() {
        let settings = create_test_settings();
        let mut req = build_request(Method::GET, "https://example.com/secure");
        let encoded = STANDARD.encode("user:wrong-pass");
        set_authorization(&mut req, &format!("Basic {encoded}"));
        req.extensions_mut()
            .insert(EdgeTerminatedAuthorization::for_test());

        let response = enforce_basic_auth(&settings, &mut req)
            .expect("should evaluate auth")
            .expect("should challenge");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(
            req.extensions()
                .get::<EdgeTerminatedAuthorization>()
                .is_none(),
            "a failed check must strip an inherited marker rather than honour it"
        );
    }

    #[test]
    fn a_non_protected_path_leaves_authorization_unmarked() {
        let settings = create_test_settings();
        let mut req = build_request(Method::GET, "https://example.com/open");
        set_authorization(&mut req, "Basic dXNlcjpwYXNz");

        assert!(
            enforce_basic_auth(&settings, &mut req)
                .expect("should evaluate auth")
                .is_none(),
            "an unprotected path should not challenge"
        );
        assert!(
            req.extensions()
                .get::<EdgeTerminatedAuthorization>()
                .is_none(),
            "a credential no handler consumed is pass-through and must stay disqualifying"
        );
    }

    #[test]
    fn rejected_credentials_leave_the_request_unmarked() {
        let settings = create_test_settings();
        let mut req = build_request(Method::GET, "https://example.com/secure");
        let encoded = STANDARD.encode("user:wrong-pass");
        set_authorization(&mut req, &format!("Basic {encoded}"));

        let response = enforce_basic_auth(&settings, &mut req)
            .expect("should evaluate auth")
            .expect("should challenge");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(
            req.extensions()
                .get::<EdgeTerminatedAuthorization>()
                .is_none(),
            "a failed credential must never be marked as terminated"
        );
    }

    #[test]
    fn no_challenge_for_non_protected_path() {
        let settings = create_test_settings();
        let mut req = build_request(Method::GET, "https://example.com/open");

        assert!(
            enforce_basic_auth(&settings, &mut req)
                .expect("should evaluate auth")
                .is_none()
        );
    }

    #[test]
    fn challenge_when_missing_credentials() {
        let settings = create_test_settings();
        let mut req = build_request(Method::GET, "https://example.com/secure");

        let response = enforce_basic_auth(&settings, &mut req)
            .expect("should evaluate auth")
            .expect("should challenge");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let realm = response
            .headers()
            .get(header::WWW_AUTHENTICATE)
            .expect("should have WWW-Authenticate header");
        assert_eq!(realm, BASIC_AUTH_REALM);
    }

    #[test]
    fn allow_when_credentials_match() {
        let settings = create_test_settings();
        let mut req = build_request(Method::GET, "https://example.com/secure/data");
        let token = STANDARD.encode("user:pass");
        set_authorization(&mut req, &format!("Basic {token}"));

        assert!(
            enforce_basic_auth(&settings, &mut req)
                .expect("should evaluate auth")
                .is_none()
        );
    }

    #[test]
    fn challenge_when_both_credentials_wrong() {
        let settings = create_test_settings();
        let mut req = build_request(Method::GET, "https://example.com/secure/data");
        let token = STANDARD.encode("wrong:wrong");
        set_authorization(&mut req, &format!("Basic {token}"));

        let response = enforce_basic_auth(&settings, &mut req)
            .expect("should evaluate auth")
            .expect("should challenge");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn challenge_when_username_wrong_password_correct() {
        // Validates that both fields are always evaluated — no short-circuit username oracle.
        let settings = create_test_settings();
        let mut req = build_request(Method::GET, "https://example.com/secure/data");
        let token = STANDARD.encode("wrong-user:pass");
        set_authorization(&mut req, &format!("Basic {token}"));

        let response = enforce_basic_auth(&settings, &mut req)
            .expect("should evaluate auth")
            .expect("should challenge");
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "should reject wrong username even with correct password"
        );
    }

    #[test]
    fn challenge_when_username_correct_password_wrong() {
        let settings = create_test_settings();
        let mut req = build_request(Method::GET, "https://example.com/secure/data");
        let token = STANDARD.encode("user:wrong-pass");
        set_authorization(&mut req, &format!("Basic {token}"));

        let response = enforce_basic_auth(&settings, &mut req)
            .expect("should evaluate auth")
            .expect("should challenge");
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "should reject correct username with wrong password"
        );
    }

    #[test]
    fn challenge_when_scheme_is_not_basic() {
        let settings = create_test_settings();
        let mut req = build_request(Method::GET, "https://example.com/secure");
        set_authorization(&mut req, "Bearer token");

        let response = enforce_basic_auth(&settings, &mut req)
            .expect("should evaluate auth")
            .expect("should challenge");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn returns_error_for_invalid_handler_regex_without_panicking() {
        let config = crate_test_settings_str().replace(r#"path = "^/secure""#, r#"path = "(""#);
        let err = Settings::from_toml(&config).expect_err("should reject invalid handler regex");
        assert!(
            err.to_string()
                .contains("Handler path regex `(` failed to compile"),
            "should describe the invalid handler regex"
        );
    }

    #[test]
    fn allow_admin_path_with_valid_credentials() {
        let settings = create_test_settings();
        let mut req = build_request(Method::POST, "https://example.com/_ts/admin/keys/rotate");
        let token = STANDARD.encode("admin:admin-pass");
        set_authorization(&mut req, &format!("Basic {token}"));

        assert!(
            enforce_basic_auth(&settings, &mut req)
                .expect("should evaluate auth")
                .is_none(),
            "should allow admin path with correct credentials"
        );
    }

    #[test]
    fn challenge_admin_path_with_wrong_credentials() {
        let settings = create_test_settings();
        let mut req = build_request(Method::POST, "https://example.com/_ts/admin/keys/rotate");
        let token = STANDARD.encode("admin:wrong");
        set_authorization(&mut req, &format!("Basic {token}"));

        let response = enforce_basic_auth(&settings, &mut req)
            .expect("should evaluate auth")
            .expect("should challenge admin path with wrong credentials");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    /// A handler regex is matched against the whole request path, so a broad
    /// pattern such as `^/_ts` covers browser-facing endpoints in that
    /// namespace — `/_ts/page-bids` and `/_ts/api/v1/identify` — not just the
    /// admin routes it was probably meant for. Anonymous browser fetches never
    /// carry Basic credentials, so those endpoints answer `401` for every
    /// visitor.
    ///
    /// This pins the behaviour rather than exempting the paths: which paths a
    /// handler covers is the operator's decision, and silently carving holes in
    /// it would be worse than a documented constraint. Operators must scope
    /// handler patterns to the paths they mean (`^/_ts/admin`) — see the
    /// configuration guide. A broad pattern will block the canonical page-bids
    /// endpoint; the hard-cutover client does not retry a compatibility alias.
    #[test]
    fn broad_handler_regex_also_covers_browser_facing_endpoints() {
        let config = crate_test_settings_str().replace(r#"path = "^/secure""#, r#"path = "^/_ts""#);
        let settings = Settings::from_toml(&config).expect("should parse broad handler regex");

        for path in [
            "https://example.com/_ts/page-bids?path=/article",
            "https://example.com/_ts/api/v1/identify",
        ] {
            let mut req = build_request(Method::GET, path);

            let response = enforce_basic_auth(&settings, &mut req)
                .expect("should evaluate auth")
                .unwrap_or_else(|| panic!("should challenge {path} under a `^/_ts` handler"));
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "a `^/_ts` handler should challenge {path}"
            );
        }
    }

    #[test]
    fn challenge_admin_path_with_missing_credentials() {
        let settings = create_test_settings();
        let mut req = build_request(Method::POST, "https://example.com/_ts/admin/keys/rotate");

        let response = enforce_basic_auth(&settings, &mut req)
            .expect("should evaluate auth")
            .expect("should challenge admin path with missing credentials");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn concrete_admin_path_without_matching_handler_fails_closed() {
        let config = crate_test_settings_str().replace(
            r#"path = "^/_ts/admin"
            username = "admin"
            password = "admin-pass""#,
            r#"path = "^/_ts/admin/(keys/rotate|keys/deactivate|ec|eids)$"
            username = "admin"
            password = "strong-test-password"

            [[handlers]]
            path = "^/_ts/admin/ec/[{]id[}]$"
            username = "admin"
            password = "strong-test-password""#,
        );
        let settings: Settings =
            toml::from_str(&config).expect("should deserialize settings without finalization");
        let ec_id = format!("{}.abc123", "a".repeat(64));
        let mut req = build_request(
            Method::GET,
            &format!("https://example.com/_ts/admin/ec/{ec_id}"),
        );

        let error = enforce_basic_auth(&settings, &mut req)
            .expect_err("should fail closed without a matching admin handler");
        assert!(
            error.to_string().contains("no configured handler"),
            "should describe the missing admin handler"
        );
    }

    #[test]
    fn similar_non_admin_prefix_without_handler_remains_public() {
        let config = crate_test_settings_str().replace(
            r#"path = "^/_ts/admin""#,
            r#"path = "^/_ts/admin/keys/rotate$""#,
        );
        let settings: Settings =
            toml::from_str(&config).expect("should deserialize settings without finalization");
        let mut req = build_request(Method::GET, "https://example.com/_ts/administrator");

        assert!(
            enforce_basic_auth(&settings, &mut req)
                .expect("should evaluate auth")
                .is_none(),
            "should not classify a similar prefix as the admin namespace"
        );
    }
}
