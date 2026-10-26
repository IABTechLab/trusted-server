//! Provider results and errors.

// Temporary: the coordinator, the first non-test caller, arrives in a later
// task. Remove this when it lands.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::iter;
use std::time::Duration;

use error_stack::Report;
use http::StatusCode;
use serde::Serialize;

use super::descriptor::{Access, Input};
use super::ids::{SourceDomain, UnixTime};
use crate::ec::kv_types::{
    KvEid, KvUid, MAX_ENTRIES_PER_SOURCE, MAX_ENTRY_SET_BYTES, MAX_PARTNER_EXT_BYTES,
    MAX_PROVIDER_STATE_BYTES, MAX_UID_LENGTH, MAX_UIDS_PER_SOURCE, entry_set_bytes,
    is_valid_eid_field, is_valid_partner_ext,
};

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
    // Temporary: first used by the provider test harness in a later task.
    #[allow(dead_code)]
    #[must_use]
    pub(crate) fn is_not_implemented(&self) -> bool {
        matches!(self, Self::Config { message } if message.starts_with(NOT_IMPLEMENTED_MARKER))
    }
}

/// A time as a provider gave it.
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
    // Temporary: the coordinator, the first non-test caller, arrives in a later task.
    #[allow(dead_code)]
    pub(crate) fn resolve(self, now: UnixTime) -> UnixTime {
        match self {
            Self::In(delay) => now + delay,
            Self::At(at) => at,
        }
    }
}

fn ext_fits(ext: &serde_json::Value) -> bool {
    serde_json::to_vec(ext).is_ok_and(|bytes| bytes.len() <= MAX_PARTNER_EXT_BYTES)
}

/// One UID within a [`ResolvedEid`].
#[derive(Clone, PartialEq)]
pub struct ResolvedUid {
    id: String,
    atype: Option<i32>,
    ext: Option<serde_json::Value>,
}

impl core::fmt::Debug for ResolvedUid {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("ResolvedUid")
            .field("id", &"[redacted]")
            .field("atype", &self.atype)
            .finish_non_exhaustive()
    }
}

impl ResolvedUid {
    /// A UID with no `atype` or ext.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            atype: None,
            ext: None,
        }
    }

    /// Sets `atype`.
    #[must_use]
    pub fn with_atype(mut self, atype: i32) -> Self {
        self.atype = Some(atype);
        self
    }

    /// UID-level ext. Over 2,048 bytes as compact JSON it is dropped and the
    /// UID kept.
    #[must_use]
    pub fn with_ext(mut self, ext: serde_json::Value) -> Self {
        let fits = ext_fits(&ext);
        if !fits {
            log::debug!("Dropping an oversized UID-level ext");
        }
        self.ext = fits.then_some(ext);
        self
    }

    /// The UID.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
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
}

/// One `OpenRTB` EID entry a provider returns: a source, its UIDs and its
/// EID-level fields.
#[derive(Clone, PartialEq)]
pub struct ResolvedEid {
    source: SourceDomain,
    uids: Vec<ResolvedUid>,
    inserter: Option<String>,
    matcher: Option<String>,
    mm: Option<i32>,
    ext: Option<serde_json::Value>,
    expiry: Option<When>,
    refresh: Option<When>,
    refresh_until: Option<UnixTime>,
}

impl core::fmt::Debug for ResolvedEid {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("ResolvedEid")
            .field("source", &self.source)
            .field("uids", &self.uids)
            .field("mm", &self.mm)
            .field("expiry", &self.expiry)
            .finish_non_exhaustive()
    }
}

impl ResolvedEid {
    /// An entry with one UID, the common case.
    #[must_use]
    pub fn new(source: &SourceDomain, uid: ResolvedUid) -> Self {
        Self {
            source: source.clone(),
            uids: vec![uid],
            inserter: None,
            matcher: None,
            mm: None,
            ext: None,
            expiry: None,
            refresh: None,
            refresh_until: None,
        }
    }

    /// Adds another UID to this entry. Repeatable.
    #[must_use]
    pub fn with_uid(mut self, uid: ResolvedUid) -> Self {
        self.uids.push(uid);
        self
    }

