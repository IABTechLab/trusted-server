//! Each contributor's answer, held in the key-value store between requests.
//!
//! A contributor is asked when nothing of its is held, or when what is held
//! is older than the contributor's own refresh. Its answer is then stored and
//! serves every request until the refresh is due again.
//!
//! What is held is kept far longer than any refresh, and is served past its
//! age when the contributor fails, because yesterday's rules are a better
//! answer than none.
//!
//! An answer is held for the host the contributor was asked for, as well as
//! for the publisher and the contributor's settings, so one host's answer is
//! never served for another.
//!
//! A store that cannot give an entry a lifetime is written without one. An
//! answer held for settings that have since changed then stays in that store,
//! one small entry for each change, until it is deleted there.
//!
//! A deployment with no key-value store holds nothing. Every request for the
//! file then asks each contributor, and a contributor that fails fails the
//! file.

use core::time::Duration;
use std::sync::Arc;

use bytes::Bytes;
use error_stack::Report;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::{Contribution, RobotsTxtContributor};
use crate::error::TrustedServerError;
use crate::module_context::{ModuleContext, ResolvedRequest};
use crate::platform::{KvError, RuntimeServices};

/// The start of every key an answer is held under, which nothing else that
/// writes the store uses.
const KEY_PREFIX: &str = "robots-txt:";

/// How long an answer is kept, far past any refresh, so it is there to serve
/// when the contributor cannot answer.
const KEPT_LIFETIME: Duration = Duration::from_secs(30 * 24 * 3600);

/// One contributor's answer as it is held.
#[derive(Debug, Serialize, Deserialize)]
struct Held {
    /// When the contributor gave it, in seconds since the Unix epoch.
    given_at: u64,
    contribution: Contribution,
}

/// Who an answer is for and who gave it, which with the host the contributor
/// is asked for name where it is held.
pub(crate) struct Holder<'a> {
    /// The publisher the file is for. One store may serve several, and one
    /// publisher's rules are never another's.
    pub(crate) publisher: &'a str,
    /// The name `[robots-txt] modules` selects the contributor by.
    pub(crate) id: &'static str,
}

/// The contribution of `contributor` from a held answer young enough to
/// serve, and from the contributor otherwise, which is asked for `request`.
///
/// `now` is the time in seconds since the Unix epoch.
///
/// # Errors
///
/// When the contributor fails and no answer of its is held. A held answer is
/// returned past its age rather than an error.
pub(crate) async fn contribution_for(
    holder: &Holder<'_>,
    contributor: &Arc<dyn RobotsTxtContributor>,
    request: &ResolvedRequest,
    services: &RuntimeServices,
    now: u64,
) -> Result<Contribution, Report<TrustedServerError>> {
    let key = held_key(holder, request.view().host(), &contributor.fingerprint());
    let held = held_answer(services, &key).await;
    if let Some(held) = &held
        && now.saturating_sub(held.given_at) < contributor.refresh().as_secs()
    {
        return Ok(held.contribution.clone());
    }
    // The answer is served to every crawler, so the contributor's services
    // carry nothing of the connection that asked.
    let services_handed = services.clone().without_connection();
    let context = ModuleContext::new(request.view()).with_services(&services_handed);
    let asked = contributor
        .contribute(context.call(holder.id, contributor.required_permissions()))
        .await;
    match asked {
        Ok(contribution) => {
            let answer = Held {
                given_at: now,
                contribution,
            };
            hold_answer(services, &key, &answer).await;
            Ok(answer.contribution)
        }
        Err(report) => match held {
            Some(held) => {
                log::warn!(
                    "Serving the held robots.txt rules of `{}` past their age: {report:?}",
                    holder.id
                );
                Ok(held.contribution)
            }
            None => Err(report.change_context(TrustedServerError::Integration {
                integration: holder.id.to_owned(),
                message: "the robots.txt contributor did not answer and none of its answers \
                          is held"
                    .to_owned(),
            })),
        },
    }
}

