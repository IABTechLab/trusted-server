//! A cache that loads a missing value once, however many callers ask for it
//! at the same time.
//!
//! The first caller for a key that no layer holds runs the [`Loader`]. Every
//! caller that asks for the key while it runs waits for that load, and all of
//! them continue with its value or its error. A failed load is not stored, so
//! the next caller loads again.
//!
//! Core declares the interface and [`NoLoadingCache`], which holds nothing. A
//! module outside core supplies the implementation through
//! [`IntegrationBuilder::with_loading_cache`](crate::integrations::IntegrationBuilder::with_loading_cache),
//! keeping a brief copy in memory over the host's shared store, being its
//! value cache where it has one and its key-value store otherwise.
//!
//! Every key is a [`StoreKey`], which starts with the publisher's
//! [`ServerKeyPrefix`], so two publishers sharing a store never share an
//! entry.

use core::fmt;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;

use super::{RuntimeServices, ServerKeyPrefix};

/// A value a [`Loader`] produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedValue {
    /// The value.
    pub bytes: Bytes,
    /// How long the value may be used from now, or `None` for as long as the
    /// cache's own lifetimes allow. Every copy of it ends by then.
    pub lifetime: Option<Duration>,
}

impl LoadedValue {
    /// A value with no lifetime of its own.
    #[must_use]
    pub fn new(bytes: impl Into<Bytes>) -> Self {
        Self {
            bytes: bytes.into(),
            lifetime: None,
        }
    }

    /// Sets how long the value may be used from now.
    #[must_use]
    pub fn with_lifetime(mut self, lifetime: Duration) -> Self {
        self.lifetime = Some(lifetime);
        self
    }
}

/// Why a load produced no value.
///
/// Every caller waiting on the load receives it, and nothing is stored, so the
/// next caller loads again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadError {
    message: std::sync::Arc<str>,
}

impl LoadError {
    /// An error saying `message`.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into().into(),
        }
    }

    /// What went wrong.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl core::error::Error for LoadError {}

/// The work a [`Loader`] does.
///
/// Not `Send`, because the platform's own futures are not. It is made on the
/// thread that runs it.
pub type LoadFuture = Pin<Box<dyn Future<Output = Result<LoadedValue, LoadError>>>>;

/// What a cache runs when no layer holds a key.
///
/// Its value serves every caller waiting on the load and every later reader
/// of the copies kept, so it uses nothing of the reader whose request started
/// it. Services it needs come from [`RuntimeServices::without_connection`],
/// never the request's own.
///
/// It runs in the caller that leads the load. It owns everything it uses,
/// and is `Send`, because a cache may keep it with the key it loads, in
/// memory every thread of the process can reach.
pub type Loader = Box<dyn FnOnce() -> LoadFuture + Send>;

/// How long one cache's copies last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheLifetimes {
    /// How long the brief copy in memory is used.
    pub memory: Duration,
    /// How long the copy in the shared store is used, at most. A value with a
    /// shorter lifetime of its own ends sooner.
    pub store: Duration,
    /// How long the copy in the shared store lasts unused.
    pub store_idle: Duration,
}

/// One cache: its name, the lifetimes its copies get, and how many values
/// its memory copy holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheSpec {
    /// The cache's name, which is part of every key it writes.
    pub name: &'static str,
    /// How long its copies last.
    pub lifetimes: CacheLifetimes,
    /// The most values the brief copy in memory holds.
    pub memory_entries: usize,
}

/// A key in a shared store, being the publisher's prefix and the cache's name
/// as a namespace, and the caller's own key below it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreKey {
    namespace: String,
    key: String,
}

impl StoreKey {
    /// The key `key` in the cache `cache`, for the publisher `prefix` names.
    ///
    /// The caller's key should be short, such as a digest, because a store
    /// refuses a long key.
    #[must_use]
    pub fn new(prefix: &ServerKeyPrefix, cache: &str, key: impl Into<String>) -> Self {
        Self {
            namespace: format!("{}/{cache}", prefix.as_str()),
            key: key.into(),
        }
    }

