# Design Spec: The loading cache core declares, and the module that supplies it

**Status:** Proposed, 2026-10-12. Implemented on the fork as jwrosewell/trusted-server#19, ready for review, answering IABTechLab/trusted-server#1282. This pull request (#1084) carries every design document of the stack in one place.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-08-27-integration-provider-seam-design.md`
**Related PRs:** #1094, jwrosewell/trusted-server#19, jwrosewell/trusted-server#26
**Last updated:** 2026-10-12

> **Why this spec exists.** A robots.txt contributor's answer, like every other value a deployment holds for every request, is read from the key-value store by each request and loaded by each request that misses, so a burst of requests for one key asks the module's service as many times as there are requests, on every instance.

## 1. Problem

A robots.txt contributor's answer, like every other value a deployment holds for every request, is read from the key-value store by each request and loaded by each request that misses, so a burst of requests for one key asks the module's service as many times as there are requests, on every instance. Core has one cache, for page templates, and no interface a module can supply a cache through, so a cache that loads once for many concurrent requests has nowhere to go that keeps core free of any vendor's package.

## 2. Design

Core declares a loading cache, `PlatformLoadingCache` in `trusted_server_core::platform`, which loads a missing value once per key however many requests ask for it at the same moment, gives every request waiting on that load the value or the error, and holds nothing that failed. Each cache is named by a `CacheSpec` with the lifetimes of its copies, a brief one in process memory and one in the host's shared store, and keys under a `StoreKey` that starts with the publisher's prefix, so publishers sharing a store never share a copy.

Each feature that holds answers this way selects the cache that holds them, by naming in its own `[cache.<name>]` table a module that offers a loading cache. A feature that names none gets `NoLoadingCache`, which holds nothing and runs every load, and a name no linked crate offers a cache from is refused when the service starts, with the names that could be written. Each robots.txt module's answer is the first feature held this way, so a module is asked once per refresh however many instances and requests want its rules.

```toml
[cache.robots-txt]
module = "51degrees"
memory_seconds = 60
store_seconds = 604800
store_idle_seconds = 2592000
```

The implementation is a module crate, `crates/loading-cache/51degrees`, maintained by 51Degrees, written with the published `fiftyone-caching` crate over each host's own stores, being Fastly's cache and key-value store, Workers KV, Spin's key-value store, and a pool of threads on a native host. It offers the cache from its builder, the stock list carries it, and an adapter takes the selections from the settings and the builders it is given with `loading_caches_from`, so no adapter names the crate.

**Why.** The same answer is asked for by every instance and every request, and a cache that lets every miss load at once asks a module's service as many times as there are requests. One load per key at a time, with the result shared by every waiter, is the pattern the other 51Degrees language packages use, and the crate that implements it is published for every 51Degrees service to use. Core keeps only the interface, so core names no vendor and depends on no package.

The pin on the published crate moves from the pre-release `4.5.8-49` to the release `4.5.8` in jwrosewell/trusted-server#26, which completes this change and has no design of its own.

## 3. What is not in this change

- The page template loaded through the loading cache. It still goes through the template cache core has, because moving it is a change to the template flow that this pull request does not make.
- One selection for the whole deployment. Each feature names its cache in its own table, because the asset cache and any later feature may choose differently.

## 4. Acceptance

The pull request's description records how it was checked, with every commit linted and tested on its own, in order, the full local check on its head and CI green on all 19 checks, and 8 deliberate faults made one at a time and each caught by a test.

## Revision record

| Date       | Change                                                          |
| ---------- | --------------------------------------------------------------- |
| 2026-10-12 | First draft, from the pull request and its issue as they stand. |
