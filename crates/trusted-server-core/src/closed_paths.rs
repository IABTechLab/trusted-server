//! Paths this server answers itself and never forwards to the publisher's origin.
//!
//! A request to `/_ts/admin`, or to the `/admin/keys` addresses beside it, is
//! an administration request. It can carry an operator's `Authorization`
//! header and a payload meant for this server alone. No route serves either
//! prefix, and forwarding such a request would hand both to the origin, so
//! every adapter asks [`closed_path_response`] before its publisher fallback
//! and the request is answered `404` here.

use std::borrow::Cow;

use edgezero_core::body::Body as EdgeBody;
use http::{Request, Response, StatusCode, header};

/// The server's administration prefix.
///
/// It is matched as a bare prefix, with no `/` after it, so that a
/// percent-encoded separator such as `/_ts/admin%2Fec` is closed with the
/// paths it decodes to. A check for a literal slash would miss that spelling.
const ADMIN_NAMESPACE_PREFIX: &str = "/_ts/admin";

/// The key administration alias outside `/_ts`.
///
/// Only the alias itself and its separator descendants are closed. A bare
/// prefix here would also close unrelated publisher paths such as
/// `/admin/keystore`.
const ADMIN_KEYS_PREFIX: &str = "/admin/keys";

/// Maximum percent-decoding rounds applied when testing a path.
///
/// Bounds the work a `%25`-chained path can force while still reaching the
/// fixed point of any separator encoding a proxy chain would plausibly decode.
const MAX_PERCENT_DECODE_ROUNDS: usize = 4;

/// Returns the local `404 Not Found` for a request to a closed path, and
/// `None` for any other request, which the publisher fallback then answers.
///
/// The path is tested as sent and after each of a bounded number of
/// percent-decoding rounds, so `/_ts/admin%2Fec` and `/admin%252Fkeys/rotate`
/// are closed with the paths they decode to.
#[must_use]
pub fn closed_path_response(req: &Request<EdgeBody>) -> Option<Response<EdgeBody>> {
    is_closed(req.uri().path()).then(not_found)
}

/// Returns whether `path` is closed either as sent or after any bounded number
/// of percent-decoding rounds.
///
/// A separator encoded twice, as in `/admin%252Fkeys/rotate`, survives one
/// decode as `/admin%2Fkeys/rotate`, so the check is repeated to a fixed point
/// rather than applied once.
fn is_closed(path: &str) -> bool {
    if is_closed_as_written(path) {
        return true;
    }

    let mut current = path.to_owned();
    for _ in 0..MAX_PERCENT_DECODE_ROUNDS {
        let Some(decoded) = percent_decoded_path(&current) else {
            return false;
        };

        // A path whose remaining `%` sequences are not decodable escapes is a
        // fixed point, so further rounds would repeat the same comparison.
        if decoded == current {
            return false;
        }

        if is_closed_as_written(&decoded) {
            return true;
        }

        current = decoded;
    }

    false
}

fn is_closed_as_written(path: &str) -> bool {
    path.starts_with(ADMIN_NAMESPACE_PREFIX)
        || path == ADMIN_KEYS_PREFIX
        || path
            .strip_prefix(ADMIN_KEYS_PREFIX)
            .is_some_and(|remainder| remainder.starts_with('/'))
}

/// Percent-decodes `path` once, returning `None` when the path contains no
/// escape sequence or decodes to invalid UTF-8.
fn percent_decoded_path(path: &str) -> Option<String> {
    if !path.contains('%') {
        return None;
    }

    urlencoding::decode(path).ok().map(Cow::into_owned)
}

