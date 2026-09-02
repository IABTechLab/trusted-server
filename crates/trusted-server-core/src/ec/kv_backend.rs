//! Platform-neutral KV primitives for the EC identity graph.
//!
//! [`super::kv::KvIdentityGraph`] owns all identity-graph business logic
//! (CAS retry loops, consent tombstone semantics, entry validation) and
//! delegates raw store access to an [`EcKvStore`] implementation provided
//! by the adapter crate (e.g. the Fastly KV Store backend in
//! `trusted-server-adapter-fastly`).
//!
//! This trait is intentionally narrow: lookup with a generation marker,
//! conditional insert, prefix counting, and delete. Conditional writes are
//! expressed through [`EcKvWriteMode`] so compare-and-swap loops stay in
//! core while the platform supplies the actual precondition mechanics.

use std::time::Duration;

use error_stack::Report;

use crate::error::TrustedServerError;

/// Result of a successful [`EcKvStore::lookup`] for an existing key.
#[derive(Debug, Clone)]
pub struct EcKvLookup {
    /// Raw entry body bytes.
    pub body: Vec<u8>,
    /// Raw metadata bytes, when the platform stored any.
    pub metadata: Option<Vec<u8>>,
    /// Generation marker for subsequent compare-and-swap writes.
    pub generation: u64,
}

/// Write request passed to [`EcKvStore::insert`].
#[derive(Debug, Clone, Copy)]
pub struct EcKvWrite<'a> {
    /// Serialized entry body.
    pub body: &'a str,
    /// Serialized entry metadata.
    pub metadata: &'a str,
    /// Time-to-live for the written entry.
    pub ttl: Duration,
    /// Precondition mode for the write.
    pub mode: EcKvWriteMode,
}

/// Precondition mode for an [`EcKvStore::insert`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EcKvWriteMode {
    /// Create the key; fail with [`EcKvWriteOutcome::PreconditionFailed`]
    /// when the key already exists.
    Add,
    /// Unconditionally overwrite any existing value.
    Overwrite,
    /// Write only when the stored generation matches the provided marker.
    IfGenerationMatch(u64),
}

/// Outcome of an [`EcKvStore::insert`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EcKvWriteOutcome {
    /// The write was applied.
    Written,
    /// The write precondition failed (key exists for
    /// [`EcKvWriteMode::Add`], or generation mismatch for
    /// [`EcKvWriteMode::IfGenerationMatch`]).
    PreconditionFailed,
}

/// Whether `key` appears exactly in a paged listing.
///
/// Backends that can only match by prefix use this to decide existence: a
/// longer key carrying `key` as a prefix is a different identity and must not
/// answer for it, so the listed keys are compared for equality.
///
/// Every page is visited until a match is found. Nothing guarantees the exact
/// key lands in the first page when other keys share its prefix, and stopping
/// early would report a held identity as missing.
///
/// `max_pages` bounds how far the listing is followed. Running out of budget
/// is reported as [`ExactKeyMatch::Undetermined`] rather than as absence: the
/// key may sit on a page that was never read, and answering "absent" would
/// discard a withdrawal for an identity the store actually holds. The listing
/// is read lazily and at most one page beyond `max_pages` — just far enough to
/// learn that it continues — so callers pass their listing untruncated rather
/// than pre-trimming it to a count that has to agree with this one.
///
/// # Errors
///
/// Returns the first page error, so a listing that cannot be read is never
/// mistaken for a listing that does not contain the key.
pub fn contains_exact_key<E>(
    pages: impl IntoIterator<Item = Result<Vec<String>, E>>,
    key: &str,
    max_pages: usize,
) -> Result<ExactKeyMatch, E> {
    for (index, page) in pages.into_iter().enumerate() {
        if index >= max_pages {
            return Ok(ExactKeyMatch::Undetermined);
        }
        if page?.iter().any(|listed| listed == key) {
            return Ok(ExactKeyMatch::Found);
        }
    }
    Ok(ExactKeyMatch::Absent)
}

/// What a bounded prefix listing established about one exact key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExactKeyMatch {
    /// The listing contained the key.
    Found,
    /// The listing was read to its end and did not contain the key.
    Absent,
    /// The listing was longer than the budget allowed, so the key's absence
    /// was never established.
    Undetermined,
}

/// Raw KV store primitives backing the EC identity graph.
///
/// Implementations map these operations onto the platform KV API.
/// Infrastructure failures are reported as [`TrustedServerError::KvStore`];
/// write precondition failures are part of the normal control flow and are
/// returned as [`EcKvWriteOutcome::PreconditionFailed`] instead of errors.
pub trait EcKvStore {
    /// Returns the platform store name, used in log and error messages.
    fn store_name(&self) -> &str;

    /// Reads the body, metadata, and generation marker for a key.
    ///
    /// Returns `Ok(None)` when the key does not exist.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] on store open or read failure.
    fn lookup(&self, key: &str) -> Result<Option<EcKvLookup>, Report<TrustedServerError>>;

