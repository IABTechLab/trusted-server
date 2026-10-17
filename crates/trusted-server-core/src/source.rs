//! Where the content the publisher path and the asset routes serve comes from.
//!
//! A source module supplies it. `[source]` selects the publisher's, and each
//! `[[proxy.asset_routes]]` entry its own in a `source` table of the same
//! shape: `module` names the source and the table beneath it holds the
//! source's settings.
//!
//! ```toml
//! [source]
//! module = "origin"
//!
//! [source.origin]
//! url = "https://origin.example.com"
//! ```
//!
//! For each request a [`Source`] answers where its content comes from, as a
//! [`SourceLocation`]. Core fetches that location over HTTP, keys its caches
//! on it, rewrites the origin's host in the page from it and tells middleware
//! the origin host from it.
//!
//! Core supplies one source, `origin`, an HTTP origin named in its table. A
//! module offers another through
//! [`IntegrationRegistrationBuilder::with_source_module`](crate::integrations::IntegrationRegistrationBuilder::with_source_module).
//! An origin that answers only signed reads is signed through the signer its
//! `auth` table names, see [`crate::origin_auth`].

use std::fmt;
use std::sync::Arc;

use error_stack::Report;
use http::{HeaderMap, Method};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Map, Value};
use url::Url;

use crate::error::TrustedServerError;
use crate::origin_auth::{OriginAuth, OriginSigners, OriginSigning};
use crate::provider_table::SectionModules;
use crate::redacted::{Redacted, SENSITIVE_MARKER};
use crate::settings::OriginQueryPolicy;

/// The section that selects the publisher's source.
pub const SOURCE_SECTION: &str = "source";

/// The name of the source core supplies, as a source table writes it.
pub const ORIGIN: &str = "origin";

/// The full name of the source core supplies.
pub const ORIGIN_MODULE: &str = "source.origin";

/// What a source is told about the request it answers for.
#[derive(Debug, Clone, Copy)]
pub struct SourceRequest<'a> {
    /// The request's method.
    pub method: &'a Method,
    /// The path the request continues with, without its query.
    pub path: &'a str,
    /// The request's query, empty when it has none.
    pub query: &'a str,
    /// The request's headers.
    pub headers: &'a HeaderMap,
    /// The path prefix request addressing removed, if it removed one.
    pub removed_prefix: Option<&'a str>,
}

/// Where the content for a request comes from.
#[derive(Clone)]
pub struct SourceLocation {
    url: String,
    parsed: Url,
    host_header: Option<String>,
    auth: Option<OriginSigning>,
    directory_index: Option<String>,
}

impl fmt::Debug for SourceLocation {
    // The address and the signing can name private storage and credentials,
    // so only the host is shown.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceLocation")
            .field("host", &self.host())
            .finish_non_exhaustive()
    }
}

impl SourceLocation {
    /// A location at `url`, an absolute `http` or `https` address whose path
    /// is the origin's folder.
    ///
    /// # Errors
    ///
    /// When `url` is not one [`validate_url`] accepts.
    pub fn new(url: impl Into<String>) -> Result<Self, Report<TrustedServerError>> {
        let url = url.into().trim().to_owned();
        let parsed = validate_url(&url)
            .map_err(|message| Report::new(TrustedServerError::Configuration { message }))?;
        Ok(Self {
            url,
            parsed,
            host_header: None,
            auth: None,
            directory_index: None,
        })
    }

    /// The same location sending `host_header` as the `Host`.
    #[must_use]
    pub fn with_host_header(mut self, host_header: Option<String>) -> Self {
        self.host_header = host_header;
        self
    }

    /// The same location signing each request with `auth`.
    #[must_use]
    pub fn with_auth(mut self, auth: Option<OriginSigning>) -> Self {
        self.auth = auth;
        self
    }

    /// The same location adding `directory_index` to a path ending in `/`.
    #[must_use]
    pub fn with_directory_index(mut self, directory_index: Option<String>) -> Self {
        self.directory_index = directory_index;
        self
    }