fn not_found() -> Response<EdgeBody> {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header(header::CONTENT_TYPE, mime::APPLICATION_JSON.as_ref())
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .body(EdgeBody::from(r#"{"error":"not found"}"#))
        .expect("should build the response for a closed path")
}

#[cfg(test)]
mod tests {
    use http::Method;

    use super::*;

    fn request(method: Method, path: &str) -> Request<EdgeBody> {
        Request::builder()
            .method(method)
            .uri(format!("https://edge.example.com{path}"))
            .body(EdgeBody::empty())
            .expect("should build test request")
    }

    fn assert_closed(path: &str) {
        for method in [
            Method::GET,
            Method::POST,
            Method::HEAD,
            Method::OPTIONS,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
        ] {
            let response = closed_path_response(&request(method.clone(), path))
                .unwrap_or_else(|| panic!("should answer {method} {path} here"));

            assert_eq!(
                response.status(),
                StatusCode::NOT_FOUND,
                "should answer {method} {path} with 404"
            );
            assert_eq!(
                response.headers().get(header::CACHE_CONTROL),
                Some(&http::HeaderValue::from_static("no-store")),
                "should prevent caching for {path}"
            );
        }
    }

    #[test]
    fn every_path_beneath_the_administration_prefix_is_closed() {
        let ec_id = format!("{}.abc123", "a".repeat(64));
        for path in [
            "/_ts/admin".to_owned(),
            "/_ts/admin/".to_owned(),
            "/_ts/admin/ec".to_owned(),
            "/_ts/admin/ec/".to_owned(),
            format!("/_ts/admin/ec/{ec_id}"),
            format!("/_ts/admin/ec/{ec_id}/extra"),
            "/_ts/admin/eids".to_owned(),
            "/_ts/admin/eids.json".to_owned(),
            "/_ts/admin/ec;foo".to_owned(),
            "/_ts/admin/keys/rotate".to_owned(),
            "/_ts/admin/cache/purge".to_owned(),
            "/_ts/admin/unknown".to_owned(),
        ] {
            assert_closed(&path);
        }
    }

    #[test]
    fn an_encoded_separator_does_not_open_a_closed_path() {
        // A check for a literal slash would miss `/_ts/admin%2Fec`, and
        // reaching the publisher fallback would forward the caller's
        // `Authorization` header and body to the origin.
        for path in [
            "/_ts/admin%2Fec",
            "/_ts/admin%2fec",
            "/_ts/admin%2Fkeys/rotate",
            "/_ts/admin%252Fec",
            "/_ts/admin%5Cec",
            "/_ts/adminec",
            "/%5Fts/admin/ec",
        ] {
            assert_closed(path);
        }
    }

    #[test]
    fn the_key_alias_outside_ts_is_closed_with_its_descendants() {
        for path in [
            "/admin/keys",
            "/admin/keys/",
            "/admin/keys/rotate",
            "/admin/keys/deactivate",
            "/admin/keys/rotate/",
            "/admin/keys/rotate/extra",
            "/admin/keys%2Frotate",
            "/admin/keys%2frotate",
            "/admin%2Fkeys/rotate",
            "/admin%2fkeys%2Frotate",
        ] {
            assert_closed(path);
        }
    }

    #[test]
    fn a_separator_encoded_more_than_once_is_still_closed() {
        // One decode leaves `/admin%252Fkeys/rotate` as `/admin%2Fkeys/rotate`,
        // which no literal check matches. Decoding to a fixed point keeps the
        // path closed against a proxy or origin that decodes it more than once.
        for path in [
            "/admin%252Fkeys/rotate",
            "/admin/keys%252Frotate",
            "/admin%25252Fkeys/rotate",
            "/_ts%252Fadmin/ec",
        ] {
            assert_closed(path);
        }
    }

    #[test]
    fn other_publisher_paths_are_left_to_the_fallback() {
        for path in [
            "/",
            "/articles/example",
            "/admin",
            "/admin/login",
            "/admin/keyboards",
            "/admin/keystore",
            "/admin/keys%25store",
            "/articles/100%25-organic",
            "/_ts/api/v1/batch-sync",
            "/_ts/data",
        ] {
            assert!(
                closed_path_response(&request(Method::POST, path)).is_none(),
                "should leave {path} to the publisher fallback"
            );
        }
    }

    #[test]
    fn the_answer_says_nothing_but_not_found() {
        let response = closed_path_response(&request(Method::GET, "/_ts/admin/ec"))
            .expect("should answer a closed path");

        assert_eq!(
            response.headers().get(header::CONTENT_TYPE),
            Some(&http::HeaderValue::from_static("application/json"))
        );
        assert_eq!(
            response.headers().get(header::X_CONTENT_TYPE_OPTIONS),
            Some(&http::HeaderValue::from_static("nosniff"))
        );
        let body = response.into_body().into_bytes().unwrap_or_default();
        assert_eq!(&body[..], br#"{"error":"not found"}"#);
    }
}
