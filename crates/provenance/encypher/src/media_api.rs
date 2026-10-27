//! Wire contract of the Encypher media signing endpoint,
//! `POST /api/v1/public/cdn/media/sign`.
//!
//! The request is `multipart/form-data` with a `metadata` JSON part and the
//! exact `source` bytes. Neither core nor this module transforms media, so no
//! `final` part is sent and the execution record lists no operations. A `200`
//! answer is the exact signed bytes, described by `X-Encypher-*` headers that
//! [`verify_signed_response`] checks before any byte reaches a reader.

use bytes::{BufMut as _, Bytes, BytesMut};
use http::{HeaderMap, HeaderValue};
use serde::ser::SerializeSeq as _;
use serde::{Deserialize, Serialize, Serializer};
use sha2::{Digest as _, Sha256};
use trusted_server_core::middleware::media_type_of;

use crate::api::valid_policy_digest;

/// The media signing route, below the API origin.
pub(crate) const MEDIA_SIGN_PATH: &str = "/api/v1/public/cdn/media/sign";
/// The request header carrying the replay key the endpoint requires.
pub(crate) const IDEMPOTENCY_KEY_HEADER: &str = "idempotency-key";
/// The whole multipart body the endpoint accepts.
pub(crate) const MAX_REQUEST_BYTES: usize = 100_000_000;
/// What signing may add to the bytes: the manifest store with its
/// certificate chain and the embedded source data.
pub(crate) const MAX_MANIFEST_OVERHEAD_BYTES: usize = 10 * 1024 * 1024;
/// A failure body is only read for its code.
pub(crate) const MAX_ERROR_BODY_BYTES: usize = 4096;
/// Longest manifest instance or document identifier accepted.
const MAX_IDENTIFIER_BYTES: usize = 256;
/// Longest URL the endpoint accepts or returns.
const MAX_URL_BYTES: usize = 2048;
/// Longest failure code kept for the log.
const MAX_ERROR_CODE_BYTES: usize = 64;

const POLICY_DIGEST_HEADER: &str = "x-encypher-policy-digest";
const SIGNED_SHA256_HEADER: &str = "x-encypher-signed-sha256";
const INSTANCE_ID_HEADER: &str = "x-encypher-manifest-instance-id";
const DISPOSITION_HEADER: &str = "x-encypher-manifest-disposition";
const MANIFEST_URL_HEADER: &str = "x-encypher-manifest-url";
const MANIFEST_SHA256_HEADER: &str = "x-encypher-manifest-sha256";
const LINK_REQUIRED_HEADER: &str = "x-encypher-manifest-link-required";
const VERIFICATION_URL_HEADER: &str = "x-encypher-verification-url";
const DOCUMENT_ID_HEADER: &str = "x-encypher-document-id";

/// Every signing header the endpoint answers with, in the order a cached
/// answer stores them.
pub(crate) const SIGNING_HEADERS: [&str; 10] = [
    "content-type",
    POLICY_DIGEST_HEADER,
    SIGNED_SHA256_HEADER,
    INSTANCE_ID_HEADER,
    DISPOSITION_HEADER,
    MANIFEST_URL_HEADER,
    MANIFEST_SHA256_HEADER,
    LINK_REQUIRED_HEADER,
    VERIFICATION_URL_HEADER,
    DOCUMENT_ID_HEADER,
];

/// The `metadata` part of one media signing request.
#[derive(Debug, Serialize)]
pub(crate) struct MediaMetadata<'a> {
    /// The reader-facing absolute URL the media is delivered on or in.
    pub(crate) page_url: &'a str,
    /// The server policy the edge holds, which the endpoint refuses when
    /// stale.
    pub(crate) expected_policy_digest: &'a str,
    pub(crate) source: SourceMetadata<'a>,
    pub(crate) execution_record: ExecutionRecord<'a>,
    pub(crate) delivery: Delivery,
}

