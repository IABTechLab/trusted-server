# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Consent decoding for identity providers is stricter: a raw consent string that cannot be decoded blocks a provider whose policy is strict, and the GPP sharing, targeted-advertising and GPC fields of the US sections now block it too. A header-only GPP string with no applicable section does not block where TCF does not apply.
- The Lockr proxy now logs only the path of the upstream URL, never its query string.
- Identity KV is written at most once per EC-bearing request. Only a full page navigation from a returning visitor (the writer request) writes KV, once, after the response is sent: browser EIDs, pull partner results and the cluster size all go in that one write, with a generation check and no retry of the whole write. Every other route reads KV and stages but writes nothing. A consent-withdrawal tombstone and the removal-only retry of a failed write are the only exceptions. Mid-branch builds drop browser ingest until the whole change ships.
- `POST /auction`, `GET /_ts/page-bids` and `GET /_ts/api/v1/identify` no longer write KV, and `/_ts/api/v1/identify` omits expired IDs. The `/identify` response shape is unchanged: one UID per source.
- Pull partners now run from a visitor's second page load, from the writer request after send, and back off on errors. They are paced by `[ec.identity.providers.pull-<id>]` (`refresh_after_sec`, `max_calls_per_minute`), so the old default of 10 calls per hour becomes an effective 60 per hour. That 60 per hour is a ceiling, not a rate: `refresh_after_sec` and error backoff gate each partner first. Runtime startup now builds each pull partner's descriptor: a partner whose allowed host fails the descriptor host check stops startup. See [Identity Providers](docs/guide/configuration.md#identity-providers-ec-identity).
- **Breaking:** `ec.pull_sync_concurrency`, and `pull_sync_ttl_sec` and `pull_sync_rate_limit` on `[[ec.partners]]`, are removed, and validation now rejects them with an error that names the replacement. Use `[ec.identity].max_concurrency` (the example config's value moves from 3 to the default 4), `refresh_after_sec`, and `max_calls_per_minute = ceil(N / 60)` for `pull_sync_rate_limit = N`, the last two in `[ec.identity.providers.<pull-id>]`.
- A removal whose KV writes all fail now waits in a `pending:<ec_id>` KV key (one-day TTL) for the next writer navigation to merge, counted in `removal_deferred`; `removal_lost` counts only a removal that could not reach that key either. Positive IDs a capture sees on a request that is not the writer wait there too, so a capture's full entry set reaches KV. Every returning writer navigation reads that key once, a second KV read.
- A global revoke (`revoke_scope = "global"`) now also sets an `HttpOnly` `ts-ec-revoked` cookie with a fingerprint of the visitor's consent, and no EC is minted until that consent changes, so a CMP that still grants does not undo the revoke on the next page.
- `max_calls_per_ip_per_minute` (default 10) on each `[ec.identity.providers.<id>]` caps calls to that provider from one client address across visitors, next to the per-visitor cap.
- `[ec.identity] max_backend_requests` (default 32, Fastly's per-execution limit) bounds the origin, auction and identity provider calls of one request together. A provider call past it is skipped and is not counted as a failure, so it never opens the provider's circuit breaker. Hold-mode calls also cap their connect timeout at their call timeout.
- A provider opt-out clears on a TCF re-grant only when the opt-out was recorded under a TCF denial that is now granted; a later CMP re-save that was already granting no longer clears it. A clear records the opt-out it lifted rather than its own time, so it cannot lift an opt-out written after its request read the row.
- `Resolves::with_refresh_grace` lets an identity provider without private state keep an expired ID it wrote for a source it owns as a refresh anchor until the expiry plus the grace. The ID is never served meanwhile.
- With `[ec.identity] mirror_enabled = true` (off by default), `/auction` responses carry `ext.ts_eids_s` and `/_ts/page-bids` responses carry `ts_eids_s`: the payload (`value` with `max_age`, `clear`, or absent) for the client-only `__Host-ts-eids-s` mirror cookie, at most 2,048 bytes and one day. The shim now writes only `[[ec.partners]]` sources into `ts-eids`, merging each auction's entries per source instead of replacing the cookie, and no longer clears it when a request carries no EIDs (a TCF block still clears it).
- The `ts-ec-pull-complete` cookie is removed: Trusted Server no longer sets or reads it, and a copy a browser holds expires within one hour.
- **Breaking:** The KV identity row moves to schema version 2. A release before this one cannot read a version 2 row, so rolling back past this release is unsupported once it has written one. See [Rolling back](docs/guide/configuration.md#rolling-back). This release should merge after the EID-fidelity changes, whose behaviour its golden tests pin.
- The `EID sync measurement: ...` log line is no longer logged for returning visitors; only a new EC still logs it. Each request now logs one `identity request measurement: ...` line after the response is sent, with the inbound `Cookie` header size, KV reads and writes by route kind, the flush outcome, lost removals and the after-send withdrawal run. It holds no identifiers. Operator dashboards that read the old line should read the new one.
- **Breaking:** Prebid Server no longer receives any Trusted Server identity cookie (`ts-ec`, `ts-eids`, and the `__Host-ts-eids-s` mirror that this release's shim writes); today it receives `ts-ec` and `ts-eids`. The publisher origin still receives `ts-ec` and `ts-eids` but never the mirror. `[ec] strip_eids_cookie_from_origin = true` strips `ts-eids` from origin requests too.
- **Breaking:** `x-ts-version` now reports the deployed git version — the release tag, else the branch, else the first 6 characters of the commit — compiled in from the build-time `TRUSTED_SERVER__GIT_VERSION` (set by the deploy pipeline) or local git, and is sent by every adapter, including on the Fastly `GET /health` probe. The Fastly service version it previously carried is now `x-ts-fastly-version`; update dashboards, monitors, and scripts that read `x-ts-version` as the Fastly version number. Builds with neither the override nor git omit the header.
- **Breaking:** `ts prebid bundle` is now `ts prebid client`, alongside the new `ts prebid server` namespace. Update scripts and runbooks to use `ts prebid client` with the same arguments. The old `bundle` spelling is no longer accepted and has no compatibility alias.
- The S2S `/_ts/api/v1/batch-sync` endpoint now validates the full batch and calls the CAS-protected update path once per distinct normalized EC ID. The last valid UID wins within a group, and infrastructure failures reject the failing and each unprocessed group, so accepted and `kv_unavailable` input indexes may interleave.
- **Breaking:** Auction providers and bidder routes now use the configuration-first `[auction.providers.<id>]` and `[auction.bidders.<id>]` maps. The removed `[auction].providers = [...]` list and removed server fields under `[integrations.prebid]` and `[integrations.aps]` are rejected even when those integrations are disabled, and `ts config push` rejects the old shape before publication. Move PBS `server_url` to provider `endpoint`, server timeout to provider `timeout_ms`, request controls and bidder-parameter overrides to the `prebid-server` `profile_config`, notification suppression to `notifications`, and each former server bidder to an `[auction.bidders.<id>]` route. Move APS endpoint, timeout, account, inventory, debug, and creative controls to an `aps` provider and its `profile_config`. Browser Prebid settings remain under `[integrations.prebid]`; values such as timeout and debug that previously affected both browser and server behavior must now be configured for each owner. Provider endpoints must be absolute HTTPS URLs. Only bidder codes present in `[auction.bidders]` are folded into Trusted Server requests; unlisted publisher bids remain native browser demand. Provider response names now use the configured provider ID, such as `pbs-main`, instead of the legacy literal `prebid`; audit consumers that match `AuctionResponse.provider`. This schema has no mixed-version-safe deployment order: old binaries reject the maps and new binaries reject the retired fields, so activate the new binary and config blob together. Rollbacks must restore an old-schema blob together with the old binary.
- **Breaking** — Admin Basic-auth coverage now includes `GET /_ts/admin/ec`, `GET /_ts/admin/ec/{id}`, and `GET /_ts/admin/eids`. Existing configurations whose `[[handlers]]` patterns protect only the key-management endpoints now fail startup; broaden coverage before deploying, preferably with a namespace-boundary pattern such as `^/_ts/admin(?:/|$)`. Coverage of the dynamic `/_ts/admin/ec/{id}` route is no longer inferred from ID-shaped samples: the router accepts any segment after `/_ts/admin/ec/` and Basic Auth runs on the raw path before routing, so patterns anchored to the EC ID grammar (for example `^/_ts/admin/ec/[a-f0-9]{64}[.][A-Za-z0-9]{6}$`) are rejected in favor of a prefix-level matcher. Placeholder and well-known weak handler passwords (`changeme`, `password`, `admin`, `replace-with-…`) now fail startup on every handler rather than only on handlers inferred to cover an admin endpoint, because first-match-wins handler selection lets a narrow handler shadow the admin namespace.
- Prebid Server provider endpoints now normalize origin-only legacy `server_url` values to `/openrtb2/auction`. Query parameters are preserved, the canonical path loses a trailing slash, and configured non-root custom paths remain exact.
- Publisher HTML uses the browser-only `Cache-Control: private, max-age=60` policy for successful GET document responses and their `304 Not Modified` revalidations when server-side ad templates are structurally inactive, while preserving origin `private`/`no-store` policies and request-scoped bot, prefetch, or consent-denied responses. The `private` directive prevents shared caches that use `Cache-Control` from storing the document. Cookie-bearing responses using the generated inactive policy are finalized as `private, max-age=0`; CDN-specific cache headers remain unchanged and continue to control supporting CDNs independently. Set `[creative_opportunities].enabled = false` to disable publisher HTML and SPA template delivery without disabling direct `POST /auction` callers; an absent configuration, an unmatched slot, or a disabled auction also make the stack structurally inactive. An explicit `enabled = false` is not compatible with older binaries: restore the default, re-push and finalize the config before rolling back.
- **Breaking** — Replaced the legacy APS contextual integration with APS OpenRTB at `/e/pb/bid`. APS configuration now uses canonical `account_id` (`pub_id` remains a compatibility alias), no longer requires APS-specific slot IDs, and defaults script creative eligibility off. Operators must update the endpoint, disable native APS demand for Trusted Server cohorts, and prepare GAM/Universal Creative targeting for `hb_bidder=aps` before rollout. `aps` entries in Prebid bidder lists are logged and stripped. APS renderer winners now preserve the upstream bid `id`, omit `crid` when APS omits it, and carry `ext.trusted_server.renderer` instead of `adm`; external `/auction` consumers must support this response shape.
- **Breaking** — All auction paths now forward only a validated publisher-owned page URL as `site.page`, removing query and fragment data. APS OpenRTB omits `site.ref`; the existing Prebid Server path continues to forward the browser `Referer` as `site.ref`. Query-driven sites may lose contextual targeting and per-page reporting signals that previously came from query parameters.
- **Breaking** — `bid_param_zone_overrides` inner values must now be JSON objects; previously non-object or empty values (`"header" = "x"`, `"header" = {}`) were accepted and silently produced a dead rule at runtime. They now fail at startup with a configuration error. Operators upgrading should audit their `bid_param_zone_overrides` config for non-object zone entries.
- **Breaking** — Integration configuration strings are no longer globally reinterpreted as JSON scalars. Operators upgrading should audit `[integrations.*]` settings and use native TOML/typed-config booleans and numbers (for example, `enabled = true`, not `enabled = "true"`); quoted numeric and boolean scalars now fail validation instead of silently converting.
- **Breaking** — Sourcepoint browser module inclusion now requires explicit `[integrations.sourcepoint].enabled = true`; operators relying on the previous unconditional Sourcepoint module should enable the integration before upgrading.
- **Breaking** — Auction creative sanitization is now opt-in: the new `[auction].sanitize_creatives` defaults to `false` because unconditional sanitization blanked script-based creatives (the majority of programmatic display) while recording normal impressions. `[auction].rewrite_creatives` keeps its `true` default. The per-creative cap is now enforced on rewritten output as well as raw input and in every processing mode (1 MiB for auction `adm`; proxied HTML documents keep the proxy's own 10 MiB bound), rewriting fails closed on parser errors instead of emitting partial output and never turns a rejected creative into a runtime-only `adm`, and `hb_cache_host`/`hb_cache_path` are emitted only for bids that supplied no creative — any bid carrying its own `adm` ships without them, so a processed or rejected creative can never be re-fetched raw from PBS Cache. Creative markup with no `<body>` token now receives the click-guard runtime, and bidder `<base>` elements are stripped whenever rewriting is enabled. The creative iframe sandbox no longer grants `allow-same-origin`, restoring origin isolation; rewritten-click recovery from the resulting opaque-origin iframe uses the GET `/first-party/proxy-rebuild` navigation fallback, now registered in every adapter and documented alongside the POST JSON form. Inside those iframes, dynamic resource signing and CORS-mode subresources (ES modules, `crossorigin` fonts) are unavailable pending the constrained asset capability in [#982](https://github.com/IABTechLab/trusted-server/issues/982); ordinary image, script, and stylesheet loads are unaffected. Upgrading: binaries that predate `sanitize_creatives` reject a blob carrying it, so upgrade the binary first, then push the config. Rollback: non-default values (`sanitize_creatives = true`, `rewrite_creatives = false`) are serialized into the config blob and older binaries reject unknown fields — before rolling back to a binary that predates a field, restore its default, push the default-compatible blob, then roll back.
- The SPA re-auction endpoint moved from `/__ts/page-bids` to `/_ts/page-bids`, joining every other internal route in the `/_ts/` namespace. The old path stays registered as a deprecated alias so already-loaded bundles keep serving ads, and responses on it carry a `Link: …; rel="deprecation"` header so remaining traffic is measurable from edge logs; removal is tracked in [#970](https://github.com/IABTechLab/trusted-server/issues/970). Two deployment notes: audit `[[handlers]]` for patterns broad enough to cover `/_ts` (for example `^/_ts`), which would put this browser-facing endpoint behind Basic Auth and return `401` to every visitor — scope them to `^/_ts/admin`; and prefer rolling forward over rolling back, since a server reverted past this release does not register the canonical path. In both cases the shipped client falls back to the deprecated alias, so the exposure is bounded until that alias is removed.
- Added optional APS `inventory_domain` and `inventory_page_origin` overrides for deployments whose edge hostname differs from the APS-authorized inventory identity.
- Preserved APS renderer capabilities through the client-side `trustedServer` Prebid adapter, allowing its generated `hb_adid` to render through GAM and Prebid Universal Creative instead of producing an empty creative.

### Security

- `/first-party/sign` now rejects valid targets outside `proxy.allowed_domains` before minting a proxy token. The creative runtime keeps image and iframe assignments blocked after this `403` policy response instead of loading the rejected URL directly; fetch-time checks still cover the initial target and every redirect.
- Reserved the complete admin namespace at the publisher-fallback boundary. Percent-encoded separators (`/_ts/admin%2Fec`, `%2f`, and double-encoded forms) matched the `^/_ts/admin` Basic-auth handler but escaped the literal-slash namespace check, so an authenticated request fell through to publisher fallback and forwarded its `Authorization` header and body to the publisher origin. The reservation now spans the whole `/_ts/admin` prefix plus the retired `/admin/keys` aliases — including trailing, descendant, and encoded-separator forms — evaluated on the raw path and on each of its bounded percent-decodings, so multi-encoded separators such as `/admin%252Fkeys/rotate` cannot survive to fallback for a proxy or origin to decode again, and applies to every adapter.
- Validate synthetic ID format on inbound values from the `x-synthetic-id` header and `synthetic_id` cookie; values that do not match the expected format (`64-hex-hmac.6-alphanumeric-suffix`) are discarded and a fresh ID is generated rather than forwarded to response headers, cookies, or third-party APIs

### Fixed

- Publisher responses now map origin URLs in `Location`, `Content-Location`, `Refresh`, and `Link` headers to the serving host, and each CSP source naming the origin gains a serving-host source beside it (#1128). A redirect to the current URL under a different scheme, or a `GET`/`HEAD` redirect to the current URL that sets no cookie (or any such redirect under a `Host` override), is left unchanged to avoid a loop. `<meta>` refresh and CSP tags in the page body are not rewritten. See "Origin URLs in proxied response headers" in the configuration guide.
- Image Optimizer configuration now rejects profile and profile-set names within the same map that collide after trimming, even when their values match, instead of silently overwriting an entry. Empty names continue to be discarded.
- Browser integration bundles now share context providers and the configured log level with TSJS core (#1196). With Permutive enabled and cohorts available, `tsjs.requestAds()` now sends `config.permutive_segments` to `/auction`; the server retains that key only when `auction.allowed_context_keys` includes `"permutive_segments"`. This restores a previously inactive data flow, not a consent gate: review publisher consent handling before relying on it. Integration logs now follow `tsjs.setConfig` and `tsjs.log.setLevel`, and creative no longer raises the default `warn` level to `info`. The `?tsdebug=1` query parameter or `localStorage['tsdebug'] = '1'` intentionally enables `debug` logging across all bundles when creative installs, overriding any log level set earlier through `tsjs.setConfig` or `tsjs.log.setLevel`. Later calls to either API can change the level again.
- `[auction].allowed_context_keys` now serializes in sorted, deduplicated order, so ESI template-cache fingerprints and `ts config diff`/`push` envelope hashes are stable across loads. Template fingerprints also sort object keys independently of `serde_json/preserve_order`. Existing envelopes may show a one-time allowlist reorder after upgrading; push once to settle it. The updated fingerprint format causes one template-cache miss per cached page after deployment.
- TSJS-generated envelopes now send `trustedServer.params.storedRequest: false`, preventing accidental PBS stored lookups without suppressing eligible non-PBS demand. PBS filters unusable impressions after overrides; explicit `true` and omission in valid envelopes retain inline-first stored fallback. A malformed envelope disables stored fallback for the entire slot, including independent direct demand left unusable after overrides. Publisher intent survives repeated and refresh auctions. Deploy compatible server admission everywhere before serving the new JS, and retain it during rollback while cached clients remain. See the Prebid deployment guide.
- Protocol-relative creative URLs now honor `rewrite.exclude_domains`, so excluded creative assets stay direct and excluded absolute or protocol-relative URLs submitted to `/first-party/sign` are rejected.
- Server-side ad template bids now always carry `hb_adid` in `window.tsjs.bids`. Bidders that return neither a Prebid Cache UUID nor an `adid` previously produced no `hb_adid` at all, so no `hb_adid` GPT targeting key was set and the Universal Creative render bridge had nothing to match — the winning creative never rendered. The OpenRTB bid `id`, which is mandatory per spec, is now the last-resort source; `cache_id` and `adid` still take priority where present. Blank `cacheId`/`adid` values no longer win that precedence and emit an unusable empty `hb_adid`, and `hb_cache_host`/`hb_cache_path` are now emitted only alongside a real Prebid Cache UUID — without one they pointed the Universal Creative at a guaranteed cache miss instead of letting it fall through to the inline creative.

### Added

- Server-side identity provider framework. An integration registers an `IdentityProvider` with `try_with_identity_provider`, and the coordinator handles consent, transport limits, backoff and the single KV write. Provider authors get a test harness (`ProviderHarness`, `check_conformance`, `output_eids`). See [Writing an Identity Provider](docs/guide/identity-providers.md). No vendor ships with the framework.
- Source ownership: `owner` on an `[[ec.partners]]` entry names the provider that owns a source. An owner's result replaces the source's whole entry set, and batch sync writes an owned source only when the row holds no value for it (an expired owned record counts as present and is not filled). Validation rejects an owner that is not enabled, is capture-only, does not claim the source, or collides with another provider. See [Source ownership](docs/guide/configuration.md#source-ownership-ec-partners-owner).
- Hold mode: `mode = "hold"` in `[ec.identity.providers.<id>]` runs a provider before the navigation's auction, within the shared `hold_budget_ms`, with `timeout_ms` and `after_send_timeout_ms` as call timeouts.
- Revoke-only passive capture for providers that proxy a vendor's API: a captured removal or opt-out is written to KV, a positive result clears an opt-out, and positive IDs are only counted. `revoke_scope` (`provider` by default, or `global` to run the consent-withdrawal tombstone) sets how far an opt-out reaches. Removals and opt-outs apply from any client, as consent withdrawals do; only a clear needs a real browser. A capture-only provider must list its `sources`. See the [capture provider checklist](docs/guide/edge-cookies.md#capture-provider-checklist).
- Complete provider EIDs: a provider can return several entries and UIDs per source, with UID-level `atype` and `ext` and EID-level `inserter`, `matcher`, `mm` and `ext`. KV, the bidstream, `x-ts-eids` and the `ts-eids-s` mirror carry the whole set in order; `/identify` still returns one UID per source. See [Complete provider EIDs](docs/guide/edge-cookies.md#complete-provider-eids).
- `/_ts/api/v1/identify` returns an ID a server-side provider wrote only to a browser call from the publisher's site (an allowed `Origin` and `Sec-Fetch-Site: same-origin` or `same-site`), because the EC ID is a bearer value partners and the origin hold. Other IDs are returned as before.
- The `capture_unpersisted` and `removal_lost` counters in the `identity request measurement: ...` log line, and a per-provider circuit breaker (`breaker_failures_per_minute`) that skips a provider for every visitor after repeated transport failures, timeouts, 429s or 5xx responses.

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
