use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use edgezero_core::key_value_store::{KvError, KvPage, KvStore};
use trusted_server_core::platform::test_support::{
    NoopBackend, NoopConfigStore, NoopGeo, NoopHttpClient, NoopSecretStore,
};
use trusted_server_core::platform::{
    CacheLifetimes, CacheSpec, ClientInfo, LoadError, LoadedValue, Loader, PlatformLoadingCache,
    PlatformValueCache, RuntimeServices, ServerKeyPrefix, StoreKey, UnavailableKvStore,
    ValueLookup, ValueReservation,
};

use super::*;

/// How many callers ask for one key at the same moment.
const CALLERS: usize = 16;

/// A key-value store in memory that counts its reads. It keeps no lifetimes,
/// as on a host whose store keeps none.
#[derive(Default)]
struct MemoryKv {
    entries: Mutex<HashMap<String, Bytes>>,
    reads: AtomicUsize,
    /// Whether a write with a lifetime is refused, as it is by a store that
    /// has none to give.
    refuses_lifetimes: bool,
}

impl MemoryKv {
    fn keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self
            .entries
            .lock()
            .expect("should lock the test store")
            .keys()
            .cloned()
            .collect();
        keys.sort();
        keys
    }

    fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait(?Send)]
impl KvStore for MemoryKv {
    async fn delete(&self, key: &str) -> Result<(), KvError> {
        self.entries
            .lock()
            .expect("should lock the test store")
            .remove(key);
        Ok(())
    }

    async fn get_bytes(&self, key: &str) -> Result<Option<Bytes>, KvError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Ok(self
            .entries
            .lock()
            .expect("should lock the test store")
            .get(key)
            .cloned())
    }

    async fn list_keys_page(
        &self,
        _prefix: &str,
        _cursor: Option<&str>,
        _limit: usize,
    ) -> Result<KvPage, KvError> {
        Ok(KvPage::default())
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
        _ttl: Duration,
    ) -> Result<(), KvError> {
        if self.refuses_lifetimes {
            return Err(KvError::Unsupported {
                operation: "put_bytes_with_ttl".to_owned(),
            });
        }
        self.put_bytes(key, value).await
    }
}

type Entries = Arc<Mutex<HashMap<String, Bytes>>>;

/// A value cache that reserves a missing key for the caller that finds it
/// missing, as Fastly's does, counting how each reservation ended.
#[derive(Default)]
struct ReservingCache {
    entries: Entries,
    filled: Arc<AtomicUsize>,
    released: Arc<AtomicUsize>,
}

struct Reservation {
    key: String,
    entries: Entries,
    filled: Arc<AtomicUsize>,
    released: Arc<AtomicUsize>,
    done: bool,
}

impl ValueReservation for Reservation {
    fn fill(mut self: Box<Self>, value: Bytes, _lifetime: Duration) {
        self.entries
            .lock()
            .expect("should lock the test cache")
            .insert(self.key.clone(), value);
        self.filled.fetch_add(1, Ordering::SeqCst);
        self.done = true;
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.done {
            self.released.fetch_add(1, Ordering::SeqCst);
        }
    }
}

#[async_trait::async_trait(?Send)]
impl PlatformValueCache for ReservingCache {
    async fn get(&self, key: &str) -> Option<Bytes> {
        self.entries
            .lock()
            .expect("should lock the test cache")
            .get(key)
            .cloned()
    }

    async fn put(&self, key: &str, value: Bytes, _lifetime: Duration) {
        self.entries
            .lock()
            .expect("should lock the test cache")
            .insert(key.to_owned(), value);
    }

    async fn lookup_or_reserve(&self, key: &str) -> ValueLookup {
        if let Some(value) = self.get(key).await {
            return ValueLookup::Hit(value);
        }
        ValueLookup::Reserved(Box::new(Reservation {
            key: key.to_owned(),
            entries: Arc::clone(&self.entries),
            filled: Arc::clone(&self.filled),
            released: Arc::clone(&self.released),
            done: false,
        }))
    }
}

fn services_builder(
    kv: Arc<impl KvStore + 'static>,
) -> trusted_server_core::platform::RuntimeServicesBuilder {
    RuntimeServices::builder()
        .permission_policy(trusted_server_core::permissions::PermissionPolicy::sample())
        .config_store(Arc::new(NoopConfigStore))
        .secret_store(Arc::new(NoopSecretStore))
        .kv_store(kv)
        .backend(Arc::new(NoopBackend))
        .http_client(Arc::new(NoopHttpClient))
        .geo(Arc::new(
            trusted_server_core::platform::PlatformGeoModule::new("noop", Arc::new(NoopGeo)),
        ))
        .client_info(ClientInfo::default())
}

