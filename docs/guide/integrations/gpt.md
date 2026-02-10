# Google Publisher Tags (GPT) Integration

**Category**: Ad Serving
**Status**: Production
**Type**: First-Party Ad Tag Delivery

## Overview

The GPT integration enables first-party delivery of Google Publisher Tags by proxying GPT's entire script cascade through the publisher's domain. This eliminates third-party script loads, improving performance and reducing exposure to ad blockers and browser privacy restrictions.

## What is GPT?

Google Publisher Tags (GPT) is the JavaScript library publishers use to define and render ad slots served by Google Ad Manager. GPT loads scripts in a cascade:

1. `gpt.js` -- the thin bootstrap loader
2. `pubads_impl.js` -- the main GPT implementation (~640 KB)
3. `pubads_impl_*.js` -- lazy-loaded sub-modules (page-level ads, side rails, etc.)
4. Auxiliary scripts -- viewability, monitoring, error reporting

All of these are served from `securepubads.g.doubleclick.net` or `pagead2.googlesyndication.com`.

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

3. **Client-side shim** -- A script guard (`script_guard.ts`) patches `Element.prototype.appendChild` and `insertBefore` to intercept dynamically inserted `<script>` and `<link rel="preload">` elements. Any URLs pointing at Google's ad-serving domains are rewritten to the first-party proxy. This is the sole mechanism that routes GPT's cascaded script loads back through the proxy.

## Configuration

Add GPT configuration to `trusted-server.toml`:

```toml
[integrations.gpt]
enabled = true
script_url = "https://securepubads.g.doubleclick.net/tag/js/gpt.js"
cache_ttl_seconds = 3600
rewrite_script = true
```

### Configuration Options

| Field               | Type    | Required | Default                                                       | Description                                  |
| ------------------- | ------- | -------- | ------------------------------------------------------------- | -------------------------------------------- |
| `enabled`           | boolean | No       | `true`                                                        | Enable/disable the integration               |
| `script_url`        | string  | No       | `https://securepubads.g.doubleclick.net/tag/js/gpt.js`        | URL for the GPT bootstrap script             |
| `cache_ttl_seconds` | integer | No       | `3600`                                                        | Cache TTL for proxied scripts (60--86400s)   |
| `rewrite_script`    | boolean | No       | `true`                                                        | Whether to rewrite GPT script URLs in HTML   |

## Endpoints

- `GET /integrations/gpt/script` -- Serves the GPT bootstrap script (`gpt.js`)
- `GET /integrations/gpt/pagead/*` -- Proxies secondary GPT scripts and resources
- `GET /integrations/gpt/tag/*` -- Proxies tag-path resources

All responses include `X-GPT-Proxy: true` and `X-Script-Source` headers for debugging.

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

Intercepts dynamically inserted `<script>` and `<link rel="preload" as="script">` elements and rewrites Google ad-serving domain URLs to the first-party proxy. Handles:

- `securepubads.g.doubleclick.net`
- `pagead2.googlesyndication.com`
- `googletagservices.com`

### Command Queue Patch

Takes over `googletag.cmd` so every queued callback runs through a wrapper. This enables future hook points for:

- Synthetic ID injection as page-level key-value targeting
- Consent gating of ad requests
- Ad-unit path rewriting for A/B testing

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
- Inspect console for "GPT guard: installing DOM interception" log message

### Ads Not Rendering

**Symptoms**: Ad slots remain empty after proxying.

**Solutions**:

- Check the proxy responses have `200` status (look for `X-GPT-Proxy: true` header)
- Verify the `script_url` config points to the correct GPT endpoint
- Review server logs for upstream fetch failures

## Implementation

- **Rust**: [crates/common/src/integrations/gpt.rs](https://github.com/AnomalyCo/trusted-server/blob/main/crates/common/src/integrations/gpt.rs)
- **TypeScript**: [crates/js/lib/src/integrations/gpt/](https://github.com/AnomalyCo/trusted-server/blob/main/crates/js/lib/src/integrations/gpt/)

## Next Steps

- Review [Integrations Overview](/guide/integrations-overview) for comparison with other integrations
- Check [Configuration Reference](/guide/configuration) for advanced options
- Learn about [First-Party Proxy](/guide/first-party-proxy) architecture
- See [Google Ad Manager](/guide/integrations/gam) for the planned direct GAM integration
