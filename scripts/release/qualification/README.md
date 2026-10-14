# Disposable release qualification framework

Status: **local framework only; Task 8 qualification remains pending**. No tool,
SDK, OCI image or recipe has become qualified. `release-tools.json` retains null
measurement slots. A source checksum and a successful unit test cannot supply
hosted execution or acquisition authenticity.

The locally authorized work consists of a strict observation schema, semantic
checks, source-owned candidate socket boundaries and an inert hosted workflow
proposal. It creates no repository, dispatches no job, registers no workflow,
publishes no tag, signs no release and deploys no publisher service.

## Local checks

Use a private Python 3.11+ environment and install the complete locked validator
set before running tests:

```sh
python3 -m venv /private/tmp/ts-qualification-venv
/private/tmp/ts-qualification-venv/bin/python -m pip install --require-hashes --only-binary=:all: -r scripts/release/requirements.txt
/private/tmp/ts-qualification-venv/bin/python -m pip check
/private/tmp/ts-qualification-venv/bin/python -B -m unittest discover -s scripts/release/tests -p 'test_qualification.py' -v
```

The real unconfined probe requires local socket permission. On macOS, set
`TS_RELEASE_BOUNDARY_TESTS=1` to additionally execute the actual candidate
sandbox-exec profile. Run those tests outside an enclosing application sandbox.
The child and grandchild execute the same probe through inherited confinement,
with private HOME/TMP and closed inherited descriptors. A successful local test
does not qualify macOS 15, GNU, Intel execution, a whole SDK or a tool acquisition.

`qualify.py validate-observation <file>` applies the bounded closed schema and
semantic joins, but prints an explicit pending qualification diagnostic even
when the observations are coherent. `require_qualified` always refuses. The
production `preflight.validate_tools(require_qualified=True)` and
`prepare.qualified_js_selection` also remain closed. Fields asserting `passed`,
caller commands and unknown keys are rejected. Actual logs, signed acquisition
evidence, trust roots and run identities still require independent verification;
no JSON field authenticates itself.

## Closed identities and fingerprints

The GNU builder selection is a distinct `gnu-builder-x86_64` slot, with no image
chosen or digest invented. UBI 9 is an initial candidate to investigate; its
availability, glibc, compiler and SDK must be measured before selecting it. UBI 9,
Ubuntu 22.04 and Debian 12 are three separate compatibility runtime slots. They
do not pretend to supply a build SDK.

macOS records GitHub's actual runner image version and the Apple SDK identity.
No VM byte hash is available through this contract, so a hash of a version string
is never labeled an image digest. The source-owned labels are `macos-15` for ARM64
and `macos-15-intel` for native Intel. Native `uname`, `sw_vers`, translation
`sysctl`, Xcode build version and SDK version/tree identity must be recorded.
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
`candidate_fingerprints` refuses missing shared Task 9 helpers. The existing
Git reader assumes a trusted immutable object store; the actual staging boundary
must make Git objects and tracked source files read-only while permitting the
private installation's required node_modules/output writes. This framework does
not claim race-proof reads against mutable Git storage.

The reviewed closure explicitly includes the Rust build scripts and their
shared git-version/JS prebuilt/inventory/bundle-set helpers, Cargo configuration
and manifests, and the executable JS build-all/module-options/composition/Vitest
pipeline. Ordinary application source and dependencies have separate commit and
dependency identities. Future imported build helpers must enter this reviewed
closure before execution; the candidate selector cannot supply its own path list.

## Candidate boundaries

The Linux seccomp baseline is the exact Moby `seccomp/v0.2.1` source revision
`e295db0a4d7b67853387fef0b2ef0a29e796b1c1`, file
`seccomp/default.json`, SHA-256
`536529b665dd0972c37bfb569f5d4ac8a53592e7b00752bc39ff063ca9864c74`.
Its Apache license is retained in `SECCOMP-LICENSE`. The baseline's default deny,
architecture mapping and other syscall conditions are preserved. Every socket,
socketpair and socketcall allowance is removed; only socket/socketpair with
argument zero equal to AF_UNIX is added. This avoids inventing a syscall allowlist.
The file hash is checked before generating the effective profile.

The candidate container command combines `--network none` with that seccomp
profile; network namespace isolation alone permits loopback sockets. It also
drops all capabilities, sets no-new-privileges, uses private IPC/PID namespaces,
a read-only root, an unprivileged fixed UID, bounded memory/CPU/processes, and
separate controlled read-only tool/source mounts plus a private writable root.
No host daemon socket is mounted. Preparation must establish writable ownership
for UID 65532. Container/profile command construction is tested locally; Linux
execution remains unmeasured because no local Docker proof exists.

Linux positive controls execute in the same selected OCI image/interpreter with
the checked default seccomp baseline and its ordinary private network namespace;
confined controls use the AF_UNIX-only profile and network-none namespace.
Both map the private host workspace to `/work`. Mount scanning rejects sockets
and symlink members escaping the approved roots; preparation must keep those
prepared trees immutable while the command executes. No local scan claims to
prevent an adversarial concurrent mount mutation.

