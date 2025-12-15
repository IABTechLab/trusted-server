# RSC Hydration URL Rewriting: Technical Findings

## Problem Statement

When proxying Next.js App Router sites, URL rewriting in RSC (React Server Components) payloads caused React hydration to fail. The symptom was 0 React fiber nodes after page load, indicating complete hydration failure.

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

### Phase 1: Streaming HTML Processing

The HTML rewriter runs in a streaming pipeline (decompress → rewrite → recompress). During this phase we:

- Rewrite standard HTML attributes (`href`, `src`, `srcset`, etc.)
- Run integration script rewriters for self-contained payloads (e.g., Pages Router `__NEXT_DATA__`)
- Leave `self.__next_f.push([1,"..."])` scripts untouched because T-chunks can span script boundaries

### Phase 2: HTML Post-Processing (cross-script RSC)

At end-of-document, a post-processor handles cross-script T-chunks:

1. **Finds all RSC push scripts** in the complete HTML
2. **Combines their payloads** with markers
3. **Processes T-chunks across the combined content**, skipping markers when counting bytes
4. **Rewrites URLs and recalculates lengths** for the combined content
5. **Splits back on markers** to get individual rewritten payloads
6. **Rebuilds the HTML** with rewritten scripts

This phase is gated by a cheap `should_process` preflight so non‑Next.js pages do not pay the extra pass.

### Marker-Based Cross-Script Processing

#### Step 1: Combine Scripts with Markers

Concatenate all RSC push payload strings using a marker delimiter that cannot appear in valid JSON/RSC content.

The marker `\x00SPLIT\x00` is chosen because:

- Contains null byte (`\x00`) which cannot appear in valid JSON/RSC content
- Easily identifiable for splitting
- Won't be confused with any escape sequence

