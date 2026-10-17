# Design Spec: Small vendor, standard and platform names leave core

**Status:** Proposed, 2026-10-14. Raised as draft pull request IABTechLab/trusted-server#1389 (from jwrosewell/trusted-server#41), for Tech Lab review.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-10-14-source-modules-design.md`, `2026-10-14-no-modules-crate-design.md` and `2026-10-13-request-addressing-design.md`
**Related PRs:** IABTechLab/trusted-server#1389, jwrosewell/trusted-server#41
**Issues:** IABTechLab/trusted-server#1341
**Last updated:** 2026-10-16

> **Why this spec exists.** Core is the interfaces, the routing, the origin fetch, the caches and their tests. After the series before this one it still names, in small pieces, a telemetry vendor, two hosting platforms' cache headers and stores, a TLS fingerprint format and a published key format. Each is a place where a change by one vendor or one platform is a change in core.

## 1. Problem

These names remain in core, each with the code that reads it:

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

**The module.** `crates/analytics/tinybird` (`trusted-server-analytics-tinybird`) carries the settings of `[analytics.tinybird]`, their checks and the sink that posts each batch to the Events API. It reads its table through `Settings::module_config`, declares `auction_token_secret` as a secret setting through `IntegrationBuilder::with_secret_settings`, so the token is looked up as the settings load and checked at deploy as every module's secret is. Each adapter lists the module's builder among the modules it links, as `shipped_module_builders()` in its own `app.rs`, and the `ts` tool lists the same builder in `linked_builders()` in `crates/trusted-server-cli/src/app_config.rs`, as `2026-10-14-no-modules-crate-design.md` describes, so `ts config validate` and `ts config push` run the module's checks and check the key name of its token.

**Which adapters send.** Only the Fastly adapter sends. The Fastly adapter asks the registry for the sink with `analytics_module()` in `crates/trusted-server-adapter-fastly/src/app.rs`, keeps the sink in its application state and hands it to the services it builds for each request. Before this change the Fastly adapter built the sink itself. The Axum, Cloudflare and Spin adapters link the module and check its settings when they build their registry, and none of them reads the sink from the registry, so each runs on core's no-op sink and sends nothing, as before.

```toml
[analytics]
module = "tinybird"

[analytics.tinybird]
api_host = "api.us-east.aws.tinybird.co"
auction_dataset = "auction_events_raw"
auction_token_secret = "tinybird_auction_append_token"
```

**What changes for an operator.** Nothing in the document above, and the form the document is stored in is the same, with the selection and the table under `[analytics]` and no table at the old name. Three refusals move out of core's settings and into the module's own checks. A table missing its host or token, a key the table does not know and a reserved setting turned on are each refused, naming the table and the setting. A refusal stops the deployment from starting, and `ts config validate` reports the same refusal. A `[tinybird]` table at the top level is still refused, as the section of a module type nothing reads, rather than with directions to the new place, because the directions named the vendor in core.

**Defaults and bounds.** The settings are those in the configuration guide under "Analytics Configuration", with these defaults and checks.

| Setting                | Default              | Check                                                                  |
| ---------------------- | -------------------- | ---------------------------------------------------------------------- |
| `api_host`             | none                 | Required, a regional host name with no scheme, port or path            |
| `auction_token_secret` | none                 | Required, the name of the secret holding the datasource's append token |
| `auction_dataset`      | `auction_events_raw` | 1 to 128 ASCII letters, digits or `_`                                  |
| `max_body_bytes`       | 1048576              | At least 1024                                                          |
| `access_enabled`       | `false`              | `true` is refused, because no access-log emitter exists                |
| `access_sample_rate`   | `0.0`                | Between 0.0 and 1.0, and read by nothing                               |
| `access_dataset`       | `access_logs_raw`    | Read by nothing                                                        |

`secret_store` and `access_token_secret` are accepted and dropped, and any other key is refused.

**What the module sends.** Core builds the rows, being the auction's summary, each provider call and each bid, from the same observation as before, and the module posts the batch it is handed without adding a field, so the privacy boundary in `docs/guide/telemetry.md` is unchanged and the rows carry no Edge Cookie identifier. Core hands rows only to the sink `[analytics] module` selected, so a deployment without the section sends nothing. A failure to build or start a send is logged as a warning and never changes the response, and the module drops the pending request without waiting for the Events API.

