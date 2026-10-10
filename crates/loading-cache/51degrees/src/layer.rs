//! The memory layer, the brief copy every request in a process shares.
//!
//! Its key is a [`Call`], which carries the caller's own way of loading the
//! value through the shared store, because what loads a value belongs to the
//! request asking for it while the layer belongs to the process. The key is
//! compared by its text alone, so every caller for one key joins one load, and
//! the load of whichever caller leads is the one that runs.

use std::future::Future;
use std::hash::{Hash, Hasher};
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use bytes::Bytes;
use fiftyone_caching::{
    Clock, Inline, Loaded, LoadingCache, LruLoadingCache, LruStore, SpawnedLocal, ValueLoader,
};
use trusted_server_core::platform::{LoadError, StoreKey};

use crate::{LocalSpawner, Runner};

/// Loads a value through the shared store, for the caller that leads.
pub(crate) type Below = Box<dyn FnOnce() -> BelowFuture + Send>;

/// The work a [`Below`] does.
pub(crate) type BelowFuture = Pin<Box<dyn Future<Output = Result<Loaded<Bytes>, LoadError>>>>;

/// A key in the memory layer, carrying the caller's [`Below`].
#[derive(Clone)]
pub(crate) struct Call {
    text: Arc<str>,
    below: Arc<Mutex<Option<Below>>>,
}

impl Call {
    /// The call for `key`, loading with `below` if it leads.
    pub(crate) fn new(key: &StoreKey, below: Below) -> Self {
        Self {
            text: key.to_string().into(),
            below: Arc::new(Mutex::new(Some(below))),
        }
    }

    /// The key alone, for a removal, which loads nothing.
    pub(crate) fn spent(key: &StoreKey) -> Self {
        Self {
            text: key.to_string().into(),
            below: Arc::new(Mutex::new(None)),
        }
    }
}

impl PartialEq for Call {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text
    }
}

impl Eq for Call {}

impl Hash for Call {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.text.hash(state);
    }
}

impl std::fmt::Display for Call {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

/// The memory layer's loader, which runs the leading caller's [`Below`].
pub(crate) struct FromCall;

impl ValueLoader<Call, Bytes> for FromCall {
    type Error = LoadError;

    async fn load(&self, call: &Call) -> Result<Loaded<Bytes>, LoadError> {
        let below = call
            .below
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        match below {
            Some(below) => below().await,
            // Each caller brings a load of its own and a retry after an
            // abandoned load leads with the retrying caller's, so the one that
            // leads always has one.
            None => Err(LoadError::new("this call's load was already run")),
        }
    }
}

/// The memory layer's cache, which drops its least recently used entry when
/// full, and whose loader is the shared store layer below it.
type Memory<R> = LruLoadingCache<Call, Bytes, FromCall, R>;

/// One cache's memory layer, with the runner its loads use.
pub(crate) enum Layer {
    InCaller(Memory<Inline>),
    #[cfg(not(target_family = "wasm"))]
    Pool(Memory<fiftyone_caching::Spawned<PoolSpawner>>),
    Local(Memory<SpawnedLocal<LocalSpawner>>),
}

impl Layer {
    pub(crate) fn new(
        lifetime: Duration,
        entries: usize,
        runner: Runner,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let store = LruStore::builder()
            .size(entries)
            .clock(Arc::clone(&clock))
            .build();
        let builder = LoadingCache::builder(store, FromCall)
            .time_to_live(lifetime)
            .clock(clock);
        match runner {
            Runner::InCaller => Self::InCaller(builder.build()),
            #[cfg(not(target_family = "wasm"))]
            Runner::Pool(pool, threads) => {
                Self::Pool(builder.spawner(PoolSpawner { pool, threads }).build())
            }
            Runner::Local(spawner) => Self::Local(builder.local_spawner(spawner).build()),
        }
    }

    pub(crate) async fn get(&self, call: &Call) -> Result<Bytes, LoadError> {
        match self {
            Self::InCaller(cache) => cache.get(call).await,
            #[cfg(not(target_family = "wasm"))]
            Self::Pool(cache) => cache.get(call).await,
            Self::Local(cache) => cache.get(call).await,
        }
    }

    pub(crate) async fn remove(&self, call: &Call) {
        match self {
            Self::InCaller(cache) => cache.remove(call).await,
            #[cfg(not(target_family = "wasm"))]
            Self::Pool(cache) => cache.remove(call).await,
            Self::Local(cache) => cache.remove(call).await,
        }
    }
}

/// Runs each load on a pool of threads, made the first time a load needs one,
/// so a process whose loads all run in their callers starts no threads.
#[cfg(not(target_family = "wasm"))]
pub(crate) struct PoolSpawner {
    pool: Arc<std::sync::OnceLock<fiftyone_caching::tokio::LocalPool>>,
    threads: usize,
}

#[cfg(not(target_family = "wasm"))]
impl fiftyone_caching::Spawn for PoolSpawner {
    fn spawn(&self, start: fiftyone_caching::StartLoad) {
        let pool = self
            .pool
            .get_or_init(|| fiftyone_caching::tokio::LocalPool::new(self.threads));
        fiftyone_caching::Spawn::spawn(pool, start);
    }
}