    /// The address as written.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The address, parsed.
    #[must_use]
    pub fn parsed(&self) -> &Url {
        &self.parsed
    }

    /// The origin's host, with its port where the address names one.
    #[must_use]
    pub fn host(&self) -> String {
        match (self.parsed.host_str(), self.parsed.port()) {
            (Some(host), Some(port)) => format!("{host}:{port}"),
            (Some(host), None) => host.to_owned(),
            (None, _) => self.url.clone(),
        }
    }

    /// The `Host` header sent to the origin.
    #[must_use]
    pub fn host_header(&self) -> String {
        self.host_header.clone().unwrap_or_else(|| self.host())
    }

    /// The `Host` header written for this location, where it is not the
    /// address's host.
    #[must_use]
    pub fn host_header_override(&self) -> Option<&str> {
        self.host_header.as_deref()
    }

    /// The signing applied to each request, if any.
    #[must_use]
    pub fn auth(&self) -> Option<&OriginSigning> {
        self.auth.as_ref()
    }

    /// The file added to a path ending in `/`, if any.
    #[must_use]
    pub fn directory_index(&self) -> Option<&str> {
        self.directory_index.as_deref()
    }

    /// Whether the reader's query is left off the request, as the signing
    /// says.
    #[must_use]
    pub fn query_is_stripped(&self) -> bool {
        self.auth
            .as_ref()
            .and_then(OriginSigning::origin_query_policy)
            == Some(OriginQueryPolicy::Strip)
    }

    /// What tells this location's responses apart from another's in a cache:
    /// the address and the `Host` it is asked with.
    #[must_use]
    pub fn identity(&self) -> String {
        format!("{}\0{}", self.url, self.host_header())
    }
}

/// A source of content, built from its settings when the application is
/// built.
pub trait Source: Send + Sync + fmt::Debug {
    /// Where the content for `request` comes from.
    ///
    /// # Errors
    ///
    /// When the source cannot say where the request's content comes from.
    /// Core answers the request with the error's status.
    fn locate(
        &self,
        request: &SourceRequest<'_>,
    ) -> Result<SourceLocation, Report<TrustedServerError>>;
}

/// Builds the source a settings table configures.
pub trait SourceFactory: Send + Sync {
    /// The source `settings` configure, where `setting` names the table in a
    /// refusal.
    ///
    /// # Errors
    ///
    /// When the table cannot be read or fails validation.
    fn build(
        &self,
        settings: &Map<String, Value>,
        setting: &str,
    ) -> Result<Arc<dyn Source>, Report<TrustedServerError>>;
}

/// The settings of core's `origin` source.
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OriginSettings {
    /// The origin's absolute `http` or `https` address. A path names the
    /// folder requests are sent under.
    #[serde(serialize_with = "crate::redacted::sensitive")]
    pub url: String,
    /// The `Host` header to send, where it is not the address's host.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "crate::redacted::sensitive"
    )]
    pub host_header: Option<String>,
    /// The file added to a request path ending in `/`, for an origin such as
    /// object storage that does not answer a folder with its index file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory_index: Option<String>,
    /// Signing of every request, for an origin that answers only signed
    /// reads: the signer's name as `type` and its settings beside it, see
    /// [`crate::origin_auth`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<OriginAuth>,
}

impl fmt::Debug for OriginSettings {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OriginSettings")
            .field("url", &Redacted::new(self.url.clone()))
            .field("host_header", &self.host_header)
            .field("directory_index", &self.directory_index)
            .field("auth", &self.auth)
            .finish()
    }
}

