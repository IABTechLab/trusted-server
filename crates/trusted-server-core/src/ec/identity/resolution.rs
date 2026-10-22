//! Provider results and errors.

use std::collections::BTreeSet;
use std::time::Duration;

use error_stack::Report;
use http::StatusCode;
use serde::Serialize;

use super::descriptor::{Access, Input};
use super::ids::{SourceDomain, UnixTime};
use crate::ec::kv_types::{MAX_PARTNER_EXT_BYTES, MAX_PROVIDER_STATE_BYTES};

/// Marker message of [`IdentityProviderError::not_implemented`]. The harness
/// matches on it to tell a missing override from a real failure.
pub(crate) const NOT_IMPLEMENTED_MARKER: &str = "capability declared but not implemented: ";

/// A provider failure. Built through the constructors below.
#[derive(Debug, derive_more::Display)]
#[non_exhaustive]
pub enum IdentityProviderError {
    /// Configuration or provider code makes the call impossible.
    #[display("identity provider configuration error: {message}")]
    Config {
        /// What is wrong. Never contains a secret, UID or URL query.
        message: String,
    },
    /// The provider read something its descriptor did not declare.
    #[display("`{access}` was read but not declared")]
    UndeclaredInput {
        /// What was read.
        access: Access,
    },
    /// A declared input is not available for this request.
    #[display("input `{input}` is unavailable for this request")]
    InputUnavailable {
        /// The missing input.
        input: Input,
    },
    /// The provider refused the request.
    #[display("provider rejected the request with status {status}")]
    Rejected {
        /// The response status.
        status: StatusCode,
    },
    /// The provider's response could not be parsed.
    #[display("provider response could not be parsed")]
    Malformed,
    /// The stored private state does not decode as the provider's type.
    #[display("stored provider state could not be decoded")]
    InvalidState,
    /// Set by the coordinator only. Providers never see transport failures.
    #[display("provider request failed in transport")]
    Transport,
}

impl core::error::Error for IdentityProviderError {}

impl IdentityProviderError {
    /// A [`Self::Config`] error.
    pub fn config(message: impl Into<String>) -> Report<Self> {
        Report::new(Self::Config {
            message: message.into(),
        })
    }

    /// A [`Self::Rejected`] error.
    pub fn rejected(status: StatusCode) -> Report<Self> {
        Report::new(Self::Rejected { status })
    }

    /// A [`Self::Malformed`] error.
    pub fn malformed() -> Report<Self> {
        Report::new(Self::Malformed)
    }

    /// The default body of a capability method: a `Config` error with a
    /// fixed marker message, so the harness can tell a missing override from
    /// a real failure.
    pub fn not_implemented(method: &'static str) -> Report<Self> {
        Self::config(format!("{NOT_IMPLEMENTED_MARKER}{method}"))
    }

    /// Whether this is the [`Self::not_implemented`] marker.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn is_not_implemented(&self) -> bool {
        matches!(self, Self::Config { message } if message.starts_with(NOT_IMPLEMENTED_MARKER))
    }
}

/// A time as a provider gave it: relative to `now`, or absolute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    /// Relative to the request's `now`.
    In(Duration),
    /// Absolute.
    At(UnixTime),
}

impl When {
    /// Resolves against `now`.
    #[must_use]
    pub(crate) fn resolve(self, now: UnixTime) -> UnixTime {
        match self {
            Self::In(delay) => now + delay,
            Self::At(at) => at,
        }
    }
}

/// One ID a provider returned.
#[derive(Clone, PartialEq)]
pub struct ResolvedId {
    source: SourceDomain,
    uid: String,
    atype: Option<i32>,
    ext: Option<serde_json::Value>,
    expiry: Option<When>,
    refresh: Option<When>,
    refresh_until: Option<UnixTime>,
}

impl core::fmt::Debug for ResolvedId {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("ResolvedId")
            .field("source", &self.source)
            .field("uid", &"[redacted]")
            .field("expiry", &self.expiry)
            .field("refresh", &self.refresh)
            .finish_non_exhaustive()
    }
}

