//! Each contributor's answer, held between requests by the loading cache.
//!
//! A contributor's answer is loaded through
//! [`RuntimeServices::load_cached`], so the contributor is asked once however
//! many requests want its rules at the same moment, and the copy in the host's
//! shared store answers every instance until the contributor's refresh is due.
//!
//! The last answer a contributor gave is also kept apart from the cache, for
//! far longer than any refresh, and read only when the contributor fails. It is
//! then served past its age, because yesterday's rules are a better answer than
//! none. It is kept in the host's value cache when the host supplies one, and
//! in the key-value store otherwise, because a host that starts an instance
//! for each request may give its requests a value cache and no key-value store.
//! A value cache may drop it sooner, so on such a host a copy past its age is
//! likely to be there rather than certain, and a key-value store that cannot
//! give an entry a lifetime keeps it until it is deleted there.

use core::time::Duration;
use std::sync::Arc;

use error_stack::Report;
use sha2::{Digest as _, Sha256};

use super::{Contribution, RobotsTxtContributor};
use crate::error::TrustedServerError;
use crate::module_context::{ModuleContext, ResolvedRequest};
use crate::platform::{
    CacheLifetimes, CacheSpec, KvError, LoadError, LoadedValue, Loader, RuntimeServices,
    ServerKeyPrefix, StoreKey,
};
use crate::settings::Settings;

/// The cache contributors' answers are held in.
pub(crate) const CACHE: &str = crate::settings::ROBOTS_TXT_CACHE;

/// Where each contributor's last answer is kept apart from the cache.
const KEPT: &str = "robots-txt-kept";

/// How long a contributor's last answer is kept, far past any refresh, so it
/// is there to serve when the contributor cannot answer.
const KEPT_LIFETIME: Duration = Duration::from_secs(30 * 24 * 3600);

/// The lifetimes `[cache.robots-txt]` leaves unset. A contributor's own
/// refresh ends each copy sooner than the store lifetime.
const DEFAULT_LIFETIMES: CacheLifetimes = CacheLifetimes {
    memory: Duration::from_secs(60),
    store: Duration::from_secs(7 * 24 * 3600),
    store_idle: Duration::from_secs(30 * 24 * 3600),
};

/// The cache contributors' answers are held in, with the lifetimes
/// `[cache.robots-txt]` sets.
///
/// Its loads run in the caller, because a contributor may block the thread it
/// runs on while it asks its service.
pub(crate) fn cache_spec(settings: &Settings) -> CacheSpec {
    CacheSpec {
        name: CACHE,
        lifetimes: settings.cache.robots_txt.lifetimes(DEFAULT_LIFETIMES),
        memory_entries: 64,
        detachable: false,
    }
}

/// The contribution of `contributor`, selected as `id`, from a copy young
/// enough to serve, and from the contributor otherwise, which is asked for
/// `request`.
///
/// # Errors
///
/// When the contributor fails and no answer of its is kept. A kept answer is
/// returned past its age rather than an error.
pub(crate) async fn contribution_for(
    id: &'static str,
    contributor: &Arc<dyn RobotsTxtContributor>,
    request: &ResolvedRequest,
    services: &RuntimeServices,
    settings: &Settings,
) -> Result<Contribution, Report<TrustedServerError>> {
    let prefix = ServerKeyPrefix::for_settings(settings);
    let held = held_key(id, &contributor.fingerprint());
    let key = StoreKey::new(&prefix, CACHE, held.clone());
    let kept = StoreKey::new(&prefix, KEPT, held);
    let ask = Ask {
        id,
        contributor: Arc::clone(contributor),
        request: request.clone(),
        services: services.clone(),
    };
    let loaded = services
        .load_cached(&cache_spec(settings), &key, load(ask, kept.clone()))
        .await
        .and_then(|bytes| {
            serde_json::from_slice::<Contribution>(&bytes)
                .map_err(|error| LoadError::new(format!("the held answer is unreadable: {error}")))
        });
    match loaded {
        Ok(contribution) => Ok(contribution),
        Err(error) => match kept_answer(services, &kept).await {
            Some(contribution) => {
                log::warn!("Serving the held robots.txt rules of `{id}` past their age: {error}");
                Ok(contribution)
            }
            None => Err(Report::new(TrustedServerError::Integration {
                integration: id.to_owned(),
                message: format!("the robots.txt contributor did not answer: {error}"),
            })),
        },
    }
}

