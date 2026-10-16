//! The request's consent context: what each permission signal module read
//! from the request, in neutral terms.
//!
//! A module reads its own scheme's signal from the request and reports it as
//! a [`SignalRecord`], being the string as received, whether it decoded, where
//! it came from, and the decoded form, which is the module's own type and is
//! opaque here. Core carries the records, the jurisdiction, and nothing of how
//! any scheme spells its signal.

use core::any::Any;
use core::fmt;
use std::sync::Arc;

use super::jurisdiction::Jurisdiction;
use crate::permissions::{PermissionSet, ValidSignal};

/// The consent context for a request, as the permission signal modules
/// filled it.
///
/// One [`SignalRecord`] per scheme a configured module read, in the order the
/// modules were asked, and the request's privacy jurisdiction. Built once by
/// the consent pipeline and read by the permission assembly, the bid request
/// builders and the identity graph.
#[derive(Debug, Clone, Default)]
pub struct ConsentContext {
    /// What each configured module read, one record per scheme, in module
    /// order.
    pub records: Vec<SignalRecord>,
    /// The privacy jurisdiction the request falls under.
    pub jurisdiction: Jurisdiction,
}

impl ConsentContext {
    /// A context holding no records under `jurisdiction`.
    #[must_use]
    pub fn new(jurisdiction: Jurisdiction) -> Self {
        Self {
            records: Vec::new(),
            jurisdiction,
        }
    }

    /// The same context with `record` added, replacing any record of the
    /// same scheme, so one scheme is reported once.
    #[must_use]
    pub fn with_record(mut self, record: SignalRecord) -> Self {
        self.records.retain(|held| held.scheme != record.scheme);
        self.records.push(record);
        self
    }

    /// The record of `scheme`, when a module read one.
    #[must_use]
    pub fn record(&self, scheme: &str) -> Option<&SignalRecord> {
        self.records.iter().find(|record| record.scheme == scheme)
    }

    /// The decoded form of `scheme`, when a module read one and it is of
    /// type `T`.
    #[must_use]
    pub fn decoded<T: Any>(&self, scheme: &str) -> Option<&T> {
        self.record(scheme).and_then(SignalRecord::decoded)
    }

    /// The signal of `scheme` as it was received, when a module read one.
    #[must_use]
    pub fn value(&self, scheme: &str) -> Option<&str> {
        self.record(scheme).and_then(SignalRecord::value)
    }

    /// Whether any module says the regulation its scheme answers for applies
    /// to this request.
    #[must_use]
    pub fn applies(&self) -> bool {
        self.records.iter().any(|record| record.applies)
    }

    /// Whether no record carries a signal.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.iter().all(|record| record.value.is_none())
    }

    /// Keeps only the signals a configured module vouched for, so a signal
    /// that was absent, unreadable, expired or not acted on is never forwarded
    /// in a bid request or anywhere else this context is read.
    ///
    /// `signals` are the [`ValidSignal`]s on the assembled permission state,
    /// matched by scheme. A record nobody vouched for keeps its status and
    /// what it decoded, because the modules have already read those, and loses
    /// the string and the sections a reader would forward.
    pub fn keep_only(&mut self, signals: &[ValidSignal]) {
        for record in &mut self.records {
            if !signals.iter().any(|signal| signal.scheme == record.scheme) {
                record.value = None;
                record.sections = Vec::new();
            }
        }
    }

    /// Drops every record `keep` answers `false` for.
    pub fn retain(&mut self, keep: impl FnMut(&SignalRecord) -> bool) {
        self.records.retain(keep);
    }
}

/// What a permission signal module read from the request for its scheme.
#[derive(Clone)]
pub struct SignalRecord {
    scheme: &'static str,
    value: Option<String>,
    status: SignalStatus,
    source: ConsentSource,
    applies: bool,
    sections: Vec<u16>,
    consented: PermissionSet,
    decoded: Option<Arc<dyn Any + Send + Sync>>,
}

impl SignalRecord {
    /// A record of `scheme` in `status`, carrying no string, no decoded form
    /// and no source yet.
    #[must_use]
    pub fn new(scheme: &'static str, status: SignalStatus) -> Self {
        Self {
            scheme,
            value: None,
            status,
            source: ConsentSource::None,
            applies: false,
            sections: Vec::new(),
            consented: PermissionSet::none(),
            decoded: None,
        }
    }

    /// The same record carrying the signal as it was received, from a cookie
    /// or a header.
    #[must_use]
    pub fn with_value(mut self, value: impl Into<String>) -> Self {
        self.value = Some(value.into());
        self.source = ConsentSource::Cookie;
        self
    }

