//! Fastly Core Cache backing for the shared transformed-template cache.
//!
//! Only the Fastly adapter implements this; every other adapter uses
//! `UnavailableTemplateCache`, so the ESI assembly mode stays portable and only
//! the caching is Fastly-only.
//!
//! **Why Core Cache and not read-through caching.** Read-through with `after_send` +
//! `set_body_transform` looks like a better fit — it keeps HTTP semantics and derives
//! TTL and surrogate keys from origin headers for free. It is unreachable here:
//! Viceroy 0.17 stubs the entire HTTP Cache ABI and the SDK converts that into a
//! *send error*, so setting `after_send` makes every publisher origin fetch fail
//! under `fastly compute serve`, `cargo test-fastly` and the parity suite. It is also
//! silently dead whenever the origin request is in pass mode, and its closure bounds
//! (`Fn + Send + Sync`) are incompatible with a platform layer that is `!Send` by
//! construction. Recorded in the spike plan's Task 3 Step 4 so nobody re-proposes it.
//!
//! Spike-only. Remove with the spike.

use fastly::cache::core::{CacheKey, Found, Transaction};
use std::io::Write as _;
use std::time::Duration;
use trusted_server_core::platform::{
    PlatformTemplateCache, PlatformTemplateCacheReservation,
    TEMPLATE_CACHE_PURGE_ALL_SURROGATE_KEY, TemplateCacheError, TemplateCacheKey,
    TemplateCacheLookup, TemplateCacheMiss, TemplateCacheReservation, TemplateEntry,
    TemplateMetadata,
};

/// Fastly Core Cache implementation of the shared template cache.
#[derive(Default)]
pub struct FastlyTemplateCache;

impl FastlyTemplateCache {
    /// Create the Fastly Core Cache implementation.
    ///
    /// Entry lifetime is supplied per insert after core validates origin freshness
    /// and applies the operator's configured safety ceiling.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

fn backend_error(message: impl Into<String>) -> TemplateCacheError {
    TemplateCacheError::Backend {
        message: message.into(),
    }
}

enum ReadFoundError {
    Invalid(TemplateCacheMiss),
    Backend(TemplateCacheError),
}

fn read_cache_body(mut reader: impl std::io::Read) -> Result<Vec<u8>, ReadFoundError> {
    let mut body = Vec::new();
    reader
        .read_to_end(&mut body)
        .map_err(|_| ReadFoundError::Invalid(TemplateCacheMiss::Truncated))?;
    Ok(body)
}

fn read_found(found: &Found, key: &TemplateCacheKey) -> Result<TemplateEntry, ReadFoundError> {
    if found.is_stale() {
        return Err(ReadFoundError::Invalid(TemplateCacheMiss::NotFound));
    }

    let metadata = TemplateMetadata::decode(&found.user_metadata()).ok_or(
        ReadFoundError::Invalid(TemplateCacheMiss::UnreadableMetadata),
    )?;
    if metadata.schema_version != key.schema_version {
        return Err(ReadFoundError::Invalid(TemplateCacheMiss::SchemaMismatch));
    }
    if found
        .known_length()
        .is_some_and(|length| length != metadata.body_len)
    {
        return Err(ReadFoundError::Invalid(TemplateCacheMiss::Truncated));
    }

    let stream = found.to_stream().map_err(|error| {
        ReadFoundError::Backend(backend_error(format!(
            "opening cached template body failed: {error:?}"
        )))
    })?;
    let body = read_cache_body(stream)?;
    if body.len() as u64 != metadata.body_len {
        return Err(ReadFoundError::Invalid(TemplateCacheMiss::Truncated));
    }
    Ok(TemplateEntry { metadata, body })
}

struct FastlyTemplateReservation {
    transaction: Transaction,
    surrogate_keys: Vec<String>,
}

impl PlatformTemplateCacheReservation for FastlyTemplateReservation {
    fn insert(
        self: Box<Self>,
        metadata: &TemplateMetadata,
        body: Vec<u8>,
        max_age: Duration,
    ) -> Result<(), TemplateCacheError> {
        if metadata.body_len != body.len() as u64 {
            return Err(backend_error(format!(
                "metadata body_len {} does not match the {} bytes supplied",
                metadata.body_len,
                body.len()
            )));
        }
        let encoded_metadata = metadata.encode().map_err(|error| {
            backend_error(format!("encoding template metadata failed: {error}"))
        })?;

        let mut writer = self
            .transaction
            .insert(max_age)
            .surrogate_keys(self.surrogate_keys.iter().map(String::as_str))
            .known_length(body.len() as u64)
            .user_metadata(encoded_metadata.into())
            .execute()
            .map_err(|e| backend_error(format!("cache insert failed: {e:?}")))?;
        writer
            .write_all(&body)
            .map_err(|e| backend_error(format!("writing template body failed: {e}")))?;
        writer
            .finish()
            .map_err(|e| backend_error(format!("finishing the cached template failed: {e}")))?;
        Ok(())
    }

