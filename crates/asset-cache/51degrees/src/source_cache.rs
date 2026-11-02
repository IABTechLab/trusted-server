//! The cache that holds a source's responses, keyed the way the source says
//! its responses vary.
//!
//! A request's address is what it asks the source for. The first response for
//! an address records the request headers its `Vary` names, and that record is
//! held in the loading cache like any other value. A later request for the
//! address reads the record and is served the copy kept under its own values
//! for those headers, as RFC 9111 section 4.1 describes.
//!
//! Requests for an address no record covers share one load. The request that
//! leads it fetches the source and writes its copy under its own values before
//! the record is published, so a waiting request with the same values is
//! served that copy, and one with other values loads its own, sharing that
//! load with every request waiting on the same values. The request whose fetch
//! ran is served what it fetched, whatever the cache kept, so no request
//! fetches the source twice.
//!
//! A response whose `Vary` is `*`, names `Cookie`, or cannot be read can be
//! served to no other request, so its record says no copy is kept, and every
//! request for the address fetches its own response until the record ends.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use http::HeaderMap;
use http::header::{self, HeaderName};
use sha2::{Digest as _, Sha256};

use trusted_server_core::platform::{
    CacheSpec, LoadError, LoadedValue, Loader, RuntimeServices, ServerKeyPrefix, StoreKey,
};

use crate::hash::hash_length_prefixed as push;

/// What a source's response says it varies on, read from its `Vary`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceVary {
    /// These request headers, in lowercase, sorted and each named once.
    /// Empty when the response varies on nothing.
    Headers(Vec<String>),
    /// `Vary: *`, which no other request matches.
    Any,
    /// `Cookie` is named, which keys a copy on every cookie a reader carries,
    /// so each reader would have a copy of their own.
    Cookie,
    /// A `Vary` that is not a list of header names.
    Unreadable,
}

impl SourceVary {
    /// What a response with `headers` says it varies on.
    pub fn of(headers: &HeaderMap) -> Self {
        let mut names = Vec::new();
        for value in headers.get_all(header::VARY) {
            let Ok(value) = value.to_str() else {
                return Self::Unreadable;
            };
            for name in value
                .split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
            {
                if name == "*" {
                    return Self::Any;
                }
                let Ok(name) = HeaderName::from_bytes(name.as_bytes()) else {
                    return Self::Unreadable;
                };
                if name == header::COOKIE {
                    return Self::Cookie;
                }
                names.push(name.as_str().to_owned());
            }
        }
        names.sort_unstable();
        names.dedup();
        Self::Headers(names)
    }

    /// The same, leaving out `name`, a header whose value never selects a
    /// different copy, such as `Accept-Encoding` for a copy kept decoded and
    /// encoded for each reader as it is served.
    #[cfg(test)]
    #[must_use]
    pub fn without(self, name: &HeaderName) -> Self {
        match self {
            Self::Headers(mut names) => {
                names.retain(|kept| kept != name.as_str());
                Self::Headers(names)
            }
            other => other,
        }
    }
}

/// What the cache holds for an address.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AddressRecord {
    /// Each copy at the address is found by a request's values for these
    /// headers.
    VariesOn(Vec<String>),
    /// No copy at the address is served to another request.
    NoCopies,
}

impl AddressRecord {
    fn encode(&self) -> Bytes {
        match self {
            Self::VariesOn(names) => Bytes::from(format!("v1:{}", names.join(","))),
            Self::NoCopies => Bytes::from_static(b"v1*"),
        }
    }

    /// The record `bytes` hold, or `None` for bytes this build did not write.
    fn decode(bytes: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(bytes).ok()?;
        if text == "v1*" {
            return Some(Self::NoCopies);
        }
        let names = text.strip_prefix("v1:")?;
        if names.is_empty() {
            return Some(Self::VariesOn(Vec::new()));
        }
        names
            .split(',')
            .map(|name| {
                HeaderName::from_bytes(name.as_bytes())
                    .ok()
                    .filter(|parsed| parsed.as_str() == name)
                    .map(|_| name.to_owned())
            })
            .collect::<Option<Vec<_>>>()
            .map(Self::VariesOn)
    }
}

