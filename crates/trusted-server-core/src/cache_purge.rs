//! The operator-facing purge endpoint for templates and tagged origin responses.
//!
//! `POST /_ts/admin/cache/purge` with `{"scope":"all"}` or
//! `{"scope":"url","url":"https://example.com/page"}`.
//!
//! # Why every method is registered, not just `POST`
//!
//! A named route only claims the methods it lists. A method it does not claim falls through
//! to the publisher, and `enforce_basic_auth` leaves the `Authorization` header in place, so
//! a `GET` to this path would authenticate and then ship the shared admin credential to the
//! publisher origin. The route therefore claims every method and this handler answers the
//! non-`POST` ones with 405 itself.
//!
//! # Why `Content-Type` is enforced exactly
//!
//! Browsers attach basic-auth credentials automatically. A cross-origin form POST with
//! `enctype="text/plain"` is not preflighted, so requiring `POST` alone does not stop CSRF;
//! requiring a `Content-Type` that a form cannot produce does.

use edgezero_core::body::Body as EdgeBody;
use error_stack::Report;
use http::{Method, Request, Response, StatusCode, header};
use serde::{Deserialize, Serialize};

use crate::error::TrustedServerError;
use crate::http_util::enforce_max_body_size;
use crate::platform::{RuntimeServices, reader_url_surrogate_key};

/// Purge bodies name a scope and, for a URL purge, one URL. Nothing here is unbounded.
const PURGE_MAX_BODY_BYTES: usize = 4096;

/// The request body, before its scope is validated.
///
/// Deserialized as a flat struct rather than an internally-tagged enum on purpose.
/// `deny_unknown_fields` does not reach the unit variant of such an enum, so
/// `{"scope":"all","url":"…"}` would parse as a full flush — an operator who typed the
/// wrong scope while intending to purge one page would empty the whole cache and be told
/// it succeeded. Validating the pair by hand makes that combination an error.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PurgeBody {
    scope: String,
    url: Option<String>,
}

/// What an operator asked to purge.
#[derive(Debug, PartialEq, Eq)]
enum PurgeRequest {
    /// Every template and tagged origin response this service has cached.
    All,
    /// One reader-facing URL, as a reader would type it; canonicalized before hashing.
    Url(String),
}

impl PurgeRequest {
    /// Parse and validate a purge body.
    ///
    /// # Errors
    ///
    /// Returns a message naming the problem when the JSON is malformed, the scope is not
    /// one of the two documented values, or the scope and `url` field disagree.
    fn parse(bytes: &[u8]) -> Result<Self, String> {
        let body: PurgeBody =
            serde_json::from_slice(bytes).map_err(|error| format!("invalid JSON: {error}"))?;

        match (body.scope.as_str(), body.url) {
            ("all", None) => Ok(Self::All),
            ("all", Some(_)) => Err(
                "scope \"all\" takes no url; did you mean {\"scope\":\"url\",\"url\":…}?"
                    .to_owned(),
            ),
            // Parsed, not merely non-empty. `reader_url_surrogate_key` hashes whatever it
            // is given, so a path, a scheme-less host, or an `ftp://` URL would be
            // acknowledged as purged under a key nothing was ever stored with — the exact
            // silent no-op that key's own documentation calls the failure that matters.
            // A CMS webhook sending paths would purge nothing and never find out.
            ("url", Some(url))
                if url::Url::parse(&url).is_ok_and(|parsed| {
                    matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some()
                }) =>
            {
                Ok(Self::Url(url))
            }
            ("url", _) => Err(
                "scope \"url\" requires an absolute http(s) url, as a reader addresses the page"
                    .to_owned(),
            ),
            (other, _) => Err(format!(
                "unknown scope {other:?}; expected \"all\" or \"url\""
            )),
        }
    }
}

/// What the purge did, in terms an operator mid-incident can act on.
#[derive(Debug, Serialize)]
struct PurgeResponse {
    purged: bool,
    scope: &'static str,
    /// Present on a URL purge, so an operator can confirm which key was hit.
    #[serde(skip_serializing_if = "Option::is_none")]
    surrogate_key: Option<String>,
    message: &'static str,
}