    fn cancel(self: Box<Self>) -> Result<(), TemplateCacheError> {
        self.transaction
            .cancel_insert_or_update()
            .map_err(|e| backend_error(format!("cancelling cache reservation failed: {e:?}")))
    }
}

#[async_trait::async_trait(?Send)]
impl PlatformTemplateCache for FastlyTemplateCache {
    async fn lookup_or_reserve(
        &self,
        key: &TemplateCacheKey,
    ) -> Result<TemplateCacheLookup, TemplateCacheError> {
        let transaction = Transaction::lookup(CacheKey::from(key.to_cache_key().into_bytes()))
            .execute()
            .map_err(|e| backend_error(format!("transactional lookup failed: {e:?}")))?;

        if transaction.must_insert_or_update() {
            return Ok(TemplateCacheLookup::Reserved(
                TemplateCacheReservation::new(Box::new(FastlyTemplateReservation {
                    transaction,
                    surrogate_keys: key.surrogate_keys(),
                })),
            ));
        }

        let found = transaction.found().ok_or_else(|| {
            backend_error("transaction returned neither a hit nor an insert obligation")
        })?;
        Ok(match read_found(&found, key) {
            Ok(entry) => TemplateCacheLookup::Hit(entry),
            Err(ReadFoundError::Invalid(miss)) => TemplateCacheLookup::Invalid(miss),
            Err(ReadFoundError::Backend(error)) => return Err(error),
        })
    }

    async fn get(&self, key: &TemplateCacheKey) -> Result<TemplateEntry, TemplateCacheMiss> {
        let cache_key = CacheKey::from(key.to_cache_key().into_bytes());

        // A plain lookup, not a transaction: a read that does not intend to insert
        // must not take an insert obligation it will never discharge, which would
        // block every other client waiting on the same key until they time out.
        let found = fastly::cache::core::lookup(cache_key)
            .execute()
            .map_err(|_| TemplateCacheMiss::NotFound)?
            .ok_or(TemplateCacheMiss::NotFound)?;

        read_found(&found, key).map_err(|error| match error {
            ReadFoundError::Invalid(miss) => miss,
            ReadFoundError::Backend(error) => {
                // This legacy method cannot expose a backend error. Production uses
                // `lookup_or_reserve`, which preserves it for bounded diagnostics.
                log::warn!("template_cache legacy read failed: {error}");
                TemplateCacheMiss::NotFound
            }
        })
    }

