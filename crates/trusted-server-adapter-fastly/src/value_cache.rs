//! Fastly's cache as the platform value cache.
//!
//! An entry is held at the point of presence that wrote it and shared by every
//! instance there, which is where a visitor's next request, and a second lookup
//! in the same request, arrive. Nothing needs creating or linking on the
//! service.
//!
//! A transaction lookup that finds nothing obliges this instance to fill the
//! key and makes every other instance that looks the key up wait for it, so a
//! loading cache over this store loads a missing key once per location rather
//! than once per instance.

use std::io::{Read as _, Write as _};
use std::time::Duration;

use bytes::Bytes;
use fastly::cache::core::{CacheKey, Found, Transaction};
use trusted_server_core::platform::{PlatformValueCache, ValueLookup, ValueReservation};

/// Values held in Fastly's cache.
#[derive(Debug, Default)]
pub struct FastlyValueCache;

/// The surrogate key an entry is written with, so [`PlatformValueCache::remove`]
/// can purge it. A 64-bit FNV-1a hash of the key, in hex, because a cache key
/// may hold characters a surrogate key may not.
fn surrogate_key(key: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in key.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("vc-{hash:016x}")
}

fn cache_key(key: &str) -> CacheKey {
    CacheKey::from(key.as_bytes().to_vec())
}

/// The bytes of a fresh entry, or `None` for a stale or unreadable one.
fn read(found: &Found) -> Option<Bytes> {
    if found.is_stale() {
        return None;
    }
    let mut body = Vec::new();
    found
        .to_stream()
        .inspect_err(|error| log::debug!("value cache body could not be opened: {error:?}"))
        .ok()?
        .read_to_end(&mut body)
        .inspect_err(|error| log::debug!("value cache body could not be read: {error}"))
        .ok()?;
    Some(Bytes::from(body))
}

#[async_trait::async_trait(?Send)]
impl PlatformValueCache for FastlyValueCache {
    async fn get(&self, key: &str) -> Option<Bytes> {
        // A plain lookup rather than a transaction, because a caller that only
        // reads must take no obligation it will not fill.
        let found = fastly::cache::core::lookup(cache_key(key))
            .execute()
            .inspect_err(|error| log::debug!("value cache lookup failed: {error:?}"))
            .ok()??;
        read(&found)
    }

    async fn put(&self, key: &str, value: Bytes, lifetime: Duration) {
        let mut writer = match fastly::cache::core::insert(cache_key(key), lifetime)
            .surrogate_keys([surrogate_key(key).as_str()])
            .known_length(value.len() as u64)
            .execute()
        {
            Ok(writer) => writer,
            Err(error) => {
                log::debug!("value cache insert could not begin: {error:?}");
                return;
            }
        };
        if let Err(error) = writer.write_all(&value) {
            log::debug!("value cache insert could not be written: {error}");
            return;
        }
        if let Err(error) = writer.finish() {
            log::debug!("value cache insert could not be finished: {error:?}");
        }
    }

    async fn lookup_or_reserve(&self, key: &str) -> ValueLookup {
        let transaction = match Transaction::lookup(cache_key(key)).execute() {
            Ok(transaction) => transaction,
            Err(error) => {
                log::debug!("value cache transaction lookup failed: {error:?}");
                return ValueLookup::Miss;
            }
        };
        // Asked before `found`, because a stale entry comes with both, and
        // reading it would leave the obligation unfilled and every other
        // instance waiting until it times out.
        if transaction.must_insert_or_update() {
            return ValueLookup::Reserved(Box::new(FastlyReservation {
                transaction: Some(transaction),
                surrogate_key: surrogate_key(key),
            }));
        }
        match transaction.found().as_ref().and_then(read) {
            Some(value) => ValueLookup::Hit(value),
            None => ValueLookup::Miss,
        }
    }

    async fn remove(&self, key: &str) {
        if let Err(error) = fastly::http::purge::purge_surrogate_key(&surrogate_key(key)) {
            log::debug!("value cache entry could not be purged: {error:?}");
        }
    }
}

/// This instance's obligation to fill a key every other instance waits for.
struct FastlyReservation {
    transaction: Option<Transaction>,
    surrogate_key: String,
}

impl ValueReservation for FastlyReservation {
    fn fill(mut self: Box<Self>, value: Bytes, lifetime: Duration) {
        let Some(transaction) = self.transaction.take() else {
            return;
        };
        // `insert` takes the transaction, so a write that fails part way can no
        // longer be canceled. The entry's own format checks its length when it
        // is read, so a short entry is a miss rather than a wrong value.
        let mut writer = match transaction
            .insert(lifetime)
            .surrogate_keys([self.surrogate_key.as_str()])
            .known_length(value.len() as u64)
            .execute()
        {
            Ok(writer) => writer,
            Err(error) => {
                log::debug!("value cache reserved insert could not begin: {error:?}");
                return;
            }
        };
        if let Err(error) = writer.write_all(&value) {
            log::debug!("value cache reserved insert could not be written: {error}");
            return;
        }
        if let Err(error) = writer.finish() {
            log::debug!("value cache reserved insert could not be finished: {error:?}");
        }
    }
}

