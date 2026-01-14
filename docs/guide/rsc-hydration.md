# RSC Hydration URL Rewriting

This document explains how Trusted Server handles URL rewriting in React Server Components (RSC) payloads for Next.js App Router sites.

## Background: How Next.js Delivers RSC Data

Next.js App Router uses React Server Components with a streaming "flight" protocol. RSC data is delivered to the browser via inline `<script>` tags that call `self.__next_f.push()`:

```html
<script>
  self.__next_f.push([0]);
</script>
<script>
  self.__next_f.push([1, "0:[[...RSC content...]"]);
</script>
<script>
  self.__next_f.push([1, '1a:T29,{"url":"https://origin.example.com"}']);
</script>
```

The `[1, "..."]` calls contain the actual RSC payload as a JavaScript string.

For client-side navigations, Next.js fetches Flight directly (no `<script>` wrapper) and expects `content-type: text/x-component`. The response body is the same row format, but without JSON/JS-string escaping (trusted-server rewrites these via `crates/common/src/rsc_flight.rs`).

## RSC Flight Protocol Format

The Flight stream is framed as **rows**. Most rows are delimited by `\n` (literal backslash-n in the JS strings inside `__next_f.push([1,"..."])`), but **`T` rows are length-delimited and do not end with a newline**.

| Record Type    | Format                       | Example                     |
| -------------- | ---------------------------- | --------------------------- |
| T-chunk (text) | `ID:T<hex_length>,<content>` | `1a:T29,{"url":"..."}`      |
| JSON array     | `ID:[...]`                   | `0:[["page",null,...]]`     |
| JSON object    | `ID:{...}`                   | `5:{"name":"value"}`        |
| Module import  | `ID:I[...]`                  | `2:I["chunk-id",...]`       |
| Head link      | `ID:HL[...]`                 | `3:HL["/_next/static/..."]` |
| Reference      | `ID:$ref`                    | `4:$L5`                     |
| String         | `ID:"..."`                   | `6:"hello"`                 |
| Null           | `ID:null`                    | `7:null`                    |

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

| Escape Sequence | String Chars | Unescaped Bytes    |
| --------------- | ------------ | ------------------ |
| `\n`            | 2            | 1                  |
| `\r`            | 2            | 1                  |
| `\t`            | 2            | 1                  |
| `\\`            | 2            | 1                  |
| `\"`            | 2            | 1                  |
| `\xHH`          | 4            | 1                  |
| `\uHHHH`        | 6            | 1-3 (UTF-8 bytes)  |
| `\uD800\uDC00`  | 12           | 4 (surrogate pair) |

## T-Chunks Can Span Multiple Push Scripts

A critical aspect of RSC handling is that T-chunk content can span multiple `self.__next_f.push()` calls:

```html
<!-- Script 10: T-chunk HEADER only -->
<script>
  self.__next_f.push([1, "11:null\n1a:T928,"]);
</script>

<!-- Script 11: T-chunk CONTENT (the 2344 unescaped bytes) -->
<script>
  self.__next_f.push([1, "...2344 bytes of actual content..."]);
</script>
```

This happens because Next.js streams RSC data as it becomes available. The T-chunk header in script 10 declares 928 bytes (0x928 = 2344 decimal), but those bytes are delivered in script 11.

### Real-World Example

In production Next.js sites, T-chunks commonly span scripts like this:

```html
<!-- Script 59: Contains T-chunk header, content starts but is incomplete -->
<script>
  self.__next_f.push([
    1,
    '435:null\n436:T68f,{"children":["article",{"href":"https://origin.example.com/news',
  ]);
</script>

<!-- Script 60: Contains the rest of the T-chunk content -->
<script>
  self.__next_f.push([1, '/tech"},"Read more"]}\n437:{"className":"footer"}']);
</script>
```

Here, the T-chunk header `436:T68f,` (declaring 0x68f = 1679 bytes) is in script 59, but the content containing URLs to rewrite spans into script 60. The header must be updated based on URL changes made in subsequent scripts.

