//! Signed evidence of who operates this deployment and which build it runs.
//!
//! The endpoint answers with evidence in the sense of RFC 9334, a description
//! of the build and the platform signed with the operator's key. A deployment
//! on a CDN cannot measure its own bytes, so the evidence proves who operates
//! the deployment, not what its code is. A verifier holding the operator's
//! public keys decides whether to trust it, and the page sends the reader there
//! rather than claiming validity itself.
//!
//! The signature is ECDSA P-256 with SHA-256, deterministic per RFC 6979, over
//! the configured context, one line feed, then the evidence bytes.

use std::sync::atomic::{AtomicBool, Ordering};

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use edgezero_core::body::Body as EdgeBody;
use http::{HeaderValue, Method, Request, Response, StatusCode, header};
use p256::ecdsa::signature::Signer as _;
use p256::ecdsa::{Signature, SigningKey};
use p256::pkcs8::DecodePrivateKey as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use validator::{Validate, ValidationError};

use crate::inspect::config::{CONFIG_JSON_PATH, CONFIG_PAGE_PATH};
use crate::inspect::data::DATA_PAGE_PATH;
use crate::inspect::html_escape;
use crate::inspect::permissions::{PERMISSIONS_JSON_PATH, PERMISSIONS_PAGE_PATH};
use crate::publisher::{PAGE_BIDS_LEGACY_PATH, PAGE_BIDS_PATH};
use crate::settings::Settings;

// `ATTESTATION_KEYS`, written by build.rs from the schedule the build names.
include!(concat!(env!("OUT_DIR"), "/attestation_keys.rs"));

/// The build script's reading of a key schedule, compiled here so a test can
/// call it.
#[cfg(test)]
#[path = "../key_schedule.rs"]
mod key_schedule;

/// The address of the attestation page when the settings name none. The JSON
/// form is at the same address plus `.json`.
pub const DEFAULT_ENDPOINT: &str = "/_ts/attestation";

/// The addresses beneath an underscore prefix that a deployment answers
/// itself whatever the settings say, so an endpoint may take none of them. A
/// segment in braces stands for any one segment, as it does in the router.
pub const RESERVED_PATHS: &[&str] = &[
    CONFIG_PAGE_PATH,
    CONFIG_JSON_PATH,
    PERMISSIONS_PAGE_PATH,
    PERMISSIONS_JSON_PATH,
    DATA_PAGE_PATH,
    PAGE_BIDS_PATH,
    PAGE_BIDS_LEGACY_PATH,
    "/_ts/api/v1/batch-sync",
    "/_ts/api/v1/identify",
    "/_ts/api/v1/ec/resolve",
    "/_ts/set-tester",
    "/_ts/clear-tester",
    // Answered ahead of the router by an adapter whose host exposes the TLS
    // fingerprint or serves several requests from one sandbox.
    "/_ts/debug/ja4",
    "/_ts/debug/sandbox",
];

/// Prefixes beneath which common web frameworks serve their assets from the
/// origin. An endpoint under one would own the prefix and answer `404` for
/// every asset there, so an endpoint may take none of them.
pub const ORIGIN_ASSET_PREFIXES: &[&str] = &[
    "_app",
    "_astro",
    "_content",
    "_expo",
    "_framework",
    "_next",
    "_nuxt",
    "_vercel",
];

/// The signature algorithm, as JOSE names it.
const ALGORITHM: &str = "ES256";

/// The version of the evidence format.
const EVIDENCE_VERSION: u8 = 1;

/// The longest nonce a caller may have signed.
const MAX_NONCE_LEN: usize = 64;

/// The architecture whose roles and terms the endpoint follows.
const STANDARD_URL: &str = "https://www.rfc-editor.org/rfc/rfc9334";

/// A key id, the Unix second the key comes into force, and the private key
/// as base64 of its SEC1 or PKCS #8 DER encoding.
type ScheduledKey<'a> = (&'a str, i64, &'a str);

/// The `[attestation]` settings section.
#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct AttestationConfig {
    /// The address of the attestation page, with the JSON form at the same
    /// address plus `.json`. When it is not the default, reads of the default
    /// address redirect here. Neither form may be one of the
    /// [`RESERVED_PATHS`].
    #[serde(default = "default_endpoint")]
    #[validate(custom(function = validate_endpoint))]
    pub endpoint: String,
    /// Who the evidence says operates this deployment.
    #[validate(length(min = 1, max = 64))]
    pub operator: String,
    /// The text signed ahead of the evidence, naming what the signature is
    /// for.
    #[validate(length(min = 1, max = 64), custom(function = validate_single_line))]
    pub context: String,
    /// Where a relying party checks the evidence.
    #[validate(custom(function = validate_https_url))]
    pub verify_url: String,
}

fn default_endpoint() -> String {
    DEFAULT_ENDPOINT.to_owned()
}

/// What the evidence says about the platform, supplied by the adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlatformIdentity {
    /// The platform's name.
    pub name: String,
    /// The platform's identifier for the service running this build.
    pub service_id: String,
    /// The platform's version number of that service.
    pub service_version: String,
    /// Whether the platform's staging environment answered.
    pub staging: bool,
    /// The location that answered.
    pub pop: String,
}

/// What a fact the platform did not report reads as.
pub const UNKNOWN: &str = "unknown";

impl PlatformIdentity {
    /// A platform that reports its name and nothing else.
    #[must_use]
    pub fn named(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            service_id: UNKNOWN.to_owned(),
            service_version: UNKNOWN.to_owned(),
            staging: false,
            pop: UNKNOWN.to_owned(),
        }
    }
}

