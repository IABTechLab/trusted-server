//! The loading cache core declares, written with 51Degrees' `fiftyone-caching`.
//!
//! [`LoadingCaches`] implements core's
//! [`PlatformLoadingCache`](trusted_server_core::platform::PlatformLoadingCache).
//! Each cache is two layers of the one loading cache `fiftyone-caching`
//! provides, each the loader of the one above it.
//!
//! - **A brief copy in process memory.** A least recently used cache with a
//!   short lifetime, shared by every request the process serves. It loads a
//!   missing key once however many requests ask for it at the same moment, and
//!   that one load covers the read of the shared store and the source together.
//! - **The host's shared store.** The host's value cache where it has one,
//!   which on Fastly makes every instance at a location wait for one load, and
//!   its key-value store otherwise. It is read before the source, written
//!   through, and an entry left unused for the cache's idle time leaves it.
//!   This layer is made for each call from that request's stores, because a
//!   host may hand each request a store of its own.
//!
//! The source is the caller's own loader. A failed load is not stored, and
//! every caller waiting on it receives the error.
//!
//! A load may run apart from its caller where the host can run work on its
//! own and the cache allows it, on a pool of threads on a native host and as a
//! task of its own on Cloudflare, kept running with the request's
//! `wait_until` where the request's handling is wrapped in [`scope_request`].
//! Fastly and Spin run each load in the caller.
//!
//! An adapter takes the cache through
//! [`trusted_server_core::integrations::loading_cache_from`] from the builder
//! [`builder`] returns, which the stock list carries, so no adapter names this
//! crate.

mod layer;
mod stores;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use bytes::Bytes;
use fiftyone_caching::{Clock, SpawnLocal};
use trusted_server_core::integrations::IntegrationBuilder;
use trusted_server_core::platform::{
    CacheSpec, LoadError, Loader, PlatformLoadingCache, RuntimeServices, StoreKey,
};

use crate::layer::{Call, Layer};
use crate::stores::WebClock;

/// A load handed to a [`LocalSpawner`].
pub use fiftyone_caching::LoadTask;

/// The name of this module, from this crate's folder. No section selects it,
/// because a loading cache is how a deployment caches rather than a module a
/// document selects, and the name says which crate the cache came from.
pub const MODULE: &str = "loading-cache.51degrees";

/// The builder a deployment hands to an adapter, which offers this loading
/// cache whatever the settings say. A stock build lists it in
/// `trusted-server-modules`.
#[must_use]
pub fn builder() -> IntegrationBuilder {
    IntegrationBuilder::implementations(MODULE, env!("CARGO_PKG_NAME")).with_loading_cache(factory)
}

/// Builds the loading cache a deployment uses, for
/// [`IntegrationBuilder::with_loading_cache`].
///
/// A new one for each application state everywhere but Cloudflare, where the
/// state is built again for every request, so there one cache lives as long as
/// the isolate and every request it serves shares it.
#[must_use]
pub fn factory() -> Arc<dyn PlatformLoadingCache> {
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    {
        static ISOLATE: std::sync::OnceLock<Arc<LoadingCaches>> = std::sync::OnceLock::new();
        let caches: Arc<LoadingCaches> =
            Arc::clone(ISOLATE.get_or_init(|| Arc::new(LoadingCaches::new())));
        caches
    }
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    {
        Arc::new(LoadingCaches::new())
    }
}

/// Wraps the handling of one Cloudflare Worker request, so it can wait on a
/// load another request started and keep a load it started running after its
/// response. A deployment composing this crate on Cloudflare wraps its fetch
/// handler in this.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub use fiftyone_caching::cloudflare::with_context as scope_request;

/// Every cache a process uses, each a memory layer over the host's shared
/// store. See the crate documentation.
pub struct LoadingCaches {
    layers: Mutex<HashMap<LayerId, Arc<Layer>>>,
    runner: Runner,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for LoadingCaches {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadingCaches").finish_non_exhaustive()
    }
}

/// What tells two memory layers apart. A cache's calls all carry one spec, so
/// in practice each cache has one layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct LayerId {
    name: &'static str,
    memory: Duration,
    entries: usize,
    detached: bool,
}

/// Where a load a cache allows to run apart from its caller runs.
#[derive(Clone)]
enum Runner {
    /// In the caller, which is the only choice where the host runs nothing on
    /// its own.
    InCaller,
    /// On a pool of threads, each with its own runtime, made the first time a
    /// load needs one.
    #[cfg(not(target_family = "wasm"))]
    Pool(
        Arc<std::sync::OnceLock<fiftyone_caching::tokio::LocalPool>>,
        usize,
    ),
    /// As a task of its own on the current thread.
    Local(LocalSpawner),
}

