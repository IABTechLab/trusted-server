//! What the module keeps for a file, being the origin's response as the
//! origin sent it, and the address it is kept under.
//!
//! A copy holds the headers a reader is served and the body. Every copy is
//! kept under what the origin's own `Vary` names, see `crate::source_cache`.

use edgezero_core::body::Body as EdgeBody;
use http::{HeaderName, HeaderValue, Response, StatusCode, header};
use sha2::{Digest as _, Sha256};

/// Version of what a kept copy holds and how it is written. Bump on any change
/// to either, so a copy written by another build is never read.
pub const ASSET_SCHEMA_VERSION: u32 = 1;

/// The most bytes a kept copy's body holds. A larger response is served as it
/// streams from the origin, because every copy is held whole, in memory and in
/// the shared store.
pub const MAX_ASSET_COPY_BYTES: usize = 1024 * 1024;

/// Response headers never kept with a copy, being those describing one
/// connection, one reader's cookies, or the length and age, which are set again
/// for each reader.
const NOT_KEPT: &[&str] = &[
    "age",
    "connection",
    "content-length",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "set-cookie",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// Inputs that select one file's address.
///
/// What the origin's response varies on is not here, because it is read from
/// the address's record once the origin has said it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetCacheKey {
    /// The URL asked of the origin, with its query.
    pub url: String,
    /// The host the reader asked, so two hosts that serve one publisher each
    /// keep a copy.
    pub request_host: String,
    /// The scheme the reader asked over.
    pub request_scheme: String,
    /// What names the origin beyond the URL, as core reports it.
    pub origin_identity: String,
}

impl AssetCacheKey {
    /// A fixed-size key naming no part of the request.
    ///
    /// Each input is written with its length before it, so no two sets of
    /// inputs run together into the same key.
    #[must_use]
    pub fn to_cache_key(&self) -> String {
        use crate::hash::hash_length_prefixed as push;

        let mut hasher = Sha256::new();
        push(&mut hasher, b"ts-asset-cache");
        push(&mut hasher, &ASSET_SCHEMA_VERSION.to_be_bytes());
        push(&mut hasher, self.request_scheme.as_bytes());
        push(&mut hasher, self.request_host.as_bytes());
        push(&mut hasher, self.origin_identity.as_bytes());
        push(&mut hasher, self.url.as_bytes());
        format!(
            "ts-asset-cache-v{ASSET_SCHEMA_VERSION}-{}",
            hex::encode(hasher.finalize())
        )
    }
}

/// One kept response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetCopy {
    /// The headers a reader is served, as the origin sent them.
    pub headers: Vec<(String, String)>,
    /// Whether the body is kept without its `Content-Encoding`. Every copy is
    /// kept as the origin sent it, so this is false.
    pub decoded: bool,
    /// When the copy was made, in seconds since the Unix epoch.
    pub made_at: u64,
    /// The response's age when the copy was made, in seconds.
    pub age_at_made: u64,
    /// The body.
    pub body: Vec<u8>,
}

impl AssetCopy {
    /// The headers of `response_headers` a copy keeps, or `None` when one
    /// cannot be written down, as a value that is not visible text cannot.
    ///
    /// `Content-Encoding` is left out of a decoded copy, which holds identity
    /// bytes.
    #[must_use]
    pub fn kept_headers(
        response_headers: &http::HeaderMap,
        decoded: bool,
    ) -> Option<Vec<(String, String)>> {
        let mut kept = Vec::new();
        for (name, value) in response_headers {
            if NOT_KEPT.contains(&name.as_str()) || (decoded && name == header::CONTENT_ENCODING) {
                continue;
            }
            let value = value.to_str().ok()?;
            kept.push((name.as_str().to_owned(), value.to_owned()));
        }
        Some(kept)
    }

    /// The response this copy is served as, `now` seconds since the Unix
    /// epoch, its `Age` counting the time it has been kept.
    pub fn into_response(self, now: u64) -> Response<EdgeBody> {
        let mut response = Response::new(EdgeBody::empty());
        *response.status_mut() = StatusCode::OK;
        for (name, value) in &self.headers {
            // Both were checked when the copy was read.
            if let (Ok(name), Ok(value)) = (
                HeaderName::from_bytes(name.as_bytes()),
                HeaderValue::from_str(value),
            ) {
                response.headers_mut().append(name, value);
            }
        }
        let age = self
            .age_at_made
            .saturating_add(now.saturating_sub(self.made_at));
        response
            .headers_mut()
            .insert(header::AGE, HeaderValue::from(age));
        response.headers_mut().insert(
            header::CONTENT_LENGTH,
            HeaderValue::from(self.body.len() as u64),
        );
        *response.body_mut() = EdgeBody::from(self.body);
        response
    }
}

