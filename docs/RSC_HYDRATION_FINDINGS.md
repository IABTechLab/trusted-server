# RSC Hydration URL Rewriting: Technical Findings

## Problem Statement

When proxying Next.js App Router sites, URL rewriting in RSC (React Server Components) payloads caused React hydration to fail. The symptom was 0 React fiber nodes after page load, indicating complete hydration failure.

## Background: How Next.js Delivers RSC Data

Next.js App Router uses React Server Components with a streaming "flight" protocol. RSC data is delivered to the browser via inline `<script>` tags that call `self.__next_f.push()`:

```html
<script>self.__next_f.push([0])</script>
<script>self.__next_f.push([1,"0:[[...RSC content...]"])</script>
<script>self.__next_f.push([1,"1a:T29,{\"url\":\"https://origin.example.com\"}"])</script>
```

The `[1, "..."]` calls contain the actual RSC payload as a JavaScript string.

For client-side navigations, Next.js fetches Flight directly (no `<script>` wrapper) and expects `content-type: text/x-component`. The response body is the same row format, but without JSON/JS-string escaping (trusted-server rewrites these via `crates/common/src/rsc_flight.rs`).

## RSC Flight Protocol Format

The Flight stream is framed as **rows**. Most rows are delimited by `\n` (literal backslash-n in the JS strings inside `__next_f.push([1,"..."])`), but **`T` rows are length-delimited and do not end with a newline**.

| Record Type | Format | Example |
|-------------|--------|---------|
| T-chunk (text) | `ID:T<hex_length>,<content>` | `1a:T29,{"url":"..."}` |
| JSON array | `ID:[...]` | `0:[["page",null,...]]` |
| JSON object | `ID:{...}` | `5:{"name":"value"}` |
| Module import | `ID:I[...]` | `2:I["chunk-id",...]` |
| Head link | `ID:HL[...]` | `3:HL["/_next/static/..."]` |
| Reference | `ID:$ref` | `4:$L5` |
| String | `ID:"..."` | `6:"hello"` |
| Null | `ID:null` | `7:null` |

### Authoritative Parsing Rules (from `react-server-dom-webpack`)

Next.js uses the `react-server-dom-webpack` client parser, which frames the stream as:

1. Read hex `ID` until `:`
2. Read one byte to determine framing:
   - `T` (and sometimes `V` in newer builds): read hex length until `,`, then read **exactly N raw bytes** (no newline)
   - `A`–`Z`: read until `\n`
   - anything else (`[`, `{`, `"`, `t`, `f`, `n`, digits…): treat as JSON row content and read until `\n`

This is the key reason URL rewriting must be **T-row-aware**: changing bytes inside a `T` row requires updating its hex length prefix.

## The Critical T-Chunk Format

T-chunks are the most important record type for URL rewriting. They contain text data with an **explicit byte length**:

```
1a:T29,{"url":"https://origin.example.com/path"}
│  │ │  └─ Content (exactly 41 unescaped bytes)
│  │ └─ Comma separator
│  └─ Length in hex (0x29 = 41 bytes)
└─ Chunk ID
```

**Critical detail**: The hex length is the **UNESCAPED byte count**. The RSC content is embedded in a JavaScript string, so escape sequences must be counted correctly:

| Escape Sequence | String Chars | Unescaped Bytes |
|-----------------|--------------|-----------------|
| `\n` | 2 | 1 |
| `\r` | 2 | 1 |
| `\t` | 2 | 1 |
| `\\` | 2 | 1 |
| `\"` | 2 | 1 |
| `\xHH` | 4 | 1 |
| `\uHHHH` | 6 | 1-3 (UTF-8 bytes) |
| `\uD800\uDC00` | 12 | 4 (surrogate pair) |

## Initial Approach: Whitespace Padding (FAILED)

The initial hypothesis was to preserve byte length by adding JSON whitespace after URLs:

