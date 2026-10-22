//! What a provider may read for one call.

use core::cell::RefCell;
use core::fmt;
use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use error_stack::Report;
use rand::RngCore as _;
use serde::de::DeserializeOwned;
use url::Url;

use super::descriptor::{Access, Input, ProviderDescriptor, ResolveKind};
use super::ids::{IdentityProviderId, SourceDomain, UnixTime};
use super::resolution::IdentityProviderError;
use crate::consent::ConsentContext;
use crate::ec::kv_types::KvPartnerId;

/// The raw consent strings a provider forwards, and `gdpr_applies`.
///
/// Providers never decide consent. The coordinator has already applied the
/// descriptor's policy.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ProviderConsent {
    gdpr_applies: bool,
    tc_string: Option<String>,
    gpp_string: Option<String>,
    gpp_section_ids: Option<Vec<u16>>,
    us_privacy: Option<String>,
}

impl fmt::Debug for ProviderConsent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderConsent")
            .field("gdpr_applies", &self.gdpr_applies)
            .field("tc_string", &self.tc_string.as_ref().map(|_| "[redacted]"))
            .field(
                "gpp_string",
                &self.gpp_string.as_ref().map(|_| "[redacted]"),
            )
            .field("gpp_section_ids", &self.gpp_section_ids)
            .field(
                "us_privacy",
                &self.us_privacy.as_ref().map(|_| "[redacted]"),
            )
            .finish()
    }
}

impl ProviderConsent {
    /// The view of a request's consent context.
    #[must_use]
    pub(crate) fn from_context(context: &ConsentContext) -> Self {
        Self {
            gdpr_applies: context.gdpr_applies,
            tc_string: context.raw_tc_string.clone(),
            gpp_string: context.raw_gpp_string.clone(),
            gpp_section_ids: context.gpp_section_ids.clone(),
            us_privacy: context.raw_us_privacy.clone(),
        }
    }

    /// Whether GDPR applies to this request.
    #[must_use]
    pub fn gdpr_applies(&self) -> bool {
        self.gdpr_applies
    }

    /// The raw TCF string.
    #[must_use]
    pub fn raw_tc_string(&self) -> Option<&str> {
        self.tc_string.as_deref()
    }

    /// The raw GPP string.
    #[must_use]
    pub fn raw_gpp_string(&self) -> Option<&str> {
        self.gpp_string.as_deref()
    }

    /// The applicable GPP section IDs.
    #[must_use]
    pub fn gpp_section_ids(&self) -> Option<&[u16]> {
        self.gpp_section_ids.as_deref()
    }

    /// The raw US Privacy string.
    #[must_use]
    pub fn raw_us_privacy(&self) -> Option<&str> {
        self.us_privacy.as_deref()
    }
}

/// Country and subdivision, upper case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    country: String,
    subdivision: Option<String>,
}

impl Region {
    /// Normalizes platform geo: upper case, and an ISO 3166-2 `US-CA` style
    /// region loses its country prefix. `None` without a two-letter country.
    #[must_use]
    pub(crate) fn from_geo(country: &str, region: Option<&str>) -> Option<Self> {
        let country = country.trim().to_ascii_uppercase();
        if country.len() != 2 || !country.bytes().all(|byte| byte.is_ascii_alphabetic()) {
            return None;
        }
        let subdivision = region
            .map(|value| value.trim().to_ascii_uppercase())
            .map(|value| match value.split_once('-') {
                Some((prefix, rest)) if prefix == country => rest.to_owned(),
                _ => value,
            })
            .filter(|value| {
                !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
            });
        Some(Self {
            country,
            subdivision,
        })
    }

    /// ISO 3166-1 alpha-2, upper case, for example `"US"`.
    #[must_use]
    pub fn country(&self) -> &str {
        &self.country
    }

    /// The ISO 3166-2 subdivision without the country prefix, upper case, for
    /// example `"CA"`. `None` when the platform does not know it.
    #[must_use]
    pub fn subdivision(&self) -> Option<&str> {
        self.subdivision.as_deref()
    }

    /// Whether this is `country` and `subdivision`.
    #[must_use]
    pub fn is(&self, country: &str, subdivision: &str) -> bool {
        self.country == country && self.subdivision.as_deref() == Some(subdivision)
    }
}

/// The 32 raw bytes of a SHA-256 of a normalized email address. Personal
/// data: its `Debug` output is redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct HashedEmail([u8; 32]);

