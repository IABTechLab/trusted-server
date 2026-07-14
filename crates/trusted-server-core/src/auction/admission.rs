//! Shared auction admission contract.
//!
//! This module owns the request-admission boundary before auction provider
//! work begins. It snapshots trusted request and privacy state once, then
//! lets endpoint-specific code perform bounded body parsing before finalizing
//! the admitted attempt.

use edgezero_core::body::Body as EdgeBody;
use http::{header, Request, Response, StatusCode};
use url::Url;
use uuid::Uuid;

use crate::consent::{consent_allows_server_side_auction, ConsentContext};
use crate::ec::EcContext;
use crate::http_util::RequestInfo;
use crate::platform::ClientInfo;
use crate::settings::Settings;

pub const MAX_AUCTION_BODY_BYTES: usize = 256 * 1024;

const AUCTION_HEADER_NAME: &str = "x-tsjs-auction";
const AUCTION_HEADER_VALUE: &str = "1";
const PAGE_BIDS_HEADER_NAME: &str = "x-tsjs-page-bids";
const PAGE_BIDS_HEADER_VALUE: &str = "1";

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum AuctionSource {
    InitialNavigation,
    SpaNavigation,
    AuctionApi,
}

impl AuctionSource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::InitialNavigation => "initial_navigation",
            Self::SpaNavigation => "spa_navigation",
            Self::AuctionApi => "auction_api",
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum AuctionDecisionReason {
    Allowed,
    AuctionDisabled,
    ConsentDenied,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum AdmissionDenialKind {
    PayloadTooLarge,
    UnsupportedMediaType,
    ForbiddenOrigin,
    InvalidBody,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RequestMetadataSnapshot {
    pub user_agent: Option<String>,
    pub accept_language: Option<String>,
    pub dnt: Option<bool>,
    pub gpc: bool,
    pub referer: Option<String>,
    pub client_ip: Option<String>,
    pub tls_protocol: Option<String>,
    pub tls_cipher: Option<String>,
    pub tls_ja4: Option<String>,
    pub h2_fingerprint: Option<String>,
    pub server_hostname: Option<String>,
    pub server_region: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AuctionAdmissionDraft {
    auction_id: Uuid,
    source: AuctionSource,
    publisher_origin: Url,
    consent: ConsentContext,
    request_metadata: RequestMetadataSnapshot,
    auction_enabled: bool,
    request_allowed: bool,
    auction_allowed: bool,
    identity_allowed: bool,
    eids_allowed: bool,
    decision_reason: Option<AuctionDecisionReason>,
}

impl AuctionAdmissionDraft {
    #[must_use]
    pub fn auction_id(&self) -> Uuid {
        self.auction_id
    }

    #[must_use]
    pub fn publisher_origin(&self) -> &Url {
        &self.publisher_origin
    }
}

#[derive(Debug, Clone)]
pub struct AuctionAdmission {
    auction_id: Uuid,
    source: AuctionSource,
    publisher_origin: Url,
    page_url: Url,
    telemetry_path: String,
    consent: ConsentContext,
    request_metadata: RequestMetadataSnapshot,
    auction_enabled: bool,
    request_allowed: bool,
    auction_allowed: bool,
    identity_allowed: bool,
    eids_allowed: bool,
    decision_reason: Option<AuctionDecisionReason>,
}

impl AuctionAdmission {
    #[must_use]
    pub fn auction_id(&self) -> Uuid {
        self.auction_id
    }

    #[must_use]
    pub fn source(&self) -> AuctionSource {
        self.source
    }

    #[must_use]
    pub fn publisher_origin(&self) -> &Url {
        &self.publisher_origin
    }

    #[must_use]
    pub fn page_url(&self) -> &Url {
        &self.page_url
    }

    #[must_use]
    pub fn telemetry_path(&self) -> &str {
        &self.telemetry_path
    }

    #[must_use]
    pub fn consent(&self) -> &ConsentContext {
        &self.consent
    }

    #[must_use]
    pub fn request_metadata(&self) -> &RequestMetadataSnapshot {
        &self.request_metadata
    }

    #[must_use]
    pub fn auction_enabled(&self) -> bool {
        self.auction_enabled
    }

    #[must_use]
    pub fn request_allowed(&self) -> bool {
        self.request_allowed
    }

    #[must_use]
    pub fn auction_allowed(&self) -> bool {
        self.auction_allowed
    }

    #[must_use]
    pub fn identity_allowed(&self) -> bool {
        self.identity_allowed
    }

    #[must_use]
    pub fn eids_allowed(&self) -> bool {
        self.eids_allowed
    }

    #[must_use]
    pub fn decision_reason(&self) -> Option<AuctionDecisionReason> {
        self.decision_reason
    }
}

#[derive(Debug, Clone)]
pub struct AdmissionDenial {
    auction_id: Uuid,
    source: AuctionSource,
    telemetry_path: Option<String>,
    kind: AdmissionDenialKind,
}

impl AdmissionDenial {
    #[must_use]
    pub fn auction_id(&self) -> Uuid {
        self.auction_id
    }

    #[must_use]
    pub fn source(&self) -> AuctionSource {
        self.source
    }

    #[must_use]
    pub fn telemetry_path(&self) -> Option<&str> {
        self.telemetry_path.as_deref()
    }

    #[must_use]
    pub fn kind(&self) -> AdmissionDenialKind {
        self.kind
    }

    #[must_use]
    pub fn status(&self) -> StatusCode {
        match self.kind {
            AdmissionDenialKind::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            AdmissionDenialKind::UnsupportedMediaType => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            AdmissionDenialKind::ForbiddenOrigin => StatusCode::FORBIDDEN,
            AdmissionDenialKind::InvalidBody => StatusCode::BAD_REQUEST,
        }
    }
}

/// Apply the shared header-only auction admission gate.
///
/// # Errors
///
/// Returns [`AdmissionDenial`] when the advertised body is too large, the
/// media type is not accepted for `/auction`, or origin/fetch-metadata checks
/// reject the request.
pub fn admit_auction_http(
    settings: &Settings,
    source: AuctionSource,
    req: &Request<EdgeBody>,
    ec_context: &EcContext,
    client_info: &ClientInfo,
) -> Result<AuctionAdmissionDraft, AdmissionDenial> {
    let auction_id = Uuid::new_v4();
    let metadata = snapshot_request_metadata(req, ec_context, client_info);

    if advertised_length_exceeds_limit(req) {
        return Err(AdmissionDenial {
            auction_id,
            source,
            telemetry_path: None,
            kind: AdmissionDenialKind::PayloadTooLarge,
        });
    }

    if source == AuctionSource::AuctionApi && !has_json_content_type(req) {
        return Err(AdmissionDenial {
            auction_id,
            source,
            telemetry_path: None,
            kind: AdmissionDenialKind::UnsupportedMediaType,
        });
    }

    let publisher_origin = public_origin(req, client_info).ok_or(AdmissionDenial {
        auction_id,
        source,
        telemetry_path: None,
        kind: AdmissionDenialKind::ForbiddenOrigin,
    })?;

    if !origin_allowed(req, &publisher_origin) || !auction_header_allowed(source, req) {
        return Err(AdmissionDenial {
            auction_id,
            source,
            telemetry_path: None,
            kind: AdmissionDenialKind::ForbiddenOrigin,
        });
    }

    let consent = ec_context.consent().clone();
    let auction_enabled = settings.auction.enabled;
    let consent_allows_auction = consent_allows_server_side_auction(&consent);
    let auction_allowed = auction_enabled && consent_allows_auction;
    let identity_allowed = ec_context.ec_allowed();
    let eids_allowed = identity_allowed;
    let decision_reason = if !auction_enabled {
        Some(AuctionDecisionReason::AuctionDisabled)
    } else if !consent_allows_auction {
        Some(AuctionDecisionReason::ConsentDenied)
    } else {
        Some(AuctionDecisionReason::Allowed)
    };

    Ok(AuctionAdmissionDraft {
        auction_id,
        source,
        publisher_origin,
        consent,
        request_metadata: metadata,
        auction_enabled,
        request_allowed: true,
        auction_allowed,
        identity_allowed,
        eids_allowed,
        decision_reason,
    })
}

#[must_use]
pub fn finalize_admission(draft: AuctionAdmissionDraft, page_url: Url) -> AuctionAdmission {
    let telemetry_path = normalized_telemetry_path(&page_url);
    AuctionAdmission {
        auction_id: draft.auction_id,
        source: draft.source,
        publisher_origin: draft.publisher_origin,
        page_url,
        telemetry_path,
        consent: draft.consent,
        request_metadata: draft.request_metadata,
        auction_enabled: draft.auction_enabled,
        request_allowed: draft.request_allowed,
        auction_allowed: draft.auction_allowed,
        identity_allowed: draft.identity_allowed,
        eids_allowed: draft.eids_allowed,
        decision_reason: draft.decision_reason,
    }
}

#[must_use]
#[allow(
    clippy::needless_pass_by_value,
    reason = "consuming the draft prevents callers from reusing a failed admission attempt"
)]
pub fn deny_invalid_body(draft: AuctionAdmissionDraft) -> AdmissionDenial {
    let AuctionAdmissionDraft {
        auction_id,
        source,
        publisher_origin: _,
        consent: _,
        request_metadata: _,
        auction_enabled: _,
        request_allowed: _,
        auction_allowed: _,
        identity_allowed: _,
        eids_allowed: _,
        decision_reason: _,
    } = draft;
    AdmissionDenial {
        auction_id,
        source,
        telemetry_path: None,
        kind: AdmissionDenialKind::InvalidBody,
    }
}

/// Convert a draft into a payload-size denial after bounded body collection.
#[must_use]
#[allow(
    clippy::needless_pass_by_value,
    reason = "consuming the draft prevents callers from reusing a failed admission attempt"
)]
pub fn deny_payload_too_large(draft: AuctionAdmissionDraft) -> AdmissionDenial {
    let AuctionAdmissionDraft {
        auction_id,
        source,
        publisher_origin: _,
        consent: _,
        request_metadata: _,
        auction_enabled: _,
        request_allowed: _,
        auction_allowed: _,
        identity_allowed: _,
        eids_allowed: _,
        decision_reason: _,
    } = draft;
    AdmissionDenial {
        auction_id,
        source,
        telemetry_path: None,
        kind: AdmissionDenialKind::PayloadTooLarge,
    }
}