    /// Sets `inserter`. An entry that sets any of `inserter`, `matcher` and
    /// `mm` states its EID-level fields completely, so the descriptor's
    /// `with_provenance` default no longer applies to it (spec 4.3).
    #[must_use]
    pub fn with_inserter(mut self, inserter: &str) -> Self {
        self.inserter = Some(inserter.to_owned());
        self
    }

    /// Sets `matcher`. As [`Self::with_inserter`], it turns off the
    /// descriptor's default for this entry.
    #[must_use]
    pub fn with_matcher(mut self, matcher: &str) -> Self {
        self.matcher = Some(matcher.to_owned());
        self
    }

    /// Sets `mm`. As [`Self::with_inserter`], it turns off the descriptor's
    /// default for this entry.
    #[must_use]
    pub fn with_mm(mut self, mm: i32) -> Self {
        self.mm = Some(mm);
        self
    }

    /// EID-level ext. Over 2,048 bytes as compact JSON it is dropped and the
    /// entry kept.
    #[must_use]
    pub fn with_ext(mut self, ext: serde_json::Value) -> Self {
        let fits = ext_fits(&ext);
        if !fits {
            log::debug!(
                "Dropping an oversized EID-level ext for source {}",
                self.source
            );
        }
        self.ext = fits.then_some(ext);
        self
    }

    /// Expires after `delay`. Timing applies to the source's whole entry
    /// set, and the earliest time across its entries wins.
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

    /// When this ID stops being refreshable. Only for a provider declared
    /// `with_private_state()`; the coordinator ignores it otherwise.
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

    /// The UIDs, in order.
    #[must_use]
    pub fn uids(&self) -> &[ResolvedUid] {
        &self.uids
    }

    /// `inserter`, if set.
    #[must_use]
    pub fn inserter(&self) -> Option<&str> {
        self.inserter.as_deref()
    }

    /// `matcher`, if set.
    #[must_use]
    pub fn matcher(&self) -> Option<&str> {
        self.matcher.as_deref()
    }

    /// `mm`, if set.
    #[must_use]
    pub fn mm(&self) -> Option<i32> {
        self.mm
    }

    /// EID-level ext.
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

#[derive(Clone, PartialEq)]
enum StateOp {
    Keep,
    Replace(String),
    Clear,
    Rejected,
}

impl core::fmt::Debug for StateOp {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Keep => formatter.write_str("Keep"),
            Self::Replace(_) => formatter.write_str("Replace([redacted])"),
            Self::Clear => formatter.write_str("Clear"),
            Self::Rejected => formatter.write_str("Rejected"),
        }
    }
}

/// What one provider call produced. Its `Debug` prints entries through
/// [`ResolvedEid`]'s redacting `Debug` and never prints provider state.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolution {
    eids: Vec<ResolvedEid>,
    invalidated: Vec<SourceDomain>,
    opted_out: bool,
    retry_in: Option<Duration>,
    state: StateOp,
}

impl Resolution {
    fn empty() -> Self {
        Self {
            eids: Vec::new(),
            invalidated: Vec::new(),
            opted_out: false,
            retry_in: None,
            state: StateOp::Keep,
        }
    }

    /// One or more EID entries. A source may appear in several entries, in
    /// the order bidders should see them. The entries of one source form its
    /// new entry set, which replaces the stored set whole for an owned
    /// source. For an unowned source the set only fills an absent or expired
    /// record (spec 7.2). A source the result does not name, or whose set
    /// the limits discard, is left as it is. An empty set is `no_match`.
    #[must_use]
    pub fn found(eids: impl IntoIterator<Item = ResolvedEid>) -> Self {
        Self {
            eids: eids.into_iter().collect(),
            ..Self::empty()
        }
    }

    /// Nothing recognized, or a capture held nothing.
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

    /// Also invalidates `source`.
    #[must_use]
    pub fn also_invalidate(mut self, source: &SourceDomain) -> Self {
        if !self.invalidated.contains(source) {
            self.invalidated.push(source.clone());
        }
        self
    }

    /// The earliest the coordinator may call again.
    #[must_use]
    pub fn with_retry_in(mut self, delay: Duration) -> Self {
        self.retry_in = Some(delay);
        self
    }