    /// Writes an entry according to the requested precondition mode.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] on store open or write
    /// failure. Precondition failures are reported through the
    /// [`EcKvWriteOutcome`] instead.
    fn insert(
        &self,
        key: &str,
        write: EcKvWrite<'_>,
    ) -> Result<EcKvWriteOutcome, Report<TrustedServerError>>;

    /// Counts keys sharing the given prefix, up to `limit`.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] on store open or list failure.
    fn count_keys_with_prefix(
        &self,
        prefix: &str,
        limit: u32,
    ) -> Result<u32, Report<TrustedServerError>>;

    /// Whether the store holds exactly `key`.
    ///
    /// Must be an exact match and strongly consistent. A prefix scan is not a
    /// substitute: another key may carry this one as a prefix, and a read that
    /// may lag would report a freshly written key as absent.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] on store open or list failure.
    fn key_exists(&self, key: &str) -> Result<bool, Report<TrustedServerError>>;

    /// Hard-deletes a key.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::KvStore`] on store open or delete failure.
    fn delete(&self, key: &str) -> Result<(), Report<TrustedServerError>>;
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use super::*;

    /// In-memory [`EcKvStore`] with generation tracking for CAS tests.
    pub(crate) struct InMemoryEcKv {
        name: String,
        entries: Mutex<BTreeMap<String, StoredEntry>>,
    }

    struct StoredEntry {
        body: Vec<u8>,
        metadata: Option<Vec<u8>>,
        generation: u64,
    }

    impl InMemoryEcKv {
        pub(crate) fn new(name: impl Into<String>) -> Self {
            Self {
                name: name.into(),
                entries: Mutex::new(BTreeMap::new()),
            }
        }
    }

    impl EcKvStore for InMemoryEcKv {
        fn store_name(&self) -> &str {
            &self.name
        }

        fn lookup(&self, key: &str) -> Result<Option<EcKvLookup>, Report<TrustedServerError>> {
            let entries = self.entries.lock().expect("should lock in-memory store");
            Ok(entries.get(key).map(|stored| EcKvLookup {
                body: stored.body.clone(),
                metadata: stored.metadata.clone(),
                generation: stored.generation,
            }))
        }

        fn insert(
            &self,
            key: &str,
            write: EcKvWrite<'_>,
        ) -> Result<EcKvWriteOutcome, Report<TrustedServerError>> {
            let mut entries = self.entries.lock().expect("should lock in-memory store");
            let existing_generation = entries.get(key).map(|stored| stored.generation);

            match write.mode {
                EcKvWriteMode::Add if existing_generation.is_some() => {
                    return Ok(EcKvWriteOutcome::PreconditionFailed);
                }
                EcKvWriteMode::IfGenerationMatch(expected)
                    if existing_generation != Some(expected) =>
                {
                    return Ok(EcKvWriteOutcome::PreconditionFailed);
                }
                EcKvWriteMode::Add
                | EcKvWriteMode::Overwrite
                | EcKvWriteMode::IfGenerationMatch(_) => {}
            }

            entries.insert(
                key.to_owned(),
                StoredEntry {
                    body: write.body.as_bytes().to_vec(),
                    metadata: Some(write.metadata.as_bytes().to_vec()),
                    generation: existing_generation.unwrap_or(0) + 1,
                },
            );
            Ok(EcKvWriteOutcome::Written)
        }

        fn count_keys_with_prefix(
            &self,
            prefix: &str,
            limit: u32,
        ) -> Result<u32, Report<TrustedServerError>> {
            let entries = self.entries.lock().expect("should lock in-memory store");
            let count = entries
                .keys()
                .filter(|key| key.starts_with(prefix))
                .take(limit as usize)
                .count();
            #[allow(clippy::cast_possible_truncation)]
            Ok(count as u32)
        }

        fn key_exists(&self, key: &str) -> Result<bool, Report<TrustedServerError>> {
            let entries = self.entries.lock().expect("should lock in-memory store");
            Ok(entries.contains_key(key))
        }

        fn delete(&self, key: &str) -> Result<(), Report<TrustedServerError>> {
            let mut entries = self.entries.lock().expect("should lock in-memory store");
            entries.remove(key);
            Ok(())
        }
    }

    /// [`EcKvStore`] that fails every operation, mimicking a missing or
    /// unreachable platform store.
    pub(crate) struct FailingEcKv {
        name: String,
    }

    impl FailingEcKv {
        pub(crate) fn new(name: impl Into<String>) -> Self {
            Self { name: name.into() }
        }

        fn error(&self, operation: &str) -> Report<TrustedServerError> {
            Report::new(TrustedServerError::KvStore {
                store_name: self.name.clone(),
                message: format!("KV store not found (failing test store, {operation})"),
            })
        }
    }

    impl EcKvStore for FailingEcKv {
        fn store_name(&self) -> &str {
            &self.name
        }

        fn lookup(&self, _key: &str) -> Result<Option<EcKvLookup>, Report<TrustedServerError>> {
            Err(self.error("lookup"))
        }

        fn insert(
            &self,
            _key: &str,
            _write: EcKvWrite<'_>,
        ) -> Result<EcKvWriteOutcome, Report<TrustedServerError>> {
            Err(self.error("insert"))
        }

