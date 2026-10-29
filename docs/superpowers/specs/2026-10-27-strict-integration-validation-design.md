# Strict integration configuration validation

## Problem and scope

Integration settings are stored as an open JSON map. Deploy validation reads only
known IDs, and ten integration schemas silently discard unknown fields. A typo
such as `[integrations.prebdi]` or `enable = false` can therefore pass validation
without producing the intended behavior.

This change hardens the current `[integrations.*]` schema. It does not introduce
the separate module-selection configuration redesign.

## Approved behavior

- Config validate and push reject unknown integration IDs, naming them and
  listing the supported IDs from the complete validation inventory.
- Runtime validation warns about unknown IDs and continues, preserving startup
  for previously stored unused tables.
- All known integration config structs reject unknown fields, including disabled
  blocks. Minimal disabled blocks continue to omit enabled-only required fields.
- `tester_cookie` and `tinybird` reject unknown fields as well.
- Integration parse errors include the offending field in the outer error text
  consumed by CLI validation.
- Intentionally open payload maps and explicit compatibility aliases remain open
  or accepted. No new dependency is required.

## Implementation boundaries

Promote the existing complete integration-ID inventory in `config.rs` to production
use. Validate map keys at deploy validation and warn at runtime validation. Keep
feature-gated fixture IDs aligned with their existing validation paths. Generic
registered builders alone are insufficient because Prebid and APS use separate
registration paths.

Add serde strictness only to the ten integration config structs that lack it,
and the two additional settings structs. Include unknown/missing-field details in
both branches of `IntegrationSettings::get_typed`; retain other serde error details
only in the report's cause chain so the outer message does not echo invalid values.

## Compatibility and alternatives

Known tables with unknown fields will fail runtime startup with the new binary.
Before deploying, run the new CLI's `ts config validate`, correct the reported
settings, and push the cleaned config while the old binary is still running. On
Fastly, wait for the config-store propagation window to elapse before activating
the new binary; a successful push does not mean every point of presence (POP) has
the new config. Removing previously ignored keys is safe for the old binary.
Unknown IDs only warn at runtime.

A warn-first release for unknown fields would avoid this startup break but needs
additional parsing machinery. The approved choice is the issue's smaller strict
schema fix with documented migration. The future module schema is outside scope.

## Acceptance and preservation

| Scenario                                         | Required result                               |
| ------------------------------------------------ | --------------------------------------------- |
| Unknown integration ID during deploy validation  | Error names ID and known IDs                  |
| Unknown integration ID at runtime                | Warning names ID; startup validation succeeds |
| Unknown field in enabled or disabled known block | Error names integration and field             |
| Minimal explicitly disabled block                | Accepted without enabled-only required fields |
| Open payload parameter maps                      | Arbitrary payload keys remain accepted        |
| Deprecated explicit aliases                      | Remain accepted                               |
| Unknown tester-cookie or Tinybird key            | Deserialization fails                         |
| CLI config validate/push                         | Errors expose offending integration key       |

Regression tests must distinguish the original behavior before implementation.
Run focused native core and CLI checks, adapter tests and target-matched lint,
then the repository's required CI gates. Record unavailable tools or mismatched
versions separately from passing evidence. Correct the configuration guide,
Lockr example, error reference, and changelog upgrade instructions.