## Two-Phase Processing

Trusted Server uses a two-phase approach to handle cross-script T-chunks correctly.

### Phase 1: Streaming HTML Processing

The HTML rewriter runs in a streaming pipeline (decompress → rewrite → recompress). During this phase we:

- Rewrite standard HTML attributes (`href`, `src`, `srcset`, etc.)
- Run integration script rewriters for self-contained payloads (e.g., Pages Router `__NEXT_DATA__`)
- For App Router `__next_f.push([1,"..."])` scripts, `NextJsRscPlaceholderRewriter` captures complete payload strings into placeholders for post-processing; fragmented scripts are left intact for the fallback re-parse

### Phase 2: HTML Post-Processing (cross-script RSC)

At end-of-document, the Next.js integration rewrites cross-script T-chunks. The fast path avoids a second HTML parse; the fallback re-parses when needed:

1. Placeholder fast path (no re-parse). During the initial `lol_html` pass, `NextJsRscPlaceholderRewriter` replaces each complete `__next_f.push([1,"..."])` payload string with a placeholder token and records the original payloads in `IntegrationDocumentState`. `NextJsHtmlPostProcessor` rewrites the recorded payload strings using the marker-based cross-script algorithm (combine → rewrite → split) and substitutes the placeholders in the final HTML.
2. Fallback re-parse. If no payloads were captured (for example, script text was fragmented), `NextJsHtmlPostProcessor` re-parses the final HTML with `lol_html`, finds `__next_f.push` payload ranges, and rewrites them in place.

This phase is gated by `IntegrationHtmlPostProcessor::should_process`, which checks for captured payloads and also scans the final HTML for `__next_f.push` plus the origin host so fragmented scripts are still handled.

## Marker-Based Cross-Script Processing

### Step 1: Combine Scripts with Markers

Concatenate all RSC push payload strings using a marker delimiter that cannot appear in valid JSON/RSC content.

The marker `\x00SPLIT\x00` is chosen because:

- Contains null byte (`\x00`) which cannot appear in valid JSON/RSC content
- Easily identifiable for splitting
- Won't be confused with any escape sequence

When there is only one payload, Trusted Server skips combining and rewrites it directly.