impl ResolvedId {
    /// An ID for `source`.
    #[must_use]
    pub fn new(source: &SourceDomain, uid: impl Into<String>) -> Self {
        Self {
            source: source.clone(),
            uid: uid.into(),
            atype: None,
            ext: None,
            expiry: None,
            refresh: None,
            refresh_until: None,
        }
    }

    /// Sets `atype`.
    #[must_use]
    pub fn with_atype(mut self, atype: i32) -> Self {
        self.atype = Some(atype);
        self
    }

    /// UID-level ext. Over 256 serialized bytes it is dropped and the ID kept.
    #[must_use]
    pub fn with_ext(mut self, ext: serde_json::Value) -> Self {
        let fits = serde_json::to_vec(&ext).is_ok_and(|bytes| bytes.len() <= MAX_PARTNER_EXT_BYTES);
        if fits {
            self.ext = Some(ext);
        } else {
            log::debug!("Dropping oversized ext for source {}", self.source);
            self.ext = None;
        }
        self
    }

    /// Expires after `delay`.
    #[must_use]
    pub fn with_expires_in(mut self, delay: Duration) -> Self {
        self.expiry = Some(When::In(delay));
        self
    }

    /// Expires at `at`.
    #[must_use]
    pub fn with_expires_at(mut self, at: UnixTime) -> Self {
        self.expiry = Some(When::At(at));
        self
    }

    /// Due for refresh after `delay`.
    #[must_use]
    pub fn with_refresh_in(mut self, delay: Duration) -> Self {
        self.refresh = Some(When::In(delay));
        self
    }

    /// Due for refresh at `at`.
    #[must_use]
    pub fn with_refresh_at(mut self, at: UnixTime) -> Self {
        self.refresh = Some(When::At(at));
        self
    }

    /// When this ID stops being refreshable, such as a refresh token's
    /// expiry. Only for a provider declared `with_private_state()`; from any
    /// other it is ignored with a warning. Until then an expired record is
    /// kept as a refresh anchor.
    #[must_use]
    pub fn with_refresh_until(mut self, at: UnixTime) -> Self {
        self.refresh_until = Some(at);
        self
    }

    /// The source.
    #[must_use]
    pub fn source(&self) -> &SourceDomain {
        &self.source
    }

    /// The UID.
    #[must_use]
    pub fn uid(&self) -> &str {
        &self.uid
    }

    /// `atype`.
    #[must_use]
    pub fn atype(&self) -> Option<i32> {
        self.atype
    }

    /// UID-level ext.
    #[must_use]
    pub fn ext(&self) -> Option<&serde_json::Value> {
        self.ext.as_ref()
    }

    /// The expiry as given.
    #[must_use]
    pub fn expiry(&self) -> Option<When> {
        self.expiry
    }

    /// The refresh hint as given.
    #[must_use]
    pub fn refresh(&self) -> Option<When> {
        self.refresh
    }

    /// The refresh limit.
    #[must_use]
    pub fn refresh_until(&self) -> Option<UnixTime> {
        self.refresh_until
    }
}

/// How a result changes the provider's private state.
#[derive(Debug, PartialEq, Eq)]
pub enum StateChange<'a> {
    /// Leave it.
    Keep,
    /// Replace it with this JSON.
    Replace(&'a str),
    /// Remove it.
    Clear,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum StateOp {
    Keep,
    Replace(String),
    Clear,
    Rejected,
}

/// What one provider call produced.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolution {
    ids: Vec<ResolvedId>,
    invalidated: Vec<SourceDomain>,
    opted_out: bool,
    retry_in: Option<Duration>,
    state: StateOp,
}

impl Resolution {
    fn empty() -> Self {
        Self {
            ids: Vec::new(),
            invalidated: Vec::new(),
            opted_out: false,
            retry_in: None,
            state: StateOp::Keep,
        }
    }

    /// One or more IDs, at most one per source: KV holds one UID per
    /// source. The coordinator keeps the first ID for a source and discards
    /// the rest. An empty set is `no_match`.
    #[must_use]
    pub fn found(ids: impl IntoIterator<Item = ResolvedId>) -> Self {
        Self {
            ids: ids.into_iter().collect(),
            ..Self::empty()
        }
    }