**A module written with its type folder.** The guide lets a section's selector name a module in full or with its type folder left off (`docs/guide/configuration-rules.md`), as `analytics.tinybird` or `tinybird` in `[analytics]`. Two functions in core now shorten a name written with the section's own type folder before they use it, in every section and not only `[analytics]`, so each takes the module's table to be `[analytics.tinybird]` whichever way the selector wrote the name. `check_tables` in `provider_table.rs` decides whether each table in a section belongs to a selected module, and `Settings::module_config` in `settings.rs` finds the table a module reads. A table written at the full name's parts, as `[analytics.analytics.tinybird]`, is therefore refused, whereas a name under another type's folder, as `testing.testlight` in `[auction]`, keeps its table at the full name's parts. Other places still look a table up by the name as the selector wrote it, among them `source::selected` for a `[source]` module, the demand and ad server tables read in `auction/plan.rs`, `Settings::section_table`, which the deploy check of a module's secret uses, and `module_secrets::clear_unused`, which the load of a stored configuration uses.

**The full name of the analytics module fails today.** With `[analytics] module = "analytics.tinybird"`, `ts config validate` refuses the document, saying the secret key reference at `analytics.analytics.tinybird.auction_token_secret` must not be empty, because the deploy check looks for the table at `[analytics.analytics.tinybird]` and finds none. The load of a stored configuration fails as well, because the load removes `auction_token_secret` from `[analytics.tinybird]` as unused and the module then refuses the missing token. The short name `tinybird` works through both. Until the deploy check and the load take the table to be at the name without the type folder, as `check_tables` and `Settings::module_config` do, `analytics.tinybird` is not a valid selector for the analytics module, although the guide documents it. The fix is code and is listed under the open question in 2.2.

**What leaves core.** The `Settings::analytics` field, `Settings::analytics_module`, `AnalyticsConfig` and core's `TinybirdSettings` are gone, `TinybirdSettings` is now the module crate's, and the Fastly adapter's own `tinybird` module is deleted. `CHANGELOG.md` says what crates outside core use instead.

**Rollout.** A deployment that already writes `[analytics] module = "tinybird"` and `[analytics.tinybird]` changes nothing, in the document or in the form stored. A deployment that writes the full name, `module = "analytics.tinybird"`, writes `tinybird` instead until the code fix under the open question is made. A deployment that lists its own builders adds `trusted_server_analytics_tinybird::builder()` to keep sending, because the registry refuses an `[analytics]` name no listed builder supplies. A deployment on the Axum, Cloudflare or Spin adapter sends nothing before or after.

### 2.2 The platform declares the header it reads a shared cache policy from

Core named four shared cache headers itself. Two come from standards, `Surrogate-Control` from the Edge Architecture Specification and `CDN-Cache-Control` from RFC 9213, and the other two belong to one platform each, `fastly-surrogate-control` and `cloudflare-cdn-cache-control`. Core could write a policy in three of them, all but `fastly-surrogate-control`, which it only stripped, and it stripped all four from every response it made private, on every adapter. It now knows the two headers every platform may read and asks the running platform for the rest.

**The interface.** An adapter declares the headers its platform reads a shared cache policy from with `platform::declare_platform_cache_headers`, once, as it builds its state. `cache_policy::edge_cache_header_names` lists the two core reads and then the declared ones, and that list is what core strips from a response it makes private, skips among an operator's response headers on an uncacheable response, and treats as a policy it does not read when it decides whether an origin page may be kept. The header an adapter writes the policy in is `EdgeCacheHeader::Platform(name)`, beside `SurrogateControl`, `CdnCacheControl`, the `s-maxage` fallback and none. A declared name that is not a valid header name is never written, and a warning is logged each time a policy is rendered for it. The declared list is held for the whole running process, so each adapter appends to the list each time the adapter builds its state, a name declared twice is listed once, and nothing removes a name. A test that declares an example name therefore leaves the name listed for every other test in the same test program, as the tests in `cache_policy.rs`, `response_privacy.rs` and the Google ad tag diagnostics do.