```
Original:  "href":"https://origin.example.com/news"
Replaced:  "href":"http://proxy.example.com/news"  ← 2 spaces after quote
```

**Why it failed**: This approach could not handle T-chunks correctly. The T-chunk length prefix declares the exact byte count, and whitespace padding doesn't update this prefix. When React's RSC parser reads a T-chunk, it reads exactly the declared number of bytes, then expects the next record. If the actual content has different length, parsing corrupts.

---

## Discovery 1: T-Chunks Can Span Multiple Push Scripts

A critical discovery was that T-chunk content can span multiple `self.__next_f.push()` calls:

```html
<!-- Script 10: T-chunk HEADER only -->
<script>self.__next_f.push([1,"11:null\n1a:T928,"])</script>

<!-- Script 11: T-chunk CONTENT (the 2344 unescaped bytes) -->
<script>self.__next_f.push([1,"...2344 bytes of actual content..."])</script>
```

This happens because Next.js streams RSC data as it becomes available. The T-chunk header in script 10 declares 928 bytes (0x928 = 2344 decimal), but those bytes are delivered in script 11.

### Real-World Example

Analysis of a Next.js App Router site revealed the following cross-script pattern:

```
Script 59 (index 58):
- T-chunk header at position 1370: "436:T68f,"
- Declares 0x68f = 1679 bytes of content
- Content starts but script ends before all bytes are delivered

Script 60 (index 59):
- Contains continuation of T-chunk content
- Includes 5 URLs pointing to the origin host that need rewriting
- URLs at byte positions within the T-chunk span
```

When the Rust implementation processed each script independently:
- Script 59: T-chunk header found, but `content_end = header_end` (0 bytes in THIS script)
- Script 60: Content processed, but no T-chunk header to update

Result: T-chunk length remained at 0x68f while actual content changed size after URL rewriting.

## Discovery 2: Combining Push Calls Breaks Hydration

```javascript
// Original: 221 push calls -> 683 fibers (works)
// Combined into 1 push call: 0 fibers (broken!)
```

Even with identical content, consolidating all RSC into a single push call broke hydration. Next.js processes each push call incrementally, and the structure matters.

## Discovery 3: Per-Script Streaming Processing Cannot Fix Cross-Script T-Chunks

The streaming HTML processor (`lol_html`) processes scripts one at a time:

```
┌─────────────────────────────────────────────────────────────────┐
│ HTML Stream                                                      │
│                                                                  │
│  <script>A</script>  <script>B</script>  <script>C</script>     │
│       │                    │                    │                │
│       ▼                    ▼                    ▼                │
│   Process A            Process B            Process C            │
│   (isolated)           (isolated)           (isolated)           │
│                                                                  │
│  Cannot share state between script processing!                   │
└─────────────────────────────────────────────────────────────────┘
```

This is a fundamental limitation: when script A declares a T-chunk that continues in script B, the streaming processor cannot:
1. Track that script A's T-chunk is incomplete
2. Update script A's header after processing script B's URLs

---

## The Solution: Two-Phase Processing

### Phase 1: Streaming HTML Processing (per-script)

The streaming processor handles scripts that are self-contained:
- Extracts RSC payload from `self.__next_f.push([1, '...'])`
- Finds T-chunks within the single script
- Rewrites URLs and recalculates lengths
- Works correctly for ~95% of scripts

### Phase 2: Post-Processing (cross-script)

After streaming completes, a post-processor handles cross-script T-chunks:
1. **Finds all RSC push scripts** in the complete HTML
2. **Combines their payloads** with markers
3. **Processes T-chunks across the combined content**, skipping markers when counting bytes
4. **Rewrites URLs and recalculates lengths** for the combined content
5. **Splits back on markers** to get individual rewritten payloads
6. **Rebuilds the HTML** with rewritten scripts

### Marker-Based Cross-Script Processing

#### Step 1: Combine Scripts with Markers