    /// The publisher's prefix and the cache's name, as `<prefix>/<cache>`.
    #[must_use]
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// The caller's own key.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }
}

impl fmt::Display for StoreKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.namespace, self.key)
    }
}

/// A cache that loads a missing value once per key at a time.
///
/// `Send + Sync` on the trait and `?Send` on the futures, as for the other
/// platform services, because [`RuntimeServices`] crosses threads while the
/// platform layer's futures never do.
#[async_trait::async_trait(?Send)]
pub trait PlatformLoadingCache: Send + Sync {
    /// The value for `key` in the cache `spec` describes, from a copy that is
    /// still fresh or from `load`.
    ///
    /// `load` runs at most once per key at a time, and a caller arriving while
    /// it runs waits for it. The copies are kept in the stores `services`
    /// supplies. [`RuntimeServices::load_cached`] hands over services that
    /// carry nothing of the reader, because the load serves every waiter.
    ///
    /// # Errors
    ///
    /// The error of the load this caller waited on, which every caller waiting
    /// on that load receives.
    async fn get_or_load(
        &self,
        services: &RuntimeServices,
        spec: &CacheSpec,
        key: &StoreKey,
        load: Loader,
    ) -> Result<Bytes, LoadError>;

    /// Removes `key` from the memory copy and the shared store.
    async fn remove(&self, services: &RuntimeServices, spec: &CacheSpec, key: &StoreKey);

    /// Whether this cache keeps what it loads. A caller whose load pays off only
    /// when its value is kept does without it on a cache that keeps nothing.
    fn keeps_values(&self) -> bool {
        true
    }
}

impl fmt::Debug for dyn PlatformLoadingCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PlatformLoadingCache")
    }
}

/// The loading cache of a deployment that links no module supplying one.
///
/// It holds nothing and runs every load, so each caller asks the source
/// itself.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoLoadingCache;

#[async_trait::async_trait(?Send)]
impl PlatformLoadingCache for NoLoadingCache {
    async fn get_or_load(
        &self,
        _services: &RuntimeServices,
        _spec: &CacheSpec,
        _key: &StoreKey,
        load: Loader,
    ) -> Result<Bytes, LoadError> {
        load().await.map(|loaded| loaded.bytes)
    }

    async fn remove(&self, _services: &RuntimeServices, _spec: &CacheSpec, _key: &StoreKey) {}

    fn keeps_values(&self) -> bool {
        false
    }
}

/// The loading cache each `[cache.<name>]` table selected, by the name a
/// [`CacheSpec`] carries. A cache with none, being one whose table names no
/// module where the build links no loading cache module or several, loads
/// through [`NoLoadingCache`].
#[derive(Default)]
pub struct LoadingCaches {
    by_feature: HashMap<String, Arc<dyn PlatformLoadingCache>>,
    /// What a cache whose table selected nothing loads through, when a test
    /// gives every caller one cache.
    unselected: Option<Arc<dyn PlatformLoadingCache>>,
}

impl LoadingCaches {
    /// Every caller loading through `cache`, for a test of one cache.
    #[must_use]
    pub fn every(cache: Arc<dyn PlatformLoadingCache>) -> Self {
        Self {
            by_feature: HashMap::new(),
            unselected: Some(cache),
        }
    }

    /// Keeps `cache` as the one the table `name` selected.
    pub fn insert(&mut self, name: &str, cache: Arc<dyn PlatformLoadingCache>) {
        self.by_feature.insert(name.to_owned(), cache);
    }

    /// The cache named `name` loads through.
    #[must_use]
    pub fn for_feature(&self, name: &str) -> &dyn PlatformLoadingCache {
        match self.by_feature.get(name).or(self.unselected.as_ref()) {
            Some(cache) => &**cache,
            None => &NoLoadingCache,
        }
    }

    /// Whether no table selected a cache.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_feature.is_empty() && self.unselected.is_none()
    }
}

