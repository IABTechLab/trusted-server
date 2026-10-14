# Design Spec: The last vendor, standard and platform names leave core

**Status:** Proposed, 2026-10-14. Implemented on a working branch of the fork, with no pull request raised yet.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-10-14-source-modules-design.md`, `2026-10-13-request-addressing-design.md`
**Related PRs:** none yet
**Issues:** IABTechLab/trusted-server#1341
**Last updated:** 2026-10-14

> **Why this spec exists.** Core is the interfaces, the routing, the origin fetch, the caches and their tests. After the series before this one it still names, in small pieces, a telemetry vendor, two hosting platforms' cache headers and stores, a TLS fingerprint format and a published key format. Each is a place where a change by one vendor or one platform is a change in core.

## 1. Problem

Five names remain in core, each with the code that reads it:

| What core names                                        | Where                                                    |
| ------------------------------------------------------ | -------------------------------------------------------- |
| The Tinybird telemetry module's settings and name      | `settings.rs`, and the module in the Fastly adapter      |
| The platforms' shared cache header names               | `cache_policy.rs`                                        |
| The platforms' store names and the removed store ids   | `settings.rs`, `request_signing/mod.rs`                  |
| The JA4 fingerprint class in the device classification | `ec/device.rs`                                           |
| The JWKS key format in request signing                 | `request_signing/jwks.rs`, `discovery.rs`, `rotation.rs` |

Each name is small. Each one means that adding a second analytics vendor, a third platform, another fingerprint format or another key format is an edit to core rather than a crate beside it.

## 2. Design

Each name moves to the crate that reads it, and core keeps an interface in its place. The settings a deployment writes do not change.

### 2.1 Analytics is a module type

`[analytics]` was a section core read itself, with one module's settings typed into core and the module built by the Fastly adapter alone. It is now a module type like `[cmp]` or `[geo]`: core reads the section as it reads every module type's section, and a module crate supplies the sink.

**The interface.** A registration declares the analytics module it supplies with `with_analytics_module(name, sink)`, where the sink is an `AuctionTelemetrySink`, the trait core already handed each settled auction's rows to. The registry resolves `[analytics] module` against the modules on offer, as `[geo] module` and `[device] module` are resolved, and `IntegrationRegistry::analytics_module` gives the sink the section selected, or `None` when there is no section, so core's no-op sink stands. A section that selects more than one name is refused, because an auction's rows go to one sink.

**The module.** `crates/analytics/tinybird` (`trusted-server-analytics-tinybird`) carries the settings of `[analytics.tinybird]`, their checks and the sink that posts each batch to the Events API. It reads its table through `Settings::module_config`, declares `auction_token_secret` as a secret setting through `IntegrationBuilder::with_secret_settings`, so the token is looked up as the settings load and checked at deploy as every module's secret is, and is offered by `crates/trusted-server-modules`, so every adapter that builds its state from that list links it. The Fastly adapter asks the registry for the sink instead of building one.

```toml
[analytics]
module = "tinybird"

[analytics.tinybird]
api_host = "api.us-east.aws.tinybird.co"
auction_dataset = "auction_events_raw"
auction_token_secret = "tinybird_auction_append_token"
```

**What changes for an operator.** Nothing in the document above. Three refusals move from where the settings load to where the module runs: a table missing its host or token, a key the table does not know, and a reserved setting turned on are refused when the registry is built and by `ts config validate`, naming the table and the setting. A `[tinybird]` table at the top level is still refused, as the section of a module type nothing reads, rather than with directions to the new place, because the directions named the vendor in core.

### 2.2 The platform declares the header it reads a shared cache policy from

Core wrote a shared cache policy in one of four headers it named itself, two of them one platform's own, and stripped all four from every response it made private. It now knows the two headers every platform may read, `Surrogate-Control` from the Edge Architecture Specification and `CDN-Cache-Control` from RFC 9213, and asks the running platform for the rest.

**The interface.** An adapter declares the headers its platform reads a shared cache policy from with `platform::declare_platform_cache_headers`, once, as it builds its state. `cache_policy::edge_cache_header_names` lists the two core reads and then the declared ones, and that list is what core strips from a response it makes private, skips among an operator's response headers on an uncacheable response, and treats as a policy it does not read when it decides whether an origin page may be kept. The header an adapter writes the policy in is `EdgeCacheHeader::Platform(name)`, beside `SurrogateControl`, `CdnCacheControl`, the `s-maxage` fallback and none.

**What each adapter declares.** The Fastly adapter declares `fastly-surrogate-control` and writes `Surrogate-Control`. The Cloudflare adapter declares `cloudflare-cdn-cache-control` and writes it. The Axum and Spin adapters declare nothing and keep the `s-maxage` fallback.

**What changes for an operator.** Nothing. Each platform's responses carry the headers they carried, and a header the platform reads still comes off every private response, because the adapter that serves on that platform declared it.

### 2.3 Later rows

The store names, the fingerprint class and the key format follow in the same series, each with its own section here as it lands.

## 3. What is not in this change

- A second analytics module. The interface takes one, and the tests prove a name no module supplies is refused with the names on offer.
- Access-log telemetry. The reserved settings stay reserved, and turning one on is still refused.

## 4. Tests

- A declared platform header is stripped with the standard ones by the cache policy and by the response privacy guard, is not restored by an operator header, and a policy is written in the platform's own header when the adapter asks for it. A name declared twice is listed once.
- The module's crate tests its sink (one Events API request per batch, no wait, the row limit, an empty batch sends nothing), its table (the host, the token, the reserved settings, the bounds, an unknown key, the deprecated fields, the documented block in the example configuration) and its selection (the short and the full name, no section, a table the section does not select, the old top-level table, a name the build does not have, the token's key name at deploy).
- Core tests the kind: a selected module that supplies no analytics module is refused naming it, a section selecting two names is refused, and no section resolves no sink.
- The stock list offers the module in hook order, and the stock module tests prove it refuses a setting it does not know.
