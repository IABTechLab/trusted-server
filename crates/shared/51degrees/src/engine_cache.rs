//! The cloud service's answers, loaded through the loading cache core
//! declares.
//!
//! Where a deployment links a loading cache module, each answer is held in
//! the host's shared store, which outlives an instance that serves one
//! request at a time, so the answer serves the next equivalent request and
//! any second lookup in the same request. A stock build links none, so there
//! every call asks the service.
//!
//! Each whole call is the loader, so where a loading cache module is linked a
//! question many requests ask at the same moment is asked of the service once,
//! and every request waiting on it gets the answer or the failure. A failure
//! is not held. The key is a hash of the endpoint, the license key, the
//! properties named and the evidence sent, so an answer is only ever given
//! back to the same question, and the license key is never written into a
//! store.
//!
//! The evidence is the `User-Agent` and the `Sec-CH-UA` hints, with no address
//! and no cookie, so one entry serves every reader whose browser sends the
//! same values.
//!
//! The cache is [`CACHE`], which the 51Degrees builder declares, and a
//! deployment selects the loading cache that holds it and sizes its copies in
//! `[cache.51degrees-answers]`, as for every declared cache.

use std::time::Duration;

use bytes::Bytes;
use sha2::{Digest as _, Sha256};
use trusted_server_core::platform::{
    CacheLifetimes, CacheSpec, LoadError, Loader, RuntimeServices, StoreKey,
};
use trusted_server_core::settings::{LoadingCacheConfig, Settings};

/// The cache the cloud service's answers are held in, and the name of its
/// `[cache.<name>]` table.
pub const CACHE: &str = "51degrees-answers";

/// How long an answer is held, an hour, after which the service is asked
/// again.
pub(crate) const ANSWER_LIFETIME: Duration = Duration::from_secs(60 * 60);

/// What a key's hash covers, so a change to it does not read old entries.
const KEY_FORMAT: u32 = 1;

/// The lifetimes `[cache.51degrees-answers]` leaves unset. Each answer's own
/// lifetime ends it sooner than the store lifetime.
const DEFAULT_LIFETIMES: CacheLifetimes = CacheLifetimes {
    memory: Duration::from_secs(60),
    store: Duration::from_secs(7 * 24 * 60 * 60),
    store_idle: Duration::from_secs(24 * 60 * 60),
};

/// One deployment's cache of cloud answers, with the lifetimes
/// `[cache.51degrees-answers]` sets. Each answer is held under the prefix of
/// the publisher the request's services carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnswerCache {
    spec: CacheSpec,
}

impl AnswerCache {
    /// The cache with the lifetimes `config` sets.
    #[must_use]
    pub fn new(config: &LoadingCacheConfig) -> Self {
        Self {
            spec: CacheSpec {
                name: CACHE,
                lifetimes: config.lifetimes(DEFAULT_LIFETIMES),
                memory_entries: 4096,
            },
        }
    }

    /// The cache with the lifetimes the settings' `[cache.51degrees-answers]`
    /// sets, each its default where the table leaves it unset or there is no
    /// table.
    #[must_use]
    pub fn for_settings(settings: &Settings) -> Self {
        let config = settings
            .cache
            .declared
            .get(CACHE)
            .cloned()
            .unwrap_or_default();
        Self::new(&config)
    }

    /// The store key `key` is held under, for the publisher the request's
    /// services carry.
    pub(crate) fn store_key(services: &RuntimeServices, key: String) -> StoreKey {
        StoreKey::new(services.server_key_prefix(), CACHE, key)
    }

    /// The bytes held for `key`, or those `load` gives.
    pub(crate) async fn held(
        &self,
        services: &RuntimeServices,
        key: String,
        load: Loader,
    ) -> Result<Bytes, LoadError> {
        services
            .load_cached(&self.spec, &Self::store_key(services, key), load)
            .await
    }
}