impl fmt::Debug for LoadingCaches {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut selected: Vec<&str> = self.by_feature.keys().map(String::as_str).collect();
        selected.sort();
        f.debug_struct("LoadingCaches")
            .field("selected", &selected)
            .field("every", &self.unselected.is_some())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn two_publishers_never_share_a_key() {
        crate::prefix::test_support::assert_keys_follow_the_publisher(|domain| {
            StoreKey::new(&ServerKeyPrefix::new("ts", domain), "page", "abc").to_string()
        });
    }

    #[test]
    fn a_key_names_the_publisher_then_the_cache() {
        let key = StoreKey::new(
            &ServerKeyPrefix::new("ts", "www.example.com"),
            "robots-txt",
            "k1",
        );

        assert_eq!(key.namespace(), "ts.example.com/robots-txt");
        assert_eq!(key.key(), "k1");
        assert_eq!(key.to_string(), "ts.example.com/robots-txt/k1");
    }

    fn spec() -> CacheSpec {
        CacheSpec {
            name: "test",
            lifetimes: CacheLifetimes {
                memory: Duration::from_secs(1),
                store: Duration::from_secs(1),
                store_idle: Duration::from_secs(1),
            },
            memory_entries: 1,
        }
    }

    #[test]
    fn the_default_cache_runs_every_load() {
        let services = super::super::test_support::noop_services();
        let key = StoreKey::new(&ServerKeyPrefix::new("ts", "example.com"), "test", "k");
        let runs = Arc::new(AtomicUsize::new(0));
        for _ in 0..2 {
            let counted = Arc::clone(&runs);
            let value = futures::executor::block_on(NoLoadingCache.get_or_load(
                &services,
                &spec(),
                &key,
                Box::new(move || {
                    Box::pin(async move {
                        counted.fetch_add(1, Ordering::SeqCst);
                        Ok(LoadedValue::new(Bytes::from_static(b"v")))
                    })
                }),
            ))
            .expect("the load should succeed");
            assert_eq!(value, Bytes::from_static(b"v"));
        }

        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }

    /// A cache that records the client address in the services it is
    /// handed.
    #[derive(Default)]
    struct Recording {
        seen: std::sync::Mutex<Vec<Option<std::net::IpAddr>>>,
    }

    #[async_trait::async_trait(?Send)]
    impl PlatformLoadingCache for Recording {
        async fn get_or_load(
            &self,
            services: &RuntimeServices,
            _spec: &CacheSpec,
            _key: &StoreKey,
            load: Loader,
        ) -> Result<Bytes, LoadError> {
            self.seen
                .lock()
                .expect("should lock the record")
                .push(services.client_info().client_ip);
            load().await.map(|loaded| loaded.bytes)
        }

        async fn remove(&self, _services: &RuntimeServices, _spec: &CacheSpec, _key: &StoreKey) {}
    }

    /// One load serves every caller waiting on it, so the cache is handed
    /// nothing of the reader whose request asked, while that request keeps
    /// its own.
    #[test]
    fn a_cache_is_handed_nothing_of_the_reader_whose_request_loads() {
        let reader = std::net::IpAddr::from([203, 0, 113, 7]);
        let recording = Arc::new(Recording::default());
        let services = super::super::test_support::noop_services_with_client_ip(reader)
            .with_loading_cache(Arc::clone(&recording) as Arc<dyn PlatformLoadingCache>);
        let key = StoreKey::new(&ServerKeyPrefix::new("ts", "example.com"), "test", "k");

        futures::executor::block_on(services.load_cached(
            &spec(),
            &key,
            Box::new(|| Box::pin(async { Ok(LoadedValue::new(Bytes::from_static(b"v"))) })),
        ))
        .expect("the load should succeed");

        assert_eq!(
            *recording.seen.lock().expect("should lock the record"),
            vec![None],
            "the cache should see no client address"
        );
        assert_eq!(services.client_info().client_ip, Some(reader));
    }
}
