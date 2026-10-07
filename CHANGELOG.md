# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Integration modules are moving out of `trusted-server-core` into crates of their own under `crates/<type>/<vendor>/`, each named by that folder, so `crates/cmp/osano` is the module `cmp.osano`. The settings that select and configure a module do not change. `crates/trusted-server-modules` lists the modules a stock build ships from crates of their own, in the order their hooks run. Every adapter builds its state and loads its settings with that list, and `ts config validate` and `ts config push` run each stock module's own rules. Code that named a moved module under `trusted_server_core::integrations` names its crate instead, such as `trusted_server_cmp_osano`. Moved so far: Osano, Lockr, Permutive, Sourcepoint and Didomi.
- The Fastly adapter is a library with a thin binary over it. A deployment that ships a vendor crate writes a binary of its own that calls `trusted_server_adapter_fastly::run_with(vec![...])` with that crate's integration builder, in place of editing the adapter. The builders are composed with the built-in ones, and a builder runs only when the settings select its module. The Wasm artifact keeps its name, `trusted-server-adapter-fastly.wasm`.
- **Breaking:** `[integrations]` is gone. Each page integration is a module selected in the section of its type, with its settings in the table at its name: `[cmp] module = "didomi"` (or `"sourcepoint"` or `"osano"`), `[tag] modules = ["google-tag-manager"]`, `[ad-tag] modules = ["google", "google.diagnostics"]`, `[bot-protection] module = "datadome"`, `[identity] module = "lockr"`, `[audience] module = "permutive"`, `[framework] module = "nextjs"`, `[auction] modules = ["prebid"]` (with `"testing.testlight"` for the test module) and `[proxy] modules = ["js_asset_proxy"]`, so `[integrations.gpt]` is `[ad-tag.google]` and `[integrations.prebid]` is `[auction.prebid]`. A module's name is `<type>.<name>`, which is its crate's path under `crates/` once it has a crate of its own and a constant in core until then, and it is written in its section with the type left off. A section that selects nothing, a table its section does not select, and a name no module in the deployment supplies are each refused at startup. An `[integrations]` or `[integration]` table is refused with directions. Environment overlays name a section as written, so `TRUSTED_SERVER__INTEGRATIONS__GPT__GAM_ATTRIBUTION_ENABLED` is `TRUSTED_SERVER__AD-TAG__GOOGLE__GAM_ATTRIBUTION_ENABLED`.
- **Breaking:** Every pluggable component now shares one configuration syntax. Each selectable type is a top-level table named for the job, a `module` key inside it selects what runs, or `modules` for a type that runs several, and a `[<type>.<name>]` table holds that name's settings when it has any. The types are `ec`, `geo`, `device`, `permission-signal`, `demand` and `ad-server`, and the section of each page integration type, such as `cmp` or `tag`. A module from a crate is named by its folder below `crates/`, such as `permission-signal.gpc`, and within its own table the type folder may be left off. A `demand` or `ad-server` name is snake_case, because it may be a label of your own. A settings table its type's selector does not name refuses startup, so a block left behind after a module is switched off is caught rather than sitting unread, and every module rejects settings it does not know, so a misspelled key fails rather than being ignored. A table's name is the implementation unless the table carries an `implementation` line, and a demand table always carries one, naming the implementation by its module path (`auction-protocol.openrtb`, `auction.prebid-server` or `auction.aps`), which is how two Prebid Servers run side by side under names of their own. The ad server implementation is `ad-server.mock`, written `mock` in `[ad-server]`. A `demand` or `ad-server` endpoint must be HTTPS, or HTTP to `127.0.0.1`, `::1` or `localhost`. The word mediator is gone. It is "ad server" in prose and `ad-server` in configuration, `[debug.auction_html_comment_options] include_mediator_response` is now `include_adserver_response`, and the auction response metadata value `parallel_mediation` is now `parallel_adserver`. `auction-protocol.openrtb`, `auction.prebid-server`, `auction.aps` and `ad-server.mock` supply implementations only, so a page integration's section that names one is refused. The full rules and what is checked at each gate are in [Configuration Rules](docs/guide/configuration-rules.md). Migrate as follows:

  | Previous                                                                             | Now                                                                                                |
  | ------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------- |
  | `[integrations.<id>]` with `enabled = true`                                          | the module in the section of its type, and `[<type>.<name>]` only for settings                     |
  | `[ec.providers.<name>]`                                                              | `[ec.<name>]`                                                                                      |
  | `[permission_signal] sources`                                                        | `[permission-signal] modules`                                                                      |
  | `host-signals`, `client-fixed`, `gpp-sale-opt-out`, `gpp_sale_opt_out`, `us_privacy` | `host_signals`, `client_fixed`, `gpp`, `us-privacy`                                                |
  | `[auction.providers.<id>]` with `protocol`, `profile` and `profile_config`           | `[demand] modules` and `[demand.<name>]`, with `implementation` and the settings flat in the table |
  | `profile = "standard"`                                                               | `implementation = "auction-protocol.openrtb"`                                                      |
  | `profile = "prebid-server"` and `profile = "aps"`                                    | `implementation = "auction.prebid-server"` and `implementation = "auction.aps"`                    |
  | `[auction] mediator = "adserver_mock"` and `[integrations.adserver_mock]`            | `[ad-server] module = "mock"` and `[ad-server.mock]`                                               |
  | `[integrations.aps] rendering_mode`                                                  | `rendering_mode` in the `[demand.<name>]` table of the `auction.aps` implementation                |
  | `[debug.auction_html_comment_options] include_mediator_response`                     | `include_adserver_response`                                                                        |

  `[auction] providers` and `[auction] mediator` are not ignored. A configuration still carrying either is refused with a message naming where the setting moved to. Provider names that carried a hyphen, such as `pbs-main`, become snake_case, such as `pbs_main`, and so does every `[auction.bidders.<code>] module` value pointing at one. Environment overlays follow the same names, so `TRUSTED_SERVER__AUCTION__PROVIDERS__PBS-MAIN__PROFILE_CONFIG__DEBUG` becomes `TRUSTED_SERVER__DEMAND__PBS_MAIN__DEBUG` and `TRUSTED_SERVER__INTEGRATIONS__<ID>__*` becomes `TRUSTED_SERVER__<SECTION>__<NAME>__*`, such as `TRUSTED_SERVER__AUCTION__PREBID__TIMEOUT_MS`. The old and new blobs are mutually incompatible, so activate the new binary and the new-shape config together, and roll back by restoring the old binary and the old-schema blob together.

  The `[integrations]` table is refused at startup and by `ts config push` with the move spelled out, and so is an `enabled` key left behind in a block, because an integration runs when, and only when, the section of its type selects its module. An integration that takes no settings needs no block, and one that requires a setting reports the setting it is missing. A name no builder supplies fails at startup, where the adapter's and a vendor crate's builders are known, while `ts config validate` accepts a name it does not know, because a vendor crate may supply it. Environment overrides move with the table, to `TRUSTED_SERVER__<SECTION>__<NAME>__<SETTING>`, and none of them can switch an integration on, because a module list is an array and the overlay replaces only scalar leaves that already exist. Secret references move with it, from `integrations.datadome.*` to `bot-protection.datadome.*`.