**What each adapter declares and strips.** An adapter writes its policy in the header shown and strips the names shown from a response core makes private.

| Adapter    | Declares                       | Writes its policy in           | Strips from a response core makes private                                |
| ---------- | ------------------------------ | ------------------------------ | ------------------------------------------------------------------------ |
| Fastly     | `fastly-surrogate-control`     | `Surrogate-Control`            | `surrogate-control`, `cdn-cache-control`, `fastly-surrogate-control`     |
| Cloudflare | `cloudflare-cdn-cache-control` | `cloudflare-cdn-cache-control` | `surrogate-control`, `cdn-cache-control`, `cloudflare-cdn-cache-control` |
| Axum       | nothing                        | `s-maxage` in `Cache-Control`  | `surrogate-control`, `cdn-cache-control`                                 |
| Spin       | nothing                        | `s-maxage` in `Cache-Control`  | `surrogate-control`, `cdn-cache-control`                                 |

**What leaves core.** `EDGE_CACHE_HEADER_NAMES`, `EdgeCacheHeader::CloudflareCdnCacheControl` and the four constants for the Fastly and Cloudflare header names (`HEADER_FASTLY_SURROGATE_CONTROL`, `HEADER_FASTLY_SURROGATE_CONTROL_NAME`, `HEADER_CLOUDFLARE_CDN_CACHE_CONTROL` and `HEADER_CLOUDFLARE_CDN_CACHE_CONTROL_NAME`) are gone from `cache_policy.rs`. `edge_cache_header_names()` and `EdgeCacheHeader::Platform` take their place, and the Fastly and Cloudflare adapters hold their own header names as `FASTLY_SURROGATE_CONTROL_HEADER` and `CDN_CACHE_CONTROL_HEADER`. `CHANGELOG.md` says what crates outside core use instead.

**What changes for an operator.** The header each adapter writes its policy in does not change, but what core strips from a response does, because each adapter now strips the two standard names and the ones it declares, where core stripped all four names on every adapter before. The same list decides three things, so each changes the same way:

1. A response core makes private no longer has `cloudflare-cdn-cache-control` removed on the Fastly adapter, `fastly-surrogate-control` removed on the Cloudflare adapter, or either removed on the Axum and Spin adapters.
2. An entry in an operator's `response_headers` for one of those names on the same adapters is no longer skipped on an uncacheable response, so it is applied.
3. An origin response that carries only one of those names on the same adapters is no longer kept out of the stored copies. `origin_response_may_be_kept` in `publisher.rs` decides whether an origin response may be kept as a stored template or a stored asset copy, and the function no longer refuses a response for carrying such a name. Only the Fastly adapter provides a template cache.

**Open question.** Is this narrowing acceptable? The other choice is for every adapter to declare all four names, which strips on each adapter what core stripped before. This design does not choose between them and leaves the choice to the review. One code fix is also still to make, and it is not a question. The full name of the analytics module, `[analytics] module = "analytics.tinybird"`, fails `ts config validate` and the load of a stored configuration today (section 2.1), so the deploy check of a module's secret and the load of a stored configuration need to take the table to be at the name without the type folder, as `check_tables` and `Settings::module_config` do.

### 2.3 Later rows

The store names, the fingerprint class and the key format follow in the same series, each with its own section here as it lands.

## 3. What is not in this change

- A second analytics module. The interface takes one, and the tests prove a name no module supplies is refused with the names on offer.
- Access-log telemetry. The reserved settings stay reserved, and turning one on is still refused.
- Telemetry from the Axum, Cloudflare and Spin adapters. They link the module and run on core's no-op sink, as before.
- The store names and removed store ids, the JA4 fingerprint class and the JWKS key format, which the table above lists and which follow in the same series.
- Names the table does not list. Core also names `fastly-client-ip` in the check of `[trusted_client_ip]` (`settings.rs`), lists `fastly-ssl` and `fastly-client-ip` among the headers it strips from a request on entry (`http_util.rs`), reads `fastly-ssl` as a signal of the request's scheme (`http_util.rs`), and removes `fastly-ssl` from the request before calling the origin (`publisher.rs`).
- The vendor's name in two places in core, being a comment in `provider_table.rs` and a secret fixture in the tests of `config_payload.rs`.

