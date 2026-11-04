//! The one-KV-write-per-page-load rule, enforced by types.
//!
//! A request classified as a top-level document navigation receives a
//! [`NavigationPermit`]. Once the final response status is known it becomes a
//! [`PageLoadWrite`], which [`KvIdentityGraph::commit_page_load`] consumes.
//! Neither type is `Clone`, so one navigation makes at most one write before
//! its response.
//!
//! The one exception is [`AfterResponseWrite`], confirmed alongside the page
//! load's permit. It allows one follow-up update after the response has been
//! sent, carrying only the results of after-response identity lookups.
//!
//! [`KvIdentityGraph::commit_page_load`]: super::kv::KvIdentityGraph::commit_page_load

use edgezero_core::body::Body as EdgeBody;
use http::{Method, Request, StatusCode};

use super::EcKvSnapshot;
use super::kv_types::KvEntry;

/// Issued to a request classified as a page-load navigation.
#[derive(Debug)]
pub struct NavigationPermit(());

impl NavigationPermit {
    /// Returns a permit when `request` is a top-level document navigation:
    /// `GET`, `Sec-Fetch-Dest: document`, `Sec-Fetch-Mode: navigate`, and no
    /// `prefetch` token in `Sec-Purpose`.
    ///
    /// Call once per request, from the publisher-proxy route only.
    #[must_use]
    pub fn classify(request: &Request<EdgeBody>) -> Option<Self> {
        let headers = request.headers();
        let header_is = |name: &str, expected: &str| {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.trim().eq_ignore_ascii_case(expected))
        };
        let prefetch = headers
            .get("sec-purpose")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| {
                value
                    .split([';', ','])
                    .any(|token| token.trim().eq_ignore_ascii_case("prefetch"))
            });
        let navigation = request.method() == Method::GET
            && header_is("sec-fetch-dest", "document")
            && header_is("sec-fetch-mode", "navigate")
            && !prefetch;
        navigation.then_some(Self(()))
    }

    /// Confirms the permit once the final response status is known.
    ///
    /// Returns `None` for a redirect (301, 302, 303, 307, 308): the redirect
    /// target is the page load. Other 3xx statuses, such as 304 Not Modified,
    /// are the page load itself and keep the permit.
    #[must_use]
    pub fn confirm(self, status: StatusCode) -> Option<PageLoadWrite> {
        (!Self::is_redirect(status)).then_some(PageLoadWrite(()))
    }

    /// Confirms the permit like [`Self::confirm`] and also issues the
    /// navigation's [`AfterResponseWrite`].
    ///
    /// Returns `None` for a redirect, which makes neither write.
    #[must_use]
    pub fn confirm_with_after_response(
        self,
        status: StatusCode,
    ) -> Option<(PageLoadWrite, AfterResponseWrite)> {
        self.confirm(status)
            .map(|permit| (permit, AfterResponseWrite(())))
    }

    /// Whether `status` is a redirect (301, 302, 303, 307, 308) on which the
    /// page load's write is skipped.
    ///
    /// Callers use this to drop work, such as pending identity lookups, whose
    /// only purpose is that write.
    #[must_use]
    pub fn is_redirect(status: StatusCode) -> bool {
        matches!(
            status,
            StatusCode::MOVED_PERMANENTLY
                | StatusCode::FOUND
                | StatusCode::SEE_OTHER
                | StatusCode::TEMPORARY_REDIRECT
                | StatusCode::PERMANENT_REDIRECT
        )
    }

    /// Builds a permit without a request, for tests.
    #[cfg(test)]
    #[must_use]
    pub fn for_test() -> Self {
        Self(())
    }
}

/// Proof that the current request may make the page load's single KV write.
#[derive(Debug)]
pub struct PageLoadWrite(());

impl PageLoadWrite {
    /// Consumes the permit. Called only by the write itself.
    pub(crate) fn spend(self) {}
}

