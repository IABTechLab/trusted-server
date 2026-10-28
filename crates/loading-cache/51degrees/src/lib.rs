//! The loading cache core declares, written with 51Degrees' `fiftyone-caching`.
//!
//! - **A brief copy in process memory.** A least recently used cache with a
//!   short lifetime, shared by every request an adapter serves with one
//!   application state. It loads a missing key once however many of them ask
//!   for it at the same moment, and that one load covers the read of the
//!   shared store and the source together.
//! - **The host's shared store.** The host's value cache where it has one,
//!   which on Fastly makes every instance at a location wait for one load, and
//!   its key-value store otherwise. It is read before the source, written
//!   through, and an entry left unused for the cache's idle time leaves it.
//!   This layer is made for each call from that request's stores, because a
//!   host may hand each request a store of its own.
//!
//! A cache is selected by naming this module, `51degrees` or in full, in its
//! own `[cache.<name>]` table.

mod layer;
mod stores;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use bytes::Bytes;
use fiftyone_caching::Clock;
use trusted_server_core::integrations::IntegrationBuilder;
use trusted_server_core::platform::{
    CacheSpec, LoadError, Loader, PlatformLoadingCache, RuntimeServices, StoreKey,
};

use crate::layer::{Call, Layer};
use crate::stores::WebClock;

/// The name of this module, from this crate's folder.
pub const MODULE: &str = "loading-cache.51degrees";

/// The builder a deployment hands to an adapter, which offers this loading
/// cache whatever the settings say. No stock build links it.
#[must_use]
pub fn builder() -> IntegrationBuilder {
    IntegrationBuilder::implementations(MODULE, env!("CARGO_PKG_NAME")).with_loading_cache(factory)
}

/// Builds the loading cache a deployment uses, for
/// [`IntegrationBuilder::with_loading_cache`].
///
/// A new one for each application state, so requests share a memory copy
/// only where their adapter keeps one state for them. Cloudflare builds its
/// state for every request, so no Worker request waits on another request's
/// load, which `fiftyone-caching` says fails unless each request runs in the
/// request scope it provides.
#[must_use]
pub fn factory() -> Arc<dyn PlatformLoadingCache> {
    Arc::new(LoadingCaches::new())
}

/// Every cache one application state uses, each a memory layer over the
/// host's shared store. See the crate documentation.
pub struct LoadingCaches {
    layers: Mutex<HashMap<LayerId, Arc<Layer>>>,
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
}

impl Default for LoadingCaches {
    fn default() -> Self {
        Self::new()
    }
}

impl LoadingCaches {
    /// The caches for this host. Every load runs in its caller.
    #[must_use]
    pub fn new() -> Self {
        Self {
            layers: Mutex::new(HashMap::new()),
            clock: Arc::new(WebClock),
        }
    }

    /// Reads the time from `clock` rather than the system clock.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// The memory layer for `spec`, made the first time it is asked for.
    fn layer(&self, spec: &CacheSpec) -> Arc<Layer> {
        let id = LayerId {
            name: spec.name,
            memory: spec.lifetimes.memory,
            entries: spec.memory_entries.max(1),
        };
        let mut layers = self.layers.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(layers.entry(id).or_insert_with(|| {
            Arc::new(Layer::new(id.memory, id.entries, Arc::clone(&self.clock)))
        }))
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