    /// The provider recognized no one, or a capture held nothing.
    #[must_use]
    pub fn no_match() -> Self {
        Self::empty()
    }

    /// The stored ID for `source` is no longer valid.
    #[must_use]
    pub fn invalidate(source: &SourceDomain) -> Self {
        Self {
            invalidated: vec![source.clone()],
            ..Self::empty()
        }
    }

    /// The provider reports the user opted out.
    #[must_use]
    pub fn opted_out() -> Self {
        Self {
            opted_out: true,
            ..Self::empty()
        }
    }

    /// Also invalidates `source`. Chainable after `found`, so one response
    /// can return some sources and drop others.
    #[must_use]
    pub fn also_invalidate(mut self, source: &SourceDomain) -> Self {
        if !self.invalidated.contains(source) {
            self.invalidated.push(source.clone());
        }
        self
    }

    /// The earliest the coordinator may call again. Default: 24 hours after
    /// `no_match`, none otherwise.
    #[must_use]
    pub fn with_retry_in(mut self, delay: Duration) -> Self {
        self.retry_in = Some(delay);
        self
    }

    /// Replaces the provider's private state.
    ///
    /// The state is stored as embedded JSON of at most 1,024 bytes. A larger
    /// or unserializable value rejects the positive part of the result whole:
    /// no ID, state or scheduling change is stored, and the old state stays.
    /// The result's removals still apply.
    #[must_use]
    pub fn replacing_state(mut self, state: &impl Serialize) -> Self {
        self.state = match serde_json::to_string(state) {
            Ok(json) if json.len() <= MAX_PROVIDER_STATE_BYTES => StateOp::Replace(json),
            _ => StateOp::Rejected,
        };
        self
    }

    /// Clears the provider's private state.
    #[must_use]
    pub fn clearing_state(mut self) -> Self {
        self.state = StateOp::Clear;
        self
    }

    /// The returned IDs, as given.
    #[must_use]
    pub fn ids(&self) -> &[ResolvedId] {
        &self.ids
    }

    /// Invalidated sources.
    #[must_use]
    pub fn invalidated(&self) -> &[SourceDomain] {
        &self.invalidated
    }

    /// Whether nothing was found, invalidated or opted out.
    #[must_use]
    pub fn is_no_match(&self) -> bool {
        self.ids.is_empty() && self.invalidated.is_empty() && !self.opted_out
    }

    /// Whether the provider reported an opt-out.
    #[must_use]
    pub fn is_opted_out(&self) -> bool {
        self.opted_out
    }

    /// The retry delay.
    #[must_use]
    pub fn retry_in(&self) -> Option<Duration> {
        self.retry_in
    }