    /// Replaces the provider's private state. Over 1,024 bytes of JSON, or
    /// not serializable, the positive part of the result is rejected whole.
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

    /// The returned entries, as given.
    #[must_use]
    pub fn eids(&self) -> &[ResolvedEid] {
        &self.eids
    }

    /// Invalidated sources.
    #[must_use]
    pub fn invalidated(&self) -> &[SourceDomain] {
        &self.invalidated
    }

    /// Whether nothing was found, invalidated or opted out.
    #[must_use]
    pub fn is_no_match(&self) -> bool {
        self.eids.is_empty() && self.invalidated.is_empty() && !self.opted_out
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

    /// The state change.
    #[must_use]
    pub fn state_change(&self) -> StateChange<'_> {
        match &self.state {
            StateOp::Keep | StateOp::Rejected => StateChange::Keep,
            StateOp::Replace(json) => StateChange::Replace(json),
            StateOp::Clear => StateChange::Clear,
        }
    }

    /// Whether `replacing_state` was given an oversized or unserializable
    /// value. The coordinator then rejects the positive part (spec 4.5, 8).
    #[must_use]
    pub(crate) fn state_rejected(&self) -> bool {
        self.state == StateOp::Rejected
    }

    /// Whether this result changes state at all.
    #[must_use]
    pub(crate) fn changes_state(&self) -> bool {
        self.state != StateOp::Keep
    }
}

/// The part of a result that a spec 4.5 limit dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, derive_more::Display)]
pub(crate) enum DroppedPart {
    /// A UID that is empty, blank or over `MAX_UID_LENGTH`.
    #[display("uid")]
    Uid,
    /// A negative `atype`.
    #[display("atype")]
    Atype,
    /// A UID-level ext that is not an object or is oversized.
    #[display("uid ext")]
    UidExt,
    /// A blank or oversized `inserter`.
    #[display("inserter")]
    Inserter,
    /// A blank or oversized `matcher`.
    #[display("matcher")]
    Matcher,
    /// A negative `mm`.
    #[display("mm")]
    Mm,
    /// An EID-level ext that is not an object or is oversized.
    #[display("eid ext")]
    EidExt,
    /// An entry left with no UID.
    #[display("entry without uid")]
    EmptyEntry,
    /// An entry equal to an earlier kept entry of its source.
    #[display("duplicate entry")]
    DuplicateEntry,
    /// An entry past the source's first 8, or with no room left for a UID.
    #[display("entry over the limit")]
    ExtraEntry,
    /// A UID past the source's first 32.
    #[display("uid over the limit")]
    ExtraUid,
}

/// One dropped part, for the coordinator's log line (provider, source and
/// part only, never a value).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EntryDrop {
    pub(crate) source: SourceDomain,
    pub(crate) part: DroppedPart,
}

/// One source's new entry set, as it is stored, and its timing resolved
/// against `now`.
#[derive(Clone, PartialEq)]
pub(crate) struct EntrySet {
    pub(crate) entries: Vec<KvEid>,
    pub(crate) expires_at: Option<UnixTime>,
    pub(crate) refresh_at: Option<UnixTime>,
    pub(crate) refresh_until: Option<UnixTime>,
}

impl core::fmt::Debug for EntrySet {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("EntrySet")
            .field("entries", &self.entries.len())
            .field("expires_at", &self.expires_at)
            .field("refresh_at", &self.refresh_at)
            .field("refresh_until", &self.refresh_until)
            .finish()
    }
}

/// A result's entries, grouped by source and checked against spec 4.5.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct EntrySets {
    /// Each source with at least one kept entry, under every limit.
    pub(crate) sets: BTreeMap<SourceDomain, EntrySet>,
    /// What the limits dropped, in order.
    pub(crate) drops: Vec<EntryDrop>,
    /// Sources whose set was over `MAX_ENTRY_SET_BYTES` and was discarded
    /// whole. A stateful provider's positive part is then rejected.
    pub(crate) oversized: Vec<SourceDomain>,
}

