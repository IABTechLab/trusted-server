# Creative Processing

Learn how Trusted Server automatically rewrites ad creative HTML and CSS to route all resources through first-party domains.

## Overview

Creative processing transforms third-party ad creatives by rewriting URLs to go through your first-party domain. This provides:

- **Privacy Control** - All resources load through your domain
- **First-Party Context** - Cookies and storage use your domain
- **Synthetic ID Integration** - Automatic ID forwarding to trackers
- **Security** - Validated, signed URLs prevent tampering
- **GDPR Compliance** - Controlled data sharing

## How It Works

```
┌──────────────────────────────────────────────────────┐
│  Original Creative HTML                              │
│  <img src="https://tracker.com/pixel.gif">          │
│  <iframe src="https://cdn.com/ad.html">             │
│  <style> .bg { background: url(cdn.com/bg.jpg); }   │
└──────────────────────────────────────────────────────┘
                        ↓
┌──────────────────────────────────────────────────────┐
│  Trusted Server Processing                           │
│  1. Parse HTML with streaming processor              │
│  2. Detect absolute/protocol-relative URLs           │
│  3. Generate signed proxy URLs                       │
│  4. Rewrite in-place                                 │
│  5. Inject TSJS library                              │
└──────────────────────────────────────────────────────┘
                        ↓
┌──────────────────────────────────────────────────────┐
│  Rewritten Creative HTML                             │
│  <img src="/first-party/proxy?tsurl=...&tstoken=sig">│
│  <iframe src="/first-party/proxy?tsurl=...&token=sig">│
│  <style> .bg { background: url(/first-party/proxy...) }│
└──────────────────────────────────────────────────────┘
```

## Processing Triggers

Creative processing is automatically triggered when:

1. **Content-Type Header**: Response is `text/html` or `text/css`
2. **Proxy Mode**: Request goes through `/first-party/proxy` (not streaming)
3. **Integration Response**: Integration returns HTML content

::: info Streaming Mode
When `with_streaming()` is enabled in `ProxyRequestConfig`, HTML/CSS processing is **skipped** to preserve origin compression and reduce latency. Use for binary files, large responses, or when rewriting isn't needed.
:::

## Rewritten Elements

### Images

**Elements**: `<img>`, `<input type="image">`

**Attributes**:
- `src` - Primary image source
- `data-src` - Lazy-loading source
- `srcset` - Responsive image sources
- `imagesrcset` - Image set (used in `<link>`)

**Example**:
```html
<!-- Original -->
<img src="https://cdn.example.com/banner.jpg" 
     srcset="https://cdn.example.com/banner@1x.jpg 1x,
             https://cdn.example.com/banner@2x.jpg 2x">

<!-- Rewritten -->
<img src="/first-party/proxy?tsurl=https://cdn.example.com/banner.jpg&tstoken=sig1"
     srcset="/first-party/proxy?tsurl=https://cdn.example.com/banner@1x.jpg&tstoken=sig2 1x,
             /first-party/proxy?tsurl=https://cdn.example.com/banner@2x.jpg&tstoken=sig3 2x">
```

### Scripts

**Elements**: `<script>`

**Attributes**:
- `src` - Script source URL

**Example**:
```html
<!-- Original -->
<script src="https://cdn.example.com/tracker.js"></script>

<!-- Rewritten -->
<script src="/first-party/proxy?tsurl=https://cdn.example.com/tracker.js&tstoken=sig"></script>
```

### Media Elements

**Elements**: `<video>`, `<audio>`, `<source>`

**Attributes**:
- `src` - Media source URL

**Example**:
```html
<!-- Original -->
<video>
  <source src="https://cdn.example.com/video.mp4" type="video/mp4">
</video>

<!-- Rewritten -->
<video>
  <source src="/first-party/proxy?tsurl=https://cdn.example.com/video.mp4&tstoken=sig" type="video/mp4">
</video>
```

### Embedded Objects

**Elements**: `<object>`, `<embed>`

**Attributes**:
- `data` - Object data source (`<object>`)
- `src` - Embed source (`<embed>`)

**Example**:
```html
<!-- Original -->
<object data="https://example.com/flash.swf"></object>
<embed src="https://example.com/media.swf">

<!-- Rewritten -->
<object data="/first-party/proxy?tsurl=https://example.com/flash.swf&tstoken=sig"></object>
<embed src="/first-party/proxy?tsurl=https://example.com/media.swf&tstoken=sig">
```

