use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, watch};
use tokio::time::Instant;

use crate::commands::dev::proxy::metrics::ProxyMetrics;

const TTL: Duration = Duration::from_secs(30);
const MAX_ENTRIES: usize = 64;

type ResolveFuture = Pin<Box<dyn Future<Output = io::Result<Vec<SocketAddr>>> + Send>>;

trait Resolver: Send + Sync {
    fn resolve(&self, host: Arc<str>, port: u16) -> ResolveFuture;
}

struct SystemResolver;

impl Resolver for SystemResolver {
    fn resolve(&self, host: Arc<str>, port: u16) -> ResolveFuture {
        Box::pin(async move {
            Ok(tokio::net::lookup_host((host.as_ref(), port))
                .await?
                .collect())
        })
    }
}

#[derive(Clone, Eq, Hash, PartialEq)]
struct CacheKey {
    host: Arc<str>,
    port: u16,
}

#[derive(Clone)]
struct OwnedError {
    kind: io::ErrorKind,
    message: Arc<str>,
}

impl OwnedError {
    fn from_io(error: &io::Error) -> Self {
        Self {
            kind: error.kind(),
            message: Arc::from(error.to_string()),
        }
    }

    fn to_io(&self) -> io::Error {
        io::Error::new(self.kind, self.message.to_string())
    }
}

type SharedResult = Result<Arc<[SocketAddr]>, OwnedError>;

enum Entry {
    Loading {
        generation: u64,
        signal: watch::Sender<Option<SharedResult>>,
    },
    Ready {
        addresses: Arc<[SocketAddr]>,
        expires: Instant,
        used: u64,
    },
}

#[derive(Default)]
struct State {
    entries: HashMap<CacheKey, Entry>,
    sequence: u64,
}

/// Small process-local DNS cache used only while opening pooled connections.
pub struct DnsCache {
    state: Arc<Mutex<State>>,
    enabled: bool,
    resolver: Arc<dyn Resolver>,
}

impl Default for DnsCache {
    fn default() -> Self {
        Self::new(true)
    }
}

impl DnsCache {
    #[must_use]
    pub fn new(enabled: bool) -> Self {
        Self::with_resolver_and_mode(Arc::new(SystemResolver), enabled)
    }

    #[cfg(test)]
    fn with_resolver(resolver: Arc<dyn Resolver>) -> Self {
        Self::with_resolver_and_mode(resolver, true)
    }

    fn with_resolver_and_mode(resolver: Arc<dyn Resolver>, enabled: bool) -> Self {
        Self {
            state: Arc::new(Mutex::new(State::default())),
            enabled,
            resolver,
        }
    }

