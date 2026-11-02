//! Identity provider interface (spec §7).
//!
//! Providers are pure: they build a lookup request and parse its response.
//! They never perform HTTP, KV, cookie or clock work; the coordinator in
//! [`crate::ec::coordinator`] executes requests and applies consent,
//! transport, validation, expiry and persistence policy.
//!
//! Provider implementations live in the `trusted-server-identity-providers`
//! crate, which depends on core; core never depends on it.

use core::fmt;
use std::borrow::Cow;
use std::collections::HashSet;
use std::net::IpAddr;
use std::num::NonZeroUsize;
use std::time::Duration;

use error_stack::{Report, ResultExt as _};
use serde::Deserialize as _;
use serde::de::DeserializeOwned;
use serde_json::value::RawValue;
use url::Url;

use crate::consent::ConsentContext;
use crate::ec::eid_types::{ProviderId, SourceDomain, UnixSeconds};
use crate::ec::kv_types::{KvEid, KvEntry};
use crate::ec::registry::PartnerRegistry;
use crate::error::TrustedServerError;
use crate::settings::{Ec, IdentityProviderConfig, ProviderSecrets};

/// Builds lookup requests and parses lookup responses for one identity provider.
///
/// Implementations must not perform I/O or read the clock; the coordinator
/// executes requests and applies consent, transport, validation, expiry and
/// persistence policy. Coordinators are built once and shared for the
/// application's lifetime, so implementations are `Send + Sync`.
pub trait IdentityProvider: Send + Sync {
    /// Describes which sources and EID shapes this provider can return.
    fn capabilities(&self) -> &ProviderCapabilities;

    /// Builds the lookup request for the given due sources.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::Request`] when the input cannot form a request.
    fn build_request(
        &self,
        lookup: &LookupInput<'_>,
    ) -> Result<ProviderRequest, Report<ProviderError>>;

    /// Parses a provider response, borrowing from its body.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::InvalidResponse`] when the body cannot be
    /// parsed at all. Individually malformed entries are returned and dropped
    /// by the coordinator's validation, not reported as an error.
    fn parse_response<'body>(
        &self,
        response: ProviderResponse<'body>,
    ) -> Result<LookupOutcome<'body>, Report<ProviderError>>;

    /// Creates IDs for due sources without a lookup, for a provider whose
    /// IDs the operator owns rather than a vendor server.
    ///
    /// Runs when a source is due. A returned entry goes through the same
    /// validation and write plan as a lookup result, and no request is built.
    /// The default creates nothing.
    fn generate(&self, lookup: &LookupInput<'_>) -> Option<Vec<ProviderEid<'static>>> {
        let _ = lookup;
        None
    }

    /// Reads IDs from the vendor's own first-party cookies on the request.
    ///
    /// Runs before any lookup for providers with due sources. Returned
    /// entries go through the same validation, consent gates and write plan
    /// as lookup results, and a kept UID makes the provider skip its lookup
    /// for this navigation. The default reads nothing.
    fn read_request_cookies<'cookie>(
        &self,
        cookies: RequestCookies<'cookie>,
    ) -> Option<Vec<ProviderEid<'cookie>>> {
        let _ = cookies;
        None
    }
}

impl<P: IdentityProvider + ?Sized> IdentityProvider for Box<P> {
    fn capabilities(&self) -> &ProviderCapabilities {
        (**self).capabilities()
    }

    fn build_request(
        &self,
        lookup: &LookupInput<'_>,
    ) -> Result<ProviderRequest, Report<ProviderError>> {
        (**self).build_request(lookup)
    }

    fn parse_response<'body>(
        &self,
        response: ProviderResponse<'body>,
    ) -> Result<LookupOutcome<'body>, Report<ProviderError>> {
        (**self).parse_response(response)
    }

    fn generate(&self, lookup: &LookupInput<'_>) -> Option<Vec<ProviderEid<'static>>> {
        (**self).generate(lookup)
    }

    fn read_request_cookies<'cookie>(
        &self,
        cookies: RequestCookies<'cookie>,
    ) -> Option<Vec<ProviderEid<'cookie>>> {
        (**self).read_request_cookies(cookies)
    }
}