/// What a load owns to ask one contributor, because it may outlive the
/// request that started it.
struct Ask {
    /// The name the contributor was declared under.
    id: &'static str,
    contributor: Arc<dyn RobotsTxtContributor>,
    request: ResolvedRequest,
    services: RuntimeServices,
}

/// Asks the contributor, keeps its answer apart for when it next fails, and
/// gives the answer the contributor's refresh as its lifetime.
fn load(ask: Ask, kept: StoreKey) -> Loader {
    Box::new(move || {
        Box::pin(async move {
            let Ask {
                id,
                contributor,
                request,
                services,
            } = ask;
            let context = ModuleContext::new(request.view()).with_services(&services);
            let contribution = contributor
                .contribute(context.call(id, contributor.required_permissions()))
                .await
                .map_err(|report| LoadError::new(format!("{report:?}")))?;
            let bytes = serde_json::to_vec(&contribution)
                .map_err(|error| LoadError::new(format!("the answer cannot be held: {error}")))?;
            keep_answer(&services, &kept, &bytes).await;
            Ok(LoadedValue::new(bytes).with_lifetime(contributor.refresh()))
        })
    })
}

/// The key one contributor's answer for one fingerprint is held under, a hash
/// so the fingerprint's contents, whatever they are, never appear in a store.
fn held_key(id: &str, fingerprint: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(id.as_bytes());
    hasher.update(b"\n");
    hasher.update(fingerprint.as_bytes());
    let mut key = String::with_capacity(64);
    for byte in hasher.finalize() {
        key.push_str(&format!("{byte:02x}"));
    }
    key
}

/// The answer kept for when the contributor fails, from the host's value
/// cache when it supplies one and from the key-value store otherwise.
///
/// Nothing read, for whatever reason, is no kept answer, and the failure that
/// led here is reported instead.
async fn kept_answer(services: &RuntimeServices, kept: &StoreKey) -> Option<Contribution> {
    let key = kept.to_string();
    let bytes = match services.value_cache() {
        Some(cache) => cache.get(&key).await?,
        None => match services.kv_store().get_bytes(&key).await {
            Ok(bytes) => bytes?,
            Err(error) => {
                log::debug!("No kept robots.txt answer could be read: {error}");
                return None;
            }
        },
    };
    serde_json::from_slice(&bytes)
        .inspect_err(|error| log::debug!("The kept robots.txt answer is unreadable: {error}"))
        .ok()
}