    /// Resolves a logical DNS origin with TTL, LRU eviction, and miss coalescing.
    ///
    /// # Errors
    ///
    /// Returns an owned resolver error; failures are never cached.
    pub async fn lookup(
        &self,
        host: &str,
        port: u16,
        deadline: Instant,
        metrics: Arc<ProxyMetrics>,
    ) -> io::Result<Arc<[SocketAddr]>> {
        let key = CacheKey {
            host: Arc::from(host.to_ascii_lowercase()),
            port,
        };
        if !self.enabled {
            return resolve_uncached(Arc::clone(&self.resolver), &key, deadline, metrics).await;
        }

        enum Action {
            Wait(watch::Receiver<Option<SharedResult>>),
            Start {
                generation: u64,
                receiver: watch::Receiver<Option<SharedResult>>,
                signal: watch::Sender<Option<SharedResult>>,
            },
            Bypass,
        }

        let action = {
            let mut state = self.state.lock().await;
            state.sequence += 1;
            let used = state.sequence;
            match state.entries.get_mut(&key) {
                Some(Entry::Ready {
                    addresses,
                    expires,
                    used: entry_used,
                }) if *expires > Instant::now() => {
                    *entry_used = used;
                    metrics.record_dns_cache_hit();
                    return Ok(Arc::clone(addresses));
                }
                Some(Entry::Loading { signal, .. }) => {
                    metrics.record_dns_cache_hit();
                    Action::Wait(signal.subscribe())
                }
                _ => {
                    state.entries.remove(&key);
                    state.entries.retain(|_, entry| {
                        !matches!(entry, Entry::Ready { expires, .. } if *expires <= Instant::now())
                    });
                    if state.entries.len() >= MAX_ENTRIES
                        && let Some(evict) = state
                            .entries
                            .iter()
                            .filter_map(|(key, entry)| match entry {
                                Entry::Ready { used, .. } => Some((key.clone(), *used)),
                                Entry::Loading { .. } => None,
                            })
                            .min_by_key(|(_, used)| *used)
                            .map(|(key, _)| key)
                    {
                        state.entries.remove(&evict);
                    }
                    if state.entries.len() >= MAX_ENTRIES {
                        Action::Bypass
                    } else {
                        state.sequence += 1;
                        let generation = state.sequence;
                        let (signal, receiver) = watch::channel(None);
                        state.entries.insert(
                            key.clone(),
                            Entry::Loading {
                                generation,
                                signal: signal.clone(),
                            },
                        );
                        Action::Start {
                            generation,
                            receiver,
                            signal,
                        }
                    }
                }
            }
        };

        let receiver = match action {
            Action::Wait(receiver) => receiver,
            Action::Start {
                generation,
                receiver,
                signal,
            } => {
                metrics.record_dns_cache_miss();
                tokio::spawn(run_lookup(
                    Arc::clone(&self.state),
                    Arc::clone(&self.resolver),
                    key,
                    generation,
                    deadline,
                    Arc::clone(&metrics),
                    signal,
                ));
                receiver
            }
            Action::Bypass => {
                return resolve_uncached(Arc::clone(&self.resolver), &key, deadline, metrics).await;
            }
        };
        wait_for_result(receiver, deadline).await
    }
}

async fn wait_for_result(
    mut receiver: watch::Receiver<Option<SharedResult>>,
    deadline: Instant,
) -> io::Result<Arc<[SocketAddr]>> {
    let shared = tokio::time::timeout_at(deadline, async {
        loop {
            if let Some(result) = receiver.borrow().clone() {
                return Ok::<SharedResult, io::Error>(result);
            }
            receiver.changed().await.map_err(|_| {
                io::Error::new(io::ErrorKind::Interrupted, "DNS lookup owner exited")
            })?;
        }
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "coalesced DNS lookup timed out"))??;
    shared.map_err(|error| error.to_io())
}

async fn run_lookup(
    state: Arc<Mutex<State>>,
    resolver: Arc<dyn Resolver>,
    key: CacheKey,
    generation: u64,
    deadline: Instant,
    metrics: Arc<ProxyMetrics>,
    signal: watch::Sender<Option<SharedResult>>,
) {
    let result = resolve(resolver, &key, deadline, &metrics).await;
    let shared = result.as_ref().map(Arc::clone).map_err(OwnedError::from_io);
    let _ = signal.send(Some(shared.clone()));

    let mut state = state.lock().await;
    let still_owner = matches!(
        state.entries.get(&key),
        Some(Entry::Loading { generation: current, .. }) if *current == generation
    );
    if !still_owner {
        return;
    }
    state.sequence += 1;
    let used = state.sequence;
    state.entries.remove(&key);
    if let Ok(addresses) = result {
        state.entries.insert(
            key,
            Entry::Ready {
                addresses,
                expires: Instant::now() + TTL,
                used,
            },
        );
    }
}

async fn resolve_uncached(
    resolver: Arc<dyn Resolver>,
    key: &CacheKey,
    deadline: Instant,
    metrics: Arc<ProxyMetrics>,
) -> io::Result<Arc<[SocketAddr]>> {
    metrics.record_dns_cache_miss();
    resolve(resolver, key, deadline, &metrics).await
}

