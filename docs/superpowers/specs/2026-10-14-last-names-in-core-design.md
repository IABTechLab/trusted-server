# Design Spec: Small vendor, standard and platform names leave core

**Status:** Proposed, 2026-10-14. Raised as pull request IABTechLab/trusted-server#1389 (from jwrosewell/trusted-server#41), for Tech Lab review.
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

**A module written with its type folder.** The guide lets a section's selector name a module in full or with its type folder left off (`docs/guide/configuration-rules.md`), as `analytics.tinybird` or `tinybird` in `[analytics]`. Four places in core now shorten a name written with the section's own type folder before they use it, in every section and not only `[analytics]`, so each takes the module's table to be `[analytics.tinybird]` whichever way the selector wrote the name. `check_tables` in `provider_table.rs` decides whether each table in a section belongs to a selected module, `Settings::module_config` in `settings.rs` finds the table a module reads, the deploy check of a module's secret settings in `config.rs` finds the table it checks, and `module_secrets::clear_unused`, which the load of a stored configuration runs, keeps the secret settings of that table in use. `ts config validate` and the load of a stored configuration therefore accept `module = "analytics.tinybird"` as they accept `tinybird`. A table written at the full name's parts, as `[analytics.analytics.tinybird]`, is refused, whereas a name under another type's folder, as `testing.testlight` in `[auction]`, keeps its table at the full name's parts. Other places still look a table up by the name as the selector wrote it, among them `source::selected` for a `[source]` module and the demand and ad server tables read in `auction/plan.rs`.

**What leaves core.** The `Settings::analytics` field, `Settings::analytics_module`, `AnalyticsConfig` and core's `TinybirdSettings` are gone, `TinybirdSettings` is now the module crate's, and the Fastly adapter's own `tinybird` module is deleted. `CHANGELOG.md` says what crates outside core use instead.

**Rollout.** A deployment that already writes `[analytics] module = "tinybird"` and `[analytics.tinybird]` changes nothing, in the document or in the form stored. A deployment that lists its own builders adds `trusted_server_analytics_tinybird::builder()` to keep sending, because the registry refuses an `[analytics]` name no listed builder supplies. A deployment on the Axum, Cloudflare or Spin adapter sends nothing before or after.

### 2.2 The adapter declares its own platform's cache header

Core named four shared cache headers itself. Two come from standards, `Surrogate-Control` from the Edge Architecture Specification and `CDN-Cache-Control` from RFC 9213, and the other two belong to one platform each, `fastly-surrogate-control` and `cloudflare-cdn-cache-control`. Core could write a policy in three of them, all but `fastly-surrogate-control`, and it stripped all four from every response it made private, on every adapter. It now names only the two standard headers, and section 2.3 says which cache headers it writes or removes.

**The interface.** An adapter declares the headers its own platform reads a shared cache policy from with `platform::declare_platform_cache_headers`, once, as it builds its state. Core reads the declaration only in `origin_response_may_be_kept` in `publisher.rs`, which keeps an origin response with a policy in a declared header out of the stored copies. The header an adapter has core write its policy in is `EdgeCacheHeader::Platform(name)`, beside `SurrogateControl`, `CdnCacheControl`, the `s-maxage` fallback and none, and `CachePolicy::apply_to_response` records it on the response as a `WrittenPlatformCacheHeader`. A name that is not a valid header name is never written, and a warning is logged each time a policy is rendered for it. The declared list is held for the whole running process, so a name declared twice is listed once, nothing removes a name, and a test that declares an example name leaves it listed for every other test in the same test program.

**What each adapter declares and writes.** Each adapter holds the header it writes its policy in as `EDGE_CACHE_HEADER` in its own `app.rs`.

| Adapter    | Declares                       | Writes its policy in           |
| ---------- | ------------------------------ | ------------------------------ |
| Fastly     | `fastly-surrogate-control`     | `Surrogate-Control`            |
| Cloudflare | `cloudflare-cdn-cache-control` | `cloudflare-cdn-cache-control` |
| Axum       | nothing                        | `s-maxage` in `Cache-Control`  |
| Spin       | nothing                        | `s-maxage` in `Cache-Control`  |