impl EntrySets {
    /// Whether the result named at least one source and every set was
    /// discarded. Every entry that is not kept records a drop, so a result
    /// with entries and no kept set always has a drop or an oversized set.
    /// The coordinator handles such a result as `Malformed` (spec 4.5).
    #[must_use]
    pub(crate) fn all_discarded(&self) -> bool {
        self.sets.is_empty() && (!self.drops.is_empty() || !self.oversized.is_empty())
    }
}

fn earliest(current: Option<UnixTime>, candidate: Option<UnixTime>) -> Option<UnixTime> {
    match (current, candidate) {
        (Some(current), Some(candidate)) => Some(current.min(candidate)),
        (current, candidate) => current.or(candidate),
    }
}

fn clean_text(
    value: Option<&str>,
    part: DroppedPart,
    dropped: &mut Vec<DroppedPart>,
) -> Option<String> {
    let value = value?;
    if !is_valid_eid_field(value) {
        dropped.push(part);
        return None;
    }
    Some(value.to_owned())
}

fn clean_number(
    value: Option<i32>,
    part: DroppedPart,
    dropped: &mut Vec<DroppedPart>,
) -> Option<i32> {
    let value = value?;
    if value < 0 {
        dropped.push(part);
        return None;
    }
    Some(value)
}

fn clean_ext(
    value: Option<&serde_json::Value>,
    part: DroppedPart,
    dropped: &mut Vec<DroppedPart>,
) -> Option<serde_json::Value> {
    let value = value?;
    if !is_valid_partner_ext(value) {
        dropped.push(part);
        return None;
    }
    Some(value.clone())
}

fn clean_uid(uid: &ResolvedUid, dropped: &mut Vec<DroppedPart>) -> Option<KvUid> {
    if uid.id.trim().is_empty() || uid.id.len() > MAX_UID_LENGTH {
        dropped.push(DroppedPart::Uid);
        return None;
    }
    Some(KvUid {
        id: uid.id.clone(),
        atype: clean_number(uid.atype, DroppedPart::Atype, dropped),
        ext: clean_ext(uid.ext.as_ref(), DroppedPart::UidExt, dropped),
    })
}

fn clean_entry(eid: &ResolvedEid, dropped: &mut Vec<DroppedPart>) -> Option<KvEid> {
    let uids: Vec<KvUid> = eid
        .uids
        .iter()
        .filter_map(|uid| clean_uid(uid, dropped))
        .collect();
    if uids.is_empty() {
        return None;
    }
    Some(KvEid {
        uids,
        inserter: clean_text(eid.inserter.as_deref(), DroppedPart::Inserter, dropped),
        matcher: clean_text(eid.matcher.as_deref(), DroppedPart::Matcher, dropped),
        mm: clean_number(eid.mm, DroppedPart::Mm, dropped),
        ext: clean_ext(eid.ext.as_ref(), DroppedPart::EidExt, dropped),
    })
}