impl fmt::Debug for HashedEmail {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("HashedEmail([redacted])")
    }
}

impl HashedEmail {
    /// From digest bytes. No code in v1 constructs one outside tests.
    #[must_use]
    pub(crate) fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The digest.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Lowercase hex, as `LiveRamp` takes it.
    #[must_use]
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Standard base64, as UID2 takes it.
    #[must_use]
    pub fn to_base64(&self) -> String {
        STANDARD.encode(self.0)
    }
}

/// A stored record for one of this provider's sources, valid or expired.
#[derive(Clone, PartialEq)]
pub struct StoredId {
    uid: String,
    atype: Option<i32>,
    ext: Option<serde_json::Value>,
    writer: Option<IdentityProviderId>,
    acquired_at: Option<UnixTime>,
    expires_at: Option<UnixTime>,
    refresh_at: Option<UnixTime>,
}

impl fmt::Debug for StoredId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoredId")
            .field("uid", &"[redacted]")
            .field("writer", &self.writer)
            .field("acquired_at", &self.acquired_at)
            .field("expires_at", &self.expires_at)
            .field("refresh_at", &self.refresh_at)
            .finish_non_exhaustive()
    }
}

impl StoredId {
    /// From a KV record. A writer that does not parse is treated as none.
    #[must_use]
    pub(crate) fn from_record(record: &KvPartnerId) -> Self {
        Self {
            uid: record.uid.clone(),
            atype: record.atype,
            ext: record.ext.clone(),
            writer: record.w.as_deref().and_then(|writer| writer.parse().ok()),
            acquired_at: record.at.map(UnixTime::from_secs),
            expires_at: record.exp.map(UnixTime::from_secs),
            refresh_at: record.ra.map(UnixTime::from_secs),
        }
    }

    /// The UID.
    #[must_use]
    pub fn uid(&self) -> &str {
        &self.uid
    }

    /// Provider-supplied `atype`.
    #[must_use]
    pub fn atype(&self) -> Option<i32> {
        self.atype
    }

    /// UID-level ext, as bidders read it in `uids[].ext`.
    #[must_use]
    pub fn ext(&self) -> Option<&serde_json::Value> {
        self.ext.as_ref()
    }

    /// The provider that wrote it. `None` for browser ingest and legacy rows.
    #[must_use]
    pub fn writer(&self) -> Option<&IdentityProviderId> {
        self.writer.as_ref()
    }

    /// When it was acquired or last refreshed.
    #[must_use]
    pub fn acquired_at(&self) -> Option<UnixTime> {
        self.acquired_at
    }

    /// When it must stop being sent.
    #[must_use]
    pub fn expires_at(&self) -> Option<UnixTime> {
        self.expires_at
    }

    /// When it is due for refresh.
    #[must_use]
    pub fn refresh_at(&self) -> Option<UnixTime> {
        self.refresh_at
    }

    /// Whether `now` is at or past its expiry.
    #[must_use]
    pub fn is_expired(&self, now: UnixTime) -> bool {
        self.expires_at.is_some_and(|expires| now >= expires)
    }
}

/// A source of random bytes.
pub(crate) trait RandomSource: Send + Sync {
    /// Fills `bytes`.
    fn fill(&self, bytes: &mut [u8]);
}

/// The platform generator EC generation uses (`rand::thread_rng`).
pub(crate) struct ThreadRandom;

impl RandomSource for ThreadRandom {
    fn fill(&self, bytes: &mut [u8]) {
        rand::thread_rng().fill_bytes(bytes);
    }
}

/// Deterministic bytes for tests: xorshift64* from a seed.
#[cfg(any(test, feature = "test-utils"))]
pub(crate) struct SeededRandom(std::sync::Mutex<u64>);

#[cfg(any(test, feature = "test-utils"))]
impl SeededRandom {
    /// A generator seeded with `seed`. A zero seed is replaced by one,
    /// because xorshift never leaves zero.
    pub(crate) fn new(seed: u64) -> Self {
        Self(std::sync::Mutex::new(seed.max(1)))
    }
}

#[cfg(any(test, feature = "test-utils"))]
impl RandomSource for SeededRandom {
    fn fill(&self, bytes: &mut [u8]) {
        let mut state = self.0.lock().expect("should lock the seed");
        for byte in bytes {
            *state ^= *state >> 12;
            *state ^= *state << 25;
            *state ^= *state >> 27;
            let [top, ..] = state.wrapping_mul(0x2545_F491_4F6C_DD1D).to_be_bytes();
            *byte = top;
        }
    }
}