**Implementation:** Marker constant at [rsc.rs:11](https://github.com/InteractiveAdvertisingBureau/trusted-server/blob/main/crates/common/src/integrations/nextjs/rsc.rs#L11) and combine/split logic in [rsc.rs:433](https://github.com/InteractiveAdvertisingBureau/trusted-server/blob/main/crates/common/src/integrations/nextjs/rsc.rs#L433)

### Step 2: Find T-Chunks Across Combined Content

Scan the combined stream for `ID:T<hex_length>,` headers, then consume exactly `hex_length` unescaped bytes to find the T-chunk boundary.

The key insight: markers don't count toward byte consumption. When a T-chunk declares 1679 bytes, we consume 1679 bytes of actual content, skipping over any markers we encounter.

**Implementation:** T-chunk discovery at [rsc.rs:202](https://github.com/InteractiveAdvertisingBureau/trusted-server/blob/main/crates/common/src/integrations/nextjs/rsc.rs#L202) with marker-aware escape sequence iterator at [rsc.rs:72](https://github.com/InteractiveAdvertisingBureau/trusted-server/blob/main/crates/common/src/integrations/nextjs/rsc.rs#L72)

### Step 3: Rewrite URLs and Recalculate Lengths

For each `T` chunk:

1. Rewrite URLs in the chunk content (preserving marker bytes)
2. Recalculate the unescaped byte length (excluding markers)
3. Rewrite the header to `ID:T<new_hex>,`

### Step 4: Split Back on Markers

Split the rewritten combined content by the marker to recover per-script payload strings.

Each resulting payload corresponds to one original script, but with:

- URLs rewritten
- T-chunk lengths correctly recalculated across script boundaries

## Byte Length Calculation Algorithm

`T`-chunk lengths use the **unescaped** byte count of the payload (after decoding JavaScript string escapes). Correct handling requires:

- Shared escape sequence iterator handles standard JS escapes (including `\\n`, `\\r`, `\\t`, `\\b`, `\\f`, `\\v`, `\\'`, `\\\"`, `\\\\`, `\\/`, `\\xHH`, `\\uHHHH`, and surrogate pairs): [rsc.rs:37](https://github.com/InteractiveAdvertisingBureau/trusted-server/blob/main/crates/common/src/integrations/nextjs/rsc.rs#L37)
- Counting unescaped bytes: [rsc.rs:166](https://github.com/InteractiveAdvertisingBureau/trusted-server/blob/main/crates/common/src/integrations/nextjs/rsc.rs#L166)
- Consuming exactly _N unescaped bytes_ to locate the end of a declared `T` chunk: [rsc.rs:171](https://github.com/InteractiveAdvertisingBureau/trusted-server/blob/main/crates/common/src/integrations/nextjs/rsc.rs#L171)
- Marker-aware byte length calculation for cross-script processing: [rsc.rs:324](https://github.com/InteractiveAdvertisingBureau/trusted-server/blob/main/crates/common/src/integrations/nextjs/rsc.rs#L324)
- Size-limited combined payload allocation (default 10 MB, configurable via `integrations.nextjs.max_combined_payload_bytes`): [rsc.rs:404](https://github.com/InteractiveAdvertisingBureau/trusted-server/blob/main/crates/common/src/integrations/nextjs/rsc.rs#L404)
- If the size limit is exceeded and all T-chunks are complete within each payload, Trusted Server rewrites each payload independently: [rsc.rs:427](https://github.com/InteractiveAdvertisingBureau/trusted-server/blob/main/crates/common/src/integrations/nextjs/rsc.rs#L427)
- Fail-safe: if `T`-chunk parsing fails or a T-chunk length is unreasonable (over 100 MB), Trusted Server skips rewriting to avoid breaking hydration: [rsc.rs:202](https://github.com/InteractiveAdvertisingBureau/trusted-server/blob/main/crates/common/src/integrations/nextjs/rsc.rs#L202)
- If the size limit is exceeded and cross-script T-chunks are present, Trusted Server skips rewriting rather than risk breaking hydration: [rsc.rs:421](https://github.com/InteractiveAdvertisingBureau/trusted-server/blob/main/crates/common/src/integrations/nextjs/rsc.rs#L421)

## URL Rewriting Patterns

The solution handles multiple URL formats in RSC content:

| Pattern              | Example                   | In RSC String             |
| -------------------- | ------------------------- | ------------------------- |
| Full HTTPS           | `https://host/path`       | `https://host/path`       |
| Full HTTP            | `http://host/path`        | `http://host/path`        |
| Protocol-relative    | `//host/path`             | `//host/path`             |
| Bare host (boundary) | `origin.example.com/path` | `origin.example.com/path` |
| JSON-escaped slashes | `//host/path`             | `\\/\\/host/path`         |
| Double-escaped       | `\\/\\/host`              | `\\\\/\\\\/host`          |
| Quad-escaped         | `\\\\/\\\\/host`          | `\\\\\\\\//host`          |

### Regex Pattern

**Implementation:** Regex-based rewriting in [shared.rs:79](https://github.com/InteractiveAdvertisingBureau/trusted-server/blob/main/crates/common/src/integrations/nextjs/shared.rs#L79)

This pattern handles:

- Optional scheme (`https?`)?
- Optional colon (`:`)?
- Multiple escape levels for slashes
- The escaped origin hostname

After the regex pass, `RscUrlRewriter` runs a boundary-aware bare-host rewrite (via `rewrite_bare_host_at_boundaries`) so fields like `siteProductionDomain` are rewritten, while `cdn.origin.example.com` and `origin.example.com.uk` are not.

## Complete Processing Flow

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         HTML Response from Origin                            │
└─────────────────────────────────────────────────────────────────────────────┘
                                      │
                                      ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                    PHASE 1: HTML Rewrite (lol_html)                           │
│                                                                              │
│  - Rewrite HTML attributes (href/src/etc.)                                    │
│  - Rewrite Pages Router data (`__NEXT_DATA__`)                                │
│  - For App Router RSC push scripts (`__next_f.push([1,\"...\"])`):            │
│      * Replace payload string with placeholder token                          │
│      * Record original payloads (IntegrationDocumentState)                    │
└─────────────────────────────────────────────────────────────────────────────┘
                                      │
                                      ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                    PHASE 2: HTML Post-Processing                             │
│                    (Integration Hook: NextJsHtmlPostProcessor)               │
│                                                                              │
│  - Rewrite recorded payloads (marker-based cross-script T-chunk logic)       │
│  - Substitute placeholders in the final HTML with rewritten payload strings  │
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

Note: this diagram shows the placeholder fast path. When no placeholders are captured, the post-processor re-parses the final HTML to locate and rewrite RSC payloads.

## Compression Pipeline with Post-Processing

Post-processing requires access to uncompressed UTF‑8 HTML, but the trusted server still preserves the origin `Content-Encoding` on the wire.

End-to-end flow:

1. `StreamingPipeline` decompresses the origin body based on `Content-Encoding`
2. The HTML processor runs `lol_html` rewriting and (optionally) integration post-processors on the complete HTML
3. `StreamingPipeline` recompresses to the original encoding

Because post-processing runs inside the HTML processor (before recompression), `publisher.rs` does not need to special-case compression for integrations.

**Implementation:** Post-processing entry point at [html_processor.rs:20](https://github.com/InteractiveAdvertisingBureau/trusted-server/blob/main/crates/common/src/html_processor.rs#L20)

## Deconstruction and Reconstruction Logic

The RSC rewriting process involves carefully deconstructing RSC payloads, rewriting URLs, and reconstructing them with correct T-chunk lengths. The main runtime entry point is `NextJsHtmlPostProcessor::post_process()` at [html_post_process.rs:53](https://github.com/InteractiveAdvertisingBureau/trusted-server/blob/main/crates/common/src/integrations/nextjs/html_post_process.rs#L53), operating on payloads captured during phase 1 by `NextJsRscPlaceholderRewriter` ([rsc_placeholders.rs:52](https://github.com/InteractiveAdvertisingBureau/trusted-server/blob/main/crates/common/src/integrations/nextjs/rsc_placeholders.rs#L52)) when available, and falling back to re-parsing the final HTML when not.

### Step 1: Capture RSC Payloads (placeholders)

During the initial HTML rewrite pass, replace each complete `self.__next_f.push([1, "..."])` payload string with a placeholder token and record the original payload strings in `IntegrationDocumentState`. Fragmented scripts are left untouched and handled by the fallback re-parse path.

**Implementation:** `NextJsRscPlaceholderRewriter::rewrite()` at [rsc_placeholders.rs:52](https://github.com/InteractiveAdvertisingBureau/trusted-server/blob/main/crates/common/src/integrations/nextjs/rsc_placeholders.rs#L52) and `IntegrationDocumentState` at [registry.rs:99](https://github.com/InteractiveAdvertisingBureau/trusted-server/blob/main/crates/common/src/integrations/registry.rs#L99)

### Step 2: Combine Payloads with Markers

Join all payloads with a marker string (`\x00SPLIT\x00`) that cannot appear in valid JSON/RSC content. This allows T-chunks to be processed across script boundaries while preserving the ability to split back later.

**Implementation:** Marker constant at [rsc.rs:11](https://github.com/InteractiveAdvertisingBureau/trusted-server/blob/main/crates/common/src/integrations/nextjs/rsc.rs#L11), combining logic in `rewrite_rsc_scripts_combined()` at [rsc.rs:433](https://github.com/InteractiveAdvertisingBureau/trusted-server/blob/main/crates/common/src/integrations/nextjs/rsc.rs#L433)

### Step 3: Find T-Chunks Across Combined Content

Parse T-chunk headers (`ID:T<hex_length>,`) and consume exactly the declared number of unescaped bytes, skipping over markers.

**Implementation:** `find_tchunks_with_markers()` at [rsc.rs:269](https://github.com/InteractiveAdvertisingBureau/trusted-server/blob/main/crates/common/src/integrations/nextjs/rsc.rs#L269), using `EscapeSequenceIter::from_position_with_marker()` at [rsc.rs:72](https://github.com/InteractiveAdvertisingBureau/trusted-server/blob/main/crates/common/src/integrations/nextjs/rsc.rs#L72)

### Step 4: Rewrite URLs in T-Chunk Content

Rewrite all URL patterns in the T-chunk content:

- `https://origin.example.com/path` → `http://proxy.example.com/path`
- `//origin.example.com/path` → `//proxy.example.com/path`
- `\\/\\/origin.example.com` → `\\/\\/proxy.example.com` (JSON-escaped)
- `\\\\//origin.example.com` → `\\\\//proxy.example.com` (double-escaped)
- `origin.example.com/path` → `proxy.example.com/path` (bare host, boundary-checked)

**Implementation:** `RscUrlRewriter::rewrite()` at [shared.rs:93](https://github.com/InteractiveAdvertisingBureau/trusted-server/blob/main/crates/common/src/integrations/nextjs/shared.rs#L93)

### Step 5: Recalculate T-Chunk Length

Calculate the new unescaped byte length (excluding markers) and update the T-chunk header with the new hex length.

**Implementation:** `calculate_unescaped_byte_length_skip_markers()` at [rsc.rs:324](https://github.com/InteractiveAdvertisingBureau/trusted-server/blob/main/crates/common/src/integrations/nextjs/rsc.rs#L324)

### Step 6: Split Back on Markers

Split the combined rewritten content back into individual payloads on the marker boundaries. Each payload corresponds to one original script, with T-chunk lengths now correct across script boundaries.

**Implementation:** Part of `rewrite_rsc_scripts_combined()` at [rsc.rs:478](https://github.com/InteractiveAdvertisingBureau/trusted-server/blob/main/crates/common/src/integrations/nextjs/rsc.rs#L478)

### Step 7: Reconstruct HTML

Substitute placeholder tokens in the final HTML with the rewritten payload strings (fast path, no HTML re-parse).

**Implementation:** `substitute_rsc_payload_placeholders()` at [html_post_process.rs:177](https://github.com/InteractiveAdvertisingBureau/trusted-server/blob/main/crates/common/src/integrations/nextjs/html_post_process.rs#L177)

### Fallback Path: Re-parse HTML for Fragmented Scripts

If no placeholders were captured during streaming, the post-processor re-parses the final HTML with `lol_html` to locate `__next_f.push` payload ranges and rewrites them in place. This path is slower, but it handles fragmented script text that could not be captured during the streaming pass.

**Implementation:** `find_rsc_push_scripts()` and `post_process_rsc_html_in_place_with_limit()` in [html_post_process.rs](https://github.com/InteractiveAdvertisingBureau/trusted-server/blob/main/crates/common/src/integrations/nextjs/html_post_process.rs)

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

## Integration Hook Architecture

The post-processing is implemented as an integration hook, allowing other integrations to also perform HTML post-processing.

### Trait Definition

**Implementation:** Per-document state at [registry.rs:99](https://github.com/InteractiveAdvertisingBureau/trusted-server/blob/main/crates/common/src/integrations/registry.rs#L99), context at [registry.rs:331](https://github.com/InteractiveAdvertisingBureau/trusted-server/blob/main/crates/common/src/integrations/registry.rs#L331), and trait at [registry.rs:341](https://github.com/InteractiveAdvertisingBureau/trusted-server/blob/main/crates/common/src/integrations/registry.rs#L341)

**Note:** `IntegrationHtmlPostProcessor::should_process` defaults to `false`, so integrations must explicitly opt in to post-processing via a cheap preflight check. The Next.js implementation checks for captured payloads and also scans the final HTML for `__next_f.push` plus the origin host to catch fragmented scripts.

### Registration

**Implementation:** Next.js registers its placeholder rewriter + HTML post-processor when enabled in [mod.rs:86](https://github.com/InteractiveAdvertisingBureau/trusted-server/blob/main/crates/common/src/integrations/nextjs/mod.rs#L86)

### Execution in HTML Processor

**Implementation:** End-of-document post-processing wrapper at [html_processor.rs:20](https://github.com/InteractiveAdvertisingBureau/trusted-server/blob/main/crates/common/src/html_processor.rs#L20)

## Implementation Files

| File                                                         | Purpose                                                             |
| ------------------------------------------------------------ | ------------------------------------------------------------------- |
| `crates/common/src/integrations/nextjs/mod.rs`               | Next.js integration config + registration                           |
| `crates/common/src/integrations/nextjs/html_post_process.rs` | HTML post-processing for cross-script RSC (placeholders + fallback) |
| `crates/common/src/integrations/nextjs/rsc_placeholders.rs`  | RSC placeholder insertion + payload capture (unfragmented scripts)  |
| `crates/common/src/integrations/nextjs/rsc.rs`               | RSC T-chunk parsing + URL rewriting                                 |
| `crates/common/src/integrations/nextjs/script_rewriter.rs`   | Script rewrites (`__NEXT_DATA__`)                                   |
| `crates/common/src/integrations/nextjs/shared.rs`            | Shared regex patterns, payload parsing, and `RscUrlRewriter`        |
| `crates/common/src/host_rewrite.rs`                          | Boundary-aware bare-host rewriting (shared by RSC and Flight)       |
| `crates/common/src/rsc_flight.rs`                            | Flight response rewriting (`text/x-component`)                      |
| `crates/common/src/integrations/registry.rs`                 | Integration traits + `IntegrationDocumentState`                     |
| `crates/common/src/integrations/mod.rs`                      | Module exports                                                      |
| `crates/common/src/html_processor.rs`                        | HTML rewriting + post-processor invocation                          |
| `crates/common/src/publisher.rs`                             | Response routing + streaming pipeline setup                         |
| `crates/common/src/streaming_processor.rs`                   | Compression transforms + `StreamProcessor`                          |

## Limitations

### Very Long Proxy URLs

If the proxy URL is significantly longer than the original, T-chunk content grows substantially. This is handled correctly (the hex length is recalculated), but it may affect:

- Response size
- Streaming behavior if scripts become much larger

### Performance Considerations

The post-processing phase has two paths:

Fast path (placeholders):

1. Placeholder insertion during the initial `lol_html` pass (payload capture)
2. Combining payloads (memory allocation)
3. Regex matching for T-chunks
4. One pass placeholder substitution over the final HTML string

Fallback path (no placeholders):

1. Re-parse the final HTML with `lol_html` to find `__next_f` payload ranges
2. Combine and rewrite payloads as above

For typical pages with 100-300 RSC scripts, the fast path adds ~1-5ms to processing time; the fallback path adds an extra `lol_html` pass and can be higher.

### Edge Cases Not Handled

- Malformed RSC content (missing closing quotes, invalid hex)
- Nested script tags (shouldn't occur in valid HTML)
- Non-UTF8 encoded pages (requires UTF-8)

## References

- React Flight Protocol: Internal React implementation for RSC streaming: https://github.com/vercel/next.js/tree/v14.2.35
- Next.js App Router: https://nextjs.org/docs/app
- lol_html: https://github.com/nicksrandall/lol-html (streaming HTML rewriter)
- Implementation: `crates/common/src/integrations/nextjs/mod.rs` and `crates/common/src/integrations/nextjs/`