/// The request's `Cookie` header, lent to providers that read their own
/// first-party cookies.
#[derive(Debug, Clone, Copy, Default)]
pub struct RequestCookies<'a>(&'a str);

impl<'a> RequestCookies<'a> {
    /// Wraps a `Cookie` header value (pairs separated by `;`).
    #[must_use]
    pub fn new(header: &'a str) -> Self {
        Self(header)
    }

    /// Returns the first value of cookie `name`, undecoded and unquoted.
    ///
    /// An empty value reads as absent.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&'a str> {
        self.0
            .split(';')
            .filter_map(|pair| pair.split_once('='))
            .find(|(key, _)| key.trim() == name)
            .map(|(_, value)| value.trim().trim_matches('"'))
            .filter(|value| !value.is_empty())
    }
}

/// What a provider can return, used by the coordinator to apply shared policy.
#[derive(Debug, Clone)]
pub struct ProviderCapabilities {
    /// Sources this implementation can return and the input each needs.
    pub sources: Box<[SourceCapability]>,
    /// Limits and fields accepted from one response.
    pub shape: EidShape,
}

/// One source a provider can return.
#[derive(Debug, Clone)]
pub struct SourceCapability {
    /// The source domain.
    pub source: SourceDomain,
    /// Visitor input the source needs.
    pub input: SourceInput,
}

/// Visitor input a source needs before it can be looked up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceInput {
    /// Works for any visitor.
    Anonymous,
    /// Needs a hashed email. Always skipped in V1.
    HashedEmail,
}

/// Limits a provider declares for one response.
#[derive(Debug, Clone, Copy)]
pub struct EidShape {
    /// Maximum EID entries kept from one response.
    pub max_eids: NonZeroUsize,
    /// Maximum UIDs kept per EID entry.
    pub max_uids_per_eid: NonZeroUsize,
    /// Whether EID-level fields are kept.
    pub eid_level_fields: EidLevelFields,
}

/// Whether `inserter`, `matcher`, `mm` and EID `ext` are kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EidLevelFields {
    /// The fields are validated and stored.
    Preserved,
    /// The fields are dropped without rejecting the entry.
    Ignored,
}

/// Request-scoped data the coordinator lends to a provider.
#[derive(Debug, Clone, Copy)]
pub struct LookupInput<'a> {
    /// Active EC ID.
    pub ec_id: &'a str,
    /// Request consent.
    pub consent: &'a ConsentContext,
    /// Client signals.
    pub client: ClientSignals<'a>,
    /// Publisher domain.
    pub publisher_domain: &'a str,
    /// Sources due for this provider, already filtered by consent and input.
    pub due_sources: &'a [SourceDomain],
    /// This provider's currently stored entries, borrowed from the row.
    pub current: OwnedEntries<'a>,
    /// The request's cookies, for a vendor first-party cookie a lookup
    /// sends as input.
    pub cookies: RequestCookies<'a>,
    /// Always `None` in V1.
    pub hashed_email: Option<&'a HashedEmail>,
}

/// Client signals lent to providers.
#[derive(Debug, Clone, Copy)]
pub struct ClientSignals<'a> {
    /// Client IP address.
    pub ip: Option<IpAddr>,
    /// Client `User-Agent`.
    pub user_agent: Option<&'a str>,
}

/// This provider's stored entries, borrowed from the row.
///
/// Only entries whose every UID this provider wrote are visible, so another
/// provider's refresh state never reaches it.
#[derive(Debug, Clone, Copy)]
pub struct OwnedEntries<'a> {
    entry: Option<&'a KvEntry>,
    sources: &'a [SourceDomain],
    owner: Option<&'a ProviderId>,
}

impl<'a> OwnedEntries<'a> {
    /// Views the entries of `entry` for `sources` that `owner` wrote.
    #[must_use]
    pub fn new(
        entry: Option<&'a KvEntry>,
        sources: &'a [SourceDomain],
        owner: &'a ProviderId,
    ) -> Self {
        Self {
            entry,
            sources,
            owner: Some(owner),
        }
    }