- `ts prebid server inspect` reads the `[demand.<name>]` tables whose implementation is `auction.prebid-server` and the `[auction.prebid]` block. Each reported demand source carries `selected`, which says whether `[demand] modules` names it, and the report's `selected` says whether `[auction] modules` selects `prebid`, in place of the removed `enabled_explicit`. `source_sections` now names `demand` and `auction.bidders` for the server side and `auction.prebid` for the browser side.
- **Breaking:** `ts prebid bundle` is now `ts prebid client`, alongside the new `ts prebid server` namespace. Update scripts and runbooks to use `ts prebid client` with the same arguments. The old `bundle` spelling is no longer accepted and has no compatibility alias.
- The S2S `/_ts/api/v1/batch-sync` endpoint now validates the full batch and calls the CAS-protected update path once per distinct normalized EC ID. The last valid UID wins within a group, and infrastructure failures reject the failing and each unprocessed group, so accepted and `kv_unavailable` input indexes may interleave.
- **Breaking:** Auction providers and bidder routes now use the configuration-first `[auction.providers.<id>]` and `[auction.bidders.<id>]` maps. The removed `[auction].providers = [...]` list and removed server fields under `[integrations.prebid]` and `[integrations.aps]` are rejected even when those integrations are disabled, and `ts config push` rejects the old shape before publication. Move PBS `server_url` to provider `endpoint`, server timeout to provider `timeout_ms`, request controls and bidder-parameter overrides to the `prebid-server` `profile_config`, notification suppression to `notifications`, and each former server bidder to an `[auction.bidders.<id>]` route. Move APS endpoint, timeout, account, inventory, debug, and creative controls to an `aps` provider and its `profile_config`. Browser Prebid settings remain under `[integrations.prebid]`; values such as timeout and debug that previously affected both browser and server behavior must now be configured for each owner. Provider endpoints must be absolute HTTPS URLs. Only bidder codes present in `[auction.bidders]` are folded into Trusted Server requests; unlisted publisher bids remain native browser demand. Provider response names now use the configured provider ID, such as `pbs-main`, instead of the legacy literal `prebid`; audit consumers that match `AuctionResponse.provider`. This schema has no mixed-version-safe deployment order: old binaries reject the maps and new binaries reject the retired fields, so activate the new binary and config blob together. Rollbacks must restore an old-schema blob together with the old binary.
- **Breaking** — Admin Basic-auth coverage now includes `GET /_ts/admin/ec`, `GET /_ts/admin/ec/{id}`, and `GET /_ts/admin/eids`. Existing configurations whose `[[handlers]]` patterns protect only the key-management endpoints now fail startup; broaden coverage before deploying, preferably with a namespace-boundary pattern such as `^/_ts/admin(?:/|$)`. Coverage of the dynamic `/_ts/admin/ec/{id}` route is no longer inferred from ID-shaped samples: the router accepts any segment after `/_ts/admin/ec/` and Basic Auth runs on the raw path before routing, so patterns anchored to the EC ID grammar (for example `^/_ts/admin/ec/[a-f0-9]{64}[.][A-Za-z0-9]{6}$`) are rejected in favor of a prefix-level matcher. Placeholder and well-known weak handler passwords (`changeme`, `password`, `admin`, `replace-with-…`) now fail startup on every handler rather than only on handlers inferred to cover an admin endpoint, because first-match-wins handler selection lets a narrow handler shadow the admin namespace.
- Prebid Server provider endpoints now normalize origin-only legacy `server_url` values to `/openrtb2/auction`. Query parameters are preserved, the canonical path loses a trailing slash, and configured non-root custom paths remain exact.
- Publisher HTML uses the browser-only `Cache-Control: private, max-age=60` policy for successful GET document responses and their `304 Not Modified` revalidations when server-side ad templates are structurally inactive, while preserving origin `private`/`no-store` policies and request-scoped bot, prefetch, or consent-denied responses. The `private` directive prevents shared caches that use `Cache-Control` from storing the document. Cookie-bearing responses using the generated inactive policy are finalized as `private, max-age=0`; CDN-specific cache headers remain unchanged and continue to control supporting CDNs independently. Set `[creative_opportunities].enabled = false` to disable publisher HTML and SPA template delivery without disabling direct `POST /auction` callers; an absent configuration, an unmatched slot, or a disabled auction also make the stack structurally inactive. An explicit `enabled = false` is not compatible with older binaries: restore the default, re-push and finalize the config before rolling back.
- **Breaking** — Replaced the legacy APS contextual integration with APS OpenRTB at `/e/pb/bid`. APS configuration now uses canonical `account_id` (`pub_id` remains a compatibility alias), no longer requires APS-specific slot IDs, and defaults script creative eligibility off. Operators must update the endpoint, disable native APS demand for Trusted Server cohorts, and prepare GAM/Universal Creative targeting for `hb_bidder=aps` before rollout. `aps` entries in Prebid bidder lists are logged and stripped. APS renderer winners now preserve the upstream bid `id`, omit `crid` when APS omits it, and carry `ext.trusted_server.renderer` instead of `adm`; external `/auction` consumers must support this response shape.
- **Breaking** — All auction paths now forward only a validated publisher-owned page URL as `site.page`, removing query and fragment data. APS OpenRTB omits `site.ref`; the existing Prebid Server path continues to forward the browser `Referer` as `site.ref`. Query-driven sites may lose contextual targeting and per-page reporting signals that previously came from query parameters.
- **Breaking** — `bid_param_zone_overrides` inner values must now be JSON objects; previously non-object or empty values (`"header" = "x"`, `"header" = {}`) were accepted and silently produced a dead rule at runtime. They now fail at startup with a configuration error. Operators upgrading should audit their `bid_param_zone_overrides` config for non-object zone entries.
- **Breaking**, Integration configuration strings are no longer globally reinterpreted as JSON scalars. Operators upgrading should audit each module's settings table and use native TOML/typed-config booleans and numbers (for example, `rewrite_sdk = true`, not `rewrite_sdk = "true"`), and quoted numeric and boolean scalars now fail validation instead of silently converting.
- **Breaking**, Sourcepoint browser module inclusion now requires selecting `sourcepoint` in `[cmp]`, so operators relying on the previous unconditional Sourcepoint module should select it before upgrading.
- **Breaking** — Auction creative sanitization is now opt-in: the new `[auction].sanitize_creatives` defaults to `false` because unconditional sanitization blanked script-based creatives (the majority of programmatic display) while recording normal impressions. `[auction].rewrite_creatives` keeps its `true` default. The per-creative cap is now enforced on rewritten output as well as raw input and in every processing mode (1 MiB for auction `adm`; proxied HTML documents keep the proxy's own 10 MiB bound), rewriting fails closed on parser errors instead of emitting partial output and never turns a rejected creative into a runtime-only `adm`, and `hb_cache_host`/`hb_cache_path` are emitted only for bids that supplied no creative — any bid carrying its own `adm` ships without them, so a processed or rejected creative can never be re-fetched raw from PBS Cache. Creative markup with no `<body>` token now receives the click-guard runtime, and bidder `<base>` elements are stripped whenever rewriting is enabled. The creative iframe sandbox no longer grants `allow-same-origin`, restoring origin isolation; rewritten-click recovery from the resulting opaque-origin iframe uses the GET `/first-party/proxy-rebuild` navigation fallback, now registered in every adapter and documented alongside the POST JSON form. Inside those iframes, dynamic resource signing and CORS-mode subresources (ES modules, `crossorigin` fonts) are unavailable pending the constrained asset capability in [#982](https://github.com/IABTechLab/trusted-server/issues/982); ordinary image, script, and stylesheet loads are unaffected. Upgrading: binaries that predate `sanitize_creatives` reject a blob carrying it, so upgrade the binary first, then push the config. Rollback: non-default values (`sanitize_creatives = true`, `rewrite_creatives = false`) are serialized into the config blob and older binaries reject unknown fields — before rolling back to a binary that predates a field, restore its default, push the default-compatible blob, then roll back.
- The SPA re-auction endpoint moved from `/__ts/page-bids` to `/_ts/page-bids`, joining every other internal route in the `/_ts/` namespace. The old path stays registered as a deprecated alias so already-loaded bundles keep serving ads, and responses on it carry a `Link: …; rel="deprecation"` header so remaining traffic is measurable from edge logs; removal is tracked in [#970](https://github.com/IABTechLab/trusted-server/issues/970). Two deployment notes: audit `[[handlers]]` for patterns broad enough to cover `/_ts` (for example `^/_ts`), which would put this browser-facing endpoint behind Basic Auth and return `401` to every visitor — scope them to `^/_ts/admin`; and prefer rolling forward over rolling back, since a server reverted past this release does not register the canonical path. In both cases the shipped client falls back to the deprecated alias, so the exposure is bounded until that alias is removed.
- Added optional APS `inventory_domain` and `inventory_page_origin` overrides for deployments whose edge hostname differs from the APS-authorized inventory identity.
- Preserved APS renderer capabilities through the client-side `trustedServer` Prebid adapter, allowing its generated `hb_adid` to render through GAM and Prebid Universal Creative instead of producing an empty creative.

### Security

- `/first-party/sign` now rejects valid targets outside `proxy.allowed_domains` before minting a proxy token. The creative runtime keeps image and iframe assignments blocked after this `403` policy response instead of loading the rejected URL directly; fetch-time checks still cover the initial target and every redirect.
- Reserved the complete admin namespace at the publisher-fallback boundary. Percent-encoded separators (`/_ts/admin%2Fec`, `%2f`, and double-encoded forms) matched the `^/_ts/admin` Basic-auth handler but escaped the literal-slash namespace check, so an authenticated request fell through to publisher fallback and forwarded its `Authorization` header and body to the publisher origin. The reservation now spans the whole `/_ts/admin` prefix plus the retired `/admin/keys` aliases — including trailing, descendant, and encoded-separator forms — evaluated on the raw path and on each of its bounded percent-decodings, so multi-encoded separators such as `/admin%252Fkeys/rotate` cannot survive to fallback for a proxy or origin to decode again, and applies to every adapter.
- Validate synthetic ID format on inbound values from the `x-synthetic-id` header and `synthetic_id` cookie; values that do not match the expected format (`64-hex-hmac.6-alphanumeric-suffix`) are discarded and a fresh ID is generated rather than forwarded to response headers, cookies, or third-party APIs

### Fixed

- `[auction].allowed_context_keys` now serializes in sorted, deduplicated order, so ESI template-cache fingerprints and `ts config diff`/`push` envelope hashes are stable across loads. Template fingerprints also sort object keys independently of `serde_json/preserve_order`. Existing envelopes may show a one-time allowlist reorder after upgrading; push once to settle it. The updated fingerprint format causes one template-cache miss per cached page after deployment.
- TSJS-generated envelopes now send `trustedServer.params.storedRequest: false`, preventing accidental PBS stored lookups without suppressing eligible non-PBS demand. PBS filters unusable impressions after overrides; explicit `true` and omission in valid envelopes retain inline-first stored fallback. A malformed envelope disables stored fallback for the entire slot, including independent direct demand left unusable after overrides. Publisher intent survives repeated and refresh auctions. Deploy compatible server admission everywhere before serving the new JS, and retain it during rollback while cached clients remain. See the Prebid deployment guide.
- Protocol-relative creative URLs now honor `rewrite.exclude_domains`, so excluded creative assets stay direct and excluded absolute or protocol-relative URLs submitted to `/first-party/sign` are rejected.
- Server-side ad template bids now always carry `hb_adid` in `window.tsjs.bids`. Bidders that return neither a Prebid Cache UUID nor an `adid` previously produced no `hb_adid` at all, so no `hb_adid` GPT targeting key was set and the Universal Creative render bridge had nothing to match — the winning creative never rendered. The OpenRTB bid `id`, which is mandatory per spec, is now the last-resort source; `cache_id` and `adid` still take priority where present. Blank `cacheId`/`adid` values no longer win that precedence and emit an unusable empty `hb_adid`, and `hb_cache_host`/`hb_cache_path` are now emitted only alongside a real Prebid Cache UUID — without one they pointed the Universal Creative at a guaranteed cache miss instead of letting it fall through to the inline creative.

### Added

- Added the `[auction].rewrite_creatives` (default `true`) and `[auction].sanitize_creatives` (default `false`) options. `rewrite_creatives` rewrites winning-bid adm to first-party endpoints across `POST /auction` and publisher SSAT/page-bids delivery (proxy/click URL conversion, bidder `<base>` removal; creative TSJS injection on `POST /auction` only). Enabling `sanitize_creatives` strips executable markup from winning-bid adm before delivery.
- `creative_opportunities.slot.gam_unit_path` is now a template supporting `{network_id}`, `{slot_id}`, and `{section}`, so a publisher whose ad unit varies by site section expresses it in one slot rule instead of one per (slot × section). `{section}` derives from the request path: `[creative_opportunities].section_segment` selects which path segment names the section (0-based, default `0`; set `1` for locale-prefixed URLs), and `section_root` supplies the value for paths with no such segment. `section_root` is required when a template uses `{section}`. Existing static and absent `gam_unit_path` configs are unchanged. Startup rejects a blank `gam_network_id` only when an absent/default path or `{network_id}` template consumes it. Trusted Server conservatively caps whole rendered dynamic paths at 100 UTF-8 bytes, informed by Google's 100-character per-ad-unit-code limit; an over-limit request-specific path omits that slot without failing the response. During typed/startup finalization, every placeholder-bearing template that omits `section_segment` materializes `section_segment = 0`, so an older binary rejects the blob loudly. Static and absent paths remain legacy-schema compatible only when both `section_root` and `section_segment` are omitted. Before rolling back below this feature, replace or remove dynamic paths, remove both keys, re-push and finalize the config, then roll back the binary.
- Added opt-in APS HTTP debug metadata for controlled test sites, exposing the direct request and response under `/auction` provider metadata using the Prebid Server `debug.httpcalls` shape.
- Added typed APS renderer transport for direct auctions and GAM/Prebid Universal Creative, using a minimized one-bid envelope, a fragment-bound nonce, and an opaque sandboxed renderer endpoint.
- Added Osano consent mirror integration docs and public enablement guidance.
- Implemented basic authentication for configurable endpoint paths (#73)
- Added integrations guide with example `testlight` integration

## [1.2.0] - 2025-10-14

### Changed

- Publisher origin backend now uses `publisher.origin_url` to dynamically create backends, deprecated `publisher.origin_backend` field
- Prebid backend now uses `prebid.server_url` to dynamically create backends, deprecated `prebid.prebid_backend` field
- Removed static backend definitions from `fastly.toml` for publisher and prebid

### Added

- Added `.rust-analyzer.json` for improved development environment support with Neovim/rust-analyzer

## [1.1.0] - 2025-10-05

### Added

- Added basic unit tests
- Added publisher config
- Add AI assist rules. Based on https://github.com/hashintel/hash
- Added ability to construct GAM requests from static permutive segments with test pages
- Add more complete e2e GAM (Google Ad Manager) integration with request construction and ad serving capabilities
- Add new partners.rs module for partner-specific configurations
- Created comprehensive publisher IDs audit document identifying hardcoded values
- Enabled first-party ad endpoints that rewrite creatives in first party domain
- Added first-party end point to proxy Prebid auctions
- Added Trusted Server TSJS SDK with bundled build, lint, and test tools for serving creatives in first-party domain

### Changed

- Upgrade to rust 1.90.0
- Upgrade to fastly-cli 12.0.0
- Changed to use constants for headers
- Changed to use log statements
- Updated fastly.toml for local development
- Changed to propagate server errors as HTTP errors
- Reworked Fastly routing so first-party endpoints and synthetic cookies stay in sync
- Added TypeScript CI lint, format, and test jobs for TSJS

### Fixed

- Rebuild when `TRUSTED_SERVER__*` env variables change

## [1.0.6] - 2025-05-29

### Changed

- Remove hard coded Fast ID in fastly.tom
- Updated README to better describe what Trusted Server does and high-level goal
- Use Rust toolchain version from .tool-versions for GitHub actions

## [1.0.5] - 2025-05-19

### Changed

- Refactor into crates to allow to separate Fastly implementation
- Remove references to POTSI
- Rename `potsi.toml` to `trusted-server.toml`

### Added

- Implemented GDPR consent for creating and passing synth headers

## [1.0.4] - 2025-04-29

### Added

- Implemented GDPR consent for creating and passing synth headers

## [1.0.3] - 2025-04-23

### Changed

- Upgraded to Fastly CLI v11.2.0

## [1.0.2] - 2025-03-28

### Added

- Documented project gogernance in [ProjectGovernance.md]
- Document FAQ for POC [FAQ_POC.md]

## [1.0.1] - 2025-03-27

### Changed

- Allow to templatize synthetic cookies

## [1.0.0] - 2025-03-26

### Added

- Initial implementation of Trusted Server

[Unreleased]: https://github.com/IABTechLab/trusted-server/compare/v1.2.0...HEAD
[1.2.0]: https://github.com/IABTechLab/trusted-server/compare/v1.1.0...v1.2.0
[1.1.0]: https://github.com/IABTechLab/trusted-server/compare/v1.0.6...v1.1.0
[1.0.6]: https://github.com/IABTechLab/trusted-server/compare/v1.0.5...v1.0.6
[1.0.5]: https://github.com/IABTechLab/trusted-server/compare/v1.0.4...v1.0.5
[1.0.4]: https://github.com/IABTechLab/trusted-server/compare/v1.0.3...v1.0.4
[1.0.3]: https://github.com/IABTechLab/trusted-server/compare/v1.0.2...v1.0.3
[1.0.2]: https://github.com/IABTechLab/trusted-server/compare/v1.0.1...v1.0.2
[1.0.1]: https://github.com/IABTechLab/trusted-server/compare/v1.0.0...v1.0.1
[1.0.0]: https://github.com/IABTechLab/trusted-server/releases/tag/v1.0.0
