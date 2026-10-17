//! The shared store layer, over the stores the host already gives each
//! request.
//!
//! A host's value cache is used where it has one, because on Fastly it is the
//! cache of the location, and a transaction lookup there makes every instance
//! at the location wait for one load. Every other host keeps the copies in its
//! key-value store. Either way the entries are written in `fiftyone-caching`'s
//! entry format, which carries each entry's own drop time, so a store that
//! deletes late or keeps no lifetimes still never returns an entry past it.

use std::convert::Infallible;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use edgezero_core::key_value_store::{KvError, KvStore as PlatformKvStore};
use fiftyone_caching::{
    ByteLookup, ByteStore, Clock, Codec, EncodedStore, Loaded, LoadingCache, Store, ValueLoader,
};
use trusted_server_core::platform::{
    CacheSpec, LoadError, Loader, PlatformValueCache, RuntimeServices, StoreKey, ValueLookup,
    ValueReservation,
};

use crate::layer::Below;

/// The lifetime given to a write that has none of its own, where the store
/// needs one.
const LONGEST: Duration = Duration::from_secs(365 * 24 * 60 * 60);

/// The shortest lifetime written to a key-value store. Workers KV refuses less
/// than a minute, and the entry's own drop time holds the exact lifetime.
const KV_SHORTEST: Duration = Duration::from_secs(60);

/// How a call loads through the shared store, for the memory layer to run
/// when the call leads.
pub(crate) fn through_store(
    services: RuntimeServices,
    spec: CacheSpec,
    key: StoreKey,
    load: Loader,
    clock: Arc<dyn Clock>,
) -> Below {
    Box::new(move || {
        Box::pin(async move { load_through(&services, &spec, &key, load, clock).await })
    })
}

async fn load_through(
    services: &RuntimeServices,
    spec: &CacheSpec,
    key: &StoreKey,
    load: Loader,
    clock: Arc<dyn Clock>,
) -> Result<Loaded<Bytes>, LoadError> {
    let source = Source {
        load: Mutex::new(Some(load)),
        clock: Arc::clone(&clock),
    };
    let key_text = key.key().to_owned();
    match services.value_cache() {
        Some(cache) => {
            let store = encoded(ValueCacheBytes(cache), key, &clock);
            layer(store, source, spec, clock)
                .get_loaded(&key_text)
                .await
        }
        None => {
            let store = encoded(KvBytes(services.kv_store()), key, &clock);
            layer(store, source, spec, clock)
                .get_loaded(&key_text)
                .await
        }
    }
}

/// Removes `key` from the shared store `services` supplies.
pub(crate) async fn remove(services: &RuntimeServices, key: &StoreKey, clock: Arc<dyn Clock>) {
    let key_text = key.key().to_owned();
    match services.value_cache() {
        Some(cache) => {
            Store::<String, Bytes>::remove(
                &encoded(ValueCacheBytes(cache), key, &clock),
                &key_text,
            )
            .await;
        }
        None => {
            Store::<String, Bytes>::remove(
                &encoded(KvBytes(services.kv_store()), key, &clock),
                &key_text,
            )
            .await;
        }
    }
}

/// The store of `bytes` entries under `key`'s namespace, being the publisher's
/// prefix and the cache's name.
fn encoded<B>(bytes: B, key: &StoreKey, clock: &Arc<dyn Clock>) -> EncodedStore<B, BytesCodec> {
    EncodedStore::builder(bytes, BytesCodec)
        .namespace(key.namespace())
        .clock(Arc::clone(clock))
        .build()
}

/// The shared store layer for one call, loading from the caller's source.
///
/// Made for each call, so it waits for nothing in the process. The memory
/// layer above has already made every other caller for the key wait, and a
/// host whose store can make other instances wait does so inside the store.
fn layer<S>(
    store: S,
    source: Source,
    spec: &CacheSpec,
    clock: Arc<dyn Clock>,
) -> LoadingCache<String, Bytes, S, Source>
where
    S: Store<String, Bytes>,
{
    let mut builder = LoadingCache::builder(store, source)
        .time_to_live(spec.lifetimes.store)
        .clock(clock)
        .concurrency(1);
    if !spec.lifetimes.store_idle.is_zero() {
        builder = builder.time_to_idle(spec.lifetimes.store_idle);
    }
    builder.build()
}