/// One address, made of the publisher's prefix, so two publishers never share
/// a record or a copy, and a digest of what the request asks the source for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceAddress {
    prefix: ServerKeyPrefix,
    key: String,
}

impl SourceAddress {
    /// The address `key` names for the publisher `prefix` names. The key
    /// should be short, such as a digest, because a store refuses a long key.
    #[must_use]
    pub fn new(prefix: ServerKeyPrefix, key: String) -> Self {
        Self { prefix, key }
    }

    fn record_key(&self, cache: &SourceCache) -> StoreKey {
        StoreKey::new(&self.prefix, cache.records.name, self.key.clone())
    }

    fn copy_key(&self, cache: &SourceCache, variant: &str) -> StoreKey {
        StoreKey::new(
            &self.prefix,
            cache.copies.name,
            format!("{}-{variant}", self.key),
        )
    }
}

/// The bytes of a variant's digest its key holds, half the digest. 128 bits
/// keeps two variants of one address from ever sharing a key, and a key is
/// read by stores that refuse long ones.
const VARIANT_DIGEST_BYTES: usize = 16;

/// Which copy a request is served, as a digest of its values for `names` and
/// of what the publisher's rule declared for it.
///
/// An absent header and one present with an empty value are different requests
/// to the source, so they give different digests, as do repeated values in a
/// different order.
fn variant(names: &[String], headers: &HeaderMap, declared: Option<&str>) -> String {
    let mut hasher = Sha256::new();
    push(&mut hasher, b"ts-asset-variant-v1");
    push(&mut hasher, &(names.len() as u64).to_be_bytes());
    for name in names {
        push(&mut hasher, name.as_bytes());
        let values: Vec<&[u8]> = headers
            .get_all(name.as_str())
            .iter()
            .map(http::HeaderValue::as_bytes)
            .collect();
        if values.is_empty() {
            push(&mut hasher, b"absent");
        } else {
            push(&mut hasher, b"present");
            push(&mut hasher, &(values.len() as u64).to_be_bytes());
            for value in values {
                push(&mut hasher, value);
            }
        }
    }
    match declared {
        None => push(&mut hasher, b"undeclared"),
        Some(token) => {
            push(&mut hasher, b"declared");
            push(&mut hasher, token.as_bytes());
        }
    }
    hex::encode(&hasher.finalize()[..VARIANT_DIGEST_BYTES])
}

/// The two loading caches one kind of response uses, one holding each
/// address's record, and one holding the copies.
#[derive(Debug, Clone)]
pub struct SourceCache {
    /// Holds each address's record of what its responses vary on.
    pub records: CacheSpec,
    /// Holds the copies, each under its address and a request's values.
    pub copies: CacheSpec,
}

/// What one fetch of the source gave the request that made it.
#[derive(Debug, Clone)]
pub struct Fetched {
    /// What the source's response varies on.
    pub vary: SourceVary,
    /// The copy cache's value for the request that fetched it, in the caller's
    /// own form, which may say the response may not be served to another
    /// request.
    pub copy: Bytes,
    /// How long the copy may be used. The record lasts as long.
    pub lifetime: Duration,
}

/// The work of one [`Fetch`].
pub type FetchFuture = Pin<Box<dyn Future<Output = Result<Fetched, LoadError>>>>;

/// Fetches the source for one request, run at most once for its record and
/// once for its copy, by whichever of the two loads this request leads.
pub type Fetch = Arc<dyn Fn() -> FetchFuture + Send + Sync>;