    /// A view holding no entries, for a provider with nothing stored.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            entry: None,
            sources: &[],
            owner: None,
        }
    }

    /// Iterates `(source, entry)` pairs in source order.
    pub fn iter(&self) -> impl Iterator<Item = (&'a str, &'a KvEid)> + 'a {
        let entry = self.entry;
        let owner = self.owner;
        self.sources.iter().flat_map(move |source| {
            entry
                .and_then(|entry| entry.ids.get_key_value(source.as_str()))
                .into_iter()
                .flat_map(move |(key, record)| {
                    record
                        .eids()
                        .iter()
                        .filter(move |eid| eid.uids().iter().all(|uid| uid.owner() == owner))
                        .map(move |eid| (key.as_str(), eid))
                })
        })
    }
}

/// A validated lowercase hex SHA-256 email digest. V1 never constructs one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HashedEmail(Box<str>);

impl HashedEmail {
    /// Returns the digest.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An outbound request description. The coordinator, not the provider, sends it.
///
/// `Debug` is implemented by hand and prints only the method and host, because
/// headers and query strings may carry secrets. Providers mark secret headers
/// with `HeaderValue::set_sensitive(true)`.
pub struct ProviderRequest {
    /// HTTP method.
    pub method: http::Method,
    /// Request URL; must be `https` with an allowlisted host.
    pub url: Url,
    /// Request headers.
    pub headers: http::HeaderMap,
    /// Request body.
    pub body: bytes::Bytes,
    /// Provider state returned with the response in
    /// [`ProviderResponse::context`]; never sent.
    pub context: Option<String>,
}

impl fmt::Debug for ProviderRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderRequest")
            .field("method", &self.method)
            .field("host", &self.url.host_str())
            .finish_non_exhaustive()
    }
}

/// A provider response, borrowed from the transport.
///
/// Its `Debug` output omits the body, headers and context, which can hold
/// identifiers or a reply-decryption key.
#[derive(Clone, Copy)]
pub struct ProviderResponse<'body> {
    /// Response status.
    pub status: http::StatusCode,
    /// Response headers.
    pub headers: &'body http::HeaderMap,
    /// Response body.
    pub body: &'body [u8],
    /// The [`ProviderRequest::context`] of the request this answers.
    pub context: Option<&'body str>,
}

impl fmt::Debug for ProviderResponse<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderResponse")
            .field("status", &self.status)
            .field("body_len", &self.body.len())
            .field("has_context", &self.context.is_some())
            .finish_non_exhaustive()
    }
}

/// A parsed provider response.
#[derive(Debug)]
pub enum LookupOutcome<'body> {
    /// Entries were returned.
    Found(Vec<ProviderEid<'body>>),
    /// The provider knows no ID for this visitor.
    NoMatch,
    /// The visitor opted out with the provider; its stored UIDs are removed.
    OptedOut,
}

/// One entry as returned by a provider, before validation.
#[derive(Debug)]
pub struct ProviderEid<'body> {
    /// EID source domain.
    pub source: Cow<'body, str>,
    /// `OpenRTB` `inserter`.
    pub inserter: Option<Cow<'body, str>>,
    /// `OpenRTB` `matcher`.
    pub matcher: Option<Cow<'body, str>>,
    /// `OpenRTB` `mm`.
    pub mm: Option<i32>,
    /// EID-level `ext`, borrowed from the body or built by the provider.
    pub ext: Option<Cow<'body, RawValue>>,
    /// UIDs of the entry.
    pub uids: Vec<ProviderUid<'body>>,
}

/// One UID as returned by a provider, before validation.
#[derive(Debug)]
pub struct ProviderUid<'body> {
    /// UID value.
    pub id: Cow<'body, str>,
    /// Agent type; filled from partner config when missing.
    pub atype: Option<i32>,
    /// UID-level `ext`, borrowed from the body or built by the provider.
    pub ext: Option<Cow<'body, RawValue>>,
    /// Expiry as the provider states it.
    pub expiry: ProviderExpiry,
    /// Private refresh state kept with the UID in the row and never served.
    pub refresh: Option<ProviderRefresh<'body>>,
}