async fn resolve(
    resolver: Arc<dyn Resolver>,
    key: &CacheKey,
    deadline: Instant,
    metrics: &ProxyMetrics,
) -> io::Result<Arc<[SocketAddr]>> {
    let started = Instant::now();
    let addresses: Arc<[SocketAddr]> =
        tokio::time::timeout_at(deadline, resolver.resolve(Arc::clone(&key.host), key.port))
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "upstream DNS timed out"))??
            .into();
    metrics.record_dns_lookup(started.elapsed());
    if addresses.is_empty() {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "upstream DNS returned no addresses",
        ))
    } else {
        Ok(addresses)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ResolveRequest {
        result: tokio::sync::oneshot::Sender<io::Result<Vec<SocketAddr>>>,
    }

    struct FakeResolver {
        requests: tokio::sync::mpsc::UnboundedSender<ResolveRequest>,
    }

    impl Resolver for FakeResolver {
        fn resolve(&self, _host: Arc<str>, _port: u16) -> ResolveFuture {
            let (result, received) = tokio::sync::oneshot::channel();
            self.requests
                .send(ResolveRequest { result })
                .expect("should observe lookup");
            Box::pin(async move {
                received.await.unwrap_or_else(|_| {
                    Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "fake resolver dropped",
                    ))
                })
            })
        }
    }

    fn fake_cache() -> (
        Arc<DnsCache>,
        tokio::sync::mpsc::UnboundedReceiver<ResolveRequest>,
    ) {
        let (requests, received) = tokio::sync::mpsc::unbounded_channel();
        (
            Arc::new(DnsCache::with_resolver(Arc::new(FakeResolver { requests }))),
            received,
        )
    }

    #[tokio::test]
    async fn caches_success_for_normalized_key() {
        let cache = DnsCache::default();
        let metrics = Arc::new(ProxyMetrics::default());
        let deadline = Instant::now() + Duration::from_secs(2);
        let first = cache
            .lookup("localhost", 80, deadline, Arc::clone(&metrics))
            .await
            .expect("resolve localhost");
        let second = cache
            .lookup("LOCALHOST", 80, deadline, Arc::clone(&metrics))
            .await
            .expect("reuse normalized cache key");
        assert_eq!(first, second);
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.dns_cache_misses, 1);
        assert_eq!(snapshot.dns_cache_hits, 1);
    }

    #[tokio::test]
    async fn concurrent_misses_are_coalesced() {
        let cache = Arc::new(DnsCache::default());
        let metrics = Arc::new(ProxyMetrics::default());
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let cache = Arc::clone(&cache);
            let metrics = Arc::clone(&metrics);
            tasks.push(tokio::spawn(async move {
                cache
                    .lookup(
                        "localhost",
                        80,
                        Instant::now() + Duration::from_secs(2),
                        metrics,
                    )
                    .await
            }));
        }
        for task in tasks {
            task.await.expect("join lookup").expect("resolve localhost");
        }
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.dns_cache_misses, 1);
        assert_eq!(snapshot.dns_cache_hits, 7);
    }

    #[tokio::test]
    async fn cancelling_lookup_owner_does_not_poison_in_flight_key() {
        let (cache, mut requests) = fake_cache();
        let metrics = Arc::new(ProxyMetrics::default());
        let first = tokio::spawn({
            let cache = Arc::clone(&cache);
            let metrics = Arc::clone(&metrics);
            async move {
                cache
                    .lookup(
                        "example.test",
                        443,
                        Instant::now() + Duration::from_secs(5),
                        metrics,
                    )
                    .await
            }
        });
        let request = requests.recv().await.expect("should start one lookup");
        first.abort();

        let second = tokio::spawn({
            let cache = Arc::clone(&cache);
            let metrics = Arc::clone(&metrics);
            async move {
                cache
                    .lookup(
                        "example.test",
                        443,
                        Instant::now() + Duration::from_secs(5),
                        metrics,
                    )
                    .await
            }
        });
        tokio::task::yield_now().await;
        assert!(
            requests.try_recv().is_err(),
            "should coalesce after cancellation"
        );
        request
            .result
            .send(Ok(vec!["127.0.0.1:443".parse().expect("socket")]))
            .expect("should deliver result");
        assert_eq!(second.await.expect("join").expect("shared lookup").len(), 1);
    }

    #[tokio::test]
    async fn failed_lookup_is_fanned_out_once_and_not_cached() {
        let (cache, mut requests) = fake_cache();
        let metrics = Arc::new(ProxyMetrics::default());
        let mut waiters = Vec::new();
        for _ in 0..2 {
            let cache = Arc::clone(&cache);
            let metrics = Arc::clone(&metrics);
            waiters.push(tokio::spawn(async move {
                cache
                    .lookup(
                        "failure.test",
                        443,
                        Instant::now() + Duration::from_secs(5),
                        metrics,
                    )
                    .await
                    .expect_err("should share failure")
            }));
        }
        let request = requests.recv().await.expect("should start one lookup");
        tokio::task::yield_now().await;
        assert!(
            requests.try_recv().is_err(),
            "should have one resolver call"
        );
        request
            .result
            .send(Err(io::Error::other("shared failure")))
            .expect("should deliver failure");
        for waiter in waiters {
            let error = waiter.await.expect("join");
            assert_eq!(error.kind(), io::ErrorKind::Other);
            assert_eq!(error.to_string(), "shared failure");
        }
        assert_eq!(metrics.snapshot().dns_cache_misses, 1);

        let retry = tokio::spawn({
            let cache = Arc::clone(&cache);
            let metrics = Arc::clone(&metrics);
            async move {
                cache
                    .lookup(
                        "failure.test",
                        443,
                        Instant::now() + Duration::from_secs(5),
                        metrics,
                    )
                    .await
            }
        });
        let retry_request = requests.recv().await.expect("failure should not be cached");
        retry_request
            .result
            .send(Ok(vec!["127.0.0.1:443".parse().expect("socket")]))
            .expect("should deliver retry");
        retry
            .await
            .expect("join retry")
            .expect("retry should resolve");
        assert_eq!(metrics.snapshot().dns_cache_misses, 2);
    }

    #[tokio::test(start_paused = true)]
    async fn ready_entry_expires_at_thirty_seconds() {
        let (cache, mut requests) = fake_cache();
        let metrics = Arc::new(ProxyMetrics::default());
        let first = tokio::spawn({
            let cache = Arc::clone(&cache);
            let metrics = Arc::clone(&metrics);
            async move {
                cache
                    .lookup(
                        "ttl.test",
                        443,
                        Instant::now() + Duration::from_secs(60),
                        metrics,
                    )
                    .await
            }
        });
        requests
            .recv()
            .await
            .expect("first lookup")
            .result
            .send(Ok(vec!["127.0.0.1:443".parse().expect("socket")]))
            .expect("first result");
        first.await.expect("join").expect("first success");

        tokio::time::advance(Duration::from_secs(29)).await;
        cache
            .lookup(
                "ttl.test",
                443,
                Instant::now() + Duration::from_secs(5),
                Arc::clone(&metrics),
            )
            .await
            .expect("cache hit before expiry");
        assert!(requests.try_recv().is_err());

        tokio::time::advance(Duration::from_secs(1)).await;
        let expired = tokio::spawn({
            let cache = Arc::clone(&cache);
            let metrics = Arc::clone(&metrics);
            async move {
                cache
                    .lookup(
                        "ttl.test",
                        443,
                        Instant::now() + Duration::from_secs(5),
                        metrics,
                    )
                    .await
            }
        });
        requests
            .recv()
            .await
            .expect("lookup after expiry")
            .result
            .send(Ok(vec!["127.0.0.1:443".parse().expect("socket")]))
            .expect("expired result");
        expired.await.expect("join").expect("expired success");
        assert_eq!(metrics.snapshot().dns_cache_misses, 2);
    }

    #[tokio::test]
    async fn all_in_flight_cache_stays_bounded_at_sixty_four() {
        let (cache, mut requests) = fake_cache();
        let metrics = Arc::new(ProxyMetrics::default());
        let mut lookups = Vec::new();
        let mut resolver_requests = Vec::new();
        for index in 0..65 {
            let cache = Arc::clone(&cache);
            let metrics = Arc::clone(&metrics);
            lookups.push(tokio::spawn(async move {
                cache
                    .lookup(
                        &format!("bound-{index}.test"),
                        443,
                        Instant::now() + Duration::from_secs(5),
                        metrics,
                    )
                    .await
            }));
            resolver_requests.push(requests.recv().await.expect("resolver request"));
        }
        assert_eq!(cache.state.lock().await.entries.len(), MAX_ENTRIES);

        for request in resolver_requests {
            request
                .result
                .send(Ok(vec!["127.0.0.1:443".parse().expect("socket")]))
                .expect("deliver result");
        }
        for lookup in lookups {
            lookup.await.expect("join lookup").expect("lookup success");
        }
    }
}