```rust
const RSC_MARKER: &str = "\x00SPLIT\x00";

// Combine all payloads
let mut combined = payloads[0].to_string();
for payload in &payloads[1..] {
    combined.push_str(RSC_MARKER);
    combined.push_str(payload);
}
// Result: "11:null\n1a:T928,\x00SPLIT\x00...2344 bytes..."
```

The marker `\x00SPLIT\x00` is chosen because:
- Contains null byte (`\x00`) which cannot appear in valid JSON/RSC content
- Easily identifiable for splitting
- Won't be confused with any escape sequence

#### Step 2: Find T-Chunks Across Combined Content

```rust
fn find_tchunks_with_markers(content: &str) -> Vec<MarkedTChunkInfo> {
    let pattern = Regex::new(r"([0-9a-fA-F]+):T([0-9a-fA-F]+),").unwrap();

    for each match:
        // Parse header: id, hex_length
        // Consume declared bytes, SKIPPING markers
        let (content_end, _) = consume_unescaped_bytes_skip_markers(
            content, header_end, declared_length
        );
}
```

The key insight: markers don't count toward byte consumption. When a T-chunk declares 1679 bytes, we consume 1679 bytes of actual content, skipping over any markers we encounter.

#### Step 3: Rewrite URLs and Recalculate Lengths

```rust
for chunk in &chunks {
    // Extract T-chunk content (may contain markers)
    let chunk_content = &combined[chunk.header_end..chunk.content_end];

    // Rewrite URLs (preserves markers)
    let rewritten_content = rewrite_rsc_url_string(chunk_content, ...);

    // Calculate new byte length (excluding markers)
    let new_length = calculate_unescaped_byte_length_skip_markers(&rewritten_content);
    let new_length_hex = format!("{:x}", new_length);

    // Write new T-chunk header and content
    result.push_str(&chunk.id);
    result.push_str(":T");
    result.push_str(&new_length_hex);
    result.push(',');
    result.push_str(&rewritten_content);
}
```

#### Step 4: Split Back on Markers

```rust
// Split on markers to get individual payloads
result.split(RSC_MARKER).map(|s| s.to_string()).collect()
```

Each resulting payload corresponds to one original script, but with:
- URLs rewritten
- T-chunk lengths correctly recalculated across script boundaries

---

## Integration Hook Architecture

The post-processing is implemented as an integration hook, allowing other integrations to also perform HTML post-processing.

### Trait Definition

```rust
/// Context for HTML post-processors.
pub struct IntegrationHtmlContext<'a> {
    pub request_host: &'a str,
    pub request_scheme: &'a str,
    pub origin_host: &'a str,
}

/// Trait for integration-provided HTML post-processors.
/// These run after streaming HTML processing to handle cases that require
/// access to the complete HTML (e.g., cross-script RSC T-chunks).
pub trait IntegrationHtmlPostProcessor: Send + Sync {
    /// Identifier for logging/diagnostics.
    fn integration_id(&self) -> &'static str;

    /// Post-process complete HTML content.
    fn post_process(&self, html: &str, ctx: &IntegrationHtmlContext<'_>) -> String;
}
```

### Registration

```rust
// In nextjs.rs
pub fn register(settings: &Settings) -> Option<IntegrationRegistration> {
    let config = build(settings)?;

    let structured = Arc::new(NextJsScriptRewriter::new(config.clone(), NextJsRewriteMode::Structured));
    let streamed = Arc::new(NextJsScriptRewriter::new(config.clone(), NextJsRewriteMode::Streamed));
    let post_processor = Arc::new(NextJsHtmlPostProcessor::new(config));

    Some(
        IntegrationRegistration::builder(NEXTJS_INTEGRATION_ID)
            .with_script_rewriter(structured)
            .with_script_rewriter(streamed)
            .with_html_post_processor(post_processor)  // <-- Post-processor hook
            .build(),
    )
}
```

### Execution in Publisher

