//! The memory layer, the brief copy every request served with one application
//! state shares.
//!
//! Its key is a [`Call`], which carries the caller's own way of loading the
//! value through the shared store, because what loads a value belongs to the
//! request asking for it while the layer belongs to the state. The key is
//! compared by its text alone, so every caller for one key joins one load, and
//! the load of whichever caller leads is the one that runs.

use std::future::Future;
use std::hash::{Hash, Hasher};
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use bytes::Bytes;
use fiftyone_caching::{
    Clock, Inline, Loaded, LoadingCache, LruLoadingCache, LruStore, ValueLoader,
};
use trusted_server_core::platform::{LoadError, StoreKey};

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

/// One cache's memory layer, which drops its least recently used entry when
/// full, and whose loader is the shared store layer below it. Every load runs
/// in its caller.
pub(crate) struct Layer(LruLoadingCache<Call, Bytes, FromCall, Inline>);

impl Layer {
    pub(crate) fn new(lifetime: Duration, entries: usize, clock: Arc<dyn Clock>) -> Self {
        let store = LruStore::builder()
            .size(entries)
            .clock(Arc::clone(&clock))
            .build();
        Self(
            LoadingCache::builder(store, FromCall)
                .time_to_live(lifetime)
                .clock(clock)
                .build(),
        )
    }

    pub(crate) async fn get(&self, call: &Call) -> Result<Bytes, LoadError> {
        self.0.get(call).await
    }

    pub(crate) async fn remove(&self, call: &Call) {
        self.0.remove(call).await;
    }
}