impl OriginSettings {
    /// Settings for the origin at `url`, with nothing else set.
    #[must_use]
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            ..Self::default()
        }
    }

    /// A source table selecting core's `origin` with these settings, as
    /// `[source]` or a route's `source` holds it.
    #[must_use]
    pub fn selection(&self) -> SectionModules {
        let table = match serde_json::to_value(self) {
            Ok(Value::Object(table)) => table,
            _ => Map::new(),
        };
        SectionModules::one(ORIGIN, table)
    }

    /// Reads the table at `setting` as written, trimmed, without checking
    /// what it names, so a check that runs before the table is used can see
    /// it.
    ///
    /// # Errors
    ///
    /// When the table has an unknown key or a value of the wrong type, naming
    /// `setting`.
    pub fn parse(
        settings: &Map<String, Value>,
        setting: &str,
    ) -> Result<Self, Report<TrustedServerError>> {
        let mut origin: Self =
            serde_json::from_value(Value::Object(settings.clone())).map_err(|error| {
                Report::new(TrustedServerError::Configuration {
                    message: format!("[{setting}] could not be read: {error}"),
                })
            })?;
        origin.url = origin.url.trim().to_owned();
        if let Some(auth) = &mut origin.auth {
            auth.normalize();
        }
        Ok(origin)
    }

    /// Reads and checks the table at `setting`. The signer `auth` names is
    /// checked where the source is built, because only the deployment's
    /// builders know the signers on offer.
    ///
    /// # Errors
    ///
    /// When the table cannot be read, or names an address, a `Host`, an index
    /// file or an `auth` core cannot use, naming `setting`.
    pub fn read(
        settings: &Map<String, Value>,
        setting: &str,
    ) -> Result<Self, Report<TrustedServerError>> {
        let origin = Self::parse(settings, setting)?;
        origin.check(setting)?;
        Ok(origin)
    }

    fn check(&self, setting: &str) -> Result<(), Report<TrustedServerError>> {
        let refuse = |message: String| {
            Report::new(TrustedServerError::Configuration {
                message: format!("[{setting}] {message}"),
            })
        };
        let parsed = validate_url(&self.url).map_err(&refuse)?;
        if let Some(host_header) = &self.host_header {
            crate::settings::check_host_header(host_header)
                .map_err(|reason| refuse(format!("host_header {reason}")))?;
        }
        if let Some(index) = &self.directory_index {
            crate::settings::check_directory_index(index)
                .map_err(|reason| refuse(format!("directory_index {reason}")))?;
        }
        if let Some(auth) = &self.auth {
            if parsed.scheme() != "https" {
                return Err(refuse("signs with auth, so url must use https".to_owned()));
            }
            auth.check(&format!("{setting}.auth"))?;
        }
        Ok(())
    }

    /// Whether the address is the unedited template placeholder host.
    #[must_use]
    pub fn is_placeholder(&self) -> bool {
        crate::settings::Publisher::is_placeholder_origin_url(&self.url)
    }

    /// The location these settings name, signing through the signer
    /// `signers` offers under `auth`'s type, where `setting` names the table
    /// in a refusal.
    ///
    /// # Errors
    ///
    /// When the address cannot be read, which [`Self::read`] has already
    /// refused for settings it returned, when `auth` names a signer nothing
    /// offers, or when the signer cannot use its table.
    pub fn location(
        &self,
        signers: &OriginSigners,
        setting: &str,
    ) -> Result<SourceLocation, Report<TrustedServerError>> {
        let auth = self
            .auth
            .as_ref()
            .map(|auth| signers.build(auth, &format!("{setting}.auth")))
            .transpose()?;
        Ok(SourceLocation::new(self.url.clone())?
            .with_host_header(self.host_header.clone())
            .with_auth(auth)
            .with_directory_index(self.directory_index.clone()))
    }
}

/// Core's source, an HTTP origin named in its table, the same for every
/// request.
#[derive(Debug, Clone)]
pub struct OriginSource {
    location: SourceLocation,
}

impl OriginSource {
    /// The source `settings` name, signing through `signers`, where `setting`
    /// names the table in a refusal.
    ///
    /// # Errors
    ///
    /// As [`OriginSettings::location`].
    pub fn new(
        settings: &OriginSettings,
        signers: &OriginSigners,
        setting: &str,
    ) -> Result<Self, Report<TrustedServerError>> {
        Ok(Self {
            location: settings.location(signers, setting)?,
        })
    }
}