/// Refresh state a provider asks the framework to keep with a UID.
///
/// The stored UID's source becomes due again at `from`, and the state is
/// handed back through [`LookupInput::current`]. `Debug` hides the state,
/// which may hold a credential.
pub struct ProviderRefresh<'body> {
    /// Provider-defined state, such as a refresh token.
    pub state: Cow<'body, str>,
    /// When the provider should be asked to refresh the UID.
    pub from: ProviderExpiry,
}

impl fmt::Debug for ProviderRefresh<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderRefresh")
            .field("from", &self.from)
            .finish_non_exhaustive()
    }
}

/// Expiry as a provider states it. The coordinator turns it into
/// [`crate::ec::eid_types::Expiry`] using its own clock, so providers never
/// read time.
#[derive(Debug, Clone, Copy)]
pub enum ProviderExpiry {
    /// No expiry was stated.
    Unspecified,
    /// Expires at this time.
    At(UnixSeconds),
    /// Expires this long after the request.
    After(Duration),
}

/// Why a provider could not build a request or parse a response.
#[derive(Debug, derive_more::Display)]
pub enum ProviderError {
    /// The input cannot form a request.
    #[display("provider request could not be built")]
    Request,
    /// The body cannot be parsed at all.
    #[display("provider response could not be parsed")]
    InvalidResponse,
}

impl core::error::Error for ProviderError {}

/// Typed constructor for one provider implementation.
pub trait ProviderKind {
    /// Value of `kind` in `[[ec.identity_providers]]`.
    const KIND: &'static str;
    /// The provider-specific `settings` table.
    type Settings: DeserializeOwned;
    /// The constructed provider.
    type Provider: IdentityProvider + 'static;

    /// Builds the provider from shared and provider-specific configuration.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderConfigError::Invalid`] when the settings are unusable.
    fn build(
        common: &ProviderCommon<'_>,
        settings: Self::Settings,
    ) -> Result<Self::Provider, Report<ProviderConfigError>>;
}

/// Shared configuration every provider receives at build time.
#[derive(Debug, Clone, Copy)]
pub struct ProviderCommon<'a> {
    /// Configured provider ID, for logs that must tell instances apart.
    pub id: &'a str,
    /// Configured lookup endpoint, absent for cookie-only kinds.
    pub endpoint: Option<&'a Url>,
    /// Configured sources.
    pub sources: &'a [SourceDomain],
    /// Resolved secrets.
    pub secrets: &'a ProviderSecrets,
}

/// Why a provider configuration was rejected.
#[derive(Debug, derive_more::Display)]
pub enum ProviderConfigError {
    /// No implementation is registered for the configured `kind`.
    #[display("provider kind is not registered")]
    UnknownKind,
    /// The configuration is unusable.
    #[display("provider configuration is invalid")]
    Invalid,
}

impl core::error::Error for ProviderConfigError {}

/// Parses `config.settings` for `K` and builds a boxed provider.
///
/// # Errors
///
/// Returns [`ProviderConfigError::Invalid`] when the settings do not
/// deserialize into `K::Settings`, a source or the endpoint is invalid, or
/// [`ProviderKind::build`] rejects them.
pub fn build_kind<K: ProviderKind>(
    config: &IdentityProviderConfig,
    secrets: &ProviderSecrets,
) -> Result<Box<dyn IdentityProvider>, Report<ProviderConfigError>> {
    let settings = K::Settings::deserialize(serde_json::Value::Object(config.settings.clone()))
        .change_context(ProviderConfigError::Invalid)
        .attach_with(|| format!("settings for provider `{}` are invalid", config.id))?;
    let endpoint = parse_endpoint(config)?;
    let sources = parse_sources(config)?;
    let common = ProviderCommon {
        id: &config.id,
        endpoint: endpoint.as_ref(),
        sources: &sources,
        secrets,
    };
    let provider = K::build(&common, settings)?;
    Ok(Box::new(provider))
}