fn services(kv: Arc<impl KvStore + 'static>) -> RuntimeServices {
    services_builder(kv).build()
}

fn spec() -> CacheSpec {
    CacheSpec {
        name: "test",
        lifetimes: CacheLifetimes {
            memory: Duration::from_secs(5),
            store: Duration::from_secs(60),
            store_idle: Duration::from_secs(60),
        },
        memory_entries: 16,
    }
}

fn key(name: &str) -> StoreKey {
    StoreKey::new(&ServerKeyPrefix::new("ts", "www.example.com"), "test", name)
}

/// A loader that counts its runs, waits long enough for every other caller to
/// arrive, and answers `value`.
fn counted(runs: &Arc<AtomicUsize>, value: &'static str) -> Loader {
    let runs = Arc::clone(runs);
    Box::new(move || {
        Box::pin(async move {
            runs.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(200)).await;
            Ok(LoadedValue::new(Bytes::from_static(value.as_bytes())))
        })
    })
}

/// A loader that fails the test if it runs.
fn never() -> Loader {
    Box::new(|| Box::pin(async { panic!("the source should not be asked") }))
}

/// Asks `caches` for one key from `CALLERS` threads at once, each with a
/// runtime of its own, as a native server's requests do.
fn ask_from_many_threads(
    caches: &Arc<LoadingCaches>,
    services: &RuntimeServices,
    spec: &CacheSpec,
    load: impl Fn() -> Loader + Sync,
) -> Vec<Result<Bytes, LoadError>> {
    let start = Barrier::new(CALLERS);
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..CALLERS)
            .map(|_| {
                scope.spawn(|| {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("should build a runtime");
                    let loader = load();
                    start.wait();
                    runtime.block_on(caches.get_or_load(services, spec, &key("k"), loader))
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("a caller should not panic"))
            .collect()
    })
}

#[test]
fn module_constant_is_the_crate_folder() {
    assert_eq!(
        MODULE,
        trusted_server_core::module_name!(),
        "should be named by the folder this crate lives in"
    );
}

#[test]
fn the_builder_offers_a_cache_that_keeps_values() {
    let mut settings = trusted_server_core::test_support::tests::create_test_settings();
    settings.cache.robots_txt.module = Some("51degrees".to_owned());
    let caches =
        trusted_server_core::integrations::loading_caches_from(&settings, &[crate::builder()])
            .expect("a feature selecting this module should find its cache");

    assert!(
        caches
            .for_feature(trusted_server_core::settings::ROBOTS_TXT_CACHE)
            .keeps_values()
    );
    assert!(
        !crate::builder().supplies_integration(),
        "the cache is offered whatever the settings say, and no section selects it"
    );
}

/// Each application state is given a cache of its own, so two states never
/// share a memory copy. Cloudflare builds a state for every request, and a
/// Worker request that waits on another request's load fails.
#[tokio::test(flavor = "current_thread")]
async fn each_application_state_is_given_a_cache_of_its_own() {
    // A store that keeps nothing, so only a memory copy could answer.
    let services = services(Arc::new(UnavailableKvStore));
    let runs = Arc::new(AtomicUsize::new(0));
    let state = factory();

    state
        .get_or_load(&services, &spec(), &key("k"), counted(&runs, "value"))
        .await
        .expect("should load");
    state
        .get_or_load(&services, &spec(), &key("k"), never())
        .await
        .expect("the state's own memory copy should answer");
    factory()
        .get_or_load(&services, &spec(), &key("k"), counted(&runs, "value"))
        .await
        .expect("another state should load for itself");

    assert_eq!(
        runs.load(Ordering::SeqCst),
        2,
        "each state should load for itself"
    );
}

#[test]
fn many_callers_on_many_threads_load_a_missing_key_once_in_the_caller() {
    let runs = Arc::new(AtomicUsize::new(0));
    let caches = Arc::new(LoadingCaches::new());
    let services = services(Arc::new(MemoryKv::default()));

    let answers = ask_from_many_threads(&caches, &services, &spec(), || counted(&runs, "value"));

    assert_eq!(
        runs.load(Ordering::SeqCst),
        1,
        "the source should be asked once"
    );
    for answer in answers {
        assert_eq!(answer.expect("every caller should get the value"), "value");
    }
}