impl Drop for FastlyReservation {
    fn drop(&mut self) {
        // Unfilled, so the obligation passes to an instance waiting on it.
        if let Some(transaction) = self.transaction.take()
            && let Err(error) = transaction.cancel_insert_or_update()
        {
            log::debug!("value cache reservation could not be released: {error:?}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run<F: std::future::Future>(future: F) -> F::Output {
        futures::executor::block_on(future)
    }

    #[test]
    fn a_value_put_is_read_back() {
        let cache = FastlyValueCache;
        run(cache.put(
            "test/value-cache/read-back",
            Bytes::from_static(b"answer"),
            Duration::from_secs(60),
        ));

        assert_eq!(
            run(cache.get("test/value-cache/read-back")).as_deref(),
            Some(&b"answer"[..]),
            "a value put should be read back"
        );
    }

    #[test]
    fn a_key_never_put_is_a_miss() {
        assert_eq!(
            run(FastlyValueCache.get("test/value-cache/never-put")),
            None,
            "a key never put should be a miss"
        );
    }

    #[test]
    fn a_missing_key_is_reserved_and_its_fill_is_then_a_hit() {
        let key = "test/value-cache/reserve-then-fill";
        let ValueLookup::Reserved(reservation) = run(FastlyValueCache.lookup_or_reserve(key))
        else {
            panic!("a missing key should oblige this caller to fill it");
        };
        reservation.fill(Bytes::from_static(b"filled"), Duration::from_secs(60));

        match run(FastlyValueCache.lookup_or_reserve(key)) {
            ValueLookup::Hit(value) => assert_eq!(value, Bytes::from_static(b"filled")),
            other => panic!("the filled key should be a hit, got {other:?}"),
        }
    }

    #[test]
    fn a_dropped_reservation_passes_the_obligation_on() {
        let key = "test/value-cache/dropped-reservation";
        let ValueLookup::Reserved(first) = run(FastlyValueCache.lookup_or_reserve(key)) else {
            panic!("a missing key should be reserved");
        };
        drop(first);

        let ValueLookup::Reserved(second) = run(FastlyValueCache.lookup_or_reserve(key)) else {
            panic!("an unfilled reservation should be given to the next caller");
        };
        second.fill(Bytes::from_static(b"second"), Duration::from_secs(60));
        assert_eq!(
            run(FastlyValueCache.get(key)).as_deref(),
            Some(&b"second"[..])
        );
    }

    #[test]
    fn a_stale_entry_is_reserved_rather_than_served() {
        // Present and stale, which a revalidation window keeps readable after
        // its lifetime has ended.
        let key = "test/value-cache/stale";
        let mut writer = fastly::cache::core::insert(cache_key(key), Duration::from_secs(0))
            .stale_while_revalidate(Duration::from_secs(60))
            .execute()
            .expect("should begin the insert");
        writer.write_all(b"stale").expect("should write the body");
        writer.finish().expect("should finish the insert");

        assert_eq!(
            run(FastlyValueCache.get(key)),
            None,
            "a plain read should not serve it"
        );
        match run(FastlyValueCache.lookup_or_reserve(key)) {
            ValueLookup::Reserved(reservation) => {
                reservation.fill(Bytes::from_static(b"fresh"), Duration::from_secs(60));
            }
            other => panic!("a stale entry should be reserved for a fresh load, got {other:?}"),
        }
        assert_eq!(
            run(FastlyValueCache.get(key)).as_deref(),
            Some(&b"fresh"[..])
        );
    }

    #[test]
    fn remove_purges_the_entry() {
        let key = "test/value-cache/remove";
        run(FastlyValueCache.put(key, Bytes::from_static(b"gone"), Duration::from_secs(60)));
        assert!(run(FastlyValueCache.get(key)).is_some());

        run(FastlyValueCache.remove(key));

        assert_eq!(run(FastlyValueCache.get(key)), None);
    }

    #[test]
    fn the_surrogate_key_is_short_and_safe() {
        let key = surrogate_key("ts.example.com/page/ts template; with ^ odd | chars");
        assert!(key.starts_with("vc-"));
        assert_eq!(key.len(), 3 + 16);
        assert!(key[3..].bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(surrogate_key("a"), surrogate_key("b"));
    }
}