    async fn put(
        &self,
        key: &TemplateCacheKey,
        metadata: &TemplateMetadata,
        body: Vec<u8>,
        max_age: Duration,
    ) -> Result<(), TemplateCacheError> {
        if metadata.body_len != body.len() as u64 {
            return Err(backend_error(format!(
                "metadata body_len {} does not match the {} bytes supplied; storing \
                 this would make every read a truncation miss",
                metadata.body_len,
                body.len()
            )));
        }
        let encoded_metadata = metadata.encode().map_err(|error| {
            backend_error(format!("encoding template metadata failed: {error}"))
        })?;

        let cache_key = CacheKey::from(key.to_cache_key().into_bytes());

        // Transactional insert so a cold key under load transforms once rather than
        // once per concurrent request.
        let tx = Transaction::lookup(cache_key)
            .execute()
            .map_err(|e| backend_error(format!("transactional lookup failed: {e:?}")))?;

        // Order matters. A STALE entry sets *both* `found()` and
        // `must_insert_or_update()`. Testing `found()` first would return early on
        // the stale bytes and never discharge the obligation, leaving every
        // concurrent waiter blocked until timeout.
        if !tx.must_insert_or_update() {
            // Someone else already inserted a fresh entry. Nothing to do, and
            // nothing to discharge.
            return Ok(());
        }

        // `Transaction::insert` takes `self`, so from here there is no handle left to
        // cancel the insert with. A write that fails part-way therefore cannot be
        // retracted — which is why `TemplateMetadata::body_len` exists and `get`
        // checks it. The metadata is written before the body, so a truncated entry
        // still carries the length it was supposed to have.
        let surrogate_keys = key.surrogate_keys();
        let mut writer = tx
            .insert(max_age)
            .surrogate_keys(surrogate_keys.iter().map(String::as_str))
            .known_length(body.len() as u64)
            .user_metadata(encoded_metadata.into())
            .execute()
            .map_err(|e| backend_error(format!("cache insert failed: {e:?}")))?;

        if let Err(e) = writer.write_all(&body) {
            // Deliberately not calling `finish()`. An unfinished entry has no known
            // length, and even if it is observable, `get`'s length check rejects it.
            return Err(backend_error(format!("writing template body failed: {e}")));
        }

        // Required. Without it the object never completes and its length stays
        // unknown, so readers see a partial or absent entry.
        writer
            .finish()
            .map_err(|e| backend_error(format!("finishing the cached template failed: {e}")))?;

        Ok(())
    }

    async fn purge_url(&self, key: &TemplateCacheKey) -> Result<(), TemplateCacheError> {
        fastly::http::purge::purge_surrogate_key(&key.url_surrogate_key())
            .map_err(|e| backend_error(format!("purging invalid template failed: {e:?}")))
    }