## 4. Tests

- A declared platform header is stripped with the standard ones by the cache policy and by the response privacy guard, is not restored by an operator header, and a policy is written in the platform's own header when the adapter asks for it. A name declared twice is listed once.
- The module's crate tests its sink (one Events API request per batch, no wait, the row limit, an empty batch sends nothing), its table (the host, the token, the reserved settings, the bounds, an unknown key, the deprecated fields, the documented block in the example configuration), its stored form (the selection and the table survive being stored, with no table at the old name) and its selection (the short name and the full name, the full name through the settings load and the module's registration only, no section, a table the section does not select, the old top-level table, a name the build does not have, the token's key name at deploy).
- Core tests the kind: a selected module that supplies no analytics module is refused naming it, a section selecting two names is refused, and no section resolves no sink.
- Each of the Axum, Cloudflare, Fastly and Spin adapters tests that its linked modules are listed in hook order with the analytics module among them (`the_linked_modules_are_offered_in_hook_order`), and the Axum adapter's shipped modules test proves the analytics module refuses a setting it does not know. The `ts` tool's list has no test of its own.

Not tested:

- That the Fastly adapter declares `fastly-surrogate-control` and the Cloudflare adapter declares `cloudflare-cdn-cache-control`. No adapter test reads the declared list and the core tests declare an example name of their own, so deleting an adapter's declaration passes every test and leaves its platform's header on a response core makes private.
- Any adapter's reading of the sink. No test builds an adapter's state with `[analytics]` selecting the module and checks which sink its services carry, so the Fastly adapter's wiring and the lack of it on the other three adapters are both untested.
- The analytics module's full name through the deploy check of its token and the load of a stored configuration. No test selects the module that way through either, and both fail for the full name today (section 2.1).
- A name written with its type folder for a module type other than analytics. No core test covers `check_tables` or `Settings::module_config` with such a name, and the Tinybird crate covers them through the settings load and the module's registration only. Nor does any test select a module of another type that way through the deploy check of its secret or the load of a stored configuration.
- A declared name that is not a valid header name, which no test declares.

## Review log

1. **Round 1, 2026-10-15.** A review in a fresh session, against `split/46-last-names` at `8110daa5b`. Issues found, four of them blocking. The page named `crates/trusted-server-modules`, which no longer exists, said nothing changes for an operator when each adapter now strips a different set of cache headers, said nothing of the fact that only the Fastly adapter sends auction telemetry, and left out the core change that lets a section name a module with its own type folder. This revision states how each adapter and the `ts` tool list the module, which headers each adapter declares and strips with the narrowing put as an open question, that only the Fastly adapter sends, and the core change with the places that still read a table by the name as written, together with the partly true and unsupported claims, the coverage gaps and the advice the round gave, and it corrects the same claims in the changelog and the telemetry guide.
2. **Round 2, 2026-10-15.** A lighter review in a fresh session, against this page at `3286cb534`, of the round 1 fixes and the new text. No blocking issues. This revision says plainly that the full name of the analytics module fails `ts config validate` and the load of a stored configuration today and lists the code fix with the open question, limits the selection test to the settings load and the module's registration, and adds to the names left in core that core reads `fastly-ssl` as a signal of the scheme and removes the header before calling the origin.

## Revision record

| Date       | Change                                                                                                                                                                                                                                                                                                                                                                                    |
| ---------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-10-14 | First draft.                                                                                                                                                                                                                                                                                                                                                                              |
| 2026-10-15 | Fixes from review round 1. The module lists of each adapter and of the `ts` tool, the headers each adapter declares and strips with the narrowing as an open question, the adapters that send telemetry, the table change for a name written with its type folder, the defaults and bounds, what leaves core, the rollout, the names left in core, the untested paths and the review log. |
| 2026-10-15 | Corrections from review round 2. The full name of the analytics module failing the deploy check and the load of a stored configuration, the code fix under the open question, what the selection test reaches, and `fastly-ssl` among the names left in core.                                                                                                                             |