### Iframes

**Elements**: `<iframe>`

**Attributes**:
- `src` - Frame source URL

**Example**:
```html
<!-- Original -->
<iframe src="https://advertiser.com/creative.html"></iframe>

<!-- Rewritten -->
<iframe src="/first-party/proxy?tsurl=https://advertiser.com/creative.html&tstoken=sig"></iframe>
```

::: warning Nested Iframes
If the iframe content itself contains HTML, it will be processed recursively. Each level of nesting gets its own URL rewriting pass.
:::

### Links

**Elements**: `<link>`

**Attributes**:
- `href` - Link target (stylesheets, preload, prefetch)
- `imagesrcset` - Responsive images in link preload

**Conditions**: Only rewritten when `rel` attribute matches:
- `stylesheet`
- `preload`
- `prefetch`

**Example**:
```html
<!-- Original -->
<link rel="stylesheet" href="https://cdn.example.com/styles.css">
<link rel="preload" href="https://cdn.example.com/font.woff2" as="font">

<!-- Rewritten -->
<link rel="stylesheet" href="/first-party/proxy?tsurl=https://cdn.example.com/styles.css&tstoken=sig">
<link rel="preload" href="/first-party/proxy?tsurl=https://cdn.example.com/font.woff2&tstoken=sig" as="font">
```

### Anchors (Click Tracking)

**Elements**: `<a>`, `<area>`

**Attributes**:
- `href` - Link destination

**Rewrite Mode**: Uses `/first-party/click` for direct redirects

**Example**:
```html
<!-- Original -->
<a href="https://advertiser.com/product?id=123">Buy Now</a>

<!-- Rewritten -->
<a href="/first-party/click?tsurl=https://advertiser.com/product&id=123&tstoken=sig">Buy Now</a>
```

::: tip Click vs Proxy
Anchors (`<a>`) use `/first-party/click` for 302 redirects, avoiding content downloads. Other elements use `/first-party/proxy` to fetch and potentially rewrite content.
:::

### SVG Elements

**Elements**: SVG `<image>`, `<use>`

**Attributes**:
- `href` - SVG 2.0 syntax
- `xlink:href` - SVG 1.1 legacy syntax

**Example**:
```html
<!-- Original -->
<svg>
  <image href="https://cdn.example.com/icon.svg" />
  <use xlink:href="https://cdn.example.com/sprite.svg#icon" />
</svg>

<!-- Rewritten -->
<svg>
  <image href="/first-party/proxy?tsurl=https://cdn.example.com/icon.svg&tstoken=sig" />
  <use xlink:href="/first-party/proxy?tsurl=https://cdn.example.com/sprite.svg&tstoken=sig#icon" />
</svg>
```

### Inline Styles

**Attributes**: `style` attribute on any element

**Patterns**: Rewrites `url(...)` values in CSS

**Example**:
```html
<!-- Original -->
<div style="background: url(https://cdn.example.com/bg.png) no-repeat;">

<!-- Rewritten -->
<div style="background: url(/first-party/proxy?tsurl=https://cdn.example.com/bg.png&tstoken=sig) no-repeat;">
```

### Style Blocks

**Elements**: `<style>`

**Patterns**: Rewrites all `url(...)` occurrences in CSS

**Example**:
```html
<!-- Original -->
<style>
  .header { background-image: url(https://cdn.example.com/header.jpg); }
  .logo { background: url('https://cdn.example.com/logo.png'); }
</style>

<!-- Rewritten -->
<style>
  .header { background-image: url(/first-party/proxy?tsurl=https://cdn.example.com/header.jpg&tstoken=sig); }
  .logo { background: url('/first-party/proxy?tsurl=https://cdn.example.com/logo.png&tstoken=sig'); }
</style>
```

## URL Detection Rules

### Absolute URLs

**Pattern**: Starts with `http://` or `https://`

**Rewritten**: ✅ Yes

**Examples**:
```
✅ https://cdn.example.com/image.png
✅ http://tracker.example.com/pixel.gif
❌ /relative/path.jpg (relative)
❌ ../images/logo.png (relative)
```

### Protocol-Relative URLs

**Pattern**: Starts with `//`

**Rewritten**: ✅ Yes (normalized to `https://`)

**Examples**:
```
Original: //cdn.example.com/script.js
Normalized: https://cdn.example.com/script.js
Rewritten: /first-party/proxy?tsurl=https://cdn.example.com/script.js&tstoken=sig
```

