# Integrations Overview

Trusted Server provides built-in integrations with third-party services for first-party data collection and consent-aware advertising. The available integrations are compared below.

## Quick Comparison

| Integration         | Type                | Endpoints  | HTML Rewriting               | Primary Use Case            | Status      |
| ------------------- | ------------------- | ---------- | ---------------------------- | --------------------------- | ----------- |
| **Prebid**          | Proxy + Rewriter    | 2-3 routes | Removes Prebid.js scripts    | Server-side header bidding  | Production  |
| **Next.js**         | Script Rewriter     | None       | Rewrites Next.js data        | First-party Next.js routing | Production  |
| **Permutive**       | Proxy + Rewriter    | 6 routes   | Rewrites SDK URLs            | First-party audience data   | Production  |
| **Sourcepoint**     | Proxy + Rewriter    | 2 routes   | Rewrites CMP asset URLs      | First-party CMP delivery    | Development |
| **Osano**           | Browser Mirror      | None       | Consent cookie mirroring     | First-party consent signals | Development |
| **GPT Diagnostics** | Browser Diagnostics | None       | Closed-shadow local console  | GPT lifecycle debugging     | Development |
| **Testlight**       | Proxy + Rewriter    | 1 route    | Rewrites integration scripts | Testing/development         | Development |

## Integration Details

### Prebid

**What it does:** Supplies the browser Prebid.js bundle and `trustedServer`
adapter while auction provider maps independently configure server-side Prebid
Server demand.

**Key Features:**

- OpenRTB 2.x protocol conversion
- EC ID injection into bid requests
- First-party creative resource proxying
- CDN URL rewriting (7+ major SSPs)
- GPC signal support
- Request signing for authentication

**Configuration:**

```toml
[integrations.prebid]
enabled = true
timeout_ms = 1000
debug = false
client_side_bidders = ["example-browser"]
external_bundle_url = "https://assets.example.com/prebid/trusted-prebid.js"

[proxy]
allowed_domains = ["assets.example.com"]

[auction.providers.pbs-main]
protocol = "openrtb-2.6"
profile = "prebid-server"
endpoint = "https://prebid.example.com/openrtb2/auction"
routing = "explicit"

[auction.bidders.example-server]
provider = "pbs-main"
```

**Endpoints:**

- `POST /auction` - Browser and programmatic auction endpoint
- `GET /integrations/prebid/bundle.js` - First-party external bundle proxy
- `GET <script_patterns>` - Configured empty-script interception routes

**When to use:** You want to monetize your site with programmatic advertising while maintaining first-party context.

**Learn more:** [Ad Serving Guide](./ad-serving.md)

---

### Next.js

**What it does:** Rewrites Next.js application data to route traffic through Trusted Server's first-party proxy.

**Key Features:**

- Next.js 13+ App Router support (RSC streaming)
- Pages Router support (static data payload)
- Configurable attribute rewriting
- Protocol-relative URL handling
- Preserves JSON structure

**Configuration:**

```toml
[integrations.nextjs]
enabled = false
rewrite_attributes = ["href", "link", "url"]
```

**Endpoints:** None (pure HTML/script rewriting)

**When to use:** You have a Next.js application and want to ensure all links and assets route through your first-party domain.

**Learn more:** [Integration Guide](./integration-guide.md)

---

### Permutive

**What it does:** Provides first-party data collection and audience segmentation by proxying Permutive's SDK and API endpoints.

**Key Features:**

- Complete first-party SDK serving
- Multi-endpoint proxying (API, Events, Sync, Secure Signals, CDN)
- SDK caching for performance
- Cookies set on the publisher's domain
- Header forwarding for authentication

**Configuration:**

```toml
[integrations.permutive]
enabled = true
organization_id = "myorg"
workspace_id = "workspace-12345"
project_id = "project-789"
api_endpoint = "https://api.permutive.com"
secure_signals_endpoint = "https://secure-signals.permutive.app"
cache_ttl_seconds = 3600
rewrite_sdk = true
```

**Endpoints:**

- `GET /integrations/permutive/sdk` - SDK serving
- `GET/POST /integrations/permutive/api/*` - API proxy
- `GET/POST /integrations/permutive/secure-signal/*` - Secure Signals
- `GET/POST /integrations/permutive/events/*` - Event collection
- `GET/POST /integrations/permutive/sync/*` - ID synchronization
- `GET /integrations/permutive/cdn/*` - CDN proxy

**When to use:** You use Permutive for audience segmentation and want to maintain first-party data collection.

