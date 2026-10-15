# Task 7: real original materials and two fresh JS builds

Status: the explicit original-material and Cargo Git tests passed locally on
native macOS, and the full Linux A/B execution test passed in
[ordinary PR CI](https://github.com/IABTechLab/trusted-server/actions/runs/38747798078)
on 2026-10-15. This is candidate execution evidence from an ordinary CI
runner. It does not qualify release tools, an OCI image, bootstrap, hosted
release environments or independently approved release source. Production
selectors remain closed.

## Original-material validation

The explicit native test
`materials::npm_bundle::tests::actual_current_lock_npm_originals` passed
locally on native macOS on 2026-10-15 in 283.74 seconds. It is ignored in
ordinary CI; there, original authentication runs only inside the A/B fixture's
npm original preparation. It authenticated all 651 locked installation
locations and 626 distinct compressed originals against the current npm
lock's SHA-512 identities, freshly extracted every original, reviewed every
manifest, retained the compressed originals, and rechecked all retained
archives and extracted trees. Its fictional source binding is a diagnostic
test input, not independent source approval or acquisition qualification.

The exact npm lock SHA-256 is
`d5a1b74310b0455531c3133cacf86fa528682cbed91c40412fb3791ccd48c527`.
An independent local raw-header diagnostic, not retained in the repository,
inspected 30,723 entries. The largest
member is the locked optional OpenBSD Rollup binary, 110,275,248 bytes. The
reviewed member limit is 128 MiB; the compressed-original limit remains
64 MiB and total decoded limit remains 512 MiB per archive. Extraction and
rehashing stream bytes and honor cancellation/deadlines.

Actual originals exposed the eight exact-version legacy type-package roots,
seven node-tar timestamp layouts, and local PAX metadata in `gulp-wrap`
0.15.0 documented in [JS composition](../js-composition.md). Only redundant
PAX path/size values matching the next raw regular member are accepted.
Unknown fields, duplicate keys, malformed framing, overrides, unsupported
record sequences, links, privileged modes, and hidden name bytes fail.
The only empty script entries were non-install separator labels in `ignore`;
empty lifecycle declarations remain rejected. The only actual dependency
install hooks were the reviewed `core-js` and `esbuild` tuples.

The separate actual-current-Cargo-Git fixture also passed locally; it is
ignored in ordinary CI. The selected
closure contained 557 objects and 6,334,558 decoded object-content bytes,
excluding canonical object headers, with root tree
`411e16f40511f3f4867349d09784ec68beac022d`. This checks actual Git material
format and closure validation; it does not qualify Git acquisition, a native
toolchain, or a Cargo recipe.

## Linux execution acceptance

Orchestration commit: `2edd46ce6`
(`Build and compare two fresh offline JavaScript installations`); reviewed
archive layouts: `e2260d457b29b1202a39f8732e78d5d4bd9c323b`; CI runner configuration:
`cbd96047400099271534f110b5decdcf73b1a5c8`; collector fixture context and
failure diagnostics: `1a2c46075e18791b623f26ea73af8f833dc5f70b`.
The [Linux producer job](https://github.com/IABTechLab/trusted-server/actions/runs/38747798078/job/116465922121)
(`cargo test (release tooling and JS prebuilt, native) (ubuntu-24.04)`)
selects exactly one ignored full-execution test:
`preparation::tests::full::real_two_fresh_js_builds`.
It passed in 687.37 seconds (14:09:47Z to 14:21:15Z) after a two-minute
optimized test build: `1 passed; 0 failed; 0 ignored; 354 filtered out`.
Every job in that Run Tests workflow passed, as did the Run Format, CodeQL
and Integration Tests workflows for the same head.

CI checked out the pull request merge commit
`15fcd34c64b0f726f42f4f4da5de5af7e3577aa2`, whose parents are `main` at
`a0b00c72705baf6b0f63c206d7721b68ee03e995` and branch head
`1a2c46075e18791b623f26ea73af8f833dc5f70b`. The fixture cloned that exact
checkout. The commit is a CI candidate, not independently approved release
source. The printed measurement was:

```json
{
  "inventory_sha256": "2cdcb1da9c6318ce812cf7fd0b9cc0b1bdc9465c93d68953054db3e361b45042",
  "kind": "ordinary-ci-two-fresh-js-builds",
  "module_count": 13,
  "private_roots_removed": 6,
  "release_qualification": false,
  "schema_version": 1,
  "settings_sha256": "6bce0b31534321c4e36209caf09cad19a0ede1dbe7f6e39254b3f68657a8ea27",
  "source_commit": "15fcd34c64b0f726f42f4f4da5de5af7e3577aa2",
  "test_counts": [1275, 1275],
  "tools_sha256": "4a461321d25eec779752960570ba574bae397102cb3b652b9b70cc24bb6431c6"
}
```

Both confined full suites passed all 1,275 registered tests including type
checks. All 13 final core/integration modules were byte-identical, six private
roots were removed and retained A bytes were rehashed at handoff. The test
writes this file only after every stage succeeds, so its presence records the
actual completed path; the file itself remains a diagnostic printout.

After this run, the fixture gained explicit assertions that each side exposes
only its own private source, installation and metadata roots, shares only the
read-only originals and tool roots, and cannot reuse the other side's boundary.
Isolation was already enforced: exact `matches_roots` checks run before
execution and each installation, and every command path is translated only
through its own boundary; the next CI run on
this branch exercises the new assertions. A unit test also now rejects failed,
pending and unregistered Vitest report results.

The fixture uses Node 24.12.0, npm 11.6.2, a source-built producer and native
observer guard, original compressed archives, and an ordinary local Linux
container image. It creates two independent tracked-source copies with no
`.git`, installed dependencies, generated bundles or shared writable cache.
Each side establishes an actual socket-denial proof in its own network/PID
boundary, seeds its private cache from authenticated originals, performs a
fresh offline installation with lifecycle observation, captures the original
collector inventory, exercises missing native tools and the actual esbuild
download fallback, and runs the full registered suite with release guards
and type checks. It compares every final core/integration bundle byte for
byte, closes all six private source/state/metadata roots, then rehashes
retained A bytes at handoff. No serialized measurement can construct private
completion authority.

After success the test writes `js-execution-measurement.json` into its
external fixture root and CI prints it. This records actual source identity,
A/B test counts, module count, selected tool/settings hashes, original A
inventory hash and removed-root count. Its `release_qualification` field is
explicitly false; the file is diagnostic evidence, not an authorization
input or a canonical release companion.

The full fixture uses test-profile optimization level 2 with debug assertions
and overflow checks explicitly enabled. Ordinary native tests retain their
existing debug profile. Cache seeding alone triggers at least 80 complete
original/tree/tool rechecks across A/B; optimization retains every check.
Local all-originals runtime fell from 283.74 seconds to 18.18 seconds after
a 48.44-second optimized compilation. A further run with both safety settings
explicitly enabled reused the same executable and passed in 17.93 seconds.
These are local performance measurements, not hosted execution proof.

## Supporting checks and scope

With the new unit test, the local producer suite passed 364 unit tests, four
boundary integration tests and 13 command tests on macOS. Two explicit
actual-material tests are ignored by that ordinary run and are exercised
separately.
All-target/all-feature producer Clippy, documentation, formatting and staged
domain lint passed. Independent spec and quality reviews found no remaining
Task 7 code blocker before the Linux run; that run has since passed.

A separate local, unretained release-mode Vitest JSON run passed all 1,275 tests. Its
52 report entries cover exactly 51 distinct registered test/type-check files;
the type-check file appears twice. This confirms the Rust report parser's
real format/coverage join; it does not substitute for either confined A/B run.

Two earlier full Linux attempts failed for concrete, since-fixed causes. The
[first](https://github.com/IABTechLab/trusted-server/actions/runs/38736177179/job/116425928695)
rejected the authentic 110,275,248-byte optional OpenBSD Rollup member under
the former 64 MiB member limit. The
[second](https://github.com/IABTechLab/trusted-server/actions/runs/38741529217/job/116444179601),
unoptimized, reached Vitest and failed one collector negative fixture after
2,027.48 seconds. That fixture passed parent
source context to its main call but omitted it from later rejection cases;
the duplicate-material case consequently failed at Git discovery in a fresh
source copy without `.git`. A separate local full no-`.git` reproduction confirmed
exactly that single failure. Copying only the corrected test file into the
same disposable tree made all 1,275 tests and type checks pass with zero
failed/pending tests. Every negative call now supplies the same source context,
preserving its intended semantic assertion. Failed Vitest runs also retain a
bounded sanitized list of registered failed test names in the process error
before checked cleanup; raw assertion messages and stacks are excluded.

R-JS allows one shared Linux JS build for later Rust artifact jobs. Task 7
does not qualify native Rust recipes or macOS lifecycle containment. The
macOS regression ports passed afterwards on [macOS 15 CI](https://github.com/IABTechLab/trusted-server/actions/runs/38765937295/job/116529009924), including the formerly residual scrubbed process-group escape, which is now killed and rejected. That boundary remains unqualified. Authenticated
bootstrap, selected hosted environments, actual Cargo recipes and official
release publication remain separate incomplete gates.