/// Proof that the current navigation may make one follow-up update after its
/// response has been sent.
///
/// Issued only by [`NavigationPermit::confirm_with_after_response`] and
/// consumed by [`KvIdentityGraph::commit_after_response`]. It is not `Clone`,
/// so a navigation makes at most one follow-up write.
///
/// [`KvIdentityGraph::commit_after_response`]: super::kv::KvIdentityGraph::commit_after_response
#[derive(Debug)]
pub struct AfterResponseWrite(());

impl AfterResponseWrite {
    /// Consumes the permit. Called only by the write itself.
    pub(crate) fn spend(self) {}

    /// Builds a permit without a navigation, for tests.
    #[cfg(test)]
    #[must_use]
    pub fn for_test() -> Self {
        Self(())
    }
}

/// KV generation a conditional write expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Generation(u64);

impl Generation {
    /// Reads the generation captured in `snapshot` for `ec_id`.
    ///
    /// Returns `None` when the snapshot has no generation, so no blind
    /// overwrite can be planned.
    #[must_use]
    pub fn of(snapshot: &EcKvSnapshot, ec_id: &str) -> Option<Self> {
        snapshot.generation_for(ec_id).map(Self)
    }

    /// The raw generation.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// The single row change of a navigation, with its CAS precondition.
#[derive(Debug)]
pub enum RowChange {
    /// Create a row that must not exist (new EC or orphan recovery).
    Create(KvEntry),
    /// Replace the row only if it is still at `expected`.
    Update {
        /// The full new row.
        entry: KvEntry,
        /// Generation the row must still have.
        expected: Generation,
    },
    /// Replace a live row with a withdrawal tombstone.
    Tombstone {
        /// Generation the row must still have.
        expected: Generation,
    },
}

/// Why the single write did not land.
#[derive(Debug, derive_more::Display)]
pub enum KvWriteError {
    /// The row changed since it was read.
    #[display("row changed since it was read")]
    Conflict,
    /// A create found an existing row.
    #[display("row already exists")]
    AlreadyExists,
    /// The KV store rejected the write.
    #[display("KV store rejected the write")]
    Store,
}

impl core::error::Error for KvWriteError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ec::kv::KvIdentityGraph;
    use crate::ec::kv_types::KvEntry;

    fn request(headers: &[(&str, &str)]) -> http::Request<EdgeBody> {
        let mut builder = http::Request::builder()
            .method(http::Method::GET)
            .uri("https://www.example.com/");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        builder
            .body(EdgeBody::empty())
            .expect("should build request")
    }

    const DOCUMENT: [(&str, &str); 2] = [
        ("sec-fetch-dest", "document"),
        ("sec-fetch-mode", "navigate"),
    ];

    #[test]
    fn classify_accepts_top_level_document_navigation() {
        assert!(
            NavigationPermit::classify(&request(&DOCUMENT)).is_some(),
            "should permit navigation"
        );
    }

    #[test]
    fn classify_rejects_missing_fetch_metadata_iframes_and_prefetch() {
        assert!(
            NavigationPermit::classify(&request(&[])).is_none(),
            "should reject without fetch metadata"
        );
        assert!(
            NavigationPermit::classify(&request(&[
                ("sec-fetch-dest", "iframe"),
                ("sec-fetch-mode", "navigate")
            ]))
            .is_none(),
            "should reject iframe"
        );
        let mut prefetch = DOCUMENT.to_vec();
        prefetch.push(("sec-purpose", "prefetch;prerender"));
        assert!(
            NavigationPermit::classify(&request(&prefetch)).is_none(),
            "should reject prefetch"
        );
    }

    #[test]
    fn confirm_rejects_redirects() {
        assert!(
            NavigationPermit::for_test()
                .confirm(http::StatusCode::FOUND)
                .is_none(),
            "should not write on redirect"
        );
        assert!(
            NavigationPermit::for_test()
                .confirm(http::StatusCode::NOT_FOUND)
                .is_some(),
            "should write on error page"
        );
    }