**What leaves core.** `EdgeCacheHeader::CloudflareCdnCacheControl` and the four constants for the Fastly and Cloudflare header names (`HEADER_FASTLY_SURROGATE_CONTROL`, `HEADER_FASTLY_SURROGATE_CONTROL_NAME`, `HEADER_CLOUDFLARE_CDN_CACHE_CONTROL` and `HEADER_CLOUDFLARE_CDN_CACHE_CONTROL_NAME`) are gone from `cache_policy.rs`, and `EDGE_CACHE_HEADER_NAMES` lists only the two standard headers. `EdgeCacheHeader::Platform` takes the variant's place, and the Fastly and Cloudflare adapters hold their own header names as `FASTLY_SURROGATE_CONTROL_HEADER` and `CDN_CACHE_CONTROL_HEADER`. `CHANGELOG.md` says what crates outside core use instead.

**What changes for an operator.** Each adapter writes its policy in the header it wrote before. Where core removed all four headers from every response it made private, it now removes `fastly-surrogate-control` from none and `cloudflare-cdn-cache-control` only where core wrote its own policy there, so either header stays where an origin or an operator's `response_headers` wrote it. An origin response with a policy in `fastly-surrogate-control` is kept out of the stored copies on Fastly alone, and one with a policy in `cloudflare-cdn-cache-control` on Cloudflare alone, where every adapter kept both out. Section 2.3 says what a deployment configures to remove such a header.

### 2.3 Cache headers follow the stored item's policy

Only a middleware changes an asset or a response. Everything else changes the model, and the cache policy of an item as stored is model. So removing what an origin sent is the job of a fetch middleware that a deployment chooses, and the cache headers a response is served with mirror the cache policy of the item as stored. Neither core nor an adapter names a platform's cache header in order to remove it.

Core owns `Cache-Control` and the two standard headers, which it reads on every adapter and may write its policy in, and on one response it also owns a platform header it wrote its policy in at the adapter's request. Where the item's policy is private, the response says so in those headers and in no others. Each place the declared list was used becomes the following.

| Where core used the declared list                                                | What it does now                                                                                                                                                                                                                |
| -------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Removing them from a response core makes private (`response_privacy.rs`)         | Removes `Surrogate-Control`, `CDN-Cache-Control` and a platform header core wrote its policy in on that response, which core records on the response when it writes it. A platform header an origin or an operator wrote stays. |
| Removing them before core writes its own policy (`cache_policy.rs`)              | Removes the two standard headers and writes the policy in the header the adapter asks for, replacing what was there.                                                                                                            |
| Skipping an operator's `response_headers` entry on an uncacheable response       | Skips `Cache-Control`, the two standard headers and a platform header core wrote its policy in. An entry naming another header is written as configured.                                                                        |
| Not keeping an origin response with a policy core does not read (`publisher.rs`) | Reads `CDN-Cache-Control` and the header the adapter declares for its own platform, because whether an item is kept is model, and keeps a response with a policy in either out of the stored copies.                            |

**What a deployment configures.** A deployment whose origin sends a platform's cache header on a page core may make private names a fetch middleware that removes the header, in a `[[fetch]]` entry for the page's media type. None exists in the stack, and adding one takes two things. Core has to let a fetch middleware remove a header from the origin's response before the item is stored, because today a fetch middleware changes only the body and only a serve middleware writes a header (`MiddlewareAction::response_headers`). Then one crate per platform supplies the middleware, `crates/middleware/fastly` for `fastly-surrogate-control` and `crates/middleware/cloudflare` for `cloudflare-cdn-cache-control`. Until both exist the deployer stops the origin sending the header on such pages.

**Where core or an adapter still alters the response.** Each place below writes or removes cache headers as a step of its own rather than in a serve middleware writing them from the item's policy, and each is to move there.