/// One request's lookup.
pub struct Lookup<'a> {
    /// The request's services, whose loading cache holds the records and the
    /// copies.
    pub services: &'a RuntimeServices,
    /// The caches this kind of response uses.
    pub cache: &'a SourceCache,
    /// What the request asks the source for.
    pub address: &'a SourceAddress,
    /// The request's headers, as they are sent to the source.
    pub headers: &'a HeaderMap,
    /// What the publisher's rule says separates this request's copy, beyond
    /// the source's own `Vary`.
    pub declared: Option<&'a str>,
    /// The copy cache's value saying a response may not be served to another
    /// request, in the caller's form.
    pub not_shareable: &'a Bytes,
}

/// Where a load leaves the copy its own fetch made, for the request whose
/// fetch it was.
type Own = Arc<Mutex<Option<Bytes>>>;

fn keep_own(own: &Own, copy: &Bytes) {
    *own.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(copy.clone());
}

fn take_own(own: &Own) -> Option<Bytes> {
    own.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
}

/// What a lookup found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// The copy cache's value for this request's values, in the caller's form.
    Copy {
        /// The value.
        value: Bytes,
        /// Where it is kept, for a caller that finds it cannot use it.
        key: StoreKey,
    },
    /// No copy at the address is served to another request, so this request
    /// fetches its own response.
    NoCopies,
}

/// The copy for `lookup`'s request, from the record and copy caches or from
/// `fetch`. When this request's own fetch ran, it is served what that fetch
/// made, even when the cache keeps it for nobody.
///
/// # Errors
///
/// The error of a fetch this request ran or waited on. Every request waiting
/// on that fetch receives it, and nothing is kept.
pub async fn get_or_fetch(lookup: &Lookup<'_>, fetch: Fetch) -> Result<Found, LoadError> {
    let own = Own::default();
    let record_key = lookup.address.record_key(lookup.cache);
    let record = lookup
        .services
        .load_cached(
            &lookup.cache.records,
            &record_key,
            record_loader(lookup, Arc::clone(&fetch), Arc::clone(&own)),
        )
        .await?;
    let Some(record) = AddressRecord::decode(&record) else {
        log::warn!("source_cache: a record this build did not write was dropped");
        lookup
            .services
            .loading_cache_for(lookup.cache.records.name)
            .remove(lookup.services, &lookup.cache.records, &record_key)
            .await;
        return Ok(Found::NoCopies);
    };
    let names = match record {
        AddressRecord::NoCopies => return Ok(Found::NoCopies),
        AddressRecord::VariesOn(names) => names,
    };
    let copy_key = lookup.address.copy_key(
        lookup.cache,
        &variant(&names, lookup.headers, lookup.declared),
    );
    if let Some(value) = take_own(&own) {
        return Ok(Found::Copy {
            value,
            key: copy_key,
        });
    }
    let value = lookup
        .services
        .load_cached(
            &lookup.cache.copies,
            &copy_key,
            copy_loader(lookup, names, fetch, Arc::clone(&own)),
        )
        .await?;
    Ok(Found::Copy {
        value: take_own(&own).unwrap_or(value),
        key: copy_key,
    })
}

/// The record's load, which fetches the source and records what its response
/// varies on. The fetching request's copy is written under its own values
/// first, so every request waiting on the record finds it.
fn record_loader(lookup: &Lookup<'_>, fetch: Fetch, own: Own) -> Loader {
    let services = lookup.services.clone().without_connection();
    let cache = lookup.cache.clone();
    let address = lookup.address.clone();
    let headers = lookup.headers.clone();
    let declared = lookup.declared.map(str::to_owned);
    Box::new(move || {
        Box::pin(async move {
            let fetched = fetch().await?;
            keep_own(&own, &fetched.copy);
            let SourceVary::Headers(names) = fetched.vary else {
                return Ok(LoadedValue::new(AddressRecord::NoCopies.encode())
                    .with_lifetime(fetched.lifetime));
            };
            let copy_key =
                address.copy_key(&cache, &variant(&names, &headers, declared.as_deref()));
            let copy = fetched.copy;
            let lifetime = fetched.lifetime;
            services
                .load_cached(
                    &cache.copies,
                    &copy_key,
                    Box::new(move || {
                        Box::pin(async move { Ok(LoadedValue::new(copy).with_lifetime(lifetime)) })
                    }),
                )
                .await?;
            Ok(LoadedValue::new(AddressRecord::VariesOn(names).encode())
                .with_lifetime(fetched.lifetime))
        })
    })
}

