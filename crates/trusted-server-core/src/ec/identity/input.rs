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

use super::descriptor::{Access, Input, ProviderDescriptor, ResolveKind, Resolves};
use super::exchange::ProviderResponse;
use super::ids::{IdentityProviderId, SourceDomain, UnixTime};
use super::resolution::IdentityProviderError;
use crate::consent::ConsentContext;
use crate::ec::kv_types::KvPartnerId;
use crate::openrtb::{Eid, Uid};

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
    // Temporary: first used by a later identity task.
    #[allow(dead_code)]
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
    // Temporary: first used by a later identity task.
    #[allow(dead_code)]
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

    /// The ISO 3166-2 subdivision without the country prefix, for example
    /// `"CA"`. `None` when the platform does not know it.
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

/// The 32 raw bytes of a SHA-256 of a normalized email. Personal data.
#[derive(Clone, PartialEq, Eq)]
pub struct HashedEmail([u8; 32]);

impl fmt::Debug for HashedEmail {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("HashedEmail([redacted])")
    }
}

impl HashedEmail {
    /// From the `ts-hem` cookie value: 64 hex digits of the digest, in
    /// either case. `None` for anything else, so a malformed cookie is
    /// treated as absent.
    #[must_use]
    pub(crate) fn from_cookie_hex(value: &str) -> Option<Self> {
        let value = value.trim();
        if value.len() != 64 {
            return None;
        }
        let mut bytes = [0u8; 32];
        hex::decode_to_slice(value, &mut bytes).ok()?;
        Some(Self(bytes))
    }

    /// From digest bytes. Test support only: the one runtime source is the
    /// `ts-hem` cookie, read with [`Self::from_cookie_hex`].
    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// From the SHA-256 digest of a normalized address. Test support only,
    /// and the one constructor over [`Self::from_bytes`].
    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub fn for_test(bytes: [u8; 32]) -> Self {
        Self::from_bytes(bytes)
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

    /// Standard base64, as `UID2` takes it.
    #[must_use]
    pub fn to_base64(&self) -> String {
        STANDARD.encode(self.0)
    }
}

/// A stored record for one of this provider's sources, valid or expired.
///
/// It holds the source's whole entry set, which is never empty.
#[derive(Clone, PartialEq)]
pub struct StoredId {
    eids: Vec<Eid>,
    uid: String,
    writer: Option<IdentityProviderId>,
    acquired_at: Option<UnixTime>,
    expires_at: Option<UnixTime>,
    refresh_at: Option<UnixTime>,
}

impl fmt::Debug for StoredId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoredId")
            .field("entries", &self.eids.len())
            .field("uid", &"[redacted]")
            .field("writer", &self.writer)
            .field("acquired_at", &self.acquired_at)
            .field("expires_at", &self.expires_at)
            .field("refresh_at", &self.refresh_at)
            .finish_non_exhaustive()
    }
}

impl StoredId {
    /// From the KV record of `source`. A writer that does not parse is
    /// treated as none. `None` for a record with no entry, which
    /// `KvEntry::validate` never lets through.
    #[must_use]
    // Temporary: first used by a later identity task.
    #[allow(dead_code)]
    pub(crate) fn from_record(source: &SourceDomain, record: &KvPartnerId) -> Option<Self> {
        let uid = record.first_uid()?.to_owned();
        let eids = record
            .eids
            .iter()
            .map(|entry| Eid {
                source: source.as_str().to_owned(),
                uids: entry
                    .uids
                    .iter()
                    .map(|uid| Uid {
                        id: uid.id.clone(),
                        atype: uid.atype,
                        ext: uid.ext.clone(),
                    })
                    .collect(),
                inserter: entry.inserter.clone(),
                matcher: entry.matcher.clone(),
                mm: entry.mm,
                ext: entry.ext.clone(),
            })
            .collect();
        Some(Self {
            eids,
            uid,
            writer: record.w.as_deref().and_then(|writer| writer.parse().ok()),
            acquired_at: record.at.map(UnixTime::from_secs),
            expires_at: record.exp.map(UnixTime::from_secs),
            refresh_at: record.ra.map(UnixTime::from_secs),
        })
    }

    fn first_uid(&self) -> Option<&Uid> {
        self.eids.first().and_then(|eid| eid.uids.first())
    }

