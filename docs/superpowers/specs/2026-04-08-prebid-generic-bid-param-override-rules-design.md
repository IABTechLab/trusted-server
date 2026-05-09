# Generic Prebid Bid Param Override Rules Design

## Problem

`prebid` currently supports two specialized bidder-param override shapes:

- `bid_param_overrides` for unconditional per-bidder overrides
- `bid_param_zone_overrides` for per-bidder, per-zone overrides

This is ergonomic for today's use cases, but it does not scale well. Every new
override dimension would require:

1. a new config field such as `bid_param_country_overrides`
2. new normalization and validation code
3. new runtime application wiring

That creates two problems:

- the config surface fragments into many `bid_param_*_overrides` sections
- adding a new override type requires code changes even when the desired
  behavior is conceptually "just another condition"

## Goals

- Replace the specialized runtime override implementations with one generic
  ordered rule engine.
- Preserve the existing operator-friendly config shape for
  `bid_param_overrides` and `bid_param_zone_overrides`.
- Introduce one canonical config format for future overrides:
  `bid_param_override_rules`.
- Keep current override semantics:
  - shallow merge into bidder params
  - deterministic last-write-wins precedence
  - exact string matching only in v1
- Fail fast on invalid config.

## Non-Goals

- Arbitrary JSON-path or expression matching in config.
- Deep JSON merge semantics.
- Immediate removal or deprecation of existing compatibility fields.
- Zero-code support for entirely new fact sources. This design makes override
  combinations config-driven, but adding a brand-new matcher dimension still
  requires exposing that fact in code.

## Proposed Configuration Model

### Compatibility Fields

Retain these existing fields:

- `integrations.prebid.bid_param_overrides`
- `integrations.prebid.bid_param_zone_overrides`

They remain supported because they are natural and concise for the two existing
use cases.

### Canonical Field

Add a new ordered rule list:

```toml
[[integrations.prebid.bid_param_override_rules]]
when.bidder = "criteo"
set = { networkId = 99999, pubid = "server-pub" }

[[integrations.prebid.bid_param_override_rules]]
when.bidder = "kargo"
when.zone = "header"
set = { placementId = "_abc" }
```

This becomes the preferred long-term configuration surface. The compatibility
fields are normalized into the same internal rule list before runtime use.

## Rule Semantics

Each rule has:

- `when`: structured matchers
- `set`: a non-empty JSON object shallow-merged into bidder params

Semantics for v1:

- all populated matchers in one rule are ANDed together
- matching is exact equality only
- rules are evaluated in order
- later rules win on overlapping keys because merge is shallow and
  last-write-wins

Example:

```toml
[[integrations.prebid.bid_param_override_rules]]
when.bidder = "kargo"
when.zone = "header"
set = { placementId = "_zone_default", extra = "a" }

[[integrations.prebid.bid_param_override_rules]]
when.bidder = "kargo"
when.zone = "header"
set = { placementId = "_zone_override" }
```

Effective result:

- `placementId` becomes `"_zone_override"`
- `extra` remains `"a"`

## Matcher Vocabulary

### V1 Matchers

Support these fields in `when`:

- `bidder`
- `zone`

Both are strings.

### Future Matchers

The runtime engine should be structured so additional typed matchers can be
added later without redesigning the system, for example:

- `country`
- `region`
- `slot_id`
- `publisher_domain`
- forwarded auction context keys

These are intentionally not part of v1.

## Runtime Architecture

### Config Types

Add new config structs in `prebid.rs`:

- `BidParamOverrideRule`
- `BidParamOverrideWhen`