/// Starts a load as a task on the current thread.
#[derive(Clone)]
pub struct LocalSpawner(Arc<dyn Fn(LoadTask) + Send + Sync>);

impl LocalSpawner {
    /// A spawner that hands each task to `spawn`, which runs it to its end.
    pub fn new(spawn: impl Fn(LoadTask) + Send + Sync + 'static) -> Self {
        Self(Arc::new(spawn))
    }
}

impl SpawnLocal for LocalSpawner {
    fn spawn_local(&self, task: LoadTask) {
        (self.0)(task);
    }
}

impl Default for LoadingCaches {
    fn default() -> Self {
        Self::new()
    }
}

impl LoadingCaches {
    /// The caches for this host: a load a cache allows to run apart from its
    /// caller does so on a pool of threads on a native host and as a task of
    /// its own on Cloudflare, kept running with the request's `wait_until`
    /// inside [`scope_request`], and every load runs in its caller on Fastly
    /// and Spin.
    #[must_use]
    pub fn new() -> Self {
        Self::with_runner(default_runner())
    }

    /// Caches whose every load runs in its caller.
    #[must_use]
    pub fn in_caller() -> Self {
        Self::with_runner(Runner::InCaller)
    }

    /// Caches that run a load a cache allows to run apart from its caller as a
    /// task started by `spawner` on the current thread.
    #[must_use]
    pub fn with_local_spawner(spawner: LocalSpawner) -> Self {
        Self::with_runner(Runner::Local(spawner))
    }

    /// Caches that run a load a cache allows to run apart from its caller on a
    /// pool of `threads` threads.
    #[cfg(not(target_family = "wasm"))]
    #[must_use]
    pub fn with_threads(threads: usize) -> Self {
        Self::with_runner(Runner::Pool(
            Arc::new(std::sync::OnceLock::new()),
            threads.max(1),
        ))
    }

    /// Reads the time from `clock` rather than the system clock.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    fn with_runner(runner: Runner) -> Self {
        Self {
            layers: Mutex::new(HashMap::new()),
            runner,
            clock: Arc::new(WebClock),
        }
    }

    /// The memory layer for `spec`, made the first time it is asked for.
    fn layer(&self, spec: &CacheSpec) -> Arc<Layer> {
        let detached = spec.detachable && !matches!(self.runner, Runner::InCaller);
        let id = LayerId {
            name: spec.name,
            memory: spec.lifetimes.memory,
            entries: spec.memory_entries.max(1),
            detached,
        };
        let mut layers = self.layers.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(layers.entry(id).or_insert_with(|| {
            Arc::new(Layer::new(
                id.memory,
                id.entries,
                if detached {
                    self.runner.clone()
                } else {
                    Runner::InCaller
                },
                Arc::clone(&self.clock),
            ))
        }))
    }
}

/// The runner a host gets by default.
fn default_runner() -> Runner {
    #[cfg(not(target_family = "wasm"))]
    {
        let threads = std::thread::available_parallelism()
            .map(std::num::NonZeroUsize::get)
            .unwrap_or(2)
            .clamp(1, 4);
        Runner::Pool(Arc::new(std::sync::OnceLock::new()), threads)
    }
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    {
        Runner::Local(LocalSpawner::new(|task| {
            fiftyone_caching::cloudflare::WaitUntil.spawn_local(task);
        }))
    }
    #[cfg(all(
        target_family = "wasm",
        not(all(target_arch = "wasm32", target_os = "unknown"))
    ))]
    {
        Runner::InCaller
    }
}

#[async_trait::async_trait(?Send)]
impl PlatformLoadingCache for LoadingCaches {
    async fn get_or_load(
        &self,
        services: &RuntimeServices,
        spec: &CacheSpec,
        key: &StoreKey,
        load: Loader,
    ) -> Result<Bytes, LoadError> {
        let call = Call::new(
            key,
            stores::through_store(
                services.clone(),
                spec.clone(),
                key.clone(),
                load,
                Arc::clone(&self.clock),
            ),
        );
        self.layer(spec).get(&call).await
    }

    async fn remove(&self, services: &RuntimeServices, spec: &CacheSpec, key: &StoreKey) {
        self.layer(spec).remove(&Call::spent(key)).await;
        stores::remove(services, key, Arc::clone(&self.clock)).await;
    }
}

#[cfg(test)]
mod tests;
