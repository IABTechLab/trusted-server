# Next.js Integration

**Category**: Framework Support
**Status**: Production
**Type**: HTML/Script Rewriter

## Overview

The Next.js integration enables seamless integration between Trusted Server and Next.js applications by rewriting Next.js-specific data structures to route traffic through first-party proxying.

## What It Does

Next.js applications generate framework-specific JSON data (`__NEXT_DATA__`) and API routes that need special handling. This integration:

- Rewrites Next.js App Router (RSC) streaming responses
- Processes Pages Router static data payloads
- Modifies Next.js internal URLs to use first-party proxy
- Preserves React Server Component hydration

## Supported Next.js Versions

- **Next.js 13+**: App Router with React Server Components (RSC)
- **Next.js 12+**: Pages Router with static generation
- **Next.js 11**: Pages Router (basic support)

## Configuration

```toml
[integrations.nextjs]
enabled = false
rewrite_attributes = ["href", "link", "url"]
```

### Configuration Options

| Field                | Type    | Default                   | Description                           |
| -------------------- | ------- | ------------------------- | ------------------------------------- |
| `enabled`            | boolean | `false`                   | Enable Next.js integration            |
| `rewrite_attributes` | array   | `["href", "link", "url"]` | Attributes to rewrite in Next.js data |

## How It Works

### App Router (RSC Streaming)

Next.js 13+ App Router uses React Server Components with streaming:

```
┌─────────────────────────────────────────┐
│  Next.js App Router                     │
│  Streams: 0:{...} 1:{...} 2:{...}      │
│  ↓                                      │
│  Trusted Server Next.js Integration     │
│  ↓                                      │
│  Rewrites URLs in RSC payload           │
│  - /_next/image → /first-party/proxy   │
│  - /api/data → /first-party/proxy      │
│  ↓                                      │
│  Browser hydrates with first-party URLs │
└─────────────────────────────────────────┘
```

### Pages Router

Next.js Pages Router embeds data in `__NEXT_DATA__` script:

```html
<!-- Original -->
<script id="__NEXT_DATA__">
  {
    "props": {
      "pageProps": {
        "imageUrl": "https://cdn.example.com/image.png"
      }
    }
  }
</script>

<!-- After Next.js Integration -->
<script id="__NEXT_DATA__">
  {
    "props": {
      "pageProps": {
        "imageUrl": "/first-party/proxy?tsurl=https://cdn.example.com/image.png&tstoken=..."
      }
    }
  }
</script>
```

## Implementation Details

The Next.js integration is implemented across multiple files:

- [crates/common/src/integrations/nextjs/mod.rs](https://github.com/IABTechLab/trusted-server/blob/main/crates/common/src/integrations/nextjs/mod.rs) - Main integration
- [crates/common/src/integrations/nextjs/rsc.rs](https://github.com/IABTechLab/trusted-server/blob/main/crates/common/src/integrations/nextjs/rsc.rs) - RSC parsing
- [crates/common/src/integrations/nextjs/script_rewriter.rs](https://github.com/IABTechLab/trusted-server/blob/main/crates/common/src/integrations/nextjs/script_rewriter.rs) - Script rewriting

### Key Components

**Script Selector**: `script#__NEXT_DATA__`

Targets the Next.js data script for rewriting.

**Attribute Rewriting**:

- `href` - Link URLs
- `link` - Preload/prefetch URLs
- `url` - Image and asset URLs

**RSC Stream Processing**:

- Parses React Server Component streaming format
- Rewrites URLs in streaming chunks
- Preserves component structure

## Use Cases

### Next.js + Trusted Server

Run your Next.js application behind Trusted Server for:

- First-party asset loading
- Synthetic ID injection
- Privacy-compliant tracking
- Ad serving integration

### Static Site with Dynamic Ads

Use Next.js for content, Trusted Server for monetization.

### Hybrid Rendering

Combine Next.js SSR/SSG with Trusted Server edge logic.

## Best Practices

### 1. Enable Only When Needed

Only enable if you're using Next.js:

```toml
[integrations.nextjs]
enabled = true  # Only if using Next.js
```

### 2. Configure Rewrite Attributes

Add custom attributes if your Next.js app uses non-standard fields:

```toml
[integrations.nextjs]
rewrite_attributes = ["href", "link", "url", "customImageUrl"]
```

### 3. Test RSC Hydration

Verify React Server Components hydrate correctly:

```bash
# Check browser console for hydration errors
# Verify interactive elements work
```

### 4. Monitor Performance

Next.js integration adds minimal overhead (<10ms), but monitor:

- Time to First Byte (TTFB)
- First Contentful Paint (FCP)
- Largest Contentful Paint (LCP)

## Troubleshooting

### Hydration Mismatch Errors

**Symptoms**: React hydration errors in browser console

**Solutions**:

- Ensure rewrite attributes match your Next.js data structure
- Check URLs are properly signed
- Verify origin URLs are accessible

### Images Not Loading

**Symptoms**: Next.js `<Image>` components show broken images

**Solutions**:

- Verify `_next/image` URLs are proxied
- Check image domains are allowed
- Ensure proxy signatures are valid

### API Routes Failing

**Symptoms**: Next.js API routes return 404/500

**Solutions**:

- Check `/api/*` routes are not being over-proxied
- Verify Next.js origin URL is correct
- Review proxy exclusion rules

## Performance

### Overhead

- RSC parsing: ~5-10ms
- URL rewriting: ~2-5ms
- Total: <15ms per request

### Optimization

- Enable HTTP/2 for streaming
- Use CDN for static assets
- Cache `__NEXT_DATA__` when possible
- Minimize rewrite attributes

## Next Steps

- Review [First-Party Proxy](/guide/first-party-proxy) for URL rewriting
- Check [Creative Processing](/guide/creative-processing) for HTML processing
- Explore [Configuration](/guide/configuration) for setup details
- Learn about [Integrations Overview](/guide/integrations-overview)
