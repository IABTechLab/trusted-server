# GPP registry snapshot (normative, vendored)

The pinned per-section accepted versions for the permission spec's §4.5
map. This file is the single reproducible authority; updating it is a
reviewed spec change. A mapped section presenting a version not listed
here is treated as malformed-present (permission spec §4.4).

| GPP section ID | Section                                             | Accepted version(s) |
| -------------- | --------------------------------------------------- | ------------------- |
| 6              | US Privacy string (uspv1, carried as a GPP section) | 1                   |
| 7              | usnat                                               | 1                   |
| 8              | usca                                                | 1                   |
| 9              | usva                                                | 1                   |
| 10             | usco                                                | 1                   |
| 11             | usut                                                | 1                   |
| 12             | usct                                                | 1                   |
| 13             | usfl                                                | 1                   |
| 14             | usmt                                                | 1                   |
| 15             | usor                                                | 1                   |
| 16             | ustx                                                | 1                   |
| 17             | usde                                                | 1                   |
| 18             | usia                                                | 1                   |
| 19             | usne                                                | 1                   |
| 20             | usnh                                                | 1                   |
| 21             | usnj                                                | 1                   |
| 22             | ustn                                                | 1                   |
| 23             | usmn                                                | 1                   |

Version values for sections 6–23 were captured from the IAB registry at
the time of writing and are re-verified against the official registry as
part of ratification review. Sections 24–27 have assigned IDs but no
reproducibly published binary layouts in the official sources as of this
snapshot; they are reserved and inert until an official layout can be
vendored here. Any change is a reviewed change to this file.

## Reserved sections — NOT accepted, no version

These state sections have assigned IDs but no reproducibly published
official binary layout as of this snapshot. They are **not** in the
accepted-version table above: an implementation MUST NOT decode them,
and a request carrying one behaves national-section-only (permission
spec §4.5, sign-off 32). A reserved ID is _expected-inert_; an unknown
ID (outside both tables) is _flagged for snapshot review_ — the only
observable difference is logging.

| GPP section ID | State     | Status                        |
| -------------- | --------- | ----------------------------- |
| 24             | usmd (MD) | reserved — no official layout |
| 25             | usin (IN) | reserved — no official layout |
| 26             | usky (KY) | reserved — no official layout |
| 27             | usri (RI) | reserved — no official layout |

## Provenance and vectors

Supported sections (6–23) pin to the official IAB GPP registry revision
recorded by the implementation PR (immutable upstream commit hash), with
per-section encoded conformance vectors vendored alongside. A date is not
a revision; the commit hash is the reproducible authority.

**Status: placeholder until ratification.** Neither the immutable
registry commit nor the conformance vectors are recorded yet; like the
PSL snapshot, filling them is a pre-ratification prerequisite
(migration spec §4) — the §4.5 field mappings cannot be reproduced
against a pinned registry until they land.
