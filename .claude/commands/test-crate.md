Test a specific crate by name.

Usage: /test-crate $ARGUMENTS

Use the target-aware test rules referenced by the
[canonical CI gate list](/CLAUDE.md#ci-gates). Select the canonical adapter or
JavaScript gate that covers `$ARGUMENTS`; do not substitute a bare workspace
test. For a host-only crate without a covering alias, follow the host-target
procedure in `CLAUDE.md`.

Report the selected canonical rule and its result. Investigate any failure.