    /// A stored one-entry, one-UID record of `source`. Test support only.
    ///
    /// # Panics
    ///
    /// Panics when `uid` is not a valid stored UID.
    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub fn for_test(source: &SourceDomain, uid: &str) -> Self {
        Self::from_record(source, &KvPartnerId::new(uid))
            .expect("should build a stored ID from a record with a UID")
    }

    /// Replaces the entry set with a full one, and `uid()` becomes its first
    /// UID. Test support only, so a test can give a refreshing provider a
    /// stored full set.
    ///
    /// # Panics
    ///
    /// Panics when `eids` holds no UID.
    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub fn with_eids(mut self, eids: Vec<Eid>) -> Self {
        self.uid = eids
            .first()
            .and_then(|eid| eid.uids.first())
            .map(|uid| uid.id.clone())
            .expect("should give the stored set at least one UID");
        self.eids = eids;
        self
    }

    /// Records `writer` as the provider that wrote it. Test support only.
    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub fn written_by(mut self, writer: &IdentityProviderId) -> Self {
        self.writer = Some(writer.clone());
        self
    }

    /// Sets the time it must stop being sent. Test support only.
    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub fn expiring_at(mut self, at: UnixTime) -> Self {
        self.expires_at = Some(at);
        self
    }

    /// Every stored entry for the source, in stored order, with its UIDs and
    /// EID-level fields. A record that browser ingest, generic pull, batch
    /// sync or a version 1 row wrote is one entry with one UID.
    #[must_use]
    pub fn eids(&self) -> &[Eid] {
        &self.eids
    }

    /// The first UID of the first entry. What a single-UID refresh, such as
    /// `LiveRamp`'s, sends back.
    #[must_use]
    pub fn uid(&self) -> &str {
        &self.uid
    }

    /// The `atype` of that first UID.
    #[must_use]
    pub fn atype(&self) -> Option<i32> {
        self.first_uid().and_then(|uid| uid.atype)
    }

