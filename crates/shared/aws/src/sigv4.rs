//! AWS Signature Version 4 over the headers of a read from S3.
//!
//! The signer writes header-based authentication with `UNSIGNED-PAYLOAD`, so
//! it needs no credential provider, no request body and no presigned query.
//! The caller gives it the final address and `Host` header, after any path
//! rewrite or query policy has run, because the signature covers both.
//!
//! Canonicalization follows the address as it will be sent. A percent escape
//! already in the path or query is kept and written in upper case, and a raw
//! reserved byte is encoded the way the scheme requires.

use std::time::SystemTime;

use chrono::{DateTime, Utc};
use error_stack::Report;
use hmac::{Hmac, KeyInit as _, Mac as _};
use http::{HeaderMap, HeaderValue, Method, header};
use sha2::{Digest as _, Sha256};
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::redacted::Redacted;
use url::Url;

type HmacSha256 = Hmac<Sha256>;

const ALGORITHM: &str = "AWS4-HMAC-SHA256";
const SERVICE: &str = "s3";
const TERMINATOR: &str = "aws4_request";
const UNSIGNED_PAYLOAD: &str = "UNSIGNED-PAYLOAD";
/// Prefix of every header name in the scheme's own namespace.
const NAMESPACE_PREFIX: &str = "x-amz-";

/// Removes every header in the scheme's namespace, so a header the reader
/// chose cannot change how the origin reads the signed request.
/// [`sign_headers`] then writes the few it needs.
pub(crate) fn remove_namespace_headers(headers: &mut HeaderMap) {
    let names: Vec<_> = headers
        .keys()
        .filter(|name| name.as_str().starts_with(NAMESPACE_PREFIX))
        .cloned()
        .collect();
    for name in names {
        headers.remove(&name);
    }
}

/// The credentials a request is signed with. A session token, for temporary
/// credentials, becomes the signed `x-amz-security-token` header.
#[derive(Debug, Clone)]
pub(crate) struct Credentials {
    pub(crate) access_key_id: String,
    pub(crate) secret_access_key: Redacted<String>,
    pub(crate) session_token: Option<Redacted<String>>,
}

/// Signs `headers` for `method` on `url` at `now`, replacing any earlier
/// `Authorization` and signing headers.
///
/// # Errors
///
/// When a header the signature needs is missing or cannot be written.
pub(crate) fn sign_headers(
    method: &Method,
    url: &Url,
    headers: &mut HeaderMap,
    region: &str,
    credentials: &Credentials,
    now: SystemTime,
) -> Result<(), Report<TrustedServerError>> {
    let datetime = DateTime::<Utc>::from(now);
    let amz_date = datetime.format("%Y%m%dT%H%M%SZ").to_string();
    let date_stamp = datetime.format("%Y%m%d").to_string();

    headers.remove(header::AUTHORIZATION);
    headers.remove("x-amz-date");
    headers.remove("x-amz-content-sha256");
    headers.remove("x-amz-security-token");

    headers.insert(
        "x-amz-date",
        HeaderValue::from_str(&amz_date).change_context_invalid_header("x-amz-date")?,
    );
    headers.insert(
        "x-amz-content-sha256",
        HeaderValue::from_static(UNSIGNED_PAYLOAD),
    );
    if let Some(token) = credentials
        .session_token
        .as_ref()
        .map(Redacted::expose)
        .filter(|token| !token.is_empty())
    {
        headers.insert(
            "x-amz-security-token",
            HeaderValue::from_str(token).change_context_invalid_header("x-amz-security-token")?,
        );
    }

    let (canonical_headers, signed_headers) = canonical_headers(headers)?;
    let canonical_request = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        method.as_str(),
        canonical_uri(url),
        canonical_query(url),
        canonical_headers,
        signed_headers,
        UNSIGNED_PAYLOAD,
    );
    let canonical_request_hash = hex_sha256(canonical_request.as_bytes());
    let credential_scope = format!("{date_stamp}/{region}/{SERVICE}/{TERMINATOR}");
    let string_to_sign =
        format!("{ALGORITHM}\n{amz_date}\n{credential_scope}\n{canonical_request_hash}");
    let signing_key = signing_key(credentials.secret_access_key.expose(), &date_stamp, region);
    let signature = hex::encode(hmac_sha256(&signing_key, string_to_sign.as_bytes()));
    let authorization = format!(
        "{ALGORITHM} Credential={}/{credential_scope}, SignedHeaders={signed_headers}, Signature={signature}",
        credentials.access_key_id,
    );

    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&authorization).change_context_invalid_header("authorization")?,
    );

    Ok(())
}