Suggested shape:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BidParamOverrideRule {
    pub when: BidParamOverrideWhen,
    pub set: Json,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BidParamOverrideWhen {
    pub bidder: Option<String>,
    pub zone: Option<String>,
}
```

`set` remains `Json` at the config boundary to preserve TOML/env flexibility,
but runtime normalization should require it to be a JSON object.

### Internal Engine

Replace the current split runtime override application with one internal engine,
for example:

```rust
struct BidParamOverrideEngine(Vec<CompiledBidParamOverrideRule>);

struct CompiledBidParamOverrideRule {
    bidder: Option<String>,
    zone: Option<String>,
    set: serde_json::Map<String, Json>,
}

struct BidParamOverrideFacts<'a> {
    bidder: &'a str,
    zone: Option<&'a str>,
}
```

The engine applies rules against facts gathered at the existing bidder-param
assembly point in `to_openrtb`.

### Normalization

Normalize all config inputs into one ordered rule list when constructing the
integration or lazily on first use.

Normalization order:

1. rules derived from `bid_param_overrides`
2. rules derived from `bid_param_zone_overrides`
3. explicit `bid_param_override_rules`

This preserves current behavior while allowing canonical explicit rules to
override compatibility-derived rules intentionally.

### Compatibility Normalization Examples

This compatibility config:

```toml
[integrations.prebid.bid_param_overrides.criteo]
networkId = 99999

[integrations.prebid.bid_param_zone_overrides.kargo]
header = { placementId = "_abc" }
```

normalizes to the internal equivalent of:

```toml
[[integrations.prebid.bid_param_override_rules]]
when.bidder = "criteo"
set = { networkId = 99999 }

[[integrations.prebid.bid_param_override_rules]]
when.bidder = "kargo"
when.zone = "header"
set = { placementId = "_abc" }
```

## Validation Rules

Validation should happen during settings parsing or integration construction,
before any live request processing.

Validation requirements:

- `set` must be a non-empty JSON object
- unknown `when.*` fields must fail parsing
- `when.bidder` and `when.zone` must be non-empty strings
- empty `when` should be rejected in v1 to avoid accidental global rules
- compatibility-derived rules must be validated through the same compiled-rule
  path as explicit rules

Failing invalid config early is preferred to silently ignoring malformed rules.

## Behavior in `to_openrtb`

Current behavior:

- bidder params are expanded
- static overrides are applied
- zone overrides are applied

New behavior:

- bidder params are expanded
- one rule engine is applied using facts built from the current request context

For v1, the facts are:

- bidder name
- zone from `trustedServer` bidder params / `mediaTypes.banner.name`

This preserves the current request-time behavior while eliminating the
specialized override application paths.

## Migration Strategy

Short term:

- keep `bid_param_overrides` and `bid_param_zone_overrides`
- add `bid_param_override_rules`
- document `bid_param_override_rules` as the preferred canonical format

Medium term:

- gather real operator usage of the canonical form
- decide later whether to deprecate the compatibility fields

This avoids forcing operators to migrate immediately while allowing new use
cases to adopt the generic rules format now.

## Testing Strategy

### Config Parsing

- parse explicit `bid_param_override_rules`
- reject unknown matcher fields
- reject empty `set`
- reject non-object `set`
- reject empty `when.bidder` / `when.zone`

### Normalization

- `bid_param_overrides` normalizes to bidder-only rules
- `bid_param_zone_overrides` normalizes to bidder-plus-zone rules
- mixed compatibility and canonical rules preserve normalization order

### Runtime Evaluation

- bidder-only rule applies when bidder matches
- bidder-plus-zone rule applies when both match
- rule does not apply when zone is absent
- later rule overrides earlier rule on overlapping keys
- non-overlapping keys are preserved through shallow merge

### Env Overrides

- JSON env override for `bid_param_override_rules` parses correctly
- compatibility env overrides continue to parse correctly

## Risks

### Compatibility Drift

If compatibility normalization does not exactly mirror current semantics,
existing configs may change behavior subtly. This is mitigated by preserving the
current ordering and adding mixed-config regression tests.

### Ambiguous Precedence

Introducing multiple config inputs can create confusion if precedence is not
explicit. This is mitigated by documenting and testing the fixed normalization
order.

### Misinterpreting "Generic"

This design makes override rules generic. It does not make matcher dimensions
fully dynamic. Adding a brand-new matcher like `country` still requires exposing
that fact in code. This is an intentional tradeoff for production-grade typed
validation and predictable behavior.

## Files Expected to Change During Implementation

- `crates/trusted-server-core/src/integrations/prebid.rs`
- `trusted-server.toml`
- `crates/trusted-server-core/src/settings.rs` tests, if env override coverage
  needs expansion

No other subsystems should be required for the initial implementation.
