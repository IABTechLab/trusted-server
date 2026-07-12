use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, watch};
use tokio::time::Instant;

use crate::commands::dev::proxy::metrics::ProxyMetrics;

const TTL: Duration = Duration::from_secs(30);
const MAX_ENTRIES: usize = 64;

#[derive(Clone, Eq, Hash, PartialEq)]
struct CacheKey {
    host: Arc<str>,
    port: u16,
}

enum Entry {
    Loading(watch::Sender<bool>),
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
    state: Mutex<State>,
    enabled: bool,
}

impl Default for DnsCache {
    fn default() -> Self {
        Self::new(true)
    }
}

impl DnsCache {
    #[must_use]
    pub fn new(enabled: bool) -> Self {
        Self {
            state: Mutex::new(State::default()),
            enabled,
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
        metrics: &ProxyMetrics,
    ) -> io::Result<Arc<[SocketAddr]>> {
        let key = CacheKey {
            host: Arc::from(host.to_ascii_lowercase()),
            port,
        };
        if !self.enabled {
            return resolve_uncached(&key, deadline, metrics).await;
        }
        loop {
            enum Action {
                Wait(watch::Receiver<bool>),
                Resolve(watch::Sender<bool>),
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
                    Some(Entry::Loading(notify)) => Action::Wait(notify.subscribe()),
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
                                    Entry::Loading(_) => None,
                                })
                                .min_by_key(|(_, used)| *used)
                                .map(|(key, _)| key)
                        {
                            state.entries.remove(&evict);
                        }
                        if state.entries.len() >= MAX_ENTRIES {
                            Action::Bypass
                        } else {
                            let (notify, _) = watch::channel(false);
                            state
                                .entries
                                .insert(key.clone(), Entry::Loading(notify.clone()));
                            Action::Resolve(notify)
                        }
                    }
                }
            };
            match action {
                Action::Wait(mut notify) => {
                    tokio::time::timeout_at(deadline, notify.changed())
                        .await
                        .map_err(|_| {
                            io::Error::new(
                                io::ErrorKind::TimedOut,
                                "coalesced DNS lookup timed out",
                            )
                        })?
                        .map_err(|_| {
                            io::Error::new(io::ErrorKind::Interrupted, "DNS lookup owner exited")
                        })?;
                }
                Action::Resolve(notify) => {
                    return self
                        .resolve_owner(key.clone(), deadline, metrics, notify)
                        .await;
                }
                Action::Bypass => return resolve_uncached(&key, deadline, metrics).await,
            }
        }
    }

    async fn resolve_owner(
        &self,
        key: CacheKey,
        deadline: Instant,
        metrics: &ProxyMetrics,
        notify: watch::Sender<bool>,
    ) -> io::Result<Arc<[SocketAddr]>> {
        let result = resolve_uncached(&key, deadline, metrics).await;
        let mut state = self.state.lock().await;
        state.sequence += 1;
        let used = state.sequence;
        state.entries.remove(&key);
        if let Ok(addresses) = &result {
            state.entries.insert(
                key,
                Entry::Ready {
                    addresses: Arc::clone(addresses),
                    expires: Instant::now() + TTL,
                    used,
                },
            );
        }
        let _ = notify.send(true);
        result
    }
}

async fn resolve_uncached(
    key: &CacheKey,
    deadline: Instant,
    metrics: &ProxyMetrics,
) -> io::Result<Arc<[SocketAddr]>> {
    metrics.record_dns_cache_miss();
    let started = Instant::now();
    let addresses: Arc<[SocketAddr]> = tokio::time::timeout_at(
        deadline,
        tokio::net::lookup_host((key.host.as_ref(), key.port)),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "upstream DNS timed out"))??
    .collect::<Vec<_>>()
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

    #[tokio::test]
    async fn caches_success_for_normalized_key() {
        let cache = DnsCache::default();
        let metrics = ProxyMetrics::default();
        let deadline = Instant::now() + Duration::from_secs(2);
        let first = cache
            .lookup("localhost", 80, deadline, &metrics)
            .await
            .expect("resolve localhost");
        let second = cache
            .lookup("LOCALHOST", 80, deadline, &metrics)
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
                        &metrics,
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
}