| Place                                                                                                                                       | What it does to the response                                                                                                                                                                                                                                                            |
| ------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `response_privacy::enforce_private_no_store`, through `enforce_terminal_private_cache_privacy` and `enforce_synthesized_html_cache_privacy` | Writes `Cache-Control: no-store, private` and removes the headers core owns, `ETag`, `Last-Modified`, `Expires` and `Age`. Core calls it on what it answers for one reader, personalized and assembled pages among them, and so do the Google ad tag diagnostics and the Didomi module. |
| `response_privacy::enforce_set_cookie_cache_privacy`                                                                                        | On a response that sets a cookie, removes the headers core owns and writes `Cache-Control: private, max-age=0` unless it already forbids shared storage.                                                                                                                                |
| `response_privacy::enforce_uncacheable_cache_privacy`                                                                                       | Removes the headers core owns from a response whose `Cache-Control` is private or no-store.                                                                                                                                                                                             |
| `response_privacy::apply_response_headers_with_cache_privacy`, called by every adapter's `apply_finalize_headers`                           | Writes the operator's `response_headers` between the two steps above, skipping the headers core owns on an uncacheable response.                                                                                                                                                        |
| `response_privacy::apply_inactive_ad_stack_browser_cache_policy`                                                                            | Writes `Cache-Control: private, max-age=60` on an inactive ad template.                                                                                                                                                                                                                 |
| `cache_policy::CachePolicy::apply_to_response` and `apply_to_headers`                                                                       | Write `Cache-Control` and the adapter's header, or `s-maxage`, for `/static/tsjs=`, a `[cache]` asset rule and the asset proxy.                                                                                                                                                         |
| `cache_policy::apply_no_store_private_to_headers`, also reached through `CacheControlPolicy::NoStorePrivate`                                | Writes `Cache-Control: no-store, private` and removes the standard headers, for the asset proxy's private answers, the Prebid module's script and the first place in this table.                                                                                                        |
| The Fastly adapter's entry point, `lib.rs`                                                                                                  | Applies the asset proxy's policy in `Surrogate-Control` after finalization, and runs the first three again after request filter effects and the Edge Cookie step (`apply_terminal_response_effects`).                                                                                   |
| Every adapter's routes                                                                                                                      | Choose the header core writes the policy in, `Surrogate-Control` on Fastly, `cloudflare-cdn-cache-control` on Cloudflare and `s-maxage` on Axum and Spin.                                                                                                                               |

### 2.4 Later rows

The store names, the fingerprint class and the key format follow in the same series, each with its own section here as it lands.

## 3. What is not in this change

- A second analytics module. The interface takes one, and the tests prove a name no module supplies is refused with the names on offer.
- Access-log telemetry. The reserved settings stay reserved, and turning one on is still refused.
- Telemetry from the Axum, Cloudflare and Spin adapters. They link the module and run on core's no-op sink, as before.
- The store names and removed store ids, the JA4 fingerprint class and the JWKS key format, which the table above lists and which follow in the same series.
- Names the table does not list. Core also names `fastly-client-ip` in the check of `[trusted_client_ip]` (`settings.rs`), lists `fastly-ssl` and `fastly-client-ip` among the headers it strips from a request on entry (`http_util.rs`), reads `fastly-ssl` as a signal of the request's scheme (`http_util.rs`), and removes `fastly-ssl` from the request before calling the origin (`publisher.rs`).
- The vendor's name in two places in core, being a comment in `provider_table.rs` and a secret fixture in the tests of `config_payload.rs`.
- The fetch middleware that removes a platform's cache header and the core change it needs, and moving the places section 2.3 lists into a serve middleware.

## 4. Tests