/// What the edge says about the captured source bytes.
#[derive(Debug, Serialize)]
pub(crate) struct SourceMetadata<'a> {
    /// A canonical `https` address of the source, or `null` where there is
    /// none: embedded media, or an address with a query or credentials,
    /// which is never stripped to make one. Without it the endpoint binds the
    /// source by embedding its bytes.
    pub(crate) reference: Option<&'a str>,
    /// The exact hostname the source was fetched from, which the endpoint
    /// holds the reference and the claim's managed origins against, or
    /// `null` only for media embedded in a document, which was fetched from
    /// no host of its own. Never a scheme, port, path or query.
    pub(crate) origin_host: Option<&'a str>,
    /// The declared media type, a hint the endpoint checks against the bytes.
    pub(crate) mime: &'a str,
    pub(crate) sha256: &'a str,
    pub(crate) byte_length: usize,
    /// The source is a platform image optimizer's answer: the endpoint
    /// refuses it unless the bytes carry an embedded manifest, which it
    /// validates as it validates any ingredient's. Sent only when true.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) require_embedded_manifest: bool,
}

/// What the edge did to the source before it was delivered: nothing.
#[derive(Debug, Serialize)]
pub(crate) struct ExecutionRecord<'a> {
    pub(crate) source_sha256: &'a str,
    pub(crate) final_sha256: &'a str,
    /// The exact source was captured and no operation was applied.
    pub(crate) complete: bool,
    pub(crate) operations: NoOperations,
}

/// The empty operation list of media delivered as it was captured.
#[derive(Debug)]
pub(crate) struct NoOperations;

impl Serialize for NoOperations {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_seq(Some(0))?.end()
    }
}

/// How the signed representation reaches readers.
#[derive(Debug, Serialize)]
pub(crate) struct Delivery {
    /// Whether every response of the representation can carry a `Link`
    /// header naming a remote manifest.
    pub(crate) http_link_header: bool,
}

impl<'a> MediaMetadata<'a> {
    /// Metadata for `source` delivered unmodified: no `final` part, an empty
    /// and complete execution record.
    pub(crate) fn unmodified(
        page_url: &'a str,
        expected_policy_digest: &'a str,
        source: SourceMetadata<'a>,
        http_link_header: bool,
    ) -> Self {
        let sha256 = source.sha256;
        Self {
            page_url,
            expected_policy_digest,
            source,
            execution_record: ExecutionRecord {
                source_sha256: sha256,
                final_sha256: sha256,
                complete: true,
                operations: NoOperations,
            },
            delivery: Delivery { http_link_header },
        }
    }
}

/// A built multipart request body and its `Content-Type`.
#[derive(Debug)]
pub(crate) struct Multipart {
    pub(crate) content_type: String,
    pub(crate) body: Bytes,
}

/// Builds the `multipart/form-data` body with the `metadata` and `source`
/// parts, with a boundary that occurs in neither.
pub(crate) fn multipart(metadata: &[u8], source: &[u8]) -> Multipart {
    let seed = sha256_hex(metadata);
    let mut attempt = 0_u32;
    let boundary = loop {
        let candidate = format!(
            "encypher-media-{}-{attempt}",
            seed.get(..32).unwrap_or(&seed)
        );
        let delimiter = format!("--{candidate}");
        if !contains(metadata, delimiter.as_bytes()) && !contains(source, delimiter.as_bytes()) {
            break candidate;
        }
        attempt = attempt.saturating_add(1);
    };
    let metadata_head = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"metadata\"\r\n\
         Content-Type: application/json\r\n\r\n"
    );
    let source_head = format!(
        "\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"source\"; \
         filename=\"source\"\r\nContent-Type: application/octet-stream\r\n\r\n"
    );
    let closing = format!("\r\n--{boundary}--\r\n");
    let mut body = BytesMut::with_capacity(
        metadata_head.len() + metadata.len() + source_head.len() + source.len() + closing.len(),
    );
    body.put_slice(metadata_head.as_bytes());
    body.put_slice(metadata);
    body.put_slice(source_head.as_bytes());
    body.put_slice(source);
    body.put_slice(closing.as_bytes());
    Multipart {
        content_type: format!("multipart/form-data; boundary={boundary}"),
        body: body.freeze(),
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Lowercase hex SHA-256 of `bytes`.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    hex_lower(&Sha256::digest(bytes))
}