impl Source for OriginSource {
    fn locate(
        &self,
        _request: &SourceRequest<'_>,
    ) -> Result<SourceLocation, Report<TrustedServerError>> {
        Ok(self.location.clone())
    }
}

/// Builds [`OriginSource`] from an `origin` table, signing through the
/// signers it was given.
#[derive(Debug, Clone, Default)]
pub struct OriginFactory {
    signers: OriginSigners,
}

impl OriginFactory {
    /// A factory whose sources sign through `signers`.
    #[must_use]
    pub fn new(signers: OriginSigners) -> Self {
        Self { signers }
    }
}

impl SourceFactory for OriginFactory {
    fn build(
        &self,
        settings: &Map<String, Value>,
        setting: &str,
    ) -> Result<Arc<dyn Source>, Report<TrustedServerError>> {
        let origin = OriginSettings::read(settings, setting)?;
        Ok(Arc::new(OriginSource::new(
            &origin,
            &self.signers,
            setting,
        )?))
    }
}

/// The one source `selection` names, as written, with its settings table, or
/// a refusal naming `setting`.
///
/// # Errors
///
/// When the table selects no source, or more than one.
pub fn selected<'a>(
    selection: &'a SectionModules,
    setting: &str,
) -> Result<(&'a str, Map<String, Value>), Report<TrustedServerError>> {
    match selection.selected() {
        [one] => Ok((
            one.as_str(),
            selection.settings_of(one).cloned().unwrap_or_default(),
        )),
        [] => Err(Report::new(TrustedServerError::Configuration {
            message: format!(
                "[{setting}] names no source. Write `module = \"origin\"` and the origin's \
                 address under [{setting}.origin] url"
            ),
        })),
        _ => Err(Report::new(TrustedServerError::Configuration {
            message: format!("[{setting}] names more than one source. Write `module` for one"),
        })),
    }
}

/// Checks a selection of core's `origin` source, which needs no module on
/// offer to read. A selection of another source is checked where the
/// registry builds it.
///
/// # Errors
///
/// When the selection names no source or several, or its `origin` table
/// cannot be used.
pub fn check_selection(
    selection: &SectionModules,
    setting: &str,
) -> Result<(), Report<TrustedServerError>> {
    let (name, table) = selected(selection, setting)?;
    if name == ORIGIN {
        OriginSettings::read(&table, &format!("{setting}.{ORIGIN}"))?;
    }
    Ok(())
}

/// Serializes a source selection, for
/// `#[serde(serialize_with = "crate::source::serialize_selection")]`. While
/// [`crate::redacted::to_marked_value`] runs, the `origin` table's `url` and
/// `host_header` are marked sensitive, as they would be read through
/// [`OriginSettings`], so the configuration page masks them by default.
///
/// # Errors
///
/// As the serializer's own.
pub fn serialize_selection<S>(selection: &SectionModules, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if !crate::redacted::is_marking() {
        return selection.serialize(serializer);
    }
    let mut value = serde_json::to_value(selection).map_err(serde::ser::Error::custom)?;
    if let Some(Value::Object(origin)) = value.get_mut(ORIGIN) {
        for key in ["url", "host_header"] {
            if let Some(field) = origin.remove(key) {
                let mut marked = Map::new();
                marked.insert(SENSITIVE_MARKER.to_owned(), field);
                origin.insert(key.to_owned(), Value::Object(marked));
            }
        }
    }
    value.serialize(serializer)
}