/// What the module holds for one request's values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetEntry {
    /// A response every request with the same values may be served.
    Copy(AssetCopy),
    /// The origin's response for these values may not be shared, so each
    /// request fetches its own. Held briefly, so the requests that follow do
    /// not wait on a load that ends the same way.
    NotShareable,
}

/// The first byte of a held [`AssetEntry`], which says its kind.
const ENTRY_NOT_SHAREABLE: u8 = 0;
const ENTRY_COPY: u8 = 1;

/// The bytes of a copy's description length, a little-endian `u32`.
const DESCRIPTION_LENGTH_BYTES: usize = size_of::<u32>();

/// The bytes a held copy starts with, its kind byte and its description
/// length.
const COPY_HEAD_BYTES: usize = 1 + DESCRIPTION_LENGTH_BYTES;

/// Why a held asset entry could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, derive_more::Display)]
pub enum AssetCacheMiss {
    /// Written by a build keeping another format.
    #[display("the kept asset was written in another format")]
    OtherFormat,
    /// Unreadable, or shorter than it says it is.
    #[display("the kept asset is unreadable")]
    Unreadable,
}

impl core::error::Error for AssetCacheMiss {}

impl AssetEntry {
    /// The bytes the cache holds for this entry, being a kind byte, then for a
    /// copy the length of its description as a little-endian `u32`, the
    /// description, one line per field, and the body.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::NotShareable => vec![ENTRY_NOT_SHAREABLE],
            Self::Copy(copy) => {
                let mut description = format!(
                    "v={ASSET_SCHEMA_VERSION}\nd={}\nt={}\na={}\nlen={}",
                    u8::from(copy.decoded),
                    copy.made_at,
                    copy.age_at_made,
                    copy.body.len()
                );
                for (name, value) in &copy.headers {
                    description.push_str(&format!("\nh={name}:{value}"));
                }
                let length = u32::try_from(description.len()).unwrap_or(u32::MAX);
                let mut bytes =
                    Vec::with_capacity(COPY_HEAD_BYTES + description.len() + copy.body.len());
                bytes.push(ENTRY_COPY);
                bytes.extend_from_slice(&length.to_le_bytes());
                bytes.extend_from_slice(description.as_bytes());
                bytes.extend_from_slice(&copy.body);
                bytes
            }
        }
    }

    /// The entry `bytes` hold.
    ///
    /// # Errors
    ///
    /// When the bytes are not an entry this build wrote.
    pub fn decode(bytes: &[u8]) -> Result<Self, AssetCacheMiss> {
        match bytes.split_first() {
            Some((&ENTRY_NOT_SHAREABLE, [])) => Ok(Self::NotShareable),
            Some((&ENTRY_COPY, rest)) => {
                let (length, rest) = rest
                    .split_first_chunk::<DESCRIPTION_LENGTH_BYTES>()
                    .ok_or(AssetCacheMiss::Unreadable)?;
                let length = usize::try_from(u32::from_le_bytes(*length))
                    .map_err(|_| AssetCacheMiss::Unreadable)?;
                if rest.len() < length {
                    return Err(AssetCacheMiss::Unreadable);
                }
                let (description, body) = rest.split_at(length);
                let description =
                    std::str::from_utf8(description).map_err(|_| AssetCacheMiss::Unreadable)?;
                decode_copy(description, body).map(Self::Copy)
            }
            _ => Err(AssetCacheMiss::Unreadable),
        }
    }
}