/// A load runs on the thread of the caller that leads it, as nothing runs a
/// load apart from its caller.
#[test]
fn a_load_runs_on_the_thread_of_the_caller_that_leads_it() {
    let caches = LoadingCaches::new();
    let services = services(Arc::new(MemoryKv::default()));
    let ran_on = Arc::new(Mutex::new(None));
    let seen = Arc::clone(&ran_on);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("should build a runtime");

    runtime
        .block_on(caches.get_or_load(
            &services,
            &spec(),
            &key("k"),
            Box::new(move || {
                Box::pin(async move {
                    *seen.lock().expect("should lock the record") =
                        Some(std::thread::current().id());
                    Ok(LoadedValue::new(Bytes::from_static(b"value")))
                })
            }),
        ))
        .expect("should load");

    assert_eq!(
        *ran_on.lock().expect("should lock the record"),
        Some(std::thread::current().id())
    );
}

#[test]
fn a_failed_load_reaches_every_waiter_and_is_not_kept() {
    let runs = Arc::new(AtomicUsize::new(0));
    let caches = Arc::new(LoadingCaches::new());
    let kv = Arc::new(MemoryKv::default());
    let services = services(Arc::clone(&kv));

    let answers = ask_from_many_threads(&caches, &services, &spec(), || {
        let runs = Arc::clone(&runs);
        Box::new(move || {
            Box::pin(async move {
                runs.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(200)).await;
                Err(LoadError::new("the source is down"))
            })
        })
    });

    assert_eq!(
        runs.load(Ordering::SeqCst),
        1,
        "the source should be asked once"
    );
    for answer in answers {
        assert_eq!(
            answer
                .expect_err("every caller should get the error")
                .message(),
            "the source is down"
        );
    }
    assert!(kv.keys().is_empty(), "a failure should not be written");

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("should build a runtime");
    let next = runtime
        .block_on(caches.get_or_load(&services, &spec(), &key("k"), counted(&runs, "value")))
        .expect("the next caller should load again");
    assert_eq!(next, "value");
    assert_eq!(
        runs.load(Ordering::SeqCst),
        2,
        "the next caller should ask the source again"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn the_store_answers_another_process_without_a_load() {
    let kv = Arc::new(MemoryKv::default());
    let services = services(Arc::clone(&kv));
    let runs = Arc::new(AtomicUsize::new(0));

    let first = LoadingCaches::new()
        .get_or_load(&services, &spec(), &key("k"), counted(&runs, "value"))
        .await
        .expect("the first process should load");
    let second = LoadingCaches::new()
        .get_or_load(&services, &spec(), &key("k"), never())
        .await
        .expect("the second process should read the store");

    assert_eq!(first, "value");
    assert_eq!(second, "value");
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert_eq!(
        kv.keys(),
        vec!["ts.example.com/test/k".to_owned()],
        "the entry should be under the publisher's prefix and the cache's name"
    );
}

/// A store with no lifetime to give still holds the entry, whose own drop
/// time ends its use.
#[tokio::test(flavor = "current_thread")]
async fn a_store_that_refuses_lifetimes_still_holds_the_entry() {
    let kv = Arc::new(MemoryKv {
        refuses_lifetimes: true,
        ..MemoryKv::default()
    });
    let services = services(Arc::clone(&kv));
    let runs = Arc::new(AtomicUsize::new(0));

    LoadingCaches::new()
        .get_or_load(&services, &spec(), &key("k"), counted(&runs, "value"))
        .await
        .expect("the first process should load");
    let second = LoadingCaches::new()
        .get_or_load(&services, &spec(), &key("k"), never())
        .await
        .expect("the second process should read the store");

    assert_eq!(second, "value");
    assert_eq!(kv.keys(), vec!["ts.example.com/test/k".to_owned()]);
}

#[tokio::test(flavor = "current_thread")]
async fn two_publishers_never_share_an_entry() {
    let kv = Arc::new(MemoryKv::default());
    let services = services(Arc::clone(&kv));
    let caches = LoadingCaches::new();
    let runs = Arc::new(AtomicUsize::new(0));
    let of = |domain: &str| StoreKey::new(&ServerKeyPrefix::new("ts", domain), "test", "k");

    let one = caches
        .get_or_load(
            &services,
            &spec(),
            &of("one.example"),
            counted(&runs, "one"),
        )
        .await
        .expect("should load the first publisher's value");
    let other = caches
        .get_or_load(
            &services,
            &spec(),
            &of("other.example"),
            counted(&runs, "other"),
        )
        .await
        .expect("should load the second publisher's value");

    assert_eq!(one, "one");
    assert_eq!(other, "other");
    assert_eq!(runs.load(Ordering::SeqCst), 2);
}

/// A clock a test moves on by hand.
fn manual_clock() -> (Arc<Mutex<SystemTime>>, Arc<dyn Clock>) {
    let now = Arc::new(Mutex::new(
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000),
    ));
    let read = Arc::clone(&now);
    let clock: Arc<dyn Clock> = Arc::new(move || *read.lock().expect("should lock the clock"));
    (now, clock)
}