```rust
// In publisher.rs - process_response_streaming()

// Phase 1: Streaming HTML processing
let mut pipeline = StreamingPipeline::new(config, processor);
pipeline.process(body, &mut output)?;

// Phase 2: Post-processing via integration hooks
let post_processors = params.integration_registry.html_post_processors();
if !post_processors.is_empty() {
    if let Ok(html) = std::str::from_utf8(&output) {
        let ctx = IntegrationHtmlContext {
            request_host: params.request_host,
            request_scheme: params.request_scheme,
            origin_host: params.origin_host,
        };
        let mut processed = html.to_string();
        for processor in post_processors {
            processed = processor.post_process(&processed, &ctx);
        }
        output = processed.into_bytes();
    }
}
```

---

## Byte Length Calculation Algorithm

To correctly calculate unescaped byte length:

```rust
fn calculate_unescaped_byte_length(s: &str) -> usize {
    let bytes = s.as_bytes();
    let mut result = 0;
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            let esc = bytes[i + 1];

            // Simple escape sequences: \n, \r, \t, \b, \f, \v, \", \', \\, \/
            if matches!(esc, b'n' | b'r' | b't' | b'b' | b'f' | b'v' | b'"' | b'\'' | b'\\' | b'/') {
                result += 1;
                i += 2;
                continue;
            }

            // \xHH - hex escape (1 byte)
            if esc == b'x' && i + 3 < bytes.len() {
                result += 1;
                i += 4;
                continue;
            }

            // \uHHHH - unicode escape
            if esc == b'u' && i + 5 < bytes.len() {
                let hex = &s[i + 2..i + 6];
                if let Ok(code_unit) = u16::from_str_radix(hex, 16) {
                    // Check for surrogate pair
                    if is_high_surrogate(code_unit) && has_low_surrogate_at(s, i + 6) {
                        result += 4;  // Surrogate pair = 4 UTF-8 bytes
                        i += 12;
                        continue;
                    }
                    // Single unicode escape - calculate UTF-8 byte length
                    let c = char::from_u32(code_unit as u32).unwrap_or('\u{FFFD}');
                    result += c.len_utf8();
                    i += 6;
                    continue;
                }
            }
        }

        // Regular character - count its UTF-8 byte length
        if bytes[i] < 0x80 {
            result += 1;
            i += 1;
        } else {
            let c = s[i..].chars().next().unwrap_or('\u{FFFD}');
            result += c.len_utf8();
            i += c.len_utf8();
        }
    }

    result
}
```

### Marker-Aware Variant

```rust
fn calculate_unescaped_byte_length_skip_markers(s: &str) -> usize {
    let without_markers = s.replace(RSC_MARKER, "");
    calculate_unescaped_byte_length(&without_markers)
}
```

---

## URL Rewriting Patterns

The solution handles multiple URL formats in RSC content:

| Pattern | Example | In RSC String |
|---------|---------|---------------|
| Full HTTPS | `https://host/path` | `https://host/path` |
| Full HTTP | `http://host/path` | `http://host/path` |
| Protocol-relative | `//host/path` | `//host/path` |
| JSON-escaped slashes | `//host/path` | `\\/\\/host/path` |
| Double-escaped | `\\/\\/host` | `\\\\/\\\\/host` |
| Quad-escaped | `\\\\/\\\\/host` | `\\\\\\\\//host` |

### Regex Pattern

```rust
let pattern = Regex::new(&format!(
    r#"(https?)?(:)?(\\\\\\\\\\\\\\\\//|\\\\\\\\//|\\/\\/|//){}"#,
    escaped_origin
)).unwrap();
```

This pattern handles:
- Optional scheme (`https?`)?
- Optional colon (`:`)?
- Multiple escape levels for slashes
- The escaped origin hostname

---

## Complete Processing Flow

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         HTML Response from Origin                            │
└─────────────────────────────────────────────────────────────────────────────┘
                                      │
                                      ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                    PHASE 1: Streaming HTML Processing                        │