/// Parses a provider's configured endpoint, when one is set.
fn parse_endpoint(
    config: &IdentityProviderConfig,
) -> Result<Option<Url>, Report<ProviderConfigError>> {
    config
        .endpoint
        .as_deref()
        .map(|endpoint| {
            Url::parse(endpoint)
                .change_context(ProviderConfigError::Invalid)
                .attach_with(|| format!("endpoint for provider `{}` is not a URL", config.id))
        })
        .transpose()
}

/// Parses and normalizes a provider's configured sources.
pub(crate) fn parse_sources(
    config: &IdentityProviderConfig,
) -> Result<Box<[SourceDomain]>, Report<ProviderConfigError>> {
    config
        .sources
        .iter()
        .map(|source| SourceDomain::try_from(source.as_str()))
        .collect::<Result<Box<[_]>, _>>()
        .change_context(ProviderConfigError::Invalid)
        .attach_with(|| format!("provider `{}` has an invalid source", config.id))
}

/// Whether `url` is `https` and its host is one of `allowed_domains`,
/// compared ASCII case-insensitively.
#[must_use]
pub(crate) fn destination_allowed<S: AsRef<str>>(url: &Url, allowed_domains: &[S]) -> bool {
    url.scheme() == "https"
        && url.host_str().is_some_and(|host| {
            allowed_domains
                .iter()
                .any(|allowed| allowed.as_ref().eq_ignore_ascii_case(host))
        })
}

/// Checks the configuration rules core owns for every enabled provider
/// (spec §12.1): a configured endpoint is an `https` URL whose host is in
/// `allowed_domains`, it claims at least one source, every source has an
/// `[[ec.partners]]` entry, no source is claimed by two enabled providers, and
/// every enabled provider has a non-empty id of its own.
///
/// Unknown kinds and capability checks need the provider crate; the adapter
/// builds the coordinators at startup and fails to start on either.
///
/// # Errors
///
/// Returns [`ProviderConfigError::Invalid`] naming the first offending
/// provider.
pub fn validate_enabled_configs<'a>(
    configs: impl IntoIterator<Item = &'a IdentityProviderConfig>,
    partners: &PartnerRegistry,
) -> Result<(), Report<ProviderConfigError>> {
    let mut claimed: HashSet<SourceDomain> = HashSet::new();
    let mut ids: HashSet<&str> = HashSet::new();
    for config in configs.into_iter().filter(|config| config.enabled) {
        // The id keys UID ownership, opt-outs and the rate-limit bucket in
        // the row, so two providers sharing one would read, refresh and clear
        // each other's IDs.
        let id = config.id.trim();
        if id.is_empty() || !ids.insert(id) {
            return Err(Report::new(ProviderConfigError::Invalid).attach(format!(
                "provider id `{}` is empty or used by another enabled provider",
                config.id
            )));
        }
        let endpoint = parse_endpoint(config)?;
        if endpoint.is_some_and(|endpoint| !destination_allowed(&endpoint, &config.allowed_domains))
        {
            return Err(Report::new(ProviderConfigError::Invalid).attach(format!(
                "endpoint for provider `{}` must be https with a host in allowed_domains",
                config.id
            )));
        }
        let sources = parse_sources(config)?;
        if sources.is_empty() {
            return Err(Report::new(ProviderConfigError::Invalid)
                .attach(format!("provider `{}` claims no sources", config.id)));
        }
        for source in sources {
            if partners.find_by_source_domain(source.as_str()).is_none() {
                return Err(Report::new(ProviderConfigError::Invalid).attach(format!(
                    "source `{source}` of provider `{}` has no [[ec.partners]] entry",
                    config.id
                )));
            }
            if !claimed.insert(source.clone()) {
                return Err(Report::new(ProviderConfigError::Invalid).attach(format!(
                    "source `{source}` of provider `{}` is claimed by another enabled provider",
                    config.id
                )));
            }
        }
    }
    Ok(())
}

