# GAM Attribution Review Resolution Design

## Goal

Resolve every actionable comment from PR #1034 review 4966544291 while preserving
the existing default-off GAM attribution behavior and avoiding unrelated changes.

## Attribute aggregation

`IntegrationRegistry::tsjs_script_tag_attributes` will remain the boundary that
combines metadata from enabled head injectors. It will preserve registration order
and keep the first value emitted for each attribute name. Later duplicate names will
be ignored. This prevents invalid duplicate HTML attributes without changing the
precedence implied by the existing ordered aggregation.

The registry test fixtures will include two injectors that emit the same attribute
name, proving that the first value wins and non-conflicting attributes retain their
order.

## Trusted static attribute validation

`tsjs_script_tag_with_attributes` will continue rendering trusted, compile-time
attribute pairs without release-mode escaping overhead. In debug builds and tests,
it will assert that names are non-empty and contain only lowercase ASCII letters,
digits, and hyphens, and that values contain none of the characters that could alter
or ambiguously encode the double-quoted HTML attribute: double quote, ampersand,
less-than, or greater-than.

Focused panic tests will cover invalid names and values. The existing rendering test
will continue proving the valid path and the byte-for-byte unchanged unmarked path.

## Bootstrap failure visibility

The early GPT attribution command will continue isolating `setConfig` exceptions so
later bootstrap and publisher commands run. Its catch block will also call
`ts.log.warn` when that logger is present, matching the guarded logging style already
used in the bootstrap. The existing throwing-`setConfig` test will be extended to
prove both isolation and warning emission.

The attribution literal will use the file's double-quote style, and the Rust source
ordering test will match the normalized source text.

## Consistency cleanup

The unused `getConfig` member will be removed from the test-only `MockGoogleTag`
interface. Version-specific EdgeZero wording in the reviewed GPT example and guide
will be replaced with capability-based wording. The integration fixture identified
as load-bearing will remain unchanged.

## Verification

Changes will be made test-first where behavior changes:

1. Add duplicate-attribute and invalid-attribute tests and observe their expected
   failures before implementing the Rust protections.
2. Extend the throwing-`setConfig` Vitest and observe the missing warning assertion
   fail before adding guarded logging.
3. Run focused Rust and JavaScript tests after each implementation step.
4. Run repository formatting, JavaScript formatting/tests, target-matched Rust tests,
   clippy aliases, parity tests, and CLI tests before handoff.

