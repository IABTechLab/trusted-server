# RSC Hydration URL Rewriting

This guide explains how Trusted Server rewrites URLs inside Next.js React
Server Components (RSC) payloads for App Router sites.

## Overview

Next.js App Router streams RSC data either inline in HTML via
`self.__next_f.push(...)` or as `text/x-component` responses. Those payloads can
contain absolute URLs that must be rewritten to the publisher's first-party
domain.

## Why This Is Tricky

- Flight payloads mix JSON rows and length-delimited "T" chunks.
- Changing bytes inside a "T" chunk requires recalculating its declared length.
- Payloads can be split across multiple script tags during streaming.

## Trusted Server Approach

Trusted Server uses a two-phase pipeline:

1. Streaming HTML rewrite for standard attributes and self-contained payloads.
2. Post-processing for cross-script payloads that span multiple chunks.

## Where This Runs

- HTML responses: streaming rewrite with optional post-processing.
- RSC Flight responses (`text/x-component`): direct Flight parsing and rewrite.

## Internals

### Background: How Next.js Delivers RSC Data

Next.js App Router streams RSC data using a "Flight" protocol. On initial page
loads, the payload is embedded in HTML via inline `<script>` tags that call
`self.__next_f.push()`:

```html
<script>
  self.__next_f.push([0])
</script>
<script>
  self.__next_f.push([1, '0:[[...RSC content...]]'])
</script>
<script>
  self.__next_f.push([1, '1a:T29,{"url":"https://origin.example.com"}'])
</script>
```

For client-side navigations, Next.js fetches Flight directly (no `<script>`
wrapper) and expects `content-type: text/x-component`. The response body is the
same row format, but without JSON/JS-string escaping.

### RSC Flight Protocol Format

The Flight stream is framed as rows. Most rows end with `\n`, but `T` rows are
length-delimited and do not end with a newline.

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

### Authoritative Parsing Rules

The React client parser frames the stream as:

1. Read hex `ID` until `:`
2. Read one byte to determine framing:
   - `T`: read hex length until `,`, then read exactly N raw bytes (no newline)
   - `A`-`Z`: read until `\n`
   - anything else (`[`, `{`, `"`, `t`, `f`, `n`, digits...): treat as JSON row
     content and read until `\n`

The key implication: changing bytes inside a `T` row requires updating its hex
length prefix.

### The Critical T-Chunk Format

T-chunks include an explicit byte length:

```
1a:T29,{"url":"https://origin.example.com/path"}
|  | |  \- Content (exactly 41 unescaped bytes)
|  | \- Comma separator
|  \- Length in hex (0x29 = 41 bytes)
\- Chunk ID
```

The hex length is the unescaped byte count. Because the RSC content is embedded
in a JavaScript string, escape sequences must be counted correctly.

| Escape Sequence  | String Chars | Unescaped Bytes    |
| ---------------- | ------------ | ------------------ |
| `\\n`            | 2            | 1                  |
| `\\r`            | 2            | 1                  |
| `\\t`            | 2            | 1                  |
| `\\\\`           | 2            | 1                  |
| `\\\"`           | 2            | 1                  |
| `\\xHH`          | 4            | 1                  |
| `\\uHHHH`        | 6            | 1-3 (UTF-8 bytes)  |
| `\\uD800\\uDC00` | 12           | 4 (surrogate pair) |

### T-Chunks Can Span Multiple Push Scripts

Next.js streams RSC data, so a T-chunk header can appear in one `<script>`
tag while its content continues in later tags:

```html
<!-- Script 10: T-chunk header only -->
<script>
  self.__next_f.push([1, '11:null\\n1a:T928,'])
</script>

<!-- Script 11: T-chunk content (2344 unescaped bytes) -->
<script>
  self.__next_f.push([1, '...2344 bytes of actual content...'])
</script>
```

This is why URL rewriting must be T-row aware and cross-script aware.

### Two-Phase Processing

Trusted Server uses a two-phase approach for App Router payloads:

1. Streaming HTML processing:
   - Rewrite standard HTML attributes (`href`, `src`, `srcset`, etc.)
   - Run integration script rewriters for self-contained payloads
   - Capture complete `__next_f.push([1,"..."])` payload strings into
     placeholders for post-processing
2. HTML post-processing:
   - If placeholders were captured, rewrite the recorded payloads and replace
     them without re-parsing HTML
   - If payloads were fragmented, re-parse the HTML to find the push payloads
     and rewrite them in place

### Marker-Based Cross-Script Processing

1. Combine all payload strings using a marker delimiter that cannot appear in
   valid JSON/RSC content (`\x00SPLIT\x00`).
2. Scan the combined stream for `ID:T<hex_length>,` headers and consume exactly
   N unescaped bytes to find each T-chunk boundary, skipping markers.
3. Rewrite URLs and recalculate unescaped byte lengths.
4. Split the combined content back on the marker to recover per-script payloads.

### Byte Length Calculation Algorithm

Trusted Server accounts for JavaScript escape sequences and UTF-8 byte counts:

- Shared escape sequence iterator handles standard JS escapes (including
  `\\n`, `\\r`, `\\t`, `\\b`, `\\f`, `\\v`, `\\'`, `\\\"`, `\\\\`, `\\/`,
  `\\xHH`, `\\uHHHH`, and surrogate pairs).
- Counts unescaped bytes for each T-chunk.
- Consumes exactly N unescaped bytes to locate chunk boundaries.
- Enforces safety limits (skip rewrite on unrealistic sizes).

### URL Rewriting Patterns

The rewrite logic handles multiple URL formats in RSC content:

| Pattern              | Example                   | In RSC String             |
| -------------------- | ------------------------- | ------------------------- |
| Full HTTPS           | `https://host/path`       | `https://host/path`       |
| Full HTTP            | `http://host/path`        | `http://host/path`        |
| Protocol-relative    | `//host/path`             | `//host/path`             |
| Bare host (boundary) | `origin.example.com/path` | `origin.example.com/path` |

### Implementation References

The current implementation lives in:

- `crates/common/src/integrations/nextjs/rsc.rs`
- `crates/common/src/rsc_flight.rs`

## Related Docs

- [Next.js Integration](/guide/integrations/nextjs)
- [Integration Guide](/guide/integration-guide)