/// Fails when `[ec]` enables an identity provider on an adapter that has no
/// provider implementations.
///
/// Ignoring the provider would leave its sources open to browser ingest with
/// no sign that it never runs.
///
/// # Errors
///
/// Returns [`TrustedServerError::Configuration`] naming the first enabled
/// provider.
pub fn reject_enabled_providers(ec: &Ec, adapter: &str) -> Result<(), Report<TrustedServerError>> {
    match ec.identity_providers.iter().find(|config| config.enabled) {
        Some(config) => Err(Report::new(TrustedServerError::Configuration {
            message: format!(
                "identity provider `{}` is enabled, but the {adapter} adapter does not run identity providers",
                config.id
            ),
        })),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use serde_json::json;

    struct Fixed(ProviderCapabilities);

    impl IdentityProvider for Fixed {
        fn capabilities(&self) -> &ProviderCapabilities {
            &self.0
        }

        fn build_request(
            &self,
            _lookup: &LookupInput<'_>,
        ) -> Result<ProviderRequest, Report<ProviderError>> {
            Err(Report::new(ProviderError::Request))
        }

        fn parse_response<'body>(
            &self,
            _response: ProviderResponse<'body>,
        ) -> Result<LookupOutcome<'body>, Report<ProviderError>> {
            Ok(LookupOutcome::NoMatch)
        }
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct FixedSettings {
        max_eids: usize,
    }

    struct FixedKind;

    impl ProviderKind for FixedKind {
        const KIND: &'static str = "fixed";
        type Settings = FixedSettings;
        type Provider = Fixed;

        fn build(
            common: &ProviderCommon<'_>,
            settings: FixedSettings,
        ) -> Result<Fixed, Report<ProviderConfigError>> {
            let max_eids = NonZeroUsize::new(settings.max_eids)
                .ok_or_else(|| Report::new(ProviderConfigError::Invalid))?;
            Ok(Fixed(ProviderCapabilities {
                sources: common
                    .sources
                    .iter()
                    .map(|source| SourceCapability {
                        source: source.clone(),
                        input: SourceInput::Anonymous,
                    })
                    .collect(),
                shape: EidShape {
                    max_eids,
                    max_uids_per_eid: max_eids,
                    eid_level_fields: EidLevelFields::Preserved,
                },
            }))
        }
    }

    fn config(settings: &serde_json::Value) -> IdentityProviderConfig {
        serde_json::from_value(json!({
            "id": "example-provider",
            "kind": FixedKind::KIND,
            "endpoint": "https://ids.example.com/v1",
            "allowed_domains": ["ids.example.com"],
            "sources": ["ids.example.com"],
            "settings": settings,
        }))
        .expect("should parse provider config")
    }

    fn partners() -> PartnerRegistry {
        let partner = serde_json::from_value(json!({
            "name": "ids", "source_domain": "ids.example.com"
        }))
        .expect("should build partner");
        PartnerRegistry::from_config(&[partner]).expect("should build registry")
    }

    fn assert_invalid(error: &Report<ProviderConfigError>) {
        assert!(
            matches!(error.current_context(), ProviderConfigError::Invalid),
            "should report invalid config"
        );
    }

    #[test]
    fn build_kind_parses_typed_settings_and_boxes_provider() {
        let provider = build_kind::<FixedKind>(
            &config(&json!({ "max_eids": 2 })),
            &ProviderSecrets::default(),
        )
        .expect("should build");

        assert_eq!(
            provider.capabilities().sources.len(),
            1,
            "should claim configured source"
        );
        assert_eq!(
            provider.capabilities().shape.max_eids.get(),
            2,
            "should apply typed settings"
        );
    }

    #[test]
    fn build_kind_rejects_unknown_settings() {
        let error = build_kind::<FixedKind>(
            &config(&json!({ "unexpected": true })),
            &ProviderSecrets::default(),
        )
        .err()
        .expect("should reject");

        assert_invalid(&error);
    }

    #[test]
    fn config_parses_from_toml_with_a_settings_table() {
        let parsed: IdentityProviderConfig = toml::from_str(
            "id = \"p\"\nkind = \"fixed\"\nendpoint = \"https://ids.example.com/\"\n[settings]\nmax_eids = 3",
        )
        .expect("should parse TOML config");

        assert_eq!(parsed.kind(), "fixed", "should keep kind");
        assert!(parsed.enabled, "should default to enabled");
        assert_eq!(
            parsed.settings.get("max_eids"),
            Some(&json!(3)),
            "should keep provider settings"
        );
    }

    #[test]
    fn request_debug_hides_query_and_headers() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            http::HeaderValue::from_static("Bearer example-secret"),
        );
        let request = ProviderRequest {
            method: http::Method::GET,
            url: Url::parse("https://ids.example.com/v1?token=example-secret")
                .expect("should parse"),
            headers,
            body: bytes::Bytes::new(),
            context: None,
        };

        let printed = format!("{request:?}");

        assert!(
            !printed.contains("example-secret"),
            "should not print secrets"
        );
        assert!(printed.contains("ids.example.com"), "should print host");
    }

    #[test]
    fn response_debug_hides_body_and_context() {
        let headers = http::HeaderMap::new();
        let response = ProviderResponse {
            status: http::StatusCode::OK,
            headers: &headers,
            body: b"example-uid",
            context: Some("example-key"),
        };

        let printed = format!("{response:?}");

        assert!(
            !printed.contains("example-uid"),
            "should not print the body"
        );
        assert!(
            !printed.contains("example-key"),
            "should not print the context"
        );
        assert!(printed.contains("200"), "should print the status");
    }

    #[test]
    fn validate_enabled_configs_accepts_valid_config() {
        validate_enabled_configs([&config(&json!({}))], &partners())
            .expect("should accept valid config");
    }

    #[test]
    fn validate_enabled_configs_rejects_endpoint_outside_allowlist() {
        let mut outside = config(&json!({}));
        outside.endpoint = Some("https://other.example.com/".to_owned());
        let mut plain_http = config(&json!({}));
        plain_http.endpoint = Some("http://ids.example.com/".to_owned());

        for rejected in [outside, plain_http] {
            let error = validate_enabled_configs([&rejected], &partners())
                .expect_err("should reject endpoint");
            assert_invalid(&error);
        }
    }

    #[test]
    fn validate_enabled_configs_rejects_source_without_partner() {
        let mut unknown = config(&json!({}));
        unknown.sources = vec!["unknown.example.com".to_owned()];

        let error = validate_enabled_configs([&unknown], &partners())
            .expect_err("should reject source without partner");

        assert_invalid(&error);
    }

    #[test]
    fn validate_enabled_configs_rejects_source_claimed_twice() {
        let first = config(&json!({}));
        let mut second = config(&json!({}));
        second.id = "second-provider".to_owned();

        let error = validate_enabled_configs([&first, &second], &partners())
            .expect_err("should reject double claim");

        assert_invalid(&error);
    }

    #[test]
    fn validate_enabled_configs_rejects_a_provider_without_sources() {
        let mut empty = config(&json!({}));
        empty.sources = Vec::new();

        let error = validate_enabled_configs([&empty], &partners())
            .expect_err("should reject a provider that claims no sources");

        assert_invalid(&error);
    }

    #[test]
    fn validate_enabled_configs_rejects_shared_or_empty_id() {
        let registry = PartnerRegistry::from_config(&[
            serde_json::from_value(json!({
                "name": "ids", "source_domain": "ids.example.com"
            }))
            .expect("should build partner"),
            serde_json::from_value(json!({
                "name": "other", "source_domain": "other.example.com"
            }))
            .expect("should build partner"),
        ])
        .expect("should build registry");
        let first = config(&json!({}));
        let mut shared = config(&json!({}));
        shared.sources = vec!["other.example.com".to_owned()];
        let mut empty = config(&json!({}));
        empty.id = "  ".to_owned();

        let error = validate_enabled_configs([&first, &shared], &registry)
            .expect_err("should reject a shared provider id");
        assert_invalid(&error);
        let error = validate_enabled_configs([&empty], &registry)
            .expect_err("should reject an empty provider id");
        assert_invalid(&error);
    }

    #[test]
    fn reject_enabled_providers_fails_only_for_an_enabled_provider() {
        let mut ec = crate::test_support::tests::create_test_settings().ec;
        let mut disabled = config(&json!({}));
        disabled.enabled = false;
        ec.identity_providers = vec![disabled];
        reject_enabled_providers(&ec, "Axum").expect("should accept a disabled provider");

        ec.identity_providers.push(config(&json!({})));
        let error =
            reject_enabled_providers(&ec, "Axum").expect_err("should reject an enabled provider");

        assert!(
            matches!(
                error.current_context(),
                TrustedServerError::Configuration { .. }
            ),
            "should report a configuration error"
        );
    }

    #[test]
    fn validate_enabled_configs_ignores_disabled_providers() {
        let first = config(&json!({}));
        let mut disabled = config(&json!({}));
        disabled.enabled = false;
        disabled.endpoint = Some("https://other.example.com/".to_owned());

        validate_enabled_configs([&first, &disabled], &partners())
            .expect("should ignore disabled provider");
    }

    #[test]
    fn validate_enabled_configs_accepts_cookie_only_provider_without_endpoint() {
        let mut cookie_only = config(&json!({}));
        cookie_only.endpoint = None;

        validate_enabled_configs([&cookie_only], &partners())
            .expect("should accept a provider without endpoint");
    }

    #[test]
    fn request_cookies_finds_named_values() {
        let cookies = RequestCookies::new("a=1; vendor_id=\"v-1\" ;empty=; vendor_id=v-2");

        assert_eq!(
            cookies.get("vendor_id"),
            Some("v-1"),
            "should find first value"
        );
        assert_eq!(cookies.get("a"), Some("1"), "should find leading cookie");
        assert_eq!(cookies.get("empty"), None, "should ignore empty values");
        assert_eq!(cookies.get("missing"), None, "should miss absent cookies");
    }

    #[test]
    fn owned_entries_yields_only_configured_sources() {
        let entry: KvEntry = serde_json::from_value(json!({
            "v": 1,
            "created": 1,
            "consent": { "ok": true, "updated": 1 },
            "geo": { "country": "US" },
            "ids": {
                "ids.example.com": { "uid": "u-1", "eids": [{ "uids": [{ "id": "u-1", "atype": 3, "owner": "example-provider" }] }] },
                "other.example.com": { "uid": "u-2", "eids": [{ "uids": [{ "id": "u-2", "atype": 3, "owner": "example-provider" }] }] }
            }
        }))
        .expect("should build entry");
        let sources = [SourceDomain::try_from("ids.example.com").expect("should parse")];
        let owner = ProviderId::new("example-provider");

        let owned: Vec<&str> = OwnedEntries::new(Some(&entry), &sources, &owner)
            .iter()
            .map(|(source, _)| source)
            .collect();

        assert_eq!(
            owned,
            ["ids.example.com"],
            "should yield owned sources only"
        );
    }

    #[test]
    fn empty_owned_entries_yields_nothing() {
        assert_eq!(
            OwnedEntries::empty().iter().count(),
            0,
            "should yield no entries"
        );
    }

    #[test]
    fn owned_entries_hides_entries_another_provider_wrote() {
        let entry: KvEntry = serde_json::from_value(json!({
            "v": 1,
            "created": 1,
            "consent": { "ok": true, "updated": 1 },
            "geo": { "country": "US" },
            "ids": {
                "ids.example.com": { "uid": "u-1", "eids": [
                    { "uids": [{ "id": "u-1", "atype": 3, "owner": "former-provider" }] },
                    { "uids": [{ "id": "u-2", "atype": 3 }] }
                ] }
            }
        }))
        .expect("should build entry");
        let sources = [SourceDomain::try_from("ids.example.com").expect("should parse")];
        let owner = ProviderId::new("example-provider");

        let count = OwnedEntries::new(Some(&entry), &sources, &owner)
            .iter()
            .count();

        assert_eq!(count, 0, "should hide entries this provider did not write");
    }
}