/// A copy's load, which fetches the source for this request's values.
///
/// A response that no longer varies on what the record names cannot be kept
/// under a key the record made, so only the request that fetched it is served
/// it, and the record is dropped so the next request learns what the source
/// varies on now.
fn copy_loader(lookup: &Lookup<'_>, names: Vec<String>, fetch: Fetch, own: Own) -> Loader {
    let services = lookup.services.clone().without_connection();
    let cache = lookup.cache.clone();
    let address = lookup.address.clone();
    let not_shareable = lookup.not_shareable.clone();
    Box::new(move || {
        Box::pin(async move {
            let fetched = fetch().await?;
            keep_own(&own, &fetched.copy);
            if fetched.vary != SourceVary::Headers(names) {
                log::debug!(
                    "source_cache: the source no longer varies as its record says, so the \
                     record is dropped"
                );
                services
                    .loading_cache_for(cache.records.name)
                    .remove(&services, &cache.records, &address.record_key(&cache))
                    .await;
                return Ok(LoadedValue::new(not_shareable).with_lifetime(Duration::ZERO));
            }
            Ok(LoadedValue::new(fetched.copy).with_lifetime(fetched.lifetime))
        })
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use http::HeaderValue;

    use super::*;
    use trusted_server_core::platform::{
        CacheLifetimes,
        test_support::{MemoryLoadingCache, StubHttpClient, build_services_with_http_client},
    };

    fn services(cache: Arc<MemoryLoadingCache>) -> RuntimeServices {
        build_services_with_http_client(Arc::new(StubHttpClient::new())).with_loading_cache(cache)
    }

    fn spec(name: &'static str) -> CacheSpec {
        CacheSpec {
            name,
            lifetimes: CacheLifetimes {
                memory: Duration::from_secs(5),
                store: Duration::from_secs(60),
                store_idle: Duration::from_secs(60),
            },
            memory_entries: 8,
        }
    }

    fn cache() -> SourceCache {
        SourceCache {
            records: spec("test-vary"),
            copies: spec("test"),
        }
    }

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.append(
                HeaderName::from_static(name),
                HeaderValue::from_str(value).expect("should be a header value"),
            );
        }
        headers
    }

    /// A source answering with `vary`, whose copy names the edition the
    /// request asked for, counting its fetches.
    fn source(vary: SourceVary, edition: &str, fetches: &Arc<AtomicUsize>) -> Fetch {
        let fetches = Arc::clone(fetches);
        let copy = Bytes::from(format!("edition {edition}"));
        Arc::new(move || {
            let fetches = Arc::clone(&fetches);
            let vary = vary.clone();
            let copy = copy.clone();
            Box::pin(async move {
                fetches.fetch_add(1, Ordering::SeqCst);
                Ok(Fetched {
                    vary,
                    copy,
                    lifetime: Duration::from_secs(30),
                })
            })
        })
    }

    fn copy(found: Found) -> Option<Bytes> {
        match found {
            Found::Copy { value, .. } => Some(value),
            Found::NoCopies => None,
        }
    }

    async fn look_up(
        services: &RuntimeServices,
        request: &HeaderMap,
        declared: Option<&str>,
        fetch: Fetch,
    ) -> Found {
        let address = SourceAddress::new(
            ServerKeyPrefix::new("ts", "example.com"),
            "address".to_owned(),
        );
        get_or_fetch(
            &Lookup {
                services,
                cache: &cache(),
                address: &address,
                headers: request,
                declared,
                not_shareable: &Bytes::from_static(b"not shareable"),
            },
            fetch,
        )
        .await
        .expect("the lookup should succeed")
    }

    #[test]
    fn record_and_copy_keys_follow_the_publisher() {
        trusted_server_core::prefix::test_support::assert_keys_follow_the_publisher(|domain| {
            let address =
                SourceAddress::new(ServerKeyPrefix::new("ts", domain), "address".to_owned());
            format!(
                "{} {}",
                address.record_key(&cache()),
                address.copy_key(&cache(), "variant")
            )
        });
    }

    #[test]
    fn vary_names_are_lowercase_sorted_and_named_once() {
        let response = headers(&[
            ("vary", "X-Edition, Accept-Encoding"),
            ("vary", "x-edition"),
        ]);

        assert_eq!(
            SourceVary::of(&response),
            SourceVary::Headers(vec!["accept-encoding".to_owned(), "x-edition".to_owned()])
        );
        assert_eq!(
            SourceVary::of(&response).without(&header::ACCEPT_ENCODING),
            SourceVary::Headers(vec!["x-edition".to_owned()]),
            "a header that never selects a copy is left out"
        );
        assert_eq!(
            SourceVary::of(&HeaderMap::new()),
            SourceVary::Headers(Vec::new())
        );
    }

    #[test]
    fn a_wildcard_a_cookie_or_an_unreadable_vary_allows_no_copies() {
        for (vary, expected) in [
            ("*", SourceVary::Any),
            ("x-edition, *", SourceVary::Any),
            ("Cookie", SourceVary::Cookie),
            ("x edition", SourceVary::Unreadable),
        ] {
            assert_eq!(
                SourceVary::of(&headers(&[("vary", vary)])),
                expected,
                "{vary}"
            );
        }
    }

    #[test]
    fn a_record_reads_back_as_written_and_a_foreign_one_does_not_read() {
        for record in [
            AddressRecord::VariesOn(Vec::new()),
            AddressRecord::VariesOn(vec!["accept-language".to_owned(), "x-edition".to_owned()]),
            AddressRecord::NoCopies,
        ] {
            assert_eq!(AddressRecord::decode(&record.encode()), Some(record));
        }
        for foreign in [&b"v2:x-edition"[..], b"", b"v1:X-Edition", b"v1:x edition"] {
            assert_eq!(AddressRecord::decode(foreign), None, "{foreign:?}");
        }
    }

    #[test]
    fn a_variant_tells_absent_empty_and_reordered_values_apart() {
        let names = vec!["x-edition".to_owned()];
        let digest = |pairs: &[(&'static str, &str)]| variant(&names, &headers(pairs), None);

        let absent = digest(&[]);
        let empty = digest(&[("x-edition", "")]);
        let one_then_two = digest(&[("x-edition", "one"), ("x-edition", "two")]);
        let two_then_one = digest(&[("x-edition", "two"), ("x-edition", "one")]);

        assert_ne!(absent, empty);
        assert_ne!(one_then_two, two_then_one);
        assert_eq!(
            digest(&[("x-edition", "one"), ("x-other", "a")]),
            digest(&[("x-edition", "one"), ("x-other", "b")]),
            "a header the source does not vary on does not separate copies"
        );
        assert_ne!(
            variant(&names, &HeaderMap::new(), Some("token")),
            variant(&names, &HeaderMap::new(), None),
            "what the publisher's rule declares separates copies"
        );
    }

    #[tokio::test]
    async fn the_first_response_records_its_vary_and_each_value_gets_its_own_copy() {
        let memory = Arc::new(MemoryLoadingCache::default());
        let services = services(Arc::clone(&memory));
        let fetches = Arc::new(AtomicUsize::new(0));
        let vary = SourceVary::Headers(vec!["x-edition".to_owned()]);
        let red = headers(&[("x-edition", "red")]);
        let blue = headers(&[("x-edition", "blue")]);

        let first = look_up(&services, &red, None, source(vary.clone(), "red", &fetches)).await;
        let again = look_up(&services, &red, None, source(vary.clone(), "red", &fetches)).await;
        let other = look_up(
            &services,
            &blue,
            None,
            source(vary.clone(), "blue", &fetches),
        )
        .await;
        let other_again = look_up(&services, &blue, None, source(vary, "blue", &fetches)).await;

        assert_eq!(copy(first), Some(Bytes::from("edition red")));
        assert_eq!(copy(again), Some(Bytes::from("edition red")));
        assert_eq!(copy(other), Some(Bytes::from("edition blue")));
        assert_eq!(copy(other_again), Some(Bytes::from("edition blue")));
        assert_eq!(
            fetches.load(Ordering::SeqCst),
            2,
            "each value is fetched once, and the first fetch also made the record"
        );
    }

    #[tokio::test]
    async fn a_response_that_allows_no_copies_has_every_request_fetch_its_own() {
        let memory = Arc::new(MemoryLoadingCache::default());
        let services = services(Arc::clone(&memory));
        let fetches = Arc::new(AtomicUsize::new(0));
        let request = headers(&[("x-edition", "red")]);

        for _ in 0..2 {
            let found = look_up(
                &services,
                &request,
                None,
                source(SourceVary::Any, "red", &fetches),
            )
            .await;
            assert_eq!(found, Found::NoCopies);
        }
        assert_eq!(
            fetches.load(Ordering::SeqCst),
            1,
            "the record saying no copies are kept is itself kept"
        );
    }

    #[tokio::test]
    async fn a_source_that_changes_its_vary_has_its_record_dropped() {
        let memory = Arc::new(MemoryLoadingCache::default());
        let services = services(Arc::clone(&memory));
        let fetches = Arc::new(AtomicUsize::new(0));
        let red = headers(&[("x-edition", "red")]);
        let blue = headers(&[("x-edition", "blue")]);
        look_up(
            &services,
            &red,
            None,
            source(
                SourceVary::Headers(vec!["x-edition".to_owned()]),
                "red",
                &fetches,
            ),
        )
        .await;

        let changed = look_up(
            &services,
            &blue,
            None,
            source(
                SourceVary::Headers(vec!["x-region".to_owned()]),
                "blue",
                &fetches,
            ),
        )
        .await;

        assert_eq!(
            copy(changed),
            Some(Bytes::from("edition blue")),
            "the request that fetched a copy made under another Vary is served it"
        );
        assert_eq!(
            fetches.load(Ordering::SeqCst),
            2,
            "that request fetches the source once"
        );
        let keys = memory.keys();
        assert!(
            keys.iter()
                .filter_map(|key| memory.held(key))
                .all(|(value, _)| value != "edition blue"),
            "no other request is served that copy"
        );
        assert!(
            keys.iter().all(|key| !key.contains("/test-vary/")),
            "the record is dropped so the next request learns the new Vary: {keys:?}"
        );
    }

    #[tokio::test]
    async fn the_publishers_rule_separates_copies_the_source_does_not() {
        let memory = Arc::new(MemoryLoadingCache::default());
        let services = services(Arc::clone(&memory));
        let fetches = Arc::new(AtomicUsize::new(0));
        let request = HeaderMap::new();
        let nothing = SourceVary::Headers(Vec::new());

        let uk = look_up(
            &services,
            &request,
            Some("uk"),
            source(nothing.clone(), "uk", &fetches),
        )
        .await;
        let us = look_up(
            &services,
            &request,
            Some("us"),
            source(nothing.clone(), "us", &fetches),
        )
        .await;
        let uk_again = look_up(
            &services,
            &request,
            Some("uk"),
            source(nothing, "x", &fetches),
        )
        .await;

        assert_eq!(copy(uk), Some(Bytes::from("edition uk")));
        assert_eq!(copy(us), Some(Bytes::from("edition us")));
        assert_eq!(copy(uk_again), Some(Bytes::from("edition uk")));
        assert_eq!(fetches.load(Ordering::SeqCst), 2);
    }
}