fn hex_lower(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

/// The bare media type an answer must carry for media declared as `declared`,
/// with the common `image/jpg` spelling read as `image/jpeg`, and the
/// `audio/x-aac` and `audio/aacp` spellings of raw AAC as `audio/aac`.
pub(crate) fn canonical_media_type(declared: &str) -> String {
    let media_type = media_type_of(declared);
    match media_type.as_str() {
        "image/jpg" => "image/jpeg".to_owned(),
        "audio/x-aac" | "audio/aacp" => "audio/aac".to_owned(),
        _ => media_type,
    }
}

/// Whether `reference` may be sent as the source's address: canonical
/// `https` on the default port, with a path, no credentials, query or
/// fragment, on `host`. Anything else is sent as no reference.
pub(crate) fn safe_source_reference(reference: &str, host: &str) -> bool {
    if reference.len() > MAX_URL_BYTES || !reference.is_ascii() {
        return false;
    }
    let Ok(parsed) = url::Url::parse(reference) else {
        return false;
    };
    parsed.scheme() == "https"
        && parsed.as_str() == reference
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.port().is_none()
        && parsed.query().is_none()
        && parsed.fragment().is_none()
        && parsed.path() != "/"
        && parsed.host_str() == Some(host)
}

/// Whether `host` is one canonical hostname alone: no scheme, user
/// information, port, path or query, in lower case.
pub(crate) fn valid_origin_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && url::Url::parse(&format!("https://{host}/")).is_ok_and(|parsed| {
            parsed.host_str() == Some(host)
                && parsed.port().is_none()
                && parsed.username().is_empty()
                && parsed.path() == "/"
        })
}

/// Where the signed bytes' manifest lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Disposition {
    /// Inside the signed bytes.
    Embedded,
    /// At [`SignedMedia::manifest_url`], named by the bytes or a `Link`.
    External,
}

/// What a signing answer must match.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Expected<'a> {
    pub(crate) policy_digest: &'a str,
    /// The bare media type the source was delivered as.
    pub(crate) media_type: &'a str,
    /// SHA-256 of the bytes as they were before signing.
    pub(crate) unsigned_sha256: &'a str,
    /// Whether every response of the representation can carry a `Link`.
    pub(crate) link_deliverable: bool,
}

/// A verified signing answer.
#[derive(Debug, Clone)]
pub(crate) struct SignedMedia {
    pub(crate) bytes: Bytes,
    pub(crate) content_type: HeaderValue,
    pub(crate) instance_id: String,
    pub(crate) document_id: String,
    pub(crate) verification_url: String,
    pub(crate) disposition: Disposition,
    pub(crate) manifest_url: Option<String>,
}

impl SignedMedia {
    /// The `Link` header value naming a remote manifest, when there is one.
    pub(crate) fn link(&self) -> Option<String> {
        self.manifest_url
            .as_deref()
            .map(|url| format!("<{url}>; rel=\"c2pa-manifest\""))
    }
}

/// Why a `200` answer is not accepted as signed media.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResponseError {
    /// The answer's media type is not the one the source was delivered as.
    ContentType,
    /// The answer was signed under another policy.
    PolicyDigest,
    /// The body is not the bytes the signed digest names.
    SignedDigest,
    /// A header is missing, repeated or malformed.
    Malformed,
    /// The manifest is remote and needs a `Link` the delivery cannot carry.
    LinkUndeliverable,
    /// The body carries no manifest and no reference to one.
    Unsigned,
}

/// The one value of `name`, when it appears exactly once as visible ASCII.
fn single<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    value.to_str().ok()
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_IDENTIFIER_BYTES
        && value.bytes().all(|byte| byte.is_ascii_graphic())
}

/// Whether `value` is a canonical absolute `https` URL that is safe inside a
/// `Link` header's angle brackets.
fn valid_https_url(value: &str) -> bool {
    value.len() <= MAX_URL_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b'<' | b'>' | b'"'))
        && url::Url::parse(value).is_ok_and(|parsed| {
            parsed.scheme() == "https" && parsed.host_str().is_some() && parsed.as_str() == value
        })
}