/// Convert an admission denial into a plain HTTP response.
///
/// # Errors
///
/// Returns [`http::Error`] if the response builder cannot construct the
/// response.
pub fn admission_denial_response(
    denial: &AdmissionDenial,
) -> Result<Response<EdgeBody>, http::Error> {
    Response::builder()
        .status(denial.status())
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(EdgeBody::from(denial.kind_message()))
}

impl AdmissionDenial {
    fn kind_message(&self) -> &'static str {
        match self.kind {
            AdmissionDenialKind::PayloadTooLarge => "Auction request body is too large",
            AdmissionDenialKind::UnsupportedMediaType => {
                "Auction request content type must be application/json"
            }
            AdmissionDenialKind::ForbiddenOrigin => "Auction request origin is not allowed",
            AdmissionDenialKind::InvalidBody => "Auction request body is invalid",
        }
    }
}

fn snapshot_request_metadata(
    req: &Request<EdgeBody>,
    ec_context: &EcContext,
    client_info: &ClientInfo,
) -> RequestMetadataSnapshot {
    RequestMetadataSnapshot {
        user_agent: header_string(req, header::USER_AGENT.as_str()),
        accept_language: header_string(req, header::ACCEPT_LANGUAGE.as_str()),
        dnt: header_string(req, "dnt").map(|value| value.trim() == "1"),
        gpc: ec_context.consent().gpc,
        referer: header_string(req, header::REFERER.as_str()),
        client_ip: client_info.client_ip.map(|ip| ip.to_string()),
        tls_protocol: client_info.tls_protocol.clone(),
        tls_cipher: client_info.tls_cipher.clone(),
        tls_ja4: client_info.tls_ja4.clone(),
        h2_fingerprint: client_info.h2_fingerprint.clone(),
        server_hostname: client_info.server_hostname.clone(),
        server_region: client_info.server_region.clone(),
    }
}