/// Every signal the coordinator holds for one request. Crate-private: a
/// provider only ever sees a [`ResolveInput`] filtered by its descriptor.
pub(crate) struct ResolveSignals<'a> {
    pub(crate) client_ip: Option<IpAddr>,
    pub(crate) user_agent: Option<&'a str>,
    pub(crate) page_url: Option<Url>,
    pub(crate) region: Option<Region>,
    pub(crate) hashed_email: Option<HashedEmail>,
    pub(crate) consent: ProviderConsent,
    pub(crate) publisher_domain: &'a str,
    pub(crate) now: UnixTime,
    pub(crate) random: &'a dyn RandomSource,
    pub(crate) ec_id: Option<String>,
    pub(crate) stored: BTreeMap<SourceDomain, StoredId>,
    pub(crate) state_json: Option<String>,
    pub(crate) dry_run: bool,
}

/// What one provider may read for one call.
///
/// Every accessor checks the descriptor. Reading something undeclared fails
/// with `UndeclaredInput` and is also recorded, so the coordinator and the
/// harness catch it even when the provider ignores the error with `.ok()`.
pub struct ResolveInput<'a> {
    descriptor: &'a ProviderDescriptor,
    kinds: BTreeMap<SourceDomain, ResolveKind>,
    signals: &'a ResolveSignals<'a>,
    available: BTreeSet<Input>,
    undeclared: RefCell<Vec<Access>>,
    state_failed: RefCell<bool>,
}

impl fmt::Debug for ResolveInput<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolveInput")
            .field("provider", self.descriptor.id())
            .field("kinds", &self.kinds)
            .finish_non_exhaustive()
    }
}

impl<'a> ResolveInput<'a> {
    /// The input for one call. `kinds` holds only the sources to resolve.
    #[must_use]
    pub(crate) fn new(
        descriptor: &'a ProviderDescriptor,
        kinds: BTreeMap<SourceDomain, ResolveKind>,
        signals: &'a ResolveSignals<'a>,
    ) -> Self {
        let available = descriptor
            .resolves()
            .map(super::descriptor::Resolves::declared_inputs)
            .unwrap_or_default();
        Self {
            descriptor,
            kinds,
            signals,
            available,
            undeclared: RefCell::new(Vec::new()),
            state_failed: RefCell::new(false),
        }
    }

    fn record(&self, access: Access) -> Report<IdentityProviderError> {
        self.undeclared.borrow_mut().push(access);
        Report::new(IdentityProviderError::UndeclaredInput { access })
    }

    fn declared<T>(
        &self,
        input: Input,
        value: Option<T>,
    ) -> Result<T, Report<IdentityProviderError>> {
        if !self.available.contains(&input) {
            return Err(self.record(Access::Input(input)));
        }
        value.ok_or_else(|| Report::new(IdentityProviderError::InputUnavailable { input }))
    }

    fn has_private_state(&self) -> bool {
        self.descriptor
            .resolves()
            .is_some_and(super::descriptor::Resolves::has_private_state)
    }

    /// Undeclared reads recorded so far.
    #[must_use]
    pub(crate) fn recorded_errors(&self) -> Vec<Access> {
        self.undeclared.borrow().clone()
    }

    /// Whether a `state()` call failed to decode.
    #[must_use]
    pub(crate) fn state_decode_failed(&self) -> bool {
        *self.state_failed.borrow()
    }