        fn count_keys_with_prefix(
            &self,
            _prefix: &str,
            _limit: u32,
        ) -> Result<u32, Report<TrustedServerError>> {
            Err(self.error("list"))
        }

        fn key_exists(&self, _key: &str) -> Result<bool, Report<TrustedServerError>> {
            Err(self.error("key_exists"))
        }

        fn delete(&self, _key: &str) -> Result<(), Report<TrustedServerError>> {
            Err(self.error("delete"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ExactKeyMatch, contains_exact_key};

    /// Page iterator that records how many pages were consumed.
    struct CountingPages {
        pages: std::vec::IntoIter<Result<Vec<String>, &'static str>>,
        consumed: std::rc::Rc<std::cell::Cell<usize>>,
    }

    impl Iterator for CountingPages {
        type Item = Result<Vec<String>, &'static str>;

        fn next(&mut self) -> Option<Self::Item> {
            let page = self.pages.next();
            if page.is_some() {
                self.consumed.set(self.consumed.get() + 1);
            }
            page
        }
    }

    fn counting(
        pages: Vec<Result<Vec<String>, &'static str>>,
    ) -> (CountingPages, std::rc::Rc<std::cell::Cell<usize>>) {
        let consumed = std::rc::Rc::new(std::cell::Cell::new(0));
        (
            CountingPages {
                pages: pages.into_iter(),
                consumed: std::rc::Rc::clone(&consumed),
            },
            consumed,
        )
    }

    fn page(keys: &[&str]) -> Result<Vec<String>, &'static str> {
        Ok(keys.iter().map(|key| (*key).to_owned()).collect())
    }

    #[test]
    fn finds_a_match_on_a_later_page() {
        let (pages, _) = counting(vec![
            page(&["wanted-suffix-a", "wanted-suffix-b"]),
            page(&["wanted-suffix-c"]),
            page(&["wanted"]),
        ]);

        assert_eq!(
            contains_exact_key(pages, "wanted", 8).expect("should scan the pages"),
            ExactKeyMatch::Found,
            "a match on the last page must still be found"
        );
    }

    #[test]
    fn ignores_keys_that_only_start_with_the_one_asked_for() {
        let (pages, _) = counting(vec![page(&["wanted-suffix", "wantedx", "wanted2"])]);

        assert_eq!(
            contains_exact_key(pages, "wanted", 8).expect("should scan the pages"),
            ExactKeyMatch::Absent,
            "a longer key is a different identity"
        );
    }

    #[test]
    fn stops_at_the_first_match() {
        let (pages, consumed) = counting(vec![
            page(&["wanted"]),
            page(&["never-read"]),
            page(&["never-read-either"]),
        ]);

        assert_eq!(
            contains_exact_key(pages, "wanted", 8).expect("should scan the pages"),
            ExactKeyMatch::Found,
            "should find the match"
        );
        assert_eq!(consumed.get(), 1, "should not read past the match");
    }

    #[test]
    fn propagates_a_page_error_rather_than_reporting_absent() {
        let (pages, _) = counting(vec![page(&["wanted-suffix"]), Err("list unavailable")]);

        assert_eq!(
            contains_exact_key(pages, "wanted", 8),
            Err("list unavailable"),
            "an unreadable listing must not read as absent"
        );
    }

    #[test]
    fn reports_undetermined_when_the_listing_outruns_the_budget() {
        // The key sits past the budget, which is exactly the case that must
        // not read as absent: answering "absent" discards the withdrawal.
        let (pages, consumed) = counting(vec![
            page(&["wanted-suffix-a"]),
            page(&["wanted-suffix-b"]),
            page(&["wanted"]),
        ]);

        assert_eq!(
            contains_exact_key(pages, "wanted", 2).expect("should scan the pages"),
            ExactKeyMatch::Undetermined,
            "a key beyond the budget must not read as absent"
        );
        assert_eq!(
            consumed.get(),
            3,
            "should read one page past the budget to learn the listing continues"
        );
    }

    #[test]
    fn reports_absent_for_a_listing_that_ends_exactly_at_the_budget() {
        let (pages, _) = counting(vec![page(&["wanted-suffix-a"]), page(&["wanted-suffix-b"])]);

        assert_eq!(
            contains_exact_key(pages, "wanted", 2).expect("should scan the pages"),
            ExactKeyMatch::Absent,
            "a listing that fits the budget is answered definitively"
        );
    }

    #[test]
    fn finds_a_match_on_the_last_page_within_the_budget() {
        let (pages, _) = counting(vec![page(&["wanted-suffix-a"]), page(&["wanted"])]);

        assert_eq!(
            contains_exact_key(pages, "wanted", 2).expect("should scan the pages"),
            ExactKeyMatch::Found,
            "the final permitted page still counts"
        );
    }

    #[test]
    fn reports_absent_for_an_empty_listing() {
        let (pages, _) = counting(Vec::new());

        assert_eq!(
            contains_exact_key(pages, "wanted", 8).expect("should scan the pages"),
            ExactKeyMatch::Absent,
            "nothing listed means nothing held"
        );
    }
}
