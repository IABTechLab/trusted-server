# Technical Specification: Origin Cache-Header Audit (`ts dev audit headers`)

**Status:** Draft
**Author:** @vasujai
**Epic:** [#834](https://github.com/IABTechLab/trusted-server/issues/834)
**Planning task:** [#835](https://github.com/IABTechLab/trusted-server/issues/835)
**Related:** [#293](https://github.com/IABTechLab/trusted-server/issues/834) (cache header refactoring), [#428](https://github.com/IABTechLab/trusted-server/issues/428) (ETag multi-value), PR #860
**Last updated:** 2026-07-14

---

## 1. Overview

Trusted Server serves many content types through one edge hostname -- HTML, JS bundles, creatives/images, static assets, RTB/JSON -- each with a different optimal caching posture. Today TS mostly passes origin cache directives through untouched and exposes no diagnostics, so publishers can't tell whether each content type is cached correctly.

This spec defines `ts dev audit headers`, a CLI command that audits origin response cache directives grouped by content type and returns a per-type pass/warn/fail verdict naming the responsible header and recommending the correct value.

**Out of scope:** Auto-fixing headers, request-side cache-key/hit-ratio analysis, and the `ts dev proxy` tool.

---

## 2. Command Surface

```
ts dev audit headers [OPTIONS] [URLS...]
```

### Arguments

| Argument | Description |
|----------|-------------|
| `URLS...` | Explicit URLs to audit (optional) |

### Options

| Flag | Description | Default |
|------|-------------|---------|
| `--config <path>` | Path to `trusted-server.toml` | `./trusted-server.toml` |
| `--origin <url>` | Override origin URL (skips config lookup) | from config `publisher.origin_url` |
| `--json` | Machine-readable JSON output | human table |

### Exit codes

| Code | Meaning |
|------|---------|
| 0 | All groups pass |
| 1 | At least one group has a FAIL verdict |
| 2 | Warnings only (no fails) |

---

## 3. Content-Type Taxonomy

Responses are classified into groups by their `Content-Type` response header:

| Group | Matching Content-Types |
|-------|----------------------|
| `Html` | `text/html` |
| `JavaScript` | `application/javascript`, `text/javascript`, `application/x-javascript` |
| `Image` | `image/*` |
| `StaticAsset` | `text/css`, `font/*`, `application/font-*`, `application/woff*` |
| `RtbJson` | `application/json` |
| `Other` | Everything else (not evaluated, reported as info) |

---

## 4. Cacheability Rules

Each content-type group has an expected caching posture. Rules are evaluated against: `Cache-Control`, `Surrogate-Control`, `Surrogate-Key`, `Vary`, `ETag`.

### 4.1 HTML (personalized/consent-sensitive)

| Header | Expected | Verdict if wrong |
|--------|----------|-----------------|
| `Cache-Control` | Contains `private` AND (`no-store` OR `no-cache`) | FAIL: "HTML served without private/no-store risks sharing personalized content across users" |
| `Vary` | Should NOT contain `*` | WARN: "Vary: * disables all caching including CDN" |
| `Surrogate-Control` | If present, `no-store` or `private` | FAIL: "CDN may cache personalized HTML" |

### 4.2 JavaScript bundles (hashed filenames, immutable)

| Header | Expected | Verdict if wrong |
|--------|----------|-----------------|
| `Cache-Control` | Contains `public` AND `max-age>=31536000` AND `immutable` | WARN: "Hashed JS bundles should use long-lived immutable caching" |
| `ETag` | Present (strong preferred) | WARN: "Missing ETag means no conditional revalidation" |
| `Surrogate-Key` | Present | WARN: "Without Surrogate-Key, this content type can't be selectively purged from the CDN -- full-cache purges required" |

### 4.3 Images / Creatives

| Header | Expected | Verdict if wrong |
|--------|----------|-----------------|
| `Cache-Control` | Contains `public` AND `max-age>=86400` | WARN: "Images re-fetched too frequently" |
| `Surrogate-Control` | If present, `max-age` should be >= `Cache-Control` max-age | WARN: "CDN caching shorter than browser caching" |
| `Surrogate-Key` | Present | WARN: "Without Surrogate-Key, creatives can't be selectively purged from the CDN -- full-cache purges required" |

### 4.4 Static Assets (CSS, fonts)

| Header | Expected | Verdict if wrong |
|--------|----------|-----------------|
| `Cache-Control` | Contains `public` AND `max-age>=31536000` AND `immutable` | WARN: "Static assets should use long-lived immutable caching" |
| `Surrogate-Key` | Present | WARN: "Without Surrogate-Key, static assets can't be selectively purged from the CDN -- full-cache purges required" |

### 4.5 RTB/JSON (real-time, never cached)

| Header | Expected | Verdict if wrong |
|--------|----------|-----------------|
| `Cache-Control` | Contains `private` AND `no-store` | FAIL: "RTB responses cached = stale bids served to users" |
| `Surrogate-Control` | If present, `no-store` | FAIL: "CDN caching RTB responses" |

### 4.6 ETag handling (ref: #428)

When evaluating ETag presence, handle multi-value `If-None-Match` correctly:
- Accept both strong (`"abc"`) and weak (`W/"abc"`) ETags
- Multiple comma-separated values in `If-None-Match` are valid per RFC 7232

### 4.7 Surrogate-Key evaluation

`Surrogate-Key` (Fastly) enables targeted cache purging by tag. It is evaluated only on
cacheable groups (JavaScript, Image, StaticAsset) -- non-cacheable groups (Html, RtbJson)
skip the check since uncached content never needs purging. Absence is a WARN, never a
FAIL: caching still works without it, but operational purges become all-or-nothing.

---

## 5. URL Discovery

When no explicit URLs are provided:

1. Read `publisher.origin_url` from `trusted-server.toml`
2. Probe well-known paths per content type:
   - HTML: `/`
   - JS: discover from `<script>` tags on `/`, or use `/_ts/` internal paths
   - Images: discover from `<img>` tags on `/`, or probe `/favicon.ico`
   - Static: discover from `<link rel="stylesheet">` on `/`
   - RTB/JSON: probe `/_ts/api/v1/identify` (if configured)

When explicit URLs are provided, skip discovery and classify each by its response `Content-Type`.

---

## 6. Output Format

### 6.1 Human-readable (default)

```
Origin: https://origin.publisher.com

 Content Type  | Type Verdict | Header           | Verdict  | Recommendation
───────────────┼──────────────┼──────────────────┼──────────┼──────────────────────────────────────
 HTML          | ✗ FAIL       | Cache-Control    | ✗ FAIL   | Set `private, no-store`
 HTML          |              | Vary             | ✓ PASS   | --
 JS Bundle     | ✓ PASS       | Cache-Control    | ✓ PASS   | --
 Image         | ⚠ WARN       | Cache-Control    | ⚠ WARN   | Consider `public, max-age=86400`
 RTB/JSON      | ✗ FAIL       | Cache-Control    | ✗ FAIL   | Set `private, no-store`

Summary (per content type): 1 pass, 1 warn, 2 fail (4 types audited)
```

The **Type Verdict** column shows the group-level rollup (worst-of across all header
checks in that group). The summary counts content-type groups, matching the epic's
"per-type pass/warn/fail verdict."

### 6.2 JSON (`--json`)

```json
{
  "origin": "https://origin.publisher.com",
  "groups": [
    {
      "content_type": "Html",
      "urls_sampled": ["https://origin.publisher.com/"],
      "verdict": "Fail",
      "verdicts": [
        {
          "header": "Cache-Control",
          "verdict": "Fail",
          "actual": "public, max-age=3600",
          "expected": "private, no-store",
          "recommendation": "HTML served without private/no-store risks sharing personalized content across users"
        }
      ]
    }
  ],
  "summary": {
    "total_groups": 4,
    "pass": 1,
    "warn": 1,
    "fail": 2
  }
}
```

---

## 7. Architecture

### 7.1 Module structure

New modules in `crates/trusted-server-cli/src/`:

```
dev_audit/
  mod.rs          -- pub entry point: run_audit_headers()
  rules.rs        -- ContentTypeGroup, CachePolicy, evaluate()
  fetch.rs        -- AuditHeadersArgs, origin fetching, classification
  analyze.rs      -- AuditReport, GroupReport, HeaderVerdict, run_analysis()
  output.rs       -- human table + JSON rendering
```

### 7.2 Key types

```rust
pub enum ContentTypeGroup { Html, JavaScript, Image, StaticAsset, RtbJson, Other }
pub enum Verdict { Pass, Warn(String), Fail(String) }

pub struct HeaderVerdict {
    pub header: String,
    pub verdict: Verdict,
    pub actual: Option<String>,
    pub expected: String,
    pub recommendation: String,
}

pub struct GroupReport {
    pub content_type: ContentTypeGroup,
    pub urls_sampled: Vec<String>,
    /// Group-level (per-type) verdict: worst-of rollup over `verdicts`.
    /// Any Fail -> Fail; else any Warn -> Warn; else Pass.
    pub verdict: Verdict,
    pub verdicts: Vec<HeaderVerdict>,
}

pub struct AuditReport {
    pub origin: String,
    pub groups: Vec<GroupReport>,
    pub summary: AuditSummary,
}

/// Counts are per content-type GROUP (the epic's "per-type verdict"),
/// not per header row. pass + warn + fail == total_groups.
pub struct AuditSummary {
    pub total_groups: usize,
    pub pass: usize,
    pub warn: usize,
    pub fail: usize,
}
```

---

## 8. CLI Integration

### 8.1 `ts dev` restructure

`ts dev` is currently a flat command wrapping `fastly compute serve`. It must be restructured into a subcommand tree:

```rust
enum DevCommand {
    Serve(DevServeArgs),      // current behavior (default)
    Audit(DevAuditCommand),
}

enum DevAuditCommand {
    Headers(AuditHeadersArgs),
}
```

`ts dev` (bare, no subcommand) defaults to `Serve` for backward compatibility.

### 8.2 Error handling

Add `CliError::HeaderAudit` variant to `error.rs`.

---

## 9. Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Cacheability rules | Hardcoded in v1 | YAGNI; configurable rules add complexity before we know the right defaults |
| CDN assumption | Fastly-aware (check `Surrogate-Control`) but don't require it | TS is Fastly-first but may run elsewhere |
| URL discovery | Both config-derived and explicit | Config for zero-arg DX, explicit for CI |
| `ts dev` restructure | Default to `serve` | Backward compat; no breaking change |
| Rules source of truth | Align with PR #860 | Don't invent rules that contradict what the runtime sets |

---

## 10. Tasks

### Task 1: Restructure `ts dev` to subcommand tree

**Type:** Refactor (modifies existing code on `feature/ts-cli`)
**Dependencies:** None

Convert `DevArgs` to a subcommand enum. `ts dev` bare defaults to `Serve`. Wire `ts dev audit headers` to a stub handler.

**Acceptance criteria:**
- `ts dev` still launches local dev server
- `ts dev audit headers --help` shows usage
- All existing tests pass

---

### Task 2: Content-type taxonomy + cacheability rules engine

**Type:** Net-new (`dev_audit/rules.rs`)
**Dependencies:** None (parallel with Task 1)

Define `ContentTypeGroup`, classification function, per-group expected cache postures, and `evaluate()` producing pass/warn/fail verdicts.

**Acceptance criteria:**
- Each group has documented expected posture
- `evaluate()` returns per-header verdicts with recommendation text
- Handles multi-value ETag (#428)
- Unit tests cover all classification + evaluation logic

---

### Task 3: Origin request engine

**Type:** Net-new (`dev_audit/fetch.rs`)
**Dependencies:** Task 2 (uses `ContentTypeGroup`)

Fetch responses from origin using `reqwest` (already a dep). Support explicit URL list and config-derived discovery.

**Acceptance criteria:**
- Fetches from origin, classifies by Content-Type
- Handles connection failures, timeouts, non-2xx gracefully
- Supports both explicit URLs and config-derived discovery

---

### Task 4: Header analysis + verdict generation

**Type:** Net-new (`dev_audit/analyze.rs`)
**Dependencies:** Tasks 2 + 3

Group responses by content type, run evaluation, produce `AuditReport`.

**Acceptance criteria:**
- Groups responses correctly
- Per-group, per-header verdicts with summary
- Group-level (per-type) verdict computed as worst-of rollup: any Fail -> Fail, else any Warn -> Warn, else Pass
- Summary counts content-type groups (pass + warn + fail == total_groups)
- Handles missing headers, multiple values, non-standard directives

---

### Task 5: Output formatting

**Type:** Net-new (`dev_audit/output.rs`)
**Dependencies:** Task 4

Terminal table (colored if tty) + `--json` mode + exit code logic.

**Acceptance criteria:**
- Human output readable in terminal
- JSON output valid, stable, machine-parseable
- Exit codes: 0=pass, 1=fail, 2=warn-only

---

### Task 6: End-to-end wiring + integration tests

**Type:** Integration
**Dependencies:** Tasks 1-5

Wire `run_audit_headers()` into dispatch. Integration test with mock HTTP server.

**Acceptance criteria:**
- `ts dev audit headers` runs end-to-end
- Integration test covers pass/warn/fail scenarios
- Error messages are actionable

---

### Task 7: Documentation

**Type:** Docs
**Dependencies:** Task 6

`docs/guide/cache-header-audit.md` + update `docs/guide/cli.md` + clap help text.

**Acceptance criteria:**
- Guide covers all user scenarios with copy-pasteable examples
- `ts dev audit headers --help` is clear and useful

---

## 11. Dependency Graph

```
┌─────────────────┐   ┌─────────────────┐
│ Task 1 (CLI)    │   │ Task 2 (Rules)  │──┐
└────────┬────────┘   └────────┬────────┘  │
         │                     │            │
         │            ┌────────▼────────┐   │
         │            │ Task 3 (Fetch)  │   │
         │            └────────┬────────┘   │
         │                     │            │
         │            ┌────────▼────────┐   │
         │            │ Task 4 (Analyze)│◄──┘
         │            └────────┬────────┘
         │                     │
         │            ┌────────▼────────┐
         │            │ Task 5 (Output) │
         │            └────────┬────────┘
         │                     │
         └──────────┬──────────┘
            ┌───────▼───────┐
            │ Task 6 (E2E)  │
            └───────┬───────┘
                    │
            ┌───────▼───────┐
            │ Task 7 (Docs) │
            └───────────────┘
```

**Parallel tracks:** Tasks 1 and 2 can start immediately and independently.

---

## 12. Open Questions

1. Should `ts dev audit headers` require a running local server, or always hit the remote origin directly?
2. Should we add a `--strict` flag that treats warnings as failures (for CI gates)?
3. Should the URL discovery crawl follow redirects from the origin?