**Implementation:** Marker constant at [nextjs.rs:903](crates/common/src/integrations/nextjs.rs#L903) and combine/split logic in [nextjs.rs:1053](crates/common/src/integrations/nextjs.rs#L1053)

#### Step 2: Find T-Chunks Across Combined Content

Scan the combined stream for `ID:T<hex_length>,` headers, then consume exactly `hex_length` unescaped bytes to find the T-chunk boundary.

The key insight: markers don't count toward byte consumption. When a T-chunk declares 1679 bytes, we consume 1679 bytes of actual content, skipping over any markers we encounter.

**Implementation:** T-chunk discovery at [nextjs.rs:1002](crates/common/src/integrations/nextjs.rs#L1002) with marker-aware consumption in [nextjs.rs:907](crates/common/src/integrations/nextjs.rs#L907)

#### Step 3: Rewrite URLs and Recalculate Lengths

For each `T` chunk:

1. Rewrite URLs in the chunk content (preserving marker bytes)
2. Recalculate the unescaped byte length (excluding markers)
3. Rewrite the header to `ID:T<new_hex>,`

#### Step 4: Split Back on Markers

Split the rewritten combined content by the marker to recover per-script payload strings.

Each resulting payload corresponds to one original script, but with:

- URLs rewritten
- T-chunk lengths correctly recalculated across script boundaries

---

## Integration Hook Architecture

The post-processing is implemented as an integration hook, allowing other integrations to also perform HTML post-processing.

### Trait Definition

**Implementation:** Context at [registry.rs:254](crates/common/src/integrations/registry.rs#L254) and trait at [registry.rs:263](crates/common/src/integrations/registry.rs#L263)

### Registration

**Implementation:** Next.js registers its HTML post-processor in [nextjs.rs:41](crates/common/src/integrations/nextjs.rs#L41)

### Execution in HTML Processor

**Implementation:** End-of-document post-processing wrapper at [html_processor.rs:20](crates/common/src/html_processor.rs#L20)

---

## Byte Length Calculation Algorithm

`T`-chunk lengths use the **unescaped** byte count of the payload (after decoding JavaScript string escapes). Correct handling requires:

- Counting unescaped bytes while accounting for `\\n`, `\\xHH`, `\\uHHHH`, and surrogate pairs: [nextjs.rs:606](crates/common/src/integrations/nextjs.rs#L606)
- Consuming exactly *N unescaped bytes* to locate the end of a declared `T` chunk: [nextjs.rs:683](crates/common/src/integrations/nextjs.rs#L683)
- Marker-aware variants for cross-script processing (skip `RSC_MARKER` during counting/consumption): [nextjs.rs:988](crates/common/src/integrations/nextjs.rs#L988) and [nextjs.rs:907](crates/common/src/integrations/nextjs.rs#L907)

---

## URL Rewriting Patterns

The solution handles multiple URL formats in RSC content:

| Pattern              | Example             | In RSC String       |
| -------------------- | ------------------- | ------------------- |
| Full HTTPS           | `https://host/path` | `https://host/path` |
| Full HTTP            | `http://host/path`  | `http://host/path`  |
| Protocol-relative    | `//host/path`       | `//host/path`       |
| JSON-escaped slashes | `//host/path`       | `\\/\\/host/path`   |
| Double-escaped       | `\\/\\/host`        | `\\\\/\\\\/host`    |
| Quad-escaped         | `\\\\/\\\\/host`    | `\\\\\\\\//host`    |

### Regex Pattern

**Implementation:** Regex-based rewriting in [nextjs.rs:800](crates/common/src/integrations/nextjs.rs#L800)

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

| Test Case                                                 | Result |
| --------------------------------------------------------- | ------ |
| T-chunk length shrinks (longer origin → shorter proxy)    | Pass   |
| T-chunk length grows (shorter origin → longer proxy)      | Pass   |
| Multiple T-chunks in same content                         | Pass   |
| Escape sequences: `\n`, `\r`, `\t`, `\\`, `\"`            | Pass   |
| Unicode escapes: `\uHHHH`                                 | Pass   |
| Surrogate pairs: `\uD800\uDC00`                           | Pass   |
| Hex escapes: `\xHH`                                       | Pass   |
| Various URL patterns (escaped slashes, etc.)              | Pass   |
| Cross-script T-chunk (header in script N, content in N+1) | Pass   |
| Cross-script with multiple URLs in continuation           | Pass   |
| Non-T-chunk content preserved                             | Pass   |
| HTML structure preserved after post-processing            | Pass   |

### Comparison: JS v7 vs JS v8 vs Rust

| Implementation | Approach                      | Fiber Count | Result |
| -------------- | ----------------------------- | ----------- | ------ |
| JS v7          | Per-script T-chunk rewriting  | 0           | FAIL   |
| JS v8          | Marker-based cross-script     | 683         | PASS   |
| Rust (final)   | Two-phase with post-processor | 683         | PASS   |

### Playwright Browser Testing (December 2024)

Automated testing with Playwright across Chrome and Firefox verified the implementation:

**Test Setup:**

- Fetched live HTML from a Next.js App Router site
- Applied RSC URL rewriting via the Rust post-processor
- Served rewritten HTML locally to isolate from bot detection

**Results (both Chrome and Firefox):**

| Metric                             | Value   |
| ---------------------------------- | ------- |
| Hydration errors detected          | 0       |
| Console errors (hydration-related) | 0       |
| Total links in page                | 120     |
| Links rewritten to proxy           | 120     |
| Links still pointing to origin     | 0       |
| RSC push scripts present           | Yes     |
| `self.__next_f` entries            | 223     |
| `__next` root element              | Present |

**Key Observations:**

1. **No hydration mismatch**: React successfully hydrated without any "Text content does not match" or "Hydration failed" errors
2. **Complete URL rewriting**: All 120 navigation links correctly point to the proxy host
3. **RSC data preserved**: All 223 RSC Flight entries present in `self.__next_f` array
4. **Cross-browser compatibility**: Identical behavior in Chrome (Chromium) and Firefox

---

## Compression Pipeline with Post-Processing

Post-processing requires access to uncompressed UTF‑8 HTML, but the trusted server still preserves the origin `Content-Encoding` on the wire.

End-to-end flow:

1. `StreamingPipeline` decompresses the origin body based on `Content-Encoding`
2. The HTML processor runs `lol_html` rewriting and (optionally) integration post-processors on the complete HTML
3. `StreamingPipeline` recompresses to the original encoding

Because post-processing runs inside the HTML processor (before recompression), `publisher.rs` does not need to special-case compression for integrations.

**Implementation:** Post-processing entry point at [html_processor.rs:20](crates/common/src/html_processor.rs#L20)

---

## Implementation Files

| File                                         | Purpose                                       |
| -------------------------------------------- | --------------------------------------------- |
| `crates/common/src/integrations/nextjs.rs`   | RSC rewriting logic, post-processor           |
| `crates/common/src/integrations/registry.rs` | `IntegrationHtmlPostProcessor` trait          |
| `crates/common/src/integrations/mod.rs`      | Module exports                                |
| `crates/common/src/html_processor.rs`        | HTML rewriting + post-processor invocation    |
| `crates/common/src/publisher.rs`             | Response routing + streaming pipeline setup   |
| `crates/common/src/streaming_processor.rs`   | Compression transforms + `StreamProcessor`    |

### Key Functions in nextjs.rs

| Function                                       | Line                                                   | Purpose                                              |
| ---------------------------------------------- | ------------------------------------------------------ | ---------------------------------------------------- |
| `extract_rsc_push_payload`                     | [229](crates/common/src/integrations/nextjs.rs#L229)   | Extract string from `self.__next_f.push([1, '...'])` |
| `calculate_unescaped_byte_length`              | [606](crates/common/src/integrations/nextjs.rs#L606)   | Count unescaped bytes with escape handling           |
| `consume_unescaped_bytes`                      | [683](crates/common/src/integrations/nextjs.rs#L683)   | Advance through string consuming N bytes             |
| `find_tchunks`                                 | [764](crates/common/src/integrations/nextjs.rs#L764)   | Find T-chunks in single script                       |
| `rewrite_rsc_url_string`                       | [800](crates/common/src/integrations/nextjs.rs#L800)   | URL rewriting with escape handling                   |
| `rewrite_rsc_tchunks`                          | [830](crates/common/src/integrations/nextjs.rs#L830)   | Single-script T-chunk processing                     |
| `consume_unescaped_bytes_skip_markers`         | [907](crates/common/src/integrations/nextjs.rs#L907)   | Advance through string, skipping markers             |
| `calculate_unescaped_byte_length_skip_markers` | [988](crates/common/src/integrations/nextjs.rs#L988)   | Count unescaped bytes, excluding markers             |
| `find_tchunks_with_markers`                    | [1002](crates/common/src/integrations/nextjs.rs#L1002) | Find T-chunks in marker-combined content             |
| `rewrite_rsc_scripts_combined`                 | [1053](crates/common/src/integrations/nextjs.rs#L1053) | Cross-script T-chunk processing                      |
| `find_rsc_push_scripts`                        | [1162](crates/common/src/integrations/nextjs.rs#L1162) | Find all RSC scripts in HTML                         |
| `post_process_rsc_html`                        | [1242](crates/common/src/integrations/nextjs.rs#L1242) | Complete HTML post-processing                        |

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

The RSC rewriting process involves carefully deconstructing RSC payloads, rewriting URLs, and reconstructing them with correct T-chunk lengths. The main entry point is `post_process_rsc_html()` at [nextjs.rs:1242](crates/common/src/integrations/nextjs.rs#L1242).

### Step 1: Find RSC Push Scripts

Find all `self.__next_f.push([1, "..."])` scripts in the HTML and extract their payloads.

**Implementation:** `find_rsc_push_scripts()` at [nextjs.rs:1162](crates/common/src/integrations/nextjs.rs#L1162)

### Step 2: Combine Payloads with Markers

Join all payloads with a marker string (`\x00SPLIT\x00`) that cannot appear in valid JSON/RSC content. This allows T-chunks to be processed across script boundaries while preserving the ability to split back later.

**Implementation:** Marker constant at [nextjs.rs:903](crates/common/src/integrations/nextjs.rs#L903), combining logic in `rewrite_rsc_scripts_combined()` at [nextjs.rs:1053](crates/common/src/integrations/nextjs.rs#L1053)

### Step 3: Find T-Chunks Across Combined Content

Parse T-chunk headers (`ID:T<hex_length>,`) and consume exactly the declared number of unescaped bytes, skipping over markers.

**Implementation:** `find_tchunks_with_markers()` at [nextjs.rs:1002](crates/common/src/integrations/nextjs.rs#L1002), using `consume_unescaped_bytes_skip_markers()` at [nextjs.rs:907](crates/common/src/integrations/nextjs.rs#L907)

### Step 4: Rewrite URLs in T-Chunk Content

Rewrite all URL patterns in the T-chunk content:

- `https://origin.example.com/path` → `http://proxy.example.com/path`
- `//origin.example.com/path` → `//proxy.example.com/path`
- `\\/\\/origin.example.com` → `\\/\\/proxy.example.com` (JSON-escaped)
- `\\\\//origin.example.com` → `\\\\//proxy.example.com` (double-escaped)

**Implementation:** `rewrite_rsc_url_string()` at [nextjs.rs:800](crates/common/src/integrations/nextjs.rs#L800)

### Step 5: Recalculate T-Chunk Length

Calculate the new unescaped byte length (excluding markers) and update the T-chunk header with the new hex length.

**Implementation:** `calculate_unescaped_byte_length_skip_markers()` at [nextjs.rs:988](crates/common/src/integrations/nextjs.rs#L988)

### Step 6: Split Back on Markers

Split the combined rewritten content back into individual payloads on the marker boundaries. Each payload corresponds to one original script, with T-chunk lengths now correct across script boundaries.

**Implementation:** Part of `rewrite_rsc_scripts_combined()` at [nextjs.rs:1053](crates/common/src/integrations/nextjs.rs#L1053)

### Step 7: Reconstruct HTML

Replace each original script with its rewritten version in the HTML.

**Implementation:** Part of `post_process_rsc_html()` at [nextjs.rs:1242](crates/common/src/integrations/nextjs.rs#L1242)

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

| Aspect                | Old (Whitespace Padding)     | New (T-Chunk Length Recalculation)         |
| --------------------- | ---------------------------- | ------------------------------------------ |
| T-chunk handling      | Broken - lengths not updated | Correct - lengths recalculated             |
| URL length change     | Limited to shorter URLs      | Any length change supported                |
| Escape sequences      | Not properly counted         | Fully supported                            |
| Cross-script T-chunks | Not handled                  | Handled via post-processing                |
| Implementation        | Simple regex replace         | Full T-chunk parsing + post-processing     |
| Architecture          | Hardcoded in processor       | Integration hook pattern                   |
| Extensibility         | None                         | Other integrations can add post-processors |

---

## Conclusion

RSC hydration requires **correct T-chunk byte lengths**. The trusted server solves this with two stages:

### Stage 1: Streaming HTML rewrite

- Run `lol_html` rewriting (attributes + integration script rewriters)
- Skip `__next_f.push` payload scripts (handled in stage 2)

### Stage 2: End-of-document post-processing (cross-script)

- After streaming completes for the full HTML document
- Combine scripts with markers
- Recalculate T-chunk lengths across boundaries
- Rewrite URLs in RSC payloads safely across script boundaries

The key insights are:

1. **T-chunk lengths must match content**: The RSC parser uses declared lengths to navigate
2. **T-chunks can span scripts**: Next.js streaming splits content arbitrarily
3. **Markers enable cross-script processing**: Combine, process, split back
4. **Integration hooks enable extensibility**: Other integrations can add post-processors

---

## References

- React Flight Protocol: Internal React implementation for RSC streaming: https://github.com/vercel/next.js/tree/v14.2.35
- Next.js App Router: https://nextjs.org/docs/app
- lol_html: https://github.com/nicksrandall/lol-html (streaming HTML rewriter)
- Implementation: `crates/common/src/integrations/nextjs.rs`