fn header_string(req: &Request<EdgeBody>, name: &str) -> Option<String> {
    req.headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn advertised_length_exceeds_limit(req: &Request<EdgeBody>) -> bool {
    req.headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|length| length > MAX_AUCTION_BODY_BYTES)
}

fn has_json_content_type(req: &Request<EdgeBody>) -> bool {
    req.headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("application/json"))
}

fn auction_header_allowed(source: AuctionSource, req: &Request<EdgeBody>) -> bool {
    let (name, expected_value) = match source {
        AuctionSource::InitialNavigation => return true,
        AuctionSource::SpaNavigation => (PAGE_BIDS_HEADER_NAME, PAGE_BIDS_HEADER_VALUE),
        AuctionSource::AuctionApi => (AUCTION_HEADER_NAME, AUCTION_HEADER_VALUE),
    };

    req.headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.trim() == expected_value)
}

fn origin_allowed(req: &Request<EdgeBody>, publisher_origin: &Url) -> bool {
    if fetch_metadata_cross_site(req) {
        return false;
    }

    let Some(origin) = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| Url::parse(value).ok())
    else {
        return true;
    };

    origins_match(&origin, publisher_origin) && scheme_allowed(&origin)
}

fn fetch_metadata_cross_site(req: &Request<EdgeBody>) -> bool {
    req.headers()
        .get("sec-fetch-site")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("cross-site"))
}