The macOS candidate reuses Task 7's locally exercised file isolation and adds
AF_UNIX/private-path rules. INET, arbitrary Mach lookup and AppleEvent delegation
remain denied by default. The same generated profile must wrap probes and the
complete Cargo/compiler/proc-macro/build-script/native-tool invocation. This file
does not replace Task 7's independent JS boundary or qualify hosted recipes.

The candidate macOS profile can mark actual tracked source files and Git-object
directories inside the private root immutable, while leaving node_modules and
output writes available. It also denies unlink/rename of their ancestors so a
writable ancestor cannot move a protected file outside its literal deny rule.
This separation is locally exercised with actual ancestor rename/removal,
hard-link, source-write and private-output controls; the full recipe must pass
its reviewed tracked-object set.

Both boundaries require actual positive then confined TCP and UDP bind/connect/
send probes for IPv4/IPv6, loopback/documentation-address destinations, across
parent/child/grandchild, plus private Unix exchange. External routing failures and
unsupported unconfined IPv6 are recorded as unavailable, never successful
controls. Positive IPv4 loopback operations must succeed, every confined INET
operation must return EPERM/EACCES, and private Unix exchange must succeed. A
socket helper is only boundary evidence; full recipe success is a separate gate.

## Reviewable hosted resource proposal

The adjacent `hosted-workflow.proposal.yml` is inert outside `.github/workflows`.
Its only trigger is manual workflow_dispatch with no inputs. It uses three fresh
GitHub-hosted jobs: `ubuntu-24.04`, `macos-15`, `macos-15-intel`; 120 minutes each,
no matrix fail-fast, no concurrent cancellation, contents-read only, no OIDC,
attestation/provider/release permissions, credentials, cache restore or publish
steps. Logs are retained for seven days. Action versions and full revisions were
read from primary GitHub tag refs for checkout 4.2.2, setup-python 5.6.0 and
upload-artifact 4.6.2; they are source-reviewed code selections, not independently
authenticated tool archives. Runner labels were checked against primary GitHub
runner documentation on 2026-10-14; availability must be rechecked at dispatch.

This proposal intentionally stops at `check-ready`. It cannot falsely report a
complete qualification with version-only probes. The reviewed executable staging
steps may replace that stop only when all prerequisites below are complete:

1. Task 9 implements the shared full production recipes, native inspection and
   exact-final-Wasm core-ABI/publisher-response validation. The staging selector
   must invoke those same source-owned functions with independently authenticated
   candidate selections. It cannot require its own future production-qualified
   result, accept downloaded commands or introduce a public local-fixture bypass.
2. Review the bootstrap chain and actually acquire Python 3.12.6, Git/shell,
   rustup/Rust 1.95.0 components/sysroot, compiler/linker/archiver, Node 24.12.0,
   npm 11.6.2 and the selected runtime tools through source-owned authenticators.
   Node uses signed checksums and independently reviewed keys; Rust uses signed
   manifests/components; gh uses its verified archive and independent bootstrap.
   A colocated checksum alone does not authenticate Fastly or an SDK. Retain raw
   signature/checksum evidence, reviewed trust roots, archive and extracted-tree
   digests and actual selected file identities. SDK/platform trust needs an
   explicit vendor/platform evidence chain. Ambient PATH or PYTHONPATH cannot
   select the interpreter before isolation. setup-python alone supplies no
   qualifying bootstrap proof.
3. Acquire the candidate GNU image by authenticated content identity, record both
   OCI index and selected AMD64 manifest digests, and measure glibc <=2.34 inside
   that builder. Select the three runtime-only images by their actual digests.
   Record native archive/link/sysroot identities: aws-lc-sys 0.42 target-suffix
   overrides and generic/target system-use=0, AWS-LC 5.1 source, ring 0.17.14
   pregenerated source membership, and SQLite 0.30.1 bundled 3.46/pkg-config=0.
4. Network-enabled authenticated acquisition/lifecycle preparation happens before
   compilation. Close source/object-store writes and run positive/negative probes,
   full working compilation, actual Cargo/npm download-attempt negatives and
   actual missing-tool negatives through the same selected boundary. Retain
   outcomes and attempted fetch evidence even when a script catches denial.
5. Outside the compile-denial boundary, run the exact CLI bytes outside checkout
   on clean UBI 9/Ubuntu 22.04/Debian 12 and actual native macOS 15 ARM/Intel.
   Verify GNU symbol ceiling/libraries and Mach-O LC_BUILD_VERSION 13.0. Validate
   the exact final Fastly Wasm with pinned Wasmtime and complete publisher-origin
   response/injection through pinned Viceroy, retaining pre/post byte hashes.
   No health-only success, cross-Intel inference or changed cohort is accepted.
6. Collect the closed observations with actual repository/owner/run/attempt/job
   identities and raw retained evidence. Independently authenticate and review
   those identities, acquisitions, source closure and selections. Only then
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
release build environments`. The local framework commit must use an accurate
title such as `Add release environment qualification checks`.