/// The caller's own loader, as the source below the shared store.
pub(crate) struct Source {
    load: Mutex<Option<Loader>>,
    clock: Arc<dyn Clock>,
}

impl ValueLoader<String, Bytes> for Source {
    type Error = LoadError;

    async fn load(&self, _key: &String) -> Result<Loaded<Bytes>, LoadError> {
        let load = self
            .load
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            // A layer is made for one call, whose one leader asks once.
            .ok_or_else(|| LoadError::new("this call's source was already asked"))?;
        let value = load().await?;
        let loaded = Loaded::new(value.bytes);
        Ok(
            match value
                .lifetime
                .and_then(|lifetime| self.clock.now().checked_add(lifetime))
            {
                Some(end) => loaded.expires_at(end),
                None => loaded,
            },
        )
    }
}

/// A host's value cache as a store of bytes.
pub(crate) struct ValueCacheBytes<'a>(pub(crate) &'a dyn PlatformValueCache);

impl ByteStore for ValueCacheBytes<'_> {
    type Reservation = Box<dyn ValueReservation>;

    async fn get(&self, key: &str) -> ByteLookup<Self::Reservation> {
        match self.0.lookup_or_reserve(key).await {
            ValueLookup::Hit(bytes) => ByteLookup::Hit(bytes.to_vec()),
            ValueLookup::Reserved(reservation) => ByteLookup::Reserved(reservation),
            ValueLookup::Miss => ByteLookup::Miss,
        }
    }

    async fn put(&self, key: &str, bytes: Vec<u8>, keep_for: Option<Duration>) {
        self.0
            .put(key, Bytes::from(bytes), keep_for.unwrap_or(LONGEST))
            .await;
    }

    async fn fill(
        &self,
        _key: &str,
        reservation: Self::Reservation,
        bytes: Vec<u8>,
        keep_for: Option<Duration>,
    ) {
        reservation.fill(Bytes::from(bytes), keep_for.unwrap_or(LONGEST));
    }

    async fn remove(&self, key: &str) {
        self.0.remove(key).await;
    }
}

/// A host's key-value store as a store of bytes.
pub(crate) struct KvBytes<'a>(pub(crate) &'a dyn PlatformKvStore);

impl ByteStore for KvBytes<'_> {
    type Reservation = Infallible;

    // Not every host's key-value store deletes an entry when its lifetime
    // ends, so an entry read past its drop time is removed.
    const DROPS_EXPIRED: bool = false;

    async fn get(&self, key: &str) -> ByteLookup<Infallible> {
        match self.0.get_bytes(key).await {
            Ok(Some(bytes)) => ByteLookup::Hit(bytes.to_vec()),
            Ok(None) => ByteLookup::Miss,
            Err(error) => {
                log::debug!("A loading cache entry could not be read, so it is loaded: {error}");
                ByteLookup::Miss
            }
        }
    }

    async fn put(&self, key: &str, bytes: Vec<u8>, keep_for: Option<Duration>) {
        let ttl = keep_for.unwrap_or(LONGEST).max(KV_SHORTEST);
        let bytes = Bytes::from(bytes);
        let written = match self.0.put_bytes_with_ttl(key, bytes.clone(), ttl).await {
            // A store that cannot give an entry a lifetime keeps it until it
            // is replaced, and the entry's own drop time ends its use.
            Err(KvError::Unsupported { .. }) => self.0.put_bytes(key, bytes).await,
            written => written,
        };
        if let Err(error) = written {
            log::debug!(
                "A loading cache entry was not written, so the next request loads it: {error}"
            );
        }
    }

    async fn remove(&self, key: &str) {
        if let Err(error) = self.0.delete(key).await {
            log::debug!("A loading cache entry was not removed: {error}");
        }
    }
}

/// Keeps a value's bytes as they are.
pub(crate) struct BytesCodec;

impl Codec<Bytes> for BytesCodec {
    fn encode(&self, value: &Bytes) -> Option<Vec<u8>> {
        Some(value.to_vec())
    }

    fn decode(&self, bytes: &[u8]) -> Option<Bytes> {
        Some(Bytes::copy_from_slice(bytes))
    }
}

/// The time, read the same way on every host, including
/// `wasm32-unknown-unknown`, which has no system clock of its own.
pub(crate) struct WebClock;

impl Clock for WebClock {
    fn now(&self) -> SystemTime {
        let since = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .unwrap_or_default();
        UNIX_EPOCH + since
    }
}