/// Hashes the parts of a key, each length-prefixed so no two lists of parts
/// share a hash, into the text a store key ends with.
fn hashed(kind: &str, parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(KEY_FORMAT.to_be_bytes());
    for part in std::iter::once(kind).chain(parts.iter().copied()) {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    let mut key = format!("{kind}-");
    for byte in hasher.finalize() {
        key.push_str(&format!("{byte:02x}"));
    }
    key
}

/// The key one question is held under: the endpoint's base, the credential,
/// the properties named and the evidence sent, in any order.
pub(crate) fn answer_key(
    base: &str,
    license_key: &str,
    values: &[&str],
    evidence: &[(String, String)],
) -> String {
    let mut values: Vec<&str> = values.to_vec();
    values.sort_unstable();
    values.dedup();
    let mut evidence: Vec<&(String, String)> = evidence.iter().collect();
    evidence.sort();
    let mut parts = vec![base, license_key, "values"];
    parts.extend(values);
    parts.push("evidence");
    for (name, value) in evidence {
        parts.push(name);
        parts.push(value);
    }
    hashed("answer", &parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "https://cloud.example.com/api/v4/";

    fn evidence(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn the_credential_never_reaches_the_store_key() {
        let key = answer_key(
            BASE,
            "a-real-secret",
            &["device.X"],
            &evidence(&[("user-agent", "Mozilla")]),
        );

        assert!(!key.contains("a-real-secret"), "{key}");
        assert!(key.starts_with("answer-"), "{key}");
    }

    #[test]
    fn a_store_key_fits_the_platform_limit() {
        // A long evidence set still has to produce a key the store accepts.
        let long = "Mozilla/5.0 ".repeat(400);
        let key = answer_key(
            BASE,
            "k",
            &["device.X"],
            &evidence(&[("user-agent", &long)]),
        );
        let stored = StoreKey::new(
            &trusted_server_core::platform::ServerKeyPrefix::new(
                "ts",
                "a-long-publisher-name.example",
            ),
            CACHE,
            key,
        );

        assert!(stored.to_string().len() <= 512, "{stored}");
    }

    /// Every answer is held under the prefix of the publisher the request's
    /// services carry.
    #[test]
    fn an_answer_is_held_under_the_publishers_prefix() {
        trusted_server_core::prefix::test_support::assert_keys_follow_the_publisher(|domain| {
            let services = crate::test_services::services_with(
                std::sync::Arc::new(crate::test_services::ScriptedHttpClient::new(&[])),
                std::sync::Arc::default(),
            )
            .with_prefixes(trusted_server_core::prefix::test_support::trusted_server_for(domain));
            AnswerCache::store_key(&services, answer_key(BASE, "k", &["device.X"], &[])).to_string()
        });
    }

    #[test]
    fn different_questions_do_not_share_an_entry() {
        let one = answer_key(BASE, "k", &["device.X"], &evidence(&[("ua", "A")]));

        assert_ne!(
            one,
            answer_key(BASE, "k", &["device.X"], &evidence(&[("ua", "B")])),
            "different evidence is a different entry"
        );
        assert_ne!(
            one,
            answer_key(BASE, "k", &["device.Y"], &evidence(&[("ua", "A")])),
            "a different property list is a different entry"
        );
        assert_ne!(
            one,
            answer_key(BASE, "other", &["device.X"], &evidence(&[("ua", "A")])),
            "another credential is a different entry"
        );
        assert_ne!(
            answer_key(BASE, "k", &["ab", "c"], &[]),
            answer_key(BASE, "k", &["a", "bc"], &[]),
            "parts that run together are still told apart"
        );
    }

    #[test]
    fn the_same_question_is_the_same_entry_in_any_order() {
        assert_eq!(
            answer_key(
                BASE,
                "k",
                &["device.X", "ip.Y"],
                &evidence(&[("a", "1"), ("b", "2")])
            ),
            answer_key(
                BASE,
                "k",
                &["ip.Y", "device.X"],
                &evidence(&[("b", "2"), ("a", "1")])
            ),
        );
    }

    #[test]
    fn the_cache_takes_its_lifetimes_from_the_settings() {
        let cache = AnswerCache::new(&LoadingCacheConfig {
            module: None,
            memory_seconds: Some(5),
            store_seconds: None,
            store_idle_seconds: Some(120),
        });

        assert_eq!(cache.spec.name, CACHE);
        assert_eq!(
            cache.spec.lifetimes,
            CacheLifetimes {
                memory: Duration::from_secs(5),
                store: DEFAULT_LIFETIMES.store,
                store_idle: Duration::from_secs(120),
            }
        );
    }

    /// The lifetimes come from the cache's own `[cache.51degrees-answers]`
    /// table, as every declared cache's do, and are the defaults without it.
    #[test]
    fn the_cache_is_sized_by_its_own_table() {
        use trusted_server_core::html_processor::test_support::page_settings;

        let sized = AnswerCache::for_settings(&page_settings(&format!(
            "[cache.{CACHE}]\nmemory_seconds = 7\nstore_seconds = 3600\n"
        )));
        let defaulted = AnswerCache::for_settings(&page_settings(""));

        assert_eq!(
            sized.spec.lifetimes,
            CacheLifetimes {
                memory: Duration::from_secs(7),
                store: Duration::from_secs(3600),
                store_idle: DEFAULT_LIFETIMES.store_idle,
            }
        );
        assert_eq!(defaulted.spec.lifetimes, DEFAULT_LIFETIMES);
    }
}