/// Keeps an answer for when the contributor next fails, in the host's value
/// cache when it supplies one and in the key-value store otherwise.
async fn keep_answer(services: &RuntimeServices, kept: &StoreKey, bytes: &[u8]) {
    let key = kept.to_string();
    let bytes = bytes::Bytes::copy_from_slice(bytes);
    if let Some(cache) = services.value_cache() {
        cache.put(&key, bytes, KEPT_LIFETIME).await;
        return;
    }
    let store = services.kv_store();
    let written = match store
        .put_bytes_with_ttl(&key, bytes.clone(), KEPT_LIFETIME)
        .await
    {
        // A store that cannot give an entry a lifetime keeps it until it is
        // replaced, so the answer is still there when the contributor fails.
        Err(KvError::Unsupported { .. }) => store.put_bytes(&key, bytes).await,
        written => written,
    };
    if let Err(error) = written {
        log::debug!("The robots.txt answer was not kept for a later failure: {error}");
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Mutex;

    use edgezero_core::body::Body as EdgeBody;

    use super::*;
    use crate::evidence::RequestInfo;
    use crate::module_context::{ModuleCall, ModuleRequest};
    use crate::platform::test_support::{MemoryLoadingCache, StubHttpClient};

    /// A contributor whose answers a test decides, counting how often it is
    /// asked.
    #[derive(Debug)]
    pub(crate) struct Scripted {
        answers: Mutex<Vec<Result<Contribution, String>>>,
        pub(crate) asked: Mutex<usize>,
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
            .header(http::header::HOST, "www.publisher.example")
            .body(EdgeBody::empty())
            .expect("should build the request")
    }

    /// The request the contributors are asked for, as the handler resolves it.
    pub(crate) fn request() -> ResolvedRequest {
        ResolvedRequest::of(&robots_request(), &crate::platform::ClientInfo::default())
            .without_query()
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

    /// The answer is shared by every crawler, so the contributor is handed
    /// the request it was asked for and its services, and nothing about who
    /// asked, not even the query.
    #[tokio::test]
    async fn the_contributor_is_asked_for_the_request_and_not_who_asked() {
        let echoing: Arc<dyn RobotsTxtContributor> = Arc::new(Echoing);
        let services = services_with(Arc::default(), Arc::default());

        let answer = contribution_for("echo", &echoing, &request(), &services, &settings())
            .await
            .expect("should ask the contributor");

        assert_eq!(answer, rules(ECHOED));
    }

    /// A store held in memory for the life of one test, so that what one
    /// instance wrote is what the next one reads.
    #[derive(Debug, Default)]
    pub(crate) struct MemoryKvStore {
        pub(crate) entries: Mutex<std::collections::HashMap<String, bytes::Bytes>>,
        /// The lifetime each entry was last written with.
        pub(crate) lifetimes: Mutex<std::collections::HashMap<String, Duration>>,
        /// Whether a write with a lifetime is refused, as it is by a store
        /// that has none to give.
        pub(crate) refuses_lifetimes: bool,
    }

    #[async_trait::async_trait(?Send)]
    impl crate::platform::PlatformKvStore for MemoryKvStore {
        async fn get_bytes(
            &self,
            key: &str,
        ) -> Result<Option<bytes::Bytes>, crate::platform::KvError> {
            Ok(self
                .entries
                .lock()
                .expect("should lock the test store")
                .get(key)
                .cloned())
        }

        async fn put_bytes(
            &self,
            key: &str,
            value: bytes::Bytes,
        ) -> Result<(), crate::platform::KvError> {
            self.entries
                .lock()
                .expect("should lock the test store")
                .insert(key.to_owned(), value);
            Ok(())
        }

        async fn put_bytes_with_ttl(
            &self,
            key: &str,
            value: bytes::Bytes,
            ttl: Duration,
        ) -> Result<(), crate::platform::KvError> {
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

        async fn delete(&self, key: &str) -> Result<(), crate::platform::KvError> {
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
        ) -> Result<edgezero_core::key_value_store::KvPage, crate::platform::KvError> {
            Ok(edgezero_core::key_value_store::KvPage::default())
        }
    }

    /// Services keeping answers in `cache` and kept answers in `store`, as one
    /// instance of a host with no value cache.
    pub(crate) fn services_with(
        cache: Arc<MemoryLoadingCache>,
        store: Arc<MemoryKvStore>,
    ) -> RuntimeServices {
        crate::platform::test_support::build_services_with_http_client(Arc::new(
            StubHttpClient::new(),
        ))
        .with_kv_store(store)
        .with_loading_cache(cache)
    }

    pub(crate) fn settings() -> Settings {
        let mut settings = crate::test_support::tests::create_test_settings();
        settings.publisher.domain = "www.publisher.example".to_owned();
        settings
    }

    fn contributor(scripted: &Arc<Scripted>) -> Arc<dyn RobotsTxtContributor> {
        Arc::clone(scripted) as Arc<dyn RobotsTxtContributor>
    }

    #[tokio::test]
    async fn a_contributor_is_asked_once_and_its_answer_is_held_after_that() {
        let scripted = Arc::new(Scripted::new(vec![Ok(rules("User-Agent: *\nAllow: /"))]));
        let cache = Arc::new(MemoryLoadingCache::default());
        let services = services_with(Arc::clone(&cache), Arc::default());

        let first = contribution_for(
            "a",
            &contributor(&scripted),
            &request(),
            &services,
            &settings(),
        )
        .await
        .expect("should ask the contributor");
        let second = contribution_for(
            "a",
            &contributor(&scripted),
            &request(),
            &services,
            &settings(),
        )
        .await
        .expect("should serve the held copy");

        assert_eq!(first, second);
        assert_eq!(scripted.times_asked(), 1);
        assert!(
            cache.calls().iter().all(|(name, _)| *name == CACHE),
            "every call should be to the robots.txt cache"
        );
    }

    #[tokio::test]
    async fn the_held_answer_is_keyed_by_the_publisher() {
        let scripted = Arc::new(Scripted::new(vec![
            Ok(rules("User-Agent: OneBot\nDisallow: /")),
            Ok(rules("User-Agent: OtherBot\nDisallow: /")),
        ]));
        let cache = Arc::new(MemoryLoadingCache::default());
        let services = services_with(Arc::clone(&cache), Arc::default());
        let mut other = settings();
        other.publisher.domain = "other.example".to_owned();

        contribution_for(
            "a",
            &contributor(&scripted),
            &request(),
            &services,
            &settings(),
        )
        .await
        .expect("should answer the first publisher");
        let answer = contribution_for("a", &contributor(&scripted), &request(), &services, &other)
            .await
            .expect("should answer the second publisher");

        assert_eq!(answer, rules("User-Agent: OtherBot\nDisallow: /"));
        assert_eq!(scripted.times_asked(), 2, "each publisher is asked for");
        let keys = cache.keys();
        assert!(
            keys[0].starts_with("ts.other.example/robots-txt/"),
            "{keys:?}"
        );
        assert!(
            keys[1].starts_with("ts.publisher.example/robots-txt/"),
            "{keys:?}"
        );
    }

    #[tokio::test]
    async fn changed_settings_are_not_answered_from_the_old_copy() {
        let scripted = Arc::new(Scripted::new(vec![
            Ok(rules("User-Agent: FirstBot\nDisallow: /")),
            Ok(rules("User-Agent: SecondBot\nDisallow: /")),
        ]));
        let services = services_with(Arc::default(), Arc::default());

        contribution_for(
            "a",
            &contributor(&scripted),
            &request(),
            &services,
            &settings(),
        )
        .await
        .expect("should ask for the first settings");
        *scripted.fingerprint.lock().expect("should lock") = "second settings".to_owned();
        let answer = contribution_for(
            "a",
            &contributor(&scripted),
            &request(),
            &services,
            &settings(),
        )
        .await
        .expect("should ask again for the new settings");

        assert_eq!(answer, rules("User-Agent: SecondBot\nDisallow: /"));
    }

    #[tokio::test]
    async fn the_kept_answer_is_served_when_the_contributor_fails() {
        let mut scripted = Scripted::new(vec![
            Ok(rules("User-Agent: HeldBot\nDisallow: /")),
            Err("down".to_owned()),
        ]);
        // A refresh of zero gives each answer no time in the cache, so the
        // second request asks again on any machine at any moment.
        scripted.refresh = Duration::ZERO;
        let scripted = Arc::new(scripted);
        let services = services_with(Arc::default(), Arc::default());

        contribution_for(
            "a",
            &contributor(&scripted),
            &request(),
            &services,
            &settings(),
        )
        .await
        .expect("should ask the contributor");
        let answer = contribution_for(
            "a",
            &contributor(&scripted),
            &request(),
            &services,
            &settings(),
        )
        .await
        .expect("should serve yesterday's rules");

        assert_eq!(answer, rules("User-Agent: HeldBot\nDisallow: /"));
        assert_eq!(scripted.times_asked(), 2, "should have asked again first");
    }

    #[tokio::test]
    async fn the_kept_answer_outlives_the_instance_that_kept_it() {
        let scripted = Arc::new(Scripted::new(vec![
            Ok(rules("User-Agent: KeptBot\nDisallow: /")),
            Err("down".to_owned()),
        ]));
        let store = Arc::new(MemoryKvStore::default());

        contribution_for(
            "a",
            &contributor(&scripted),
            &request(),
            &services_with(Arc::default(), Arc::clone(&store)),
            &settings(),
        )
        .await
        .expect("should ask the contributor");
        // A second instance, with a cache of its own and the same store.
        let answer = contribution_for(
            "a",
            &contributor(&scripted),
            &request(),
            &services_with(Arc::default(), Arc::clone(&store)),
            &settings(),
        )
        .await
        .expect("should serve the kept answer");

        assert_eq!(answer, rules("User-Agent: KeptBot\nDisallow: /"));
        assert_eq!(scripted.times_asked(), 2);
        let keys: Vec<String> = store
            .entries
            .lock()
            .expect("should lock the test store")
            .keys()
            .cloned()
            .collect();
        assert_eq!(keys.len(), 1, "{keys:?}");
        assert!(
            keys[0].starts_with("ts.publisher.example/robots-txt-kept/"),
            "{keys:?}"
        );
        let lifetimes = store.lifetimes.lock().expect("should lock the test store");
        assert_eq!(
            lifetimes.values().copied().collect::<Vec<_>>(),
            vec![KEPT_LIFETIME],
            "the answer should be kept far longer than any refresh"
        );
    }

    /// A store with no lifetime to give still keeps the answer, so it is
    /// there to serve when the contributor fails.
    #[tokio::test]
    async fn a_store_without_lifetimes_still_keeps_the_answer() {
        let mut scripted = Scripted::new(vec![
            Ok(rules("User-Agent: OneBot\nDisallow: /")),
            Err("cannot answer".to_owned()),
        ]);
        scripted.refresh = Duration::ZERO;
        let scripted = Arc::new(scripted);
        let store = Arc::new(MemoryKvStore {
            refuses_lifetimes: true,
            ..MemoryKvStore::default()
        });
        let services = services_with(Arc::default(), Arc::clone(&store));

        contribution_for(
            "a",
            &contributor(&scripted),
            &request(),
            &services,
            &settings(),
        )
        .await
        .expect("should ask the contributor");
        let later = contribution_for(
            "a",
            &contributor(&scripted),
            &request(),
            &services,
            &settings(),
        )
        .await
        .expect("should serve the kept answer");

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

    #[tokio::test]
    async fn with_nothing_kept_a_failure_is_an_error_and_is_not_held() {
        let scripted = Arc::new(Scripted::new(vec![
            Err("down".to_owned()),
            Ok(rules("User-Agent: LateBot\nDisallow: /")),
        ]));
        let services = services_with(Arc::default(), Arc::default());

        assert!(
            contribution_for(
                "a",
                &contributor(&scripted),
                &request(),
                &services,
                &settings()
            )
            .await
            .is_err()
        );
        let answer = contribution_for(
            "a",
            &contributor(&scripted),
            &request(),
            &services,
            &settings(),
        )
        .await
        .expect("the next request should ask again");

        assert_eq!(answer, rules("User-Agent: LateBot\nDisallow: /"));
        assert_eq!(scripted.times_asked(), 2);
    }

    /// A value cache held in memory for the life of one test.
    #[derive(Debug, Default)]
    struct MemoryValueCache {
        entries: Mutex<std::collections::HashMap<String, bytes::Bytes>>,
    }

    #[async_trait::async_trait(?Send)]
    impl crate::platform::PlatformValueCache for MemoryValueCache {
        async fn get(&self, key: &str) -> Option<bytes::Bytes> {
            self.entries
                .lock()
                .expect("should lock the test cache")
                .get(key)
                .cloned()
        }

        async fn put(&self, key: &str, value: bytes::Bytes, _lifetime: Duration) {
            self.entries
                .lock()
                .expect("should lock the test cache")
                .insert(key.to_owned(), value);
        }
    }

    /// A host that gives its requests a value cache and no key-value store
    /// keeps the answer in the value cache, and serves it from there when the
    /// contributor fails.
    #[tokio::test]
    async fn the_kept_answer_is_in_the_value_cache_where_the_host_has_one() {
        let mut scripted = Scripted::new(vec![
            Ok(rules("User-Agent: CachedBot\nDisallow: /")),
            Err("down".to_owned()),
        ]);
        scripted.refresh = Duration::ZERO;
        let scripted = Arc::new(scripted);
        let value_cache = Arc::new(MemoryValueCache::default());
        let services = crate::platform::RuntimeServices::builder()
            .config_store(Arc::new(crate::platform::test_support::NoopConfigStore))
            .secret_store(Arc::new(crate::platform::test_support::NoopSecretStore))
            .kv_store(Arc::new(crate::platform::UnavailableKvStore))
            .backend(Arc::new(crate::platform::test_support::NoopBackend))
            .http_client(Arc::new(StubHttpClient::new()))
            .geo(Arc::new(crate::platform::test_support::NoopGeo))
            .value_cache(Arc::clone(&value_cache) as Arc<dyn crate::platform::PlatformValueCache>)
            .loading_caches(Arc::new(crate::platform::LoadingCaches::every(Arc::new(
                MemoryLoadingCache::default(),
            ))))
            .client_info(crate::platform::ClientInfo::default())
            .build();

        contribution_for(
            "a",
            &contributor(&scripted),
            &request(),
            &services,
            &settings(),
        )
        .await
        .expect("should ask the contributor");
        let answer = contribution_for(
            "a",
            &contributor(&scripted),
            &request(),
            &services,
            &settings(),
        )
        .await
        .expect("should serve the kept answer from the value cache");

        assert_eq!(answer, rules("User-Agent: CachedBot\nDisallow: /"));
        assert_eq!(
            value_cache
                .entries
                .lock()
                .expect("should lock the test cache")
                .len(),
            1,
            "the answer should be kept in the value cache"
        );
    }

    #[test]
    fn the_key_carries_nothing_of_the_fingerprint() {
        let key = held_key("a", "license=secret-value");

        assert_eq!(key.len(), 64);
        assert!(!key.contains("secret"), "{key}");
        assert_ne!(key, held_key("b", "license=secret-value"));
    }
}
