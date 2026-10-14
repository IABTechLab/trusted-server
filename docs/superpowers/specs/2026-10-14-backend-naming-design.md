# Design Spec: Each adapter names its own backends

**Status:** Proposed, 2026-10-14. Implemented on a working branch of the fork, with no pull request raised yet.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-08-10-config-first-auction-provider-architecture-design.md`, `2026-03-19-edgezero-migration-design.md`
**Related PRs:** none yet
**Issues:** IABTechLab/trusted-server#1339
**Last updated:** 2026-10-14

> **Why this spec exists.** Core held a backend naming and transport timer policy for each of the four adapters, so that startup validation, the `ts` command and the adapters predicted the same backend names before any was registered. The policies called no platform, and they still named every platform in core, where the rule is that core names none.

## 1. Problem

`crates/trusted-server-core/src/platform/backend_naming.rs` carried an enum with a variant for Fastly, Axum, Cloudflare and Spin, the naming rule and the transport timer rounding of each, the backend budget of the one host whose names are a limited resource, and a second enum of the same four targets with each one's fan-out capability. `PlatformBackend` asked every adapter which variant it was, and `AuctionPlan::validate_for_target` took a variant and read the policy from core.

That kept three callers agreeing by putting the host's rules in core. An adapter could not change how it names a backend without a change in core, and a fifth host could not be added without naming it there. The rule the stack serves is that core is the interfaces, the routing, the origin fetch and the caches, and names no hosting platform.

## 2. Design

**Core keeps one interface.** `PlatformBackend` is how core asks the host what a backend will be called and what transport timer it allows. It already answered `predict_name` and `canonicalize_transport_timeout_ms`. It now also answers the two questions plan validation asks, `auction_backend_budget`, which is `None` by default because a correlation name is not a limited resource, and `transport_timeout_bucket_count`, which is one by default because a name that does not carry the timer reaches one name per provider. The method that returned a platform variant is gone.

**The adapter describes itself.** Plan validation takes an `AuctionTargetDescriptor` the adapter builds from its name, the backend implementation its runtime services use, and its `AuctionTargetCapabilities`, which say whether it can call several providers at once and whether it caps a provider request's whole lifetime. Core validates the plan against that description and nothing else. Because the descriptor carries the same implementation the request path registers through, the names validation predicts are the names the host registers, which is what keeping the policies in core was for.

**Each adapter carries its own policy beside its registration.** The Fastly adapter's policy sits in `backend.rs` next to `BackendConfig::ensure`, the code that registers a dynamic backend, so the two cannot drift. The name is a readable prefix of the specification cut to a bound and a digest of the whole specification, the transport timer rounds to a bounded ladder so a budget-derived timer cannot register a backend per request, and the auction keeps 160 of the 200 dynamic backend names a service allows. The Axum, Cloudflare and Spin adapters register nothing, so their names are correlation keys alone, each made in that adapter's `platform.rs` beside its `PlatformBackend`, and their timers are the exact bounded value. Cloudflare and Spin make the same shape of name today and still each own their function, so a change for one is a change in one place.

**The `ts` command names nothing.** Its one `PlatformBackend` is a backend nothing asks for, which now implements the interface alone. The command does not yet validate a plan against a target, as the auction guide says, and when it does it will take the descriptor of the adapter it is told about.

**EdgeZero owns no adapter's registration.** The Fastly adapter registers its backends itself through the platform SDK in `backend.rs`. EdgeZero's own Fastly proxy registers backends under its own prefix for its own proxy path, which this policy never predicted. The Cloudflare, Spin and Axum adapters register no backend at all, through EdgeZero or otherwise. So every policy lives in its adapter and nothing moves into EdgeZero.

## 3. What is not in this change

- The names a deployment sees. Each adapter predicts the names it predicted before, pinned by a test in that adapter.
- The capabilities each adapter declares. Fastly and Axum fan out, Cloudflare and Spin run one provider at a time, and no adapter claims a total request deadline, as before.
- Running target-aware validation from the `ts` command before a push. That needs the EdgeZero callback the auction guide describes.

## 4. Tests

- Core validates a plan against a descriptor built from test doubles: a target that fans out accepts two providers, a target that does not refuses them naming the target and the count, a target whose budget the providers' timer buckets exceed is refused naming both numbers, a backend that cannot name a provider fails naming the provider and the target, two providers given one name are refused naming both and the name, and a disabled plan asks nothing of the target.
- The descriptor answers what the adapter gave it, including a backend that declares no budget.
- Each adapter pins the name and the transport timer its backend makes for one specification, and its capabilities test reads the descriptor it builds.
- The Fastly adapter keeps the policy's tests: the pinned name with its digest, bracketed and bare IPv6 hosts naming one backend, a control character refused, the timer ladder, and the bucket count equal to the distinct timers a ceiling can reach.