/// Handle a purge request.
///
/// # Errors
///
/// Returns an error when the body cannot be read, exceeds [`PURGE_MAX_BODY_BYTES`], is not
/// valid JSON for a known scope, or when the platform's purge fails.
pub async fn handle_cache_purge(
    services: &RuntimeServices,
    req: Request<EdgeBody>,
    authenticated_principal: Option<&str>,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    if req.method() != Method::POST {
        return Ok(method_not_allowed());
    }

    // Checked before the body is read: a request that cannot be a legitimate API call
    // should not have its payload parsed at all.
    let content_type = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if !is_exactly_json(content_type) {
        return Ok(json_response(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            r#"{"purged":false,"error":"Content-Type must be application/json"}"#,
        ));
    }

    let body = req.into_body();
    if body.is_stream() {
        return Err(Report::new(TrustedServerError::BadRequest {
            message: "cache-purge request body must be buffered, not streamed".into(),
        }));
    }
    let bytes = body.into_bytes().unwrap_or_default();
    enforce_max_body_size(&bytes, PURGE_MAX_BODY_BYTES, "cache-purge")?;

    let request =
        PurgeRequest::parse(&bytes).map_err(|message| TrustedServerError::BadRequest {
            message: format!("invalid cache-purge request: {message}"),
        })?;

    match request {
        PurgeRequest::All => {
            // An unbounded flush behind one shared static credential is also an
            // origin-stampede lever, and there is no rate-limit primitive on this path, so
            // who used it is recorded every time.
            log::warn!(
                "Cache purge: ALL templates purged by {}",
                authenticated_principal.unwrap_or("<unidentified principal>")
            );
            services
                .template_cache()
                .purge_all()
                .await
                .map_err(|error| TrustedServerError::Configuration {
                    message: format!("cache purge failed: {error}"),
                })?;
            Ok(purge_ok(&PurgeResponse {
                purged: true,
                scope: "all",
                surrogate_key: None,
                // One surrogate-key call, so there is no partial state to reason about:
                // an error means nothing was purged and the call is safe to retry.
                message: "All templates purged. Purge is idempotent and safe to repeat.",
            }))
        }
        PurgeRequest::Url(url) => {
            // The reader-facing key, so callers pass the URL a reader would see and never
            // have to replay this service's origin rewriting.
            let surrogate_key = reader_url_surrogate_key(&url);
            log::info!(
                "Cache purge: url {url} (key {surrogate_key}) by {}",
                authenticated_principal.unwrap_or("<unidentified principal>")
            );
            services
                .template_cache()
                .purge_url_surrogate_key(&surrogate_key)
                .await
                .map_err(|error| TrustedServerError::Configuration {
                    message: format!("cache purge failed: {error}"),
                })?;
            Ok(purge_ok(&PurgeResponse {
                purged: true,
                scope: "url",
                surrogate_key: Some(surrogate_key),
                message: "URL purged. Purge is idempotent and safe to repeat.",
            }))
        }
    }
}

/// Exactly `application/json`, with optional parameters and whitespace.
///
/// Deliberately strict: accepting `application/json`-ish media types would readmit the
/// form-post CSRF shape this check exists to close.
fn is_exactly_json(content_type: &str) -> bool {
    content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .eq_ignore_ascii_case("application/json")
}

fn method_not_allowed() -> Response<EdgeBody> {
    let mut response = json_response(
        StatusCode::METHOD_NOT_ALLOWED,
        r#"{"purged":false,"error":"cache purge requires POST"}"#,
    );
    response
        .headers_mut()
        .insert(header::ALLOW, header::HeaderValue::from_static("POST"));
    response
}

