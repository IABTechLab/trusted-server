# Google Publisher Tags (GPT) Integration

**Category**: Ad Serving
**Status**: Production
**Type**: First-Party Ad Tag Delivery

## Overview

The GPT integration delivers Google Publisher Tags via the publisher's domain by proxying GPT's script cascade in first-party context. This avoids cross-origin script loads, improving performance and reducing friction with ad blockers and Intelligent Tracking Prevention.

## What is GPT?

Google Publisher Tags (GPT) is the JavaScript library publishers use to define and render ad slots served by Google Ad Manager. GPT loads scripts in a cascade:

1. `gpt.js` -- the thin bootstrap loader
2. `pubads_impl.js` -- the main GPT implementation (~640 KB)
3. `pubads_impl_*.js` -- lazy-loaded sub-modules (page-level ads, side rails, etc.)
4. Auxiliary scripts -- viewability, monitoring, error reporting

All of these are served from `securepubads.g.doubleclick.net`.

## How It Works

```
  Publisher HTML
  │
  ├─ <script src="securepubads.g.doubleclick.net/tag/js/gpt.js">
  │   ↓ (attribute rewriter)
  │   <script src="publisher.com/integrations/gpt/script">
  │
  ├─ Server fetches gpt.js from Google, serves it verbatim
  │
  ├─ Client-side shim intercepts dynamic script insertions
  │   ↓ (script guard)
  │   securepubads.g.doubleclick.net/pagead/…
  │   → publisher.com/integrations/gpt/pagead/…
  │
  └─ Server proxies cascade scripts from Google, serves verbatim
```

There are three layers:

1. **HTML attribute rewriting** (server-side) -- Rewrites `src`/`href` attributes on the initial `gpt.js` `<script>` tag to the first-party endpoint `/integrations/gpt/script`.

2. **Script proxy** (server-side) -- Fetches scripts from Google and serves them through the publisher's domain. Script bodies are served **verbatim** with no modification.

3. **Client-side shim** -- A script guard (`script_guard.ts`) uses six interception layers -- `document.write` interception, `HTMLScriptElement.prototype.src` property descriptor, `setAttribute` patch, `document.createElement` patch, DOM insertion patches, and a `MutationObserver` -- to catch GPT script URLs regardless of how they are set or inserted. The `document.write` layer is the most critical, as GPT's primary loading path uses `document.write` to synchronously inject `pubads_impl.js` into the HTML parser stream. This is the sole mechanism that routes GPT's cascaded script loads back through the proxy.

## Configuration

Add GPT configuration to `trusted-server.toml`:

```toml
[integrations.gpt]
enabled = true
gam_attribution_enabled = false
script_url = "https://securepubads.g.doubleclick.net/tag/js/gpt.js"
cache_ttl_seconds = 3600
rewrite_script = true
```

### Configuration Options

| Field                     | Type    | Required | Default                                                | Description                                                       |
| ------------------------- | ------- | -------- | ------------------------------------------------------ | ----------------------------------------------------------------- |
| `enabled`                 | boolean | No       | `true`                                                 | Enable/disable the integration                                    |
| `gam_attribution_enabled` | boolean | No       | `false`                                                | Add fixed page-level `ts=true` targeting for GAM cohort reporting |
| `script_url`              | string  | No       | `https://securepubads.g.doubleclick.net/tag/js/gpt.js` | URL for the GPT bootstrap script                                  |
| `cache_ttl_seconds`       | integer | No       | `3600`                                                 | Cache TTL for proxied scripts (60--86400s)                        |
| `rewrite_script`          | boolean | No       | `true`                                                 | Whether to rewrite GPT script URLs in HTML                        |

The environment override
`TRUSTED_SERVER__INTEGRATIONS__GPT__GAM_ATTRIBUTION_ENABLED` works only when
`gam_attribution_enabled` is already present under `[integrations.gpt]` in the
TOML file. The environment overlay cannot create a missing configuration leaf.