fn canonical_headers(headers: &HeaderMap) -> Result<(String, String), Report<TrustedServerError>> {
    let mut names = vec!["host", "x-amz-content-sha256", "x-amz-date"];
    if headers.contains_key("x-amz-security-token") {
        names.push("x-amz-security-token");
    }
    names.sort_unstable();

    let mut canonical = String::new();
    for name in &names {
        let value = headers.get(*name).ok_or_else(|| {
            Report::new(TrustedServerError::Proxy {
                message: format!("missing required signing header `{name}`"),
            })
        })?;
        canonical.push_str(name);
        canonical.push(':');
        canonical.push_str(&normalize_header_value(value)?);
        canonical.push('\n');
    }

    Ok((canonical, names.join(";")))
}

fn normalize_header_value(value: &HeaderValue) -> Result<String, Report<TrustedServerError>> {
    let value = value.to_str().map_err(|err| {
        Report::new(TrustedServerError::InvalidHeaderValue {
            message: format!("signing header value is not valid text: {err}"),
        })
    })?;
    Ok(value.split_whitespace().collect::<Vec<_>>().join(" "))
}

fn canonical_uri(url: &Url) -> String {
    percent_encode_preserving_escapes(url.path(), false)
}

fn canonical_query(url: &Url) -> String {
    let Some(query) = url.query().filter(|query| !query.is_empty()) else {
        return String::new();
    };

    let mut pairs: Vec<(String, String)> = query
        .split('&')
        .map(|part| {
            let (key, value) = part.split_once('=').unwrap_or((part, ""));
            (
                percent_encode_preserving_escapes(key, true),
                percent_encode_preserving_escapes(value, true),
            )
        })
        .collect();
    pairs.sort_unstable();
    pairs
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("&")
}

fn percent_encode_preserving_escapes(value: &str, encode_slash: bool) -> String {
    let bytes = value.as_bytes();
    let mut out = String::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && bytes[index + 1].is_ascii_hexdigit()
            && bytes[index + 2].is_ascii_hexdigit()
        {
            out.push('%');
            out.push(char::from(bytes[index + 1].to_ascii_uppercase()));
            out.push(char::from(bytes[index + 2].to_ascii_uppercase()));
            index += 3;
            continue;
        }
        push_percent_encoded_byte(&mut out, bytes[index], encode_slash);
        index += 1;
    }
    out
}

fn push_percent_encoded_byte(out: &mut String, byte: u8, encode_slash: bool) {
    match byte {
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
            out.push(char::from(byte));
        }
        b'/' if !encode_slash => out.push('/'),
        other => out.push_str(&format!("%{other:02X}")),
    }
}

fn hex_sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("should create HMAC from arbitrary key");
    mac.update(message);
    mac.finalize().into_bytes().to_vec()
}

fn signing_key(secret_access_key: &str, date_stamp: &str, region: &str) -> Vec<u8> {
    let date_key = hmac_sha256(
        format!("AWS4{secret_access_key}").as_bytes(),
        date_stamp.as_bytes(),
    );
    let region_key = hmac_sha256(&date_key, region.as_bytes());
    let service_key = hmac_sha256(&region_key, SERVICE.as_bytes());
    hmac_sha256(&service_key, TERMINATOR.as_bytes())
}

trait HeaderValueResultExt<T> {
    fn change_context_invalid_header(self, name: &str) -> Result<T, Report<TrustedServerError>>;
}