/// The key one contributor's answer for one publisher, one host and one
/// fingerprint is held under. A hash, so that the fingerprint's contents,
/// whatever they are, never appear in the store.
fn held_key(holder: &Holder<'_>, host: &str, fingerprint: &str) -> String {
    let mut hasher = Sha256::new();
    for part in [holder.publisher, host, holder.id, fingerprint] {
        // Each part with its length, so that no two different sets of parts
        // run together into the same bytes.
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    let mut key = String::with_capacity(KEY_PREFIX.len() + 64);
    key.push_str(KEY_PREFIX);
    for byte in hasher.finalize() {
        key.push_str(&format!("{byte:02x}"));
    }
    key
}

/// The answer held under `key`.
///
/// Nothing read, for whatever reason, is no held answer, and the contributor
/// is asked.
async fn held_answer(services: &RuntimeServices, key: &str) -> Option<Held> {
    let bytes = match services.kv_store().get_bytes(key).await {
        Ok(bytes) => bytes?,
        Err(error) => {
            log::debug!("No held robots.txt answer could be read: {error}");
            return None;
        }
    };
    serde_json::from_slice(&bytes)
        .inspect_err(|error| log::debug!("The held robots.txt answer is unreadable: {error}"))
        .ok()
}

/// Holds `answer` under `key`. An answer that cannot be held is still served,
/// so a failure here is logged and nothing more.
///
/// A store that cannot give an entry a lifetime is written without one, so
/// the answer is still there to serve when the contributor fails.
async fn hold_answer(services: &RuntimeServices, key: &str, answer: &Held) {
    let bytes = match serde_json::to_vec(answer) {
        Ok(bytes) => Bytes::from(bytes),
        Err(error) => {
            log::debug!("The robots.txt answer cannot be held: {error}");
            return;
        }
    };
    let store = services.kv_store();
    let written = match store
        .put_bytes_with_ttl(key, bytes.clone(), KEPT_LIFETIME)
        .await
    {
        Err(KvError::Unsupported { .. }) => store.put_bytes(key, bytes).await,
        written => written,
    };
    if let Err(error) = written {
        log::debug!("The robots.txt answer was not held: {error}");
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use edgezero_core::body::Body as EdgeBody;
    use edgezero_core::key_value_store::{KvError, KvPage, KvStore as PlatformKvStore};

    use super::*;
    use crate::evidence::RequestInfo;
    use crate::module_context::{ModuleCall, ModuleRequest};
    use crate::platform::ClientInfo;
    use crate::platform::test_support::noop_services;

    /// The publisher the tests hold answers for.
    pub(crate) const PUBLISHER: &str = "www.publisher.example";

    /// A time to ask at, in seconds since the Unix epoch.
    pub(crate) const NOW: u64 = 1_800_000_000;

    /// A contributor whose answers a test decides, counting how often it is
    /// asked.
    #[derive(Debug)]
    pub(crate) struct Scripted {
        answers: Mutex<Vec<Result<Contribution, String>>>,
        asked: Mutex<usize>,
        pub(crate) refresh: Duration,
        pub(crate) fingerprint: Mutex<String>,
    }

    impl Scripted {
        pub(crate) fn new(answers: Vec<Result<Contribution, String>>) -> Self {
            Self {
                answers: Mutex::new(answers),
                asked: Mutex::new(0),
                refresh: Duration::from_secs(3600),
                fingerprint: Mutex::new("first settings".to_owned()),
            }
        }

        pub(crate) fn times_asked(&self) -> usize {
            *self.asked.lock().expect("should lock the count")
        }
    }

    #[async_trait::async_trait(?Send)]
    impl RobotsTxtContributor for Scripted {
        fn refresh(&self) -> Duration {
            self.refresh
        }

        fn fingerprint(&self) -> String {
            self.fingerprint.lock().expect("should lock").clone()
        }

        async fn contribute(
            &self,
            _call: ModuleCall<'_>,
        ) -> Result<Contribution, Report<TrustedServerError>> {
            *self.asked.lock().expect("should lock the count") += 1;
            let next = self
                .answers
                .lock()
                .expect("should lock the answers")
                .remove(0);
            next.map_err(|message| Report::new(TrustedServerError::Proxy { message }))
        }
    }

    pub(crate) fn rules(text: &str) -> Contribution {
        Contribution::parse(text)
    }

    /// A request for `/robots.txt` as a crawler makes it, with a query of its
    /// own.
    pub(crate) fn robots_request() -> http::Request<EdgeBody> {
        http::Request::builder()
            .uri("https://www.publisher.example/robots.txt?crawler=one")
            .header(http::header::HOST, PUBLISHER)
            .body(EdgeBody::empty())
            .expect("should build the request")
    }

    /// The request the contributors are asked for, as the handler resolves it.
    pub(crate) fn request() -> ResolvedRequest {
        ResolvedRequest::of(&robots_request(), &ClientInfo::default()).without_query()
    }

    /// A contributor whose rules name what it was handed, so a test can read
    /// back the request, whether the reader's evidence reached it, and
    /// whether the services did.
    #[derive(Debug)]
    pub(crate) struct Echoing;

    impl Echoing {
        fn rules(
            &self,
            request: ModuleRequest<'_>,
            evidence: Option<&dyn RequestInfo>,
            services: Option<&RuntimeServices>,
        ) -> Contribution {
            rules(&format!(
                "User-Agent: EchoBot\nDisallow: /{}{}?{}/{}/{}",
                request.host(),
                request.path(),
                request.query(),
                if evidence.is_some() {
                    "evidence"
                } else {
                    "no-evidence"
                },
                if services.is_some() {
                    "services"
                } else {
                    "no-services"
                },
            ))
        }
    }

    #[async_trait::async_trait(?Send)]
    impl RobotsTxtContributor for Echoing {
        fn refresh(&self) -> Duration {
            Duration::from_secs(60)
        }

        fn fingerprint(&self) -> String {
            "echo".to_owned()
        }

        async fn contribute(
            &self,
            call: ModuleCall<'_>,
        ) -> Result<Contribution, Report<TrustedServerError>> {
            Ok(call.inject(self, Self::rules)?)
        }
    }

    /// What [`Echoing`] answers for [`request`].
    pub(crate) const ECHOED: &str =
        "User-Agent: EchoBot\nDisallow: /www.publisher.example/robots.txt?/no-evidence/services";

    /// A store held in memory for the life of one test, so that what one
    /// request wrote is what the next one reads.
    #[derive(Debug, Default)]
    pub(crate) struct MemoryKvStore {
        pub(crate) entries: Mutex<HashMap<String, Bytes>>,
        /// The lifetime each entry was last written with.
        pub(crate) lifetimes: Mutex<HashMap<String, Duration>>,
        /// Whether a write with a lifetime is refused, as it is by a store
        /// that has none to give.
        pub(crate) refuses_lifetimes: bool,
    }

    #[async_trait::async_trait(?Send)]
    impl PlatformKvStore for MemoryKvStore {
        async fn get_bytes(&self, key: &str) -> Result<Option<Bytes>, KvError> {
            Ok(self
                .entries
                .lock()
                .expect("should lock the test store")
                .get(key)
                .cloned())
        }

        async fn put_bytes(&self, key: &str, value: Bytes) -> Result<(), KvError> {
            self.entries
                .lock()
                .expect("should lock the test store")
                .insert(key.to_owned(), value);
            Ok(())
        }

        async fn put_bytes_with_ttl(
            &self,
            key: &str,
            value: Bytes,
            ttl: Duration,
        ) -> Result<(), KvError> {
            if self.refuses_lifetimes {
                return Err(KvError::Unsupported {
                    operation: "put_bytes_with_ttl".to_owned(),
                });
            }
            self.lifetimes
                .lock()
                .expect("should lock the test store")
                .insert(key.to_owned(), ttl);
            self.put_bytes(key, value).await
        }

        async fn delete(&self, key: &str) -> Result<(), KvError> {
            self.entries
                .lock()
                .expect("should lock the test store")
                .remove(key);
            Ok(())
        }

        async fn list_keys_page(
            &self,
            _prefix: &str,
            _cursor: Option<&str>,
            _limit: usize,
        ) -> Result<KvPage, KvError> {
            Ok(KvPage::default())
        }
    }

    /// Services that hold answers in `store`.
    pub(crate) fn services_with(store: Arc<MemoryKvStore>) -> RuntimeServices {
        noop_services().with_kv_store(store)
    }

    fn contributor(scripted: &Arc<Scripted>) -> Arc<dyn RobotsTxtContributor> {
        Arc::clone(scripted) as Arc<dyn RobotsTxtContributor>
    }

    fn holder(id: &'static str) -> Holder<'static> {
        Holder {
            publisher: PUBLISHER,
            id,
        }
    }

    async fn ask(
        scripted: &Arc<Scripted>,
        services: &RuntimeServices,
        now: u64,
    ) -> Result<Contribution, Report<TrustedServerError>> {
        contribution_for(
            &holder("a"),
            &contributor(scripted),
            &request(),
            services,
            now,
        )
        .await
    }

    /// The answer is shared by every crawler, so the contributor is handed
    /// the request it was asked for and its services, and nothing about who
    /// asked, not even the query.
    #[tokio::test]
    async fn the_contributor_is_asked_for_the_request_and_not_who_asked() {
        let echoing: Arc<dyn RobotsTxtContributor> = Arc::new(Echoing);
        let services = services_with(Arc::default());

        let answer = contribution_for(&holder("echo"), &echoing, &request(), &services, NOW)
            .await
            .expect("should ask the contributor");

        assert_eq!(answer, rules(ECHOED));
    }

    #[tokio::test]
    async fn a_contributor_is_asked_once_and_its_answer_is_held_after_that() {
        let scripted = Arc::new(Scripted::new(vec![Ok(rules("User-Agent: *\nAllow: /"))]));
        let services = services_with(Arc::default());

        let first = ask(&scripted, &services, NOW)
            .await
            .expect("should ask the contributor");
        let second = ask(&scripted, &services, NOW + 3599)
            .await
            .expect("should serve the held answer");

        assert_eq!(first, second);
        assert_eq!(scripted.times_asked(), 1);
    }

    #[tokio::test]
    async fn a_contributor_is_asked_again_when_its_refresh_is_due() {
        let scripted = Arc::new(Scripted::new(vec![
            Ok(rules("User-Agent: OneBot\nDisallow: /")),
            Ok(rules("User-Agent: TwoBot\nDisallow: /")),
        ]));
        let services = services_with(Arc::default());

        ask(&scripted, &services, NOW)
            .await
            .expect("should ask the contributor");
        let later = ask(&scripted, &services, NOW + 3600)
            .await
            .expect("should ask the contributor again");

        assert_eq!(later, rules("User-Agent: TwoBot\nDisallow: /"));
        assert_eq!(scripted.times_asked(), 2);
    }

    /// Yesterday's rules are a better answer than none.
    #[tokio::test]
    async fn a_held_answer_is_served_past_its_age_when_the_contributor_fails() {
        let scripted = Arc::new(Scripted::new(vec![
            Ok(rules("User-Agent: OneBot\nDisallow: /")),
            Err("cannot answer".to_owned()),
        ]));
        let services = services_with(Arc::default());

        ask(&scripted, &services, NOW)
            .await
            .expect("should ask the contributor");
        let later = ask(&scripted, &services, NOW + 7 * 24 * 3600)
            .await
            .expect("should serve the held answer past its age");

        assert_eq!(later, rules("User-Agent: OneBot\nDisallow: /"));
        assert_eq!(scripted.times_asked(), 2, "should have asked first");
    }

    #[tokio::test]
    async fn with_nothing_held_a_failure_is_an_error() {
        let scripted = Arc::new(Scripted::new(vec![Err("cannot answer".to_owned())]));
        let services = services_with(Arc::default());

        let error = ask(&scripted, &services, NOW)
            .await
            .expect_err("there is nothing to serve");

        assert!(
            format!("{error:?}").contains("cannot answer"),
            "should keep what the contributor said: {error:?}"
        );
    }

    /// An answer given for other settings is never served for these.
    #[tokio::test]
    async fn an_answer_is_held_for_the_fingerprint_it_was_given_for() {
        let scripted = Arc::new(Scripted::new(vec![
            Ok(rules("User-Agent: OneBot\nDisallow: /")),
            Ok(rules("User-Agent: TwoBot\nDisallow: /")),
        ]));
        let services = services_with(Arc::default());

        ask(&scripted, &services, NOW)
            .await
            .expect("should ask the contributor");
        *scripted.fingerprint.lock().expect("should lock") = "second settings".to_owned();
        let after = ask(&scripted, &services, NOW + 1)
            .await
            .expect("should ask for the new settings");

        assert_eq!(after, rules("User-Agent: TwoBot\nDisallow: /"));
    }

    /// One store may serve several publishers, and one publisher's rules are
    /// never another's.
    #[tokio::test]
    async fn an_answer_is_held_for_the_publisher_it_was_given_for() {
        let scripted = Arc::new(Scripted::new(vec![
            Ok(rules("User-Agent: OneBot\nDisallow: /")),
            Ok(rules("User-Agent: TwoBot\nDisallow: /")),
        ]));
        let services = services_with(Arc::default());
        let other = Holder {
            publisher: "www.another.example",
            id: "a",
        };

        ask(&scripted, &services, NOW)
            .await
            .expect("should ask the contributor");
        let for_other = contribution_for(
            &other,
            &contributor(&scripted),
            &request(),
            &services,
            NOW + 1,
        )
        .await
        .expect("should ask for the other publisher");

        assert_eq!(for_other, rules("User-Agent: TwoBot\nDisallow: /"));
    }

    #[test]
    fn a_key_holds_nothing_of_what_it_was_made_from() {
        let key = held_key(
            &holder("a_contributor"),
            "crawled.example",
            "a secret an implementer left in",
        );

        assert!(key.starts_with(KEY_PREFIX), "{key}");
        assert_eq!(key.len(), KEY_PREFIX.len() + 64, "{key}");
        for part in [PUBLISHER, "crawled.example", "a_contributor", "secret"] {
            assert!(!key.contains(part), "{key} should not hold {part}");
        }
    }

    #[test]
    fn parts_that_run_together_do_not_share_a_key() {
        let key = |publisher, host, id| held_key(&Holder { publisher, id }, host, "d");

        assert_ne!(key("ab", "h", "c"), key("a", "bh", "c"));
        assert_ne!(key("p", "ab", "c"), key("p", "a", "bc"));
        assert_ne!(key("p", "h", "c"), key("p", "other.example", "c"));
    }

    /// One deployment may serve several hosts, and each is answered for
    /// itself.
    #[tokio::test]
    async fn an_answer_is_held_for_the_host_it_was_asked_for() {
        let scripted = Arc::new(Scripted::new(vec![
            Ok(rules("User-Agent: OneBot\nDisallow: /")),
            Ok(rules("User-Agent: TwoBot\nDisallow: /")),
        ]));
        let services = services_with(Arc::default());
        let other_host = http::Request::builder()
            .uri("/robots.txt")
            .header(http::header::HOST, "other.publisher.example")
            .body(EdgeBody::empty())
            .expect("should build the request");

        ask(&scripted, &services, NOW)
            .await
            .expect("should ask the contributor");
        let for_other_host = contribution_for(
            &holder("a"),
            &contributor(&scripted),
            &ResolvedRequest::of(&other_host, &ClientInfo::default()),
            &services,
            NOW + 1,
        )
        .await
        .expect("should ask for the other host");

        assert_eq!(for_other_host, rules("User-Agent: TwoBot\nDisallow: /"));
    }

    #[tokio::test]
    async fn an_answer_is_kept_far_longer_than_any_refresh() {
        let scripted = Arc::new(Scripted::new(vec![Ok(rules("User-Agent: *\nAllow: /"))]));
        let store = Arc::new(MemoryKvStore::default());
        let services = services_with(Arc::clone(&store));

        ask(&scripted, &services, NOW)
            .await
            .expect("should ask the contributor");

        let lifetimes = store.lifetimes.lock().expect("should lock the test store");
        assert_eq!(
            lifetimes.values().copied().collect::<Vec<_>>(),
            vec![KEPT_LIFETIME]
        );
    }

    /// A store with no lifetime to give still holds the answer, so it is
    /// there to serve when the contributor fails.
    #[tokio::test]
    async fn a_store_without_lifetimes_still_holds_the_answer() {
        let scripted = Arc::new(Scripted::new(vec![
            Ok(rules("User-Agent: OneBot\nDisallow: /")),
            Err("cannot answer".to_owned()),
        ]));
        let store = Arc::new(MemoryKvStore {
            refuses_lifetimes: true,
            ..MemoryKvStore::default()
        });
        let services = services_with(Arc::clone(&store));

        ask(&scripted, &services, NOW)
            .await
            .expect("should ask the contributor");
        let later = ask(&scripted, &services, NOW + 3600)
            .await
            .expect("should serve the held answer");

        assert_eq!(later, rules("User-Agent: OneBot\nDisallow: /"));
        assert!(
            store
                .lifetimes
                .lock()
                .expect("should lock the test store")
                .is_empty(),
            "the entry was written without a lifetime"
        );
    }

    /// A deployment with no key-value store still serves the file, by asking
    /// every time.
    #[tokio::test]
    async fn with_no_store_the_contributor_is_asked_every_time() {
        let scripted = Arc::new(Scripted::new(vec![
            Ok(rules("User-Agent: *\nAllow: /")),
            Ok(rules("User-Agent: *\nAllow: /")),
        ]));
        let services = noop_services();

        ask(&scripted, &services, NOW)
            .await
            .expect("should ask the contributor");
        ask(&scripted, &services, NOW + 1)
            .await
            .expect("should ask the contributor again");

        assert_eq!(scripted.times_asked(), 2);
    }
}
