# Integrations Overview

Trusted Server provides built-in integrations with popular third-party services, enabling first-party data collection and privacy-preserving advertising. This page provides a comparison of all available integrations.

## Quick Comparison

| Integration   | Type             | Endpoints  | HTML Rewriting               | Primary Use Case            | Status      |
| ------------- | ---------------- | ---------- | ---------------------------- | --------------------------- | ----------- |
| **Prebid**    | Proxy + Rewriter | 2-3 routes | Removes Prebid.js scripts    | Server-side header bidding  | Production  |
| **Next.js**   | Script Rewriter  | None       | Rewrites Next.js data        | First-party Next.js routing | Production  |
| **Permutive** | Proxy + Rewriter | 6 routes   | Rewrites SDK URLs            | First-party audience data   | Production  |
| **Testlight** | Proxy + Rewriter | 1 route    | Rewrites integration scripts | Testing/development         | Development |

## Integration Details

### Prebid

**What it does:** Enables server-side header bidding through Prebid Server while maintaining first-party context.

**Key Features:**

- OpenRTB 2.x protocol conversion
- Synthetic ID injection for privacy
- First-party creative resource proxying
- CDN URL rewriting (7+ major SSPs)
- GPC signal support
- Request signing for authentication

**Configuration:**

```toml
[integrations.prebid]
enabled = true
server_url = "https://prebid-server.example.com"
timeout_ms = 1000
bidders = ["appnexus", "rubicon"]
auto_configure = true
debug = false
```

**Endpoints:**

- `GET /first-party/ad` - Server-side ad rendering
- `POST /third-party/ad` - Client-side auction endpoint
- `GET /prebid.js` - Optional empty script override

**When to use:** You want to monetize your site with programmatic advertising while maintaining privacy and first-party context.

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

**When to use:** You have a Next.js application and want to ensure all links and assets route through your first-party domain for better tracking and privacy.

**Learn more:** [Integration Guide](./integration-guide.md)

---

### Permutive

**What it does:** Provides first-party data collection and audience segmentation by proxying Permutive's SDK and API endpoints.

**Key Features:**

- Complete first-party SDK serving
- Multi-endpoint proxying (API, Events, Sync, Secure Signals, CDN)
- SDK caching for performance
- Privacy compliance (first-party cookies)
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
- `GET/POST /integrations/permutive/events/*` - Event tracking
- `GET/POST /integrations/permutive/sync/*` - ID synchronization
- `GET /integrations/permutive/cdn/*` - CDN proxy

**When to use:** You use Permutive for audience segmentation and want to maintain first-party data collection in a privacy-compliant way.

**Learn more:** [Integration Guide](./integration-guide.md)

---

### Testlight

**What it does:** Testing/development integration for validating the integration system with OpenRTB-like auctions.

**Key Features:**

- Synthetic ID injection demonstration
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

**When to use:** You're developing or testing integration functionality and need a simple endpoint to validate synthetic ID injection.

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

Are you developing/testing integrations?
├─ Yes → Enable Testlight integration
└─ No → Skip Testlight
```

## Performance Considerations

| Integration   | Performance Impact | Caching Strategy            | Notes                                        |
| ------------- | ------------------ | --------------------------- | -------------------------------------------- |
| **Prebid**    | Medium             | Response caching possible   | Timeout configurable (default 1s)            |
| **Next.js**   | Low                | N/A (streaming rewrite)     | Minimal overhead, runs during HTML streaming |
| **Permutive** | Low                | SDK cached (1 hour default) | API calls proxied in real-time               |
| **Testlight** | Low                | No caching                  | Development use only                         |

## Environment Variables

All integrations can be configured via environment variables:

```bash
# Pattern: TRUSTED_SERVER__INTEGRATIONS__{INTEGRATION}__{SETTING}

# Prebid
TRUSTED_SERVER__INTEGRATIONS__PREBID__SERVER_URL="https://new-server.com"
TRUSTED_SERVER__INTEGRATIONS__PREBID__TIMEOUT_MS=2000

# Next.js
TRUSTED_SERVER__INTEGRATIONS__NEXTJS__ENABLED=true

# Permutive
TRUSTED_SERVER__INTEGRATIONS__PERMUTIVE__ORGANIZATION_ID="neworg"
TRUSTED_SERVER__INTEGRATIONS__PERMUTIVE__WORKSPACE_ID="workspace-123"

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

No. All integrations are optional. You can run Trusted Server with no integrations enabled and use it purely for synthetic ID generation and first-party proxying.

### How do I add a new integration?

See the [Integration Guide](./integration-guide.md) for a complete tutorial on building custom integrations.

## Next Steps

- Learn about [Configuration](./configuration.md)
- Understand [Request Signing](./request-signing.md)
- Explore [Creative Processing](./creative-processing.md)
- Review [API Reference](./api-reference.md)