fn public_origin(req: &Request<EdgeBody>, client_info: &ClientInfo) -> Option<Url> {
    if let Some(scheme) = req.uri().scheme_str() {
        let authority = req.uri().authority()?.as_str();
        return Url::parse(&format!("{scheme}://{authority}")).ok();
    }

    let info = RequestInfo::from_request(req, client_info);
    if info.host.is_empty() {
        return None;
    }
    Url::parse(&format!("{}://{}", info.scheme, info.host)).ok()
}

fn origins_match(left: &Url, right: &Url) -> bool {
    left.scheme().eq_ignore_ascii_case(right.scheme())
        && left.host_str().map(str::to_ascii_lowercase)
            == right.host_str().map(str::to_ascii_lowercase)
        && left.port_or_known_default() == right.port_or_known_default()
}

fn scheme_allowed(origin: &Url) -> bool {
    origin.scheme().eq_ignore_ascii_case("https")
        || (origin.scheme().eq_ignore_ascii_case("http") && is_localhost(origin))
}

fn is_localhost(url: &Url) -> bool {
    matches!(
        url.host_str(),
        Some("localhost") | Some("127.0.0.1") | Some("[::1]")
    )
}

fn normalized_telemetry_path(page_url: &Url) -> String {
    let path = page_url.path();
    if path.is_empty() {
        "/".to_owned()
    } else {
        path.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use edgezero_core::body::Body as EdgeBody;
    use http::{header, Method, Request, StatusCode};
    use url::Url;

    use crate::consent::jurisdiction::Jurisdiction;
    use crate::consent::ConsentContext;
    use crate::ec::EcContext;
    use crate::platform::ClientInfo;
    use crate::test_support::tests::create_test_settings;

    use super::*;

    fn request_with(
        method: Method,
        uri: &str,
        headers: &[(&str, &str)],
        body: &'static [u8],
    ) -> Request<EdgeBody> {
        let mut builder = Request::builder().method(method).uri(uri);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        builder
            .body(EdgeBody::from(body))
            .expect("should build test request")
    }

    fn ec_context() -> EcContext {
        EcContext::new_for_test(
            None,
            ConsentContext {
                jurisdiction: Jurisdiction::NonRegulated,
                ..ConsentContext::default()
            },
        )
    }

    fn client_info() -> ClientInfo {
        ClientInfo {
            client_ip: Some("203.0.113.7".parse().expect("should parse test IP")),
            tls_protocol: Some("TLSv1.3".to_owned()),
            tls_cipher: None,
            tls_ja4: Some("t13d1516h2_8daaf6152771_b0da82dd1658".to_owned()),
            h2_fingerprint: Some("h2fp".to_owned()),
            server_hostname: Some("edge-pop.example".to_owned()),
            server_region: Some("iad".to_owned()),
        }
    }

    #[test]
    fn admission_rejects_first_applicable_header_rule_before_json_parsing() {
        let settings = create_test_settings();
        let assert_denial = |name: &str,
                             headers: &[(&str, &str)],
                             expected_kind: AdmissionDenialKind,
                             expected_status: StatusCode| {
            let request = request_with(Method::POST, "/auction", headers, b"not-json");

            let denial = admit_auction_http(
                &settings,
                AuctionSource::AuctionApi,
                &request,
                &ec_context(),
                &client_info(),
            )
            .unwrap_err();

            assert_eq!(denial.kind(), expected_kind, "{name}");
            assert_eq!(denial.status(), expected_status, "{name}");
            assert_ne!(
                denial.auction_id(),
                uuid::Uuid::nil(),
                "{name}: should allocate independent auction UUID"
            );
        };

        assert_denial(
            "advertised body too large wins first",
            &[
                (header::CONTENT_LENGTH.as_str(), "262145"),
                (header::CONTENT_TYPE.as_str(), "text/plain"),
                ("origin", "https://evil.example"),
            ],
            AdmissionDenialKind::PayloadTooLarge,
            StatusCode::PAYLOAD_TOO_LARGE,
        );
        assert_denial(
            "unsupported media type wins before origin",
            &[
                (header::CONTENT_TYPE.as_str(), "text/plain"),
                ("origin", "https://evil.example"),
            ],
            AdmissionDenialKind::UnsupportedMediaType,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
        );
        assert_denial(
            "forbidden origin wins before body parsing",
            &[
                (header::CONTENT_TYPE.as_str(), "application/json"),
                ("origin", "https://evil.example"),
            ],
            AdmissionDenialKind::ForbiddenOrigin,
            StatusCode::FORBIDDEN,
        );
        assert_denial(
            "missing custom auction header is forbidden",
            &[
                (header::CONTENT_TYPE.as_str(), "application/json"),
                ("origin", "https://publisher.example"),
            ],
            AdmissionDenialKind::ForbiddenOrigin,
            StatusCode::FORBIDDEN,
        );
        assert_denial(
            "cross-site fetch metadata is forbidden",
            &[
                (header::CONTENT_TYPE.as_str(), "application/json"),
                ("origin", "https://publisher.example"),
                ("x-tsjs-auction", "1"),
                ("sec-fetch-site", "cross-site"),
            ],
            AdmissionDenialKind::ForbiddenOrigin,
            StatusCode::FORBIDDEN,
        );
    }

    #[test]
    fn admission_allows_same_origin_https_and_localhost_http() {
        let mut settings = create_test_settings();
        settings.auction.enabled = true;
        let cases = [
            (
                "https same origin",
                "https://publisher.example/auction",
                "https://publisher.example",
                "https://publisher.example/article",
            ),
            (
                "localhost http",
                "http://localhost:8787/auction",
                "http://localhost:8787",
                "http://localhost:8787/article",
            ),
        ];

        for (name, uri, origin, page_url) in cases {
            let request = request_with(
                Method::POST,
                uri,
                &[
                    (header::CONTENT_TYPE.as_str(), "application/json"),
                    ("origin", origin),
                    ("x-tsjs-auction", "1"),
                    ("sec-fetch-site", "same-origin"),
                    ("user-agent", "Mozilla/5.0"),
                    ("accept-language", "en-US,en;q=0.9"),
                    ("dnt", "1"),
                    ("referer", page_url),
                ],
                b"{}",
            );

            let draft = admit_auction_http(
                &settings,
                AuctionSource::AuctionApi,
                &request,
                &ec_context(),
                &client_info(),
            )
            .expect("should admit same-origin request");
            let auction_id = draft.auction_id();
            let admission =
                finalize_admission(draft, Url::parse(page_url).expect("should parse page URL"));

            assert_eq!(admission.auction_id(), auction_id, "{name}");
            assert_eq!(admission.source(), AuctionSource::AuctionApi, "{name}");
            assert_eq!(
                admission.publisher_origin().as_str(),
                format!("{origin}/"),
                "{name}"
            );
            assert_eq!(admission.page_url().as_str(), page_url, "{name}");
            assert_eq!(admission.telemetry_path(), "/article", "{name}");
            assert!(admission.auction_enabled(), "{name}");
            assert!(admission.request_allowed(), "{name}");
            assert!(admission.auction_allowed(), "{name}");
            assert!(admission.identity_allowed(), "{name}");
            assert!(admission.eids_allowed(), "{name}");
            assert_eq!(
                admission.decision_reason(),
                Some(AuctionDecisionReason::Allowed),
                "{name}"
            );
            assert!(admission.consent().is_empty(), "{name}");
            assert_eq!(
                admission.request_metadata().user_agent.as_deref(),
                Some("Mozilla/5.0"),
                "{name}"
            );
            assert_eq!(
                admission.request_metadata().accept_language.as_deref(),
                Some("en-US,en;q=0.9"),
                "{name}"
            );
            assert_eq!(admission.request_metadata().dnt, Some(true), "{name}");
            assert_eq!(
                admission.request_metadata().referer.as_deref(),
                Some(page_url),
                "{name}"
            );
            assert_eq!(
                admission.request_metadata().client_ip.as_deref(),
                Some("203.0.113.7"),
                "{name}"
            );
            assert_eq!(
                admission.request_metadata().server_region.as_deref(),
                Some("iad"),
                "{name}"
            );
        }
    }
}