## Endpoints

- `GET /integrations/gpt/script` -- Serves the GPT bootstrap script (`gpt.js`)
- `GET /integrations/gpt/pagead/*` -- Proxies secondary GPT scripts and resources
- `GET /integrations/gpt/tag/*` -- Proxies tag-path resources

Successful proxy responses include the `X-GPT-Proxy: true` header for debugging.

## Features

- **Full cascade proxying**: Every script in GPT's loading chain is served first-party
- **Verbatim script delivery**: No server-side script modification -- scripts are proxied as-is
- **Client-side interception**: DOM-level script guard catches all dynamic script insertions
- **Configurable caching**: Tune TTL per deployment (default 1 hour, range 60s--24h)
- **HTML attribute rewriting**: Automatic rewrite of `src`/`href` attributes in publisher HTML
- **Protocol-aware**: The client-side shim matches the page's protocol (HTTP for local dev, HTTPS for production)

## Client-Side Shim

The GPT integration includes a TypeScript module bundled into the unified TSJS bundle. It provides two capabilities:

### Script Guard

The script guard uses six interception layers to catch GPT script URLs regardless of how they are set or inserted into the DOM:

1. **`document.write` / `document.writeln`** -- GPT's primary loading mechanism. When `gpt.js` loads synchronously, it uses `document.write` to inject `<script src="...pubads_impl.js">` directly into the HTML parser stream. The guard intercepts these calls and rewrites URLs whose hostname is `securepubads.g.doubleclick.net` inside the HTML string before passing it to the native method.
2. **Property descriptor** on `HTMLScriptElement.prototype.src` -- intercepts `script.src = url` assignments. This catches GPT's async fallback path (used when `document.write` is unavailable, e.g. after page load or with `async` scripts).
3. **`setAttribute` patch** on `HTMLScriptElement.prototype` -- catches `script.setAttribute('src', url)` calls that bypass the property setter.
4. **`document.createElement` patch** -- tags every newly created `<script>` element with a per-instance `src` descriptor, ensuring coverage even if the prototype-level descriptor cannot be installed.
5. **DOM insertion patches** on `appendChild` / `insertBefore` -- catches scripts and `<link rel="preload">` elements whose `src`/`href` is already set at insertion time.
6. **`MutationObserver`** -- catches elements added via `innerHTML`, `.append()`, or other DOM methods, as well as attribute mutations on existing elements.

Intercepts scripts from `securepubads.g.doubleclick.net` and rewrites them to the first-party proxy.

### Command Queue Patch

Takes over `googletag.cmd` so every queued callback is wrapped before GPT executes it. This enables future hook points for:

- EC ID injection as page-level key-value targeting
- Consent gating of ad requests
- Ad-unit path rewriting for A/B testing

### GAM Treatment Attribution

Setting `gam_attribution_enabled = true` adds the fixed page-level GPT targeting
value `ts=true`. It is applied before publisher GPT initialization and remains
for the browser document's lifetime, so initial, lazy, refresh, publisher-owned,
and SPA-route requests inherit it unless another targeting consumer clears or
overrides the key. The attribution switch is independently controlled and
defaults to `false`, but the GPT integration's `enabled` master switch must also
be `true`.

This key is distinct from the existing slot-level `ts_initial=1` value.
`ts_initial` retains its current cleanup lifecycle; Trusted Server does not
clear the page-level `ts` value during Prebid refresh or SPA cleanup.

For an eligible publisher document whose activation script was not cloned,
`ts=true` means Trusted Server emitted the rewritten document head before the
GPT request. It does not prove that the response body completed, that a Trusted
Server bid won, or that an impression was caused by treatment. A publisher can
copy the activation script with `srcdoc` or `document.write`; treat any marker
on an unrewritten nested document as contamination, not attribution proof.

Before enabling attribution in a cohort:

1. Complete privacy and CSP review, create the reportable predefined `true`
   value in the target GAM network, and verify the chosen GAM reporting surface
   and billing approval.
2. Audit the short `ts` key across publisher GPT code, effective Prebid
   `bidderSettings[*].adserverTargeting` output (including
   `setTargetingForGPTAsync`), the effective creative-opportunity targeting map,
   and every GAM consumer that can affect eligibility, pricing, protection, or
   routing. Trusted Server accepts and forwards operator targeting verbatim; it
   does not reserve, filter, or intercept a slot-level `ts` key at runtime.
3. With treatment routing stopped, deploy attribution enabled and validate
   initial, lazy, refresh, publisher-owned, and SPA requests. Confirm every
   excluded path reports zero marked requests, then save a short paired-report
   dry run that satisfies the invariants below before starting the cohort.

For reporting, save one exact eligible universe: GAM network, inventory units,
routes, formats, time zone, date window, metrics, and all exclusions. Report A
is the nonduplicated total for that universe. Report B uses identical filters
and metrics plus exactly `ts=true`. If Enhanced Key-Value reporting is
unavailable, unapproved, or incompatible, use an exactly filtered legacy
key-value report and never sum its repeated key-value rows. Derive control as
`A - B`, and require `0 <= B <= A` for every metric. A violation invalidates the
whole report pair; never clamp a negative result. Use the same reporting-latency
and invalid-traffic maturation window for both reports.

GAM results are descriptive delivery attribution, not a causal treatment
effect. Aggregate monitoring and synthetic/manual samples can detect obvious
failures but cannot prove marker completeness on every production request
without request-correlated telemetry.

For a normal rollback, first stop and verify new treatment assignment at the
router, record a clean reporting boundary, and let already-open documents drain.
Exclude the drain interval, then set `gam_attribution_enabled = false` after
marked traffic reaches zero for the agreed interval. An emergency kill may flip
the setting immediately, but the affected interval and subsequent drain must be
treated as invalid for experiment reporting.

## Use Cases

### First-Party Ad Delivery

**Problem**: Third-party script loads from Google's domains are blocked by ad blockers and browser privacy features.

**Solution**: GPT integration routes all scripts through the publisher's domain, making them indistinguishable from first-party resources.

### Local Development

**Problem**: GPT scripts fail to load or behave differently in local development environments.

**Solution**: The integration works with both HTTP and HTTPS schemes. When running locally with Viceroy, the client-side shim produces `http://` URLs matching the dev server.

## Troubleshooting

### Scripts Not Loading Through Proxy

**Symptoms**: Network tab shows requests to `securepubads.g.doubleclick.net` instead of first-party domain.

**Solutions**:

- Verify `rewrite_script` is `true` in config
- Check that the TSJS bundle with the GPT shim is loaded **before** GPT
- Inspect console for "GPT guard: installing interception for Google ad scripts" log message

### Ads Not Rendering

**Symptoms**: Ad slots remain empty after proxying.

**Solutions**:

- Check the proxy responses have `200` status (look for `X-GPT-Proxy: true` header)
- Verify the `script_url` config points to the correct GPT endpoint
- Review server logs for upstream fetch failures

## Implementation

- **Rust**: [crates/trusted-server-core/src/integrations/gpt.rs](https://github.com/IABTechLab/trusted-server/blob/main/crates/trusted-server-core/src/integrations/gpt.rs)
- **TypeScript**: [crates/trusted-server-js/lib/src/integrations/gpt/](https://github.com/IABTechLab/trusted-server/blob/main/crates/trusted-server-js/lib/src/integrations/gpt/)

## Next Steps

- Review [Integrations Overview](/guide/integrations-overview) for comparison with other integrations
- Check [Configuration Reference](/guide/configuration) for advanced options
- Learn about [First-Party Proxy](/guide/first-party-proxy) architecture
- See [Google Ad Manager](/guide/integrations/gam) for the planned direct GAM integration