fn purge_ok(payload: &PurgeResponse) -> Response<EdgeBody> {
    let body = serde_json::to_string(payload)
        .unwrap_or_else(|_| r#"{"purged":true,"scope":"unknown"}"#.to_owned());
    json_response(StatusCode::OK, &body)
}

/// Purge answers are per-operator and per-moment; nothing may store one.
fn json_response(status: StatusCode, body: &str) -> Response<EdgeBody> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CACHE_CONTROL, "private, no-store")
        .body(EdgeBody::from(body.to_owned()))
        .expect("should build a cache-purge response")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_type_must_be_exactly_json() {
        assert!(is_exactly_json("application/json"));
        assert!(is_exactly_json("application/json; charset=utf-8"));
        assert!(is_exactly_json("  APPLICATION/JSON  "));
    }

    #[test]
    fn form_postable_content_types_are_refused() {
        // These three are the only types a cross-origin form can send, and a form POST
        // carries basic-auth credentials without a preflight.
        for content_type in [
            "text/plain",
            "application/x-www-form-urlencoded",
            "multipart/form-data",
            "",
        ] {
            assert!(
                !is_exactly_json(content_type),
                "{content_type} must not be accepted"
            );
        }
    }

    #[test]
    fn json_lookalike_types_are_refused() {
        for content_type in [
            "application/jsonrequest",
            "text/json",
            "application/ld+json",
        ] {
            assert!(
                !is_exactly_json(content_type),
                "{content_type} must not be accepted"
            );
        }
    }

    #[test]
    fn a_non_post_answer_names_the_method_it_accepts() {
        let response = method_not_allowed();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(
            response
                .headers()
                .get(header::ALLOW)
                .and_then(|value| value.to_str().ok()),
            Some("POST")
        );
    }

    #[test]
    fn every_answer_forbids_storage() {
        for response in [method_not_allowed(), json_response(StatusCode::OK, "{}")] {
            assert_eq!(
                response
                    .headers()
                    .get(header::CACHE_CONTROL)
                    .and_then(|value| value.to_str().ok()),
                Some("private, no-store"),
                "a purge answer must never be stored"
            );
        }
    }

    #[test]
    fn scope_parsing_accepts_both_documented_shapes() {
        assert_eq!(
            PurgeRequest::parse(br#"{"scope":"all"}"#).expect("should parse"),
            PurgeRequest::All
        );
        assert_eq!(
            PurgeRequest::parse(br#"{"scope":"url","url":"https://example.com/a"}"#)
                .expect("should parse"),
            PurgeRequest::Url("https://example.com/a".to_owned())
        );
    }

    #[test]
    fn a_url_alongside_scope_all_is_refused_rather_than_flushing_everything() {
        // The dangerous typo: an operator means to purge one page, mistypes the scope, and
        // would otherwise be told a full flush succeeded.
        let error = PurgeRequest::parse(br#"{"scope":"all","url":"https://example.com/a"}"#)
            .expect_err("should refuse");
        assert!(
            error.contains("takes no url"),
            "the error must point at the confusion, got: {error}"
        );
    }

    #[test]
    fn an_unknown_or_malformed_scope_is_refused() {
        // A typo must not silently widen into a full flush.
        for body in [
            &br#"{"scope":"everything"}"#[..],
            br#"{"scope":"url"}"#,
            br#"{"scope":"url","url":"   "}"#,
            // Hashed as given, these would each be acknowledged as purged under a key
            // that can never match a stored object.
            br#"{"scope":"url","url":"/article"}"#,
            br#"{"scope":"url","url":"example.com/article"}"#,
            br#"{"scope":"url","url":"ftp://example.com/article"}"#,
            br#"{"scope":"all","url":"https://example.com/a"}"#,
            br#"{}"#,
            br#"{"scope":"ALL"}"#,
            br#"{"scope":"all","extra":1}"#,
            br#"not json"#,
        ] {
            assert!(
                PurgeRequest::parse(body).is_err(),
                "{} must not parse",
                String::from_utf8_lossy(body)
            );
        }
    }

    #[test]
    fn a_reader_facing_url_still_parses() {
        for body in [
            &br#"{"scope":"url","url":"https://ts.example.com/article"}"#[..],
            br#"{"scope":"url","url":"http://ts.example.com/article?a=1"}"#,
            br#"{"scope":"url","url":"https://ts.example.com"}"#,
        ] {
            assert!(
                PurgeRequest::parse(body).is_ok(),
                "{} is a URL a reader can address and must still purge",
                String::from_utf8_lossy(body)
            );
        }
    }
}
