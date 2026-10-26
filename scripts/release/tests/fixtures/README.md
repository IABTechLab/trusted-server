# Release document fixtures

Phase 2 staging note: this catalog includes controlled destination fixtures, not
a claim that their qualification or index/publication integrations are active.
Controlled JS preparation still pins esbuild 0.27.3 while the current lock uses
0.28.2; ordinary releases and Gate 1 do not use that observer. See the
[compatibility notes](../../../../docs/superpowers/plans/binary-attestations-phase1-compatibility.md#controlled-js-observer-gap).

The host-only `crates/trusted-server-release` implements current strict document/schema utilities. Introduced Python utilities and requirements are removed; their behavior/results do not establish replacement Rust execution. The [migration addendum](../../../../docs/superpowers/specs/2026-10-14-release-tooling-rust-migration.md) and [primary plan](../../../../docs/superpowers/plans/2026-10-13-attested-release-implementation.md#rust-migration-and-current-execution-boundary) describe remaining source/bootstrap/qualification gates.

Private unit cases live alongside `src/documents.rs` or in registered
`src/documents/tests.rs`. Shared byte fixtures live under
`crates/trusted-server-release/tests/fixtures/`, and `tests/commands.rs` exercises
public process behavior. Do not expose internal APIs for integration tests.
Exact byte cases must retain duplicate keys, invalid UTF-8/surrogates/BOM, trailing
controls, nonfinite/unsafe numbers, depth 65 and oversized role inputs. Test lexical
integer overflow in opaque nested fields and top-level values, decimal/exponent
float overflow, and exact `i64` negative/`u64` nonnegative boundary controls. Large
number-like text inside JSON strings, including escaped quotes/backslashes, must
remain accepted text. Role fixtures additionally reject unsafe integers and
negative/noninteger lengths. Fixtures
also cover flat names, integer lengths, ambiguous identities, original-byte hashes,
deterministic output and atomic replacement failures. Use fictional names and
`example.com` URLs only.

Run the registered local utility tests:

```sh
set -eu
HOST_TARGET=$(rustc -vV | sed -n 's/^host: //p')
test -n "$HOST_TARGET"
RELEASE_TEST_LIST=$(mktemp)
trap 'rm -f "$RELEASE_TEST_LIST"' EXIT
cargo test -p trusted-server-release --target "$HOST_TARGET" documents::tests -- --list > "$RELEASE_TEST_LIST"
awk '/: test$/ { count++ } END { if (count == 0) { print "Documents filter selected zero tests"; exit 1 } print "Selected " count " document tests" }' "$RELEASE_TEST_LIST"
cargo test -p trusted-server-release --target "$HOST_TARGET" documents::tests
cargo test -p trusted-server-release --target "$HOST_TARGET" --test commands
cargo clippy -p trusted-server-release --target "$HOST_TARGET" --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```

These source-built developer commands provide local behavior evidence. Production
uses the independently approved-source, authenticated native bootstrap and reviewed
qualification; no official release-binary proof follows from this local build.

The implemented Task 2 utility uses a recursive Serde visitor/seed to reject duplicate keys before
inserting values. Its finite-float check is not a complete strict front end: Serde
can convert lexical integer overflow to a finite float. Before `deserialize_any`,
scan the whole document's numeric tokens while correctly skipping quoted text and
escapes. Negative integers must fit `i64`, nonnegative integers must fit `u64`, and
decimal/exponent tokens must parse as finite `f64`. The existing scanner in
`crates/trusted-server-js/build/inventory.rs` is a behavioral reference, not a new
crate dependency. Apply stricter role safe-integer/length checks afterward.
It chooses `max_bytes` for each role before reading/allocation;
container depth is at most 64 including the root, and brackets within strings do
not increase depth. Hash original bytes before any semantic decoding. Producer
JSON sorts keys, uses compact UTF-8 and one final newline. Atomic writes use a
private sibling with mode `0600`, sync before replacement and clean failures.

The selected Rust evaluator is locally checked with explicit offline Draft 2020-12
`prefixItems`/`items` positives/negatives for governing schemas and Draft 7 for the
authoritative SPDX 2.3 schema. Retain the 45,312-byte official schema at commit
`aadf3b0b8dbbabdb4d880b0fc714255fea436ff7`, SHA-256
`239208b7ac287b3cf5d9a9af23f9d69863971102a5e1587a27a398b43490b89b`.
Use only known retained schemas, reject remote/file retrieval and unknown refs,
and permit no arbitrary public schema-file override. The evaluator dependency is locked; native acquisition and production qualification
remain pending. Schema validation does not
replace strict byte parsing, original-byte hashing or full real-artifact SPDX
coverage/semantic checks.