    /// The UID-level ext of that first UID, as bidders read it in
    /// `uids[].ext`.
    #[must_use]
    pub fn ext(&self) -> Option<&serde_json::Value> {
        self.first_uid().and_then(|uid| uid.ext.as_ref())
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
// Temporary: first used by a later identity task.
#[allow(dead_code)]
pub(crate) struct ThreadRandom;

impl RandomSource for ThreadRandom {
    fn fill(&self, bytes: &mut [u8]) {
        rand::thread_rng().fill_bytes(bytes);
    }
}

/// Deterministic bytes for tests: xorshift64* from a seed.
#[cfg(any(test, feature = "test-utils"))]
#[derive(Debug)]
pub struct SeededRandom(std::sync::Mutex<u64>);

#[cfg(any(test, feature = "test-utils"))]
impl SeededRandom {
    /// A generator that yields the same bytes for the same seed. A zero
    /// seed is treated as one, since xorshift never leaves zero.
    #[must_use]
    pub fn new(seed: u64) -> Self {
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
            *byte = state.wrapping_mul(0x2545_F491_4F6C_DD1D).to_be_bytes()[0];
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
    /// The request's first-party cookies other than Trusted Server's own and
    /// the consent cookies. A provider reads only the names it declared.
    pub(crate) cookies: BTreeMap<String, String>,
}

/// What one provider may read for one call.
pub struct ResolveInput<'a> {
    descriptor: &'a ProviderDescriptor,
    kinds: BTreeMap<SourceDomain, ResolveKind>,
    signals: &'a ResolveSignals<'a>,
    declared: BTreeSet<Input>,
    available: BTreeSet<Input>,
    state_json: Option<String>,
    undeclared: RefCell<Vec<Access>>,
    state_failed: RefCell<bool>,
    state_decoded: RefCell<bool>,
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
    /// The inputs of every kind present are available, plus the wanted ones.
    /// An input declared only for a kind this call does not hold is
    /// declared, so reading it is not an error of the provider, but it is
    /// unavailable (spec 4.4).
    #[must_use]
    pub(crate) fn new(
        descriptor: &'a ProviderDescriptor,
        kinds: BTreeMap<SourceDomain, ResolveKind>,
        signals: &'a ResolveSignals<'a>,
    ) -> Self {
        let (declared, available) =
            descriptor
                .resolves()
                .map_or_else(Default::default, |resolves| {
                    let mut available = resolves.wants().clone();
                    for kind in kinds.values().copied().collect::<BTreeSet<_>>() {
                        available.extend(resolves.needs(kind));
                    }
                    (resolves.declared_inputs(), available)
                });
        Self {
            descriptor,
            kinds,
            signals,
            declared,
            available,
            state_json: None,
            undeclared: RefCell::new(Vec::new()),
            state_failed: RefCell::new(false),
            state_decoded: RefCell::new(false),
        }
    }

    /// Gives the call its own provider's stored private state `ps.st`, as
    /// JSON. State is per provider and sensitive, so it never rides on the
    /// signals every call shares.
    #[must_use]
    pub(crate) fn with_state_json(mut self, state_json: Option<String>) -> Self {
        self.state_json = state_json;
        self
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
        if !self.declared.contains(&input) {
            return Err(self.record(Access::Input(input)));
        }
        if !self.available.contains(&input) {
            return Err(Report::new(IdentityProviderError::InputUnavailable {
                input,
            }));
        }
        value.ok_or_else(|| Report::new(IdentityProviderError::InputUnavailable { input }))
    }

    /// Undeclared reads recorded so far. The coordinator logs them as
    /// `Config`, and the harness fails the test.
    #[must_use]
    pub(crate) fn recorded_errors(&self) -> Vec<Access> {
        self.undeclared.borrow().clone()
    }

    /// Whether a `state()` call failed to decode.
    #[must_use]
    pub(crate) fn state_decode_failed(&self) -> bool {
        *self.state_failed.borrow()
    }

    /// Whether a `state()` call decoded the stored state. The conformance
    /// check of the test harness uses it to see a provider accept a state
    /// that no valid state looks like.
    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub(crate) fn state_was_decoded(&self) -> bool {
        *self.state_decoded.borrow()
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

    /// The descriptor's primary source.
    #[must_use]
    pub fn primary_source(&self) -> &SourceDomain {
        self.descriptor.primary_source()
    }

    /// The stored record for one of this provider's sources.
    #[must_use]
    pub fn stored(&self, source: &SourceDomain) -> Option<&StoredId> {
        self.signals.stored.get(source)
    }

    /// This provider's private state, decoded. `Ok(None)` when there is none.
    ///
    /// # Errors
    ///
    /// `InvalidState` when the stored state does not decode as `T`. The
    /// failure is recorded, and the coordinator clears the state whatever
    /// the provider returns. `UndeclaredInput` without `with_private_state()`.
    pub fn state<T: DeserializeOwned>(&self) -> Result<Option<T>, Report<IdentityProviderError>> {
        if !self
            .descriptor
            .resolves()
            .is_some_and(Resolves::has_private_state)
        {
            return Err(self.record(Access::PrivateState));
        }
        let Some(raw) = self.state_json.as_deref() else {
            return Ok(None);
        };
        serde_json::from_str(raw)
            .map(|state| {
                *self.state_decoded.borrow_mut() = true;
                Some(state)
            })
            .map_err(|_| {
                *self.state_failed.borrow_mut() = true;
                Report::new(IdentityProviderError::InvalidState)
            })
    }

    /// The endpoint declared with `Resolves::with_endpoint`.
    ///
    /// # Errors
    ///
    /// `UndeclaredInput { access: Endpoint }` when none was declared.
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

    /// Random bytes from the platform generator. Fixed by the harness.
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
    /// As [`Self::client_ip`].
    pub fn user_agent(&self) -> Result<&str, Report<IdentityProviderError>> {
        self.declared(Input::UserAgent, self.signals.user_agent)
    }

    /// The page URL, without query string or fragment.
    ///
    /// # Errors
    ///
    /// As [`Self::client_ip`].
    pub fn page_url(&self) -> Result<&Url, Report<IdentityProviderError>> {
        self.declared(Input::PageUrl, self.signals.page_url.as_ref())
    }

    /// Country and subdivision.
    ///
    /// # Errors
    ///
    /// As [`Self::client_ip`].
    pub fn region(&self) -> Result<&Region, Report<IdentityProviderError>> {
        self.declared(Input::Region, self.signals.region.as_ref())
    }

    /// The hashed email from the `ts-hem` cookie.
    ///
    /// # Errors
    ///
    /// As [`Self::client_ip`].
    pub fn hashed_email(&self) -> Result<&HashedEmail, Report<IdentityProviderError>> {
        self.declared(Input::HashedEmail, self.signals.hashed_email.as_ref())
    }

    /// The value of the first-party cookie `name`, or `None` when the request
    /// does not carry it.
    ///
    /// # Errors
    ///
    /// `UndeclaredInput { access: Cookie }` when the descriptor did not
    /// declare `name` with
    /// [`Resolves::with_cookie`](super::descriptor::Resolves::with_cookie).
    pub fn cookie(&self, name: &str) -> Result<Option<&str>, Report<IdentityProviderError>> {
        if !self
            .descriptor
            .resolves()
            .is_some_and(|resolves| resolves.cookies().contains(name))
        {
            return Err(self.record(Access::Cookie));
        }
        Ok(self.signals.cookies.get(name).map(String::as_str))
    }

    /// The full EC ID. Only for a resolver declared `with_ec_id()`.
    ///
    /// # Errors
    ///
    /// `UndeclaredInput { access: EcId }` otherwise. `Config` when the
    /// request has no EC ID, which the coordinator never allows: it does not
    /// dispatch generic pull without one.
    pub(in crate::ec::identity) fn ec_id(&self) -> Result<&str, Report<IdentityProviderError>> {
        if !self.descriptor.resolves().is_some_and(Resolves::gets_ec_id) {
            return Err(self.record(Access::EcId));
        }
        self.signals.ec_id.as_deref().ok_or_else(|| {
            IdentityProviderError::config("generic pull was called without an EC ID")
        })
    }
}

/// What a capture may read from one proxied response.
pub struct CaptureInput<'a> {
    descriptor: &'a ProviderDescriptor,
    path: &'a str,
    response: &'a ProviderResponse,
    stored: &'a BTreeMap<SourceDomain, StoredId>,
    consent: &'a ProviderConsent,
    publisher_domain: &'a str,
    now: UnixTime,
}

impl fmt::Debug for CaptureInput<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CaptureInput")
            .field("provider", self.descriptor.id())
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl<'a> CaptureInput<'a> {
    /// The input for one capture.
    #[must_use]
    pub(crate) fn new(
        descriptor: &'a ProviderDescriptor,
        path: &'a str,
        response: &'a ProviderResponse,
        stored: &'a BTreeMap<SourceDomain, StoredId>,
        consent: &'a ProviderConsent,
        publisher_domain: &'a str,
        now: UnixTime,
    ) -> Self {
        Self {
            descriptor,
            path,
            response,
            stored,
            consent,
            publisher_domain,
            now,
        }
    }

    /// The proxied request path, without query string.
    #[must_use]
    pub fn path(&self) -> &str {
        self.path
    }

    /// The proxied response, decoded.
    #[must_use]
    pub fn response(&self) -> &ProviderResponse {
        self.response
    }

    /// The descriptor's primary source.
    #[must_use]
    pub fn primary_source(&self) -> &SourceDomain {
        self.descriptor.primary_source()
    }

    /// Finds a declared source by a vendor's spelling.
    #[must_use]
    pub fn source_named(&self, name: &str) -> Option<&SourceDomain> {
        self.descriptor.source_named(name)
    }

    /// The stored record for a source.
    #[must_use]
    pub fn stored(&self, source: &SourceDomain) -> Option<&StoredId> {
        self.stored.get(source)
    }

    /// The request's consent, from its cookies and headers. The request
    /// body is never read (spec 4.7).
    #[must_use]
    pub fn consent(&self) -> &ProviderConsent {
        self.consent
    }

    /// The publisher's domain.
    #[must_use]
    pub fn publisher_domain(&self) -> &str {
        self.publisher_domain
    }

    /// The request's time.
    #[must_use]
    pub fn now(&self) -> UnixTime {
        self.now
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::net::IpAddr;

    use super::*;
    use crate::ec::identity::descriptor::{Captures, ProviderDescriptor, Resolves};
    use crate::ec::identity::exchange::ProviderResponse;
    use crate::ec::kv_types::{KvEid, KvPartnerId, KvUid};

    fn descriptor(resolves: Resolves) -> ProviderDescriptor {
        ProviderDescriptor::builder("p".parse().expect("should parse the provider id"))
            .with_source("example.com".parse().expect("should parse the source"))
            .with_resolves(resolves)
            .with_consent_policy(crate::ec::identity::test_support::resolver_policy())
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
            ec_id: Some("a".repeat(64) + ".ABC123"),
            stored: BTreeMap::new(),
            cookies: BTreeMap::from([("_vendor_fpid".to_owned(), "fpid-value".to_owned())]),
        }
    }

    #[test]
    fn declared_cookies_are_readable_and_undeclared_ones_are_recorded() {
        let descriptor = descriptor(
            Resolves::new()
                .with_endpoint("https://a.example.com")
                .with_cookie("_vendor_fpid")
                .with_cookie("_vendor_other"),
        );
        let signals = signals();
        let input = ResolveInput::new(
            &descriptor,
            kinds(&descriptor, ResolveKind::Initial),
            &signals,
        );

        assert_eq!(
            input
                .cookie("_vendor_fpid")
                .expect("should read a declared cookie"),
            Some("fpid-value"),
            "should return the cookie value"
        );
        assert_eq!(
            input
                .cookie("_vendor_other")
                .expect("should read a declared cookie"),
            None,
            "should return None for a declared cookie the request lacks"
        );
        let error = input
            .cookie("_undeclared")
            .expect_err("should refuse an undeclared cookie");
        assert!(
            matches!(
                error.current_context(),
                IdentityProviderError::UndeclaredInput {
                    access: Access::Cookie
                }
            ),
            "should report UndeclaredInput"
        );
        assert_eq!(
            input.recorded_errors(),
            [Access::Cookie],
            "should record the undeclared read"
        );
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
            input.client_ip().expect("should read the ip"),
            "192.0.2.10".parse::<IpAddr>().expect("should parse the ip"),
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
            "should report a declared but missing input as InputUnavailable"
        );
        assert!(
            input.recorded_errors().is_empty(),
            "should record nothing for declared reads"
        );
    }

    #[test]
    fn input_declared_only_for_an_absent_kind_is_unavailable() {
        let descriptor = descriptor(
            Resolves::new()
                .with_endpoint("https://a.example.com")
                .with_needs([Input::ClientIp])
                .with_needs_for(ResolveKind::Refresh, [Input::UserAgent]),
        );
        let signals = signals();

        let initial = ResolveInput::new(
            &descriptor,
            kinds(&descriptor, ResolveKind::Initial),
            &signals,
        );
        let refresh = ResolveInput::new(
            &descriptor,
            kinds(&descriptor, ResolveKind::Refresh),
            &signals,
        );

        let error = initial
            .user_agent()
            .expect_err("should hide a Refresh-only input on an Initial call");
        assert!(
            matches!(
                error.current_context(),
                IdentityProviderError::InputUnavailable {
                    input: Input::UserAgent
                }
            ),
            "should report InputUnavailable, not an undeclared read"
        );
        assert!(
            initial.recorded_errors().is_empty(),
            "should record no undeclared read"
        );
        assert_eq!(
            refresh
                .user_agent()
                .expect("should read the user agent on a Refresh call"),
            "ExampleBrowser/1.0",
            "should return the user agent for the kind that declares it"
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
        assert!(state.is_err(), "should need with_private_state for state");
        assert_eq!(
            input.recorded_errors(),
            vec![Access::Input(Input::UserAgent), Access::PrivateState],
            "should record both undeclared reads"
        );
        input
            .endpoint()
            .expect("should always return the declared endpoint");
    }

    #[test]
    fn state_decodes_and_a_bad_state_is_recorded() {
        let descriptor = descriptor(
            Resolves::new()
                .with_endpoint("https://a.example.com")
                .with_private_state(),
        );
        let good = signals();
        let input = ResolveInput::new(&descriptor, kinds(&descriptor, ResolveKind::Refresh), &good)
            .with_state_json(Some(r#"{"signature":"s"}"#.to_owned()));
        let state: serde_json::Value = input
            .state()
            .expect("should decode the state")
            .expect("should have a state");
        assert_eq!(state["signature"], "s", "should decode the state");
        assert!(
            !input.state_decode_failed(),
            "should not record a good state as a failure"
        );

        let bad = signals();
        let input = ResolveInput::new(&descriptor, kinds(&descriptor, ResolveKind::Refresh), &bad)
            .with_state_json(Some(r#""a bare string""#.to_owned()));
        let error = input
            .state::<TypedState>()
            .expect_err("should not decode a bare string");
        assert!(
            matches!(error.current_context(), IdentityProviderError::InvalidState),
            "should report InvalidState"
        );
        assert!(input.state_decode_failed(), "should record the failure");
    }

    #[test]
    fn ec_id_is_only_for_providers_declared_with_ec_id() {
        let generic = descriptor(Resolves::new().with_host("a.example.com").with_ec_id());
        let other = descriptor(Resolves::new().with_host("a.example.com"));
        let with_ec = signals();
        ResolveInput::new(&generic, kinds(&generic, ResolveKind::Initial), &with_ec)
            .ec_id()
            .expect("should let generic pull read the EC ID");
        let input = ResolveInput::new(&other, kinds(&other, ResolveKind::Initial), &with_ec);
        assert!(
            input.ec_id().is_err(),
            "should refuse the EC ID to other providers"
        );
        assert_eq!(
            input.recorded_errors(),
            vec![Access::EcId],
            "should record an EC ID read"
        );

        let mut no_ec = signals();
        no_ec.ec_id = None;
        let error = ResolveInput::new(&generic, kinds(&generic, ResolveKind::Initial), &no_ec)
            .ec_id()
            .expect_err("should fail without an EC ID");
        assert!(
            matches!(
                error.current_context(),
                IdentityProviderError::Config { .. }
            ),
            "should report a missing EC ID as Config, not as a missing input"
        );
    }

    #[test]
    fn stored_ids_and_expiry() {
        let record = KvPartnerId {
            w: Some("p".to_owned()),
            rv: Some("0123456789abcdef".to_owned()),
            at: Some(10),
            exp: Some(20),
            ra: Some(15),
            ru: None,
            eids: vec![KvEid {
                uids: vec![KvUid {
                    id: "u".to_owned(),
                    atype: Some(3),
                    ext: None,
                }],
                inserter: None,
                matcher: None,
                mm: None,
                ext: None,
            }],
        };
        let source: SourceDomain = "example.com".parse().expect("should parse the source");
        let stored =
            StoredId::from_record(&source, &record).expect("should read a record with an entry");
        assert_eq!(
            stored.writer().map(IdentityProviderId::as_str),
            Some("p"),
            "should parse the writer"
        );
        assert!(
            !stored.is_expired(UnixTime::from_secs(19)),
            "should not be expired before exp"
        );
        assert!(
            stored.is_expired(UnixTime::from_secs(20)),
            "should be expired at exp"
        );
        assert_eq!(
            stored.refresh_at(),
            Some(UnixTime::from_secs(15)),
            "should read ra"
        );
        assert_eq!(
            stored.eids().len(),
            1,
            "should expose a one-UID record as one entry"
        );
        let eid = &stored.eids()[0];
        assert_eq!(eid.source, "example.com", "should fill in the source");
        assert_eq!(eid.uids.len(), 1, "should expose its one UID");
        assert_eq!(eid.uids[0].id, "u", "should keep the UID");
        assert_eq!(eid.uids[0].atype, Some(3), "should keep the atype");
        assert_eq!(
            (
                eid.inserter.as_deref(),
                eid.matcher.as_deref(),
                eid.mm,
                eid.ext.as_ref()
            ),
            (None, None, None, None),
            "should expose no EID-level fields when none are stored"
        );
        assert_eq!(
            (stored.uid(), stored.atype()),
            ("u", Some(3)),
            "should describe the first UID"
        );

        let empty = KvPartnerId {
            eids: Vec::new(),
            ..record
        };
        assert!(
            StoredId::from_record(&source, &empty).is_none(),
            "should not read a record with no entry"
        );
    }

    #[test]
    fn a_full_record_exposes_every_entry_and_its_first_uid() {
        let record = KvPartnerId {
            w: Some("p".to_owned()),
            rv: Some("0123456789abcdef".to_owned()),
            at: Some(10),
            exp: None,
            ra: None,
            ru: None,
            eids: vec![
                KvEid {
                    uids: vec![
                        KvUid {
                            id: "first".to_owned(),
                            atype: Some(1),
                            ext: Some(serde_json::json!({ "rtiPartner": "TDID" })),
                        },
                        KvUid {
                            id: "second".to_owned(),
                            atype: Some(3),
                            ext: None,
                        },
                    ],
                    inserter: Some("inserter.example.com".to_owned()),
                    matcher: None,
                    mm: Some(2),
                    ext: Some(serde_json::json!({ "segments": ["a"] })),
                },
                KvEid {
                    uids: vec![KvUid {
                        id: "third".to_owned(),
                        atype: None,
                        ext: None,
                    }],
                    inserter: None,
                    matcher: Some("matcher.example.com".to_owned()),
                    mm: None,
                    ext: None,
                },
            ],
        };
        let source: SourceDomain = "example.com".parse().expect("should parse the source");

        let stored = StoredId::from_record(&source, &record).expect("should read the record");

        let ids: Vec<Vec<&str>> = stored
            .eids()
            .iter()
            .map(|eid| eid.uids.iter().map(|uid| uid.id.as_str()).collect())
            .collect();
        assert_eq!(
            ids,
            vec![vec!["first", "second"], vec!["third"]],
            "should keep every entry in stored order"
        );
        assert!(
            stored.eids().iter().all(|eid| eid.source == "example.com"),
            "should fill in the source"
        );
        assert_eq!(
            stored.eids()[0].inserter.as_deref(),
            Some("inserter.example.com"),
            "should keep the inserter"
        );
        assert_eq!(stored.eids()[0].mm, Some(2), "should keep mm");
        assert_eq!(
            stored.eids()[0].ext,
            Some(serde_json::json!({ "segments": ["a"] })),
            "should keep the EID-level ext"
        );
        assert_eq!(
            stored.eids()[1].matcher.as_deref(),
            Some("matcher.example.com"),
            "should keep the matcher"
        );
        assert_eq!(
            stored.uid(),
            "first",
            "should return the first UID of the first entry"
        );
        assert_eq!(stored.atype(), Some(1), "should return that UID's atype");
        assert_eq!(
            stored.ext(),
            Some(&serde_json::json!({ "rtiPartner": "TDID" })),
            "should return that UID's ext"
        );
        assert!(
            !format!("{stored:?}").contains("second"),
            "should redact every UID in Debug"
        );
    }

    #[test]
    fn hashed_email_debug_is_redacted() {
        let email = HashedEmail::from_bytes([0xab; 32]);
        assert_eq!(
            email.to_hex(),
            "ab".repeat(32),
            "should encode lowercase hex"
        );
        assert_eq!(
            format!("{email:?}"),
            "HashedEmail([redacted])",
            "should redact Debug"
        );
    }

    #[test]
    fn hashed_email_parses_the_cookie_hex_in_either_case() {
        let lower = HashedEmail::from_cookie_hex(&"ab".repeat(32)).expect("should parse lowercase");
        let upper = HashedEmail::from_cookie_hex(&"AB".repeat(32)).expect("should parse uppercase");

        assert_eq!(lower, upper, "should read both cases as one digest");
        assert_eq!(lower.as_bytes(), &[0xab; 32], "should decode the digest");
    }

    #[test]
    fn hashed_email_rejects_a_cookie_that_is_not_a_digest() {
        for value in [
            "",
            "ab",
            &"ab".repeat(31),
            &"ab".repeat(33),
            &"zz".repeat(32),
        ] {
            assert!(
                HashedEmail::from_cookie_hex(value).is_none(),
                "should reject `{value}`"
            );
        }
    }

    #[test]
    fn capture_input_exposes_path_and_never_the_request_body() {
        let descriptor =
            ProviderDescriptor::builder("c".parse().expect("should parse the provider id"))
                .with_source("example.com".parse().expect("should parse the source"))
                .with_captures(Captures::new().with_path(http::Method::POST, "/integrations/c/api"))
                .build()
                .expect("should build");
        let response = ProviderResponse::for_test(http::StatusCode::OK, b"{}");
        let consent = ProviderConsent::default();
        let stored = BTreeMap::new();
        let input = CaptureInput::new(
            &descriptor,
            "/integrations/c/api",
            &response,
            &stored,
            &consent,
            "www.example.com",
            UnixTime::from_secs(1),
        );
        assert_eq!(
            input.path(),
            "/integrations/c/api",
            "should expose the proxied path"
        );
        assert_eq!(
            input.primary_source().as_str(),
            "example.com",
            "should expose the primary source"
        );
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

    #[derive(Debug, serde::Deserialize)]
    struct TypedState {
        #[allow(dead_code)]
        signature: String,
    }
}