    /// The state change. A rejected replacement reports `Keep`.
    #[must_use]
    pub fn state_change(&self) -> StateChange<'_> {
        match &self.state {
            StateOp::Keep | StateOp::Rejected => StateChange::Keep,
            StateOp::Replace(json) => StateChange::Replace(json),
            StateOp::Clear => StateChange::Clear,
        }
    }

    /// Whether `replacing_state` was given an oversized or unserializable
    /// value. The coordinator then rejects the positive part.
    #[must_use]
    pub(crate) fn state_rejected(&self) -> bool {
        self.state == StateOp::Rejected
    }

    /// Whether this result asks for any state change.
    #[must_use]
    pub(crate) fn changes_state(&self) -> bool {
        self.state != StateOp::Keep
    }

    /// The first ID per source, in order, and how many duplicates were
    /// dropped.
    #[must_use]
    pub(crate) fn found_ids_by_source(&self) -> (Vec<&ResolvedId>, usize) {
        let mut seen = BTreeSet::new();
        let mut kept = Vec::new();
        let mut dropped = 0;
        for id in &self.ids {
            if seen.insert(id.source()) {
                kept.push(id);
            } else {
                dropped += 1;
            }
        }
        (kept, dropped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_implemented_marker_is_recognised() {
        let marker = IdentityProviderError::not_implemented("capture");
        assert!(
            marker.current_context().is_not_implemented(),
            "should recognise the marker"
        );
        let other = IdentityProviderError::config("bad partner id");
        assert!(
            !other.current_context().is_not_implemented(),
            "an ordinary Config error is not the marker"
        );
    }
}

#[cfg(test)]
mod resolution_tests {
    use std::time::Duration;

    use serde_json::json;

    use super::*;

    fn source(name: &str) -> SourceDomain {
        name.parse().expect("should parse")
    }

    #[test]
    fn empty_found_is_no_match() {
        let resolution = Resolution::found([]);
        assert!(resolution.is_no_match(), "should be no_match");
        assert!(resolution.ids().is_empty(), "no ids");
    }

    #[test]
    fn found_keeps_the_first_id_per_source() {
        let a = source("a.example.com");
        let resolution =
            Resolution::found([ResolvedId::new(&a, "first"), ResolvedId::new(&a, "second")]);
        let (kept, dropped) = resolution.found_ids_by_source();
        assert_eq!(kept.len(), 1, "one per source");
        assert_eq!(kept[0].uid(), "first", "should keep the first");
        assert_eq!(dropped, 1, "should count the duplicate");
    }

    #[test]
    fn invalidations_chain_and_state_changes_are_reported() {
        let a = source("a.example.com");
        let b = source("b.example.com");
        let resolution = Resolution::found([ResolvedId::new(&a, "u")])
            .also_invalidate(&b)
            .with_retry_in(Duration::from_secs(60))
            .replacing_state(&json!({ "signature": "s" }));
        assert_eq!(resolution.invalidated(), &[b], "should invalidate b");
        assert_eq!(
            resolution.retry_in(),
            Some(Duration::from_secs(60)),
            "should keep the retry"
        );
        assert_eq!(
            resolution.state_change(),
            StateChange::Replace(r#"{"signature":"s"}"#),
            "should replace the state"
        );
        assert_eq!(
            Resolution::no_match().clearing_state().state_change(),
            StateChange::Clear,
            "should clear"
        );
        assert_eq!(
            Resolution::no_match().state_change(),
            StateChange::Keep,
            "should keep by default"
        );
        assert!(
            !Resolution::invalidate(&source("c.example.com")).is_no_match(),
            "an invalidation is not no_match"
        );
    }

    #[test]
    fn oversized_state_is_flagged_not_dropped_silently() {
        let resolution = Resolution::found([ResolvedId::new(&source("a.example.com"), "u")])
            .replacing_state(&"x".repeat(2000));
        assert!(
            resolution.state_rejected(),
            "should flag the state as rejected"
        );
        assert_eq!(
            resolution.state_change(),
            StateChange::Keep,
            "a rejected state keeps the old one"
        );
    }

    #[test]
    fn oversized_ext_is_dropped_and_the_id_kept() {
        let id = ResolvedId::new(&source("a.example.com"), "u")
            .with_ext(json!({ "pad": "x".repeat(300) }));
        assert_eq!(id.uid(), "u", "should keep the id");
        assert!(id.ext().is_none(), "should drop the ext");
        let small =
            ResolvedId::new(&source("a.example.com"), "u").with_ext(json!({ "linkType": 2 }));
        assert_eq!(
            small.ext(),
            Some(&json!({ "linkType": 2 })),
            "control: a small ext is kept"
        );
    }

    #[test]
    fn times_resolve_against_now() {
        let id = ResolvedId::new(&source("a.example.com"), "u")
            .with_expires_in(Duration::from_secs(10))
            .with_refresh_at(UnixTime::from_secs(5));
        let now = UnixTime::from_secs(100);
        assert_eq!(
            id.expiry().map(|when| when.resolve(now)),
            Some(UnixTime::from_secs(110)),
            "relative expiry"
        );
        assert_eq!(
            id.refresh().map(|when| when.resolve(now)),
            Some(UnixTime::from_secs(5)),
            "absolute refresh"
        );
    }

    #[test]
    fn opted_out_is_reported() {
        assert!(Resolution::opted_out().is_opted_out(), "opted out");
        assert!(!Resolution::no_match().is_opted_out(), "control");
    }
}