    async fn purge_all(&self) -> Result<(), TemplateCacheError> {
        fastly::http::purge::purge_surrogate_key(TEMPLATE_CACHE_PURGE_ALL_SURROGATE_KEY)
            .map_err(|e| backend_error(format!("purging templates failed: {e:?}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use trusted_server_core::creative_opportunities::AssemblyMode;
    use trusted_server_core::platform::TEMPLATE_SCHEMA_VERSION;

    struct FailingReader {
        returned_prefix: bool,
    }

    impl io::Read for FailingReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.returned_prefix {
                return Err(io::Error::other("abandoned cache stream"));
            }
            self.returned_prefix = true;
            buffer[..3].copy_from_slice(b"abc");
            Ok(3)
        }
    }

    /// Distinct per test, so tests sharing the process cache cannot collide.
    fn key(url: &str) -> TemplateCacheKey {
        TemplateCacheKey {
            url: url.to_string(),
            request_host: "example.com".to_string(),
            request_scheme: "https".to_string(),
            origin_identity: "https://origin.example.com\0origin.example.com".to_string(),
            assembly_mode: AssemblyMode::Esi,
            vary_values: vec![trusted_server_core::platform::VaryHeaderValues {
                name: "rsc".to_string(),
                values: Some(vec![b"1".to_vec()]),
            }],
            template_fingerprint: "fp".to_string(),
            schema_version: TEMPLATE_SCHEMA_VERSION,
        }
    }

    fn metadata_for(body: &[u8]) -> TemplateMetadata {
        TemplateMetadata {
            policy_headers: Vec::new(),
            content_encoding: "identity".to_string(),
            content_type: "text/html; charset=utf-8".to_string(),
            schema_version: TEMPLATE_SCHEMA_VERSION,
            body_len: body.len() as u64,
        }
    }

    /// The trait is `async_trait(?Send)` and this crate has no async test runtime,
    /// so drive the futures directly.
    fn run<T>(fut: impl core::future::Future<Output = T>) -> T {
        futures::executor::block_on(fut)
    }

    fn cache() -> FastlyTemplateCache {
        FastlyTemplateCache::new()
    }

    #[test]
    fn cache_body_read_error_is_a_truncated_miss() {
        let error = read_cache_body(FailingReader {
            returned_prefix: false,
        })
        .expect_err("should reject an abandoned cache stream");

        assert!(
            matches!(error, ReadFoundError::Invalid(TemplateCacheMiss::Truncated)),
            "should classify a cache stream read failure as truncated"
        );
    }

    #[test]
    fn a_stored_template_reads_back_intact() {
        let cache = cache();
        let key = key("https://example.com/roundtrip");
        let body = b"<html><body>template</body></html>".to_vec();
        let metadata = metadata_for(&body);

        run(cache.put(&key, &metadata, body.clone(), Duration::from_secs(60)))
            .expect("should store");

        let entry = run(cache.get(&key)).expect("should read back");
        assert_eq!(entry.body, body, "bytes must survive the round trip");
        assert_eq!(entry.metadata, metadata, "metadata must survive too");
    }

    #[test]
    fn transactional_lookup_reserves_before_insert_then_hits() {
        let cache = cache();
        let key = key("https://example.com/pre-origin-reservation");
        let body = b"<html><body>collapsed</body></html>".to_vec();
        let metadata = metadata_for(&body);

        let reservation = match run(cache.lookup_or_reserve(&key)).expect("lookup should work") {
            TemplateCacheLookup::Reserved(reservation) => reservation,
            _ => panic!("a cold transactional lookup must assign the insert obligation"),
        };
        reservation
            .insert(&metadata, body.clone(), Duration::from_secs(17))
            .expect("reservation should insert");

        match run(cache.lookup_or_reserve(&key)).expect("warm lookup should work") {
            TemplateCacheLookup::Hit(entry) => assert_eq!(entry.body, body),
            _ => panic!("the next transactional lookup must see the inserted template"),
        }
    }

    #[test]
    fn an_absent_key_is_a_miss_not_an_error() {
        let miss =
            run(cache().get(&key("https://example.com/never-stored"))).expect_err("should miss");
        assert_eq!(miss, TemplateCacheMiss::NotFound);
    }

    #[test]
    fn a_different_assembly_mode_does_not_read_the_same_entry() {
        // The arms emit different bytes. If they shared an entry, one would serve
        // the other's template.
        let cache = cache();
        let esi = key("https://example.com/mode-split");
        let mut inline = esi.clone();
        inline.assembly_mode = AssemblyMode::Inline;

        let body = b"esi-template".to_vec();
        run(cache.put(&esi, &metadata_for(&body), body, Duration::from_secs(60)))
            .expect("should store");

        assert_eq!(
            run(cache.get(&inline)).err(),
            Some(TemplateCacheMiss::NotFound),
            "inline must not read the ESI arm's template"
        );
    }

    #[test]
    fn a_schema_bump_reads_a_miss_rather_than_a_stale_shape() {
        let cache = cache();
        let key_v1 = key("https://example.com/schema");
        let body = b"old-shape".to_vec();
        run(cache.put(&key_v1, &metadata_for(&body), body, Duration::from_secs(60)))
            .expect("should store");

        // A deploy that changes the transform bumps the constant. The old entry must
        // not be assembled against.
        let mut key_v2 = key_v1.clone();
        key_v2.schema_version = TEMPLATE_SCHEMA_VERSION + 1;

        assert_eq!(
            run(cache.get(&key_v2)).err(),
            Some(TemplateCacheMiss::NotFound),
            "a bumped schema changes the key, so the old entry is simply not found"
        );
    }

    #[test]
    fn a_stale_but_present_entry_reads_as_a_miss_rather_than_being_served() {
        // Stale-while-revalidate is a real option and deliberately not taken: it is a
        // state machine `cache::core` does not implement for you, and serving stale here
        // means serving a template built by an older transform or an older JS bundle.
        //
        // The entry has to be *present and stale*, not merely expired. A zero TTL with no
        // `stale_while_revalidate` window is simply absent, so a test written that way
        // passes without ever reaching `is_stale()` — verified: reverting the staleness
        // check left that version green. The revalidate window is what keeps the object
        // readable while stale, so this actually exercises the branch.
        let key = key("https://example.com/stale");
        let body = b"stale-template".to_vec();
        let metadata = metadata_for(&body);
        let cache_key = CacheKey::from(key.to_cache_key().into_bytes());

        let mut writer = fastly::cache::core::insert(cache_key, Duration::from_secs(0))
            .stale_while_revalidate(Duration::from_secs(60))
            .user_metadata(
                metadata
                    .encode()
                    .expect("valid metadata should encode")
                    .into(),
            )
            .execute()
            .expect("should begin insert");
        writer.write_all(&body).expect("should write body");
        writer.finish().expect("should finish insert");

        let miss = run(cache().get(&key)).expect_err("a stale template must not be served");
        assert_eq!(miss, TemplateCacheMiss::NotFound);
    }

    #[test]
    fn purge_all_clears_stored_templates() {
        // The rollback lever. Without this, backing out a bad template means waiting
        // for the TTL.
        let cache = cache();
        let key = key("https://example.com/purge");
        let body = b"template".to_vec();
        run(cache.put(&key, &metadata_for(&body), body, Duration::from_secs(60)))
            .expect("should store");
        run(cache.get(&key)).expect("should be present before purge");

        run(cache.purge_all()).expect("should purge");

        assert!(
            run(cache.get(&key)).is_err(),
            "purge must clear the template, or rollback is TTL-bound"
        );
    }

    #[test]
    fn a_second_put_on_a_fresh_entry_is_a_no_op() {
        // Exercises the `must_insert_or_update` early return: a concurrent writer
        // that finds a fresh entry must neither error nor overwrite.
        let cache = cache();
        let key = key("https://example.com/second-put");
        let first = b"first".to_vec();
        run(cache.put(
            &key,
            &metadata_for(&first),
            first.clone(),
            Duration::from_secs(60),
        ))
        .expect("first put stores");

        let second = b"second".to_vec();
        run(cache.put(
            &key,
            &metadata_for(&second),
            second,
            Duration::from_secs(60),
        ))
        .expect("second put should be a no-op, not an error");

        assert_eq!(
            run(cache.get(&key)).expect("should read").body,
            first,
            "a fresh entry must not be overwritten by a racing writer"
        );
    }

    #[test]
    fn the_cache_round_trips_through_the_platform_trait_object() {
        // Every other test here calls `FastlyTemplateCache` concretely. The publisher
        // never does — it reaches the cache as a `dyn PlatformTemplateCache` behind
        // `RuntimeServices`. That join is what `app.rs` wires, and until this test it
        // was only type-checked, never executed.
        let cache: std::sync::Arc<dyn PlatformTemplateCache> = std::sync::Arc::new(cache());
        let key = key("https://example.com/via-trait-object");
        let body = b"<html><body>template</body></html>".to_vec();

        run(cache.put(
            &key,
            &metadata_for(&body),
            body.clone(),
            Duration::from_secs(60),
        ))
        .expect("should store");

        assert_eq!(
            run(cache.get(&key)).expect("should read back").body,
            body,
            "the trait object must reach the same Core Cache the concrete type does"
        );
    }

    #[test]
    fn a_length_mismatch_is_refused_at_write_rather_than_stored() {
        // Storing metadata whose length disagrees with the body would make every
        // subsequent read a truncation miss — a cache that silently never hits.
        // Catch it at the write instead.
        let cache = cache();
        let key = key("https://example.com/length-mismatch");
        let mut metadata = metadata_for(b"12345");
        metadata.body_len = 999;

        let err = run(cache.put(&key, &metadata, b"12345".to_vec(), Duration::from_secs(60)))
            .expect_err("a length mismatch must be refused");
        assert!(
            matches!(err, TemplateCacheError::Backend { .. }),
            "expected a backend error, got {err:?}"
        );
    }
}
