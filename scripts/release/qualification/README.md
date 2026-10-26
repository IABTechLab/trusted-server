# Disposable release qualification framework

Phase 2 staging note: this catalog includes controlled destination fixtures, not
a claim that their qualification or index/publication integrations are active.
Controlled JS preparation still pins esbuild 0.27.3 while the current lock uses
0.28.2; ordinary releases and Gate 1 do not use that observer. See the
[compatibility notes](../../../docs/superpowers/plans/binary-attestations-phase1-compatibility.md#controlled-js-observer-gap).

Status: **Rust migration in progress; candidate recipes for all four assets pass ordinary CI, but Tasks 8–9 qualification remains incomplete**. No tool,
SDK, OCI image or recipe has become qualified. `release-tools.json` retains null
measurement slots. A source checksum and a successful unit test cannot supply
hosted execution or acquisition authenticity.

The retained observation schema, experimental source-owned boundaries and inert
hosted proposal guide Rust semantic checks and native execution. This framework creates no repository, dispatches no job, registers no workflow,
publishes no tag, signs no release and deploys no publisher service.

## Rust migration and Task 9 status

Introduced Python qualification/recipe tooling and validator requirements are removed. Historical local socket/Cargo/tiny Mach-O probes are not selected macOS 15/GNU qualification. The [Rust migration addendum](../../../docs/superpowers/specs/2026-10-14-release-tooling-rust-migration.md) and [primary plan](../../../docs/superpowers/plans/2026-10-13-attested-release-implementation.md#rust-migration-and-current-execution-boundary) define the host-only `crates/trusted-server-release` tool. Strict document/policy checks, local source authentication, helper-closure fingerprinting, bounded Git/process execution and final-artifact inspection are implemented. Authenticated bootstrap/acquisition and actual qualification remain pending.

The experimental macOS sandbox denies network access but does not own
descendant cleanup the way a container does. Commit `60fafa717` supersedes the
earlier finding that a `posix_spawn` escape, or a child that scrubs its
tracking environment, could survive the run: the boundary now identifies the
run's sandbox instance, finds, kills and rejects every process of that
instance after the run, and reports `descendant_cleanup: "post-run-reaped"`
(Linux containers report `owned-by-container`). Survivors exist until that
reap, so this is weaker than ownership and remains unqualified. Only the
`cfg(test)` ordinary-CI candidate selection accepts post-run-reaped cleanup
(approved 2026-10-19). Socket denial and successful utility tests do not
qualify the lifecycle boundary. The production macOS and Linux selectors remain
unconstructible until their required controls and independently authenticated
inputs are established.

Current wrappers fail closed until the independently approved native Rust
bootstrap and source-owned authenticated selections are implemented. Neither
inventory labels/hashes nor a self-authored checksum authenticates its producer.
Private typed source/acquisition approval and actual JS completion are constructed
only by the corresponding successful verification/execution path; no serialized
authorization boolean is accepted. The tool has no core/CLI/JS crate dependency.

Task 9 is candidate-implemented, with qualification pending. Candidate recipes for
all four canonical assets (Fastly Wasm, Linux x86-64 `ts`, Apple ARM `ts`, Apple
Intel `ts`) build, inspect and start in ordinary PR CI with
`release_qualification: false`; see the dated reports in this directory. The
closed `build.sh` interface, the production reproduction profile, authenticated
acquisition/tool selections and hosted qualification remain open. Rust MUST compose original-material preparation, actual
whole-product Cargo compilation, final-byte acceptance and output copying. Task 10
MUST collect explicit-target metadata/tree, effective unit features, native archive
and sysroot evidence through the same boundary before hosted measurement. Native
selected runtimes and complete Fastly A/B acceptance remain mandatory. Independently
reviewed all-cohort remote evidence is distinct from current installed-tool rehash
and SDK/environment checks; neither is a substitute for the other.

Native reproduction scheduling changed in `3e3e30996`: ordinary push/PR runs
defer the optional CLI reproduction, while scheduled/dispatched runs and PRs
labelled before a triggering event measure it. Fastly equality and native
canonical acceptance remain mandatory. `b4c8e0c0f` increases the ordinary-CI
per-build wall to 45 minutes after Intel's optional reproduction again exhausted
25 minutes in run 39540057202. The [schedule/cache report](2026-10-21-native-reproduction-schedule.md)
records baseline timings and pending validation; no Intel equality under the new
wall is established. The [candidate index report](2026-10-21-candidate-release-index.md)
records four successful per-asset entry checks, not a real cross-job assembly
or completion of Task 13. None of these changes qualifies a production selection.

## Local Rust checks

Current registered utility checks use the source-built developer target. The
qualification module/filter below is a future instruction: it MUST fail its
nonzero guard until the real qualification tests are registered.

```sh
set -eu
HOST_TARGET=$(rustc -vV | sed -n 's/^host: //p')
test -n "$HOST_TARGET"
RELEASE_TEST_LIST=$(mktemp)
trap 'rm -f "$RELEASE_TEST_LIST"' EXIT
cargo test -p trusted-server-release --target "$HOST_TARGET" qualification::tests -- --list > "$RELEASE_TEST_LIST"
awk '/: test$/ { count++ } END { if (count == 0) { print "Qualification filter selected zero tests"; exit 1 } print "Selected " count " qualification tests" }' "$RELEASE_TEST_LIST"
cargo test -p trusted-server-release --target "$HOST_TARGET" qualification::tests
cargo clippy -p trusted-server-release --target "$HOST_TARGET" --all-targets --all-features -- -D warnings
```

These commands provide local behavior evidence, not authenticated native bootstrap
or official release proof. Private tests live alongside `src/qualification.rs` or
in its registered child `tests.rs`; public process tests use `tests/commands.rs`.
Fixtures MUST stay fictional. Do not make internal APIs public for tests.

The real unconfined probe requires local socket permission. Selected actual native
boundary probes MUST run outside an enclosing application sandbox. Child and
grandchild execute the same compiled Rust probe under inherited confinement with
private HOME/TMP and closed inherited descriptors. No ambient interpreter/tool
selection is trusted. A successful local test does not qualify macOS 15, GNU,
Intel execution, the whole SDK or original distribution acquisition.

The proposed command `trusted-server-release qualify validate-observation <file>`
MUST check bounded closed schema and semantic joins once implemented; coherent JSON still has pending
qualification until independent evidence is verified. The production qualified
selector remains closed. Fields asserting `passed`, caller commands and unknown
keys are rejected. Actual logs, acquisition signatures, independent trust roots
and run identities MUST be verified; no JSON field authenticates itself. Native
bootstrap/schema pins MUST be reviewed before this command can serve production.

## Closed identities and fingerprints

The GNU builder selection is a distinct `gnu-builder-x86_64` slot, with no image
chosen or digest invented. UBI 9 is an initial candidate to investigate; its
availability, glibc, compiler and SDK MUST be measured before selecting it. UBI 9,
Ubuntu 22.04 and Debian 12 are three separate compatibility runtime slots. They
do not pretend to supply a build SDK.

macOS records GitHub's actual runner image version and the Apple SDK identity.
No VM byte hash is available through this contract, so a hash of a version string
is never labeled an image digest. The source-owned labels are `macos-15` for ARM64
and `macos-15-intel` for native Intel. Native `uname`, `sw_vers`, translation
`sysctl`, Xcode build version and SDK version/tree identity MUST be recorded.
The actual startup OS is 15; the Mach-O loader floor is explicitly 13.0. Neither
Rosetta nor startup on a newer developer Mac establishes this matrix.

The recipe closure is SHA-256 of
`trusted-server-recipe-closure-v1\n` followed by deterministic JSON containing
sorted reviewed file path/executable-bit/SHA-256 records and effective settings.
The execution selection digest uses
`trusted-server-execution-selection-v1\n` followed by deterministic JSON of the
tool, environment and recipe selections. It includes acquisition trust roots,
archive identities and selected SDK/image identities. Only derived qualification
status and recipe-result digests are excluded. Complete raw `release-tools.json`
bytes remain an independently recorded build/preflight input.

This avoids a self-hash cycle: a result cannot hash a future tools file containing
that result's digest. The observation records its own source commit; the later
reviewed measured-pin commit has different raw tools bytes. Recipe/profile/helper
changes or changed executable/acquisition selections invalidate qualification.
The Rust candidate fingerprint validator MUST refuse missing shared Task 9 helpers or changed executable source. The closure includes the release-tool manifest/lockfile and full compiled path-dependency closure, all loaded Rust modules, POSIX adapters, retained Node collector, reviewed profiles and schema bytes; a candidate cannot choose that path list. The existing
Git reader assumes a trusted immutable object store; the actual staging boundary
MUST make Git objects and tracked source files read-only while permitting the
private installation's required node_modules/output writes. This framework does
not claim race-proof reads against mutable Git storage.

The reviewed closure explicitly includes the Rust build scripts and their
shared git-version/JS prebuilt/inventory/bundle-set helpers, Cargo configuration
and ALL path-dependency manifests, explicitly `crates/trusted-server-openrtb/Cargo.toml`, tool-version files, authenticated executable digests, SDK identities and raw receipts, and the executable JS build-all/module-options/composition/Vitest
pipeline. Ordinary application source and dependencies have separate commit and
dependency identities. Future imported build helpers MUST enter this reviewed
closure before execution; the candidate selector cannot supply its own path list.

The host release helper itself MUST load/run inside the GNU <=2.34 builder; build
it through an independently approved baseline image/toolchain/material channel,
not ambient Ubuntu 24 followed by an assumed UBI 9 invocation. Bind helper target,
loader/runtime and its own source/dependency closure. Native macOS helper CPU/SDK
and floor compatibility need the same independent approval. Initial shell/hash/
Rust/Cargo trust is external to the tool; authenticate rustup only if actually used; compiled native socket/lifecycle probes
cannot bootstrap their own authority.

## Candidate boundaries

The Linux seccomp baseline is retained from `moby/profiles`: annotated tag
`seccomp/v0.2.1` has tag-object identity
`e295db0a4d7b67853387fef0b2ef0a29e796b1c1` and peels to commit
`5ad5f40ecde90d78e4a7c861c003fe92747d5518`, file
`seccomp/default.json`, SHA-256
`536529b665dd0972c37bfb569f5d4ac8a53592e7b00752bc39ff063ca9864c74`.
Its Apache license is retained in `SECCOMP-LICENSE`. The future Rust profile MUST preserve the baseline's default deny,
architecture mapping and other reviewed syscall conditions. It MUST remove every socket/socketpair/socketcall
allowance and add only socket/socketpair with argument zero equal to AF_UNIX. This avoids inventing a syscall allowlist.
The original file hash MUST be checked before generating the effective profile.
Both original and effective profile hashes MUST be checked before EVERY host command.
The AF_UNIX layer is required because this contract denies INET sockets including
loopback; `--network none` alone permits loopback. Preserve reviewed syscalls
rather than introducing an unchecked custom allowlist.

The Rust container adapter (`boundary/docker.rs`, implemented; unqualified) MUST combine `--network none` with that
seccomp profile, drop all capabilities, set no-new-privileges, use private IPC
and Docker's default private PID namespace (no invalid `--pid=private`), a
read-only root and UID 65532 with prepared writable ownership. Memory, CPU and
process limits MUST fit the actual host; Cargo A jobs 4/B jobs 2 does not
authorize four container CPUs on a two-vCPU host. No host daemon socket is mounted.

Both profiles and an empty Docker client configuration MUST live in a host-owned
root disjoint from every container-writable workspace. Scan BEFORE creating
these inputs and rehash BOTH profiles before EVERY host command. Select the
authenticated fixed Docker client, explicit empty config and Unix daemon; reject
TTY, plugins, alternate contexts and inherited Docker overrides. Read-only
anchors MUST remain outside the writable root. Test escaping/dangling aliases
in the CONTAINER namespace, not only their host interpretation.

The earlier local-only finding (a Docker client without a usable daemon, so no
actual Linux execution proof) is superseded: ordinary Ubuntu 24.04 CI runs the
real container boundary and the Linux candidate recipes, recorded in the
[confined Cargo candidate](2026-10-16-confined-cargo-candidate.md) and
[Linux CLI candidate](2026-10-16-linux-cli-candidate.md) reports and the
[GNU follow-up](2026-10-20-linux-cli-gnu-follow-up.md). That is candidate
evidence, not qualification. Rust MUST execute positive, denied and full-recipe
controls with the same selected native probe and OCI image, not merely compare
command arguments. Controls map approved workspace inputs to `/work`, with
shared tools/source read-only. Scan sockets and aliases before use; this does
not claim protection against an adversarial mutable host mount.

The macOS Rust adapter (`boundary/macos.rs`, implemented; unqualified) MUST enforce private-path/AF_UNIX rules, denying
INET, arbitrary Mach lookup and AppleEvent delegation. The same generated
profile wraps positive/denied probes and the full Cargo/compiler/proc-macro/
build-script/native-tool invocation; Task 7's lifecycle/JS boundary needs the
same actual qualification.

Tracked source and Git-object stores MUST be immutable while private installation/
output writes remain available. Deny ancestor unlink/rename. Every immutable
regular file MUST have `nlink == 1` before execution; reject pre-existing
hard-link aliases and execute write-through/restoration negatives. Limit signals
to `same-sandbox`: own-child success and outside same-user sibling/orchestrator
denial are required. Deleted local prototypes do not establish these Rust proofs.

Both boundaries require actual positive then confined TCP and UDP bind/connect/
send probes for IPv4/IPv6, loopback/documentation-address destinations, across
parent/child/grandchild, plus private Unix exchange. External routing failures and
unsupported unconfined IPv6 are recorded as unavailable, never successful
controls. Positive IPv4 loopback operations MUST succeed, every confined INET
operation MUST return EPERM/EACCES, and private Unix exchange MUST succeed. A
socket helper is only boundary evidence; full recipe success is a separate gate.

The Task 13 candidate release index (`src/distribution.rs`) is candidate
evidence only. Its tag is the sentinel `v0.0.0-unqualified-candidate`, its bytes
are named only `candidate-release-index.json`, and both signature bundle records
of every artifact are empty-file records. Every unsigned companion declares
`"release_qualification": false` and lists its gaps. The CLI verifier rejects
size-0 bundle records, so a candidate index can never verify. It does not
qualify an environment, and no command writes, signs or publishes it.

## Reviewable hosted resource proposal

The adjacent `hosted-workflow.proposal.yml` is inert outside `.github/workflows`.
Its only trigger is manual workflow_dispatch with no inputs. It uses three fresh
GitHub-hosted jobs: `ubuntu-24.04`, `macos-15`, `macos-15-intel`; 120 minutes each,
no matrix fail-fast, no concurrent cancellation, contents-read only, no OIDC,
attestation/provider/release permissions, credentials, cache restore or publish
steps. Logs are retained for seven days. Action versions and full revisions were
verified from primary action metadata for checkout 5.0.0 and upload-artifact
6.0.0 (Node 24); full fetch history avoids the pending shallow-source rejection.
Pins require manual review/maintenance: Dependabot does not scan this inert
path outside `.github/workflows`. Logs use `tee` with `pipefail` so failure
output is visible and retained. Native Rust bootstrap steps remain pending; they are source-reviewed code selections, not independently
authenticated tool archives. Runner labels were checked against primary GitHub
runner documentation on 2026-10-14; availability MUST be rechecked at dispatch.

This proposal intentionally stops at `check-ready`. It cannot falsely report a
complete qualification with version-only probes. The reviewed executable staging
steps may replace that stop only when all prerequisites below are complete. Production selection MUST
require all qualified inputs by default; named audit diagnostics cannot
construct approval state or turn pending/null pins into success:

1. Task 9 implements the shared full production recipes, native inspection and
   exact-final-Wasm core-ABI/publisher-response validation. The staging selector
   MUST invoke those same source-owned functions with independently authenticated
   candidate selections. It cannot require its own future production-qualified
   result, accept downloaded commands or introduce a public local-fixture bypass.
2. Review independently approved-source native Rust bootstrap and actually acquire
   Git/shell, signed Rust 1.98.1 components/sysroot, compiler/linker/archiver,
   Node 24.20.0,
   npm 11.19.0 and the selected runtime tools through source-owned authenticators.
   Node uses signed checksums and independently reviewed keys; Rust uses signed
   manifests/components; gh uses its verified archive and independent bootstrap.
   A colocated checksum alone does not authenticate Fastly or an SDK. Retain raw
   signature/checksum evidence, reviewed trust roots, archive and extracted-tree
   digests and actual selected file identities. SDK/platform trust needs an
   explicit vendor/platform evidence chain. Ambient PATH cannot select unreviewed native tools before isolation. A
   source-built tool and colocated checksum supply no qualifying bootstrap proof.
   Authenticate original Fastly vendor archive bytes through independently reviewed
   vendor-source evidence; no missing-root fallback is permitted.
3. Acquire the candidate GNU image by authenticated content identity, record both
   OCI index and selected AMD64 manifest digests, and measure glibc <=2.34 inside
   that builder. Select the three runtime-only images by their actual digests.
   Record native archive/link/sysroot identities: aws-lc-sys 0.42 target-suffix
   overrides and generic/target system-use=0, AWS-LC 5.1 source, ring 0.17.14
   pregenerated source membership, and libsqlite3-sys 0.38.2 bundled SQLite
   source/pkg-config=0. Wrapper versions do not establish the native
   implementation version in emitted evidence.
4. Network-enabled authenticated archive/tool acquisition precedes fresh offline
   installation and compilation. Execute actual lifecycle hooks under the selected
   native denial boundary; recording an install flag alone is insufficient.
   Close source/object-store writes and run positive/negative probes,
   full working compilation, actual Cargo/npm download-attempt negatives and
   actual missing-tool negatives through the same selected boundary. Retain
   outcomes and attempted fetch evidence even when a script catches denial.
   Audit inherited hosted/setup exports before clearing them; only explicitly
   reviewed qualified values/path identities may survive. Bound subprocess
   pipes/deadlines, clear token/provider/injection environment, terminate/reap
   descendant groups on timeout/cancellation/failure and
   clean up private profiles/resources and Docker containers with bounded removal.
5. Outside the compile-denial boundary, run the exact CLI bytes outside checkout
   on clean UBI 9/Ubuntu 22.04/Debian 12 and actual native macOS 15 ARM/Intel.
   Verify GNU symbol ceiling/libraries and Mach-O LC_BUILD_VERSION 13.0. Validate
   the exact final Fastly Wasm with pinned Wasmtime and complete publisher-origin
   response/injection through pinned Viceroy, retaining pre/post byte hashes.
   No health-only success, cross-Intel inference or changed cohort is accepted.
6. Join every observation to the reviewed approved-source catalog, selection
   digest, exact tool versions/authentication methods and full helper closure
   before accepting it. Record actual errno; do not invent `ETIMEDOUT`. Hash the
   selected shell rather than assuming `/bin/sh --version` works. Use
   host-independent tool/environment fixtures. Collect the closed observations
   with actual repository/owner/run/attempt/job
   identities and raw retained evidence. Independently authenticate and review
   those identities, acquisitions, source closure and selections. A valid receipt
   proves only its signed contents; independent catalog/source/root approval is
   still required and cannot be supplied by that receipt or its checksum. Only then
   update the source-owned measured pins and implement their production selector.

New workflow files on a feature branch may be unavailable to manual dispatch
until registered on a repository's default branch. The concrete safe option is
an explicitly authorized disposable mirror whose default branch contains the
approved source commit plus the reviewed staging workflow. Record its actual
repository and owner IDs as disposable observations. Do not merge into this
project's main branch to register the proposal. A mirror's identity never satisfies
the fixed production IAB repository/SAN/policy, and no production policy changes
are authorized by rehearsal. Repository creation/default-branch registration,
resource spending and dispatch remain separate permission gates.

Missing native Intel or the GNU baseline stops this cohort. Only after actual
hosted qualification may the final Task 8 commit use the title `Qualify controlled
release build environments`. The local framework commit MUST use an accurate
title such as `Add release environment qualification checks`.