│                                                                              │
│  ┌──────────────────┐  ┌──────────────────┐  ┌──────────────────┐          │
│  │ Process Script 1 │  │ Process Script 2 │  │ Process Script N │          │
│  │                  │  │                  │  │                  │          │
│  │ - Extract payload│  │ - Extract payload│  │ - Extract payload│          │
│  │ - Find T-chunks  │  │ - Find T-chunks  │  │ - Find T-chunks  │          │
│  │ - Rewrite URLs   │  │ - Rewrite URLs   │  │ - Rewrite URLs   │          │
│  │ - Update lengths │  │ - Update lengths │  │ - Update lengths │          │
│  └──────────────────┘  └──────────────────┘  └──────────────────┘          │
│                                                                              │
│  Works for self-contained T-chunks, but cross-script T-chunks may have      │
│  incorrect lengths at this point.                                           │
└─────────────────────────────────────────────────────────────────────────────┘
                                      │
                                      ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                    PHASE 2: HTML Post-Processing                             │
│                    (Integration Hook: NextJsHtmlPostProcessor)               │
│                                                                              │
│  1. Find all RSC push scripts in complete HTML                              │
│                                                                              │
│  2. Extract payloads and combine with markers:                              │
│     "payload1\x00SPLIT\x00payload2\x00SPLIT\x00payload3..."                 │
│                                                                              │
│  3. Find T-chunks across combined content (markers don't count as bytes)    │
│                                                                              │
│  4. For each T-chunk:                                                        │
│     - Extract content (may span markers)                                    │
│     - Rewrite URLs                                                          │
│     - Calculate new byte length (excluding markers)                         │
│     - Write new header: ID:T<new_hex>,                                      │
│                                                                              │
│  5. Split on markers to get individual payloads                             │
│                                                                              │
│  6. Rebuild HTML with corrected scripts                                      │
└─────────────────────────────────────────────────────────────────────────────┘
                                      │
                                      ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                    Final HTML Response to Client                             │
│                                                                              │
│  - All URLs rewritten to proxy host                                         │
│  - All T-chunk lengths correctly reflect content after URL rewriting        │
│  - Script structure preserved (same number of push calls)                   │
│  - React hydration succeeds                                                 │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Test Results

| Test Case | Result |
|-----------|--------|
| T-chunk length shrinks (longer origin → shorter proxy) | Pass |
| T-chunk length grows (shorter origin → longer proxy) | Pass |
| Multiple T-chunks in same content | Pass |
| Escape sequences: `\n`, `\r`, `\t`, `\\`, `\"` | Pass |
| Unicode escapes: `\uHHHH` | Pass |
| Surrogate pairs: `\uD800\uDC00` | Pass |
| Hex escapes: `\xHH` | Pass |
| Various URL patterns (escaped slashes, etc.) | Pass |
| Cross-script T-chunk (header in script N, content in N+1) | Pass |
| Cross-script with multiple URLs in continuation | Pass |
| Non-T-chunk content preserved | Pass |
| HTML structure preserved after post-processing | Pass |

### Comparison: JS v7 vs JS v8 vs Rust

| Implementation | Approach | Fiber Count | Result |
|----------------|----------|-------------|--------|
| JS v7 | Per-script T-chunk rewriting | 0 | FAIL |
| JS v8 | Marker-based cross-script | 683 | PASS |
| Rust (final) | Two-phase with post-processor | 683 | PASS |

### Playwright Browser Testing (December 2024)

Automated testing with Playwright across Chrome and Firefox verified the implementation:

**Test Setup:**
- Fetched live HTML from a Next.js App Router site
- Applied RSC URL rewriting via the Rust post-processor
- Served rewritten HTML locally to isolate from bot detection

**Results (both Chrome and Firefox):**

| Metric | Value |
|--------|-------|
| Hydration errors detected | 0 |
| Console errors (hydration-related) | 0 |
| Total links in page | 120 |
| Links rewritten to proxy | 120 |
| Links still pointing to origin | 0 |
| RSC push scripts present | Yes |
| `self.__next_f` entries | 223 |
| `__next` root element | Present |

**Key Observations:**
1. **No hydration mismatch**: React successfully hydrated without any "Text content does not match" or "Hydration failed" errors
2. **Complete URL rewriting**: All 120 navigation links correctly point to the proxy host
3. **RSC data preserved**: All 223 RSC Flight entries present in `self.__next_f` array
4. **Cross-browser compatibility**: Identical behavior in Chrome (Chromium) and Firefox

---

## Decompression Pipeline for Post-Processing

The post-processor requires access to uncompressed HTML. Since origin responses are typically gzip or brotli compressed, the streaming pipeline was extended to support decompression-only mode.

### The Problem

```
┌─────────────────────────────────────────────────────────────────┐
│ Original Flow (without post-processing):                        │
│                                                                  │
│   Gzip In → Decompress → Process HTML → Recompress → Gzip Out   │
│                                                                  │
│ With post-processing, we need uncompressed output:              │
│                                                                  │
│   Gzip In → Decompress → Process HTML → ??? → Post-Process      │
│                                                                  │
│ If we recompress, post-processor gets garbage (compressed bytes)│
└─────────────────────────────────────────────────────────────────┘
```

### Solution: Decompression-Only Pipeline Modes

Added new pipeline transformation modes that decompress without recompressing:
- `process_gzip_to_none()` at [streaming_processor.rs:215](crates/common/src/streaming_processor.rs#L215)
- `process_deflate_to_none()` at [streaming_processor.rs:273](crates/common/src/streaming_processor.rs#L273)
- `process_brotli_to_none()` at [streaming_processor.rs:336](crates/common/src/streaming_processor.rs#L336)

### Publisher Flow with Post-Processing

The post-processing flow in publisher.rs:
1. Get post-processors from the integration registry
2. If post-processors exist, output uncompressed HTML (decompression-only mode)
3. Run streaming HTML processing
4. Apply each post-processor to the uncompressed HTML
5. Recompress once at the end to match original Content-Encoding

**Implementation:** Post-processing logic at [publisher.rs:203](crates/common/src/publisher.rs#L203)

### Benefits

1. **Single compression pass**: Avoids decompress → recompress → decompress → recompress cycle
2. **Valid UTF-8 for post-processor**: Post-processor receives actual HTML, not compressed bytes
3. **Preserves original compression**: Final output matches original Content-Encoding

---

## Implementation Files

| File | Purpose |
|------|---------|
| `crates/common/src/integrations/nextjs.rs` | RSC rewriting logic, post-processor |
| `crates/common/src/integrations/registry.rs` | `IntegrationHtmlPostProcessor` trait |
| `crates/common/src/integrations/mod.rs` | Module exports |
| `crates/common/src/publisher.rs` | Post-processor invocation, decompression flow |
| `crates/common/src/streaming_processor.rs` | Decompression-only pipeline modes |

### Key Functions in nextjs.rs

| Function | Line | Purpose |
|----------|------|---------|
| `extract_rsc_push_payload` | [232](crates/common/src/integrations/nextjs.rs#L232) | Extract string from `self.__next_f.push([1, '...'])` |
| `calculate_unescaped_byte_length` | [609](crates/common/src/integrations/nextjs.rs#L609) | Count unescaped bytes with escape handling |
| `consume_unescaped_bytes` | [686](crates/common/src/integrations/nextjs.rs#L686) | Advance through string consuming N bytes |
| `find_tchunks` | [767](crates/common/src/integrations/nextjs.rs#L767) | Find T-chunks in single script |
| `rewrite_rsc_url_string` | [803](crates/common/src/integrations/nextjs.rs#L803) | URL rewriting with escape handling |
| `rewrite_rsc_tchunks` | [833](crates/common/src/integrations/nextjs.rs#L833) | Single-script T-chunk processing |
| `consume_unescaped_bytes_skip_markers` | [910](crates/common/src/integrations/nextjs.rs#L910) | Advance through string, skipping markers |
| `calculate_unescaped_byte_length_skip_markers` | [991](crates/common/src/integrations/nextjs.rs#L991) | Count unescaped bytes, excluding markers |
| `find_tchunks_with_markers` | [1005](crates/common/src/integrations/nextjs.rs#L1005) | Find T-chunks in marker-combined content |
| `rewrite_rsc_scripts_combined` | [1056](crates/common/src/integrations/nextjs.rs#L1056) | Cross-script T-chunk processing |
| `find_rsc_push_scripts` | [1165](crates/common/src/integrations/nextjs.rs#L1165) | Find all RSC scripts in HTML |
| `post_process_rsc_html` | [1245](crates/common/src/integrations/nextjs.rs#L1245) | Complete HTML post-processing |

---

## Limitations

### Very Long Proxy URLs

If the proxy URL is significantly longer than the original, T-chunk content grows substantially. This is handled correctly (the hex length is recalculated), but it may affect:
- Response size
- Streaming behavior if scripts become much larger

### Performance Considerations

The post-processing phase requires:
1. Parsing complete HTML to find scripts (O(n) string scan)
2. Combining payloads (memory allocation)
3. Regex matching for T-chunks
4. String rebuilding

For typical pages with 100-300 RSC scripts, this adds ~1-5ms to processing time.

### Edge Cases Not Handled

- Malformed RSC content (missing closing quotes, invalid hex)
- Nested script tags (shouldn't occur in valid HTML)
- Non-UTF8 encoded pages (requires UTF-8)

---

## Deconstruction and Reconstruction Logic

The RSC rewriting process involves carefully deconstructing RSC payloads, rewriting URLs, and reconstructing them with correct T-chunk lengths. The main entry point is `post_process_rsc_html()` at [nextjs.rs:1245](crates/common/src/integrations/nextjs.rs#L1245).

### Step 1: Find RSC Push Scripts

Find all `self.__next_f.push([1, "..."])` scripts in the HTML and extract their payloads.

**Implementation:** `find_rsc_push_scripts()` at [nextjs.rs:1165](crates/common/src/integrations/nextjs.rs#L1165)

### Step 2: Combine Payloads with Markers

Join all payloads with a marker string (`\x00SPLIT\x00`) that cannot appear in valid JSON/RSC content. This allows T-chunks to be processed across script boundaries while preserving the ability to split back later.

**Implementation:** Marker constant at [nextjs.rs:906](crates/common/src/integrations/nextjs.rs#L906), combining logic in `rewrite_rsc_scripts_combined()` at [nextjs.rs:1056](crates/common/src/integrations/nextjs.rs#L1056)

### Step 3: Find T-Chunks Across Combined Content

Parse T-chunk headers (`ID:T<hex_length>,`) and consume exactly the declared number of unescaped bytes, skipping over markers.

**Implementation:** `find_tchunks_with_markers()` at [nextjs.rs:1005](crates/common/src/integrations/nextjs.rs#L1005), using `consume_unescaped_bytes_skip_markers()` at [nextjs.rs:910](crates/common/src/integrations/nextjs.rs#L910)

### Step 4: Rewrite URLs in T-Chunk Content

Rewrite all URL patterns in the T-chunk content:
- `https://origin.example.com/path` → `http://proxy.example.com/path`
- `//origin.example.com/path` → `//proxy.example.com/path`
- `\\/\\/origin.example.com` → `\\/\\/proxy.example.com` (JSON-escaped)
- `\\\\//origin.example.com` → `\\\\//proxy.example.com` (double-escaped)

**Implementation:** `rewrite_rsc_url_string()` at [nextjs.rs:803](crates/common/src/integrations/nextjs.rs#L803)

### Step 5: Recalculate T-Chunk Length

Calculate the new unescaped byte length (excluding markers) and update the T-chunk header with the new hex length.

**Implementation:** `calculate_unescaped_byte_length_skip_markers()` at [nextjs.rs:991](crates/common/src/integrations/nextjs.rs#L991)

### Step 6: Split Back on Markers

Split the combined rewritten content back into individual payloads on the marker boundaries. Each payload corresponds to one original script, with T-chunk lengths now correct across script boundaries.

**Implementation:** Part of `rewrite_rsc_scripts_combined()` at [nextjs.rs:1056](crates/common/src/integrations/nextjs.rs#L1056)

### Step 7: Reconstruct HTML

Replace each original script with its rewritten version in the HTML.

**Implementation:** Part of `post_process_rsc_html()` at [nextjs.rs:1245](crates/common/src/integrations/nextjs.rs#L1245)

### Visual Example

```
BEFORE (2 scripts, T-chunk spans both):
┌──────────────────────────────────────────────────────────────────┐
│ Script 1: self.__next_f.push([1,"11:null\n1a:T68f,"])           │
│           └─ T-chunk header: 1a:T68f (1679 bytes declared)       │
├──────────────────────────────────────────────────────────────────┤
│ Script 2: self.__next_f.push([1,"{\"url\":\"https://origin...."])│
│           └─ T-chunk content continues here (1679 bytes total)   │
└──────────────────────────────────────────────────────────────────┘

COMBINED (with marker):
"11:null\n1a:T68f,\x00SPLIT\x00{\"url\":\"https://origin.example.com/...\"}"
                  ^^^^^^^^^^ marker (not counted in byte length)

AFTER URL REWRITE:
"11:null\n1a:T652,\x00SPLIT\x00{\"url\":\"http://proxy.example.com/...\"}"
              ^^^ new hex length (shorter URL = smaller length)

SPLIT BACK:
┌──────────────────────────────────────────────────────────────────┐
│ Script 1: self.__next_f.push([1,"11:null\n1a:T652,"])           │
│           └─ Updated T-chunk header with correct length          │
├──────────────────────────────────────────────────────────────────┤
│ Script 2: self.__next_f.push([1,"{\"url\":\"http://proxy.exa..."])│
│           └─ Rewritten URLs in content                           │
└──────────────────────────────────────────────────────────────────┘
```

---

## Comparison: Old vs New Approach

| Aspect | Old (Whitespace Padding) | New (T-Chunk Length Recalculation) |
|--------|--------------------------|-------------------------------------|
| T-chunk handling | Broken - lengths not updated | Correct - lengths recalculated |
| URL length change | Limited to shorter URLs | Any length change supported |
| Escape sequences | Not properly counted | Fully supported |
| Cross-script T-chunks | Not handled | Handled via post-processing |
| Implementation | Simple regex replace | Full T-chunk parsing + post-processing |
| Architecture | Hardcoded in processor | Integration hook pattern |
| Extensibility | None | Other integrations can add post-processors |

---

## Conclusion

RSC hydration requires **correct T-chunk byte lengths**. The solution involves two phases:

### Phase 1: Streaming (per-script)
- Process each script as it arrives
- Handle self-contained T-chunks
- ~95% of T-chunks are handled here

### Phase 2: Post-Processing (cross-script)
- After streaming completes
- Combine scripts with markers
- Recalculate T-chunk lengths across boundaries
- Handles the remaining ~5% edge cases

The key insights are:
1. **T-chunk lengths must match content**: The RSC parser uses declared lengths to navigate
2. **T-chunks can span scripts**: Next.js streaming splits content arbitrarily
3. **Markers enable cross-script processing**: Combine, process, split back
4. **Integration hooks enable extensibility**: Other integrations can add post-processors

---

## References

- React Flight Protocol: Internal React implementation for RSC streaming
- Next.js App Router: https://nextjs.org/docs/app
- lol_html: https://github.com/nicksrandall/lol-html (streaming HTML rewriter)
- Implementation: `crates/common/src/integrations/nextjs.rs`