    #[test]
    fn confirm_keeps_not_modified_and_drops_redirects() {
        assert!(
            NavigationPermit::for_test()
                .confirm(http::StatusCode::NOT_MODIFIED)
                .is_some(),
            "should write on 304 Not Modified"
        );
        for status in [
            http::StatusCode::FOUND,
            http::StatusCode::TEMPORARY_REDIRECT,
        ] {
            assert!(
                NavigationPermit::for_test().confirm(status).is_none(),
                "should not write on redirect {status}"
            );
        }
    }

    #[test]
    fn is_redirect_matches_only_write_skipping_statuses() {
        for status in [
            http::StatusCode::MOVED_PERMANENTLY,
            http::StatusCode::FOUND,
            http::StatusCode::SEE_OTHER,
            http::StatusCode::TEMPORARY_REDIRECT,
            http::StatusCode::PERMANENT_REDIRECT,
        ] {
            assert!(
                NavigationPermit::is_redirect(status),
                "should treat {status} as a redirect"
            );
        }
        for status in [http::StatusCode::OK, http::StatusCode::NOT_MODIFIED] {
            assert!(
                !NavigationPermit::is_redirect(status),
                "should not treat {status} as a redirect"
            );
        }
    }

    fn permit() -> PageLoadWrite {
        NavigationPermit::for_test()
            .confirm(http::StatusCode::OK)
            .expect("should confirm")
    }

    fn live_entry() -> KvEntry {
        let mut entry = KvEntry::tombstone(1000);
        entry.consent.ok = true;
        entry
    }

    #[test]
    fn commit_create_reports_existing_row() {
        let graph = KvIdentityGraph::in_memory("page_load");
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        graph
            .commit_page_load(permit(), &ec_id, RowChange::Create(live_entry()))
            .expect("should create row");

        let error = graph
            .commit_page_load(permit(), &ec_id, RowChange::Create(live_entry()))
            .expect_err("should refuse second create");

        assert!(
            matches!(error.current_context(), KvWriteError::AlreadyExists),
            "should report existing row"
        );
    }

    #[test]
    fn commit_update_with_stale_generation_conflicts_without_retry() {
        let graph = KvIdentityGraph::in_memory("page_load");
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        graph
            .create(&ec_id, &live_entry())
            .expect("should seed row");
        let snapshot = graph.load_snapshot(&ec_id);
        let expected = Generation::of(&snapshot, &ec_id).expect("should read generation");
        graph
            .create_or_revive(&ec_id, &live_entry())
            .expect("should no-op on live row");
        graph
            .commit_page_load(
                permit(),
                &ec_id,
                RowChange::Update {
                    entry: live_entry(),
                    expected,
                },
            )
            .expect("should write at current generation");

        let error = graph
            .commit_page_load(
                permit(),
                &ec_id,
                RowChange::Update {
                    entry: live_entry(),
                    expected,
                },
            )
            .expect_err("should reject stale generation");

        assert!(
            matches!(error.current_context(), KvWriteError::Conflict),
            "should report conflict"
        );
    }

    #[test]
    fn commit_tombstone_marks_consent_withdrawn() {
        let graph = KvIdentityGraph::in_memory("page_load");
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        graph
            .create(&ec_id, &live_entry())
            .expect("should seed row");
        let expected =
            Generation::of(&graph.load_snapshot(&ec_id), &ec_id).expect("should read generation");

        let committed = graph
            .commit_page_load(permit(), &ec_id, RowChange::Tombstone { expected })
            .expect("should write tombstone");

        assert!(!committed.consent.ok, "should return tombstone");
        let (stored, _) = graph
            .get(&ec_id)
            .expect("should read")
            .expect("should exist");
        assert!(!stored.consent.ok, "should persist tombstone");
    }
}