- Core tests that a declared platform header stays on a private response and on a cookie response, whoever wrote it (`a_declared_platform_header_stays_on_a_private_response`, `a_platform_header_core_did_not_write_is_left_on_a_cookie_response`), that a policy core wrote in a platform header is removed with the headers core owns and an operator header does not restore it (`a_policy_written_in_a_platform_header_is_removed_with_the_owned_headers`, `a_policy_core_wrote_in_a_platform_header_leaves_a_cookie_response`), and that a private policy leaves nothing in the header it is written in (`a_private_policy_empties_the_platform_header_it_is_written_in`). A policy is written in the platform's own header when the adapter asks for it, an origin response with a policy in a declared header is not kept, and a name declared twice is listed once.
- The module's crate tests its sink (one Events API request per batch, no wait, the row limit, an empty batch sends nothing), its table (the host, the token, the reserved settings, the bounds, an unknown key, the deprecated fields, the documented block in the example configuration), its stored form (the selection and the table survive being stored, with no table at the old name) and its selection (the short name and the full name, no section, a table the section does not select, the old top-level table, a name the build does not have, the token's key name at deploy). The full name goes through the settings load and the module's registration, through the deploy check of its token (`the_full_name_s_token_is_checked_at_deploy_in_the_table_the_module_reads`) and through the load of a stored configuration (`the_full_name_loads_from_the_stored_form_with_its_token`).
- Core tests a module of another type selected by its full name, `testing.probe` in `[testing]`, through the deploy check of its secret (`deploy_validation_checks_the_secret_of_a_module_selected_by_its_full_name`), the clearing of secrets not in use (`a_module_selected_by_its_full_name_keeps_the_settings_in_the_table_it_reads`) and the load of a stored configuration (`the_load_looks_up_the_secret_of_a_module_selected_by_its_full_name`).
- Core tests the kind: a selected module that supplies no analytics module is refused naming it, a section selecting two names is refused, and no section resolves no sink.
- Each of the Axum, Cloudflare, Fastly and Spin adapters tests that its linked modules are listed in hook order with the analytics module among them (`the_linked_modules_are_offered_in_hook_order`), and the Axum adapter's shipped modules test proves the analytics module refuses a setting it does not know. The `ts` tool's list has no test of its own.
- Each of the Axum, Cloudflare, Fastly and Spin adapters builds its state, proves it declares its own platform's header and no other, and proves a response core makes private carries `no-store, private`, loses the policy core wrote in the adapter's header and keeps a platform's header the origin sent (`a_private_response_follows_its_policy_and_keeps_the_origins_header`).

Not tested:

- Any adapter's reading of the sink. No test builds an adapter's state with `[analytics]` selecting the module and checks which sink its services carry, so the Fastly adapter's wiring and the lack of it on the other three adapters are both untested.
- `Settings::module_config` given a name written with its type folder for a module type other than analytics, and the refusal by `check_tables` of a table at the full name's parts. No test covers either.
- A platform header that is not a valid header name, which no test writes.

## Revision record

| Date       | Change                                                                                                                                                                                                                                                                                                                                                                                                                            |
| ---------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-10-14 | First draft.                                                                                                                                                                                                                                                                                                                                                                                                                      |
| 2026-10-15 | Fixes from review round 1. The module lists of each adapter and of the `ts` tool, the headers each adapter declares and strips with the narrowing as an open question, the adapters that send telemetry, the table change for a name written with its type folder, the defaults and bounds, what leaves core, the rollout, the names left in core, the untested paths and the review log.                                         |
| 2026-10-15 | Corrections from review round 2. The full name of the analytics module failing the deploy check and the load of a stored configuration, the code fix under the open question, what the selection test reaches, and `fastly-ssl` among the names left in core.                                                                                                                                                                     |
| 2026-10-15 | Code fixes. The deploy check of a module's secret settings and the load of a stored configuration read the table of a module selected by its full name at the name without its type folder, and the tests of the fix are listed.                                                                                                                                                                                                  |
| 2026-10-16 | Cache headers follow the stored item's policy, in a new section 2.3 that closes the open question on the cache headers. Each adapter declares only its own platform's header, which core reads only to decide whether an origin response may be kept, core removes from a private response only the headers it owns, and the page says what a deployment configures and lists where core or an adapter still alters the response. |

## Review log

1. **Round 1, 2026-10-15.** A review in a fresh session, against `split/46-last-names` at `8110daa5b`. Issues found, four of them blocking. The page named `crates/trusted-server-modules`, which no longer exists, said nothing changes for an operator when each adapter now strips a different set of cache headers, said nothing of the fact that only the Fastly adapter sends auction telemetry, and left out the core change that lets a section name a module with its own type folder. This revision states how each adapter and the `ts` tool list the module, which headers each adapter declares and strips with the narrowing put as an open question, that only the Fastly adapter sends, and the core change with the places that still read a table by the name as written, together with the partly true and unsupported claims, the coverage gaps and the advice the round gave, and it corrects the same claims in the changelog and the telemetry guide.
2. **Round 2, 2026-10-15.** A lighter review in a fresh session, against this page at `3286cb534`, of the round 1 fixes and the new text. No blocking issues. This revision says plainly that the full name of the analytics module fails `ts config validate` and the load of a stored configuration today and lists the code fix with the open question, limits the selection test to the settings load and the module's registration, and adds to the names left in core that core reads `fastly-ssl` as a signal of the scheme and removes the header before calling the origin.