impl Default for PlatformIdentity {
    /// A platform nothing has been reported about.
    fn default() -> Self {
        Self::named(UNKNOWN)
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Build {
    version: &'static str,
    built_at: &'static str,
    commit: &'static str,
    run: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AttestationEvidence<'a> {
    v: u8,
    operator: &'a str,
    publisher: &'a str,
    host: &'a str,
    platform: &'a PlatformIdentity,
    build: Build,
    #[serde(skip_serializing_if = "Option::is_none")]
    nonce: Option<&'a str>,
    issued_at: String,
}

/// The addresses the endpoint answers at, the page first, or `None` when the
/// settings carry no `[attestation]` section.
#[must_use]
pub fn attestation_paths(settings: &Settings) -> Option<[String; 2]> {
    Some(paths_of(&settings.attestation.as_ref()?.endpoint))
}

/// The page and JSON addresses of an endpoint.
fn paths_of(endpoint: &str) -> [String; 2] {
    [endpoint.to_owned(), format!("{endpoint}.json")]
}

/// The first segment of an endpoint, being the prefix its pages sit under.
fn prefix_of(endpoint: &str) -> &str {
    endpoint
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or_default()
}

/// A route a deployment registers for its attestation pages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefixRoute {
    /// The path, in the router's own syntax.
    pub path: String,
    /// Whether every method is answered here rather than by the publisher's
    /// origin. True for the routes beneath a prefix the deployment owns.
    pub every_method: bool,
}

/// The routes to register for the attestation pages, empty when the settings
/// carry no `[attestation]` section.
///
/// A deployment whose endpoint sits under a prefix of its own owns that
/// prefix, so the prefix itself lists the pages beneath it and any other path
/// there is not found. The default prefix is shared with other routes, so
/// only the pages themselves are taken from it. When the endpoint is not the
/// default, the default address is registered too, to redirect.
#[must_use]
pub fn prefix_routes(settings: &Settings) -> Vec<PrefixRoute> {
    let Some(config) = settings.attestation.as_ref() else {
        return Vec::new();
    };
    let prefix = prefix_of(&config.endpoint);
    let owns_prefix = prefix != prefix_of(DEFAULT_ENDPOINT);
    let route = |path: String, every_method: bool| PrefixRoute { path, every_method };
    let mut routes: Vec<PrefixRoute> = paths_of(&config.endpoint)
        .into_iter()
        .map(|path| route(path, owns_prefix))
        .collect();
    if config.endpoint != DEFAULT_ENDPOINT {
        routes.extend(
            paths_of(DEFAULT_ENDPOINT)
                .into_iter()
                .map(|path| route(path, false)),
        );
    }
    if owns_prefix {
        routes.extend(
            [
                format!("/{prefix}"),
                format!("/{prefix}/"),
                format!("/{prefix}/{{*rest}}"),
            ]
            .into_iter()
            .map(|path| route(path, true)),
        );
    }
    routes
}

/// Answers every request routed for the attestation pages, being the pages
/// themselves, a redirect from the default address when the endpoint has
/// moved, the list of pages at an owned prefix, and not found for anything
/// else.
#[must_use]
pub fn handle_prefix(
    settings: &Settings,
    platform: &PlatformIdentity,
    req: &Request<EdgeBody>,
) -> Response<EdgeBody> {
    let Some(config) = settings.attestation.as_ref() else {
        return error_response(
            StatusCode::NOT_FOUND,
            "This deployment serves no attestation.",
        );
    };
    let read = matches!(*req.method(), Method::GET | Method::HEAD);
    let path = req.uri().path();
    let [page, json] = paths_of(&config.endpoint);
    if read && (path == page || path == json) {
        return handle_attestation(settings, platform, req);
    }
    if read && config.endpoint != DEFAULT_ENDPOINT {
        let [default_page, default_json] = paths_of(DEFAULT_ENDPOINT);
        if path == default_page {
            return redirect(&page, req.uri().query());
        }
        if path == default_json {
            return redirect(&json, req.uri().query());
        }
    }
    let prefix = prefix_of(&config.endpoint);
    if read && (path == format!("/{prefix}") || path == format!("/{prefix}/")) {
        return response(
            StatusCode::OK,
            "text/html; charset=utf-8",
            render_index(&page, &json),
        );
    }
    response(
        StatusCode::NOT_FOUND,
        "text/html; charset=utf-8",
        render_not_found(prefix),
    )
}

/// Sends the caller to `location`, keeping the query it asked with so a
/// nonce survives the move.
fn redirect(location: &str, query: Option<&str>) -> Response<EdgeBody> {
    let target = match query {
        Some(query) if !query.is_empty() => format!("{location}?{query}"),
        _ => location.to_owned(),
    };
    let mut redirect = response(
        StatusCode::MOVED_PERMANENTLY,
        "application/json",
        json!({ "location": target }).to_string(),
    );
    if let Ok(value) = HeaderValue::from_str(&target) {
        redirect.headers_mut().insert(header::LOCATION, value);
    }
    redirect
}

/// Answers with evidence signed by the compiled-in key in force now, as a
/// page or, at the `.json` address, as data.
///
/// Every failure becomes a JSON body with a status, so an adapter only routes
/// here. The handler makes no outbound request.
#[must_use]
pub fn handle_attestation(
    settings: &Settings,
    platform: &PlatformIdentity,
    req: &Request<EdgeBody>,
) -> Response<EdgeBody> {
    answer(
        settings,
        platform,
        req,
        ATTESTATION_KEYS,
        crate::ec::current_timestamp(),
    )
}

fn answer(
    settings: &Settings,
    platform: &PlatformIdentity,
    req: &Request<EdgeBody>,
    schedule: &[ScheduledKey<'_>],
    now: u64,
) -> Response<EdgeBody> {
    let Some(config) = settings.attestation.as_ref() else {
        return error_response(
            StatusCode::NOT_FOUND,
            "This deployment serves no attestation.",
        );
    };
    let nonce = match nonce_from_query(req.uri().query()) {
        Ok(nonce) => nonce,
        Err(message) => return error_response(StatusCode::BAD_REQUEST, message),
    };
    let Some(&(key_id, _, private_key)) = key_in_force(schedule, now) else {
        log::error!("attestation has no signing key in force at {now}");
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "No signing key is built in for this time, so no evidence can be signed.",
        );
    };
    let host = request_host(req);
    let evidence = AttestationEvidence {
        v: EVIDENCE_VERSION,
        operator: &config.operator,
        publisher: &settings.publisher.domain,
        host: &host,
        platform,
        build: Build {
            version: crate::build_info::VERSION,
            built_at: crate::build_info::BUILT_AT,
            commit: crate::build_info::COMMIT,
            run: crate::build_info::BUILD_RUN,
        },
        nonce: nonce.as_deref(),
        issued_at: rfc3339(now),
    };
    let Ok(payload) = serde_json::to_vec(&evidence) else {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "The evidence could not be written.",
        );
    };
    let signature = match sign(&config.context, &payload, private_key) {
        Ok(signature) => signature,
        Err(message) => {
            log::error!("attestation key `{key_id}` is unusable: {message}");
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "The signing key could not be read, so no evidence can be signed.",
            );
        }
    };
    let envelope = json!({
        "evidence": serde_json::from_slice::<Value>(&payload).unwrap_or(Value::Null),
        "payload": URL_SAFE_NO_PAD.encode(&payload),
        "signature": {
            "kid": key_id,
            "alg": ALGORITHM,
            "value": URL_SAFE_NO_PAD.encode(signature),
        },
        "verify": config.verify_url,
    });
    if req.uri().path().ends_with(".json") {
        let body = serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| "{}".to_owned());
        response(StatusCode::OK, "application/json", body)
    } else {
        response(
            StatusCode::OK,
            "text/html; charset=utf-8",
            render_page(
                &config.operator,
                &config.verify_url,
                &json_address(req),
                &envelope,
            ),
        )
    }
}

/// The absolute address of the JSON form of the page being served, from the
/// request's scheme and the host it was addressed to.
///
/// The scheme follows a `Forwarded` or `X-Forwarded-Proto` header when one is
/// sent. The host is read the way [`request_host`] reads the one the evidence
/// signs, so the link and the signature name the same host, and a forwarded
/// host header is never read.
fn json_address(req: &Request<EdgeBody>) -> String {
    let info =
        crate::http_util::RequestInfo::from_request(req, &crate::platform::ClientInfo::default());
    let authority = req
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .or_else(|| req.uri().authority().map(http::uri::Authority::as_str))
        .unwrap_or_default();
    format!("{}://{authority}{}.json", info.scheme, req.uri().path())
}

/// Set once the process has warned that it signs with the last key built in.
static LAST_KEY_WARNED: AtomicBool = AtomicBool::new(false);

/// Whether this is the first time `warned` is asked, setting it.
fn first_time(warned: &AtomicBool) -> bool {
    !warned.swap(true, Ordering::Relaxed)
}

/// The key in force at `now`, being the latest to have started. Warns once
/// for each process when no later key follows it, because the build must
/// then be refreshed.
fn key_in_force<'a>(schedule: &'a [ScheduledKey<'a>], now: u64) -> Option<&'a ScheduledKey<'a>> {
    let now = i64::try_from(now).ok()?;
    let current = schedule
        .iter()
        .filter(|(_, starts_at, _)| *starts_at <= now)
        .max_by_key(|(_, starts_at, _)| *starts_at)?;
    if !schedule
        .iter()
        .any(|(_, starts_at, _)| *starts_at > current.1)
        && first_time(&LAST_KEY_WARNED)
    {
        log::warn!(
            "attestation key `{}` is the last one built in, so the build must be refreshed",
            current.0
        );
    }
    Some(current)
}

/// The bytes a signature covers, being the context, one line feed, then the
/// evidence.
#[must_use]
pub fn signed_bytes(context: &str, payload: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(context.len() + 1 + payload.len());
    message.extend_from_slice(context.as_bytes());
    message.push(b'\n');
    message.extend_from_slice(payload);
    message
}

/// Signs `payload` under `context` with a key held as base64 DER text,
/// returning the raw 64 byte `r` and `s` pair.
fn sign(context: &str, payload: &[u8], key_text: &str) -> Result<Vec<u8>, &'static str> {
    let der = STANDARD
        .decode(key_text.trim())
        .map_err(|_| "the key is not base64")?;
    let secret = p256::SecretKey::from_sec1_der(&der)
        .or_else(|_| p256::SecretKey::from_pkcs8_der(&der))
        .map_err(|_| "the key is not a P-256 private key in SEC1 or PKCS #8 DER")?;
    let signature: Signature = SigningKey::from(secret).sign(&signed_bytes(context, payload));
    Ok(signature.to_bytes().to_vec())
}

/// The caller's nonce, when it asked for one and it is well formed.
fn nonce_from_query(query: Option<&str>) -> Result<Option<String>, &'static str> {
    let Some(query) = query else {
        return Ok(None);
    };
    let Some((_, nonce)) =
        url::form_urlencoded::parse(query.as_bytes()).find(|(name, _)| name == "nonce")
    else {
        return Ok(None);
    };
    let well_formed = !nonce.is_empty()
        && nonce.len() <= MAX_NONCE_LEN
        && nonce
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if well_formed {
        Ok(Some(nonce.into_owned()))
    } else {
        Err("A nonce is 1 to 64 characters from A to Z, a to z, 0 to 9, _ and -.")
    }
}