### Relative URLs

**Patterns**:
- Starts with `/` (absolute path)
- Starts with `./` or `../` (relative path)
- No scheme prefix (relative)

**Rewritten**: ❌ No

**Examples**:
```
❌ /assets/image.png
❌ ./local.jpg
❌ ../parent/file.css
❌ image.png
```

::: info Why Skip Relative URLs?
Relative URLs already point to your domain (publisher origin). Rewriting them would create unnecessary proxy loops and break functionality.
:::

### Non-Network Schemes

**Skipped Schemes**:
- `data:` - Data URIs (inline content)
- `javascript:` - JavaScript execution
- `mailto:` - Email links
- `tel:` - Phone numbers
- `blob:` - Blob objects
- `about:` - Browser internal pages

**Rewritten**: ❌ No

**Examples**:
```
❌ data:image/png;base64,iVBORw0KGgo...
❌ javascript:void(0)
❌ mailto:contact@example.com
❌ tel:+1234567890
❌ blob:https://example.com/uuid
❌ about:blank
```

## Srcset Processing

### Srcset Syntax

Srcset attributes contain comma-separated candidates with optional descriptors:

**Format**: `url descriptor, url descriptor, ...`

**Descriptors**:
- `1x`, `2x`, `3x` - Pixel density
- `100w`, `200w` - Width in pixels

### Parsing Rules

