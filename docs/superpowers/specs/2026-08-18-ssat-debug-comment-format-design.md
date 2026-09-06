# SSAT Debug Comment Output Format Design

**Date:** 2026-08-18

**Status:** Approved for implementation planning

**Related PR:** [IABTechLab/trusted-server#943](https://github.com/IABTechLab/trusted-server/pull/943)

## Summary

The configurable SSAT `ts-debug` comment currently serializes its auction dump
as compact JSON. Full-verbosity dumps can contain bids, creatives, provider
metadata, and PBS HTTP-call diagnostics, making the single-line payload hard to
navigate in page source.

Add a `format` option to `[debug.auction_html_comment_options]` with two values:

- `compact` (default): preserve the existing one-line JSON representation.
- `pretty`: serialize the outer auction dump as indented JSON.

Pretty formatting does not parse or transform JSON-looking strings such as PBS
`requestbody` and `responsebody`. Those values remain byte-for-byte equivalent
JSON string values so the comment continues to represent what the provider
integration captured rather than a guessed interpretation of it.

## Goals

1. Make the outer provider, bid, metadata, and HTTP-call hierarchy easier to
   navigate in HTML source during local debugging.
2. Preserve today's compact output unless an operator explicitly opts in.
3. Preserve the existing dump schema and the exact contents of nested strings.
4. Keep the existing security and size protections independent of formatting.

## Non-goals

- Recursively parsing or formatting `requestbody`, `responsebody`, creative
  markup, or any other string that happens to contain JSON.
- Adding a browser UI, CLI viewer, downloadable artifact, or syntax highlighting.
- Changing verbosity, redaction, section toggles, metadata selection, creative
  truncation, or provider response capture.
- Making the 256 KiB total dump cap configurable.

## Configuration

Extend `AuctionDebugCommentOptions` with an enum-backed field:

```rust
#[serde(default)]
pub format: AuctionDebugCommentFormat,

#[derive(Debug, Clone, Copy, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuctionDebugCommentFormat {
    #[default]
    Compact,
    Pretty,
}
```

The hand-written `Default` implementation for `AuctionDebugCommentOptions`
sets `format` to `Compact`, matching serde's default. Unknown strings fail
configuration loading because serde rejects enum variants other than `compact`
and `pretty`; the enum deliberately has no catch-all variant.

Example:

```toml
[debug.auction_html_comment_options]
include_provider_responses = true
include_mediator_response = false
include_bids = false
metadata_keys = ["error_type", "http_status", "message"]
verbosity = "full"
format = "pretty"
```

`trusted-server.example.toml` documents both accepted values and keeps
`format = "compact"` in the example to make the default visible.

## Rendering and Data Flow

`prepend_auction_debug_comment` continues building the same
`serde_json::Value::Object`. Only the serialization selection changes:

- `Compact` uses `serde_json::to_string`.
- `Pretty` uses `serde_json::to_string_pretty`.

The serialized string then passes through the existing processing in the same
order:

1. Neutralize HTML comment terminators (`-->` and `--!>`).
2. Apply the unconditional 256 KiB total serialized dump cap.
3. Place the result after `dump=` in the `ts-debug` HTML comment.

Pretty mode intentionally introduces line breaks after `dump=`. The comment's
summary line remains unchanged, and both modes remain valid JSON whenever the
total cap is not reached.

Pretty printing increases serialized size, so it may reach the existing cap
sooner than compact output. If capped, the dump may end mid-JSON exactly as it
can today; the existing `…(truncated N bytes)` marker continues to make that
condition explicit.

## Compatibility and Safety

- Existing configurations that omit `format` produce byte-for-byte-equivalent
  compact JSON output.
- Formatting does not change which fields are included or expose data hidden by
  `redacted` or `upstream` verbosity.
- Nested request and response bodies remain strings. Pretty mode adds whitespace
  only to the outer serialized representation; deserializing an uncapped dump
  produces the same JSON value as compact mode.
- Comment-terminator neutralization and the total cap apply in both modes.
- The existing warning remains: `upstream` and `full` output can expose identity
  and request data and must not be enabled in production.

## Testing

Follow test-driven development with focused tests before production changes:

1. Settings deserialization accepts `format = "pretty"` and defaults to
   `Compact` when omitted.
2. An invalid format string fails deserialization.
3. Compact rendering retains the existing one-line `dump={...}` representation.
4. Pretty rendering contains indented outer JSON and deserializes to the same
   value as compact rendering for the same auction result.
5. A nested JSON `requestbody` remains a JSON string rather than becoming an
   object in pretty mode.
6. Pretty rendering still neutralizes every tested HTML-comment terminator.
7. Pretty rendering still respects the 256 KiB total dump cap and emits the
   existing truncation marker.

After focused tests pass, run the repository-required Fastly/core test suite,
format check, and target-matched clippy checks relevant to the changed Rust
core code.

## Future Direction

If operators need decoded navigation of nested provider bodies, add a separate
local inspection tool that extracts the comment and conditionally parses known
JSON body fields for display. Keeping that transformation outside the emitted
comment preserves raw capture fidelity and avoids increasing every debug page's
payload.