impl<T> HeaderValueResultExt<T> for Result<T, http::header::InvalidHeaderValue> {
    fn change_context_invalid_header(self, name: &str) -> Result<T, Report<TrustedServerError>> {
        self.map_err(|err| {
            Report::new(TrustedServerError::InvalidHeaderValue {
                message: format!("invalid signing header `{name}`: {err}"),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The published example of a signed `GET`, whose host and credentials
    /// are the ones the signature was published for.
    #[test]
    fn signs_the_published_get_example() {
        let url = Url::parse("https://examplebucket.s3.amazonaws.com/test.txt")
            .expect("should parse URL");
        let mut headers = HeaderMap::new();
        headers.insert(
            header::HOST,
            HeaderValue::from_static("examplebucket.s3.amazonaws.com"),
        );
        let credentials = Credentials {
            access_key_id: "AKIAIOSFODNN7EXAMPLE".to_string(),
            secret_access_key: Redacted::new(
                "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_string(),
            ),
            session_token: None,
        };
        let now = DateTime::parse_from_rfc3339("2013-05-24T00:00:00Z")
            .expect("should parse fixture date")
            .with_timezone(&Utc)
            .into();

        sign_headers(
            &Method::GET,
            &url,
            &mut headers,
            "us-east-1",
            &credentials,
            now,
        )
        .expect("should sign request");

        assert_eq!(
            headers.get("x-amz-content-sha256"),
            Some(&HeaderValue::from_static(UNSIGNED_PAYLOAD)),
            "should use unsigned payload for read-only asset requests"
        );
        let authorization = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .expect("should set authorization header");
        assert_eq!(
            authorization,
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature=17ee2dc4ebe24953b3ebb4aad72c73aada1b27aa77109a55301af128fdcf571f",
            "should include expected credential scope, signed headers, and signature"
        );
    }

    #[test]
    fn a_session_token_is_signed_and_a_reader_s_header_in_the_namespace_is_removed() {
        let url = Url::parse("https://bucket.storage.example.com/site/page").expect("url");
        let mut headers = HeaderMap::new();
        headers.insert(
            header::HOST,
            HeaderValue::from_static("bucket.storage.example.com"),
        );
        headers.insert("x-amz-meta-reader", HeaderValue::from_static("chosen"));
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer reader"),
        );
        let credentials = Credentials {
            access_key_id: "TESTACCESSKEYID".to_string(),
            secret_access_key: Redacted::new("test-secret".to_string()),
            session_token: Some(Redacted::new("test-session".to_string())),
        };

        remove_namespace_headers(&mut headers);
        sign_headers(
            &Method::HEAD,
            &url,
            &mut headers,
            "test-region-1",
            &credentials,
            SystemTime::UNIX_EPOCH,
        )
        .expect("should sign request");

        assert!(headers.get("x-amz-meta-reader").is_none());
        assert_eq!(
            headers.get("x-amz-security-token"),
            Some(&HeaderValue::from_static("test-session"))
        );
        let authorization = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .expect("should set authorization header");
        assert!(
            authorization.starts_with(
                "AWS4-HMAC-SHA256 Credential=TESTACCESSKEYID/19700101/test-region-1/s3/aws4_request, \
                 SignedHeaders=host;x-amz-content-sha256;x-amz-date;x-amz-security-token, "
            ),
            "should sign the token with the other headers: {authorization}"
        );
    }

    #[test]
    fn a_request_without_a_host_header_is_refused() {
        let url = Url::parse("https://bucket.storage.example.com/site/page").expect("url");
        let mut headers = HeaderMap::new();
        let credentials = Credentials {
            access_key_id: "TESTACCESSKEYID".to_string(),
            secret_access_key: Redacted::new("test-secret".to_string()),
            session_token: None,
        };

        let err = sign_headers(
            &Method::GET,
            &url,
            &mut headers,
            "test-region-1",
            &credentials,
            SystemTime::UNIX_EPOCH,
        )
        .expect_err("should refuse without a Host");
        assert!(
            format!("{err:?}").contains("missing required signing header `host`"),
            "{err:?}"
        );
    }

    #[test]
    fn credentials_debug_redacts_secret_material() {
        let credentials = Credentials {
            access_key_id: "AKIAIOSFODNN7EXAMPLE".to_string(),
            secret_access_key: Redacted::new("debug-secret-key".to_string()),
            session_token: Some(Redacted::new("debug-session-token".to_string())),
        };

        let debug_output = format!("{credentials:?}");

        assert!(
            debug_output.contains("AKIAIOSFODNN7EXAMPLE"),
            "should leave non-secret access key ID visible"
        );
        assert!(
            debug_output.contains("[REDACTED]"),
            "should show redaction markers for secret fields"
        );
        assert!(
            !debug_output.contains("debug-secret-key"),
            "should not expose the secret access key in Debug output"
        );
        assert!(
            !debug_output.contains("debug-session-token"),
            "should not expose the session token in Debug output"
        );
    }

    #[test]
    fn canonical_uri_preserves_existing_percent_encoded_path() {
        let url = Url::parse("https://bucket.storage.example.com/foo%20bar/%e2%9c%93.jpg")
            .expect("should parse URL");

        assert_eq!(canonical_uri(&url), "/foo%20bar/%E2%9C%93.jpg");
    }

    #[test]
    fn canonical_uri_encodes_raw_path_bytes_without_double_encoding() {
        let url = Url::parse("https://bucket.storage.example.com/image*name%20raw.jpg")
            .expect("should parse URL");

        assert_eq!(canonical_uri(&url), "/image%2Aname%20raw.jpg");
    }

    #[test]
    fn canonical_query_sorts_and_encodes() {
        let url = Url::parse("https://bucket.storage.example.com/object?z=two&a=sp ace")
            .expect("should parse URL");
        assert_eq!(canonical_query(&url), "a=sp%20ace&z=two");
    }

    #[test]
    fn canonical_query_empty_query_is_empty() {
        let url =
            Url::parse("https://bucket.storage.example.com/object?").expect("should parse URL");

        assert_eq!(canonical_query(&url), "");
    }

    #[test]
    fn canonical_query_preserves_plus_and_existing_escapes() {
        let url = Url::parse(
            "https://bucket.storage.example.com/object?v=a+b&space=a%20b&slash=a/b&encoded=a%2bb&empty",
        )
        .expect("should parse URL");

        assert_eq!(
            canonical_query(&url),
            "empty=&encoded=a%2Bb&slash=a%2Fb&space=a%20b&v=a%2Bb"
        );
    }
}