**Robust Comma Handling**:
- Splits on commas with or without spaces
- Preserves `data:` URIs (doesn't split on internal commas)
- Handles irregular spacing

**Example**:
```html
<!-- Various spacing patterns -->
srcset="url1.jpg 1x, url2.jpg 2x"  <!-- standard -->
srcset="url1.jpg 1x,url2.jpg 2x"   <!-- no space after comma -->
srcset="url1.jpg   1x  ,  url2.jpg  2x"  <!-- extra spaces -->
```

### Descriptor Preservation

Descriptors are preserved exactly as written:

```html
<!-- Original -->
<img srcset="
  https://cdn.com/small.jpg 480w,
  https://cdn.com/medium.jpg 800w,
  https://cdn.com/large.jpg 1200w
">

<!-- Rewritten -->
<img srcset="
  /first-party/proxy?tsurl=https://cdn.com/small.jpg&tstoken=sig1 480w,
  /first-party/proxy?tsurl=https://cdn.com/medium.jpg&tstoken=sig2 800w,
  /first-party/proxy?tsurl=https://cdn.com/large.jpg&tstoken=sig3 1200w
">
```

### Mixed URL Types

Srcset can mix absolute and relative URLs:

```html
<!-- Original -->
<img srcset="
  /local/small.jpg 1x,
  https://cdn.com/large.jpg 2x
">

<!-- Rewritten (only absolute URL) -->
<img srcset="
  /local/small.jpg 1x,
  /first-party/proxy?tsurl=https://cdn.com/large.jpg&tstoken=sig 2x
">
```

## CSS URL Rewriting

### url() Syntax Variations

CSS `url()` values support multiple quote styles:

```css
/* No quotes */
url(https://example.com/image.png)

/* Single quotes */
url('https://example.com/image.png')

/* Double quotes */
url("https://example.com/image.png")

/* With spaces */
url(  "https://example.com/image.png"  )
```

**All are rewritten correctly**, preserving the original quote style.

### CSS Properties

Common properties with `url()` values:

```css
/* Background images */
background: url(https://cdn.com/bg.jpg);
background-image: url(https://cdn.com/pattern.png);

/* Borders */
border-image: url(https://cdn.com/border.svg);

/* List styles */
list-style-image: url(https://cdn.com/bullet.png);

/* Cursors */
cursor: url(https://cdn.com/cursor.cur), pointer;

/* Masks */
mask-image: url(https://cdn.com/mask.svg);

/* Filters */
filter: url(https://cdn.com/filter.svg#blur);
```

**All `url()` occurrences are rewritten** regardless of property.

### Multiple url() Values

Properties can have multiple `url()` values:

```css
/* Original */
.element {
  background: 
    url(https://cdn.com/top.png) top,
    url(https://cdn.com/bottom.png) bottom;
}

/* Rewritten */
.element {
  background: 
    url(/first-party/proxy?tsurl=https://cdn.com/top.png&tstoken=sig1) top,
    url(/first-party/proxy?tsurl=https://cdn.com/bottom.png&tstoken=sig2) bottom;
}
```

### @import Rules

CSS `@import` with URLs:

```css
/* Original */
@import url(https://fonts.googleapis.com/css?family=Roboto);

/* Rewritten */
@import url(/first-party/proxy?tsurl=https://fonts.googleapis.com/css?family=Roboto&tstoken=sig);
```

## Exclude Domains

### Configuration

Prevent specific domains from being rewritten:

```toml
[rewrite]
exclude_domains = [
  "*.cdn.trusted-partner.com",  # Wildcard pattern
  "first-party.example.com",    # Exact match
  "localhost",                  # Development
]
```

### Pattern Matching

**Wildcard Patterns**: `*` matches any subdomain

```
Pattern: *.cdn.example.com
Matches:
  ✅ assets.cdn.example.com
  ✅ images.cdn.example.com
  ❌ cdn.example.com (no subdomain)
  ❌ cdn.example.com.evil.com (different domain)
```

**Exact Patterns**: No `*` requires exact host match

```
Pattern: api.example.com
Matches:
  ✅ api.example.com
  ❌ www.api.example.com
  ❌ api.example.com.evil.com
```

### Use Cases

**Trusted Partners**:
```toml
exclude_domains = ["*.trusted-cdn.com"]
```
Skip rewriting for partners already providing first-party scripts.

**Development**:
```toml
exclude_domains = ["localhost", "127.0.0.1"]
```
Avoid proxying local development servers.

**Same-Origin Resources**:
```toml
exclude_domains = ["assets.publisher.com"]
```
Skip resources already on your domain.

## Integration Hooks

### Attribute Rewriters

Integrations can override attribute rewriting:

**Example**: Next.js integration rewrites origin URLs

```rust
impl IntegrationAttributeRewriter for NextJsIntegration {
    fn rewrite(&self, attr_name: &str, attr_value: &str, ctx: &Context) 
        -> AttributeRewriteAction 
    {
        if attr_name == "href" && attr_value.contains(&ctx.origin_host) {
            let rewritten = attr_value.replace(&ctx.origin_host, &ctx.request_host);
            return AttributeRewriteAction::replace(rewritten);
        }
        AttributeRewriteAction::keep()
    }
}
```

**Actions**:
- `keep()` - Leave attribute unchanged
- `replace(value)` - Change attribute value
- `remove_element()` - Delete entire element

### Script Rewriters

Integrations can modify `<script>` content:

**Example**: Next.js rewrites `__NEXT_DATA__` JSON

```rust
impl IntegrationScriptRewriter for NextJsIntegration {
    fn selector(&self) -> &'static str {
        "script#__NEXT_DATA__"
    }
    
    fn rewrite(&self, content: &str, ctx: &Context) -> ScriptRewriteAction {
        let rewritten = rewrite_next_data_urls(content, ctx);
        ScriptRewriteAction::replace(rewritten)
    }
}
```

**Actions**:
- `keep()` - Leave script unchanged
- `replace(content)` - Replace script content
- `remove_node()` - Delete script element

See [Integration Guide](/guide/integration-guide) for creating custom rewriters.

## TSJS Injection

### Automatic Injection

The Trusted Server JavaScript (TSJS) library is automatically injected:

**Location**: Start of `<head>` element

**Tag**:
```html
<script async src="/static/tsjs-core.min.js" data-tsjs-integration="core"></script>
```

**Timing**: Injected **once per HTML response** before any other scripts.

### Integration Bundles

Integrations can request additional bundles:

```rust
IntegrationRegistration::builder("my_integration")
    .with_asset("my_integration")  // Requests tsjs-my_integration.min.js
    .build()
```

**Result**:
```html
<head>
  <script async src="/static/tsjs-core.min.js" data-tsjs-integration="core"></script>
  <script async src="/static/tsjs-my_integration.min.js" data-tsjs-integration="my_integration"></script>
  <!-- Rest of head content -->
</head>
```

### Bundle Types

Available bundles (from `crates/js/lib/src/integrations/`):

- `tsjs-core.min.js` - Core API (always included)
- `tsjs-ext.min.js` - Extensions (Prebid integration)
- `tsjs-creative.min.js` - Creative tracking utilities
- `tsjs-permutive.min.js` - Permutive integration
- `tsjs-testlight.min.js` - Testlight integration

## Performance Optimization

### Streaming Processing

HTML is processed in **chunks** (default 8192 bytes):

**Benefits**:
- Low memory footprint
- Handles large creatives
- Incremental output
- Fast first byte

**Trade-offs**:
- Cannot access full DOM
- Element-by-element processing
- No look-ahead

### Compression

**Buffered Mode** (with rewriting):
```
Origin Response (gzipped)
  ↓ Decompress
Processing
  ↓ Rewrite URLs
Response (uncompressed)
  ↓ Fastly edge can re-compress
Client
```

**Streaming Mode** (no rewriting):
```
Origin Response (gzipped)
  ↓ Passthrough
Client (stays gzipped)
```

Use streaming for binary/large files to preserve compression.

### Caching

Rewritten creatives can be cached:

**Cache Key Components**:
- Original creative URL
- Publisher domain
- Integration configuration

**Headers to Set**:
```http
Cache-Control: public, max-age=3600
Vary: Accept-Encoding
```

## Debugging

### Logging

Enable debug logging for rewrite operations:

```rust
log::debug!("creative: rewriting {} -> {}", original_url, proxy_url);
log::debug!("creative: excluded domain {}", url);
log::debug!("creative: skipped non-network scheme {}", url);
```

### Testing Rewrites

**Manual Testing**:
1. Save original creative HTML to file
2. Pass through rewrite function
3. Compare output

```rust
let original = "<img src=\"https://tracker.com/pixel.gif\">";
let rewritten = rewrite_creative_html(original, &settings);
assert!(rewritten.contains("/first-party/proxy"));
```

**Integration Tests**:
```rust
#[test]
fn test_image_src_rewrite() {
    let html = r#"<img src="https://cdn.example.com/banner.jpg">"#;
    let result = rewrite_creative_html(html, &test_settings());
    assert!(result.contains("/first-party/proxy?tsurl="));
    assert!(result.contains("&tstoken="));
}
```

### Common Issues

**Relative URLs Not Working**:
- Ensure origin response includes proper `<base>` tag
- Or convert to absolute URLs before rewriting

**Data URIs Being Rewritten**:
- Should be automatically skipped
- Check for malformed `data:` scheme

**Srcset Parsing Errors**:
- Verify comma-separated format
- Check for unclosed quotes

## Security Considerations

### URL Validation

All rewritten URLs are validated:

1. **Scheme Check**: Only `http://` and `https://`
2. **Signature**: HMAC-SHA256 token required
3. **Expiration**: Optional `tsexp` timestamp
4. **Exclusion List**: Configurable domain blacklist

### Content Security Policy

Recommended CSP headers for rewritten creatives:

```http
Content-Security-Policy: 
  default-src 'self';
  img-src 'self' /first-party/proxy;
  script-src 'self' /static/tsjs;
  style-src 'self' 'unsafe-inline';
  frame-src 'self' /first-party/proxy;
```

### Injection Prevention

**Automatic Protection**:
- URLs are properly encoded
- No raw user input in rewrites
- Signature prevents tampering

**Manual Checks**:
- Validate origin creative sources
- Sanitize user-generated content
- Monitor for suspicious patterns

## Best Practices

### Configuration

✅ **Do**:
- Use strong `proxy_secret` (32+ bytes random)
- Exclude trusted first-party domains
- Set appropriate cache headers
- Test rewrites before production

❌ **Don't**:
- Hardcode secrets in source
- Rewrite same-origin URLs unnecessarily
- Skip signature validation
- Disable TSJS injection without reason

### Performance

✅ **Do**:
- Use streaming for large/binary responses
- Enable compression at edge
- Cache rewritten creatives
- Monitor rewrite latency

❌ **Don't**:
- Buffer entire response unnecessarily
- Rewrite on every request (cache!)
- Process non-HTML/CSS with rewriter
- Chain multiple rewrites

### Monitoring

Track these metrics:

- **Rewrite operations** - Count of rewrites per request
- **Excluded domains** - Frequency of exclusions
- **Processing time** - Latency added by rewriting
- **Cache hit rate** - Effectiveness of caching
- **TSJS injection** - Verify library loads

## Next Steps

- Learn about [First-Party Proxy](/guide/first-party-proxy) for URL handling
- Review [Integration Guide](/guide/integration-guide) for custom rewriters
- Set up [Configuration](/guide/configuration) for your creatives
- Explore [Synthetic IDs](/guide/synthetic-ids) for identity management