    /// The same record carrying `decoded`, the module's own form of the
    /// signal, read back with [`decoded`](Self::decoded).
    #[must_use]
    pub fn with_decoded<T: Any + Send + Sync>(mut self, decoded: T) -> Self {
        self.decoded = Some(Arc::new(decoded));
        self
    }

    /// The same record with where the signal came from.
    #[must_use]
    pub fn with_source(mut self, source: ConsentSource) -> Self {
        self.source = source;
        self
    }

    /// The same record saying whether the regulation the scheme answers for
    /// applies to this request.
    #[must_use]
    pub fn with_applies(mut self, applies: bool) -> Self {
        self.applies = applies;
        self
    }

    /// The same record carrying the identifiers of the sections the signal
    /// is made of, for a scheme whose string carries sections.
    #[must_use]
    pub fn with_sections(mut self, sections: Vec<u16>) -> Self {
        self.sections = sections;
        self
    }

    /// The same record carrying the permissions the signal itself holds the
    /// visitor's consent to, read from the signal alone, whatever the policy
    /// and the place rules make of it.
    #[must_use]
    pub fn with_consented(mut self, consented: PermissionSet) -> Self {
        self.consented = consented;
        self
    }

    /// The scheme, as the module names it and a reader asks for it.
    #[must_use]
    pub fn scheme(&self) -> &'static str {
        self.scheme
    }

    /// The signal as it was received, when one arrived.
    #[must_use]
    pub fn value(&self) -> Option<&str> {
        self.value.as_deref()
    }

    /// Whether the signal decoded, could not be read, expired, or was left
    /// as received.
    #[must_use]
    pub fn status(&self) -> SignalStatus {
        self.status
    }

    /// Where the signal came from.
    #[must_use]
    pub fn source(&self) -> ConsentSource {
        self.source
    }

    /// Whether the module says the regulation its scheme answers for applies
    /// to this request. A scheme that does not say leaves it `false`.
    #[must_use]
    pub fn applies(&self) -> bool {
        self.applies
    }

    /// The identifiers of the sections the signal is made of, in the scheme's
    /// own numbering, and empty for a scheme without sections.
    #[must_use]
    pub fn sections(&self) -> &[u16] {
        &self.sections
    }

    /// The permissions the signal holds the visitor's consent to, and none
    /// for a signal that carries no consent, such as an opt-out, or one that
    /// did not decode.
    #[must_use]
    pub fn consented(&self) -> PermissionSet {
        self.consented
    }

    /// The decoded form, when the module stored one of type `T`.
    #[must_use]
    pub fn decoded<T: Any>(&self) -> Option<&T> {
        self.decoded
            .as_deref()
            .and_then(|decoded| decoded.downcast_ref::<T>())
    }
}

impl fmt::Debug for SignalRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SignalRecord")
            .field("scheme", &self.scheme)
            .field("value", &self.value.as_ref().map(String::len))
            .field("status", &self.status)
            .field("source", &self.source)
            .field("applies", &self.applies)
            .field("sections", &self.sections)
            .field("consented", &self.consented)
            .field("decoded", &self.decoded.is_some())
            .finish()
    }
}

/// What became of the signal a module read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalStatus {
    /// No signal of the scheme arrived. A record in this state carries what
    /// the module can still say without one.
    Absent,
    /// The signal decoded, and the record carries the decoded form.
    Decoded,
    /// The signal arrived and was left as received, because the settings
    /// say not to decode.
    NotDecoded,
    /// The signal arrived and could not be read.
    Unreadable,
    /// The signal decoded and is older than the settings allow.
    Expired,
}

impl fmt::Display for SignalStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Absent => "absent",
            Self::Decoded => "present",
            Self::NotDecoded => "not-decoded",
            Self::Unreadable => "decode-failed",
            Self::Expired => "expired",
        })
    }
}