/// The host the request was addressed to, lowercased with any port removed.
fn request_host(req: &Request<EdgeBody>) -> String {
    let raw = req
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .or_else(|| req.uri().host())
        .unwrap_or_default();
    let without_port = match raw.rsplit_once(':') {
        Some((host, port)) if port.bytes().all(|b| b.is_ascii_digit()) => host,
        _ => raw,
    };
    without_port.to_ascii_lowercase()
}

/// Seconds since the epoch as RFC 3339 in UTC, to the second.
fn rfc3339(seconds: u64) -> String {
    i64::try_from(seconds)
        .ok()
        .and_then(|seconds| chrono::DateTime::from_timestamp(seconds, 0))
        .map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_default()
}

fn response(status: StatusCode, content_type: &'static str, body: String) -> Response<EdgeBody> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, HeaderValue::from_static(content_type))
        .header(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))
        .header(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_static("*"),
        )
        .body(EdgeBody::from(body.into_bytes()))
        .expect("should build attestation response")
}

fn error_response(status: StatusCode, message: &str) -> Response<EdgeBody> {
    response(
        status,
        "application/json",
        json!({ "error": message }).to_string(),
    )
}

/// The page form: the claim in words, the link to the verifier, the address
/// of the JSON form to copy, the envelope itself, then the standard followed.
fn render_page(operator: &str, verify_url: &str, json_url: &str, envelope: &Value) -> String {
    let verifier = url::Url::parse(verify_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_else(|| verify_url.to_owned());
    let pretty = serde_json::to_string_pretty(envelope).unwrap_or_else(|_| "{}".to_owned());
    let body = format!(
        "<h1>Attestation</h1>\n\
<p>This page is served by a deployment that says it is operated by {operator}. \
Do not take this page's word for it.</p>\n\
<p><a class=\"check\" href=\"{href}\">Check this claim at {verifier}</a></p>\n\
<p>To retrieve this data as JSON for use in your own verification checks use the URL</p>\n\
<div class=\"copy\"><code id=\"json-url\">{json_url}</code>\
<button type=\"button\" id=\"copy\">Copy</button></div>\n\
<p>The JSON response is</p>\n\
<pre>{pretty}</pre>\n\
<p class=\"standard\">The attester, evidence and verifier on this page follow the \
<a href=\"{STANDARD_URL}\">IETF Remote Attestation Procedures (RATS) Architecture, \
RFC 9334</a>.</p>\n",
        operator = html_escape(operator),
        href = escape_attribute(verify_url),
        verifier = html_escape(&verifier),
        json_url = html_escape(json_url),
        pretty = html_escape(&pretty),
    );
    let script = "document.getElementById(\"copy\").addEventListener(\"click\",function(){\
var button=this;\
navigator.clipboard.writeText(document.getElementById(\"json-url\").textContent)\
.then(function(){button.textContent=\"Copied\";});});";
    html_document("Attestation", &body, script)
}

/// The list of pages served beneath the prefix, at the prefix itself.
fn render_index(page: &str, json: &str) -> String {
    let body = format!(
        "<h1>Information pages</h1>\n\
<p>This deployment serves these pages about itself.</p>\n\
<ul>\n\
<li><a href=\"{page}\">Attestation</a>, who operates this deployment \
and which build it runs, with a link to check the claim.</li>\n\
<li><a href=\"{json}\">Attestation as JSON</a>, the same signed \
evidence as data, for your own verification checks.</li>\n\
</ul>\n",
        page = escape_attribute(page),
        json = escape_attribute(json),
    );
    html_document("Information pages", &body, "")
}

/// The answer for any other address beneath a prefix the deployment owns.
fn render_not_found(prefix: &str) -> String {
    let body = format!(
        "<h1>Not found</h1>\n\
<p>Nothing is served at this address. The pages this deployment serves are listed at \
<a href=\"/{prefix}\">/{prefix}</a>.</p>\n",
        prefix = escape_attribute(prefix),
    );
    html_document("Not found", &body, "")
}

/// The shell every page here shares. It loads nothing from anywhere else.
fn html_document(title: &str, body: &str, script: &str) -> String {
    let script = if script.is_empty() {
        String::new()
    } else {
        format!("<script>\n{script}\n</script>\n")
    };
    format!(
        "<!DOCTYPE html>\n\
<html lang=\"en\">\n\
<head>\n\
<meta charset=\"utf-8\">\n\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
<meta name=\"robots\" content=\"noindex\">\n\
<title>{title}</title>\n\
<style>\n\
body{{font-family:system-ui,-apple-system,\"Segoe UI\",Roboto,sans-serif;\
margin:0;padding:2rem 1rem;background:#fff;color:#232628;line-height:1.5}}\n\
main{{max-width:52rem;margin:0 auto}}\n\
h1{{font-size:1.4rem;margin:0 0 .5rem}}\n\
p{{margin:0 0 1rem}}\n\
a.check{{font-weight:600}}\n\
pre{{background:#f5f5f5;padding:1rem;border-radius:6px;overflow-x:auto;\
font-size:.85rem;line-height:1.45}}\n\
.copy{{display:flex;gap:.5rem;align-items:center;margin:0 0 1rem}}\n\
.copy code{{flex:1;background:#f5f5f5;padding:.6rem .8rem;border-radius:6px;\
font-size:.85rem;overflow-x:auto;white-space:nowrap;user-select:all}}\n\
.copy button{{padding:.5rem .9rem;border:1px solid #c9ccce;border-radius:6px;\
background:#fff;color:inherit;font:inherit;cursor:pointer}}\n\
.standard{{margin-top:1.5rem;font-size:.85rem;color:#555}}\n\
@media(prefers-color-scheme:dark){{body{{background:#232628;color:#f4f4f4}}\
a{{color:#8cc8ff}}pre,.copy code{{background:#1a1c1d}}\
.copy button{{background:#2d3134;border-color:#4a4f52}}.standard{{color:#bbb}}}}\n\
</style>\n\
</head>\n\
<body>\n\
<main>\n\
{body}\
</main>\n\
{script}\
</body>\n\
</html>\n",
        title = html_escape(title),
    )
}

fn escape_attribute(value: &str) -> String {
    html_escape(value).replace('"', "&quot;")
}

/// Validates the endpoint, an absolute path of two or more segments of lower
/// case letters, digits, `_` and `-`, the first beginning with an underscore
/// so the pages sit apart from the publisher's own, and without `.json`,
/// which names the JSON form. Neither the page nor the JSON form may be one
/// of the [`RESERVED_PATHS`], and the prefix may not be one of the
/// [`ORIGIN_ASSET_PREFIXES`].
///
/// # Errors
///
/// Returns `invalid_endpoint` for any other shape, `reserved_endpoint`,
/// naming the address, when the page or the JSON form is reserved, and
/// `origin_prefix`, naming the prefix, when the prefix is one a framework
/// serves its assets from.
pub fn validate_endpoint(value: &str) -> Result<(), ValidationError> {
    let segment_ok = |segment: &str| {
        !segment.is_empty()
            && segment
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
    };
    let segments: Vec<&str> = value
        .strip_prefix('/')
        .map(|rest| rest.split('/').collect())
        .unwrap_or_default();
    let well_formed = value.len() <= 64
        && segments.len() >= 2
        && segments.iter().all(|segment| segment_ok(segment))
        && segments
            .first()
            .is_some_and(|first| first.len() > 1 && first.starts_with('_'));
    if !well_formed {
        return Err(ValidationError::new("invalid_endpoint"));
    }
    if let Some(reserved) = reserved_path_taken(value, RESERVED_PATHS) {
        let mut error = ValidationError::new("reserved_endpoint");
        error.message = Some(format!("{reserved} is answered by the deployment itself").into());
        return Err(error);
    }
    let prefix = prefix_of(value);
    if ORIGIN_ASSET_PREFIXES.contains(&prefix) {
        let mut error = ValidationError::new("origin_prefix");
        error.message =
            Some(format!("/{prefix} is where a web framework serves its assets").into());
        return Err(error);
    }
    Ok(())
}

/// The first of `reserved` that the endpoint's page or JSON form would take.
fn reserved_path_taken(endpoint: &str, reserved: &[&'static str]) -> Option<&'static str> {
    let addresses = paths_of(endpoint);
    reserved.iter().copied().find(|route| {
        addresses
            .iter()
            .any(|address| route_answers(route, address))
    })
}

/// Whether a route answers an address, a segment in braces answering any one
/// segment.
fn route_answers(route: &str, address: &str) -> bool {
    let mut route_segments = route.split('/');
    let mut address_segments = address.split('/');
    loop {
        match (route_segments.next(), address_segments.next()) {
            (None, None) => return true,
            (Some(expected), Some(given)) if expected == given || expected.starts_with('{') => {}
            _ => return false,
        }
    }
}

fn validate_single_line(value: &str) -> Result<(), ValidationError> {
    if value.chars().any(char::is_control) {
        Err(ValidationError::new("control_character"))
    } else {
        Ok(())
    }
}

fn validate_https_url(value: &str) -> Result<(), ValidationError> {
    match url::Url::parse(value) {
        Ok(url) if url.scheme() == "https" && url.host_str().is_some() => Ok(()),
        _ => Err(ValidationError::new("invalid_https_url")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::VerifyingKey;
    use p256::ecdsa::signature::Verifier as _;
    use p256::pkcs8::{DecodePublicKey as _, EncodePublicKey as _};

    use crate::test_support::tests::crate_test_settings_str;

    const CONTEXT: &str = "example-attestation:v1";
    const FIRST: [u8; 32] = [0x11; 32];
    const SECOND: [u8; 32] = [0x22; 32];
    const FIRST_STARTS: i64 = 1_000;
    const SECOND_STARTS: i64 = 2_000;

    fn secret_key(scalar: &[u8; 32]) -> p256::SecretKey {
        p256::SecretKey::from_slice(scalar).expect("a valid scalar")
    }

    /// A key as a schedule holds it, base64 of its SEC1 DER form.
    fn key_text(scalar: &[u8; 32]) -> String {
        STANDARD.encode(secret_key(scalar).to_sec1_der().expect("encodes"))
    }

    fn settings_with_attestation() -> Settings {
        let mut settings =
            Settings::from_toml(&crate_test_settings_str()).expect("test settings should load");
        settings.attestation = Some(AttestationConfig {
            endpoint: "/_ex/attestation".to_owned(),
            operator: "Example Operator".to_owned(),
            context: CONTEXT.to_owned(),
            verify_url: "https://verifier.example/verify?host=test-publisher.com".to_owned(),
        });
        settings
    }

    fn request(uri: &str) -> Request<EdgeBody> {
        Request::builder()
            .uri(uri)
            .header(header::HOST, "Test-Publisher.com:443")
            .body(EdgeBody::empty())
            .expect("request")
    }

    fn platform() -> PlatformIdentity {
        PlatformIdentity {
            name: "example-platform".to_owned(),
            service_id: "svc-1".to_owned(),
            service_version: "39".to_owned(),
            staging: false,
            pop: "LHR".to_owned(),
        }
    }

    /// Answers from a two key schedule at `now`.
    fn answer_at(settings: &Settings, uri: &str, now: u64) -> Response<EdgeBody> {
        let first = key_text(&FIRST);
        let second = key_text(&SECOND);
        let schedule = [
            ("example-first", FIRST_STARTS, first.as_str()),
            ("example-second", SECOND_STARTS, second.as_str()),
        ];
        answer(settings, &platform(), &request(uri), &schedule, now)
    }

    fn body_bytes(response: Response<EdgeBody>) -> Vec<u8> {
        response
            .into_body()
            .into_bytes()
            .expect("a buffered body")
            .to_vec()
    }

    fn json_body(response: Response<EdgeBody>) -> Value {
        serde_json::from_slice(&body_bytes(response)).expect("a JSON body")
    }

    fn verifying_key(scalar: &[u8; 32]) -> VerifyingKey {
        let spki = secret_key(scalar)
            .public_key()
            .to_public_key_der()
            .expect("encodes");
        VerifyingKey::from_public_key_der(spki.as_bytes()).expect("decodes")
    }

    fn payload_and_signature(envelope: &Value) -> (Vec<u8>, Signature) {
        let payload = URL_SAFE_NO_PAD
            .decode(envelope["payload"].as_str().expect("payload"))
            .expect("base64url payload");
        let raw = URL_SAFE_NO_PAD
            .decode(envelope["signature"]["value"].as_str().expect("value"))
            .expect("base64url signature");
        assert_eq!(raw.len(), 64, "the signature is the raw r and s pair");
        (payload, Signature::from_slice(&raw).expect("64 bytes"))
    }

    /// A build given no schedule, an empty one or blank lines compiles a
    /// schedule with no key in it, which is what makes the endpoint answer
    /// 503.
    #[test]
    fn a_build_given_no_schedule_compiles_an_empty_one() {
        let empty = "const ATTESTATION_KEYS: &[(&str, i64, &str)] = &[];";

        for (case, schedule) in [
            ("no schedule", None),
            ("an empty file", Some("")),
            ("blank lines", Some("\n  \n\r\n")),
        ] {
            assert_eq!(
                key_schedule::constant_source(schedule).as_deref(),
                Ok(empty),
                "{case} should compile an empty schedule"
            );
        }
    }

    /// Each line becomes one key, in the order written, whichever line ending
    /// the file was saved with.
    #[test]
    fn each_line_of_a_schedule_becomes_a_key() {
        let first = key_text(&FIRST);
        let second = key_text(&SECOND);
        let expected = format!(
            "const ATTESTATION_KEYS: &[(&str, i64, &str)] = &[\
             (\"example-first\", 1000, {first:?}),(\"example-second\", 2000, {second:?}),];"
        );

        for ending in ["\n", "\r\n"] {
            let schedule = format!(
                "example-first\t1000\t{first}{ending}example-second\t2000\t{second}{ending}"
            );
            assert_eq!(
                key_schedule::constant_source(Some(&schedule)),
                Ok(expected.clone()),
                "should read both keys from a file with {ending:?} line endings"
            );
        }
    }

    /// A line of any other shape stops the build. Its key is never repeated
    /// in the message, and nothing it holds can end the string it would be
    /// written in.
    #[test]
    fn a_schedule_line_of_any_other_shape_is_refused_without_showing_the_key() {
        let key = key_text(&FIRST);
        let cases = [
            ("two fields", format!("example-first\t{key}")),
            ("four fields", format!("example-first\t1000\t{key}\textra")),
            (
                "spaces in place of tabs",
                format!("example-first 1000 {key}"),
            ),
            (
                "a start that is not a number",
                format!("example-first\tsoon\t{key}"),
            ),
            ("an empty key id", format!("\t1000\t{key}")),
            ("an empty key", "example-first\t1000\t".to_owned()),
            (
                "a key id that would end its string",
                format!("example\"); evil(\"\t1000\t{key}"),
            ),
            (
                "a key that would end its string",
                format!("example-first\t1000\t{key}\"), (\"x"),
            ),
            (
                "a key holding a backslash",
                format!("example-first\t1000\t{key}\\"),
            ),
        ];
        for (case, line) in cases {
            let schedule = format!("example-zero\t500\t{key}\n{line}\n");
            let refused = key_schedule::constant_source(Some(&schedule))
                .expect_err("should refuse the schedule");

            assert!(
                refused.starts_with("line 2 "),
                "{case} should be refused by its line number, got: {refused}"
            );
            assert!(
                !refused.contains(&key),
                "{case} should be refused without repeating the key"
            );
        }
    }

    /// Two lines sharing a key id or a start stop the build, so the key in
    /// force never depends on the order of the lines and a key id names one
    /// key.
    #[test]
    fn a_schedule_repeating_a_key_id_or_a_start_is_refused() {
        let first = key_text(&FIRST);
        let second = key_text(&SECOND);
        for (case, schedule, refusal) in [
            (
                "a repeated key id",
                format!("example-first\t1000\t{first}\nexample-first\t2000\t{second}\n"),
                "line 2 should not repeat the key id of line 1",
            ),
            (
                "a repeated start",
                format!("example-first\t1000\t{first}\n\nexample-second\t1000\t{second}\n"),
                "line 3 should not start at the same second as line 1",
            ),
        ] {
            assert_eq!(
                key_schedule::constant_source(Some(&schedule)),
                Err(refusal.to_owned()),
                "{case} should be refused"
            );
        }
    }

    /// A verifier with only the public key, in the `SubjectPublicKeyInfo` DER
    /// form a key schedule publishes, accepts the signature over exactly the
    /// context, one line feed, then the payload. Without the context, without
    /// its line feed, or under another context it never verifies, which is
    /// what binds a signature to its purpose.
    #[test]
    fn the_signature_covers_the_context_a_line_feed_and_the_payload() {
        let envelope = json_body(answer_at(
            &settings_with_attestation(),
            "/_ex/attestation.json?nonce=abc123",
            1_500,
        ));
        assert_eq!(envelope["signature"]["alg"], "ES256");
        let (payload, signature) = payload_and_signature(&envelope);
        let verifier = verifying_key(&FIRST);

        let bytes = signed_bytes(CONTEXT, &payload);
        assert!(bytes.starts_with(CONTEXT.as_bytes()));
        assert_eq!(bytes[CONTEXT.len()], b'\n');
        assert_eq!(&bytes[CONTEXT.len() + 1..], payload.as_slice());
        verifier
            .verify(&bytes, &signature)
            .expect("the signature covers context, line feed and payload");

        for (case, message) in [
            ("no context", payload.clone()),
            (
                "the context without its line feed",
                [CONTEXT.as_bytes(), payload.as_slice()].concat(),
            ),
            (
                "another context",
                signed_bytes("another-purpose:v1", &payload),
            ),
        ] {
            assert!(
                verifier.verify(&message, &signature).is_err(),
                "the signature must not verify with {case}",
            );
        }
    }

    /// Each key signs from its own start until the next key's start, and the
    /// evidence of a time verifies only with the key in force then.
    #[test]
    fn the_key_in_force_is_the_latest_to_have_started() {
        let settings = settings_with_attestation();
        for (now, kid, signing, other) in [
            (1_000, "example-first", &FIRST, &SECOND),
            (1_500, "example-first", &FIRST, &SECOND),
            (1_999, "example-first", &FIRST, &SECOND),
            (2_000, "example-second", &SECOND, &FIRST),
            (2_500, "example-second", &SECOND, &FIRST),
        ] {
            let envelope = json_body(answer_at(&settings, "/_ex/attestation.json", now));
            assert_eq!(envelope["signature"]["kid"], kid, "at {now}");
            let (payload, signature) = payload_and_signature(&envelope);
            let bytes = signed_bytes(CONTEXT, &payload);
            assert!(
                verifying_key(signing).verify(&bytes, &signature).is_ok(),
                "at {now} the key in force signs"
            );
            assert!(
                verifying_key(other).verify(&bytes, &signature).is_err(),
                "at {now} the other key must not verify"
            );
        }
    }

    /// The warning that the last key built in is signing is given once for
    /// each process, not on every request.
    #[test]
    fn the_last_key_warning_is_given_once() {
        let warned = AtomicBool::new(false);

        assert!(first_time(&warned), "should warn the first time");
        assert!(!first_time(&warned), "should not warn again");
        assert!(!first_time(&warned), "should not warn on any later request");
    }

    /// Nothing is signed before the first key starts, from an empty schedule,
    /// with a key that is not a P-256 private key, or in a build given no
    /// schedule, which is every build its builder names no schedule for.
    #[test]
    fn no_usable_key_in_force_answers_service_unavailable() {
        assert!(ATTESTATION_KEYS.is_empty(), "test builds carry no keys");
        let settings = settings_with_attestation();
        let first = key_text(&FIRST);
        let second = key_text(&SECOND);
        let ed25519_seed = STANDARD.encode([0x33u8; 32]);
        let no_key = "No signing key is built in for this time";
        let unreadable = "The signing key could not be read";
        let cases: [(&str, Vec<ScheduledKey<'_>>, u64, &str); 5] = [
            (
                "before the first key starts",
                vec![
                    ("example-first", FIRST_STARTS, first.as_str()),
                    ("example-second", SECOND_STARTS, second.as_str()),
                ],
                999,
                no_key,
            ),
            ("an empty schedule", Vec::new(), 1_500, no_key),
            (
                "a key that is not base64",
                vec![("example-bad", FIRST_STARTS, "not base64 at all")],
                1_500,
                unreadable,
            ),
            (
                "a key that is not a P-256 private key",
                vec![("example-bad", FIRST_STARTS, ed25519_seed.as_str())],
                1_500,
                unreadable,
            ),
            (
                "the schedule this build was given",
                ATTESTATION_KEYS.to_vec(),
                crate::ec::current_timestamp(),
                no_key,
            ),
        ];
        for (case, schedule, now, says) in cases {
            let response = answer(
                &settings,
                &platform(),
                &request("/_ex/attestation.json"),
                &schedule,
                now,
            );
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "{case}");
            let body = json_body(response);
            assert!(
                body["error"]
                    .as_str()
                    .is_some_and(|error| error.starts_with(says)),
                "{case}: {body}"
            );
        }
    }

    /// The decoded evidence shown to people is exactly the payload signed.
    #[test]
    fn the_evidence_shown_is_the_payload_signed() {
        let envelope = json_body(answer_at(
            &settings_with_attestation(),
            "/_ex/attestation.json?nonce=abc123",
            1_500,
        ));
        let (payload, _) = payload_and_signature(&envelope);
        let decoded: Value = serde_json::from_slice(&payload).expect("JSON payload");

        assert_eq!(decoded, envelope["evidence"]);
        assert_eq!(decoded["v"], 1);
        assert_eq!(decoded["operator"], "Example Operator");
        assert_eq!(decoded["host"], "test-publisher.com");
        assert_eq!(decoded["nonce"], "abc123");
        assert_eq!(decoded["platform"]["serviceVersion"], "39");
        assert_eq!(decoded["build"]["version"], crate::build_info::VERSION);
        assert_eq!(decoded["issuedAt"], "1970-01-01T00:25:00Z");
        assert_eq!(
            envelope["verify"],
            "https://verifier.example/verify?host=test-publisher.com"
        );
    }

    /// With no nonce asked for, the evidence carries none.
    #[test]
    fn no_nonce_is_signed_unless_asked_for() {
        let envelope = json_body(answer_at(
            &settings_with_attestation(),
            "/_ex/attestation.json",
            1_500,
        ));
        assert!(envelope["evidence"].get("nonce").is_none());
    }

    #[test]
    fn a_malformed_nonce_is_refused() {
        let settings = settings_with_attestation();
        for uri in [
            "/_ex/attestation.json?nonce=",
            "/_ex/attestation.json?nonce=a%20b",
            "/_ex/attestation.json?nonce=%3Cscript%3E",
        ] {
            let response = answer_at(&settings, uri, 1_500);
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
        }
        let long = format!("/_ex/attestation.json?nonce={}", "a".repeat(65));
        let response = answer_at(&settings, &long, 1_500);
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    /// A key held as PKCS #8 signs the same way as one held as SEC1.
    #[test]
    fn a_pkcs8_key_signs_too() {
        use p256::pkcs8::EncodePrivateKey as _;
        let pkcs8 = STANDARD.encode(
            secret_key(&FIRST)
                .to_pkcs8_der()
                .expect("encodes")
                .as_bytes(),
        );
        let schedule = [("example-pkcs8", FIRST_STARTS, pkcs8.as_str())];
        let response = answer(
            &settings_with_attestation(),
            &platform(),
            &request("/_ex/attestation.json"),
            &schedule,
            1_500,
        );
        assert_eq!(response.status(), StatusCode::OK);
    }

    /// The page never says the evidence is valid, because only the verifier
    /// can, and it links there.
    #[test]
    fn the_page_links_to_the_verifier_and_claims_nothing() {
        let response = answer_at(&settings_with_attestation(), "/_ex/attestation", 1_500);
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/html; charset=utf-8"
        );
        let html = String::from_utf8(body_bytes(response)).expect("utf-8");
        assert!(html.contains("Check this claim at verifier.example"));
        assert!(html.contains("href=\"https://verifier.example/verify?host=test-publisher.com\""));
        assert!(html.contains("Do not take this page's word for it."));
        // The JSON shown is escaped as text, which leaves quotes as they are.
        assert!(
            !html.contains("\"valid\""),
            "the JSON shown carries no claim of validity"
        );
        assert!(html.contains(
            "To retrieve this data as JSON for use in your own verification checks use the URL"
        ));
        assert!(
            html.contains("/_ex/attestation.json</code>"),
            "the JSON address is offered to copy",
        );
        assert!(html.contains("The JSON response is"));
        assert!(html.contains("href=\"https://www.rfc-editor.org/rfc/rfc9334\""));
    }

    /// The address offered to copy is absolute, and its scheme is the one the
    /// request arrived over.
    #[test]
    fn the_json_address_carries_the_scheme_and_host_the_request_used() {
        let mut req = request("/_ex/attestation");
        assert_eq!(
            json_address(&req),
            "http://Test-Publisher.com:443/_ex/attestation.json"
        );
        req.headers_mut()
            .insert("x-forwarded-proto", HeaderValue::from_static("https"));
        assert!(json_address(&req).starts_with("https://"));
    }

    /// The link names the host the evidence signs, never a forwarded one, on
    /// an adapter that leaves forwarded host headers in place.
    #[test]
    fn the_json_address_never_names_a_forwarded_host() {
        for (name, value) in [
            ("x-forwarded-host", "relay.example"),
            ("forwarded", "host=relay.example"),
        ] {
            let mut req = request("/_ex/attestation");
            req.headers_mut()
                .insert(name, HeaderValue::from_static(value));

            let address = json_address(&req);

            assert!(
                address.contains("://Test-Publisher.com:443/") && !address.contains("relay"),
                "{name} should not reach the link: {address}"
            );
        }
    }

    #[test]
    fn answers_are_never_cached_and_readable_from_any_origin() {
        let settings = settings_with_attestation();
        for uri in ["/_ex/attestation", "/_ex/attestation.json"] {
            let response = answer_at(&settings, uri, 1_500);
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            assert_eq!(response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
        }
    }

    #[test]
    fn the_paths_follow_the_endpoint_and_default_to_ts() {
        let mut settings = settings_with_attestation();
        assert_eq!(
            attestation_paths(&settings),
            Some([
                "/_ex/attestation".to_owned(),
                "/_ex/attestation.json".to_owned()
            ])
        );
        if let Some(config) = settings.attestation.as_mut() {
            config.endpoint = DEFAULT_ENDPOINT.to_owned();
        }
        assert_eq!(
            attestation_paths(&settings),
            Some([
                "/_ts/attestation".to_owned(),
                "/_ts/attestation.json".to_owned()
            ])
        );
        settings.attestation = None;
        assert_eq!(attestation_paths(&settings), None);
    }

    #[test]
    fn the_endpoint_is_an_absolute_path_under_an_underscore_prefix() {
        for good in [
            "/_ts/attestation",
            "/_ex/attestation",
            "/_a1/proof-of_operator",
            "/_ex/pages/attestation",
        ] {
            assert!(validate_endpoint(good).is_ok(), "{good}");
        }
        let too_long = format!("/_ex/{}", "a".repeat(60));
        for bad in [
            "",
            "/",
            "_ex/attestation",
            "/attestation",
            "/ex/attestation",
            "/_/attestation",
            "/_ex",
            "/_ex/",
            "/_ex//attestation",
            "/_Ex/attestation",
            "/_ex/attestation.json",
            "/_ex/attestation/",
            "/_ex/attestation?nonce=1",
            too_long.as_str(),
        ] {
            assert!(validate_endpoint(bad).is_err(), "{bad}");
        }
    }

    /// An endpoint owns its prefix and answers `404` for every other address
    /// beneath it, so a prefix a framework serves its assets from is refused,
    /// naming it.
    #[test]
    fn an_endpoint_under_a_framework_asset_prefix_is_refused_naming_it() {
        for prefix in ORIGIN_ASSET_PREFIXES {
            let endpoint = format!("/{prefix}/attestation");
            let error = validate_endpoint(&endpoint).expect_err("should refuse the endpoint");

            assert_eq!(error.code, "origin_prefix", "{endpoint} should be refused");
            assert_eq!(
                error.message.as_deref(),
                Some(format!("/{prefix} is where a web framework serves its assets").as_str()),
                "{endpoint} should name its prefix"
            );
        }
    }

    #[test]
    fn the_section_loads_from_a_document_and_refuses_unknown_fields() {
        let base = crate_test_settings_str();
        let with_section = format!(
            "{base}\n[attestation]\n\
             endpoint = \"/_ex/attestation\"\n\
             operator = \"Example Operator\"\n\
             context = \"example-attestation:v1\"\n\
             verify_url = \"https://verifier.example/verify\"\n"
        );
        let settings = Settings::from_toml(&with_section).expect("the section loads");
        let config = settings.attestation.as_ref().expect("the section is read");
        assert_eq!(config.endpoint, "/_ex/attestation");
        assert_eq!(config.operator, "Example Operator");

        let without_endpoint = with_section.replace("endpoint = \"/_ex/attestation\"\n", "");
        let settings = Settings::from_toml(&without_endpoint).expect("the endpoint has a default");
        assert_eq!(
            settings.attestation.as_ref().map(|a| a.endpoint.as_str()),
            Some(DEFAULT_ENDPOINT)
        );

        let bad_endpoint = with_section.replace("/_ex/attestation", "/_ex/attestation.json");
        assert!(
            Settings::from_toml(&bad_endpoint).is_err(),
            "the JSON form is derived, so an endpoint ending in .json is refused",
        );

        let with_key_name = format!("{with_section}signing_key = \"a_key\"\n");
        assert!(
            Settings::from_toml(&with_key_name).is_err(),
            "keys are built in, so a document naming one is refused",
        );

        let plain_http = with_section.replace("https://verifier", "http://verifier");
        assert!(Settings::from_toml(&plain_http).is_err());
    }

    /// Every field the evidence depends on is checked when the settings load.
    /// The context is signed ahead of a line feed, so one holding a control
    /// character would make that framing ambiguous, and the operator and the
    /// context are each 1 to 64 characters.
    #[test]
    fn each_field_the_evidence_depends_on_is_checked_at_load() {
        let base = crate_test_settings_str();
        let document = |operator: Option<&str>, context: Option<&str>, verify_url: Option<&str>| {
            let mut text = format!("{base}\n[attestation]\n");
            for (key, value) in [
                ("operator", operator),
                ("context", context),
                ("verify_url", verify_url),
            ] {
                if let Some(value) = value {
                    text.push_str(&format!("{key} = \"{value}\"\n"));
                }
            }
            text
        };
        let operator = Some("Example Operator");
        let context = Some("example-attestation:v1");
        let verify_url = Some("https://verifier.example/verify");
        let longest = "a".repeat(64);
        let too_long = "a".repeat(65);

        Settings::from_toml(&document(
            Some(longest.as_str()),
            Some(longest.as_str()),
            verify_url,
        ))
        .expect("an operator and a context of 64 characters load");
        for (case, (operator, context, verify_url), refusal) in [
            (
                "a context holding a line feed",
                (operator, Some("example\\nattestation"), verify_url),
                "attestation.context: control_character",
            ),
            (
                "an empty operator",
                (Some(""), context, verify_url),
                "attestation.operator: length",
            ),
            (
                "a 65 character operator",
                (Some(too_long.as_str()), context, verify_url),
                "attestation.operator: length",
            ),
            (
                "an empty context",
                (operator, Some(""), verify_url),
                "attestation.context: length",
            ),
            (
                "a 65 character context",
                (operator, Some(too_long.as_str()), verify_url),
                "attestation.context: length",
            ),
            (
                "no operator",
                (None, context, verify_url),
                "missing field `operator`",
            ),
            (
                "no context",
                (operator, None, verify_url),
                "missing field `context`",
            ),
            (
                "no verify_url",
                (operator, context, None),
                "missing field `verify_url`",
            ),
            (
                "a verify_url with no host",
                (operator, context, Some("https://")),
                "attestation.verify_url: invalid_https_url",
            ),
        ] {
            let Err(error) = Settings::from_toml(&document(operator, context, verify_url)) else {
                panic!("{case} should be refused");
            };
            let text = format!("{error:?}");
            assert!(
                text.contains(refusal),
                "{case} should be refused as {refusal}: {text}"
            );
        }
    }

    /// An endpoint whose page or JSON form is an address the deployment
    /// answers itself is refused when the settings load, naming that address,
    /// because a second route there stops the router being built.
    #[test]
    fn an_endpoint_at_a_reserved_address_is_refused_naming_it() {
        let base = crate_test_settings_str();
        let document = |endpoint: &str| {
            format!(
                "{base}\n[attestation]\n\
                 endpoint = \"{endpoint}\"\n\
                 operator = \"Example Operator\"\n\
                 context = \"example-attestation:v1\"\n\
                 verify_url = \"https://verifier.example/verify\"\n"
            )
        };
        for (endpoint, reserved) in [
            ("/_ts/config", "/_ts/config"),
            ("/_ts/permissions", "/_ts/permissions"),
            ("/_ts/set-tester", "/_ts/set-tester"),
            ("/_ts/api/v1/identify", "/_ts/api/v1/identify"),
            ("/__ts/page-bids", "/__ts/page-bids"),
        ] {
            let Err(error) = Settings::from_toml(&document(endpoint)) else {
                panic!("{endpoint} should be refused");
            };
            let text = format!("{error:?}");
            assert!(
                text.contains("attestation.endpoint: reserved_endpoint"),
                "{endpoint} should be refused as reserved: {text}"
            );
            assert!(
                text.contains(&format!(
                    "({reserved} is answered by the deployment itself)"
                )),
                "{endpoint} should name {reserved}: {text}"
            );
        }
        for endpoint in [
            DEFAULT_ENDPOINT,
            "/_ex/attestation",
            "/_ts/proof",
            "/_ts/configuration",
        ] {
            let settings = Settings::from_toml(&document(endpoint))
                .unwrap_or_else(|error| panic!("{endpoint} should load: {error:?}"));
            assert_eq!(
                settings.attestation.as_ref().map(|a| a.endpoint.as_str()),
                Some(endpoint)
            );
        }
    }

    /// The JSON form is checked as well as the page, and a segment in braces
    /// stands for any one segment.
    #[test]
    fn both_forms_are_checked_against_the_reserved_addresses() {
        assert_eq!(
            reserved_path_taken("/_ex/page", &["/_ex/page.json"]),
            Some("/_ex/page.json"),
            "the JSON form clashes"
        );
        assert_eq!(
            reserved_path_taken("/_ex/page", &["/_ex/{name}"]),
            Some("/_ex/{name}"),
            "a segment in braces answers the page"
        );
        assert_eq!(
            reserved_path_taken("/_ex/page", &["/_ex", "/_ex/page/more", "/_ex/other.json"]),
            None,
            "a parent, a child and another JSON form do not clash"
        );
    }

    fn request_with(method: Method, uri: &str) -> Request<EdgeBody> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header(header::HOST, "Test-Publisher.com:443")
            .body(EdgeBody::empty())
            .expect("request")
    }

    fn html(response: Response<EdgeBody>) -> String {
        String::from_utf8(body_bytes(response)).expect("utf-8")
    }

    /// The routes as (path, every method) pairs.
    fn routes_of(settings: &Settings) -> Vec<(String, bool)> {
        prefix_routes(settings)
            .into_iter()
            .map(|route| (route.path, route.every_method))
            .collect()
    }

    /// A deployment whose endpoint sits under a prefix of its own owns all
    /// of that prefix and answers the default address with a redirect. The
    /// default prefix gives up only the pages themselves, because other
    /// routes share it.
    #[test]
    fn an_own_prefix_is_owned_and_the_default_is_shared() {
        let mut settings = settings_with_attestation();
        assert_eq!(
            routes_of(&settings),
            [
                ("/_ex/attestation".to_owned(), true),
                ("/_ex/attestation.json".to_owned(), true),
                ("/_ts/attestation".to_owned(), false),
                ("/_ts/attestation.json".to_owned(), false),
                ("/_ex".to_owned(), true),
                ("/_ex/".to_owned(), true),
                ("/_ex/{*rest}".to_owned(), true),
            ]
        );

        if let Some(config) = settings.attestation.as_mut() {
            config.endpoint = "/_ts/proof".to_owned();
        }
        assert_eq!(
            routes_of(&settings),
            [
                ("/_ts/proof".to_owned(), false),
                ("/_ts/proof.json".to_owned(), false),
                ("/_ts/attestation".to_owned(), false),
                ("/_ts/attestation.json".to_owned(), false),
            ]
        );

        if let Some(config) = settings.attestation.as_mut() {
            config.endpoint = DEFAULT_ENDPOINT.to_owned();
        }
        assert_eq!(
            routes_of(&settings),
            [
                ("/_ts/attestation".to_owned(), false),
                ("/_ts/attestation.json".to_owned(), false),
            ]
        );

        settings.attestation = None;
        assert!(prefix_routes(&settings).is_empty());
    }

    /// When the endpoint is not the default, a read of the default address
    /// is sent to the endpoint, page to page and JSON to JSON, with the query
    /// kept so a nonce survives. Other methods there are not found.
    #[test]
    fn reads_of_the_default_address_redirect_to_the_endpoint() {
        let settings = settings_with_attestation();
        for (method, uri, location) in [
            (Method::GET, "/_ts/attestation", "/_ex/attestation"),
            (Method::HEAD, "/_ts/attestation", "/_ex/attestation"),
            (
                Method::GET,
                "/_ts/attestation.json?nonce=abc123",
                "/_ex/attestation.json?nonce=abc123",
            ),
            (
                Method::HEAD,
                "/_ts/attestation.json",
                "/_ex/attestation.json",
            ),
        ] {
            let case = format!("{method} {uri}");
            let response = handle_prefix(&settings, &platform(), &request_with(method, uri));
            assert_eq!(response.status(), StatusCode::MOVED_PERMANENTLY, "{case}");
            assert_eq!(response.headers()[header::LOCATION], location, "{case}");
            assert_eq!(
                response.headers()[header::CACHE_CONTROL],
                "no-store",
                "{case}"
            );
            assert_eq!(json_body(response)["location"], location, "{case}");
        }

        let post = handle_prefix(
            &settings,
            &platform(),
            &request_with(Method::POST, "/_ts/attestation"),
        );
        assert_eq!(post.status(), StatusCode::NOT_FOUND);
    }

    /// With the default endpoint the default address is the endpoint, so it
    /// is answered there and nothing redirects.
    #[test]
    fn the_default_address_is_answered_directly_under_the_default_endpoint() {
        let mut settings = settings_with_attestation();
        if let Some(config) = settings.attestation.as_mut() {
            config.endpoint = DEFAULT_ENDPOINT.to_owned();
        }
        for uri in ["/_ts/attestation", "/_ts/attestation.json"] {
            let response = handle_prefix(&settings, &platform(), &request(uri));
            assert_eq!(
                response.status(),
                StatusCode::SERVICE_UNAVAILABLE,
                "{uri} reaches the handler, which has no key built in",
            );
            assert!(response.headers().get(header::LOCATION).is_none(), "{uri}");
        }
        let response = handle_prefix(&settings, &platform(), &request("/_ex/attestation"));
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    /// With no `[attestation]` section nothing is signed. The prefix handler
    /// and the page handler both answer every address not found, in JSON,
    /// even with a key in force.
    #[test]
    fn without_a_section_every_address_is_not_found_in_json() {
        let mut settings = settings_with_attestation();
        settings.attestation = None;
        for uri in [
            "/_ts/attestation",
            "/_ts/attestation.json?nonce=abc123",
            "/_ex/attestation",
        ] {
            for (handler, response) in [
                (
                    "prefix",
                    handle_prefix(&settings, &platform(), &request(uri)),
                ),
                ("page", answer_at(&settings, uri, 1_500)),
            ] {
                let case = format!("{handler} {uri}");
                assert_eq!(response.status(), StatusCode::NOT_FOUND, "{case}");
                assert_eq!(
                    response.headers()[header::CONTENT_TYPE],
                    "application/json",
                    "{case}"
                );
                assert_eq!(
                    json_body(response)["error"],
                    "This deployment serves no attestation.",
                    "{case}"
                );
            }
        }
    }

    /// The prefix itself lists the pages beneath it.
    #[test]
    fn the_prefix_lists_its_pages() {
        let settings = settings_with_attestation();
        for uri in ["/_ex", "/_ex/"] {
            let response = handle_prefix(&settings, &platform(), &request(uri));
            assert_eq!(response.status(), StatusCode::OK, "{uri}");
            let page = html(response);
            assert!(page.contains("href=\"/_ex/attestation\""), "{uri}");
            assert!(page.contains("href=\"/_ex/attestation.json\""), "{uri}");
        }
    }

    /// Any other address beneath an owned prefix is not found, and says where
    /// the pages are.
    #[test]
    fn any_other_address_beneath_the_prefix_is_not_found() {
        let settings = settings_with_attestation();
        let response = handle_prefix(&settings, &platform(), &request("/_ex/nothing/here"));
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(html(response).contains("href=\"/_ex\""));

        let post = handle_prefix(
            &settings,
            &platform(),
            &request_with(Method::POST, "/_ex/attestation"),
        );
        assert_eq!(
            post.status(),
            StatusCode::NOT_FOUND,
            "only reads are answered"
        );
    }

    /// A read of an attestation address reaches the attestation handler,
    /// which with no key built in answers that it cannot sign.
    #[test]
    fn a_read_of_an_attestation_address_reaches_the_attestation_handler() {
        let settings = settings_with_attestation();
        for method in [Method::GET, Method::HEAD] {
            let response = handle_prefix(
                &settings,
                &platform(),
                &request_with(method.clone(), "/_ex/attestation.json"),
            );
            assert_eq!(
                response.status(),
                StatusCode::SERVICE_UNAVAILABLE,
                "{method}"
            );
        }
    }

    /// The evidence signed for a request whose Host header is `host`, with
    /// any further headers given.
    fn evidence_for(host: &str, headers: &[(&str, &str)]) -> Value {
        let mut builder = Request::builder()
            .uri("/_ex/attestation.json")
            .header(header::HOST, host);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let req = builder.body(EdgeBody::empty()).expect("request");
        let first = key_text(&FIRST);
        let schedule = [("example-first", FIRST_STARTS, first.as_str())];
        let envelope = json_body(answer(
            &settings_with_attestation(),
            &platform(),
            &req,
            &schedule,
            1_500,
        ));
        let (payload, _) = payload_and_signature(&envelope);
        serde_json::from_slice(&payload).expect("JSON payload")
    }

    /// The signed host is the Host header the request arrived with. A
    /// forwarded host header, which a relay can set, never replaces it.
    #[test]
    fn forwarded_host_headers_never_become_the_signed_host() {
        let evidence = evidence_for(
            "test-publisher.com",
            &[
                ("x-forwarded-host", "relay.example"),
                ("forwarded", "host=relay.example;proto=https"),
                ("x-original-host", "relay.example"),
            ],
        );
        assert_eq!(evidence["host"], "test-publisher.com");
    }

    /// The signed publisher comes from the settings, so a request under
    /// another name changes the signed host and never the publisher.
    #[test]
    fn the_signed_publisher_comes_from_the_settings() {
        let settings = settings_with_attestation();
        let evidence = evidence_for("www.relay.example", &[]);
        assert_eq!(evidence["host"], "www.relay.example");
        assert_eq!(evidence["publisher"], settings.publisher.domain.as_str());
    }

    /// Without a Host header the host comes from the request address, lower
    /// cased and without its port. An IPv6 literal keeps its brackets.
    #[test]
    fn the_host_falls_back_to_the_request_address() {
        let absolute = Request::builder()
            .uri("https://Test-Publisher.com:8443/_ex/attestation.json")
            .body(EdgeBody::empty())
            .expect("request");
        assert_eq!(request_host(&absolute), "test-publisher.com");
        let literal = Request::builder()
            .uri("/_ex/attestation.json")
            .header(header::HOST, "[::1]:8080")
            .body(EdgeBody::empty())
            .expect("request");
        assert_eq!(request_host(&literal), "[::1]");
    }

    /// Changing a signed byte, here the host, stops the signature verifying,
    /// so a copy cannot be edited to name another site.
    #[test]
    fn an_altered_payload_fails_verification() {
        let envelope = json_body(answer_at(
            &settings_with_attestation(),
            "/_ex/attestation.json?nonce=abc123",
            1_500,
        ));
        let (payload, signature) = payload_and_signature(&envelope);
        let text = String::from_utf8(payload).expect("utf-8");
        let altered = text.replacen(
            "\"host\":\"test-publisher.com\"",
            "\"host\":\"relay.example\"",
            1,
        );
        assert_ne!(altered, text, "the host is in the payload");
        assert!(
            verifying_key(&FIRST)
                .verify(&signed_bytes(CONTEXT, altered.as_bytes()), &signature)
                .is_err(),
            "an altered payload must not verify",
        );
    }

    /// Signing is deterministic, as RFC 6979 describes, so the same evidence
    /// always carries the same signature.
    #[test]
    fn the_same_evidence_carries_the_same_signature() {
        let settings = settings_with_attestation();
        let uri = "/_ex/attestation.json?nonce=abc123";
        let first = json_body(answer_at(&settings, uri, 1_500));
        let second = json_body(answer_at(&settings, uri, 1_500));
        assert_eq!(first["payload"], second["payload"]);
        assert_eq!(first["signature"]["value"], second["signature"]["value"]);
    }

    /// The envelope carries exactly the documented fields.
    #[test]
    fn the_envelope_carries_exactly_the_documented_fields() {
        let envelope = json_body(answer_at(
            &settings_with_attestation(),
            "/_ex/attestation.json",
            1_500,
        ));
        let names = |value: &Value| {
            let mut names: Vec<String> = value
                .as_object()
                .expect("an object")
                .keys()
                .cloned()
                .collect();
            names.sort();
            names
        };
        assert_eq!(
            names(&envelope),
            ["evidence", "payload", "signature", "verify"]
        );
        assert_eq!(names(&envelope["signature"]), ["alg", "kid", "value"]);
    }

    /// The evidence fields are signed in the documented order.
    #[test]
    fn the_evidence_fields_are_signed_in_the_documented_order() {
        let envelope = json_body(answer_at(
            &settings_with_attestation(),
            "/_ex/attestation.json?nonce=abc123",
            1_500,
        ));
        let (payload, _) = payload_and_signature(&envelope);
        let text = String::from_utf8(payload).expect("utf-8");
        let order = [
            "\"v\":",
            "\"operator\":",
            "\"publisher\":",
            "\"host\":",
            "\"platform\":",
            "\"name\":",
            "\"serviceId\":",
            "\"serviceVersion\":",
            "\"staging\":",
            "\"pop\":",
            "\"build\":",
            "\"version\":",
            "\"builtAt\":",
            "\"commit\":",
            "\"run\":",
            "\"nonce\":",
            "\"issuedAt\":",
        ];
        let positions: Vec<usize> = order
            .iter()
            .map(|field| {
                text.find(field)
                    .unwrap_or_else(|| panic!("{field} is signed: {text}"))
            })
            .collect();
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "{text}");
    }

    /// The longest nonce allowed is signed back unchanged, and a nonce sent
    /// percent-encoded is signed as its decoded value.
    #[test]
    fn a_nonce_is_signed_back_as_sent() {
        let settings = settings_with_attestation();
        let longest = "a".repeat(64);
        let envelope = json_body(answer_at(
            &settings,
            &format!("/_ex/attestation.json?nonce={longest}"),
            1_500,
        ));
        let (payload, _) = payload_and_signature(&envelope);
        let evidence: Value = serde_json::from_slice(&payload).expect("JSON payload");
        assert_eq!(evidence["nonce"], longest.as_str());
        let encoded = json_body(answer_at(
            &settings,
            "/_ex/attestation.json?nonce=Ab_9%2Dz",
            1_500,
        ));
        assert_eq!(encoded["evidence"]["nonce"], "Ab_9-z");
    }

    /// Markup from the document reaches the page as text, in the claim and
    /// in the JSON shown.
    #[test]
    fn the_page_escapes_what_it_shows() {
        let mut settings = settings_with_attestation();
        if let Some(config) = settings.attestation.as_mut() {
            config.operator = "<b>Example</b> & Co".to_owned();
        }
        let page = html(answer_at(&settings, "/_ex/attestation", 1_500));
        assert!(!page.contains("<b>Example</b>"), "{page}");
        assert!(page.contains("&lt;b&gt;Example&lt;/b&gt; &amp; Co"));
    }

    /// The evidence reports the build identity compiled in and the platform
    /// as the adapter describes it, staging included.
    #[test]
    fn the_evidence_reports_the_build_and_the_platform() {
        let envelope = json_body(answer_at(
            &settings_with_attestation(),
            "/_ex/attestation.json",
            1_500,
        ));
        let evidence = &envelope["evidence"];
        assert_eq!(evidence["build"]["commit"], crate::build_info::COMMIT);
        assert_eq!(evidence["build"]["run"], crate::build_info::BUILD_RUN);
        assert_eq!(evidence["build"]["builtAt"], crate::build_info::BUILT_AT);
        assert_eq!(evidence["platform"]["name"], "example-platform");
        assert_eq!(evidence["platform"]["serviceId"], "svc-1");
        assert_eq!(evidence["platform"]["staging"], false);
        assert_eq!(evidence["platform"]["pop"], "LHR");

        let mut staged = platform();
        staged.staging = true;
        let first = key_text(&FIRST);
        let schedule = [("example-first", FIRST_STARTS, first.as_str())];
        let from_staging = json_body(answer(
            &settings_with_attestation(),
            &staged,
            &request("/_ex/attestation.json"),
            &schedule,
            1_500,
        ));
        assert_eq!(from_staging["evidence"]["platform"]["staging"], true);
    }
}