**Learn more:** [Integration Guide](./integration-guide.md)

---

### Sourcepoint

**What it does:** Proxies Sourcepoint CMP CDN endpoints through Trusted Server and rewrites publisher references to first-party paths.

**Key Features:**

- CDN proxy for `cdn.privacy-mgmt.com`
- HTML attribute rewriting for Sourcepoint assets
- JavaScript body rewriting for webpack chunks and API URLs
- Head-injected `window._sp_` property trap for runtime config
- Client-side script guard for dynamic script insertion

**Configuration:**

```toml
[integrations.sourcepoint]
enabled = true
rewrite_sdk = true
cdn_origin = "https://cdn.privacy-mgmt.com"
# auth_cookie_name = "sp_auth"
cache_ttl_seconds = 3600
```

**Endpoints:**

- `GET/POST /integrations/sourcepoint/cdn/*` - Sourcepoint CDN proxy

**When to use:** You load Sourcepoint CMP assets and want them to flow through first-party paths without introducing an open-ended proxy.

**Learn more:** [Sourcepoint Integration](./integrations/sourcepoint.md)

---

### Osano

**What it does:** Mirrors Osano's browser IAB API consent signals into standard first-party consent cookies for later Trusted Server requests.

**Key Features:**

- Mirrors USP, GPP, and TCF consent signals
- Writes `us_privacy`, `__gpp`, `__gpp_sid`, and `euconsent-v2`
- Marks owned cookies with `_ts_consent_src=osano`
- Preserves unmarked or foreign-owned consent cookies
- Clears stale Osano-owned cookies only after Osano readiness

**Configuration:**

```toml
[integrations.osano]
enabled = true
```

**Endpoints:** None. Osano v1 only enables the browser consent mirror module.

**Operational note:** Because mirroring runs in the browser after the page response starts, the first page request cannot include cookies written by the Osano mirror. Server-side consent behavior should rely on mirrored cookies from subsequent requests after Osano APIs are ready.

**When to use:** You use Osano for consent management and want Osano's browser consent signals available to Trusted Server as standard first-party cookies.

**Learn more:** [Osano Integration](./integrations/osano.md)

---

### GPT Runtime Diagnostics

**What it does:** Observes documented GPT lifecycle callbacks and presents directly observed slot, timing, coverage, binding, and visibility facts in a local browser console.

**Key Features:**

- Explicit browser-session `ts_console` activation through a host-only HttpOnly cookie
- Conditional standalone delivery only on active HTML documents
- Initial and refresh request-cycle history
- Conservative unmatched and ambiguous callback reporting
- Exact DOM binding and non-layout-changing viewport badges
- Versioned local JSON export with no diagnostic upload
- No creative-provenance or auction attribution claims

**Configuration:**

```toml
[integrations.gpt_diagnostics]
enabled = true
```

**Endpoints:** None. The feature observes GPT in the browser and makes no diagnostic network request.

**When to use:** You need to debug GPT request, response, render, load, viewability, refresh, and slot-binding behavior without changing ad delivery.

**Learn more:** [GPT Runtime Diagnostics](./integrations/gpt-diagnostics.md)

---

### Testlight

**What it does:** Testing/development integration for validating the integration system with OpenRTB-like auctions.

**Key Features:**

- EC ID injection demonstration
- Flexible JSON schema (preserves unknown fields)
- Stream passthrough mode
- Script replacement capability
- Validation with serde + validator

**Configuration:**

```toml
[integrations.testlight]
enabled = true
endpoint = "https://testlight-server.example.com"
timeout_ms = 1000
shim_src = "/static/tsjs-unified.js"
rewrite_scripts = false
```

**Endpoints:**

- `POST /integrations/testlight/auction` - Auction endpoint with ID injection

**When to use:** You're developing or testing integration functionality and need a simple endpoint to validate EC ID injection.

**Learn more:** [Testing Guide](./testing.md)

---

## Integration Architecture

All integrations use a consistent architecture:

### Route Namespacing

- Pattern: `/integrations/{integration_name}/{endpoint}`
- Examples:
  - `/integrations/permutive/api/settings`
  - `/integrations/testlight/auction`

### Configuration Pattern

All integrations support:

- TOML configuration in `trusted-server.toml`
- Environment variable overrides
- Enable/disable flags
- Validation at startup

### Rewriting System

Integrations can implement four types of rewriting:

