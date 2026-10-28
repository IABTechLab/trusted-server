//! A shared cache of small values with lifetimes, which a host supplies when it
//! has one.
//!
//! A module that pays for a remote answer holds it here, so the next
//! equivalent request, or a second lookup in the same request, is answered
//! without paying again. An entry can go at any time, which costs the caller one
//! fetch and never an error, so nothing durable belongs here.

use std::time::Duration;

use bytes::Bytes;

/// Values keyed by text, shared by every request the host serves from the same
/// place. The caller owns its key space, so a key starts with a prefix naming
/// the module that wrote it.
///
/// `Send + Sync` on the trait and `?Send` on the futures, as for the other
/// platform services, because [`super::RuntimeServices`] crosses threads while
/// the platform layer's futures never do.
#[async_trait::async_trait(?Send)]
pub trait PlatformValueCache: Send + Sync {
    /// The value held under `key`, or `None` when there is none or it cannot be
    /// read.
    async fn get(&self, key: &str) -> Option<Bytes>;

    /// Holds `value` under `key` for `lifetime`. A failure is the host's to
    /// log and is not returned, because the caller's only cost is fetching
    /// again.
    async fn put(&self, key: &str, value: Bytes, lifetime: Duration);

    /// The value held under `key`, or, when there is none and the host can
    /// make other callers for the key wait, a [`ValueReservation`] obliging
    /// this caller to fill it.
    ///
    /// The default never reserves, for a host whose cache cannot make callers
    /// wait.
    async fn lookup_or_reserve(&self, key: &str) -> ValueLookup {
        match self.get(key).await {
            Some(value) => ValueLookup::Hit(value),
            None => ValueLookup::Miss,
        }
    }

    /// Removes the value held under `key`. The default does nothing, for a
    /// host whose cache cannot remove an entry before its lifetime ends.
    async fn remove(&self, _key: &str) {}
}

/// What [`PlatformValueCache::lookup_or_reserve`] found.
pub enum ValueLookup {
    /// The value held for the key.
    Hit(Bytes),
    /// Nothing held, and this caller is the one to fill the key. The host
    /// makes other callers for the key wait until the reservation is filled
    /// or dropped.
    Reserved(Box<dyn ValueReservation>),
    /// Nothing held, and no reservation.
    Miss,
}

impl std::fmt::Debug for ValueLookup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Hit(value) => f.debug_tuple("Hit").field(&value.len()).finish(),
            Self::Reserved(_) => f.write_str("Reserved"),
            Self::Miss => f.write_str("Miss"),
        }
    }
}

/// The obligation to fill a key that other callers are waiting for.
///
/// Dropping it unfilled releases the callers waiting on it, and one of them
/// is given the obligation, so a failed or canceled load never leaves them
/// waiting.
pub trait ValueReservation {
    /// Holds `value` for `lifetime` and releases the callers waiting on it. A
    /// failure is the host's to log, as for [`PlatformValueCache::put`].
    fn fill(self: Box<Self>, value: Bytes, lifetime: Duration);
}
