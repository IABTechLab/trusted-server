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
| 24             | usmd                                                | 1                   |
| 25             | usin                                                | 1                   |
| 26             | usky                                                | 1                   |
| 27             | usri                                                | 1                   |

At the pinned commit below, the official section registry assigns IDs 24–27
to MD, IN, KY, and RI and each named state specification defines accepted
version 1. That commit-backed statement, rather than an unverified publication
month, is the authority for admitting them. Treating them as national-only
would discard a state-specific choice. Unknown IDs outside the accepted table
still contribute nothing and are flagged for snapshot review.

## Provenance and vectors

The immutable authority is the official
`InteractiveAdvertisingBureau/Global-Privacy-Platform` commit:

`00ffaefe91513785e886c83877e9b56a4ec8e88c`

Normative upstream paths for the newly admitted layouts are:

- `Sections/US-States/MD/Maryland Privacy Technical Specification.md`
- `Sections/US-States/IN/Indiana Privacy Technical Specification.md`
- `Sections/US-States/KY/Kentucky Privacy Technical Specification.md`
- `Sections/US-States/RI/Rhode Island Privacy Technical Specification.md`
- `Sections/Section Information.md`

The implementation vendors decoder fixtures under
`crates/trusted-server-core/testdata/gpp/00ffaefe91513785e886c83877e9b56a4ec8e88c/`.
That directory contains a `manifest.json` object with:

- `upstream_commit_oid` and `upstream_commit_tree_oid`;
- a sorted `sources` array containing `{path, blob_oid, sha256_hex}` for all
  five normative paths above — the four state specifications and
  `Sections/Section Information.md`; and
- a sorted `cases` array whose entries are
  `{section_id, version, case, encoded, expected}`.

The vendoring PR description quotes the same commit/tree/blob values and the
independent command output used to verify every raw source SHA-256 and the
byte-for-byte copy. A commit OID without its tree and source-blob witnesses is
not accepted as completed provenance. `expected` uses the
permission spec's normalized P1/P4/GPC tokens, not decoder-library enums.
Fixture encodings must be constructed from the pinned bit layouts by an
independent generator or hand-checked vector, never emitted and consumed only
by the decoder under test. For every accepted section/version the corpus must
contain: minimum valid core-only string, core + GPC true, each mapped opt-out
value, each explicit not-opted-out value, explicit N/A, malformed/truncated
input, unsupported version, and a mixed known/unknown-section string. CI
refuses to update this file unless the complete corpus for the new commit is
present.