1. **HTTP Proxying** - Route requests through first-party domain
2. **HTML Attribute Rewriting** - Modify element attributes during streaming
3. **Script Content Rewriting** - Transform inline script content
4. **Head Injection** - Insert HTML snippets at the start of `<head>`

## Choosing an Integration

Use this flowchart to determine which integrations you need:

```
Do you serve ads?
├─ Yes → Enable Prebid integration
└─ No → Skip Prebid

Do you use Next.js?
├─ Yes → Enable Next.js integration
└─ No → Skip Next.js

Do you use Permutive for audience data?
├─ Yes → Enable Permutive integration
└─ No → Skip Permutive

Do you use Sourcepoint for consent management?
├─ Yes → Enable Sourcepoint integration
└─ No → Skip Sourcepoint

Do you use Osano for consent management?
├─ Yes → Enable Osano integration
└─ No → Skip Osano

Are you developing/testing integrations?
├─ Yes → Enable Testlight integration
└─ No → Skip Testlight
```

## Performance Considerations

| Integration         | Performance Impact | Caching Strategy              | Notes                                           |
| ------------------- | ------------------ | ----------------------------- | ----------------------------------------------- |
| **Prebid**          | Medium             | Response caching possible     | Timeout configurable (default 1s)               |
| **Next.js**         | Low                | N/A (streaming rewrite)       | Minimal overhead, runs during HTML streaming    |
| **Permutive**       | Low                | SDK cached (1 hour default)   | API calls proxied in real-time                  |
| **Sourcepoint**     | Low                | CDN cached (1 hour default)   | JS rewriting adds minor overhead                |
| **GPT Diagnostics** | Low when active    | Static module publicly cached | Module omitted until browser-session activation |
| **Testlight**       | Low                | No caching                    | Development use only                            |

## Environment Variables

All integrations can be configured via environment variables:

```bash
# Pattern: TRUSTED_SERVER__INTEGRATIONS__{INTEGRATION}__{SETTING}

# Existing Prebid browser-map leaves
TRUSTED_SERVER__INTEGRATIONS__PREBID__TIMEOUT_MS=2000
TRUSTED_SERVER__INTEGRATIONS__PREBID__DEBUG=true

# Existing provider-map leaves use the validated provider ID segment
TRUSTED_SERVER__AUCTION__PROVIDERS__PBS_MAIN__ENDPOINT="https://prebid.example.com/openrtb2/auction"

# Next.js
TRUSTED_SERVER__INTEGRATIONS__NEXTJS__ENABLED=true

# Permutive
TRUSTED_SERVER__INTEGRATIONS__PERMUTIVE__ORGANIZATION_ID="neworg"
TRUSTED_SERVER__INTEGRATIONS__PERMUTIVE__WORKSPACE_ID="workspace-123"

# Sourcepoint
TRUSTED_SERVER__INTEGRATIONS__SOURCEPOINT__ENABLED=true
TRUSTED_SERVER__INTEGRATIONS__SOURCEPOINT__CDN_ORIGIN="https://cdn.privacy-mgmt.com"

# Testlight
TRUSTED_SERVER__INTEGRATIONS__TESTLIGHT__ENDPOINT="https://test.example.com"
```

See [Configuration Reference](./configuration.md) for complete details.

## Custom Integrations

You can create your own integrations by implementing the integration traits:

- `IntegrationProxy` - For HTTP endpoint proxying
- `IntegrationAttributeRewriter` - For HTML attribute rewriting
- `IntegrationScriptRewriter` - For script content transformation
- `IntegrationHeadInjector` - For injecting HTML snippets into `<head>`

See the [Integration Guide](./integration-guide.md) for details on building custom integrations.

## Common Questions

### Can I enable multiple integrations?

Yes! All integrations can run simultaneously. They operate independently and don't conflict.

### Do integrations affect page load time?

Minimal impact. HTML rewriting happens during streaming (Next.js), and proxy endpoints only execute when called. Prebid timeout is configurable.

### Can I disable integrations at runtime?

No. Integration configuration is read at startup. You must redeploy to change integration settings.

### Are integrations required?

No. All integrations are optional. You can run Trusted Server with no integrations enabled and use it purely for EC ID generation and first-party proxying.

### How do I add a new integration?

See the [Integration Guide](./integration-guide.md) for a complete tutorial on building custom integrations.

## Next Steps

- Learn about [Configuration](./configuration.md)
- Understand [Request Signing](./request-signing.md)
- Explore [Creative Processing](./creative-processing.md)
- Review [API Reference](./api-reference.md)