/// Where a signal came from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ConsentSource {
    /// Read from the request, a cookie or a header.
    Cookie,
    /// Constructed from the deployment's declared policy defaults.
    PolicyDefault,
    /// No signal.
    #[default]
    None,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_context_holds_one_record_per_scheme() {
        let context = ConsentContext::default()
            .with_record(SignalRecord::new("first", SignalStatus::Decoded).with_value("one"))
            .with_record(SignalRecord::new("first", SignalStatus::Decoded).with_value("two"));
        assert_eq!(context.records.len(), 1, "a scheme is reported once");
        assert_eq!(
            context.value("first"),
            Some("two"),
            "the later record replaces the earlier one"
        );
        assert_eq!(
            context.value("other"),
            None,
            "an unread scheme has no value"
        );
    }

    #[test]
    fn a_record_carries_its_decoded_form_by_type() {
        #[derive(Debug, PartialEq)]
        struct Decoded(u8);

        let record = SignalRecord::new("example", SignalStatus::Decoded).with_decoded(Decoded(7));
        assert_eq!(record.decoded::<Decoded>(), Some(&Decoded(7)));
        assert_eq!(
            record.decoded::<String>(),
            None,
            "asking for another type answers nothing rather than something wrong"
        );
        let context = ConsentContext::default().with_record(record);
        assert_eq!(context.decoded::<Decoded>("example"), Some(&Decoded(7)));
    }

    #[test]
    fn a_value_marks_the_record_as_read_from_the_request() {
        let read = SignalRecord::new("example", SignalStatus::Decoded).with_value("v");
        assert_eq!(read.source(), ConsentSource::Cookie);
        let declared = read.with_source(ConsentSource::PolicyDefault);
        assert_eq!(
            declared.source(),
            ConsentSource::PolicyDefault,
            "a source set afterwards stands"
        );
        assert_eq!(
            SignalRecord::new("example", SignalStatus::Absent).source(),
            ConsentSource::None
        );
    }

    #[test]
    fn emptiness_and_applicability_read_across_the_records() {
        assert!(ConsentContext::default().is_empty());
        let context = ConsentContext::default()
            .with_record(SignalRecord::new("a", SignalStatus::Absent).with_applies(true))
            .with_record(SignalRecord::new("b", SignalStatus::Decoded).with_value("x"));
        assert!(!context.is_empty(), "a record with a value is a signal");
        assert!(context.applies(), "one record applying is enough");
        assert!(
            !ConsentContext::default()
                .with_record(SignalRecord::new("a", SignalStatus::Absent))
                .applies()
        );
    }

    #[test]
    fn keep_only_drops_the_strings_nobody_vouched_for() {
        let mut context = ConsentContext::default()
            .with_record(
                SignalRecord::new("kept", SignalStatus::Decoded)
                    .with_value("k")
                    .with_sections(vec![1]),
            )
            .with_record(
                SignalRecord::new("dropped", SignalStatus::Unreadable)
                    .with_value("d")
                    .with_sections(vec![2])
                    .with_applies(true),
            );
        context.keep_only(&[ValidSignal::new("module", "kept", "k")]);
        assert_eq!(context.value("kept"), Some("k"));
        assert_eq!(
            context.record("kept").map(SignalRecord::sections),
            Some(&[1][..])
        );
        let dropped = context.record("dropped").expect("the record stays");
        assert_eq!(dropped.value(), None, "the string goes no further");
        assert!(dropped.sections().is_empty(), "nor do its sections");
        assert_eq!(
            dropped.status(),
            SignalStatus::Unreadable,
            "what the module read stays on record"
        );
        assert!(dropped.applies(), "and so does what it said applies");
    }

    #[test]
    fn a_record_carries_what_the_visitor_consented_to_whoever_vouched() {
        use crate::permissions::Permission;

        let storage = PermissionSet::none().with(Permission::StoreOnDevice);
        assert!(
            SignalRecord::new("example", SignalStatus::Decoded)
                .consented()
                .is_empty(),
            "a record holds no consent unless its module says so"
        );
        let mut context = ConsentContext::default().with_record(
            SignalRecord::new("example", SignalStatus::Decoded)
                .with_value("v")
                .with_consented(storage),
        );
        context.keep_only(&[]);
        assert_eq!(
            context.record("example").map(SignalRecord::consented),
            Some(storage),
            "what the module read stays on record when the string goes no further"
        );
    }

    #[test]
    fn retain_drops_whole_records() {
        let mut context = ConsentContext::default()
            .with_record(SignalRecord::new("a", SignalStatus::Decoded).with_value("1"))
            .with_record(
                SignalRecord::new("b", SignalStatus::Decoded)
                    .with_value("2")
                    .with_source(ConsentSource::PolicyDefault),
            );
        context.retain(|record| record.source() == ConsentSource::PolicyDefault);
        assert!(context.record("a").is_none());
        assert_eq!(context.value("b"), Some("2"));
    }

    #[test]
    fn status_reads_as_a_word() {
        assert_eq!(SignalStatus::Decoded.to_string(), "present");
        assert_eq!(SignalStatus::Unreadable.to_string(), "decode-failed");
        assert_eq!(SignalStatus::Expired.to_string(), "expired");
        assert_eq!(SignalStatus::NotDecoded.to_string(), "not-decoded");
        assert_eq!(SignalStatus::Absent.to_string(), "absent");
    }

    #[test]
    fn debug_shows_the_length_of_a_signal_and_never_its_text() {
        let record = SignalRecord::new("example", SignalStatus::Decoded).with_value("secret");
        let shown = format!("{record:?}");
        assert!(shown.contains("value: Some(6)"), "{shown}");
        assert!(!shown.contains("secret"), "{shown}");
    }
}