/// Checks a `200` signing answer against what was sent, and keeps it only
/// when it is signed media this delivery can carry.
///
/// # Errors
///
/// Returns the first [`ResponseError`] the answer shows.
pub(crate) fn verify_signed_response(
    headers: &HeaderMap,
    body: Bytes,
    expected: &Expected<'_>,
) -> Result<SignedMedia, ResponseError> {
    let content_type = single(headers, "content-type").ok_or(ResponseError::Malformed)?;
    if canonical_media_type(content_type) != expected.media_type {
        return Err(ResponseError::ContentType);
    }
    let policy_digest = single(headers, POLICY_DIGEST_HEADER).ok_or(ResponseError::Malformed)?;
    if !valid_policy_digest(policy_digest) {
        return Err(ResponseError::Malformed);
    }
    if policy_digest != expected.policy_digest {
        return Err(ResponseError::PolicyDigest);
    }
    let signed_sha256 = single(headers, SIGNED_SHA256_HEADER).ok_or(ResponseError::Malformed)?;
    if !valid_policy_digest(signed_sha256) {
        return Err(ResponseError::Malformed);
    }
    if body.is_empty() || sha256_hex(&body) != signed_sha256 {
        return Err(ResponseError::SignedDigest);
    }
    let instance_id = single(headers, INSTANCE_ID_HEADER)
        .filter(|value| valid_identifier(value))
        .ok_or(ResponseError::Malformed)?;
    let document_id = single(headers, DOCUMENT_ID_HEADER)
        .filter(|value| valid_identifier(value))
        .ok_or(ResponseError::Malformed)?;
    let verification_url = single(headers, VERIFICATION_URL_HEADER)
        .filter(|value| valid_https_url(value))
        .ok_or(ResponseError::Malformed)?;
    let link_required = match single(headers, LINK_REQUIRED_HEADER) {
        Some("true") => true,
        Some("false") => false,
        _ => return Err(ResponseError::Malformed),
    };
    let manifest_url = headers.get(MANIFEST_URL_HEADER).map(|_| {
        single(headers, MANIFEST_URL_HEADER)
            .filter(|value| valid_https_url(value))
            .ok_or(ResponseError::Malformed)
    });
    let changed = signed_sha256 != expected.unsigned_sha256;
    let disposition = match single(headers, DISPOSITION_HEADER) {
        Some("embedded") => {
            // The manifest store is in the bytes, so they changed, hold the
            // `c2pa` label and point nowhere else. These are consistency
            // checks against a wrong answer, not a signature check: the
            // signature is trusted through the authenticated endpoint and
            // the digest binding above.
            if manifest_url.is_some() || link_required {
                return Err(ResponseError::Malformed);
            }
            if !changed || !contains(&body, b"c2pa") {
                return Err(ResponseError::Unsigned);
            }
            Disposition::Embedded
        }
        Some("external") => {
            if manifest_url.is_none() {
                return Err(ResponseError::Malformed);
            }
            single(headers, MANIFEST_SHA256_HEADER)
                .filter(|value| valid_policy_digest(value))
                .ok_or(ResponseError::Malformed)?;
            if link_required && !expected.link_deliverable {
                return Err(ResponseError::LinkUndeliverable);
            }
            // A remote manifest no `Link` names must be named by the bytes.
            if !link_required && !changed {
                return Err(ResponseError::Unsigned);
            }
            Disposition::External
        }
        _ => return Err(ResponseError::Malformed),
    };
    let manifest_url = manifest_url.transpose()?.map(str::to_owned);
    let content_type = HeaderValue::from_str(content_type).map_err(|_| ResponseError::Malformed)?;
    Ok(SignedMedia {
        bytes: body,
        content_type,
        instance_id: instance_id.to_owned(),
        document_id: document_id.to_owned(),
        verification_url: verification_url.to_owned(),
        disposition,
        manifest_url,
    })
}