fn advance(now: &Arc<Mutex<SystemTime>>, by: Duration) {
    let mut now = now.lock().expect("should lock the clock");
    *now += by;
}

#[tokio::test(flavor = "current_thread")]
async fn the_memory_copy_is_brief_and_the_store_is_read_after_it() {
    let kv = Arc::new(MemoryKv::default());
    let services = services(Arc::clone(&kv));
    let (now, clock) = manual_clock();
    let caches = LoadingCaches::new().with_clock(clock);
    let runs = Arc::new(AtomicUsize::new(0));

    caches
        .get_or_load(&services, &spec(), &key("k"), counted(&runs, "value"))
        .await
        .expect("should load");
    let reads = kv.reads();
    caches
        .get_or_load(&services, &spec(), &key("k"), never())
        .await
        .expect("the memory copy should answer");
    assert_eq!(kv.reads(), reads, "the memory copy should answer first");

    advance(&now, Duration::from_secs(6));
    caches
        .get_or_load(&services, &spec(), &key("k"), never())
        .await
        .expect("the store should answer once the memory copy has gone");
    assert_eq!(kv.reads(), reads + 1, "the store should then be read");
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn an_entry_left_unused_for_its_idle_time_is_loaded_again() {
    let kv = Arc::new(MemoryKv::default());
    let services = services(Arc::clone(&kv));
    let (now, clock) = manual_clock();
    let caches = LoadingCaches::new().with_clock(clock);
    let runs = Arc::new(AtomicUsize::new(0));
    let mut idle = spec();
    idle.lifetimes.store = Duration::from_secs(3600);
    idle.lifetimes.store_idle = Duration::from_secs(10);

    caches
        .get_or_load(&services, &idle, &key("k"), counted(&runs, "value"))
        .await
        .expect("should load");
    advance(&now, Duration::from_secs(11));
    caches
        .get_or_load(&services, &idle, &key("k"), counted(&runs, "again"))
        .await
        .expect("should load again");

    assert_eq!(
        runs.load(Ordering::SeqCst),
        2,
        "the unused entry should have expired"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_value_with_a_lifetime_of_its_own_ends_by_then() {
    let kv = Arc::new(MemoryKv::default());
    let services = services(Arc::clone(&kv));
    let (now, clock) = manual_clock();
    let caches = LoadingCaches::new().with_clock(clock);
    let runs = Arc::new(AtomicUsize::new(0));
    let short =
        |runs: &Arc<AtomicUsize>| -> Loader {
            let runs = Arc::clone(runs);
            Box::new(move || {
                Box::pin(async move {
                    runs.fetch_add(1, Ordering::SeqCst);
                    Ok(LoadedValue::new(Bytes::from_static(b"v"))
                        .with_lifetime(Duration::from_secs(2)))
                })
            })
        };

    caches
        .get_or_load(&services, &spec(), &key("k"), short(&runs))
        .await
        .expect("should load");
    advance(&now, Duration::from_secs(3));
    caches
        .get_or_load(&services, &spec(), &key("k"), short(&runs))
        .await
        .expect("should load again");

    assert_eq!(
        runs.load(Ordering::SeqCst),
        2,
        "the value's own lifetime should end it"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_reserving_value_cache_is_filled_by_the_load_and_released_by_a_failure() {
    let cache = Arc::new(ReservingCache::default());
    let services = services_builder(Arc::new(MemoryKv::default()))
        .value_cache(Arc::clone(&cache) as Arc<dyn PlatformValueCache>)
        .build();
    let runs = Arc::new(AtomicUsize::new(0));

    let failed = LoadingCaches::new()
        .get_or_load(
            &services,
            &spec(),
            &key("k"),
            Box::new(|| Box::pin(async { Err(LoadError::new("down")) })),
        )
        .await;
    assert!(failed.is_err(), "the failure should reach the caller");
    assert_eq!(
        cache.released.load(Ordering::SeqCst),
        1,
        "a failure should release the waiting callers"
    );
    assert_eq!(cache.filled.load(Ordering::SeqCst), 0);

    let value = LoadingCaches::new()
        .get_or_load(&services, &spec(), &key("k"), counted(&runs, "value"))
        .await
        .expect("should load");
    assert_eq!(value, "value");
    assert_eq!(
        cache.filled.load(Ordering::SeqCst),
        1,
        "the load should fill the reservation"
    );

    let again = LoadingCaches::new()
        .get_or_load(&services, &spec(), &key("k"), never())
        .await
        .expect("another process should read the filled entry");
    assert_eq!(again, "value");
}

#[tokio::test(flavor = "current_thread")]
async fn remove_drops_the_memory_copy_and_the_stored_one() {
    let kv = Arc::new(MemoryKv::default());
    let services = services(Arc::clone(&kv));
    let caches = LoadingCaches::new();
    let runs = Arc::new(AtomicUsize::new(0));

    caches
        .get_or_load(&services, &spec(), &key("k"), counted(&runs, "old"))
        .await
        .expect("should load");
    caches.remove(&services, &spec(), &key("k")).await;
    let fresh = caches
        .get_or_load(&services, &spec(), &key("k"), counted(&runs, "new"))
        .await
        .expect("should load again");

    assert_eq!(fresh, "new");
    assert_eq!(runs.load(Ordering::SeqCst), 2);
}

/// A memory lifetime of zero keeps no memory copy, so every call reads the
/// store, and callers asking at the same moment still share one load.
#[test]
fn a_memory_lifetime_of_zero_keeps_no_memory_copy_and_still_loads_once() {
    let runs = Arc::new(AtomicUsize::new(0));
    let caches = Arc::new(LoadingCaches::new());
    let kv = Arc::new(MemoryKv::default());
    let services = services(Arc::clone(&kv));
    let mut no_memory = spec();
    no_memory.lifetimes.memory = Duration::ZERO;

    let answers = ask_from_many_threads(&caches, &services, &no_memory, || counted(&runs, "value"));

    assert_eq!(
        runs.load(Ordering::SeqCst),
        1,
        "the source should be asked once"
    );
    for answer in answers {
        assert_eq!(answer.expect("every caller should get the value"), "value");
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("should build a runtime");
    let reads = kv.reads();
    let again = runtime
        .block_on(caches.get_or_load(&services, &no_memory, &key("k"), never()))
        .expect("the store should answer");
    assert_eq!(again, "value");
    assert_eq!(
        kv.reads(),
        reads + 1,
        "with no memory copy the store is read"
    );
}

/// A caller whose load is dropped before it ends hands the load on, so a
/// caller waiting on it loads for itself rather than waiting for ever.
#[tokio::test(flavor = "current_thread")]
async fn a_waiting_caller_loads_when_the_caller_leading_the_load_is_dropped() {
    let caches = LoadingCaches::new();
    let services = services(Arc::new(MemoryKv::default()));
    let (spec, key) = (spec(), key("k"));
    let runs = Arc::new(AtomicUsize::new(0));
    let unending: Loader =
        Box::new(|| Box::pin(std::future::pending::<Result<LoadedValue, LoadError>>()));

    let mut leading = caches.get_or_load(&services, &spec, &key, unending);
    assert!(
        futures::poll!(leading.as_mut()).is_pending(),
        "the first caller's load should be running"
    );
    let mut waiting = caches.get_or_load(&services, &spec, &key, counted(&runs, "value"));
    assert!(
        futures::poll!(waiting.as_mut()).is_pending(),
        "the second caller should wait on the first caller's load"
    );
    drop(leading);

    let value = waiting
        .await
        .expect("the waiting caller should load for itself");
    assert_eq!(value, "value");
    assert_eq!(
        runs.load(Ordering::SeqCst),
        1,
        "the waiting caller's own load should run"
    );
}

/// A store that fails every call, as the Axum adapter's does, holds nothing.
/// A failed read is a miss and a failed write still serves the value, so the
/// memory copy alone answers until it is removed.
#[tokio::test(flavor = "current_thread")]
async fn a_store_that_fails_every_call_leaves_the_memory_copy_answering() {
    let services = services(Arc::new(UnavailableKvStore));
    let caches = LoadingCaches::new();
    let runs = Arc::new(AtomicUsize::new(0));

    let value = caches
        .get_or_load(&services, &spec(), &key("k"), counted(&runs, "value"))
        .await
        .expect("a store that fails should not fail the load");
    let again = caches
        .get_or_load(&services, &spec(), &key("k"), never())
        .await
        .expect("the memory copy should answer");
    caches.remove(&services, &spec(), &key("k")).await;
    let fresh = caches
        .get_or_load(&services, &spec(), &key("k"), counted(&runs, "fresh"))
        .await
        .expect("should load again once the memory copy is removed");

    assert_eq!(value, "value");
    assert_eq!(again, "value");
    assert_eq!(fresh, "fresh");
    assert_eq!(runs.load(Ordering::SeqCst), 2);
}
