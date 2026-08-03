# PR #957 Review Resolution Design

**Date:** 2026-08-03

**Status:** Approved

**PR:** [IABTechLab/trusted-server#957](https://github.com/IABTechLab/trusted-server/pull/957)

## Summary

Resolve the outstanding review feedback on per-section `gam_unit_path`
templates without changing the behavior of legacy static or absent paths. The
changes make dynamic-template rollbacks fail loudly on older binaries, validate
`gam_network_id` only when it is consumed, bound request-controlled rendering,
and correct the documentation and small API issues identified during review.

## Goals

1. A config blob containing any dynamic placeholder must be rejected by the
   legacy schema rather than accepted with placeholder braces rendered
   literally.
2. A blank `gam_network_id` must remain valid when every configured slot uses
   an explicit static path that does not consume it.
3. A request path must not amplify repeated `{section}` placeholders into an
   unbounded allocation.
4. Initial and SPA rendering must continue to use the same bounded slot-building
   path.
5. Documentation and visibility must describe the implemented behavior
   accurately.

## Non-goals

- Introduce a public template-version setting.
- Change the serialized shape of static or absent `gam_unit_path` configs.
- Change the supported placeholder set.
- Change section casing. Google Ad Manager documents ad-unit codes as
  case-insensitive, so the review claim that casing routes to different
  inventory does not justify a behavior change.
- Refactor creative-opportunity configuration outside the reviewed code.

## Design

### Automatic rollback marker

`CreativeOpportunitiesConfig::compile_unit_templates` continues to parse and
cache every explicit template. After parsing, it detects whether any slot uses
at least one dynamic placeholder. If so, and `section_segment` is absent, it
materializes the existing default as `Some(0)`.

The typed CLI deserializes `TrustedServerAppConfig` through
`Settings::finalize_deserialized`, which runs runtime preparation before the
wrapper is serialized for a config push. The materialized `section_segment`
therefore appears in every pushed dynamic-template blob. A binary predating
`section_segment` rejects that blob through its `deny_unknown_fields` schema.

Static templates and slots without `gam_unit_path` do not materialize the
marker, so their serialized shape remains compatible with the legacy schema.
An existing nonzero `section_segment` remains unchanged.

Using the existing field is preferable to a new
`gam_unit_path_template_version` field: it creates the required fail-loud
behavior with no additional public configuration surface, and `Some(0)` is
semantically identical to the existing default.

### Scoped network-ID validation

Add a `template_uses_network_id` helper parallel to
`template_uses_section`. It checks the compiled cache when present and parses
the raw template as a fallback when the cache is absent.

`gam_network_id` is required only when at least one slot either:

- omits `gam_unit_path`, which uses the `/<network_id>/<slot_id>` default; or
- contains `{network_id}`.

Explicit static paths and templates using only `{slot_id}` and/or `{section}`
do not consume the network ID and remain valid when it is blank.

### Bounded dynamic rendering

Define a 100-character limit for a rendered dynamic GAM unit path, matching the
documented Google Ad Manager ad-unit-code limit. The compatibility promise for
pre-existing static and absent paths takes precedence, so those two legacy
cases keep their current string-returning behavior even if their configured
values exceed the new dynamic limit.

Section sanitization keeps at most 100 ASCII output characters. This bounds the
first request-controlled allocation and is safe at byte boundaries because the
sanitized output is ASCII-only.

Before allocating a rendered dynamic path, the renderer computes the exact
length of literals and substituted values with checked arithmetic. If addition
overflows or the total exceeds 100 characters, rendering returns `None`.
Otherwise it allocates once with the computed capacity and appends each part.

Startup validation renders each dynamic template with `section_root` when it
uses `{section}`, or with an empty section when it does not. This rejects
configurations whose fixed values or repeated root substitution already exceed
the limit. A non-root request can still derive a longer section; that request's
slot is recoverably omitted before the final path allocation.

The raw-template fallback applies the same checked renderer, so callers that
construct or deserialize slots without runtime preparation cannot bypass the
bound. Malformed templates remain a startup error in the normal finalized
settings path; the existing raw fallback remains only for compatibility with
direct callers.

### Publisher data flow

Change `build_slot_json` to accept a derived `&str` section and return
`Option<serde_json::Value>`. Both production callers derive the section once
per request and use `filter_map` while constructing the slot list. An
over-limit dynamic path therefore omits only that slot; it does not fail the
page response or SPA endpoint.

This preserves the existing single slot-wire-shape implementation shared by
initial and SPA rendering while removing repeated section allocation per slot.

### Focused cleanup and documentation

- Extract `is_section_char` and use it in both sanitization and
  `section_root` validation.
- Make `derive_section` private because only `section_for_path` and same-module
  tests use it.
- Correct `section_root` rustdoc to refer to the configured segment rather than
  the first segment.
- Clarify that raw-template fallback enforces placeholder-dependent validation,
  while compilation is still required to reject malformed templates.
- Update the placeholder table, validation wording, changelog, and rollback
  guidance to describe the automatic compatibility marker.
- Prepare a sourced technical reply instead of changing casing behavior.

## Error handling

- Malformed templates, invalid roots, blank consumed network IDs, and dynamic
  paths that are already over limit with configured values fail settings
  preparation with the existing configuration error flow.
- A request-derived over-limit dynamic path returns `None` and omits that slot
  from the generated JSON. No partial path reaches `googletag.defineSlot`.
- Checked arithmetic prevents integer overflow before allocation.

## Testing

Implementation follows test-driven development.

1. Add a push-shaped test that deserializes/finalizes
   `TrustedServerAppConfig`, serializes it, and passes the creative-opportunity
   value to a test-only legacy schema with `deny_unknown_fields`:
   - `{network_id}` and `{slot_id}` templates are rejected because the automatic
     marker is present;
   - static and absent paths are accepted because the marker is absent.
2. Verify blank network IDs are accepted for explicit static paths and rejected
   for absent paths or `{network_id}` templates, including raw-cache fallback.
3. Verify section sanitization is capped at 100 ASCII characters.
4. Verify checked rendering handles repeated placeholders, rejects overflow or
   over-limit output before allocation, and preserves static/absent behavior.
5. Verify publisher slot building omits only an over-limit dynamic slot and that
   initial/SPA paths continue to share the same section and slot builder.
6. Run core/adapter tests, formatting, clippy, documentation formatting, and the
   repository's full CI-equivalent verification before handoff.

## Acceptance criteria

- [ ] Every pushed dynamic-template blob fails legacy-schema deserialization.
- [ ] Pushed static and absent-path blobs remain legacy-schema compatible.
- [ ] Blank `gam_network_id` is rejected exactly when a rendered slot consumes
      it.
- [ ] Dynamic rendered length is checked before final allocation and cannot
      exceed 100 characters.
- [ ] Request-time overflow omits the affected slot without failing the
      response.
- [ ] Static and absent paths retain their pre-template behavior.
- [ ] All sound review comments are implemented and the casing comment has a
      sourced technical response.
- [ ] Relevant tests and CI-equivalent checks pass.