/// The failure code of an error answer, kept only as a short identifier.
pub(crate) fn error_code(body: &[u8]) -> Option<String> {
    #[derive(Deserialize)]
    struct ErrorBody {
        error: ErrorDetail,
    }
    #[derive(Deserialize)]
    struct ErrorDetail {
        code: String,
    }
    if body.len() > MAX_ERROR_BODY_BYTES {
        return None;
    }
    let parsed: ErrorBody = serde_json::from_slice(body).ok()?;
    let code = parsed.error.code;
    (!code.is_empty()
        && code.len() <= MAX_ERROR_CODE_BYTES
        && code
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'))
    .then_some(code)
}

#[cfg(test)]
mod tests {
    use http::HeaderName;
    use serde_json::json;

    use super::*;

    const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const UNSIGNED: &[u8] = b"\x89PNG\r\n\x1a\nunsigned";
    const SIGNED: &[u8] = b"\x89PNG\r\n\x1a\nunsigned-caBXjumbc2pa-manifest";

    fn expected(unsigned_sha256: &str, link_deliverable: bool) -> Expected<'_> {
        Expected {
            policy_digest: DIGEST,
            media_type: "image/png",
            unsigned_sha256,
            link_deliverable,
        }
    }

    fn embedded_headers(body: &[u8]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in [
            ("content-type", "image/png".to_owned()),
            (POLICY_DIGEST_HEADER, DIGEST.to_owned()),
            (SIGNED_SHA256_HEADER, sha256_hex(body)),
            (INSTANCE_ID_HEADER, "xmp:iid:0001".to_owned()),
            (DISPOSITION_HEADER, "embedded".to_owned()),
            (LINK_REQUIRED_HEADER, "false".to_owned()),
            (
                VERIFICATION_URL_HEADER,
                "https://verify.example.com/doc-1".to_owned(),
            ),
            (DOCUMENT_ID_HEADER, "doc-1".to_owned()),
        ] {
            headers.insert(
                HeaderName::from_static(name),
                HeaderValue::from_str(&value).expect("should build header"),
            );
        }
        headers
    }

    fn external_headers(body: &[u8], link_required: bool) -> HeaderMap {
        let mut headers = embedded_headers(body);
        headers.insert(DISPOSITION_HEADER, HeaderValue::from_static("external"));
        headers.insert(
            MANIFEST_URL_HEADER,
            HeaderValue::from_static("https://api.example.com/manifests/doc-1"),
        );
        headers.insert(
            MANIFEST_SHA256_HEADER,
            HeaderValue::from_str(DIGEST).expect("should build header"),
        );
        headers.insert(
            LINK_REQUIRED_HEADER,
            HeaderValue::from_static(if link_required { "true" } else { "false" }),
        );
        headers
    }

    #[test]
    fn metadata_matches_the_frozen_wire_shape() {
        let sha = sha256_hex(UNSIGNED);
        let metadata = MediaMetadata::unmodified(
            "https://publisher.example.com/news/story",
            DIGEST,
            SourceMetadata {
                reference: None,
                origin_host: None,
                mime: "image/png",
                sha256: &sha,
                byte_length: UNSIGNED.len(),
                require_embedded_manifest: false,
            },
            false,
        );
        let value = serde_json::to_value(&metadata).expect("should serialize metadata");
        assert_eq!(
            value,
            json!({
                "page_url": "https://publisher.example.com/news/story",
                "expected_policy_digest": DIGEST,
                "source": {
                    "reference": null,
                    "origin_host": null,
                    "mime": "image/png",
                    "sha256": sha,
                    "byte_length": UNSIGNED.len(),
                },
                "execution_record": {
                    "source_sha256": sha,
                    "final_sha256": sha,
                    "complete": true,
                    "operations": [],
                },
                "delivery": { "http_link_header": false },
            }),
            "should send an unmodified, complete record with a null reference"
        );
    }

    #[test]
    fn an_optimizer_answer_requires_its_embedded_manifest_on_the_wire() {
        let sha = sha256_hex(SIGNED);
        let metadata = MediaMetadata::unmodified(
            "https://publisher.example.com/images/photo.jpg",
            DIGEST,
            SourceMetadata {
                reference: None,
                origin_host: Some("origin.example.com"),
                mime: "image/png",
                sha256: &sha,
                byte_length: SIGNED.len(),
                require_embedded_manifest: true,
            },
            true,
        );
        let value = serde_json::to_value(&metadata).expect("should serialize metadata");
        assert_eq!(
            value["source"],
            json!({
                "reference": null,
                "origin_host": "origin.example.com",
                "mime": "image/png",
                "sha256": sha,
                "byte_length": SIGNED.len(),
                "require_embedded_manifest": true,
            }),
            "should require the optimizer's embedded manifest and name no address"
        );
        assert_eq!(
            value["execution_record"],
            json!({
                "source_sha256": sha,
                "final_sha256": sha,
                "complete": true,
                "operations": [],
            }),
            "should report no operation of the edge's own"
        );
    }

    #[test]
    fn multipart_carries_exact_source_bytes_under_an_absent_boundary() {
        let source = b"--encypher-media-binary\r\n\x00\xff";
        let built = multipart(br#"{"page_url":"x"}"#, source);
        let boundary = built
            .content_type
            .strip_prefix("multipart/form-data; boundary=")
            .expect("should declare a boundary");
        let body = built.body.as_ref();
        let source_start = body
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .and_then(|first| {
                body[first + 4..]
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .map(|second| first + 4 + second + 4)
            })
            .expect("should find the source part body");
        let tail = format!("\r\n--{boundary}--\r\n");
        assert!(body.ends_with(tail.as_bytes()), "should close the body");
        assert_eq!(
            &body[source_start..body.len() - tail.len()],
            source,
            "should carry the exact source bytes"
        );
        assert!(
            body.windows(9).any(|window| window == b"name=\"sou"),
            "should name the source part"
        );
        assert!(
            !body.windows(7).any(|window| window == b"\"final\""),
            "should send no final part for unmodified media"
        );
    }

    #[test]
    fn an_embedded_answer_is_accepted_only_with_matching_bytes_and_policy() {
        let unsigned = sha256_hex(UNSIGNED);
        let signed = verify_signed_response(
            &embedded_headers(SIGNED),
            Bytes::from_static(SIGNED),
            &expected(&unsigned, false),
        )
        .expect("should accept a verified embedded answer");
        assert_eq!(signed.bytes.as_ref(), SIGNED, "should keep the exact bytes");
        assert_eq!(
            signed.disposition,
            Disposition::Embedded,
            "should be embedded"
        );
        assert_eq!(signed.link(), None, "should need no Link");

        let tampered = verify_signed_response(
            &embedded_headers(SIGNED),
            Bytes::from_static(b"\x89PNG\r\n\x1a\nc2pa-other"),
            &expected(&unsigned, false),
        );
        assert_eq!(
            tampered.expect_err("should refuse a body the digest does not name"),
            ResponseError::SignedDigest
        );

        let mut stale = embedded_headers(SIGNED);
        stale.insert(
            POLICY_DIGEST_HEADER,
            HeaderValue::from_str(&"b".repeat(64)).expect("should build header"),
        );
        assert_eq!(
            verify_signed_response(
                &stale,
                Bytes::from_static(SIGNED),
                &expected(&unsigned, false)
            )
            .expect_err("should refuse another policy"),
            ResponseError::PolicyDigest
        );

        let mut retyped = embedded_headers(SIGNED);
        retyped.insert("content-type", HeaderValue::from_static("image/jpeg"));
        assert_eq!(
            verify_signed_response(
                &retyped,
                Bytes::from_static(SIGNED),
                &expected(&unsigned, false)
            )
            .expect_err("should refuse a changed media type"),
            ResponseError::ContentType
        );
    }

    #[test]
    fn unchanged_bytes_are_never_accepted_as_signed() {
        let unsigned = sha256_hex(UNSIGNED);
        assert_eq!(
            verify_signed_response(
                &embedded_headers(UNSIGNED),
                Bytes::from_static(UNSIGNED),
                &expected(&unsigned, false),
            )
            .expect_err("should refuse an unsigned embedded answer"),
            ResponseError::Unsigned
        );
        assert_eq!(
            verify_signed_response(
                &external_headers(UNSIGNED, false),
                Bytes::from_static(UNSIGNED),
                &expected(&unsigned, true),
            )
            .expect_err("should refuse a remote manifest nothing names"),
            ResponseError::Unsigned
        );
    }

    #[test]
    fn a_remote_manifest_needs_a_deliverable_link() {
        let unsigned = sha256_hex(UNSIGNED);
        assert_eq!(
            verify_signed_response(
                &external_headers(UNSIGNED, true),
                Bytes::from_static(UNSIGNED),
                &expected(&unsigned, false),
            )
            .expect_err("should refuse a Link the delivery cannot carry"),
            ResponseError::LinkUndeliverable
        );
        let signed = verify_signed_response(
            &external_headers(UNSIGNED, true),
            Bytes::from_static(UNSIGNED),
            &expected(&unsigned, true),
        )
        .expect("should accept a remote manifest the delivery links");
        assert_eq!(
            signed.link().as_deref(),
            Some("<https://api.example.com/manifests/doc-1>; rel=\"c2pa-manifest\""),
            "should name the manifest in a Link"
        );
    }

    #[test]
    fn repeated_or_malformed_signing_headers_are_refused() {
        let unsigned = sha256_hex(UNSIGNED);
        let mut repeated = embedded_headers(SIGNED);
        repeated.append(INSTANCE_ID_HEADER, HeaderValue::from_static("xmp:iid:0002"));
        let mut bad_url = embedded_headers(SIGNED);
        bad_url.insert(
            VERIFICATION_URL_HEADER,
            HeaderValue::from_static("http://verify.example.com/doc-1"),
        );
        let mut bad_disposition = embedded_headers(SIGNED);
        bad_disposition.insert(DISPOSITION_HEADER, HeaderValue::from_static("sidecar"));
        for headers in [repeated, bad_url, bad_disposition] {
            assert_eq!(
                verify_signed_response(
                    &headers,
                    Bytes::from_static(SIGNED),
                    &expected(&unsigned, false)
                )
                .expect_err("should refuse malformed headers"),
                ResponseError::Malformed
            );
        }
    }

    #[test]
    fn common_misspellings_name_the_media_type_the_service_lists() {
        for (declared, canonical) in [
            ("image/jpg", "image/jpeg"),
            ("audio/x-aac", "audio/aac"),
            ("Audio/AACP; codecs=aac", "audio/aac"),
            ("audio/aac", "audio/aac"),
            ("audio/mp4", "audio/mp4"),
        ] {
            assert_eq!(canonical_media_type(declared), canonical, "{declared}");
        }
    }

    #[test]
    fn source_references_are_sent_only_when_canonical_and_on_the_claim_host() {
        let host = "publisher.example.com";
        assert!(
            safe_source_reference("https://publisher.example.com/images/a.png", host),
            "should send a canonical https address"
        );
        for unsafe_reference in [
            "https://publisher.example.com/images/a.png?token=secret",
            "https://user:pass@publisher.example.com/images/a.png",
            "https://publisher.example.com/images/a.png#frag",
            "http://publisher.example.com/images/a.png",
            "https://publisher.example.com:8443/images/a.png",
            "https://origin.example.com/images/a.png",
            "https://PUBLISHER.example.com/images/a.png",
            "https://publisher.example.com/",
        ] {
            assert!(
                !safe_source_reference(unsafe_reference, host),
                "should never send {unsafe_reference}"
            );
        }
    }

    #[test]
    fn error_codes_are_read_as_short_identifiers_only() {
        let body = serde_json::to_vec(&json!({
            "success": false,
            "error": {"code": "TRUSTED_SERVER_POLICY_CHANGED", "message": "stale"},
            "correlation_id": "c-1",
        }))
        .expect("should serialize");
        assert_eq!(
            error_code(&body).as_deref(),
            Some("TRUSTED_SERVER_POLICY_CHANGED")
        );
        let injected =
            serde_json::to_vec(&json!({"error": {"code": "a b\nc"}})).expect("should serialize");
        assert_eq!(error_code(&injected), None, "should drop an unsafe code");
    }
}