/// Groups `eids` by source, in order, and applies every limit of spec 4.5:
/// the browser parser's field and UID rules, then per-source caps the
/// browser parser does not have: duplicates, the first 8 entries and 32 UIDs
/// of a source, and `MAX_ENTRY_SET_BYTES` as [`entry_set_bytes`] measures
/// the stored set. A set over that is discarded whole, never trimmed: field
/// caps do not guarantee the set cap. Each set's `exp`, `ra` and `ru` are
/// the earliest its kept entries set; a dropped or discarded entry's times
/// do not count.
///
/// Source claims and operator narrowing are the coordinator's, which drops
/// a set for an unclaimed source before using the result.
///
/// # Performance
///
/// Serializes each set once to measure it, and compares each entry with
/// the at most 8 entries already kept.
#[must_use]
pub(crate) fn entry_sets(eids: &[ResolvedEid], now: UnixTime) -> EntrySets {
    let mut result = EntrySets::default();
    for eid in eids {
        let mut dropped = Vec::new();
        let cleaned = clean_entry(eid, &mut dropped);
        let set = result
            .sets
            .entry(eid.source.clone())
            .or_insert_with(|| EntrySet {
                entries: Vec::new(),
                expires_at: None,
                refresh_at: None,
                refresh_until: None,
            });
        match cleaned {
            None => dropped.push(DroppedPart::EmptyEntry),
            Some(entry) if set.entries.contains(&entry) => {
                dropped.push(DroppedPart::DuplicateEntry)
            }
            Some(_) if set.entries.len() >= MAX_ENTRIES_PER_SOURCE => {
                dropped.push(DroppedPart::ExtraEntry)
            }
            Some(mut entry) => {
                let used: usize = set.entries.iter().map(|kept| kept.uids.len()).sum();
                let room = MAX_UIDS_PER_SOURCE.saturating_sub(used);
                if entry.uids.len() > room {
                    dropped.extend(iter::repeat_n(
                        DroppedPart::ExtraUid,
                        entry.uids.len() - room,
                    ));
                    entry.uids.truncate(room);
                }
                if entry.uids.is_empty() {
                    dropped.push(DroppedPart::ExtraEntry);
                } else {
                    set.expires_at =
                        earliest(set.expires_at, eid.expiry.map(|when| when.resolve(now)));
                    set.refresh_at =
                        earliest(set.refresh_at, eid.refresh.map(|when| when.resolve(now)));
                    set.refresh_until = earliest(set.refresh_until, eid.refresh_until);
                    set.entries.push(entry);
                }
            }
        }
        result
            .drops
            .extend(dropped.into_iter().map(|part| EntryDrop {
                source: eid.source.clone(),
                part,
            }));
    }
    result.sets.retain(|source, set| {
        if set.entries.is_empty() {
            return false;
        }
        let fits = entry_set_bytes(&set.entries) <= MAX_ENTRY_SET_BYTES;
        if !fits {
            result.oversized.push(source.clone());
        }
        fits
    });
    result
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::*;
    use crate::ec::kv_types::MAX_EID_FIELD_BYTES;

    fn source(name: &str) -> SourceDomain {
        name.parse().expect("should parse the source")
    }

    fn uid(id: &str) -> ResolvedUid {
        ResolvedUid::new(id)
    }

    fn kv_uid(id: &str) -> KvUid {
        KvUid {
            id: id.to_owned(),
            atype: None,
            ext: None,
        }
    }

    fn now() -> UnixTime {
        UnixTime::from_secs(1_000)
    }

    fn parts(sets: &EntrySets) -> Vec<DroppedPart> {
        sets.drops.iter().map(|drop| drop.part).collect()
    }

    #[test]
    fn empty_found_is_no_match() {
        let resolution = Resolution::found([]);
        assert!(resolution.is_no_match(), "should be no_match");
        assert!(resolution.eids().is_empty(), "should hold no entries");
    }

    #[test]
    fn a_one_uid_entry_becomes_a_one_entry_set_without_eid_level_fields() {
        let a = source("a.example.com");
        let resolution = Resolution::found([ResolvedEid::new(&a, uid("u").with_atype(3))]);

        let sets = entry_sets(resolution.eids(), now());

        assert_eq!(
            sets.sets[&a].entries,
            vec![KvEid {
                uids: vec![KvUid {
                    id: "u".to_owned(),
                    atype: Some(3),
                    ext: None
                }],
                inserter: None,
                matcher: None,
                mm: None,
                ext: None,
            }],
            "should produce the one-UID shape browser ingest stores"
        );
        assert!(
            sets.drops.is_empty() && sets.oversized.is_empty(),
            "should drop nothing"
        );
    }

    #[test]
    fn entries_group_by_source_and_keep_every_uid_and_field_in_order() {
        let a = source("a.example.com");
        let b = source("b.example.com");
        let first = ResolvedEid::new(
            &a,
            uid("a1")
                .with_atype(1)
                .with_ext(json!({ "rtiPartner": "TDID" })),
        )
        .with_uid(uid("a2"))
        .with_inserter("inserter.example.com")
        .with_matcher("matcher.example.com")
        .with_mm(2)
        .with_ext(json!({ "segments": ["s"] }));
        let other = ResolvedEid::new(&b, uid("b1"));
        let second = ResolvedEid::new(&a, uid("a3"));

        let sets = entry_sets(&[first, other, second], now());

        assert_eq!(
            sets.sets[&a].entries,
            vec![
                KvEid {
                    uids: vec![
                        KvUid {
                            id: "a1".to_owned(),
                            atype: Some(1),
                            ext: Some(json!({ "rtiPartner": "TDID" }))
                        },
                        kv_uid("a2"),
                    ],
                    inserter: Some("inserter.example.com".to_owned()),
                    matcher: Some("matcher.example.com".to_owned()),
                    mm: Some(2),
                    ext: Some(json!({ "segments": ["s"] })),
                },
                KvEid {
                    uids: vec![kv_uid("a3")],
                    inserter: None,
                    matcher: None,
                    mm: None,
                    ext: None
                },
            ],
            "should keep a's entries in the order returned"
        );
        assert_eq!(sets.sets[&b].entries.len(), 1, "should group b on its own");
        assert!(sets.drops.is_empty(), "should drop nothing");
    }

    #[test]
    fn bad_fields_are_dropped_and_their_entries_kept() {
        let a = source("a.example.com");
        let entry = ResolvedEid::new(
            &a,
            uid("u")
                .with_atype(-1)
                .with_ext(json!(["not", "an", "object"])),
        )
        .with_inserter("  ")
        .with_matcher(&"m".repeat(MAX_EID_FIELD_BYTES + 1))
        .with_mm(-2)
        .with_ext(json!("not an object"));

        let sets = entry_sets(&[entry], now());

        assert_eq!(
            sets.sets[&a].entries,
            vec![KvEid {
                uids: vec![kv_uid("u")],
                inserter: None,
                matcher: None,
                mm: None,
                ext: None
            }],
            "should keep the entry without its bad fields"
        );
        assert_eq!(
            parts(&sets),
            vec![
                DroppedPart::Atype,
                DroppedPart::UidExt,
                DroppedPart::Inserter,
                DroppedPart::Matcher,
                DroppedPart::Mm,
                DroppedPart::EidExt,
            ],
            "should name each dropped field"
        );
    }

    #[test]
    fn bad_uids_are_dropped_and_an_entry_without_uids_is_discarded() {
        let a = source("a.example.com");
        let mixed = ResolvedEid::new(&a, uid(" ")).with_uid(uid("kept"));
        let empty = ResolvedEid::new(&a, uid(&"x".repeat(MAX_UID_LENGTH + 1)))
            .with_inserter("inserter.example.com");

        let sets = entry_sets(&[mixed, empty], now());

        assert_eq!(
            sets.sets[&a].entries.len(),
            1,
            "should discard the entry left with no UID"
        );
        assert_eq!(
            sets.sets[&a].entries[0].uids,
            vec![kv_uid("kept")],
            "should keep the valid UID"
        );
        assert_eq!(
            parts(&sets),
            vec![DroppedPart::Uid, DroppedPart::Uid, DroppedPart::EmptyEntry],
            "should name the dropped UIDs and the discarded entry"
        );
    }

    #[test]
    fn a_source_with_no_kept_entry_is_not_named() {
        let a = source("a.example.com");

        let sets = entry_sets(&[ResolvedEid::new(&a, uid(""))], now());

        assert!(
            !sets.sets.contains_key(&a),
            "should leave the stored set as it is"
        );
        assert_eq!(
            parts(&sets),
            vec![DroppedPart::Uid, DroppedPart::EmptyEntry],
            "should record the dropped UID and the discarded entry"
        );
        assert!(
            sets.all_discarded(),
            "should report that every named set was discarded"
        );
    }

    #[test]
    fn all_discarded_only_when_nothing_was_kept() {
        let a = source("a.example.com");
        let b = source("b.example.com");

        let mixed = entry_sets(
            &[
                ResolvedEid::new(&a, uid("")),
                ResolvedEid::new(&b, uid("kept")),
            ],
            now(),
        );
        let empty = entry_sets(&[], now());

        assert!(
            !mixed.all_discarded(),
            "should not report a result that kept one set"
        );
        assert!(
            !empty.all_discarded(),
            "should not report a result that named nothing"
        );
    }

    #[test]
    fn duplicates_and_entries_and_uids_over_the_limits_are_dropped() {
        let a = source("a.example.com");
        let mut eids = vec![
            ResolvedEid::new(&a, uid("e0")),
            ResolvedEid::new(&a, uid("e0")),
        ];
        // 7 more entries of 4 UIDs: 1 + 28 = 29 UIDs, then a ninth entry.
        for entry in 1..=7 {
            let mut eid = ResolvedEid::new(&a, uid(&format!("e{entry}u0")));
            for index in 1..4 {
                eid = eid.with_uid(uid(&format!("e{entry}u{index}")));
            }
            eids.push(eid);
        }
        eids.push(ResolvedEid::new(&a, uid("ninth")));

        let sets = entry_sets(&eids, now());

        let set = &sets.sets[&a];
        assert_eq!(
            set.entries.len(),
            MAX_ENTRIES_PER_SOURCE,
            "should keep 8 entries"
        );
        let uid_count: usize = set.entries.iter().map(|entry| entry.uids.len()).sum();
        assert_eq!(
            uid_count, 29,
            "should stay under the UID cap in this control"
        );
        assert_eq!(
            parts(&sets),
            vec![DroppedPart::DuplicateEntry, DroppedPart::ExtraEntry],
            "should drop the duplicate and the ninth entry"
        );

        let mut wide = ResolvedEid::new(&a, uid("u0"));
        for index in 1..=MAX_UIDS_PER_SOURCE {
            wide = wide.with_uid(uid(&format!("u{index}")));
        }
        let sets = entry_sets(&[wide, ResolvedEid::new(&a, uid("later"))], now());
        assert_eq!(
            sets.sets[&a].entries.len(),
            1,
            "should discard the entry left with no room"
        );
        assert_eq!(
            sets.sets[&a].entries[0].uids.len(),
            MAX_UIDS_PER_SOURCE,
            "should keep 32 UIDs"
        );
        assert_eq!(
            parts(&sets),
            vec![
                DroppedPart::ExtraUid,
                DroppedPart::ExtraUid,
                DroppedPart::ExtraEntry
            ],
            "should drop the 33rd UID and the entry with no room"
        );
    }

    #[test]
    fn an_oversized_set_is_discarded_whole_and_others_are_kept() {
        let a = source("a.example.com");
        let b = source("b.example.com");

        let sets = entry_sets(
            &[oversized_entry(&a), ResolvedEid::new(&b, uid("small"))],
            now(),
        );

        assert!(
            !sets.sets.contains_key(&a),
            "should never trim an oversized set"
        );
        assert_eq!(
            sets.oversized,
            vec![a.clone()],
            "should report the oversized source"
        );
        assert!(sets.sets.contains_key(&b), "should keep the other source");
        assert!(
            !sets.all_discarded(),
            "should not report the result as all discarded"
        );

        let only = entry_sets(&[oversized_entry(&a)], now());
        assert!(
            only.all_discarded(),
            "should report a result whose only set is oversized"
        );
    }

    fn oversized_entry(source: &SourceDomain) -> ResolvedEid {
        let mut big = ResolvedEid::new(source, uid(&"x".repeat(500)));
        for index in 0..8 {
            big = big.with_uid(uid(&format!("{index}{}", "x".repeat(499))));
        }
        big
    }

    #[test]
    fn a_set_takes_the_earliest_times_its_entries_set() {
        let a = source("a.example.com");
        let first = ResolvedEid::new(&a, uid("one"))
            .with_expires_in(Duration::from_secs(100))
            .with_refresh_at(UnixTime::from_secs(1_080))
            .with_refresh_until(UnixTime::from_secs(5_000));
        let second = ResolvedEid::new(&a, uid("two"))
            .with_expires_at(UnixTime::from_secs(1_050))
            .with_refresh_in(Duration::from_secs(90))
            .with_refresh_until(UnixTime::from_secs(4_000));
        let third = ResolvedEid::new(&a, uid("three"));

        let set = entry_sets(&[first, second, third], now())
            .sets
            .remove(&a)
            .expect("should keep a");

        assert_eq!(
            set.expires_at,
            Some(UnixTime::from_secs(1_050)),
            "should take the earliest expiry"
        );
        assert_eq!(
            set.refresh_at,
            Some(UnixTime::from_secs(1_080)),
            "should take the earliest refresh"
        );
        assert_eq!(
            set.refresh_until,
            Some(UnixTime::from_secs(4_000)),
            "should take the earliest refresh limit"
        );
    }

    #[test]
    fn dropped_entries_do_not_count_toward_timing() {
        let a = source("a.example.com");
        let kept = ResolvedEid::new(&a, uid("one")).with_expires_at(UnixTime::from_secs(2_000));
        let duplicate =
            ResolvedEid::new(&a, uid("one")).with_expires_at(UnixTime::from_secs(1_100));
        let empty = ResolvedEid::new(&a, uid("")).with_refresh_at(UnixTime::from_secs(1_010));

        let set = entry_sets(&[kept, duplicate, empty], now())
            .sets
            .remove(&a)
            .expect("should keep a");

        assert_eq!(
            set.expires_at,
            Some(UnixTime::from_secs(2_000)),
            "should ignore the expiry of a dropped duplicate"
        );
        assert_eq!(
            set.refresh_at, None,
            "should ignore the refresh time of a discarded entry"
        );
    }

    #[test]
    fn invalidations_chain_and_state_changes_are_reported() {
        let a = source("a.example.com");
        let b = source("b.example.com");
        let resolution = Resolution::found([ResolvedEid::new(&a, uid("u"))])
            .also_invalidate(&b)
            .with_retry_in(Duration::from_secs(60))
            .replacing_state(&json!({ "signature": "s" }));
        assert_eq!(resolution.invalidated(), &[b], "should invalidate b");
        assert_eq!(
            resolution.retry_in(),
            Some(Duration::from_secs(60)),
            "should keep the retry delay"
        );
        assert_eq!(
            resolution.state_change(),
            StateChange::Replace(r#"{"signature":"s"}"#),
            "should replace the state"
        );
        assert_eq!(
            Resolution::no_match().clearing_state().state_change(),
            StateChange::Clear,
            "should clear the state"
        );
        assert_eq!(
            Resolution::no_match().state_change(),
            StateChange::Keep,
            "should keep the state by default"
        );
    }

    #[test]
    fn resolution_debug_redacts_state() {
        let resolution =
            Resolution::no_match().replacing_state(&json!({ "signature": "secret-state" }));

        let debug = format!("{resolution:?}");

        assert!(
            !debug.contains("secret-state"),
            "should not print provider state"
        );
        assert!(
            debug.contains("Replace([redacted])"),
            "should show that state is replaced"
        );
    }

    #[test]
    fn oversized_state_is_marked_invalid_not_dropped_silently() {
        let resolution = Resolution::found([ResolvedEid::new(&source("a.example.com"), uid("u"))])
            .replacing_state(&"x".repeat(2000));
        assert!(
            resolution.state_rejected(),
            "should flag the state as rejected"
        );
    }

    #[test]
    fn oversized_ext_is_dropped_and_the_uid_and_entry_kept() {
        let pad = json!({ "pad": "x".repeat(MAX_PARTNER_EXT_BYTES) });
        let resolved = uid("u").with_ext(pad.clone());
        assert_eq!(resolved.id(), "u", "should keep the UID");
        assert!(resolved.ext().is_none(), "should drop the UID-level ext");
        let small = uid("u").with_ext(json!({ "linkType": 2 }));
        assert_eq!(
            small.ext(),
            Some(&json!({ "linkType": 2 })),
            "should keep a small ext"
        );

        let eid = ResolvedEid::new(&source("a.example.com"), uid("u")).with_ext(pad);
        assert_eq!(eid.uids().len(), 1, "should keep the entry");
        assert!(eid.ext().is_none(), "should drop the EID-level ext");
    }

    #[test]
    fn debug_redacts_every_uid() {
        let eid = ResolvedEid::new(&source("a.example.com"), uid("secret-one"))
            .with_uid(uid("secret-two"));
        let debug = format!("{eid:?} {:?}", eid.uids()[0]);
        assert!(!debug.contains("secret"), "should not print a UID");
        assert!(debug.contains("a.example.com"), "should print the source");
    }

    #[test]
    fn opted_out_is_reported() {
        assert!(
            Resolution::opted_out().is_opted_out(),
            "should report the opt-out"
        );
        assert!(
            !Resolution::no_match().is_opted_out(),
            "should not report an opt-out for no_match"
        );
    }
}