fn decode_copy(description: &str, body: &[u8]) -> Result<AssetCopy, AssetCacheMiss> {
    let unreadable = AssetCacheMiss::Unreadable;
    let mut version = None;
    let mut decoded = None;
    let mut made_at = None;
    let mut age_at_made = None;
    let mut length = None;
    let mut headers = Vec::new();
    for line in description.split('\n') {
        let (field, value) = line.split_once('=').ok_or(unreadable)?;
        let once = |held: Option<u64>| held.is_none().then_some(()).ok_or(unreadable);
        match field {
            "v" => {
                once(version)?;
                version = Some(value.parse::<u64>().map_err(|_| unreadable)?);
            }
            "d" => {
                decoded = Some(match value {
                    "0" if decoded.is_none() => false,
                    "1" if decoded.is_none() => true,
                    _ => return Err(unreadable),
                });
            }
            "t" => {
                once(made_at)?;
                made_at = Some(value.parse().map_err(|_| unreadable)?);
            }
            "a" => {
                once(age_at_made)?;
                age_at_made = Some(value.parse().map_err(|_| unreadable)?);
            }
            "len" => {
                once(length)?;
                length = Some(value.parse().map_err(|_| unreadable)?);
            }
            "h" => {
                let (name, header_value) = value.split_once(':').ok_or(unreadable)?;
                let parsed = HeaderName::from_bytes(name.as_bytes()).map_err(|_| unreadable)?;
                if parsed.as_str() != name || NOT_KEPT.contains(&name) {
                    return Err(unreadable);
                }
                HeaderValue::from_str(header_value).map_err(|_| unreadable)?;
                headers.push((name.to_owned(), header_value.to_owned()));
            }
            _ => return Err(unreadable),
        }
    }
    if version.ok_or(unreadable)? != u64::from(ASSET_SCHEMA_VERSION) {
        return Err(AssetCacheMiss::OtherFormat);
    }
    if length.ok_or(unreadable)? != body.len() as u64 {
        return Err(unreadable);
    }
    Ok(AssetCopy {
        headers,
        decoded: decoded.ok_or(unreadable)?,
        made_at: made_at.ok_or(unreadable)?,
        age_at_made: age_at_made.ok_or(unreadable)?,
        body: body.to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> AssetCacheKey {
        AssetCacheKey {
            url: "https://origin.example.com/assets/site.css".to_owned(),
            request_host: "www.example.com".to_owned(),
            request_scheme: "https".to_owned(),
            origin_identity: "https://origin.example.com\0origin.example.com".to_owned(),
        }
    }

    fn copy() -> AssetCopy {
        AssetCopy {
            headers: vec![
                ("content-type".to_owned(), "text/css".to_owned()),
                ("vary".to_owned(), "Accept-Encoding".to_owned()),
            ],
            decoded: true,
            made_at: 1_000,
            age_at_made: 5,
            body: b"body { color: red }".to_vec(),
        }
    }

    #[test]
    fn every_input_changes_the_key_and_none_appears_in_it() {
        let base = key().to_cache_key();
        let changes: [fn(&mut AssetCacheKey); 4] = [
            |key| key.url.push_str("?v=2"),
            |key| key.request_host = "other.example.com".to_owned(),
            |key| key.request_scheme = "http".to_owned(),
            |key| key.origin_identity.push('x'),
        ];
        for change in changes {
            let mut changed = key();
            change(&mut changed);
            assert_ne!(changed.to_cache_key(), base);
        }
        assert!(base.starts_with("ts-asset-cache-v1-"));
        for named in ["example.com", "site.css"] {
            assert!(!base.contains(named), "the key names `{named}`: {base}");
        }
    }

    #[test]
    fn an_entry_reads_back_as_written() {
        for entry in [AssetEntry::Copy(copy()), AssetEntry::NotShareable] {
            assert_eq!(AssetEntry::decode(&entry.encode()), Ok(entry));
        }
    }

    #[test]
    fn a_short_foreign_or_other_format_entry_is_not_read() {
        let bytes = AssetEntry::Copy(copy()).encode();
        assert_eq!(
            AssetEntry::decode(&bytes[..bytes.len() - 1]),
            Err(AssetCacheMiss::Unreadable)
        );
        for foreign in [&b""[..], b"\x07", b"\x01\xff\xff", b"\x00extra"] {
            assert!(AssetEntry::decode(foreign).is_err(), "{foreign:?}");
        }
        let other_version = String::from_utf8(bytes.clone())
            .expect("the test copy is text")
            .replacen("v=1", "v=9", 1);
        assert_eq!(
            AssetEntry::decode(other_version.as_bytes()),
            Err(AssetCacheMiss::OtherFormat)
        );
    }

    #[test]
    fn a_copy_keeps_no_connection_cookie_or_length_header() {
        let mut response = http::HeaderMap::new();
        for (name, value) in [
            ("content-type", "image/png"),
            ("content-encoding", "gzip"),
            ("set-cookie", "a=b"),
            ("content-length", "10"),
            ("age", "3"),
            ("connection", "keep-alive"),
            ("cache-control", "public, max-age=60"),
        ] {
            response.append(
                HeaderName::from_static(name),
                HeaderValue::from_static(value),
            );
        }

        let raw = AssetCopy::kept_headers(&response, false).expect("should keep the headers");
        let decoded = AssetCopy::kept_headers(&response, true).expect("should keep the headers");

        let names = |kept: &[(String, String)]| -> Vec<String> {
            kept.iter().map(|(name, _)| name.clone()).collect()
        };
        assert_eq!(
            names(&raw),
            ["content-type", "content-encoding", "cache-control"]
        );
        assert_eq!(
            names(&decoded),
            ["content-type", "cache-control"],
            "a decoded copy's encoding is set for each reader"
        );
    }

    #[test]
    fn a_served_copy_counts_its_age_from_when_it_was_made() {
        let response = copy().into_response(1_030);

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::AGE),
            Some(&HeaderValue::from_static("35"))
        );
        assert_eq!(
            response.headers().get(header::CONTENT_LENGTH),
            Some(&HeaderValue::from_static("19"))
        );
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE),
            Some(&HeaderValue::from_static("text/css"))
        );
    }
}