/// Whether `url` is an address a source location can be at: absolute `http`
/// or `https` with a host, no credentials, query, fragment or trailing `/`.
/// A path names a folder.
///
/// # Errors
///
/// A message naming what is wrong.
pub fn validate_url(url: &str) -> Result<Url, String> {
    if url.ends_with('/') {
        return Err(format!("url `{url}` must not end in `/`"));
    }
    let parsed = Url::parse(url)
        .map_err(|error| format!("url `{url}` is not an absolute address: {error}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(format!("url `{url}` must use http or https"));
    }
    if parsed.host_str().is_none() {
        return Err(format!("url `{url}` must name a host"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(format!(
            "url `{url}` must not carry a user name or password"
        ));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(format!("url `{url}` must not carry a query or a fragment"));
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn table(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(table) => table,
            _ => unreachable!("the tests write objects"),
        }
    }

    #[test]
    fn a_location_host_keeps_a_port_the_address_names() {
        for (url, host) in [
            ("https://origin.example.com:8080", "origin.example.com:8080"),
            ("https://origin.example.com", "origin.example.com"),
            ("https://origin.example.com:443", "origin.example.com"),
            ("http://localhost:9090", "localhost:9090"),
            ("http://192.168.1.1:8080", "192.168.1.1:8080"),
            ("http://[::1]:8080", "[::1]:8080"),
        ] {
            let location = SourceLocation::new(url).expect("should accept the address");
            assert_eq!(location.host(), host, "the host of `{url}`");
        }
    }

    #[test]
    fn a_location_refuses_an_address_core_cannot_fetch() {
        for url in [
            "localhost:9090",
            "ftp://assets.example.com",
            "https://assets.example.com/",
            "https://user:pass@assets.example.com",
            "https://assets.example.com?token=abc",
            "https://assets.example.com#fragment",
            "/relative/path",
        ] {
            assert!(SourceLocation::new(url).is_err(), "should refuse `{url}`");
        }
    }

    #[test]
    fn a_location_path_names_a_folder() {
        let location = SourceLocation::new("https://storage.example.com/bucket/site")
            .expect("should accept a folder");
        assert_eq!(location.parsed().path(), "/bucket/site");
        assert_eq!(location.host(), "storage.example.com");
    }

    #[test]
    fn the_host_header_defaults_to_the_address_host() {
        let location =
            SourceLocation::new("https://origin.example.com:8443").expect("should accept");
        assert_eq!(location.host_header(), "origin.example.com:8443");
        assert_eq!(location.host_header_override(), None);

        let location = location.with_host_header(Some("www.example.com".to_owned()));
        assert_eq!(location.host_header(), "www.example.com");
        assert_eq!(location.host_header_override(), Some("www.example.com"));
    }

    #[test]
    fn the_identity_tells_host_headers_apart() {
        let plain = SourceLocation::new("https://origin.example.com").expect("should accept");
        let named = plain
            .clone()
            .with_host_header(Some("www.example.com".to_owned()));
        assert_ne!(plain.identity(), named.identity());
        assert_eq!(
            plain.identity(),
            "https://origin.example.com\0origin.example.com",
            "the identity should match the key the caches used before sources"
        );
    }

    #[test]
    fn origin_settings_read_trim_and_check() {
        let origin = OriginSettings::read(
            &table(json!({ "url": "  https://origin.example.com  ", "host_header": "www.example.com" })),
            "source.origin",
        )
        .expect("should read the table");
        assert_eq!(origin.url, "https://origin.example.com");
        assert_eq!(origin.host_header.as_deref(), Some("www.example.com"));

        for (value, expected) in [
            (
                json!({ "url": "https://origin.example.com", "host_headr": "x" }),
                "host_headr",
            ),
            (json!({ "host_header": "www.example.com" }), "url"),
            (
                json!({ "url": "https://origin.example.com", "host_header": "https://x" }),
                "[source.origin] host_header",
            ),
            (
                json!({ "url": "https://origin.example.com", "directory_index": "a/b.html" }),
                "[source.origin] directory_index",
            ),
            (
                json!({ "url": "http://origin.example.com", "auth": { "type": "fixture_scheme", "realm": "site" } }),
                "[source.origin] signs with auth, so url must use https",
            ),
            (
                json!({ "url": "https://origin.example.com", "auth": { "realm": "site" } }),
                "[source.origin] could not be read",
            ),
            (
                json!({ "url": "https://origin.example.com", "auth": { "type": " " } }),
                "[source.origin.auth] names no signer",
            ),
        ] {
            let err = OriginSettings::read(&table(value.clone()), "source.origin")
                .expect_err("should refuse the table");
            assert!(
                format!("{err:?}").contains(expected),
                "the refusal of {value} should say `{expected}`: {err:?}"
            );
        }
    }

    #[test]
    fn a_selection_reads_back_as_the_settings_it_was_built_from() {
        let origin = OriginSettings {
            host_header: Some("www.example.com".to_owned()),
            directory_index: Some("index.html".to_owned()),
            ..OriginSettings::new("https://storage.example.com/site")
        };
        let selection = origin.selection();

        let (name, table) = selected(&selection, SOURCE_SECTION).expect("should select one");
        assert_eq!(name, ORIGIN);
        let read = OriginSettings::read(&table, "source.origin").expect("should read back");
        assert_eq!(read.url, origin.url);
        assert_eq!(read.host_header, origin.host_header);
        assert_eq!(read.directory_index, origin.directory_index);
        check_selection(&selection, SOURCE_SECTION).expect("should pass the check");
    }

    #[test]
    fn a_selection_must_name_exactly_one_source() {
        let err = check_selection(&SectionModules::default(), SOURCE_SECTION)
            .expect_err("should refuse a section naming no source");
        assert!(format!("{err:?}").contains("names no source"), "{err:?}");
    }

    #[test]
    fn the_origin_source_answers_the_same_location_for_every_request() {
        let source = OriginFactory::default()
            .build(
                &table(json!({ "url": "https://origin.example.com" })),
                "source.origin",
            )
            .expect("should build");
        let headers = HeaderMap::new();
        let location = source
            .locate(&SourceRequest {
                method: &Method::GET,
                path: "/any/path",
                query: "a=1",
                headers: &headers,
                removed_prefix: Some("/www.example.com"),
            })
            .expect("should locate");
        assert_eq!(location.url(), "https://origin.example.com");
    }

    #[test]
    fn the_origin_source_signs_through_the_signer_its_auth_names() {
        use crate::origin_auth::fixture;

        let settings = table(json!({
            "url": "https://storage.example.com/site",
            "auth": { "type": fixture::SIGNER, "realm": "site", "origin_query": "strip" },
        }));

        let err = OriginFactory::default()
            .build(&settings, "source.origin")
            .expect_err("should refuse the signer with none on offer");
        assert!(
            format!("{err:?}").contains(
                "[source.origin.auth] type `fixture_scheme` names an origin signer this \
                 deployment does not offer"
            ),
            "{err:?}"
        );

        let source = OriginFactory::new(fixture::signers())
            .build(&settings, "source.origin")
            .expect("should build with the fixture on offer");
        let headers = HeaderMap::new();
        let location = source
            .locate(&SourceRequest {
                method: &Method::GET,
                path: "/page",
                query: "",
                headers: &headers,
                removed_prefix: None,
            })
            .expect("should locate");
        assert!(
            location.query_is_stripped(),
            "the auth table strips the query"
        );
        let mut sent = HeaderMap::new();
        sent.insert(
            http::header::HOST,
            http::HeaderValue::from_static("storage.example.com"),
        );
        location
            .auth()
            .expect("should sign")
            .sign(&Method::GET, location.parsed(), &mut sent)
            .expect("should sign the request");
        assert!(sent.contains_key(http::header::AUTHORIZATION));

        let err = OriginFactory::new(fixture::signers())
            .build(
                &table(json!({
                    "url": "https://storage.example.com/site",
                    "auth": { "type": fixture::SIGNER, "realm": "" },
                })),
                "source.origin",
            )
            .expect_err("should refuse a table the signer cannot use");
        assert!(
            format!("{err:?}")
                .contains("[source.origin.auth] fixture_scheme realm must not be empty"),
            "{err:?}"
        );
    }
}