    /// The sources to resolve this time, each with its own kind.
    pub fn sources(&self) -> impl Iterator<Item = (&SourceDomain, ResolveKind)> + '_ {
        self.kinds.iter().map(|(source, kind)| (source, *kind))
    }

    /// Whether `source` is being resolved for the first time.
    #[must_use]
    pub fn is_initial(&self, source: &SourceDomain) -> bool {
        self.kinds.get(source) == Some(&ResolveKind::Initial)
    }

    /// Whether `source` is being refreshed.
    #[must_use]
    pub fn is_refresh(&self, source: &SourceDomain) -> bool {
        self.kinds.get(source) == Some(&ResolveKind::Refresh)
    }

    /// The descriptor's primary source, for single-source providers.
    #[must_use]
    pub fn primary_source(&self) -> &SourceDomain {
        self.descriptor.primary_source()
    }

    /// The stored record for one of this provider's sources, valid or
    /// expired.
    #[must_use]
    pub fn stored(&self, source: &SourceDomain) -> Option<&StoredId> {
        self.signals.stored.get(source)
    }

    /// This provider's private state, decoded. `Ok(None)` when there is none.
    ///
    /// # Errors
    ///
    /// `InvalidState` when the stored state does not decode as `T`. The
    /// failure is recorded, and the coordinator clears the stored state
    /// whatever the provider returns. Without `with_private_state()` it is
    /// `UndeclaredInput`.
    pub fn state<T: DeserializeOwned>(&self) -> Result<Option<T>, Report<IdentityProviderError>> {
        if !self.has_private_state() {
            return Err(self.record(Access::PrivateState));
        }
        let Some(raw) = self.signals.state_json.as_deref() else {
            return Ok(None);
        };
        serde_json::from_str(raw).map(Some).map_err(|_| {
            *self.state_failed.borrow_mut() = true;
            Report::new(IdentityProviderError::InvalidState)
        })
    }

    /// True during the dry run, when the request is discarded unsent. Always
    /// false for a provider declared `with_private_state()`, which gets no
    /// dry run.
    #[must_use]
    pub fn is_dry_run(&self) -> bool {
        self.signals.dry_run && !self.has_private_state()
    }

    /// The endpoint declared with `Resolves::with_endpoint`.
    ///
    /// # Errors
    ///
    /// `UndeclaredInput` with [`Access::Endpoint`] when none was declared.
    pub fn endpoint(&self) -> Result<&Url, Report<IdentityProviderError>> {
        self.descriptor
            .endpoint()
            .ok_or_else(|| self.record(Access::Endpoint))
    }

    /// The consent strings to forward.
    #[must_use]
    pub fn consent(&self) -> &ProviderConsent {
        &self.signals.consent
    }

    /// The publisher's domain.
    #[must_use]
    pub fn publisher_domain(&self) -> &str {
        self.signals.publisher_domain
    }

    /// The request's time.
    #[must_use]
    pub fn now(&self) -> UnixTime {
        self.signals.now
    }

    /// Random bytes from the platform's generator. Fixed by the harness, so
    /// tests are deterministic.
    #[must_use]
    pub fn random_bytes<const N: usize>(&self) -> [u8; N] {
        let mut bytes = [0u8; N];
        self.signals.random.fill(&mut bytes);
        bytes
    }

    /// The platform's client address.
    ///
    /// # Errors
    ///
    /// `UndeclaredInput` when not declared, `InputUnavailable` when missing.
    pub fn client_ip(&self) -> Result<IpAddr, Report<IdentityProviderError>> {
        self.declared(Input::ClientIp, self.signals.client_ip)
    }

    /// The `User-Agent`.
    ///
    /// # Errors
    ///
    /// `UndeclaredInput` when not declared, `InputUnavailable` when missing.
    pub fn user_agent(&self) -> Result<&str, Report<IdentityProviderError>> {
        self.declared(Input::UserAgent, self.signals.user_agent)
    }

    /// The page URL, without query string or fragment.
    ///
    /// # Errors
    ///
    /// `UndeclaredInput` when not declared, `InputUnavailable` when missing.
    pub fn page_url(&self) -> Result<&Url, Report<IdentityProviderError>> {
        self.declared(Input::PageUrl, self.signals.page_url.as_ref())
    }

    /// Country and subdivision.
    ///
    /// # Errors
    ///
    /// `UndeclaredInput` when not declared, `InputUnavailable` when missing.
    pub fn region(&self) -> Result<&Region, Report<IdentityProviderError>> {
        self.declared(Input::Region, self.signals.region.as_ref())
    }

    /// A hashed email. Never available in v1.
    ///
    /// # Errors
    ///
    /// `UndeclaredInput` when not declared, `InputUnavailable` when missing.
    pub fn hashed_email(&self) -> Result<&HashedEmail, Report<IdentityProviderError>> {
        self.declared(Input::HashedEmail, self.signals.hashed_email.as_ref())
    }

    /// The full EC ID. Only for a resolver declared `with_ec_id()`.
    ///
    /// # Errors
    ///
    /// `UndeclaredInput` otherwise. `InputUnavailable` when the request has
    /// no EC ID.
    pub(in crate::ec::identity) fn ec_id(&self) -> Result<&str, Report<IdentityProviderError>> {
        if !self
            .descriptor
            .resolves()
            .is_some_and(super::descriptor::Resolves::gets_ec_id)
        {
            return Err(self.record(Access::PrivateState));
        }
        self.signals.ec_id.as_deref().ok_or_else(|| {
            Report::new(IdentityProviderError::InputUnavailable {
                input: Input::ClientIp,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::net::IpAddr;

    use super::*;
    use crate::ec::identity::descriptor::{ProviderDescriptor, Resolves};
    use crate::ec::kv_types::KvPartnerId;

    fn descriptor(resolves: Resolves) -> ProviderDescriptor {
        ProviderDescriptor::builder("p".parse().expect("should parse"))
            .with_source("example.com".parse().expect("should parse"))
            .with_resolves(resolves)
            .build()
            .expect("should build")
    }

    fn signals() -> ResolveSignals<'static> {
        ResolveSignals {
            client_ip: Some("192.0.2.10".parse().expect("should parse ip")),
            user_agent: Some("ExampleBrowser/1.0"),
            page_url: None,
            region: Region::from_geo("us", Some("us-ca")),
            hashed_email: None,
            consent: ProviderConsent::default(),
            publisher_domain: "www.example.com",
            now: UnixTime::from_secs(1_760_000_000),
            random: &ThreadRandom,
            ec_id: Some(format!("{}.ABC123", "a".repeat(64))),
            stored: BTreeMap::new(),
            state_json: None,
            dry_run: false,
        }
    }

    fn kinds(
        descriptor: &ProviderDescriptor,
        kind: ResolveKind,
    ) -> BTreeMap<SourceDomain, ResolveKind> {
        descriptor
            .sources()
            .iter()
            .map(|source| (source.clone(), kind))
            .collect()
    }

    #[test]
    fn declared_inputs_are_available_and_wanted_missing_ones_are_unavailable() {
        let descriptor = descriptor(
            Resolves::new()
                .with_endpoint("https://a.example.com")
                .with_needs([Input::ClientIp, Input::Region])
                .with_wants([Input::PageUrl]),
        );
        let signals = signals();
        let input = ResolveInput::new(
            &descriptor,
            kinds(&descriptor, ResolveKind::Initial),
            &signals,
        );

        assert_eq!(
            input.client_ip().expect("should read ip"),
            "192.0.2.10".parse::<IpAddr>().expect("should parse ip"),
            "should return the client ip"
        );
        let region = input.region().expect("should read region");
        assert_eq!(
            (region.country(), region.subdivision()),
            ("US", Some("CA")),
            "should normalize the region"
        );
        assert!(region.is("US", "CA"), "should test country and subdivision");
        let error = input.page_url().expect_err("should be unavailable");
        assert!(
            matches!(
                error.current_context(),
                IdentityProviderError::InputUnavailable {
                    input: Input::PageUrl
                }
            ),
            "a declared but missing input is InputUnavailable"
        );
        assert!(
            input.recorded_errors().is_empty(),
            "declared reads record nothing"
        );
        assert!(
            input.is_initial(input.primary_source()),
            "should be initial"
        );
        assert!(
            !input.is_refresh(input.primary_source()),
            "should not be refresh"
        );
    }

    #[test]
    fn undeclared_reads_are_recorded_even_when_ignored() {
        let descriptor = descriptor(Resolves::new().with_endpoint("https://a.example.com"));
        let signals = signals();
        let input = ResolveInput::new(
            &descriptor,
            kinds(&descriptor, ResolveKind::Initial),
            &signals,
        );

        let ignored = input.user_agent().ok();
        let state = input.state::<serde_json::Value>();

        assert!(ignored.is_none(), "should not return an undeclared input");
        assert!(state.is_err(), "state needs with_private_state");
        assert_eq!(
            input.recorded_errors(),
            vec![Access::Input(Input::UserAgent), Access::PrivateState],
            "should record both undeclared reads"
        );
        input
            .endpoint()
            .expect("the declared endpoint is always available");
    }

    #[test]
    fn state_decodes_and_a_bad_state_is_recorded() {
        #[derive(serde::Deserialize)]
        struct Typed {
            signature: String,
        }

        let descriptor = descriptor(
            Resolves::new()
                .with_endpoint("https://a.example.com")
                .with_private_state(),
        );
        let mut good = signals();
        good.state_json = Some(r#"{"signature":"s"}"#.to_owned());
        let input = ResolveInput::new(&descriptor, kinds(&descriptor, ResolveKind::Refresh), &good);
        let state: Typed = input
            .state()
            .expect("should decode")
            .expect("should be present");
        assert_eq!(state.signature, "s", "should decode the state");
        assert!(
            !input.state_decode_failed(),
            "a good state is not a failure"
        );

        let mut bad = signals();
        bad.state_json = Some(r#""a bare string""#.to_owned());
        let input = ResolveInput::new(&descriptor, kinds(&descriptor, ResolveKind::Refresh), &bad);
        let error = input.state::<Typed>().err().expect("should not decode");
        assert!(
            matches!(error.current_context(), IdentityProviderError::InvalidState),
            "should be InvalidState"
        );
        assert!(input.state_decode_failed(), "should record the failure");
    }

    #[test]
    fn dry_run_is_never_true_for_stateful_providers() {
        let stateful = descriptor(
            Resolves::new()
                .with_endpoint("https://a.example.com")
                .with_private_state(),
        );
        let stateless = descriptor(Resolves::new().with_endpoint("https://a.example.com"));
        let mut dry = signals();
        dry.dry_run = true;
        assert!(
            !ResolveInput::new(&stateful, kinds(&stateful, ResolveKind::Initial), &dry)
                .is_dry_run(),
            "stateful providers get no dry run"
        );
        assert!(
            ResolveInput::new(&stateless, kinds(&stateless, ResolveKind::Initial), &dry)
                .is_dry_run(),
            "stateless providers see the dry run"
        );
    }

    #[test]
    fn ec_id_is_only_for_providers_declared_with_ec_id() {
        let generic = descriptor(Resolves::new().with_host("a.example.com").with_ec_id());
        let other = descriptor(Resolves::new().with_host("a.example.com"));
        let signals = signals();
        ResolveInput::new(&generic, kinds(&generic, ResolveKind::Initial), &signals)
            .ec_id()
            .expect("generic pull reads the EC ID");
        let input = ResolveInput::new(&other, kinds(&other, ResolveKind::Initial), &signals);
        assert!(input.ec_id().is_err(), "others may not");
        assert_eq!(input.recorded_errors().len(), 1, "should record the read");
    }

    #[test]
    fn stored_ids_and_expiry() {
        let record = KvPartnerId {
            uid: "u".to_owned(),
            w: Some("p".to_owned()),
            rv: Some("0123456789abcdef".to_owned()),
            at: Some(10),
            exp: Some(20),
            ra: Some(15),
            ru: None,
            atype: Some(3),
            ext: None,
        };
        let stored = StoredId::from_record(&record);
        assert_eq!(
            stored.writer().map(IdentityProviderId::as_str),
            Some("p"),
            "should parse the writer"
        );
        assert!(
            !stored.is_expired(UnixTime::from_secs(19)),
            "not expired before exp"
        );
        assert!(stored.is_expired(UnixTime::from_secs(20)), "expired at exp");
        assert_eq!(
            stored.refresh_at(),
            Some(UnixTime::from_secs(15)),
            "should expose ra"
        );
        assert_eq!(stored.atype(), Some(3), "should expose atype");
        assert!(
            !format!("{stored:?}").contains("\"u\""),
            "Debug should not print the UID"
        );
    }

    #[test]
    fn hashed_email_debug_is_redacted() {
        let email = HashedEmail::from_bytes([0xab; 32]);
        assert_eq!(email.to_hex(), "ab".repeat(32), "should hex-encode");
        assert_eq!(
            format!("{email:?}"),
            "HashedEmail([redacted])",
            "should redact Debug"
        );
    }

    #[test]
    fn region_rejects_bad_countries_and_strips_prefix_only_for_own_country() {
        assert!(Region::from_geo("USA", None).is_none(), "three letters");
        assert!(Region::from_geo("", None).is_none(), "empty");
        let foreign = Region::from_geo("US", Some("GB-ENG")).expect("should parse");
        assert_eq!(
            foreign.subdivision(),
            None,
            "should drop a subdivision with another country's prefix"
        );
        let plain = Region::from_geo("us", Some("ca")).expect("should parse");
        assert_eq!(plain.subdivision(), Some("CA"), "should upper-case");
    }

    #[test]
    fn seeded_random_is_deterministic() {
        let first = SeededRandom::new(9);
        let second = SeededRandom::new(9);
        let mut a = [0u8; 16];
        let mut b = [0u8; 16];
        first.fill(&mut a);
        second.fill(&mut b);
        assert_eq!(a, b, "same seed, same bytes");
        let mut c = [0u8; 16];
        first.fill(&mut c);
        assert_ne!(a, c, "the stream advances");
    }
}
