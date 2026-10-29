# Attested release binaries and verified consumption Implementation Plan

## Phasing — 2026-10-22

This is the phased rework of [#1262](https://github.com/IABTechLab/trusted-server/pull/1262), frozen at `122b78664b1235c717e9208fd200f9a9b0c335df`. The reviewed contract below is retained as the destination design. This section controls phase applicability; historical statements about first-release gates and PR ownership below refer to the original combined proposal. No new spec approval cycle is introduced by reducing scope.

**Phase 1 contributes to #1235 (D7 settings pending):** tag-triggered builds of Fastly wasm, three native CLI binaries and three production Axum binaries; SLSA provenance and a shipped, attested SBOM for each binary; draft/upload/publish of an immutable release with `SHA256SUMS`; GitHub CLI verification pinned to the reusable `release-build.yml` signer and predicate; and CI enforcement of configuration independence. Operators deploy the verified release bytes. Reproducibility is not a Phase 1 claim or gate. The source-version and fresh JS mechanisms already on main are reused.

Phase 1 publishes SPDX 2.3 for a conservative selected-Cargo-target and installed-npm-build-tree inventory, with the final binary hash. CycloneDX is only an intermediate generator format. Complete composition qualification below remains Phase 2+; Phase 1 does not claim exact retained-code or native-source closure.

**Phase 2+ retains all remaining requirements:** two-build reproducibility, confined recipes, platform qualification, authenticated tool/image acquisition, complete composition/SPDX coverage, candidate collection/index, the custom consumer, publication owner, Task 14 same-commit validation, and Cloudflare/Spin distributions plus full custom-verifier qualification of Axum. These slices remain closed to production selection and signing until separately qualified and authorized.

**Phase 1 Axum extension:** the original four asset names remain unchanged;
three named Axum variants extend this slice to seven binaries and 29 files.
GNU builds enforce a maximum glibc requirement of 2.34; musl is fully static.
The cohort records this explicit extension, but tools/policy and the frozen v1
index schema remain unchanged. Phase 1 emits no index. The later index/consumer
slices need a reviewed seven-artifact amendment, including new target/capability
forms; see the [compatibility audit](binary-attestations-phase1-compatibility.md)
and slice-map annotations.

### Phase 1 review notes

Phase 1 is **part of #1235**, pending D7 maintainer settings. These notes supersede
historical phase applicability, not the retained destination requirements.
Goals, Non-goals, R-COHORT and R-BOUNDS refer to the original four-artifact design;
this slice explicitly includes seven binaries / 29 files, including Axum and
Linux ARM64. Its original four public names stay unchanged; no v1 index is emitted.
The Node producer helpers and inline publisher are interim exceptions to the
retained Rust-producer constraint, replaced by slices 2.5 (recipes/inspection),
2.6 (composition/SPDX) and 2.8 (publication). The frozen Status and tool versions
below describe #1262, not current Phase 1 implementation status.

Current tool pins: Rust 1.98.1, Node 24.20.0, Fastly CLI 16.0.0, Viceroy 0.21.0;
verification uses GitHub CLI 2.102.0. The reusable release-build.yml is the signer.
Slice 2.3 immutable build facts are reused from introducing commit
`bfb697404551a28217af8906024506095bcd9559` and revision
`6d0efda7210983021ab3c9efe8a0a3190e9eb81b`, as retained at the frozen source.
Core's resolver and new CLI/Fastly owner build scripts validate
`TRUSTED_SERVER__RELEASE_BUILD=1` and the full lowercase source SHA.
Cargo supplies the owning target/package; features supply the variant. Tags and
run IDs stay external. CLI now labels package/source/target/variant; Fastly queues
the same immutable facts through existing startup diagnostics in reuse mode only; single mode retains the source response header without per-request build-fact logs. The source audit
permits exactly this release control, GIT_VERSION and Cargo-owned BUILD_TARGET,
not deployment values. CLI source-resource cleanup remains deferred.

Axum binding is runtime-only: `EDGEZERO__ADAPTER__HOST` defaults to loopback;
containers can set `0.0.0.0`, with runtime `EDGEZERO__ADAPTER__PORT` or the compatible `PORT` override. Both values are validated, and conflicting values fail startup. The smoke uses
bridge networking and reaches the process through a published host port.
TLS termination, graceful shutdown and request/resource limits remain follow-up.

Publication requires `RELEASE_PUBLICATION_ENABLED=true` only after maintainers
confirm immutable releases and the v\* tag ruleset. There is no settings token or
prepublication settings API call, following the reviewed signing boundary;
postpublication immutable=true readback fails loudly. Hyphenated tags are
prereleases and never Latest. Stable tags are normal releases with `make_latest: legacy`, allowing GitHub to choose Latest by version/date rather than forcing backports to Latest. Eligibility of
rc-branch tags is an unresolved maintainer decision; Phase 1 does not implement
the destination branch-ancestry/ruleset validator or authorize rc tags.

The phase labels on requirement headings below apply only to the reduced Phase 1 subset described above; all additional clauses and requirements marked Phase 2+ remain deferred. The original assurance profiles, custom verifier, retained evidence graph and reproduction gates do not block the first Phase 1 release. The [implementation plan](../plans/2026-10-13-attested-release-implementation.md#phasing--2026-10-22) contains the acceptance mapping and exhaustive slice inventory.

### Phase 1 implementation checklist

- [x] Copy reviewed spec, plan, feasibility and schemas from `122b78664b1235c717e9208fd200f9a9b0c335df`; preserve linked historical evidence.
- [x] Add `.github/workflows/release.yml` as caller/publisher and reusable `release-build.yml` as builder/signer: seven tag builds, isolated provenance/SBOM signing, draft/upload/publish and immutable readback; optional read-only PR builds.
- [x] Generate intermediate CycloneDX from the selected Cargo target after its locked release build and npm's installed tree from that same build; merge without reference collisions, convert to published SPDX 2.3 and bind the final binary hash. Record conservative build-dependency coverage, including npm build tools; do not claim exact retained-byte or native-source closure.
- [x] Enforce fresh npm install, prohibit skip/prebuilt overrides, audit compile-time config inputs and reject planted operator values in every binary.
- [x] Document exact GitHub CLI provenance/SBOM and immutable-asset verification, native baselines and deployment of verified Fastly bytes; update CLI guide, navigation and changelog.
- [x] Run pure local tests, actionlint, format/domain checks, touched build gates, independent and security review; push this branch and open only the requested draft PR.

| #1235 acceptance item                                 | Phase 1 evidence                                                       |
| ----------------------------------------------------- | ---------------------------------------------------------------------- |
| Reviewed spec and compile-time input list             | Retained R-CONFIG/R-INPUTS, this phasing section; #1262 review history |
| Tag publishes immutable binaries, SBOMs and checksums | release.yml caller/publisher and release-build.yml signer              |
| Provenance, SBOM predicate and asset verification     | release-verification.md; exact predicate and asset checks              |
| Fresh JS; fail missing npm/install/skip               | Existing main build.rs plus clean npm ci and release preflight         |
| Deploy exact bytes                                    | Manual verified Fastly package route; no reproducibility claim         |
| Immutable setting and protected tags                  | Maintainer gate; no setting changes by this PR                         |
| Operator documentation                                | CLI guide, release-verification, CHANGELOG                             |

### Complete remaining-work slice map

The [machine-readable inventory](binary-attestations-slice-map.json) assigns every one of 434 source-diff paths exactly once, with full source commit citations. It is an ownership map, not automatic cherry-pick boundaries: shared Cargo manifests/locks, entry points and workflows require staged hunks reconciled with main. No source work is discarded. Source files later reused in Phase 1 are additionally identified in the PR; the remaining hunks stay with their assigned slice.

| Phase / PR suffix | Scope                                         |
| ----------------- | --------------------------------------------- |
| 2.1               | strict documents policy preflight source      |
| 2.2               | process and native sandbox                    |
| 2.3               | immutable build facts                         |
| 2.4               | js inventory composition materials two builds |
| 2.5               | recipes inspection manual package             |
| 2.6               | composition spdx                              |
| 2.7               | collection workflow jobs candidate index      |
| 2.8               | publication owner                             |
| 2.9               | candidate ci same commit validation           |
| 2.10              | consumer cli                                  |
| 2.11              | docs and format misc                          |

All PR titles begin `Binary attestations: phase 2.N –`; all branches begin `feature/binary-attestations-phase2-N-`. Stack slices in order except the consumer CLI, which may branch directly from Phase 1. Keep every later slice closed: no signing triggers, production selector or qualification bypass. The source PR remains frozen and open until the user decides it is superseded. Cloudflare and Spin distribution, plus full Axum consumer/platform qualification, follow these slices; Phase 1 already distributes the seven binaries described above. Task 14 remains Phase 2.9. Human gates are immutable releases/tag protection and explicit authorization of the first real tag; signing/publication evidence is retained in the compatibility audit and release PR.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Publish and verify the four canonical Fastly/`ts` artifacts required by #1235, with complete evidence and a validated manual Fastly production route.

**Architecture:** A small domain-specific host-only Rust release tool builds and describes bytes; POSIX wrappers select its independently approved native bootstrap; a reusable workflow signs only validated outputs; one caller publishes the complete immutable distribution. A host-only Trusted Server verifier separately establishes build evidence, publication and consumer authorization before retaining verified bytes. Packaging and integrated EdgeZero deployment are separate milestones that consume these boundaries.

**Tech Stack:** Rust 1.95.0/2024, existing workspace serde/serde_json/error-stack/derive_more/clap/sha2/log/tempfile/toml/gix/libc/flate2 as needed, qualified Rust schema/archive/binary parsers, Node 24.12.0/npm 11.6.2, Vite/Rollup, SPDX 2.3, GitHub CLI 2.97.0, actions/attest 4.2.2, Fastly CLI 15.1.0. CLI consumer tasks retain their existing tokio/reqwest architecture.

---

## Authority, baseline and scope

Planning baseline: `44c5b5620bfb1f82e5b959ab5ba8ec69c7091e52`, inspected 2026-10-13. The governing [contract](../specs/2026-10-09-1235-release-attestation-design.md), [feasibility register](../specs/2026-10-12-1235-release-attestation-feasibility.md) and [index schema](../specs/schemas/release-index-v1.schema.json) remain drafts awaiting maintainer approval. Delivery direction on 2026-10-13 is to develop/review the spec, plans and implementation together on `spec/1235-release-attestation` in PR #1262, with no new Trusted Server PR. The reviewed plan guides implementation here; maintainer contract/code approval remains required before merge, and the plan does not qualify a release or authorize live publication/deployment. Task 1 records this delivery contract and the unchanged qualification prerequisites.

#1214 and #1213 are present at this baseline. Extend their merged private JS output/prebuilt interface and source-version mechanism; do not reimplement the historical audit snapshots. Source deployment currently delegates to EdgeZero and no release workflow/verifier exists.

The initial cohort is exactly:

| Canonical asset               | Required assurance                                         | Blocking qualification                                                             |
| ----------------------------- | ---------------------------------------------------------- | ---------------------------------------------------------------------------------- |
| `trusted-server-fastly.wasm`  | `provenance-sbom-repeatable`; default, no reusable sandbox | Final complete-byte A/B equality, real SBOM/runtime and verified manual submission |
| `ts-x86_64-unknown-linux-gnu` | `provenance-sbom`                                          | glibc ceiling 2.34; clean UBI 9, Ubuntu 22.04, Debian 12 startup                   |
| `ts-aarch64-apple-darwin`     | `provenance-sbom`                                          | Explicit loader floor 13.0; actual macOS 15 ARM execution                          |
| `ts-x86_64-apple-darwin`      | `provenance-sbom`                                          | Same loader floor; actual macOS 15 Intel execution                                 |

Every selected canonical Build A, SBOM, signature and publication check is mandatory. CLI Build B is a bounded measurement, not a shipping equality requirement. No Linux ARM64, installer packages, other adapter releases, retained-input archive, operator reconstruction or official offline deployment is added by this plan.

Two linked plans keep independent work out of these closure criteria:

- [CLI packaging attestation integration](2026-10-13-cli-packaging-attestation-integration.md): reconcile #1292, package exact canonical bytes and authenticate member identities. Later releases only after the required spec/schema amendments are approved.
- [Fastly prebuilt EdgeZero integration](2026-10-13-fastly-prebuilt-edgezero-integration.md): upstream prepared payload, integrated online production/staging and durable recovery; AC-8 is separate from initial #1235 closure.

## Rust migration and current execution boundary

The user's 2026-10-14 correction prohibits introduced Python release tooling. The [migration design addendum](../specs/2026-10-14-release-tooling-rust-migration.md) selects `crates/trusted-server-release`: a Rust 2024/1.95.0 host-only workspace member, excluded from default members and independent of core, CLI and JS crates. The existing host-only codegen member is the precedent. The tool is internal producer tooling, not a fifth canonical asset, a generic attestation framework or a publisher abstraction. `ts` remains the consumer in Tasks 17–20: CLI → core → JS makes the CLI unsuitable as the JS/bootstrap producer.

Preserve `collect-js.mjs` and the implemented Node composition hook. POSIX wrappers use an independently approved-source, OS-native Rust toolchain to build/select the release tool before executing it; a colocated SHA file or self-reported source label supplies no authority. Developer Cargo commands below establish local behavior only. Production needs the authenticated bootstrap, full qualified source/dependency/tool closure and reviewed native OS adapters. No language migration waives native SDK/tool acquisition or authenticating the original Fastly CLI archive through independently reviewed vendor-source evidence.

- [x] **M0: Remove superseded tooling.** Introduced Python release code and validator requirements are removed. Deleted prototype behavior is not implemented or fixed by removal; pending Rust regressions below remain mandatory.
- [ ] **M1: Port primitives and preflight (Tasks 2–3).** Strict document/schema utilities, fixed policy/environment checks, local Git object/worktree authentication and the helper-closure fingerprint are implemented. Git commands use bounded process supervision and a shared command deadline. Independently authenticated bootstrap/tool acquisition and qualification remain incomplete; the closure fingerprint supplies no acquisition authority. Register each private test module before applying the nonzero-selection guard.
- [ ] **M2: Port material/process/OS boundaries (Tasks 7–9).** Establish original archive/Git identities, bounded child execution, kill/reap/cleanup, private staging, source/object read-only protections and real native denial controls. Preserve independent all-cohort remote receipts separately from rehashing tools actually installed in the current execution environment.
- [x] **M3: Restore real JS orchestration (Task 7).** Perform two fresh offline installations, actual lifecycle observations and denied missing-tool probes, all tracked bundle comparisons and both full registered JS/type-check suites. Only actual successful execution constructs a private typed completion bound to source, tools, settings and retained A bytes; parsed JSON, paths or booleans cannot construct it. The full Linux A/B execution test passed in ordinary PR CI on 2026-10-15 (1,275/1,275 tests per side, 13 byte-identical modules, six private roots removed); see the [two-build evidence report](../../../scripts/release/qualification/2026-10-15-js-two-builds.md). This is candidate CI execution, not hosted release qualification.
- [ ] **M4: Restore canonical recipes and inspection (Tasks 8–9).** Candidate recipes are implemented for all four assets and pass ordinary CI with `release_qualification: false`; qualification, the closed `build.sh` interface and the production reproduction profile remain pending. Compose actual Cargo preparation, compile, final acceptance and copying; collect Task 10 metadata/tree/native evidence through that same boundary before hosted qualification. Local tiny artifacts cannot qualify the full product or actual macOS 15/GNU cohort.
- [ ] **Then Tasks 10–22.** Implement the remaining Rust producer modules, workflow contracts, consumer verification and manual route in dependency order. Complete every selected qualification and full CI gate before release enablement/handoff.

The producer helper MUST itself be built and executable inside the selected GNU builder at glibc <=2.34, using an independently approved image/toolchain/material channel. An ambient Ubuntu 24 build cannot be assumed to load in UBI 9. Bind helper target, loader/runtime, source/dependency closure and actual native executable identity before execution. macOS helpers require the approved native CPU/SDK/floor-compatible chain. Initial shell/hash/Rust/Cargo authority is independent of the tool; record and authenticate rustup only if it is actually used (direct signed-component installation does not require it); the tool cannot authenticate itself. Native Rust helper/staged POSIX drivers implement socket and lifecycle probes, without language substitutions.

Current implementation includes strict documents/policy, local source/preflight checks, bounded process supervision, owning build facts, checked JS inventory and Node composition. Original archive authentication, bounded private extraction and lifecycle declaration review are implemented. Final-artifact inspection and experimental native boundaries are implemented, with separate consumer evidence verification. The experimental macOS profile now kills and rejects descendants that escape the process group, including with a scrubbed environment, by identifying its sandbox instance after each run; it remains unqualified and does not own descendant cleanup the way a container does. Native boundary controls pass ordinary Ubuntu 24.04 and macOS 15 CI; they do not qualify complete release recipes or authenticated tool selections. Original npm and Cargo Git material verification and two-installation JS orchestration are implemented; the full Linux A/B execution test passed in ordinary PR CI. Candidate recipes for all four assets are implemented and pass ordinary CI (unqualified, `release_qualification: false`). Qualification, the closed `build.sh` interface, the production reproduction profile, authenticated bootstrap/acquisition and hosted qualification remain incomplete. Production wrappers and qualified selection remain closed.

## Historical progress references

Original task progress is preserved at source commit `122b78664b1235c717e9208fd200f9a9b0c335df` in this plan's original path. It is evidence for later slices, not Phase 1 completion. Last confirmed full green: `b6f5d92e4`, run `39639053390`; four-asset candidate join: run `39577910126`. Other cited runs: `39273777887`, `39290139247`, `39417433680`, `39420011152`, `39540057202`.

## Work breakdown and ownership

Use successive focused commit/review groups within PR #1262: (1) input/fact/JS guards, (2) qualified recipes and composition, (3) collection/signing/publication, (4) verifier/manual route/docs. Each depends on its predecessor; tag enablement waits for all four. No new Trusted Server implementation PR is created. Build scripts can be proved locally before production YAML exists. No intermediate PR may enable an incomplete release trigger.

| File group                                                                                                                    | Responsibility                                                                                           |
| ----------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------- |
| `scripts/release/release-cohort.json`, `release-tools.json`                                                                   | Reviewed fixed identities, profiles, approved RC branches and qualified environment pins                 |
| `crates/trusted-server-release/Cargo.toml`, `src/main.rs`                                                                     | Host-only producer dispatch, with no product-crate dependencies                                          |
| Release tool `src/documents.rs`, `policy.rs`, `preflight.rs`, `source.rs`                                                     | Strict bounded JSON, fixed contracts, authenticated source and explicit environment                      |
| Release tool `src/materials.rs`, `process.rs`, `boundary.rs`, `preparation.rs`, `javascript.rs`, `qualification.rs`           | Original acquisitions, bounded execution, real OS isolation, fresh JS and native qualification           |
| `scripts/release/bootstrap.sh`, `build-js.sh`, `build.sh`, `collect-js.mjs`                                                   | Approved native Rust bootstrap, fixed POSIX adapters and retained Node observer                          |
| Release tool `src/recipe.rs`, `inspect.rs`, `composition.rs`, `spdx.rs`, `collection.rs`, `distribution.rs`, `publication.rs` | Fixed recipes, actual composition, SPDX, transfers, distribution and sole-owner publication support      |
| Release tool `src/package.rs`                                                                                                 | Bounded trusted-source manual Fastly inspection; later approved packaging adapters                       |
| `.github/workflows/release-validation.yml`, `release-build.yml`, `release.yml`                                                | Same-commit gates; isolated builds/signing; sole draft/publication owner                                 |
| `crates/trusted-server-cli/src/release/`                                                                                      | Host-only consumer verifier: policy, strict documents, bounded gh, GitHub publication and evidence graph |
| `crates/trusted-server-cli/src/commands/release.rs`                                                                           | Public read-only `ts release verify` adapter                                                             |
| Release tool private tests, `tests/commands.rs`, `tests/fixtures/`; CLI/core/JS tests                                         | Producer negatives, public-command process tests, consumer fixtures and actual qualification             |

Execute each listed negative fixture as a separate red/green microcycle: write one case, run its focused filter, implement the smallest relevant behavior and rerun it, then move to the next case. A task groups related work for review/commit; its fixture list is not one giant implementation step.

Task 2 utilities and the retained Rust/Node guard work are implemented. Remaining Rust module names below are proposed, not existing APIs. Create only modules needed by the current stage, without placeholder files. Private unit tests live alongside `src/<module>.rs` under `#[cfg(test)]`, or in registered `src/<module>/tests.rs`; fixture data lives in `crates/trusted-server-release/tests/fixtures/`. `tests/commands.rs` drives the public binary. Do not make internal APIs public merely for integration tests. Use concrete error-stack reports, top-level imports, descriptive assertions, documentation on public APIs and project commit conventions.

Keep direct dependency features minimal. Reuse workspace dependencies only where an actual stage needs them; the producer has no dependency on core, CLI or JS. Maintained TAR/ZIP, schema and binary-format parsers are justified by existing bounded archive, Draft 2020-12/Draft 7 and ELF/Mach-O/core-Wasm requirements. Research exact maintained versions from primary sources and qualify semantics/acquisition before pinning; this plan invents no replacement version or digest. Library parsing does not replace archive limits or source authentication.

### Task 1: Freeze approved scope and reconcile the packaging owner

**Phase: 2+ (historical destination task).** Phase 1 uses the reduced implementation checklist above, rather than completing this task.

**Files:** Modify this plan and the governing spec only for approved corrections; reference PR #1292's `docs/superpowers/specs/2026-10-12-863-cli-release-packaging-design.md` and `docs/superpowers/plans/2026-10-12-cli-release-packaging.md` after their owning branch is available.

- [ ] Read the current spec revision; record its commit and the same-branch/same-PR delivery direction. Work through reviewed tasks in PR #1262; obtain maintainer contract/code approval before merge. Do not require a separate spec merge or open another implementation PR.
- [ ] Record one publication owner, `.github/workflows/release.yml`, and one signing identity, same-revision `release-build.yml`. #1292 MUST NOT independently publish an overlapping immutable tag while #1235 assets are still pending.
- [ ] Confirm the current four-artifact cohort and three CLI baselines. Initial #1235 delivery may precede packaging on a different new tag. Adding package records or ARM64 requires the linked plan's approved delta, never a covert schema expansion here.
- [ ] Settle one strict producer control channel: `TRUSTED_SERVER__RELEASE_BUILD=1`, injected by trusted recipe after environment sanitation, recorded as an input, rejected if present with another value. This is a fact/input guard, not the prohibited parallel JS fallback mode. Name the checked prebuilt inventory input `TRUSTED_SERVER__TSJS_PREBUILT_INVENTORY`.
- [ ] Confirm permission policy remains runtime-variable; conflicting #1045/#1084 code blocks release enablement. Record the owner and dependency rather than designing a new permission transport.
- [ ] Review milestone exits with maintainers in PR #1262. Commit any approved documentation delta as a focused commit on this branch with `Clarify release implementation prerequisites`.

**Exit:** An engineer can identify approved source, scope, producer controls and the single release authority; no unspecified cross-PR ownership decision remains.

### Task 2: Establish Rust producer fixtures and strict document utilities

**Phase: 2+ (historical destination task).** Phase 1 uses the reduced implementation checklist above, rather than completing this task.

**Create:** `crates/trusted-server-release/Cargo.toml`, `src/main.rs`, `src/error.rs`, `src/documents.rs`, private registered `documents::tests`, `tests/commands.rs`, `tests/fixtures/README.md`.
**Modify:** workspace `Cargo.toml` membership only; keep default members unchanged. Retain `scripts/release/tests/fixtures/README.md` as the migration pointer.

- [x] Write private Rust tests for duplicate keys at every depth, nonfinite/out-of-range JSON, invalid UTF-8/surrogates/BOM/trailing controls, unsafe names, depth 65, oversized role inputs, negative/noninteger lengths and ambiguous duplicate identities. Include lexical integer overflow in opaque nested fields and top-level values (`18446744073709551616`, `-9223372036854775809`), decimal and exponent float overflow, exact integer boundary controls, and large number-like text inside strings with escaped quotes/backslashes that MUST NOT be rejected as a numeric token.
- [x] Register tests, derive the host target as below, and run `cargo test -p trusted-server-release --target "$HOST_TARGET" documents::tests` with the nonzero-selection guard. Expect compilation/assertion failure for absent behavior, not an empty successful selection.
- [x] Implement bounded byte-first loading, SHA-256 of original bytes, safe flat names, deterministic output and atomic writes. Use a recursive Serde visitor/seed that rejects duplicate object keys **before insertion**, rather than deserializing into `serde_json::Value` first. A representative parser core is:

```rust
use std::fmt;

use serde::de::{DeserializeSeed, Error as _, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};

struct StrictSeed {
    depth: usize,
}

impl<'de> DeserializeSeed<'de> for StrictSeed {
    type Value = Value;

    fn deserialize<D>(self, deserializer: D) -> Result<Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for StrictSeed {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("bounded unambiguous JSON")
    }

    fn visit_map<A>(self, mut access: A) -> Result<Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        if self.depth > 64 {
            return Err(A::Error::custom("JSON container depth exceeds 64"));
        }
        let mut object = Map::new();
        while let Some(key) = access.next_key::<String>()? {
            if object.contains_key(&key) {
                return Err(A::Error::custom("duplicate JSON key"));
            }
            let value = access.next_value_seed(StrictSeed {
                depth: self.depth + 1,
            })?;
            object.insert(key, value);
        }
        Ok(Value::Object(object))
    }

    fn visit_seq<A>(self, mut access: A) -> Result<Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        if self.depth > 64 {
            return Err(A::Error::custom("JSON container depth exceeds 64"));
        }
        let mut values = Vec::new();
        while let Some(value) = access.next_element_seed(StrictSeed {
            depth: self.depth + 1,
        })? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }

    fn visit_bool<E>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_str<E>(self, value: &str) -> Result<Value, E> {
        Ok(Value::String(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Value, E> {
        Ok(Value::String(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Value, E> {
        Ok(Value::Number(Number::from(value)))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Value, E> {
        Ok(Value::Number(Number::from(value)))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Value, E>
    where
        E: serde::de::Error,
    {
        Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("nonfinite JSON number"))
    }

    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
}
```

The implemented utility chooses the mandatory role byte limit before reading/allocating and reject BOM/invalid encoding. Before `deserialize_any`, validate numeric tokens across the **whole document**, including opaque fields and top-level values, with a string-aware lexical scanner that skips quoted text and handles escapes. Negative integer tokens MUST fit `i64`; nonnegative integer tokens MUST fit `u64`. Decimal/exponent tokens MUST parse as finite `f64`; overflow fails. `serde_json` otherwise converts some overflowing integer tokens to finite floats, so the visitor's finite-float check alone is insufficient. Use the existing scanner in `crates/trusted-server-js/build/inventory.rs` as a behavioral reference without adding a JS-crate dependency. Then start with `StrictSeed { depth: 1 }` and require `serde_json::Deserializer::end()` so grammar/trailing data fails. Closed role validators additionally enforce stricter safe-integer limits and reject negative/noninteger lengths. The representative visitor above is the ambiguity parser, not the complete strict front end; schema validation never substitutes for lexical prevalidation, byte bounds, exact-original hashing or semantic joins.

- [x] Qualify a maintained Rust schema evaluator. The locked `jsonschema` 0.58.6 is a locally checked evaluator candidate, not an approved production tool pin: use minimal features, explicit dialect and offline construction, with only known retained schemas and no arbitrary schema-file CLI override. Governing schemas require Draft 2020-12 `prefixItems`/`items` positive/negative cases; the official SPDX 2.3 schema uses Draft 7. Disable remote/file retrieval and reject unregistered references.
- [x] Retain the official SPDX schema at source commit `aadf3b0b8dbbabdb4d880b0fc714255fea436ff7`, path `schemas/spdx-schema.json`, 45,312 bytes and SHA-256 `239208b7ac287b3cf5d9a9af23f9d69863971102a5e1587a27a398b43490b89b`. Confirm raw-byte identity and dialect before use. Candidate compatibility or a schema-valid toy document does not establish complete real-artifact coverage.
- [x] Rerun documents/private atomic-write negatives and command-process tests; run host-target clippy/format. Commit only after authorized review with `Add strict Rust release document utilities`.

### Task 3: Encode cohort, tools and fail-closed release preflight

**Phase: 2+ (historical destination task).** Phase 1 uses the reduced implementation checklist above, rather than completing this task.

**Retain:** `scripts/release/release-cohort.json`, `release-tools.json`, `crates/trusted-server-cli/release-policy.json`.
**Create as needed:** release tool `src/policy.rs`, `preflight.rs`, `source.rs` and private registered tests; wire the approved POSIX bootstrap adapter without weakening pending selections.

- [ ] Port cases for missing/extra artifacts, profiles changed after failure, unknown target/feature, missing pins and every skip value (`""`, `"0"`, `"1"`). Include wrapper, Cargo config, NODE_OPTIONS, native selectors, unexpected ignored/nested validation files, modes/links and source mutation.
- [ ] Run `cargo test -p trusted-server-release --target "$HOST_TARGET" preflight::tests` and `source::tests` separately with the nonzero-selection guard; expect the missing Rust behavior to fail.
- [ ] Preserve exact tracked producer/consumer policy bytes, concrete fixed identity/profile/action/bounds and the raw source-owned tools input. Runtime consumer enforcement remains Tasks 17–20. No downloaded policy override or silent schema/cohort/capability amendment is permitted.
- [ ] Map four asset IDs to fixed package/bin/target/features; never accept JSON commands. Preserve Rust/Node/npm, gh 2.97.0, attest 4.2.2/full SHA and Fastly 15.1.0. Populate binary/image/SDK digests only from authenticated acquisition and Task 8 measured review. The production preflight/selector requires all qualifications by default and MUST fail on pending/null pins; omitting a flag MUST NOT return success. A separately named audit diagnostic MAY describe incomplete inputs but MUST NOT construct authorization state. No JSON qualified boolean or dummy digest is accepted.
- [ ] Audit the actual inherited environment before clearing it; reject skips and unreviewed channels, then build an allowlisted child environment. Do not hide hosted/setup overrides under `env -i`. Accept vetted setup exports such as `RUSTFLAGS`/`VCPKG_INSTALLATION_ROOT` only with explicitly reviewed, qualified values and path identities; otherwise reject. Remove publisher prefixes and inject only approved commit/control facts. Derive epoch from approved Git committer time. Check tracked and unexpected source-only files, including the full JS validation library, modes/links before and after execution; preserve narrow reviewed coordination exceptions.
- [ ] Select generic and target-suffixed AWS-LC settings, bundled SQLite, Cargo config/home layers, sysroot/tool paths and Vite mode. Set `AWS_LC_SYS_USE_SYSTEM=0`, `LIBSQLITE3_SYS_USE_PKG_CONFIG=0`; reject alternate selectors rather than setting weaker defaults.
- [ ] Authenticate approved source and acquisitions into private typed states. Keep independent all-cohort remote qualification receipts separate from current-environment installation identity: rehash the actual selected native binaries/SDK inputs immediately before use. Serialized `authenticated`/`qualified`/`completed` booleans, source labels or checksum files cannot construct either state.
- [ ] Rerun negatives, including fictional operator-config independence and wrong-bootstrap source; run host lint/format. Commit `Define fixed Rust release inputs and preflight policy`.

**Interface:** after independently approved native bootstrap, `trusted-server-release preflight --cohort scripts/release/release-cohort.json --source-commit <approved-commit> --require-qualified`; exit 0 only for a complete selected contract, otherwise fail before preparation/signing. A developer may exercise it with `cargo run -p trusted-server-release --target "$HOST_TARGET" -- preflight --cohort scripts/release/release-cohort.json --source-commit "$APPROVED_SOURCE_COMMIT" --require-qualified`, which provides no official tool or release assurance.

- [ ] Reject unlisted RC branches and a malicious candidate cohort that lists itself; require independently reviewed tracked branch/governance inclusion, actual remote-tip ancestry and active ruleset/bypass validation. Current eligibility is exactly `["main"]`; reject local remote-ref substitutions.
- [ ] Audit the true Git common directory, including linked worktrees, for alternates, grafts, shallow/replacement/config/environment channels. Explicitly reject unsupported linked indirections rather than partially checking them. Preserve bounded detailed Git errors. Reject an untracked extensionless `rust-toolchain` before bootstrap/rustup selection, including alongside the tracked TOML pin. Join approved source to the original JS inventory commit.

### Task 4: Reconcile immutable build facts and CLI source resources

**Phase: 2+ (historical destination task).** Phase 1 uses the reduced implementation checklist above, rather than completing this task.

**Modify:** `crates/trusted-server-core/build.rs`, `build_support/git_version.rs`, `tests/git_version_resolve.rs`, `src/lib.rs`; `crates/trusted-server-cli/src/run.rs`, `src/prebid_bundle.rs`.
**Create:** core `src/build_info.rs`; CLI `build.rs`, `tests/release_build_facts.rs`.
**Modify when adding the owning facts surface:** `crates/trusted-server-adapter-fastly/src/main.rs`; retain existing version-header/service-version behavior.

- [x] Add strict full-commit tests: missing/malformed/branch/tag values fail in release mode without local-git fallback; development behavior stays covered. Add two external tags/same source yielding identical facts, owning-target/variant checks and compiled-path fallback rejection.
- [x] Run `cargo test -p trusted-server-core --test git_version_resolve` and `cargo test -p trusted-server-cli --target "$HOST_TARGET" --test release_build_facts`; expect the new behavior to fail.
- [x] Extend the merged #1213 helper using the approved release control. Derive target from Cargo `TARGET` through `TRUSTED_SERVER__BUILD_TARGET`, package version from the owning product, and default/reuse variant at the Fastly owner. Expose typed immutable facts without importing verifier/publisher keys into core. Do not embed tag, run ID, time or final checksum.
- [x] Set clap's version text to `ts package=<pkg> source=<identity> target=<triple> variant=default`. Read Fastly owning package/target/variant facts through existing startup diagnostics. Remove unread variant and source-owner re-emissions; core remains the source identity owner. Official releases compile out `find_js_lib_dir`'s literal developer checkout fallback; preserve explicit/current-checkout resource discovery. Keep the tracked example config as a fictional scaffold, never active settings.
- [x] Run core fact/template tests, CLI tests/clippy and Fastly version-header tests. Outside-checkout help/version/config-init MUST pass; Prebid source-dependent commands return documented prerequisites. Commit `Derive immutable release build facts`.

### Task 5: Authenticate the existing prebuilt JS interface

**Phase: 2+ (historical destination task).** Phase 1 uses the reduced implementation checklist above, rather than completing this task.

**Modify:** `crates/trusted-server-js/build.rs`, `build/bundle_set.rs`, `src/lib.rs` test inclusion.
**Create:** `crates/trusted-server-js/tests/release_prebuilt.rs`.

- [x] Write process/fixture tests for prebuilt mode plus any skip presence, absent inventory, wrong source/digest/size, duplicate IDs, unsafe filenames, missing/empty/extra modules and source changed during copy.
- [x] Run `cargo test -p trusted-server-js --target "$HOST_TARGET" --test release_prebuilt` with the nonzero-selection guard; expect current prebuilt behavior to fail these guards.
- [x] Extend the existing bundle-set validator; require `TRUSTED_SERVER__TSJS_PREBUILT_INVENTORY` in release mode and validate exact input and private copied bytes. Track both paths/control values with Cargo rerun directives. Reject skip before selecting prebuilt/source path; no TSJS_RELEASE_BUILD parallel mode and no read from old `dist/`.
- [x] Preserve ordinary development's supported fresh source build and #1214's inventory-absent prebuilt path, including unrelated non-bundle extras and supported directory aliases. An inventory-present development build validates inventory source syntax/hash but MUST NOT compare it with the override/tag/branch display label. In release Rust jobs require prebuilt inputs and prohibit npm invocation; a fake failing npm proves it is never run.
- [x] Run JS crate tests, Fastly/CLI target checks and prebuilt negatives. Commit `Bind release JS inputs to a checked inventory`.

### Task 6: Collect JS contributions without changing bundle bytes

**Phase: 2+ (historical destination task).** Phase 1 uses the reduced implementation checklist above, rather than completing this task.

**Modify:** `crates/trusted-server-js/lib/build-module-options.mjs`, `build-all.mjs`.
**Create:** `lib/test/release-composition.test.mjs`, `scripts/release/collect-js.mjs`.

- [x] Write temporary IIFE fixtures for type-only/external Prebid import, eliminated module, transitive npm contributor and plugin/virtual helper. Build with/without recording and assert identical final JS bytes.
- [x] Run `cd crates/trusted-server-js/lib && npx vitest run test/release-composition.test.mjs`; expect missing recording behavior.
- [x] Add an opt-in Rollup contribution hook at `moduleBuildOptions()`. Join source/package origins to the lockfile, retain installed/lifecycle/build-tool materials separately, and hash final files after Vite/esbuild. Suppress ambient `.env` loading through the reviewed release invocation; explicit production mode alone is not sufficient.
- [x] Emit sorted version-1 inventory with source, module names/lengths/digests and origins; ambiguous contributing ownership fails. State conservative post-Rollup reachability limits.
- [x] Run byte-preservation and ownership negatives plus the full JS test/format checks. Commit `Record release JS composition evidence`.

### Task 7: Prepare and compare two independent JS builds

**Phase: 2+ (historical destination task).** Phase 1 uses the reduced implementation checklist above, rather than completing this task.

**Retain/modify:** `scripts/release/build-js.sh`, `bootstrap.sh`, `collect-js.mjs`. **Create:** release tool `src/materials.rs`, `process.rs`, `boundary.rs`, `preparation.rs`, `javascript.rs` and private registered tests; add public process fixtures to `tests/commands.rs`.

- [x] Write tests for archive/revision mismatch, stale extracted tree, prohibited install download, two bundles differing by one byte, test failure and changed source. Assert no generated output exchange between A/B. Prohibited download and missing-tool denial are actual runtime controls exercised in the full Linux fixture; that fixture also asserts each side exposes only its own private roots and shares only read-only originals and tools.
- [x] Run `cargo test -p trusted-server-release --target "$HOST_TARGET" materials::tests`, `process::tests`, `boundary::tests`, `preparation::tests` and `javascript::tests` separately with the nonzero-selection guard; expect missing Rust orchestration.
- [x] Preparation verifies registry/git/npm materials and required native esbuild package archives, then freshly materializes each installation. Run pinned `npm ci --offline --include=dev --no-audit --no-fund` with verified freshly prepared caches in two independent workspaces under the selected denial boundary; review/record lifecycle scripts and fail any lifecycle tool fetch. Rust migration MUST reprove this with actual full local execution and fresh fixtures; Task 8 MUST qualify the actual hosted JS boundary before Task 15 may use it officially. No restored node_modules/generated JS. Read-only original sources/object stores and authenticated archive/tool inputs remain outside private installation outputs. Timeouts/cancellation/pipe limits kill and reap descendant process groups; bounded Docker stop/remove and all temporary-profile/resource cleanup run on success and failure. Bundle under qualified network denial; compilation-time downloads fail. The passing run used an ordinary-CI Docker denial boundary; qualifying that boundary remains Task 8.
- [x] Compare final core and all tracked integration files byte-for-byte. Retain only A plus its original inventory after equality. Construct a private typed `JsBuildCompleted` only from the actual successful two-install/test/build/control/comparison path, binding authenticated source/tools/settings, inventory raw digest and retained module bytes. Rehash at handoff; a checked inventory or JSON completion assertion cannot substitute. Unconditionally inject `TRUSTED_SERVER__RELEASE_JS_TESTS=1` into BOTH actual A/B Vitest processes, independent of inherited developer flags; run every registered test/type check and reject a zero selected suite; verify source-only trees remained intact.
- [x] Prove a missing prepared tool cannot fetch a substitute; rerun negatives and full JS suite. Require both actual fresh full registered JS/type-check suites and all tracked final bundles before `Build and compare shared Rust release JS inputs`; fixture-only counts remain separate. Landed as `Build and compare two fresh offline JavaScript installations` plus follow-up fixes; Linux CI evidence is in the [two-build evidence report](../../../scripts/release/qualification/2026-10-15-js-two-builds.md).

- [x] Port actual regressions for immutable macOS source/object regular files with `nlink == 1`; reject pre-existing hard links and prove write-through/restoration cannot bypass the boundary. Restrict signals to `same-sandbox`: own-child positive, outside same-user sibling and orchestrator denial negatives. Parent/child/grandchild install lifecycle, Cargo/compiler/proc-macro/native children MUST inherit the same OS-denied controls. `nlink == 1` scans, pre-existing hard-link rejection, run-time hard-link denial, run-time mutation detection, `same-sandbox` signal positives/negatives, grandchild socket denial and escaped-descendant socket denial pass on macOS 15 CI. A descendant that leaves its process group, even with a scrubbed environment, is identified by its per-run sandbox instance, killed and the run rejected; only processes started during the run are eligible. R-JS uses the single shared Linux JS job, so no macOS install-lifecycle path exists; Task 9 must reprove Cargo/compiler/proc-macro/native child inheritance through its actual recipes.
- [x] Port Docker boundary regressions: use Docker's default private PID namespace (no invalid `--pid=private`), and actually execute positive/denial/recipe controls instead of testing argv alone. Keep BOTH host seccomp profiles and an empty Docker config disjoint from every container-writable root; scan before creating them and rehash BOTH before EVERY host command. Select fixed authenticated Docker client/config/Unix daemon, no TTY/plugins/context or environment overrides, and read-only anchors. Reject escaping and dangling aliases in the CONTAINER namespace. Linux container tests execute actual positive, denial and confined commands in ordinary CI. Cleanup `stop`/`rm` still rehashes every input first: a changed client or config blocks the command, while a changed seccomp profile (which those commands do not read) is recorded as a failure and cleanup proceeds.
- [x] Bound child pipes/deadlines; timeout/cancellation/group kill MUST reap descendants and clear token/provider/injection environment. Record real errno without inventing `ETIMEDOUT`. Use host-independent tool/path/environment fixtures; no ambient shim or interpreter supplies trust.

### Task 8: Qualify real platform environments before fixing recipes

**Phase: 2+ (historical destination task).** Phase 1 uses the reduced implementation checklist above, rather than completing this task.

**Retain:** `scripts/release/qualification/results.schema.json` and reviewed seccomp baseline/license. **Create:** release tool `src/qualification.rs`, native probe dispatch in `src/boundary.rs`, private registered tests; update `release-tools.json` only with authenticated measured pins.

- [ ] Write schema/preflight fixtures rejecting wrong architecture, missing SDK/tool digest, failed startup, permitted confined sockets and invented host compatibility.
- [ ] Run `cargo test -p trusted-server-release --target "$HOST_TARGET" qualification::tests` with the nonzero-selection guard; expect unimplemented Rust qualification checks.
- [ ] Execute throwaway full recipes on the actual proposed GitHub-hosted environments: digest-pinned GNU builder <=2.34, matching macOS 15 ARM/Intel, explicit loader floor 13.0. Record host, rustup paths, compiler/linker/SDK/native identities. Docker absence on a developer Mac is not replaced by an inferred Linux result.
- [ ] On each recipe run an unconfined socket success control and a confined denied socket probe, then full compilation under the same qualified boundary. Linux container uses process network isolation; macOS profile MUST permit required filesystem/process/tool activity while denying sockets. Archive/tool acquisition is separate and authenticated; actual offline install lifecycle hooks execute under denial.
- [ ] Record actual startup/link evidence and complete Fastly runtime request, not only host health. Missing Intel execution or GNU baseline stops qualification. A different platform/cohort requires reviewed spec changes, not a silent downgrade.
- [ ] Pin authenticated observed digests/bootstrap data and add result validation; commit `Qualify controlled release build environments`.

**Gate:** This task produces measured pins, not placeholder configuration. It can run only with the separately authorized disposable hosted resources. It does not publish official tags or deploy a real publisher service.

Implementation sequencing: review the local validation framework and candidate boundaries first, then implement the shared Task 9 recipes and Task 10 composition captures. Complete the authenticated candidate/production adapters and shared-JS producer binding before measuring the executable closure. Hosted qualification MUST invoke those same recipes with independently authenticated candidate selections; it cannot require its own future qualification result. Production selection remains closed until the hosted evidence and measured pins are reviewed. Local framework completion does not satisfy the hosted checkboxes above.

- [ ] Join observations to the reviewed catalog at the approved source: exact selection digest, tool versions/authentication methods, complete helper compiled closure and every path-dependency manifest, including `crates/trusted-server-openrtb/Cargo.toml`, tool-version files, authenticated executable digests, SDK identities and raw receipts. A code change invalidates the closure; labels/checksums cannot self-qualify. Hash the actual selected shell; do not assume `/bin/sh --version` works on dash. Unsupported pending selections keep production closed.
- [ ] Derive container resource limits no higher than actual host capacity (a private Ubuntu runner may have two vCPUs). A Cargo jobs A=4/B=2 variation does not authorize `--cpus 4` on that host. Use actual controls and whole-product builds, not deleted prototype claims.

### Task 9: Implement fixed fresh Rust recipes and native inspection

**Phase: 2+ (historical destination task).** Phase 1 uses the reduced implementation checklist above, rather than completing this task.

Fastly host-tool selection increment (2026-10-22): the Linux x86_64 candidate
plan explicitly selects `/usr/bin/cc` as its host Rust linker driver and
`HOST_CC`, `/usr/bin/ar` as `HOST_AR`, and the measured Rust tree's
`lib/rustlib/x86_64-unknown-linux-gnu/bin/rust-lld` as the Wasm linker.
The host-triple Cargo linker setting applies to host artifacts during an
explicit-target build; no unstable host configuration or compiler wrapper is
introduced. Host C selectors do not establish target C compilation or prove
that the selected drivers executed. Existing GNU and Apple selections stay
separate.

The existing staged measurement helper gains a fixed private Fastly mode,
without a new command in `main.rs`. It runs before and after Cargo in the
existing denied sibling sandbox, excluding the writable target root. It checks
the actual Rust host, resolves and hashes selected files inside the image,
and independently hashes the compiler's reported assembler candidate. The
receipt binds the helper, exact plan and build/measurement boundary identities;
changed before/after observations fail. This changes only the opaque
`platform.native_tools` receipt payload, not the checked composition fields or
candidate-index companion formats. Production selection remains closed.

**Retain/modify:** `scripts/release/build.sh` and approved native bootstrap. **Create:** release tool `src/recipe.rs`, `inspect.rs`, private registered recipe/native/final-Fastly tests and public-command process fixtures.

- [ ] Test unknown asset/variant, reused target output, source/material mutation, unselected native library, GLIBC symbol ceiling violations, runner-only paths and Mach-O deployment-target mismatch. Add role-specific variation fixtures: missing selected B variation, normalized values misreported as varied, unsupported user variation claimed as passed and incorrect observed concurrency/umask.
- [ ] Run `cargo test -p trusted-server-release --target "$HOST_TARGET" recipe::tests` and `inspect::tests` separately with the nonzero-selection guard; expect missing Rust recipe/inspection behavior.
- [ ] Implement a closed interface: `build.sh --asset <fixed-id> --role A|B --source-commit <approved> --js-dir <absolute> --js-inventory <absolute> --materials <fresh-root> --output <fresh-root>`. Select package/target/features from reviewed source; JSON never supplies executable commands.
- [ ] Implement checked A/B ambient profiles in the recipe and record them in `release-tools.json`: different external checkout roots, A umask 022/B umask 027, UTC versus a qualified alternate timezone, distinct qualified locale settings, different observed start times and A jobs 4/B jobs 2 within the qualified insensitive envelope. Vary execution user only where Task 8 proves support; otherwise record it unsupported without claiming it changed. Capture both ambient inputs and effective compiler values after sanitation/remapping; fixed internal locale/path/epoch values are normalized, not varied. No unreviewed caller overrides can choose profiles. Tests assert genuinely observed supported differences and reject dishonest/omitted records.
- [ ] Apply Task 8's qualified OS/container denial profile in the actual production `build.sh`, not just the qualification prototype. Every production invocation MUST record a successful unconfined socket control and denied confined socket probe, then execute Cargo, compiler, proc-macro, build-script and native-tool children inside that same denied boundary. Authenticated archive/tool acquisition alone may fetch materials; offline installation lifecycle hooks and all compilation run under denial. A build-script/lifecycle download-attempt fixture MUST fail, and a missing or ineffective confinement proof rejects the output; `--frozen` is not the boundary.
- [ ] Build explicit packages with `--release --frozen --message-format=json-render-diagnostics` after preparation. Keep `debug = 1`. Capture compiler/build-script output separately from diagnostic stderr; copy only final output after any reviewed transformations. Do not invoke Fastly source build/annotation. Never strip after evidence/signing.
- [x] Enforce final-byte path checks inside recipe completion using its own bound host/execution roots, not only candidate fixtures. Require bounded nofollow size/hash verification, reject `/boundary/` and actual build roots, and require a remapped prefix. For GNU builds, close unbound writable `/tmp` and shared-memory storage and reject image-declared volumes. Keep macOS's temporary-path inspection unchanged. Docker execution starts only an acknowledged full container ID and confirms cleanup by that ID.
- [x] Prove the refined GNU boundary with actual denied writes, successful bound-`TMPDIR` write/read/removal, the real timeout-cleanup regression, full fresh JS and Cargo builds, final CLI inspection and all three clean startup images. The observed Chromiumoxide `/tmp/aws/lib` lookup constant is runtime data, not evidence of build identity; its actual ELF context was measured before refining the GNU-only path policy. Passed in ordinary PR CI with identical Linux CLI canonical/reproduction bytes and all three startups; see the [Linux CLI candidate report](../../../scripts/release/qualification/2026-10-16-linux-cli-candidate.md). These ordinary-CI results do not qualify production pins or authenticated acquisition.
- [ ] Inspect ELF/Mach-O libraries and paths, run clean Linux startup fixtures and real ARM/Intel macOS startup outside checkout. With differing fictional operator config removed by preflight, require identical sanitized effective source/tool/build inputs for every artifact and reject embedded operator sentinels/config fallback. Require complete executable equality for Fastly; preserve native nonmatching outcomes as measurements rather than adding a native repeatability shipping gate. Separately show runtime configuration can change without rebuilding.
- [x] Before accepting each final Fastly A/B output, validate actual core-Wasm format with a pinned maintained validator and run a positive publisher-origin response/injection fixture through Viceroy using those exact final bytes. The fixture MUST NOT invoke Cargo/Fastly build or modify the wasm. Assert final hash unchanged, retain runtime outcome, and test non-wasm/component/wrong-ABI/health-only success rejection. These are required producer gates, separate from Task 8 throwaway qualification and later live deployment. Implemented and accepted for both canonical and reproduction outputs in ordinary PR CI, with a closed-origin control rejected; see the [runtime gate report](../../../scripts/release/qualification/2026-10-16-fastly-runtime-gate.md). The Viceroy digest is recorded, not yet independently pinned.
- [ ] Run recipe/native negatives and one actual artifact per selected environment. Commit `Build and inspect canonical release executables`.

Progress (2026-10-16, no Task 9 exit yet): the closed recipe, checked A/B ambient profiles, role umasks, path translation, source replacement, strict Cargo message selection and `RecipeCompleted` are implemented. Ordinary PR CI built `trusted-server-fastly.wasm` role A through `PreparedRecipe::execute` inside the Linux denial boundary and proved build-script, grandchild and procedural-macro denial; see the [confined Cargo candidate report](../../../scripts/release/qualification/2026-10-16-confined-cargo-candidate.md). That first build used unauthenticated acquisition and `cargo vendor` manifests. The producer now normalizes Git package manifests itself from the verified Git tree; later builds record eight producer-normalized and zero caller-derived manifests, and an evidence-only differential against `cargo vendor` found no differences (see the [producer-normalized manifests report](../../../scripts/release/qualification/2026-10-16-producer-normalized-manifests.md)). Acquisition remains unauthenticated. The plan now remaps the checkout, vendor, Cargo home, Rust tool (sysroot) and target roots to fixed prefixes through one `--config target.<triple>.rustflags` entry, which merges after reviewed target rustflags (environment rustflags stay rejected, a tracked `build.rustflags`, which target rustflags would supersede, is refused, as is any extensionless `.cargo/config`). The remapped roots must be pairwise disjoint, because rustc applies the last matching prefix, and the remap is bound by the plan digest. On Linux both roles see identical container paths (`/boundary/...`), so the checkout-root variation is host-only; the remap is proven on the final Fastly bytes by a scan that rejects `/boundary/` and every host build root and requires a fixed remapped prefix. Literals built from `env!("OUT_DIR")` or `env!("CARGO_MANIFEST_DIR")` are not remapped by rustc and are caught by the same scan. A pure comparison requires one role A and one role B Fastly build over identical source, lock, materials, JS handoff and tools, an honest ambient pairing and complete final-byte equality, reporting sizes, digests, the first differing offset and its Wasm section otherwise. The ordinary CI job now builds roles A (umask 022, four jobs) and B (test-only alternate profile: umask 027, two jobs, distinct checkout root and start time) from one JS handoff and compares them. Both builds were byte-identical in ordinary PR CI, and the scan found the remapped prefixes and no host or container paths; see the [canonical and reproduction equality report](../../../scripts/release/qualification/2026-10-16-fastly-canonical-reproduction-equality.md). The Linux `ts` candidate now builds through the GNU baseline, passes final-path/ELF inspection and all three clean startup images, and has a byte-identical reproduction; see the [Linux CLI candidate report](../../../scripts/release/qualification/2026-10-16-linux-cli-candidate.md). Viceroy final-byte acceptance is also proven in ordinary CI as recorded above. The two Apple `ts` candidate recipes and native startups have since passed in ordinary CI (runs 39273777887 and 39290139247, recorded below); qualified pins remain open, and production role B stays unqualified. Qualified and production selections stay rejected by the execute gate. Only the `cfg(test)` ordinary-CI candidate selection accepts the macOS boundary, which uses post-run-reaped descendant cleanup (approved 2026-10-19); that is candidate evidence, not Task 8 qualification, and production code cannot name that selection. In code, role A is `Role::Canonical` (Build A) and role B is `Role::Reproduction` (Build B); only `Role::parse` and `Role::spec_label` keep the external `A`/`B` spelling used by `--role` and the tracked `ambient_profiles` keys.

### Task 10: Record selected Cargo units and native materials

**Phase: 2+ (historical destination task).** Phase 1 uses the reduced implementation checklist above, rather than completing this task.

Fastly selected-file evidence (2026-10-22, local implementation; hosted proof
pending): the receipt retains selected and resolved paths, SHA-256 and size for
the host C compiler driver, host archiver, configured host assembler candidate
and selected Wasm Rust linker. The host Rust linker selects the same measured
C driver. SPDX projects these four raw files only as build tools, with no
invented acquisition, download, license or executed-helper claims. The bound
Rust installed-tree measurement remains separate.

After successful denied measurement, the compiler, archiver and linker
host-path gaps can be removed for this explicitly selected Linux profile.
The assembler is merely compiler-configured, so its role gap remains. Expected
candidate SPDX gaps are exactly `fastly-original-acquisition-unverified`,
`fastly-tool-closure-unmeasured`, `native-material-coverage-incomplete`,
`native-tool-host-path-unresolved:assembler` and
`native-tool-host-path-unresolved:build_tools`. Missing or inconsistent required
measurements fail rather than manufacturing evidence. Other profiles retain
their existing gaps. This does not complete Task 10, original acquisition,
qualification or qualified SPDX generation. The next hosted Fastly candidate
must confirm these gaps and the unchanged real producer-receipt check before
this increment is described as integrated.

Local verification passed 216 pure tests: 70 SPDX, 68 composition, seven Fastly
tool tests, 18 GNU measurements, 14 recipe-plan, 13 tool-selection, four
source-replacement, 20 completion and two unchanged candidate-index golden
tests. The explicit-selection assertion failed against the previous bare
driver selection and passed after restoring the implementation. Filesystem
tests independently check raw SHA-256/size, changed bytes and escaping aliases;
parser tests reject malformed compiler queries. Strict host-target
all-target/all-feature Clippy, all-feature documentation and formatting passed.
Independent specification and quality reviews found no remaining blockers.
The minimal Fastly expectations in `recipe/tests/plan.rs` were explicitly
authorized; their Linux branch and real boundary execution remain hosted checks.
No process-spawning or ignored tests were run locally.

Remaining dependency audit (2026-10-22, `41560cfd9`): the open capture exit
below requires conservative native/tool coverage. The specification does not
require an exact compiler DAG or mandatory execution tracing: an inventory of
all permitted relevant inputs, coupled with enforced access containment, can
also establish closure. The current recipes do not establish that containment.
The next implementation must address these dependencies rather than treating
another successful snapshot hash as closure:

| Remaining evidence                      | Dependency and current limitation                                                                                                                                                                                                                                                                                                                                    |
| --------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Fastly host tools and supporting inputs | At audit head `41560cfd9`, five host-tool roles were unresolved. The increment above measures selected Linux host compiler/archiver/linkers while retaining assembler, build-tools and full-closure gaps. Other Fastly profiles keep all five role gaps. Supporting inputs still require Task 9/10 closure work; GNU CLI receipts do not cover Fastly automatically. |
| GNU tool/input closure                  | Task 9 must constrain or account for relevant inputs beyond the inventory roots; Task 10 must inventory and join that closure. `recipe/gnu/image.rs` explicitly leaves other files available. Configured helper paths are observations, not exhaustive dependency coverage.                                                                                          |
| Apple tool/library closure              | Task 9 must constrain or account for relevant inputs outside measured directories; Task 10 must measure and join them. `recipe/apple.rs` permits broad developer-directory access. Measuring the selected toolchain's entire `usr/lib` does not resolve outside-directory or transitive dependency inputs.                                                           |
| Unreadable GNU privileged helpers       | Tasks 9/10 must obtain measured content or establish reviewed, enforced noncontribution/exclusion treatment for the three helpers. Acquisition evidence alone cannot close their content gap. Retain metadata-only observations and the gap until proven; do not infer hashes or expand privileges.                                                                  |
| Original acquisition                    | Tasks 3/8 must bind authenticated Rust/tool/SDK/image distributions to the selected inputs. Installed-tree hashes do not authenticate original acquisition; pending pins in `scripts/release/release-tools.json` remain pending.                                                                                                                                     |
| Platform qualification                  | Task 8 must qualify the actual platform, boundary and recipes against the selected closure. Passing ordinary-CI candidates does not construct the closed qualified tool or sandbox types.                                                                                                                                                                            |
| Complete owning composition             | Task 10 must resolve the first four material-coverage rows before supplying complete composition evidence. Acquisition and production qualification are independent additional requirements; complete measured coverage alone would not open `spdx::generate_from_completed`, which deliberately rejects current candidates.                                         |

Paths in the table are relative to `crates/trusted-server-release/src/` unless
shown otherwise. Content measurement, dependency closure, authenticated original
acquisition and qualification are distinct proofs. Acquisition and qualification
rows describe production dependencies, not prerequisites for measuring complete
material coverage. None is substituted for another, and this audit changes no
completeness or qualification state.

**Create:** release tool `src/composition.rs`, private `composition::tests`, `crates/trusted-server-release/tests/fixtures/composition/`.

- [x] Add fixtures for host/target dual-role packages with different unit features, ambiguous source IDs, dev-only graph entries, unknown native origin, system substitution and prepared-but-discarded CLI JS.
- [x] Run `cargo test -p trusted-server-release --target "$HOST_TARGET" --bin trusted-server-release composition::` with the nonzero-selection guard; the scoped 2026-10-21 run selected and passed 63 pure tests.
- [x] Join explicit-target compiler/build-script messages with metadata/tree and selected material identities; preserve each observed unit's features/context. Package union features cannot stand in for effective per-unit features. Retain raw observations for audit; do not claim an exact compiler dependency DAG.
- [ ] Capture AWS-LC/ring/SQLite native source and output archive hashes/link directives, Rust sysroot and external platform libraries. Validate generic/target-specific selection against the recipe. Distinguish embedded components, external requirements and build tools.
- [x] Inspect final files conservatively for complete JS module strings. Record absence explicitly; Fastly and CLI do not need the same retained set. Do not force unused JS into CLI.

Scoped audit and hardening (2026-10-21): the checked items above describe existing
implemented collectors, not newly established full coverage. Named fixtures in
`composition/tests.rs`, `graph/tests.rs`, `native/tests.rs` and
`javascript/tests.rs` already exercise the listed cases. The dual-role feature
cases are synthetic; retained real cross-build fixtures establish host-only
membership, not a captured dual-role build or exact compiler dependency DAG.
AWS-LC/ring/SQLite observations already bind verified original distributions,
prepared source/subtrees, consuming contexts/features, static output archives
and link directives. Rust installed-tree measurements and logical external
runtime requirements remain distinct from original acquisition and complete
native/tool coverage; that capture exit stays unchecked.

A new negative fixture replaces a native archive between checking its magic and
hashing its path. The previous implementation accepted a valid header from one
inode and a digest from a different non-archive file. The collector now hashes
the checked header and remaining bytes through the same held descriptor, with
bounded reads, owning-attempt checks and matching final descriptor/path metadata.
No reported path or digest becomes trusted without local measurement.

GNU receipts already record incomplete closure and unqualified acquisition.
Candidate SPDX now names both limitations as `gnu-tool-closure-unmeasured` and
`gnu-original-acquisition-unverified`, independently of the existing
`gnu-privileged-helper-content-unmeasured` gap. These names are absent for
non-GNU receipts and remain sorted/unique. No serialized receipt fields, schemas,
workflow assertions, completeness flags or production constructors changed.
Hosted Fastly/Linux/Apple candidate validation and emitted-gap inspection are
recorded below for `e91d75b74`. Complete coverage and qualification stay
false; this does not complete Task 10 or touch the parallel Task 12 adapter.

Local verification for this increment passed 63 composition tests with the
nonzero-selection guard and 62 SPDX tests, strict native all-target/all-feature
Clippy, documentation generation and formatting. The inode-swap regression and
each new GNU gap assertion failed before its corresponding fix and passed after.
Independent spec and quality review approved the scoped changes; plan wording
was corrected to distinguish the implemented collector from the historical
missing-collector test step. No child-process or ignored tests were run locally.

Hosted gap follow-up (2026-10-21): [run 39565110219](https://github.com/IABTechLab/trusted-server/actions/runs/39565110219)
has head `7ab7f96e71c896b214e6d09e760b7da7ea22d1b4`. Completed candidate logs
for Fastly [119105403584](https://github.com/IABTechLab/trusted-server/actions/runs/39565110219/job/119105403584),
Linux CLI [119105404005](https://github.com/IABTechLab/trusted-server/actions/runs/39565110219/job/119105404005)
and Apple ARM [119111703005](https://github.com/IABTechLab/trusted-server/actions/runs/39565110219/job/119111703005)
record built PR merge source `f5bac2786585169cf597139069d05dd47513d9f1` and
`material_native_coverage_complete: false`. Linux alone reports both
`gnu-tool-closure-unmeasured` and `gnu-original-acquisition-unverified`, alongside
its existing privileged-helper and incomplete-native-material gaps. Fastly and
ARM report neither new GNU gap; their existing host-tool/Apple exclusions remain.
At this evidence check Intel [119111702865](https://github.com/IABTechLab/trusted-server/actions/runs/39565110219/job/119111702865)
was still running its denial fixture. Its final candidate gap list and the overall
run result remain pending; no all-four confirmation is inferred.

Intel follow-up (2026-10-21): successful [run 39577910126](https://github.com/IABTechLab/trusted-server/actions/runs/39577910126),
head `d774ee47c274257fa0a088280f7df37ed530020c` (including `04be89638`),
completed Intel candidate [119154065062](https://github.com/IABTechLab/trusted-server/actions/runs/39577910126/job/119154065062).
Its built PR merge source is `fb42d8268af93c011959d032c81bfa581306c7c2`.
The final candidate SPDX reports only `apple-coverage-excluded:toolchain_libraries`,
`apple-tool-closure-unmeasured` and `native-material-coverage-incomplete`;
neither GNU closure/acquisition gap appears, and
`material_native_coverage_complete` and `release_qualification` remain false.
Together with the three completed logs above, this closes the four-platform
gap-placement follow-up across the recorded runs, not complete native coverage.

Progress (2026-10-19, partial Task 10 only): candidate recipes retain an unfiltered Cargo metadata identity catalog and run an explicit-target tree query under the same denied boundary and selected tool/environment/source replacements as the build. Completion joins actual compiler messages to exact metadata package/target identities, preserves observed host/target unit features and build-script outputs, and requires the selected final executable to appear exactly once. Raw metadata/tree and normalized observations, including query plan digests, are retained privately and rehashed at handoff; candidate measurements keep `material_native_coverage_complete: false`. Ordinary CI at `456d15005` passed Fastly A/B equality and final-byte Viceroy acceptance with 255 observed packages, 377 units and 39 build-script executions per side, and Linux CLI capture/startup with 486 packages, 649 units and 62 build-script executions. Those runs precede the new component receipts below; they do not validate their full-product integration (later runs 39273777887, 39290139247 and 39417433680 do, as an ordinary-CI candidate). Apple full-product acceptance has since passed in ordinary CI (runs 39273777887 and 39290139247, recorded under Task 9 and below).

The candidate completion path now also reconciles normal/build metadata and tree edges against observed compiler membership, rejects ambiguous source joins and dev-only paths, and keeps potential graph membership separate from actual compiled units. It does not claim an exact compiler unit dependency DAG. Verified package origins join opaque compiled IDs to approved workspace manifests or original registry/Git materials. Reviewed AWS-LC, ring and bundled SQLite collectors bind original source, prepared source trees, static output archives, observed features and link directives; unknown native contributors and system substitutions fail. Final-byte JS checks classify complete retained modules conservatively: Fastly requires every prepared module, while CLI absence remains build-material evidence. Bound source checks cover every tracked input before/after execution and immediately before sealing. Native working inputs are rehashed at sealing; retained immutable receipt bytes are verified before cleanup and at subsequent handoffs. No retained-input archive is implied. Tool/sysroot/SDK/external-requirement coverage, all-cohort real component validation and qualification remain open, so these components do not complete Task 10.

The verified vendoring path records a read-only package index on `PreparedCargoMaterials`, keyed by vendor directory. Registry entries retain the locked source, original archive SHA-256 and prepared manifest SHA-256; Git entries additionally retain the complete selector/revision, repository/package trees, package path, committed manifest blob/SHA-256 and any producer-normalized manifest SHA-256. Entries are constructed only after package writing succeeds. The origin join selects only observed packages from this larger prepared-material index; preparation alone never establishes contributor membership.

A read-only full-workspace graph check caught and fixed a real input-size mismatch: the CLI's `--no-dedupe` tree contains 163,493 occurrences, exceeding the separate 65,536 compiler-message limit. Trees now have their own 1,048,576-occurrence bound within the unchanged 16 MiB byte bound. An indexed name/version lookup preserves conservative ambiguity rejection. The actual Fastly and Linux CLI potential trees parse successfully; this parser check uses synthetic selected-root membership and does not establish actual full-product compiled membership for the new receipts.

Candidate platform receipts additionally retain the measured installed Rust tree digest and explicit Cargo/compiler/selected target standard-library roles. Typed Apple exports retain the measured clang, archiver, linker and SDK-settings identities and versions, with explicit exclusions for SDK headers, stubs, frameworks, helper tools, libLTO and compiler resources (narrowed for measured selections; see the 2026-10-21 Apple toolchain note below). Accepted ELF/Mach-O/Wasm facts supply artifact-bound observed runtime requirements; these are logical dependencies, not installed runtime-library hashes or provenance. The collector independently checks artifact, target and Apple SDK agreement, compares the retained Rust digest with its bound identity, and rechecks inputs before retaining evidence. Linux host-PATH native tools remain unresolved, original acquisition is not inferred, and coverage remains incomplete. The component/integration verification batch passes 252 focused tests, native and Linux cross-target Clippy, formatting and documentation generation; independent review passed. All four candidate cohorts have since executed these receipts in ordinary CI (runs 39273777887 and 39290139247, recorded below); qualification remains pending.

Ordinary CI at `3fef2c5f0` subsequently passed the complete Fastly composition/platform receipt path, A/B byte equality and Viceroy acceptance. Its Linux CLI candidate stopped at a generic native-feature rejection. Investigation found a concrete mismatch with the locked AWS sources and resolved CLI feature graph: `all-bindings` is optional, and the selected `prebuilt-nasm` feature cannot activate prebuilt objects on the selected Linux/macOS platforms. The narrowly corrected policy accepts these reviewed profiles while retaining original-source, archive, environment and static-link checks. New regressions cover the profiles and bounded package/context/feature diagnostics. Full native candidate reruns remain required; the original generic error did not identify the exact failing unit.

The corrected receipt path subsequently passed all four ordinary-CI candidate recipes in [run 39273777887](https://github.com/IABTechLab/trusted-server/actions/runs/39273777887), over merge source `cace4c508159bc7502d359e0cce49fef47d61f8b`. Fastly, Linux CLI and Apple ARM completed byte-identical A/B builds and their required runtime checks. Intel completed its mandatory canonical build, inspections and native startup; its optional reproduction exhausted the confined 25-minute wall budget while compiling and was recorded as a measurement-only build failure. No Intel equality result is inferred. These are candidate integration results for that source, not authenticated acquisition, complete composition coverage or qualification of subsequent changes.

Typed owning access now rechecks retained observations and requires sealed component receipts before exposing observed units, verified package origins and potential graph facts. The typed view borrows its owner and exact normalized-receipt digest; it changes no raw receipt bytes and grants no completeness capability. Its real recipe checks run after source/target cleanup. Selected SDK-content enforcement now stages a separately owned copy with independent regular files, preserves safe internal aliases and modes, binds a structured-tree digest into Apple identity, and selects that copy through explicit C/Rust sysroot arguments, immutable boundary roots, remaps and final-byte path rejection. The [SDK input audit and smoke](../../../scripts/release/qualification/2026-10-19-apple-sdk-inputs.md) records the local installed-SDK pass and subsequent hosted ARM/Intel candidate acceptance. Compiler/helper/sysroot and original-acquisition closure remain separate work.

Hosted follow-up (2026-10-20): [run 39290139247](https://github.com/IABTechLab/trusted-server/actions/runs/39290139247) finalized success for head `df4a6e5c30fe8c93df68748ead77a3bc559d16b2`, building merge source `506d8ead69757556e1bd8ed29b578d0257d05169`. All four actual candidate fixtures accepted sealed typed composition, verified Rust-package SPDX projection and retained candidate exports that survive build-root cleanup. Fastly, Linux CLI and Apple ARM reproduced identically; Fastly also passed final-byte Viceroy acceptance. Intel canonical inspection/startup/export passed, while optional reproduction failed on its time bound, so no Intel equality evidence exists. Observed package/unit/build-script counts were Fastly `255/377/39`, Linux and ARM `486/649/62`, and Intel canonical `487/650/62`. Both Apple recipes used the detached SDK. This closes the previously pending hosted integration checks for these specific increments; full native/tool closure, complete SPDX generation, authenticated acquisition and release qualification remain open. The receipts retain both completeness and qualification as false.

Apple and Intel candidate details (2026-10-20, verified from job logs; see the [Apple CLI candidates report](../../../scripts/release/qualification/2026-10-20-apple-cli-candidates.md)): in both runs the ARM recipe built byte-identical canonical and reproduction executables and every canonical Apple build, inspection and native startup passed. The Intel optional reproduction failed in both runs, for different recorded reasons: run 39273777887 exceeded the confined build wall-clock deadline while compiling, and run 39290139247 recorded "source preparation cancelled or timed out". The later runs 39417433680 (`bfab8ef20`) and 39420011152 (`1719cf15f`) recorded Intel reproduction `identical`, with the Intel candidate test taking 3,552 and 3,305 seconds against its 60-minute test deadline. Because the timing margin is thin and no code change addressed it, this is a measurement, not a resolved Intel equality gate; native B stays optional. All of this is ordinary-CI candidate evidence with `release_qualification: false`. The macOS boundary reports `descendant_cleanup: "post-run-reaped"`, which only the `cfg(test)` ordinary-CI selection accepts.

The subsequent GNU increment replaces ambient compiler selection for the Linux CLI candidate with source-owned `/usr/bin/gcc` and `/usr/bin/ar` selectors for both host and target C builds, an explicit Rust linker driver, and exact AWS-LC cc-backend controls that prevent fallback to CMake. A staged Rust helper runs inside the same denied Docker boundary before and after Cargo. It records selected/configured compiler, assembler, archiver, linker and compiler-resource identities plus a bounded conservative installed-image file inventory, including headers/libraries and reviewed loader/alternatives configuration. Image hardlinks use a separate held-descriptor policy; strict owned-tree and host-document policies remain unchanged. The receipt binds the helper, boundary, profile and invocation, rejects drift, and remains retained through composition sealing. Installed-file coverage does not establish which helpers executed, authenticated acquisition, or complete native closure. Those flags remain false, and the actual GNU helper/profile integration has since run in hosted CI, passing at `bfab8ef20` after two fixes (see the GNU snapshot and measurement notes below).

GNU snapshot of reviewed root-only helpers (2026-10-20, `e5a780d5c`): the hosted Linux CLI candidate at `7be86d935` failed with "cannot open image file: Permission denied" (job 118460869596, run 39371724671), because the unprivileged helper cannot open three root-only privileged helpers in the UBI 9 builder. The snapshot now accepts a permission-denied file only at the exact reviewed paths `/usr/libexec/utempter/utempter` (setgid), `/usr/sbin/unix_update` and `/usr/sbin/userhelper` (setuid), owned by root with no group/other write and no other-read bit and unchanged since first inspection. Each is recorded by mode, owner and size rather than content and listed as `content_unmeasured_files`, which must be a sorted subset of the reviewed paths. Every other unreadable file, including an execute-only compiler helper, still fails closed, and selected build tools must stay readable and hashed. This records a coverage gap; it measures no helper content and qualifies nothing.

GNU measurement in a sibling sandbox (2026-10-20, `bfab8ef20`): the next hosted run, at `e5a780d5c`, failed because the post-build image measurement ran in the build boundary, whose unchanged pre-run writable scan rejects Cargo hard links inside the target directory ("has 2 hard links; a link outside the root would bypass write denial", job 118494982595, run 39382624785). Both GNU measurements now run in a sibling Linux container boundary derived from the build boundary with the same image, settings, read-only and tool roots and every writable root except the target directory. The sibling gets a fresh fingerprint, records the boundary it derived from and proves denial with the staged probe. Cargo queries and the build keep the build boundary and proof; receipts record both fingerprints. Ordinary-CI run 39417433680 (head `bfab8ef20`) passed the Linux CLI candidate (job 118609761876): byte-identical reproduction and three clean startups, recorded in the [GNU follow-up report](../../../scripts/release/qualification/2026-10-20-linux-cli-gnu-follow-up.md). This is candidate evidence only.

Apple toolchain identity (2026-10-21, uncommitted, Tasks 9/10 candidate only): the ordinary-CI Apple selection now also measures, in place below the selected `clang`'s toolchain `usr` directory, a structured-tree digest of the helper directory beside `clang` (files, modes and relative alias targets), the single versioned clang resource directory (`lib/clang/<version>`; any second real version directory, foreign alias or other entry fails closed) and the SHA-256 of `lib/libLTO.dylib` (missing, aliased or non-regular fails closed). The trees are bounded by entry, file and total byte limits and reject special files and absolute, dangling or escaping aliases. Vendor hard links are accepted because the tree is not owned; every file is rehashed instead. Nothing is copied, executed or resolved through `xcrun`. The receipt is part of the Apple identity record and therefore the plan digest, and it is rechecked at every Apple verification point and at evidence sealing. Candidate exports drop the `tool_helpers`, `liblto` and `clang_resource_directory` exclusions only when the receipt is present, and add a `toolchain_libraries` exclusion for other toolchain libraries such as `libtapi`. The `apple-tool-closure-unmeasured` gap stays: which helpers clang actually ran is unmeasured, and the developer directory remains a broad read/execute system root. The platform SPDX projection adds `libLTO` and the two trees as components. Pure fictional fixtures cover this locally; the real Xcode layout is exercised only by the ordinary-CI macOS jobs, which have not yet run it. It grants no qualification.

Apple measurement follow-up (2026-10-21): the earlier uncommitted/unexecuted
status above is superseded by committed `7d045ac83` and the successful ARM/Intel
candidate jobs in run 39540057202. Their candidate entries retain
`apple-coverage-excluded:toolchain_libraries` and `apple-tool-closure-unmeasured`;
passing the measurement path does not establish complete closure or acquisition
authenticity. See the [candidate index report](../../../scripts/release/qualification/2026-10-21-candidate-release-index.md).

Timing follow-up (2026-10-21): the Intel optional reproduction timeout recorded
in run 39273777887 recurred in run 39540057202, after historical successful
Intel comparisons. The ordinary-CI selection now supplies a 45-minute confined
Cargo wall through `ToolSelection::build_wall` (`b4c8e0c0f`), rather than the fixed
25 minutes. See the [schedule/cache report](../../../scripts/release/qualification/2026-10-21-native-reproduction-schedule.md).
The running speedup CI is not yet a completed acceptance result; neither deferral
nor the changed budget establishes repeatability, complete coverage or qualification.

GNU receipt follow-up (2026-10-21): capture now validates the exact nine selected
file roles and eight compiler-query identities, not only their counts. Selected
drivers match the fixed profile; configured cc1, collect2, assembler, linker and
compiler-runtime files join their recorded query paths. The raw sysroot response
is rechecked against the supported image-root forms. Bare assembler/linker names
must match the fixed image candidate's resolved path, digest and size; `/bin`
aliases of the same file and explicit alternative image-helper paths remain
supported. These are installed/configured observations, not execution tracing or
general PATH-resolution proof; tools-root helpers remain outside this profile.

The audit found existing AWS-LC/ring/SQLite collection already retains original
registry archive identities, prepared source/subtree hashes, static-link
directives and rehashed output archives. Rust tool trees, measured Apple tools/
SDK inputs and GNU installed/configured files already project into SPDX. No new
native source fact, external-library identity or authenticated acquisition is
invented by strengthening the receipt checks. Native material coverage stays
incomplete. The existing Fastly unresolved-host-tool, GNU closure/acquisition/
privileged-helper-content and Apple closure/toolchain-library gaps remain open;
complete sysroot/tool execution closure and external platform inputs still need
additional owner measurements and qualification. Hosted acceptance of this
guard is recorded below; none of these changes completes Task 10.

Scoped verification: nonzero guards selected 18 pure GNU tests, 67 pure SPDX
tests and two unchanged producer golden checks; all passed. Strict host clippy,
documentation and owned-file formatting pass. Independent spec and quality
reviews approve this increment without blockers, including preservation of
explicit GCC paths and same-identity aliases. No child-process or ignored tests
were executed locally. The earlier Task 14 run is not evidence for this guard.

Hosted guard acceptance (2026-10-21): [Run Tests 39616416485](https://github.com/IABTechLab/trusted-server/actions/runs/39616416485)
and [Run Format 39616416604](https://github.com/IABTechLab/trusted-server/actions/runs/39616416604)
succeeded at head `e91d75b7447c9611d6e1ccb5dc7c2dce07377ff6`. Candidate
logs record built PR merge source `624cef618ffadbfad72c41c9072d8c5f5c1f4f64`.
Actual SPDX gaps were inspected for all four assets:

- Fastly [119278571153](https://github.com/IABTechLab/trusted-server/actions/runs/39616416485/job/119278571153):
  `native-material-coverage-incomplete` and `native-tool-host-path-unresolved:`
  for `archiver`, `assembler`, `build_tools`, `compiler` and `linker`.
  Canonical and reproduction SPDX report the same gaps and identical document
  bytes; document equality remains a measurement.
- Linux CLI [119278571303](https://github.com/IABTechLab/trusted-server/actions/runs/39616416485/job/119278571303):
  `gnu-original-acquisition-unverified`, `gnu-privileged-helper-content-unmeasured`,
  `gnu-tool-closure-unmeasured` and `native-material-coverage-incomplete`.
- Apple ARM [119287733613](https://github.com/IABTechLab/trusted-server/actions/runs/39616416485/job/119287733613)
  and Intel [119287733492](https://github.com/IABTechLab/trusted-server/actions/runs/39616416485/job/119287733492):
  `apple-coverage-excluded:toolchain_libraries`, `apple-tool-closure-unmeasured`
  and `native-material-coverage-incomplete` on each asset.

Every candidate keeps `material_native_coverage_complete: false` and SPDX
`release_qualification: false`; optional native reproductions are absent, not
invented successes. No unexpected gap difference was found. This closes hosted
acceptance of `e91d75b74`, not Task 10/11 completeness. That run predates
`5a98c18ad`'s strict real-receipt parser; parser-inclusive acceptance is recorded
in the 2026-10-22 note below.

Cargo script presence hardening (2026-10-21): graph sealing now requires every
observed custom-build package to have execution evidence, and the execution
package/context set to match the observed non-custom consumers of those
packages. A compiled host script cannot substitute for its target execution;
dual-role packages need evidence in both consuming contexts. Multiple executions
within one context remain permitted, and effective unit features remain separate.
This is package/context presence consistency, not proof that every feature-specific
execution within a context was retained or an exact compiler dependency DAG.
No serialized receipt field, SPDX rendering, completeness flag or collector
fixture changes. Coverage remains incomplete.

Two negative fixtures were observed failing for their intended reasons before
the production checks: missing execution evidence and a host execution with only
a target consumer. A dual-role regression also covers a missing second context,
matching host/target executions and multiple executions in one context. Scoped
nonzero guards passed 66 composition tests, 67 SPDX tests, 18 GNU tests, 46
collection tests and two unchanged producer golden checks. The collection
transfer module was excluded because it includes subprocess/network fixtures;
one other ignored collection test was not executed. Strict host clippy,
documentation and owned-file formatting passed. Independent review found no
blocker and confirmed the package/context limitation stated above. Real-build
acceptance of this graph guard is recorded in the 2026-10-22 note below.

Selected Apple library measurement (2026-10-21): the in-place toolchain receipt
now records the SHA-256 of the exact `usr/lib/libtapi.dylib` when present,
alongside the existing `libLTO`, helper and resource measurements. A missing
leaf is recorded explicitly as `libtapi_sha256: null`; unreadable files, aliases,
special files and exhausted bounds are errors. Present files use the existing
no-follow descriptor/stamp checks, a 128 MiB limit and the attempt's cancellation
and deadline. Remeasurement rejects content changes, appearance and disappearance.
The new field is inside the collector's opaque `platform.apple.toolchain`
subtree; all strictly checked receipt fields remain unchanged.

This measures one selected toolchain library, not its execution or loading.
Other toolchain libraries, runtime inputs, original acquisition and the full
tool closure remain unresolved. The existing `toolchain_libraries` exclusion,
`apple-tool-closure-unmeasured` gap and incomplete coverage flags are retained.
Hosted acceptance of this additional measurement is recorded below for
`39639053390`; run `39637275502` contains the preceding graph checks only.

Verification of this increment selected and passed 66 composition, 68 SPDX,
19 Apple toolchain, 18 GNU, 46 collection and two unchanged producer golden
tests with nonzero guards. The collection transfer module and ignored tests
were excluded under the local execution restrictions. Strict host clippy,
documentation and owned-file formatting passed. Independent spec and code
quality reviews approved the increment. Review also strengthened the absence
check: a missing or aliased parent cannot be recorded as a missing library.
The existing vendor-tree intermediate-directory swap limitation still applies;
no stronger isolation guarantee is claimed.

Hosted receipt acceptance (verified 2026-10-22): [Run Tests 39637275502](https://github.com/IABTechLab/trusted-server/actions/runs/39637275502)
succeeded at head `416b43f43c5d7e69c08bca32753240abded8523c`, building PR
merge source `17251a872e295e4f13047efd14b10077944620fb`. [Run Tests 39639053390](https://github.com/IABTechLab/trusted-server/actions/runs/39639053390)
succeeded at head `b6f5d92e4bc4483f81b1e0d8a376e7d67c9da8fd`, building PR
merge source `c7b1a9cb04ab8abf8e4fa2d5198c4c1337a96b1f`. Format,
Integration Tests and CodeQL also succeeded at both heads. All four actual
candidate fixtures selected and passed one test each in both runs. Each invokes
the unconditional real-receipt check in `check_owned_export_after_cleanup`;
none of the eight job logs contains the receipt-check panic. This establishes
producer/collector receipt acceptance for these builds, not complete coverage.

The full job logs were inspected for exact candidate SPDX gap lists:

- Fastly: [416 job 119351108924](https://github.com/IABTechLab/trusted-server/actions/runs/39637275502/job/119351108924)
  and [b6f job 119357467783](https://github.com/IABTechLab/trusted-server/actions/runs/39639053390/job/119357467783).
  Both roles report `native-material-coverage-incomplete` and
  `native-tool-host-path-unresolved:` for `archiver`, `assembler`, `build_tools`,
  `compiler` and `linker`.
- Linux CLI: [416 job 119351108938](https://github.com/IABTechLab/trusted-server/actions/runs/39637275502/job/119351108938)
  and [b6f job 119357467960](https://github.com/IABTechLab/trusted-server/actions/runs/39639053390/job/119357467960).
  Exactly `gnu-original-acquisition-unverified`,
  `gnu-privileged-helper-content-unmeasured`, `gnu-tool-closure-unmeasured`
  and `native-material-coverage-incomplete`.
- Apple ARM: [416 job 119360930044](https://github.com/IABTechLab/trusted-server/actions/runs/39637275502/job/119360930044)
  and [b6f job 119367262345](https://github.com/IABTechLab/trusted-server/actions/runs/39639053390/job/119367262345).
  Apple Intel: [416 job 119360930038](https://github.com/IABTechLab/trusted-server/actions/runs/39637275502/job/119360930038)
  and [b6f job 119367262261](https://github.com/IABTechLab/trusted-server/actions/runs/39639053390/job/119367262261).
  Each reports exactly `apple-coverage-excluded:toolchain_libraries`,
  `apple-tool-closure-unmeasured` and `native-material-coverage-incomplete`.

No GNU gaps leak onto Fastly or Apple. Every candidate retains
`material_native_coverage_complete: false` and SPDX `release_qualification: false`.
Fastly canonical/reproduction SPDX documents are byte-identical in each run;
SPDX equality remains measurement-only. Native reproductions are deferred and
their SPDX comparisons remain null. Both Apple jobs in the `b6f` run record a
non-null `apple_developer_tools.toolchain.libtapi_sha256`, confirming the new
measurement ran on both architectures. The unqualified four-asset candidate
index job also succeeded in the `b6f` run. No collector edits or field-mismatch
workaround was needed. Tasks 10/11 remain partial.

Selected Apple library-directory inventory (2026-10-22): extend the in-place
toolchain receipt to the entire selected `usr/lib` directory. A read-only
metadata inventory of local Xcode 16.4 (the version recorded by both hosted
Apple candidates) found 6,205 entries, 2,601,020,072 regular-file bytes, a largest
file of 354,743,440 bytes and 41 contained relative aliases. This establishes
local sizing only, not hosted performance or qualification.

The existing vendor-tree collector measures every file, directory, mode and
alias target under that root with its own `toolchain-library-directory` digest
domain. Limits are 512 MiB per file, 4 GiB per tree and 20,000 entries. Reads
retain the existing cancellation/deadline, no-follow, stable-descriptor and
alias-containment checks. The receipt's new `library_tree` stays inside opaque
`platform.apple.toolchain`; strictly checked collector fields are unchanged.
Every verification rehashes the tree: prior metadata equality cannot substitute
for fresh content measurement. No digest cache or extra attempt time is added.

The `toolchain_libraries` exclusion now denotes unresolved dependency/loading
coverage and libraries outside the measured selected directories. It does not
mean the selected `usr/lib` files are unhashed. Host runtime, dynamic loading,
original acquisition and full closure remain unresolved, so no completeness
or qualification flag changes. This inventory deliberately overlaps the
separate LTO/TAPI/resource identities and proves neither which files the tools
read nor which bytes contributed to the final artifact. Its extra hashing cost
must be accepted by a later hosted run under the existing attempt budgets;
the earlier green runs do not validate this increment.

Local verification passed 66 composition, 68 SPDX, 25 Apple toolchain, 18 GNU,
46 collection and two unchanged producer golden tests, each with a nonzero
selection guard. The collection transfer module and ignored tests were excluded
under the local execution restrictions. The added-library regression first
failed against the previous collector and then passed with the full inventory.
Fixtures also cover nested entries, edits/removals, alias targets, unsafe files,
limits, cancellation, expiry and structured SPDX rendering. Strict host clippy,
documentation and owned-file formatting passed. Spec review corrected two stale
comments so exclusions describe unresolved coverage rather than claiming the
newly inventoried files are unhashed. Independent code quality review found no
blocking findings and confirmed that hosted read-cost acceptance is still needed.

Main-merge candidate compatibility (2026-10-22): main `43a25619a` upgrades Rust
to 1.98.1, Node to 24.20.0, Viceroy to 0.21.0 and `libsqlite3-sys` to 0.38.2.
Run `39716862538` failed before the real candidates could complete: three
preparation jobs rejected the old Node pin, nine npm-observer tests rejected
the old Node/npm pins, and both native release-tool suites detected the stale
Viceroy pin. These failures provide no acceptance of the library-directory
inventory above.

Align the pending compiled/JSON tool and recipe pins with main, including npm
11.19.0 observed in the source-selected Node distribution. Original acquisition,
digests and qualification remain pending/null. The JS execution/settings checks,
fixture preparation and paired producer/collector Viceroy checks remain exact
version checks; no range, bypass or fallback is introduced. The user authorized
the minimal collector version check and fixture update. Receipt fields, nullable
members and enum tags are unchanged, and candidate index goldens are untouched.

Review of the new SQLite wrapper confirms the bundled `sqlite3` archive and
source subtree remain the same selection, with default feature
`min_sqlite_version_3_34_1` replacing `min_sqlite_version_3_14_0`. Update only its
reviewed wrapper version/features; AWS-LC 0.42.0 and ring 0.17.14 stay fixed.
The native implementation version and native license are not inferred from the
wrapper version. Source-lock and source-tool-pin regressions first fail against
the stale rules, then guard the aligned cohort.

Source review of npm 11.19.0 found the intercepted lifecycle entry points and
spawn adapter byte-identical to npm 11.6.2; the new script-policy defaults leave
the current reviewed hooks enabled. Do not add an `--allow-scripts` override.
Actual lifecycle, runtime, native-source and receipt acceptance require a new
hosted run; no local process-spawning tests are authorized by this migration.

Migration verification: 370 selected pure tests pass, including unchanged
candidate-index goldens and rejection of the previous Viceroy version. Strict
host-target clippy, documentation, read-only workspace rustfmt, changed-file
Prettier, JavaScript syntax and diff checks pass. Independent specification/security
and code-quality reviews found no blocking findings. Hosted validation remains
pending for the actual npm lifecycle, four candidate builds and their receipts.

Git dependency compatibility (2026-10-22): run `39720199554` at `0bf452370`
passes ordinary/native suites and the Linux JS handoff, but candidate jobs
`119631656292`, `119631656310` and `119635856319` reject the Git-sourced `esi`
dependency introduced by main. Its locked commit contains package-local LICENSE
and README.md links to regular files in the repository parent. Both the original
Git closure parser and the package vendorer previously rejected mode `120000`.

Retain canonical link blobs as inert authenticated objects in the complete
locked commit closure. During package vendoring, resolve only direct relative
file links through the same repository tree, rehashing every traversed object
and the target under existing budgets. Copy target bytes and executable mode
into regular files through the existing checksum writer; never create or follow
filesystem links. Reject absolute/escaping targets, malformed or oversize text,
missing targets, directory or chained aliases, aliased manifests and submodules.
Reject parent traversal after descending into a target component rather than
collapsing paths with potentially different link semantics. This changes no
receipt fields, origin identities, acquisition authority or qualification claims.

The positive pure vendoring regression first fails at the original unsupported
mode check, then passes with authenticated parent-file bytes. Pure regressions
also cover unsafe targets, manifest aliases, target executable mode and object
tampering. Full original-object collection and actual four-candidate builds
remain hosted checks; local process-spawning tests are not run.

Local verification selects and passes 186 pure tests: 42 vendoring, 67
composition, 68 SPDX, two candidate-index goldens, four authentication and three
entropy-reader tests. Strict host-target clippy, documentation, read-only
workspace formatting and changed-document Prettier pass. Independent
specification/security and code-quality reviews report no blocking findings.

Hosted follow-up: run `39724280588` at `0c67be66e` exposes an outdated
whole-tree rejection fixture in macOS job `119644526058`: it still labels mode
`120000` as unsupported. Keep its unrelated-subtree rejection assertion and
replace that entry with a still-forbidden `160000` submodule. The separate new
collection fixture requires retention of authenticated inert link blobs. This
process-spawning collection regression remains a hosted check.

Security verification: CodeQL run `39723446088` scans merge `eca28ba934`,
containing entropy correction `d4408d2e0`. All analyses pass and alerts 224–230
are fixed, with no relocated entropy warning or open release-tool finding.
Existing core/adapter/JS baseline findings are separate. This is security
acceptance of the entropy correction, not candidate-build qualification.

Cargo 1.98 compatibility (2026-10-22): run `39725113066` rejects canonical
Linux (`119647122855`) and Fastly (`119647122894`) candidates solely because a
`registry` directory exists in their private Cargo homes. Source review of
Cargo's 1.98 registry constructor and source-replacement loader shows that the
original registry is initialized before replacement with the vendor source.
Its backup-exclusion helper may create an empty directory or the exact
177-byte `CACHEDIR.TAG`, without fetching registry data. Directory presence
alone therefore cannot establish a source bypass.

The recipe now admits only an absent registry, a direct empty directory or a
direct directory holding only that exact tag. A held directory and matching
before/after inode and metadata checks bind inspection; the existing bounded
no-follow hasher requires a stable, single-link regular tag with the exact size
and SHA-256. Registry `index`, `cache`, `src`, unexpected children, aliases,
changed tags and all Git state remain rejected. No state is deleted or repaired;
offline execution, source replacement and verified package-origin joins remain
unchanged. The tag's upstream URL is runtime compatibility bytes, with a narrowly
reviewed domain suppression; it is never fetched.

The empty-directory regression first fails at the former presence-only gate,
then passes. Verification selects 177 pure tests, including eight dedicated
bookkeeping cases and 13 recipe tool tests, with nonzero-selection guards.
Strict host-target clippy, documentation and read-only workspace formatting
pass. Independent specification/security and code-quality reviews find no
blockers. Actual candidate builds and their receipts still require hosted
validation after this correction.

Hosted native acceptance (2026-10-22): run `39729655151` at `5d194d00f`
builds merge `a8c33bcc9990a4eb6520a8afd0f7dd2641c25111`. Linux CLI job
`119661528635`, Apple ARM job `119667144194` and Apple Intel job `119667144562`
all pass their actual producer fixtures, retaining composition receipts with
native completeness and release qualification false. Linux/ARM record
495 packages, 659 units and 61 build-script executions; Intel records
496 packages, 660 units and 61 executions. The real-receipt assertions pass.

Both Apple legs measure 6,205 selected toolchain library entries and
2,601,020,072 regular bytes, with digest
`da141ea11bf363ffcf61e620ccf28cf72224a7439d3e08e5cc945df01eb2333e`.
Their fixtures complete in 1,277.15 and 2,279.81 seconds respectively. This
accepts the inventory increment under the existing cohort budgets; no separate
inventory timing is logged, so its isolated cost is not established. Both
retain exactly `apple-coverage-excluded:toolchain_libraries`,
`apple-tool-closure-unmeasured` and `native-material-coverage-incomplete`.
Linux retains exactly `gnu-original-acquisition-unverified`,
`gnu-privileged-helper-content-unmeasured`, `gnu-tool-closure-unmeasured` and
`native-material-coverage-incomplete`. These results do not complete Tasks 10/11.

The same run's only failing job is Fastly `119661528580`: canonical Wasm
inspection rejects `fastly_async_io`. The Cargo bookkeeping barrier is passed;
no Fastly runtime, reproduction, SPDX or four-asset index acceptance is inferred.
Review of locked `fastly-sys` 0.13.1 and project async request call paths binds
this module to `select`/`is_ready`; pinned Viceroy 0.21.0 registers it. Admit only
that exact module. Other unreviewed SDK modules and component-style imports stay
rejected, along with non-function imports, invalid Wasm, wrong entry ABI and
missing debug information. A pure two-function regression first reproduces the
failure and passes after the policy update. Actual Fastly acceptance remains
a hosted check after the fix.

The scoped ABI verification passes 223 pure tests, including all 68 artifact
inspection tests, plus strict host-target clippy, documentation, read-only
workspace formatting and plan Prettier. Independent specification/security and
code-quality reviews find no blockers. No runtime gate, receipt field,
qualification flag or companion golden changes.

### Task 11: Generate complete artifact-specific SPDX

**Phase: 2+ (historical destination task).** Phase 1 uses the reduced implementation checklist above, rather than completing this task.

JavaScript declaration guard (2026-10-22): projected JavaScript facts contain no
measured declared-license evidence. Reject any supplied declaration at the
JavaScript origin boundary, matching the existing native-origin restriction,
instead of allowing a renderer input to manufacture a declaration. A pure
red/green regression injects MIT into all six supported original/role pairs;
the existing valid-document byte digest remains unchanged. Unknown JS licenses
continue to render as `NOASSERTION`; no new license measurement, receipt field,
companion format or completeness claim is introduced.

Scoped verification passes 187 pure tests, including all 69 SPDX tests and
unchanged candidate-index goldens, plus strict host-target clippy, documentation,
read-only workspace formatting and plan Prettier. Independent specification and
code-quality reviews find no blockers. The corrected Git collection fixture is
compiled locally but is run only in hosted CI under the process-test restriction.

**Create:** release tool `src/spdx.rs`, private `spdx::tests`, `crates/trusted-server-release/tests/fixtures/spdx/`.

- [x] Write cases for incorrect root digest, missing native coverage, ambiguous package identity, unknown license, duplicate SPDX IDs, dangling relationships, uncompiled dev dependencies, type-only/virtual JS and oversize predicates.
- [x] Run the pure `spdx::` suite with the nonzero-selection guard. The scoped 2026-10-21 run selected and passed 66 tests; this validates renderer/candidate behavior, not the closed qualified generator.
- [ ] Emit SPDX 2.3 JSON from Task 10 normalized evidence. Root checksum is final executable SHA-256; contributors/materials/tools/external requirements have distinct relationships. Unknown licenses use NOASSERTION; missing contributing origin/coverage fails. Validator uses a pinned authoritative SPDX schema.
- [ ] Generate full real Fastly and all three CLI SBOMs; schema-valid toy fixtures are not sufficient. Validate size <=16 MiB and document equality semantics: reject duplicate keys, ignore object order, preserve array order.
- [ ] Re-run fixtures plus actual coverage inspection. Commit `Generate validated per-artifact SPDX evidence`.

Fixture exit audit (2026-10-22, `41560cfd9`): the first checkbox records existing
written regression cases, not complete qualified generation. The cases span
owner validation, projections, rendering and the JS collector:

| Required case                                                       | Existing regression                                                                                                                                                                                                                                                                                                                                               |
| ------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Incorrect root digest, duplicate SPDX IDs and dangling relationship | `spdx/tests.rs::rejects_root_digest_changes_duplicate_identities_missing_origins_and_dangling_edges` rejects each input mutation. `stable_ids_and_document_bytes_do_not_depend_on_input_component_order` checks unique emitted package SPDX IDs. Candidate merge tests additionally reject cross-part duplicates and dangling/cross-part edges.                   |
| Missing measured native coverage                                    | `composition/native/tests.rs::native_spdx_requires_each_reviewed_consuming_context_and_unambiguous_features` clears measured contributors and requires projection rejection. `rejects_unknown_native_scripts_and_missing_native_execution` rejects missing captured build-script output. These tests do not establish complete platform coverage.                 |
| Ambiguous package identity                                          | `composition/tests.rs::ambiguous_or_missing_package_and_target_joins_are_rejected` rejects duplicate metadata. `identical_names_and_versions_from_distinct_sources_remain_separate` and `spdx/cargo/tests.rs::original_identity_distinguishes_complete_registry_and_git_facts` preserve distinct source identities. Native tests reject original/context aliases. |
| Unknown license                                                     | `spdx/tests.rs::unknown_licenses_use_noassertion_and_never_become_unsupported_expressions` verifies `NOASSERTION` for absent and unknown declarations. Native and JS origin tests reject manufactured declarations.                                                                                                                                               |
| Uncompiled dev dependencies                                         | `composition/graph/tests.rs::catalog_dev_only_entries_never_create_contributors` excludes potential-only packages; `spdx/cargo/tests.rs::package_edges_require_both_observed_endpoints_and_skip_dev_kinds` excludes their projected dependency edges.                                                                                                             |
| Type-only JS                                                        | `crates/trusted-server-js/lib/test/release-composition.test.mjs` checks that a type-only import and a zero-length rendered namespace are omitted. `javascript/contributions/tests.rs` rejects a zero rendered-length contribution.                                                                                                                                |
| Virtual JS                                                          | The same JS collector file rejects an unknown virtual producer and checks recognized CommonJS helper ownership. `spdx/tests.rs::generated_js_requires_a_known_producer_and_retains_generated_bytes_identity` rejects a missing producer while preserving generated-byte identity.                                                                                 |
| Oversize predicates                                                 | `spdx/tests.rs::rejects_an_spdx_document_exceeding_the_attestation_byte_limit` and `spdx/candidate/tests.rs::candidate_bounds_reject_documents_over_sixteen_mebibytes` enforce the output bound.                                                                                                                                                                  |

Rust paths above are relative to `crates/trusted-server-release/src/`. Running
only the SPDX and composition selections does not execute the JS collector
cases. No new test API, duplicate fixture, schema change or candidate-index
golden change is needed to close the written-cases checkbox. Complete native/tool
coverage, qualified generator integration, the producer/consumer contract and
full real four-asset SBOM exits remain open.

Audit verification: fresh pure selections passed 69 SPDX and 67 composition
tests, each with a nonzero selected count. Strict host-target all-target/all-feature
Clippy, all-feature documentation, read-only workspace formatting and this plan's
Prettier check passed. JS collector cases were inspected, not rerun locally.
Independent specification and quality reviews supported the fixture checkbox;
their corrections explicitly separate acquisition/qualification from material
coverage and identify the emitted-ID uniqueness assertion.

Progress (2026-10-19, partial renderer foundation): a private pure renderer validates executable root identity, component origins and roles, stable package/relationship identities, license declarations and bounded output against the existing pinned SPDX 2.3 schema. Structured-tree evidence stays in explanatory source information rather than raw-file checksums. Fictional fixtures exercise structural/semantic acceptance; schema validity supplies no signing or qualification authority. The closed (qualified) generator entry point still rejects every current candidate; a separate private candidate-only assembly (see the 2026-10-20 notes below) renders an unqualified document that cannot become the closed type. There is no functioning generation command, no caller-controlled completeness flag and no JSON constructor for complete inputs. Typed Cargo/JS contributor export, complete native/tool coverage, producer/consumer contract fixtures and full real Fastly/three-platform CLI SBOMs remain required; the renderer does not satisfy those exits.

The owning JS handoff now exports typed conservative source/npm/generated contributions, material observations and tool joins after rehashing all retained files before and after export. The export preserves the original inventory digest, distinguishes npm archive integrity from installed-tree evidence, checks generated producer identities and rejects conflicting source hashes. It supplies no license inference, final-function attribution or complete-coverage capability. The real two-installation JS fixture exercises this export after its private working roots are removed; the full two-installation test (`preparation::tests::full::real_two_fresh_js_builds`) that carries the new export assertion passed in ordinary CI, run 39290139247 (job 118188353041, 680.82 seconds; head `df4a6e5c3` descends from `b99a17695`). Complete native/tool closure and qualified SPDX generation remain open; typed Cargo, native and platform projections and the candidate-only whole-document assembly are recorded below.

The Cargo adapter now derives renderer component identities from approved workspace paths or verified original registry/Git identities, excluding opaque Cargo IDs and temporary roots. Host proc macros and build scripts have build-tool roles; supporting host libraries remain build materials, while target units have conservative Rust-contributor roles. Only package roles are combined; effective unit features remain separate borrowed observations. Potential graph edges are emitted only between observed endpoints and retain their potential-package meaning. Registry archive SHA-256 values identify actual original archives; registry index locations are not invented download URLs, and Git object/tree identities remain provenance information rather than raw SHA-256 checksums. The complete generator stays closed while the remaining native/tool and whole-artifact integrations are unavailable. The shared real-candidate helper exercises the owner adapter; all four canonical cohort checks passed in run 39290139247, as recorded under Task 10.

The next JS adapter joins that sealed final-byte membership to checked original source/npm/generated facts. Complete bundles receive a containment relationship; positive Rollup-rendered inputs remain conservative observations before esbuild minification, and absent CLI bundles remain build materials. npm archive SHA-512 integrity is separate from installed-tree SHA-256 provenance. Generated wrappers retain their checked Vite producer, and unknown licenses remain `NOASSERTION`. The adapter consumes the completed recipe's frozen original inventory, without depending on the preparation attempt's later lifetime. The JS projection ran inside the shared real-candidate checks of ordinary-CI run 39417433680 (head `bfab8ef20`, every candidate job succeeded; verified from job conclusions, as per-assertion output is not logged). The complete generator stays closed.

Native and platform adapters now also borrow the sealed completed owner. Native projection requires exact observed wrapper/context/features and verified original-source joins, rejects missing contexts and duplicate aliases of the same original, and keeps original registry archive checksums separate from structured source trees and measured static output archives. Wrapper versions do not assert native implementation versions. Platform projection preserves measured Rust trees, Apple tool files/SDK materials and GNU installed/configured identities with their acquisition and coverage limitations. Logical runtime requirements retain accepted artifact/target/format facts without invented installed-runtime checksums. Source, artifact, target, tool-tree, GNU boundary/plan/profile and Apple SDK joins are checked. Private working paths and opaque Cargo IDs stay out of projected identities. The shared real-candidate helper exercises both adapters, with hosted validation of these assertions in ordinary-CI run 39417433680 (all candidate jobs succeeded; verified from job conclusions, not per-assertion output). Complete generation remains closed until full owning composition coverage and whole-document integration are available.

Candidate whole-document SPDX (2026-10-20, `1719cf15f` and `073704cc7`): a private `candidate_from_completed` joins the Cargo, JS, native and platform projections of one completed recipe into a single validated SPDX 2.3 document, returned as a separate `CandidateSpdx` with no conversion into the closed `GeneratedSpdx` and no generation command. The source commit must match the build, the platform receipt must equal the composition view's, and the root package must be the policy asset's verified workspace package at the same commit and version. Parts merge only with unique identities and edges inside their own part. The creation comment marks the document as an unqualified candidate and lists the platform coverage gaps, which must not be empty. The root checksum equals the rehashed artifact, the creation time derives from the source epoch and documents stay within the SPDX size bound. `073704cc7` runs it on every real completed artifact in the Fastly, Linux CLI and Apple candidate tests, checks the gaps each platform must report (unresolved Fastly host tools, the reviewed GNU privileged helpers, the Apple exclusions and unmeasured tool closure) and retains document digests and sizes as measurements; Fastly canonical-versus-reproduction document identity is a measurement only. Hosted status: ordinary-CI run 39420011152 passed for `1719cf15f`, which contains the assembly but not the real-build checks. The run for `073704cc7` (Run Tests 39452090186) was still in progress when this note was written, so no hosted result exists for the real-artifact SPDX checks yet. Task 11 remains open: complete native/tool coverage, qualified generation, the producer/consumer contract and full real SBOMs for all four assets are still required.

Renderer validation follow-up (2026-10-21): JavaScript originals now require
exactly one compatible role before rendering relationships. A complete bundle
may be retained or build material; a rendered contribution may be conservative
input or build material; preparation/tool material may be a build tool or build
material. Crossed, empty, duplicate and mixed roles fail. This prevents a material
record or pre-minification contribution from manufacturing `CONTAINS` evidence.
The normal completed-owner projection already selects these roles correctly;
this is validation hardening, not evidence of a real recipe misclassification.

An integrated negative fixture rendered unsupported containment before the guard
and rejects it afterward. All six accepted pairs are covered. A fixed valid
document retains its pre-change SHA-256
`2a79890ec323d58b8c6b8f304b06977c8355c8a5c341301b9782a82ddc489ee8`.
Both pure producer golden checks pass against the unchanged candidate index and
companions; no golden, schema or companion format was rewritten.

License regression tests confirm the existing conservative behavior: the four
explicitly reviewed exact declarations retain their original spelling/order;
missing or unreviewed declarations, unsupported expressions and an observed
`NONE` use `NOASSERTION`, preserving bounded observed text in license comments.
No license conclusion, file-license analysis, wrapper-to-native license inference
or complete SPDX license-catalog support is asserted. This change does not add
license facts or infer missing origins. Existing named native/tool/acquisition
gaps remain present, and complete qualified generation remains closed.

The local suite passes 66 pure SPDX tests plus the two pure golden checks.
Real-artifact acceptance is recorded under Task 10 for `e91d75b74`. Complete
native/tool coverage, complete qualified per-artifact SBOMs and the remaining
Task 11 exits stay unchecked. Parallel Task 12 code and its fixtures are untouched.
Task 11 resumes after recording Task 14's hosted preservation result and open
maintainer decision below. Task 14 remains open; this ordering does not waive
either task's coverage, authority or qualified-generation requirements.

Native license boundary follow-up (2026-10-21): the renderer rejects a native
implementation license declaration when the measured native original has no
such license fact. The owning adapter already supplies no declaration; this
closes an invalid-input path rather than correcting a demonstrated real-build
misclassification. The integrated regression covers all three reviewed wrappers
in host and target contexts. Native license declarations and conclusions remain
`NOASSERTION`; wrapper declarations cannot substitute for native license evidence.
No license catalog is expanded and no existing measured package or relationship
was found missing from its projection. Complete artifact-specific generation
still requires the unmeasured closure/input facts and qualified owning adapters.
Valid rendering, companion formats and candidate golden fixtures are unchanged;
all four candidate jobs accepted the strengthened guard in the `e91d75b74`
run recorded under Task 10. Complete generation remains closed.

Selected Apple library projection (2026-10-21): a present measured `libtapi`
digest projects as a build tool with a raw-file SHA-256. Recorded absence
creates no package. The projection asserts no original download, license,
runtime bytes or execution, and retains `NOASSERTION` through the existing
renderer. Other package projections, relationship policies and candidate golden
fixtures are unchanged. This adds measured bytes to candidate SPDX without
opening complete qualified generation or closing Task 11. Hosted receipt and
candidate SPDX acceptance for both increments is recorded under Task 10 in the
2026-10-22 note; all named coverage gaps remain present.

Selected Apple library-directory projection (2026-10-22): retain the new
structured inventory as a build material with no raw-file checksum, original
download or license claim. Candidate SPDX retains the same named coverage gaps.
The measured directory is an input inventory, not an asserted executable
dependency set; complete qualified SPDX generation remains closed.

### Task 12: Bind transfers and enforce selected comparisons

**Phase: 2+ (historical destination task).** Phase 1 uses the reduced implementation checklist above, rather than completing this task.

**Create:** release tool `src/collection.rs`, private `collection::tests` and exact transfer fixture data.

- [ ] Test stale/wrong-run/source transfers, reused A/B artifact IDs, transfer hash warning, traversal/duplicate ZIP members, missing A, changed inventory, missing supported B variation or dishonest normalized/unsupported claims, absent final-byte runtime proof, missing/failed production socket controls or an unconfined build invocation, and a one-byte final wasm difference.
- [ ] Run `cargo test -p trusted-server-release --target "$HOST_TARGET" collection::tests` with the nonzero-selection guard; expect missing validator.
- [ ] Accept only expected upload-step outputs: IDs/archive digests mapped by trusted workflow dependencies to job/attempt/role/recipe. Re-download exact current-run artifacts and validate REST repository/run/source plus transfer bytes. Artifact API does not independently identify the producing job/attempt. Verify bounded declared ZIP members before extraction.
- [ ] Match expected role-specific ambient profiles from the approved recipe against observed A/B records before signing. For mandatory Fastly A/B, all qualified supported dimensions MUST differ as selected; normalized values remain explicitly identified and omitted supported dimensions fail. Successful native comparisons use their selected observed profile and result binding, but a bounded native B failed/unavailable before environment setup records unavailable fields honestly and cannot be rejected for lacking observations it never produced. Add that exact nonblocking fixture; all native A qualification/composition remains mandatory. Verify final-byte core-format/publisher-request results for both Fastly outputs. Fresh-job identity alone cannot satisfy the variation or runtime contract.
- [ ] Compare complete final Fastly files with no normalized/excluded regions. For native B produce exactly matched/mismatched/failed/unavailable within its deadline; native A failures always block. Record ambient variations and normalized internal values honestly.
- [ ] Preserve A as canonical only after all mandatory requirements pass. Add explicit tests that optional B failure cannot hide any A/composition/JS failure. Commit `Validate release transfers and comparison outcomes`.

Progress (2026-10-19, partial candidate implementation): 18 focused tests cover trusted exact-ID transfer tuples, raw artifact REST corroboration, complete mandatory A/Fastly B records, bounded optional native B outcomes, ambient/runtime/composition bindings and adversarial ZIP32 inputs. The in-memory ZIP reader checks declared flat members, exact lengths/digests, CRCs, central/local agreement and complete DEFLATE stream termination. Independent spec and quality review passed after fixing a truncated-stream acceptance finding. Imported records remain diagnostic data and cannot construct local recipe, confinement, runtime or signing capabilities. A qualified dependency constructor is deliberately uninhabited. Superseded in part (2026-10-20): the bounded HTTP transport and a candidate producer export now exist (see below), and upload-action ZIP interoperability passed in ordinary CI. The trusted workflow dependency adapter and hosted qualification remain required before this task can close.

A subsequent exact-ID, read-only audit checked one actual ordinary-CI upload archive (242,907 bytes; SHA-256 `ef2714df5e38c09a011cf25fb37268830de2f03426b81ee646b7b287989401cc`) against unchanged REST metadata and its archive digest. Its 16 regular files use version-needed 2.0, DEFLATE, signed ZIP32 descriptors, Unix regular-file mode and no extras/comments/ZIP64. These observed header properties now have a synthetic regression, bringing collection tests to 19. The JS handoff's nested bundle/inventory paths violated the intentional flat-member policy when this was written; the candidate producer described below since exports the declared flat layout. One small archive's format compatibility does not authenticate its producing job, qualify the trusted dependency adapter or prove every upload size/profile.

The owning recipe now retains typed socket-control/probe observations after successful denial evaluation, with boundary-instance fingerprint checks at capture, completion and checked access. Diagnostic receipts cannot reconstruct local denial capabilities; explicit pure-test fixtures have no measured receipt. A separate approved-source recipe-definition identity is shared across A/B, while the typed exact invocation digest preserves role-specific paths, jobs, environment and umask. Both retained getters rehash owned outputs, and real candidate helpers exercise them after source/target cleanup. This closes two producer-export prerequisites without implementing the final export/transport adapter or granting release authority. The combined verification batch passes 322 focused tests with independent review; actual hosted validation of these new prerequisites remains pending.

A candidate producer now copies four flat files into owned storage: `artifact`, `composition.json`, the original `tsjs-inventory.json` and `candidate-build-record.json`. Checked access requires sealed composition receipts, the approved-source recipe definition and measured socket observations; it records actual single-build ambient conditions and keeps the shared recipe digest separate from the exact invocation digest. The recipe freezes checked JS facts and exact original inventory bytes while the preparation owner is valid, retains them independently at completion, and verifies their source/digest and retained file at subsequent handoffs. This prevents a later optional native B timeout from invalidating A's evidence through an expired JS preparation lease. Every copied file is rehashed, and altered, extra, missing or linked members are rejected. This explicitly unqualified diagnostic format still differs from the strict collector's contract: runtime acceptance, ambient pair claims, complete composition closure, authenticated bootstrap and trusted workflow producer identity remain declared gaps. All four canonical recipes proved the previous three-file export's lifetime in run 39290139247; the fourth member (`candidate-build-record.json`, introduced in `7bcf93e50`) and JS projection assertions have since passed in the candidate jobs of ordinary-CI runs 39417433680 and 39420011152 (verified from job conclusions, not per-assertion output). Pure diagnostic fixtures do not establish collector compatibility. No strict collector rule or qualified dependency constructor is relaxed.

The Fastly path can additionally retain its actual final-byte Viceroy acceptance in the diagnostic record. Acceptance now binds the build role as well as approved source and final artifact size/hash, so equal A/B bytes cannot substitute one role's runtime run for the other's. Checked export rehashes the completed owner before/after formatting bounded observations, including actual Viceroy/fixture/configuration and publisher-response identities; JSON remains output only. Both real Fastly runtime owners are retained through export and cleanup checks. This removes the Fastly runtime-export gap only when the actual owning acceptance is supplied. Native startup export, ambient pair claims, strict collector integration, qualification and workflow authority remain open; native records continue to declare absent runtime acceptance. The Fastly job passed in ordinary-CI run 39417433680 (job conclusions verified).

Further candidate implementation (2026-10-20): native startup now produces an owning acceptance after executing and cleaning up the exact copied final CLI. Its expected version output derives from the selected package's verified approved workspace origin and sealed composition, rather than the release helper's package version. Linux acceptance requires the three selected distributions and distinct actual local image identities; these images remain unqualified. Apple acceptance checks the actual host cohort, CPU and final Mach-O minimum OS before execution. Checked diagnostic export binds source, role, asset, final size/hash and package version, retaining actual bounded startup observations after build-root cleanup. Optional native B without a successful startup retains an explicit missing-acceptance gap. Independent spec and quality review found no outstanding native-startup findings; these assertions ran in the native candidate jobs of ordinary-CI run 39417433680 (job conclusions verified; not per-assertion output). Both Linux and Apple startup accepted outputs are recorded in the dated reports.

The read-only HTTP backend now binds exact selected IDs/names/archive digests to strict raw REST repository/run/head metadata. It uses fixed-origin authenticated API requests and one manually checked, credential-free redirect to the supported Azure Blob storage host subset, with compiled WebPKI roots, no proxy or automatic redirects, asynchronous DNS, bounded streaming, cancellation and per-response/attempt deadlines. The unchanged flat ZIP reader remains the extraction gate. Local fictional TCP/TLS tests cover credential separation, changed metadata, denied redirects, transformed/truncated/oversized bodies, cancellation and injected-CA rejection. A test-only ordinary-CI hook exercises this backend without constructing trusted dependencies. Real upload-action interoperability, the trusted workflow dependency adapter, ambient pair claims, strict producer/collector integration, complete composition coverage and hosted qualification remain required; Task 12 is still open.

An ordinary-CI producer/receiver fixture now uploads four inert fictional flat members using the pinned upload action and passes its exact ID/name/archive digest through job outputs. A separate read-only receiver downloads that original archive twice through the actual backend, corroborates current-run REST metadata and verifies every member's bytes using the unchanged ZIP reader. Fork PRs skip these jobs. The fixture distinguishes the API's PR head from checkout's synthetic merge and grants neither source approval nor release authority. The guarded test has since passed in hosted CI: `collection::transfer::ordinary::real_uploaded_archive` in job 118609812109 of run 39417433680 (0.77 seconds), with the producer job 118609761722 also succeeding. Independent transport spec review and transport/workflow quality review found no actionable findings. This small upload is interoperability evidence only; it cannot establish large-artifact coverage, complete composition or qualified producer identity.

Local verification of this batch passed 853 native unit tests (seven ignored external/helper fixtures), 13 command tests, strict native Clippy, documentation generation, Rust/Markdown/YAML formatting, actionlint and staged domain lint. A full-suite failure exposed inherited nonblocking accepted sockets in the fictional macOS HTTP fixture; fragmented-header red/green and aborted-header regressions now cover the corrected test server. Independent review found no remaining findings. Linux cross-target Clippy on this Mac cannot compile the newly needed TLS C dependencies without a Linux C cross-compiler; hosted Linux validation remains required. No candidate or release qualification is inferred from these local checks.

Cross-job candidate-entry transfer (2026-10-21, implemented; first hosted pass in run 39577910126): each real candidate job (Fastly, Linux CLI and both macOS legs) now exports its rechecked entry as six flat files: the final executable `artifact`, `tsjs-inventory.json` and the four unsigned companions. It writes a separate CI-internal transfer file and uploads the files with the pinned upload action under `candidate-entry-<asset>-<run_id>-<run_attempt>` (1-day retention, fork PRs skipped). A describe step merges the exact upload ID, name and archive digest with the producing job's own run attempt into a job-output descriptor; the macOS matrix uses per-leg keys. The new `test-release-candidate-index` job (`contents`/`actions` read only, token only in the download step, restore-only cache) runs the ignored `distribution::ordinary::real_four_job_candidate_index`. It strictly parses the four descriptors, retrieves each exact upload through the unchanged bounded backend and ZIP reader with REST run/head/name/digest corroboration, and rehashes the executable against build-evidence `artifact_sha256`/`artifact_size`. Existing bounds cover the largest measured executable (Linux CLI about 285 MB; 512 MiB member, 768 MiB archive). The descriptor is not a release schema and grants no trusted workflow dependency: companions keep `null` workflow/transfer identities and the `trusted-workflow-transfer-not-bound` gap. Pure tests cover descriptor parsing, export writing and transfer rejections. The trusted dependency adapter, ambient pair claims and qualification remain open, so Task 12 stays open.

### Task 13: Assemble and validate the exact evidence graph

**Phase: 2+ (historical destination task).** Phase 1 uses the reduced implementation checklist above, rather than completing this task.

**Create:** release tool `src/distribution.rs`, private `distribution::tests`, `crates/trusted-server-release/tests/fixtures/index/`.
**Schema boundary:** governing index v1 raw bytes remain immutable; any change requires an explicit reviewed amendment.

- [ ] Port positive and adversarial schema fixtures: exact four artifacts, unknown capabilities and structurally compatible but currently forbidden `fastly-staging`, duplicate names/keys, source/cohort/recipe mismatch, unsafe names/newlines, altered lengths/digests and role-specific limits.
- [ ] Run `cargo test -p trusted-server-release --target "$HOST_TARGET" distribution::tests` with the nonzero-selection guard; expect missing assembly/semantic checks.
- [ ] Assemble unsigned companions from successful observed outputs, then artifact provenance/SBOM bundles, then the signed index. Index names/hashes all companions and shared JS inventory; exclude own digest/bundle. Recipe/rebuild records are authenticated informational data, not executable programs or independent trust.
- [ ] Require SPDX parsed predicate/document equality plus separate file hashes. Producer verifies every exact expected provenance subject; no additional unapproved code subject. SHA256SUMS covers every other published asset and not itself.
- [ ] Cross-check schema and semantic joins on all real artifacts before signing/rehearsal. Commit `Assemble the authenticated release distribution`.

Progress (2026-10-20, partial candidate index): a private `distribution` module assembles an unqualified candidate release index. `candidate_entry_from_completed` accepts only a canonical completed build at its approved source with the matching actual acceptance (Fastly runtime plus canonical/reproduction equality, or native startup), renders the candidate SPDX itself and renders build-evidence, build-recipe and rebuild-check companions. Each companion carries `"release_qualification": false`, sorted non-empty gaps and `null` workflow run and transfer IDs. Fastly records `matched` only from the checked equality; native assets record `unavailable`. `assemble_candidate_index` requires each fixed asset exactly once with one source commit, producer policy identity and inventory, emits artifacts sorted by name with no timestamps, and rechecks the rendered bytes against the retained v1 schema plus mirrored consumer semantics. Fixed per-asset identity comes from a compiled accessor equal to the schema row, so `fastly-staging` cannot appear. The tag is the sentinel `v0.0.0-unqualified-candidate`, the bytes are named only `candidate-release-index.json`, and both signature bundle slots are `AwaitingSignature`: they render the true empty-file record and their `Signed` variant is uninhabited. There is no command, writer or signer. Golden fixtures under `crates/trusted-server-release/tests/fixtures/index/candidate/` are byte-compared by the producer and run through the CLI's unsigned consumer joins. Each entry derives `policy_version` and `policy_sha256` from the same approved policy bytes, as the consumer does, and rejects a version other than the compiled one. The CLI verifier now rejects size-0 provenance and SBOM bundle records for every indexed artifact immediately after parsing the index, and again for each consumed artifact, so the candidate cannot pass full verification. Follow-up (see Task 15): once real signing exists, the consumer must also reject the sentinel tag and `"release_qualification": false` documents. Pure fixtures cannot exercise a successful completed-build entry, because they have no composition receipts or actual acceptances. Only owner wiring rejections are covered locally, and real four-asset assembly is deferred to Tasks 12 and 15. Signing, SHA256SUMS, provenance subject checks and qualification remain open.

Hosted candidate-entry follow-up (2026-10-21): `4483e8256` wires independent
real canonical-entry checks into the candidate jobs. Run 39529922087 completed
Fastly, Linux CLI and Apple ARM jobs, but Intel and the overall run were cancelled.
Run 39540057202 subsequently passed all four per-asset entry checks for source
`d0cd53343fcbe1fb6f06eafc009e306e24b1245d`, with the same JS inventory digest
in all four jobs. The [candidate index report](../../../scripts/release/qualification/2026-10-21-candidate-release-index.md)
retains exact job/entry identities and distinguishes native measurement results
from the entries' deliberately `unavailable` rebuild outcome. This establishes
per-asset integration, not real cross-job four-entry assembly. Task 12's trusted
transfer adapter is the next join prerequisite; complete native/tool coverage,
authenticated acquisition, signing, publication and qualification remain separate
unfinished exits. Task 13 remains partial.

Real cross-job assembly (2026-10-21, implemented; first hosted pass in run 39577910126): the
collector job rebuilds each transferred entry with the test-only
`CandidateArtifactEntry::from_ordinary_ci_transfer`, the one documented
exception to the production-only constructor. Production is unchanged. The
rebuild reruns every producer join over the transferred bytes and derives
`policy_version`/`policy_sha256` from the consumer policy read out of a
retained clean clone authenticated at `github.sha`. Fastly must be `matched`
with reproduction equal to canonical, native assets `unavailable`. Gaps must be
sorted, unique, a superset of the fixed entry gaps and identical across
companions. Every companion is rehashed, and each rebuilt entry must recheck to
the producing job's entry digest. The unchanged `assemble_candidate_index` then
writes `candidate-release-index.json`, the inventory and sixteen companions. The
collector prints the index and its SHA-256, the CLI's ignored
`ordinary_ci_assembled_candidate_passes_unsigned_consumer_joins` runs the
consumer's unsigned joins and empty-bundle rejection over those files, and the
directory is uploaded as 7-day unqualified candidate evidence. It runs only when
all four candidate jobs succeed and is not a required check. A pure round trip
reproduces the golden index byte for byte, alongside rejection, descriptor and
writer tests. The first hosted all-four assembly passed in run 39577910126 at
merge source `fb42d8268`: index SHA-256
`4872ef96086e93f720187f363e7262ae2921e663fbc9a531f2a42cc5d803b14d`, accepted by
the consumer's unsigned joins (see the
[four-asset candidate join report](../../../scripts/release/qualification/2026-10-21-four-asset-candidate-join.md)).
The preceding run 39567710071 rejected its first download because the collector
passed one 20-minute deadline to a transport capped at 15 minutes;
`d774ee47c` bounds each download separately. Signing, publication and
qualification stay open.

Pair comparison (2026-10-21, `de52e66fd`): the collector also rejects a
canonical/reproduction pair sharing one boundary instance and treats an equal
host locale or timezone as a missing supported variation. New pure tests
isolate the pair check per dimension, record every optional native
reproduction outcome, show no reproduction state hides a mandatory canonical
failure, and require Fastly runtime proof for both roles.

Collector follow-ups (2026-10-21):

- `f17377a4e`: an optional native reproduction without an upload is an
  observed job (started flag, terminal conclusion, bounded elapsed/deadline)
  with no upload identity fields; the collector derives `unavailable` (queued
  at the deadline, skipped, cancelled before any step) or `failed` (started
  then cancelled or still running, failure, timed out, success without an
  upload) and rejects contradictory, premature or unbounded observations. Each
  dependency channel accepts only its own build roles before any fetch, and the
  Fastly runtime record must carry its record's role. That role is
  producer-asserted, so it guards an honest pipeline; a per-run Viceroy
  identity remains open.
- `15893ef1e`: the earlier "exact invocation" check compared a fingerprint
  forced equal to the boundary instance, so it only proved distinct instances.
  Records now carry `execution_fingerprint` (boundary instance, bound to the
  denial receipt) and a separate `invocation_digest` from the exact Cargo plan
  digest (program, arguments including jobs, environment, role checkout root,
  umask); the pair must differ in both. A test-reachable producer renderer
  emits this strict record from producer-owned facts.
- `5a98c18ad`: the collector parses the producer's real sealed composition
  receipt instead of a fixture-only summary, keeping every earlier check and
  adding materials, target, package-join, Fastly embedding, coverage-claim and
  platform-shape bindings. Real candidate tests feed each real receipt through
  the same check, so producer/collector drift fails ordinary CI.

Still open: the trusted workflow adapter that fills observed job states and
upload identities; job/attempt identity supplied by the workflow; native
startup acceptance has no collector slot; native canonical input digests are
only compared against a reproduction; an invalid uploaded native reproduction
rejects the whole collection rather than recording `failed`; and the
`failed`/`unavailable` mapping is not yet normative in the spec.

### Task 14: Expose trusted same-commit validation

**Phase: 2+ (historical destination task).** Phase 1 uses the reduced implementation checklist above, rather than completing this task.

**Create:** `.github/workflows/release-validation.yml`, release tool private workflow-contract tests (register alongside the production policy/dispatch module; do not expose a test-only public API).
**Modify:** `.github/workflows/test.yml`, `format.yml` only to extract/reuse existing required jobs without losing existing triggers/gates.

- [ ] Add static workflow fixtures for wrong SHA/event/ref, unlisted RC branch, malicious candidate allowlist, forged local remote refs, missing governance/ruleset/bypass approval, skipped/path-filtered gates, arbitrary commands, excessive permissions and any floating external action ref in the complete release/validation invocation.
- [x] Run the pure `workflow_contract` suite with the nonzero-selection guard. The scoped 2026-10-21 run selected and passed 19 tests; three release-binary-only command tests and 45 pure Node tests also passed. These checks do not establish hosted release eligibility.
- [ ] SHA-pin every external action in release.yml, release-build.yml and all called release validation jobs, including checkout/setup/cache/upload/download and attest. Preserve readable version comments, audit dependencies and reject floating @v\*/main refs. Local workflow calls are allowed only at the caller's checked source revision. Existing test/format action refs MUST NOT be inherited unreviewed into this path.
- [ ] Expose reviewed workflow_call or explicit jobs running every current AGENTS gate at the tag SHA. Maintain PR/main tests; RC is permitted only by exact reviewed cohort branch entries. Validate strict SemVer/lightweight tag independently; annotated tags fail.
- [ ] Wait up to 90 minutes for this invocation's required terminal validation results; fail cancelled/skipped/timeout results with rerun guidance. Do not trust arbitrary named successful checks or unrelated main commits.
- [ ] Include JS build/tests/format, all Rust aliases/parity/CLI/build-digest, the explicit host-only release-tool test/clippy gate (or reviewed aliases), docs/Markdown and `scripts/lint-wasm-clock.sh`. Commit `Validate release source at the exact tag commit`.

Progress (2026-10-21, partial Task 14): `release-validation.yml` exposes only
`workflow_call` with one required source SHA. Its explicit jobs cover the current
AGENTS gates, host release-tool Linux/macOS tests and clippy, and existing parity,
codegen, JS prebuilt, documentation and trust-test checks. Every checkout selects
the supplied SHA with persisted credentials disabled. Both external actions use
reviewed commit pins and version comments; no composite or local workflow is
inherited. Shell Rust setup follows the existing native release-tool approach
without introducing a floating action or shared cache.

The read-only `check-workflow-contract --workflow <path>` command checks exactly
one reviewed workflow and its fixed sibling helper. It executes nothing, performs
no network access and reports both source eligibility and release qualification
as false. Inputs are bounded to 1 MiB before parsing; held regular-file reads use
no-follow/nonblocking open. Raw workflow, canonical parsed structure and helper
digests bind commands, permissions, conditions, action pins and each version
comment. YAML references, merge keys, duplicate/non-string keys, tags and
unsupported ambiguous spellings fail. The restricted parser conservatively
rejects reference indicators outside literal run blocks, even in quoted scalars.
Formatting or helper changes require explicit review and pin updates. Additional
files and local invocation graphs remain unsupported; Tasks 15–16 must extend
this closed contract when their actual caller/signing workflows exist. The YAML
dependency reuses the workspace package; the lockfile adds only its dependency
edge, with no new package.

The runtime helper requires the same full source/workflow SHA, official repository
identity, tag-push event, exact caller/ref and strict bounded SemVer. Fixed-origin
bounded read-only REST requests observe the unpeeled lightweight tag, actual
remote `main` tip and ancestry; neither local refs nor candidate branch entries
provide authority. No RC branch is approved. Admission records observations and
`source_eligibility: false`, allowing quality checks to run without authorizing a
release. The parallel waiter enumerates this run attempt's required terminal jobs,
rejects missing/duplicate/wrong-source results and non-success conclusions, and
uses an original-run, queue-inclusive 90-minute window with monotonic remaining
budgets for requests and polling. The first bounded REST read obtains the trusted
run creation time. Rerunning jobs cannot reset that window; diagnostics distinguish
reruns within it from an expired window requiring a fresh trusted invocation.

Even if every quality gate succeeds, the terminal helper fails because an
independently approved governance/ruleset/bypass profile is not established.
No approval schema, ruleset identity or bypass actor is invented. Consequently
Task 14 remains open for independent governance authority and live callable
acceptance, and the remaining checklist exits stay unchecked. No tag/release
trigger, signing, publication, settings change or qualified capability is added.

Preservation check: all 16 pre-existing `test.yml` jobs and five `format.yml` jobs
retain their exact names, conditions, dependencies and matrices. `test.yml` only
appends the pure helper-contract job; `format.yml` is byte-identical. Thus existing
triggers, path classification, caches, native-reproduction selectors, four
candidate legs and the candidate-index collector remain unchanged. The before/
after listing was compared against `d774ee47c` before editing.

Local verification passes the 19 pure Rust cases, three checker command cases,
45 pure Node tests, strict host clippy, documentation, owned-file rustfmt and
Prettier, and the forbidden-action guard. Modified workflows pass actionlint.
The all-workflow actionlint run reproduces three existing diagnostics in unchanged
`deploy-docs.yml`/`docs-proposal.yml` (unquoted output path and locally unsupported
concurrency/permission keys); the baseline at `d774ee47c` reports the same three.
The broad `.github` Prettier check also reports ten unchanged baseline files;
the owned workflow/helper files pass the pinned docs configuration. No unrelated
workflow or shared-tree formatting is changed. Hosted preservation/contract
results after this push remain pending; pure fixtures are not a live invocation.
Independent spec and quality reviews approve this scoped foundation; the narrow
official REST-host suppression also received explicit review. Review fixes cover
Node extraction quoting, least-privilege permissions, documented REST attempt
responses, tag bounds, per-request remaining budgets and composed source-API
coverage. Task 14 is not marked complete by those approvals.

Hosted preservation follow-up (2026-10-21): successful [Run Tests 39596973649](https://github.com/IABTechLab/trusted-server/actions/runs/39596973649)
has head `ac07047ed2b4f1bbe0e1f2e4aac9f845e70b7c7c`, including `2b642b509`.
All 20 expanded test jobs succeeded, including Fastly candidate
[119211581826](https://github.com/IABTechLab/trusted-server/actions/runs/39596973649/job/119211581826),
Linux CLI [119211581675](https://github.com/IABTechLab/trusted-server/actions/runs/39596973649/job/119211581675),
Apple ARM [119219615235](https://github.com/IABTechLab/trusted-server/actions/runs/39596973649/job/119219615235),
Apple Intel [119219615180](https://github.com/IABTechLab/trusted-server/actions/runs/39596973649/job/119219615180),
the index collector [119250788447](https://github.com/IABTechLab/trusted-server/actions/runs/39596973649/job/119250788447)
and helper contracts [119211581539](https://github.com/IABTechLab/trusted-server/actions/runs/39596973649/job/119211581539).
The collector's built source is PR merge `1c78dd4556b03ddaa369680f2248edac1761bcd8`,
distinct from the run head. Matching [Run Format 39596973672](https://github.com/IABTechLab/trusted-server/actions/runs/39596973672)
passed all five jobs. Comparison with `d774ee47c` confirms the same 16 original
test and five format definitions, names, conditions, dependencies and matrices;
only the helper job was appended. Matrix expansion accounts for 19 original test
instances plus the helper. This establishes ordinary-CI preservation, not a live
release-validation invocation or governance/source authority.

**Unresolved maintainer decision before Task 14 can advance:**

1. Select an independently governed approval record and its approving maintainers.
   Approve exact repository/organization ruleset identities, active enforcement,
   ref inclusions/exclusions and restrictions covering `refs/tags/v*`, `main`
   and every future exact reviewed RC branch. Specify release-tag creation
   authority and effective update/deletion restrictions. Eligible branches remain
   exactly `["main"]`; a candidate entry or name pattern cannot approve an RC.
2. Approve exact branch-required check identities, their expected reporting
   applications and branch update policy. Separately approve the release
   invocation's mandatory same-commit gates: admission, Fastly, native/parity,
   Cloudflare/Spin, both host release-tool jobs, JavaScript, docs and terminal
   eligibility. Record actual caller-qualified names when Tasks 15/16 define the
   caller. Another source/run/attempt or reporting application cannot satisfy
   these gates by reusing a successful check name.
3. Approve an empty bypass list or exact actor IDs, actor types, modes and scope
   of every exception, including inherited organization rules and tag creation
   authority. Record who reviews changes and approves exceptions. Administrator
   status, visible settings and candidate assertions do not establish approval.
4. Approve the independent trust root and read-back evidence. Validation must
   obtain bounded official API observations of repository identity, applicable
   repository/inherited ruleset details, active enforcement and ref scope,
   effective required-check/application bindings and exact bypass actors/modes.
   Compare with the independently approved record, binding its reviewed revision
   and approvers. Retain the unpeeled remote tag, actual approved branch tip and
   ancestry, and this source/workflow/run/attempt's complete terminal gate results.
   Missing, unreadable, truncated, ambiguous or changed approval/settings
   evidence must deny eligibility; settings cannot approve themselves.

These are decisions to obtain, not an implemented approval profile or settings
mutation. No ruleset ID, check configuration, bypass actor or approver is selected
here. GitHub describes rule applicability and application-bound checks in
[available rules](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/available-rules-for-rulesets).
The terminal job continues refusing eligibility. Stop governance work here until
maintainers approve the record and Tasks 15/16 supply the reviewed caller and an
authorized live invocation; bounded Tasks 11/10 resume meanwhile.

### Task 15: Orchestrate independent builds and isolated signing

**Phase: 2+ (historical destination task).** Phase 1 uses the reduced implementation checklist above, rather than completing this task.

**Create:** `.github/workflows/release-build.yml`; extend workflow contract fixtures.

- [ ] Test never-started/queued or scheduler-cancelled optional native B jobs as well as pre-step failure: the trusted collector MUST emit a bounded failed/unavailable measurement with absent observations represented honestly and no fabricated upload identity. Confirm the workflow cannot wait indefinitely or hide a mandatory A failure.
- [ ] Test topology: shared JS A/B, separate fresh Fastly A/B, three mandatory native A jobs, bounded optional native B jobs, validated current-run collector and no signing credentials in executable/lifecycle jobs. Require job configurations to apply Task 9 role profiles and publish observed variation/runtime records; tests reject default-only A/B jobs or collectors that bypass these gates.
- [ ] Run `cargo test -p trusted-server-release --target "$HOST_TARGET" workflow_contract` with the nonzero-selection guard; expect absent graph.
- [x] (from Task 13) The consumer rejects the candidate sentinel tag `v0.0.0-unqualified-candidate` (exact match) as the first check of `verify_index` and `verify_build_evidence`, which every online and offline path reaches. It also rejects any consumed build-evidence, build-recipe or rebuild-check companion declaring `"release_qualification": false` or a non-boolean value, after the companion bytes are authenticated against the signed index; an absent field or `true` is accepted because the R-INDEX contracts do not define the field. Both rules hold independently of the empty-bundle rule: a golden candidate index with non-empty bundle records is still rejected (`ac07047ed`). Making the field normative in the spec's companion contract needs a reviewed amendment.
- [ ] Call local reusable workflow at source revision. Build jobs are read-only. Collector uses Task 12 outputs; signing grants only contents-read/id-token-write/attestations-write, plus actions-read when needed. No artifact-metadata write for file attestations. Caller grants enough permissions for called signing jobs but its own build jobs explicitly narrow them.
- [ ] Invoke pinned attest 4.2.2 twice per canonical asset: exact executable/SBOM/evidence/recipe/rebuild provenance subjects, then executable subject + SPDX predicate. Save named bundles, validate all proofs/joins and sign index after collection. Do not construct caller predicates or sign source supplied commands.
- [ ] Rehearse actual certificates/toolkit fields in a disposable repository with test-only policy fixtures. Verify repository IDs, exact SAN, issuer, build/caller/source/event/run fields for both predicate types. Commit `Sign validated release outputs through the reusable builder`.

Progress (2026-10-21, partial, collector logic only): `f17377a4e` implements
the collector side of the never-started/queued/cancelled and pre-step-failure
bullet as pure logic (see Task 12 collector follow-ups): no upload identity can
exist for such a job, outcomes are derived from bounded observed job states,
and a missing mandatory canonical rejects collection under every optional
state. The bullet stays open until the trusted workflow adapter supplies real
job observations and `release-build.yml` bounds the wait; no workflow, signing
or publication was added.

### Task 16: Publish through one immutable-release owner

**Phase: 2+ (historical destination task).** Phase 1 uses the reduced implementation checklist above, rather than completing this task.

**Create:** `.github/workflows/release.yml`, release tool `src/publication.rs` and private `publication::tests`.

- [x] Add fake API cases for matching/conflicting draft, missing/altered asset, concurrent rerun, lost response, already published tag, mutable/unknown final state and permission differences on approved RC commits.
- [ ] Run `cargo test -p trusted-server-release --target "$HOST_TARGET" publication::tests` and `workflow_contract` separately with the nonzero-selection guard; expect absent publication owner.
- [ ] Keep tag-scoped concurrency without cancellation. Draft, upload, re-download and validate the exact asset set before publication. Match source/bytes for draft resume; never overwrite published assets/tags. Publication has contents-write only, no OIDC/provider/settings token.
- [ ] Require maintainer governance approval and immutable/tag-rule setup before enabling tags; afterward read immutable=true and verify GitHub release statement/membership with the normal token. A confirmed mutable release fails publication, is removed only when the workflow release-write role authorizes that still-mutable cleanup, and produces an incident with the cleanup outcome. Never delete an immutable release. Unknown state produces an incident and read-only reconciliation; it MUST NOT trigger blind deletion. A lost publish response also triggers read-only reconciliation before another mutation.
- [ ] Exercise all failure paths in an authorized disposable public repository and record measured evidence. Production tag/permissions require separate release authorization. Commit `Publish complete immutable attested releases`.

Progress (2026-10-21, `9bd8be2c2`, closed pure owner): `src/publication/`
implements the sole-owner state machine against a `ReleaseApi` trait with an
in-process fake (33 tests). It refuses a tag that has a published release or
points at another commit, resumes a draft only on matching tag, commit and
bytes, re-downloads and validates the exact asset set within bounds derived
from the index asset set and role limits, re-checks the tag and draft before
publishing, and reconciles lost responses read-only. After publication it
requires an immutable release whose observed tag, commit and statement
membership equal the intended release; a confirmed mutable release is removed
only with cleanup authority after a fresh read, with an incident carrying the
API failure. Immutable or unknown releases are never deleted. Publication holds
only a contents-write marker built from the caller-attested job permissions.
There is no real API adapter, `release.yml`, `workflow_contract` publication
fixture, command or token handling, so the remaining bullets stay open; partial
uploads after lost responses need a manual recovery runbook.

### Task 17: Implement strict consumer documents and embedded policy

**Phase: 2+ (historical destination task).** Phase 1 uses the reduced implementation checklist above, rather than completing this task.

**Modify:** CLI `release-policy.json` from Task 3.
**Create:** CLI `src/release/mod.rs`, `error.rs`, `policy.rs`, `documents.rs`, `src/release/tests/mod.rs`, `src/release/tests/documents.rs`, `tests/fixtures/release/`.
**Modify:** CLI `src/lib.rs`, `Cargo.toml` (host-only sha2/schema validator with justification).

- [x] Before the first filtered run, wire the host-only `mod release` in CLI `src/lib.rs`, the production child modules in `src/release/mod.rs`, `#[cfg(test)] mod tests` there, and `mod documents` in `src/release/tests/mod.rs`. Add the test cases before checking selection; Rust does not discover this directory automatically. Keep these internals private.
- [x] Test closed schema/kind/profile/action combinations, structurally valid but policy-forbidden `fastly-staging`, duplicate/deep/oversize JSON, self-selected policy, historical policy acceptance/retirement, altered approved-source policy bytes, allowlisted digest paired with the wrong source, unsupported tool and CLI-as-server substitution.
- [x] Run `cargo test -p trusted-server-cli --target "$HOST_TARGET" release::tests::documents`; expect the intended missing implementation or assertion failure. When it compiles, use the nonzero-selection guard below before accepting red or green; a successful run selecting zero tests is a failure of this plan, not a pass.
- [x] Implement byte-first duplicate-rejecting parsers and the same schema/semantic fixtures as producer. Embed reviewed policy bytes; compute their SHA-256 externally at runtime. Policy contains no own digest. First bind producer-policy bytes to the independently approved source commit through the fixed official repository path `crates/trusted-server-cli/release-policy.json` (independently approved source snapshot or fixed official GitHub Contents API acquisition from that exact independently approved full commit); never obtain this authority from the release index or an arbitrary supplied path. Match its exact-byte digest/version, then accept only current policy bytes or the reviewed historical allowlist. For offline checks, independently transfer the approved source-policy snapshot alongside other trust inputs. Downloaded metadata never changes consumer policy.
- [x] Follow [R-POLICY](../specs/2026-10-09-1235-release-attestation-design.md#r-policy): fetch only `https://api.github.com/repos/IABTechLab/trusted-server/contents/crates/trusted-server-cli/release-policy.json?ref=<approved-full-commit>`, `Accept: application/vnd.github.raw+json`, explicit read-only credential, 1 MiB maximum and attempt deadline. Reject arbitrary host/path or credential-forwarding redirects. Add source/API mismatch, unsupported producer v2 under consumer v1, reviewed historical v1 under newer consumer, and current-consumer-bound enforcement negatives. <!-- allow-domain: api.github.com -->
- [x] Keep the already registered tests active and require nonzero selected counts on every focused rerun; process tests use public CLI dispatch rather than making internals public. Keep concrete release errors as Report<ReleaseError>; convert to the existing String dispatch boundary only at the command adapter. Do not introduce a new generic attestation crate or publisher types.
- [x] Fix repository/owner IDs, supported combinations, exact gh/action contract and all R-BOUNDS limits. Re-run tests/clippy. Commit `Parse release evidence under embedded consumer policy`.

### Task 18: Implement the bounded isolated gh runner

**Phase: 2+ (historical destination task).** Phase 1 uses the reduced implementation checklist above, rather than completing this task.

**Create:** CLI `src/release/gh.rs`, `src/release/tests/gh.rs`.

**Modify:** CLI `src/release/mod.rs`, `src/release/tests/mod.rs`.

- [x] Register the new gh production modules and `mod gh` test module before the first focused run. Apply the nonzero-selection guard; missing module registration or zero matches MUST never report a passing security check.

- [x] Write fake-child tests for hangs/cancellation, simultaneous stdout/stderr saturation, malformed/truncated JSON, exit failure, multiple attestations, token precedence, host/config overrides and log redaction.
- [x] Run `cargo test -p trusted-server-cli --target "$HOST_TARGET" release::tests::gh`; expect absent runner.
- [x] Resolve gh once through trusted PATH/explicit trusted installer configuration, require 2.97.0, and retain that executable target. Pass fixed argument vectors with exact certificate identity; never arbitrary forwarded verifier flags. Accept explicit GH_TOKEN then GITHUB_TOKEN, pass only GH_TOKEN, isolate home/config/state, disable prompts, reject loader injection and keep provider credentials out.
- [x] Concurrently drain bounded pipes: 120 seconds per child, 15 minutes per attempt, 128 MiB stdout/1 MiB stderr. Kill/reap on either limit/cancellation. Use real OS fake-child fixtures, not mocks that omit pipe pressure. Failed exit never yields trusted parsed claims.
- [x] Re-run runner negatives and an approved signed public fixture under the pinned gh binary. Actual attest 4.2.2 provenance and SPDX signatures, isolated token-free replay, offline SPDX replay and signed release output were measured; see [consumer qualification report](../../../scripts/release/qualification/2026-10-15-consumer-real-fixtures.md). Commit `Bound and isolate release verification subprocesses`.

### Task 19: Verify signatures, evidence joins and official publication

**Phase: 2+ (historical destination task).** Phase 1 uses the reduced implementation checklist above, rather than completing this task.

**Create:** CLI `src/release/evidence.rs`, `github.rs`, `artifact.rs`, `src/release/tests/verification.rs`.

**Modify:** CLI `src/release/mod.rs`, `src/release/tests/mod.rs`.

- [x] Register the new evidence/github/artifact production modules and `mod verification` test module before the first focused run. Apply the nonzero-selection guard; missing module registration or zero matches MUST never report a passing security check.

- [x] Add a valid graph and independently altered edges for index, all provenance subjects, SPDX subject/document/root, JS prepared/retained records, policy/source/run/caller identities, signed names, REST lengths/immutability and tag object type.
- [x] Run verification tests; expect no implemented evidence graph.
- [x] Verify index provenance before trusting fields. Obtain only approved official GitHub host/repository data with explicit read-only credential and bounded responses. Validate lightweight source target, source approval, independently acquired fixed-path producer policy bytes at that commit, exact signed release names/digests and actual/index/REST sizes. Primitive gh success is insufficient.
- [x] Check verified certificate numeric IDs, exact SAN/issuer/build config/digest/trigger/run URI in both proofs, and provenance build type/caller/external parameters/resolved source/invocation. SPDX lacks SLSA caller fields; use its certificate plus subject/document constraints.
- [x] Separate BuildEvidenceVerified, PublicationVerified and consumer ActionAuthorized states; constructors remain private. A retained verified-release-asset owns private bytes/handle and source/policy/role evidence. Read-only CLI verification returns a VerificationReport, not deployment authorization. The linked integration plan adds the private action-scoped VerifiedArtifact only after concrete context approval; an asserted path/hash cannot construct it. Stream bounded asset responses into retained files, enforcing each role limit while downloading; do not buffer a 512 MiB executable through the gh JSON pipe or check limits only after writing it. Rehash before later use and reject offline official-deploy construction.
- [ ] Official project first-use remains pending: upstream attest 4.2.2 formats and both real predicate signatures are measured in the report above, but third-party fixtures cannot satisfy the official repository/index policy. Exercise first-use verification using an independently trusted previous verifier or source-built verifier from independently approved source, never the downloaded executable verifying itself. Commit `Verify complete official release evidence before authorization`.

### Task 20: Expose read-only CLI verification and bootstrap instructions

**Phase: 2+ (historical destination task).** Phase 1 uses the reduced implementation checklist above, rather than completing this task.

**Create:** CLI `src/commands/release.rs`, `tests/release_cli.rs`, `src/release/offline.rs`, `src/release/tests/offline.rs`, `docs/guide/release-verification.md`.
**Modify:** CLI `src/commands/mod.rs`, `src/run.rs`, `src/release/mod.rs`, `src/release/tests/mod.rs`; `docs/guide/cli.md`, `.vitepress/config.mts`.

- [x] Register `mod offline` in both the private production and test roots before its first focused run. Register the public command module/dispatch before process tests; run `release::tests::offline` with the nonzero-selection guard in addition to the CLI process target.
- [x] Test the required parse path `ts release verify v1.2.0 --asset ts-aarch64-apple-darwin --expected-source-commit <approved-commit>`, invalid source/asset/unsupported offline-deploy flags and no provider/config/build hooks on all failure paths.
- [x] Run `cargo test -p trusted-server-cli --target "$HOST_TARGET" --test release_cli`; expect unknown command.
- [x] Add the command adapter to the reusable host verifier. The initial public surface stays the spec's verify/tag/asset/expected-source contract; any cache/export/output flag MUST be documented and reviewed, not smuggled into an implementation example.
- [x] Document trusted gh installation and independent source/previous-verifier bootstrap before first executing downloaded ts. A source-built verifier provides approved-source trust, not official binary provenance for itself. Describe exact policy checks after primitive gh signatures; do not instruct self-verification or checksum-only installation.
- [x] Add an internal build-evidence-only offline verification path, reusing parser/policy/subject/SPDX joins and bounded gh runner with independently approved roots/source/digest. Test it with networking denied and token absent; its result cannot construct publication/deployment authorization. No initial public offline flag is added. Document the pinned primitive commands and the manual companion/document/policy checks needed beyond their exit status; offline success is build evidence only, no official release membership or offline deploy. Run CLI helper, docs lint/format/build and malformed/timeout/zero-provider-write fixtures. Commit `Expose official release verification and trusted bootstrap`.

### Task 21: Validate and document manual Fastly production consumption

**Phase: 2+ (historical destination task).** Phase 1 uses the reduced implementation checklist above, rather than completing this task.

**Create:** release tool `src/package.rs`, private `package::tests`, bounded manual Fastly archive fixtures and public command tests.
**Modify:** `docs/guide/release-verification.md`, `docs/guide/cli.md`, `CHANGELOG.md`; correct obsolete config-embedding comments in source/test/setup files identified below.

- [x] Test accepted four-member layout, generic manifest-only archive, swept-in files, reserved/empty package name, traversal, links, duplicates, wrong module/manifest, malformed/non-core wasm and max-size bounds.
- [x] Run `cargo test -p trusted-server-release --target "$HOST_TARGET" package::tests` with the nonzero-selection guard; expect absent inspector.
- [ ] Use a fresh private directory, independently bootstrapped approved-source `trusted-server-release inspect-fastly-package`, an allowlisted deployment-only manifest and verified wasm. `compute pack --wasm-binary` has two output archives; select named archive only after exact layout/content checks. Pack/validate accepting a file does not prove valid Wasm or application execution. Verify valid core format and positive publisher-request fixture separately.
- [x] Inspector takes the approved expected executable digest and manifest identity from the successful verification/context procedure, rehashes both the retained archive and its actual module member immediately before submission, and never trusts a digest supplied by the archive. Document trusted-source acquisition for the helper; it is not a separately authenticated release asset.
- [x] Document observed service/base-version bindings and explicit production command with --service-id, --version and --no-default-domain. Explain manifest precedence, automatic clone/activation, possible same-hash skip and inability to supply integrated staging/preactivation guarantees. Serialize manual operator changes. No compute publish/source build is permitted.
- [ ] Run a separately authorized disposable Fastly service submission; compare submitted member and actual returned version, including skip/uncertain cases. Record live proof before declaring AC-9 passed. No customer context or provider credentials enter producer jobs.

  This exit remains unchecked: no live service submission is authorized in the
  local Task 21 increment. Inspector/command tests and instructions do not supply
  submitted-member, returned-version or application-runtime observations.

- [x] Correct false config-embedding text in `.github/actions/setup-integration-test-env/action.yml`, `.github/workflows/test.yml`, integration runtime/framework/parity helpers, browser infra helper and Spin app/middleware/route comments; behavior changes remain separate. Commit `Document and validate verified-byte Fastly deployment`.

- [x] Update `docs/guide/getting-started.md`, `edgezero.md` and `error-reference.md`: canonical `x-ts-version` is full source identity, external tag belongs to the index, and development retains #1213 display fallback. Annotate the historical #1213 goals. Validate manual retained-byte packaging/submission as AC-9/D6 and operator guidance as AC-9/D8; upstream EdgeZero delay MUST NOT block this manual route. Context/preactivation/staging/recovery remain AC-8.

Local implementation (2026-10-21): `inspect-fastly-package` selects only the
named archive derived from a separately approved manifest, checks its four raw
TAR members without extraction and validates actual module bytes with the pinned
core-Wasm inspector. The initial manifest profile is deliberately narrow: exactly
`manifest_version = 3` and a portable nonreserved `name`, in canonical two-line
form without comments. It accepts no build hooks, setup payloads or local fixtures.
Held-file/path identity checks and repeated archive/member hashes precede the
diagnostic report; expected hashes are independent inputs and grant no authority.
Pure archive fixtures and command tests exercise local inspection only.

The guidance documents trusted helper bootstrap, separate retained verified-byte
acquisition, service/base-version observations and explicit production limits.
`ts release verify` still has no public export option. The source-header split
and historical #1213 development fallback remain documented. The checked guidance
item above records the text implementation, not completion of its AC-9/D6 live
validation dependency. An actual pinned Fastly CLI pack/inspect compatibility run,
positive application-request runtime acceptance in this manual operation, and
separately authorized disposable-service submission remain unobserved here.
The pack/private-directory exit and live submission exit stay unchecked; AC-9
and Task 21 are not complete. No production selector, workflow trigger, transfer
adapter, signer, deployment code or provider resource was changed.

Local verification passed 17 pure package tests with a nonzero-selection guard,
both new public-command tests (spawning only the release binary), strict native
all-target/all-feature Clippy, documentation generation, formatting and workflow
lint. The acceptance command failed before dispatch existed and passed afterward;
a non-UTF-8 report-path panic was separately reproduced and corrected. Independent
spec and quality review found no remaining findings. No sandbox/container tests,
Fastly CLI process or provider submission were run locally. The complete hosted
CI matrix and actual pinned-pack compatibility remain separate validation.

### Task 22: Full validation, qualification evidence and release handoff

**Phase: 2+ (historical destination task).** Phase 1 uses the reduced implementation checklist above, rather than completing this task.

**Modify:** the implementation PR's evidence record and docs; no automatic production mutation.

- [ ] Run the complete command block below and collect exit statuses/log summaries. On local certificate/localhost sandbox failures rerun the same tests with needed access; do not weaken TLS or alter assertions.
- [ ] Rehearse hosted source authorization, shared JS, Fastly A/B, all three native A/SBOMs, optional B terminal failures, actual exact-action signatures, immutable draft/publication and full consumer verification together. Toy/synthetic component proofs cannot replace this complete composition.
- [ ] Recheck permission/config separation, governance/rules/bypass authority, bootstrap docs, actual platform evidence and live manual submission. Missing evidence blocks tag enablement; native B equality, packaging or upstream integration do not add hidden blockers.
- [ ] Review against AC-1–AC-7, AC-9–AC-10; keep AC-8 separate. Use @superpowers:requesting-code-review and @superpowers:verification-before-completion. Maintainer approval precedes any production tag.
- [ ] Commit validated documentation/evidence with `Record initial release qualification`; update existing PR #1262 with implementation and validation evidence only when the required checks pass; never create a replacement implementation PR.

## Threat-to-negative-proof trace

| Threat | Existing test families | Plan negative proofs                                                                                               |
| ------ | ---------------------- | ------------------------------------------------------------------------------------------------------------------ |
| TH-1   | T-8, T-9               | Tasks 13, 17, 19: byte/subject/companion tampering and publication membership                                      |
| TH-2   | T-8, T-9, T-10         | Tasks 17–20: independent source-policy acquisition, roots, historical identity and offline limits                  |
| TH-3   | T-2, T-3, T-4, T-5     | Tasks 3, 7–11: source/common-Git/env audit, actual lifecycle/compile denial, native and JS coverage                |
| TH-4   | T-7                    | Tasks 3, 14, 16: unlisted/self-approved RC, remote-ref substitution, governance bypass, skipped/wrong-SHA gates    |
| TH-5   | T-4, T-7               | Tasks 8, 12, 15: authenticated tools/full closure, real boundary, current-run transfers and separated signer       |
| TH-6   | T-9, T-11, T-12        | Tasks 18–21 and linked AC-8: exact gh bootstrap, retained substitution, bounded children, manual equality/recovery |
| TH-7   | T-7, T-8, T-9          | Tasks 15–19: exact certificate/source/caller/run semantics and immutable membership; GitHub remains a trust root   |

## Focused Rust test selection guard

Before accepting any filtered private-module run, require an actual selected test. Register producer modules/tests before filters such as `documents::tests`, `preflight::tests`, `source::tests`, `materials::tests`, `process::tests`, `boundary::tests`, `preparation::tests`, `javascript::tests`, `qualification::tests`, `recipe::tests`, `inspect::tests`, `composition::tests`, `spdx::tests`, `collection::tests`, `distribution::tests`, `publication::tests` and `package::tests`. For Tasks 17–20 select `trusted-server-cli` and its registered `release::tests::{documents,gh,verification,offline}` filters instead. An intended compile failure for missing behavior can establish the first red state; successful zero selection never can.

```sh
set -eu
HOST_TARGET=$(rustc -vV | sed -n 's/^host: //p')
test -n "$HOST_TARGET"
RELEASE_TEST_PACKAGE=trusted-server-release
RELEASE_TEST_FILTER=documents::tests
RELEASE_TEST_LIST=$(mktemp)
trap 'rm -f "$RELEASE_TEST_LIST"' EXIT
cargo test -p "$RELEASE_TEST_PACKAGE" --target "$HOST_TARGET" \
  "$RELEASE_TEST_FILTER" -- --list > "$RELEASE_TEST_LIST"
awk '/: test$/ { count++ } END { if (count == 0) { print "Focused release test filter selected zero tests"; exit 1 } print "Selected " count " release tests" }' "$RELEASE_TEST_LIST"
cargo test -p "$RELEASE_TEST_PACKAGE" --target "$HOST_TARGET" "$RELEASE_TEST_FILTER"
```

Set `HOST_TARGET=$(rustc -vV | sed -n 's/^host: //p')` before each independent shell session/filter. `$APPROVED_SOURCE_COMMIT` MUST come from independent source approval, never the release index. These Cargo commands build a local developer tool only. Qualified production wrappers MUST select the independently approved source and OS-native toolchain described above. The ordinary `test-release-native` CI matrix on Ubuntu 24.04/macOS 15 directly runs all native helper tests, clippy/all-targets/all-features, documentation and actual nonzero native JS `release_prebuilt` tests/clippy. The prior WASI alias selecting zero native tests supplies no coverage. This source-built CI matrix is not production bootstrap or qualification proof. Future aliases require separate review; adapter/CLI aliases do not cover the helper. The explicit host commands below remain required until aliases are reviewed/wired. Never use default WASI output to bootstrap a native producer.

## Full implementation verification commands

Run from the repository root with pinned tools and installed npm dependencies. Linux CI uses its host triple; macOS can use the CLI helper. Record each command independently so a later command cannot mask an earlier failure.

```sh
cargo fmt --all -- --check
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-cloudflare-wasm
cargo clippy-spin-native
cargo clippy-spin-wasm
cargo clippy-cli
cargo clippy-codegen
cargo test-fastly
cargo test-fastly-reuse
cargo test-axum
cargo test-cloudflare
cargo test-spin
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
./scripts/test-cli.sh
cargo test -p trusted-server-core --test template_build_digest
cargo clippy -p trusted-server-core --test template_build_digest -- -D warnings
scripts/lint-wasm-clock.sh
HOST_TARGET=$(rustc -vV | sed -n 's/^host: //p')
cargo test -p trusted-server-release --target "$HOST_TARGET" --all-features
cargo doc -p trusted-server-release --target "$HOST_TARGET" --no-deps --all-features
cargo test -p trusted-server-js --target "$HOST_TARGET" --test release_prebuilt
cargo clippy -p trusted-server-js --target "$HOST_TARGET" --all-targets --all-features -- -D warnings
cargo clippy -p trusted-server-release --target "$HOST_TARGET" --all-targets --all-features -- -D warnings
```

Run the remaining commands with the repository-pinned Node/npm selected explicitly (subdirectory shell startup can otherwise pick a different Node):

```sh
(cd crates/trusted-server-js/lib && node build-all.mjs && TRUSTED_SERVER__RELEASE_JS_TESTS=1 npx vitest run && npm run format)
(cd docs && npm run lint && npm run format && npm run build)
docs/node_modules/.bin/prettier --config docs/.prettierrc --check "*.md" ".claude/**/*.md" ".github/**/*.md" "crates/**/*.md" "scripts/**/*.md" "scripts/**/*.mjs" "tinybird/**/*.md"
RELEASE_HOST_TARGET="$(rustc -vV | awk '/host:/ { print $2 }')"
cargo run -p trusted-server-cli --target "$RELEASE_HOST_TARGET" -- dev lint domains --staged
git diff --check
git diff --cached --check
```

Stage the reviewed implementation files before the staged-domain check; each grouped command stops at its first failure. New producer/schema dependencies MUST be installed at their exact reviewed versions first. No bare workspace cargo test/build or global all-features clippy.

## Traceability and milestone exits

| Requirements                                                          | Tasks / principal negative proof                                                                   | Acceptance |
| --------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------- | ---------- |
| R-COHORT, R-PLATFORM, R-VARIANT                                       | 1,3,8,9: exact cohort/profile, incompatible native runtime, unsupported reuse                      | AC-1, AC-2 |
| R-CONFIG, R-INPUTS, R-ENV                                             | 3–5,7,9: operator inputs, unexpected source files, skips/overrides, source integrity               | AC-2       |
| R-JS                                                                  | 5–7,10: locked A/B, inventory joins, input versus retained bytes                                   | AC-3       |
| R-BUILD, R-REPEAT, R-NATIVE                                           | 8–12: actual confinement, complete wasm equality, valid transfers, honest native outcome           | AC-3       |
| R-SBOM                                                                | 10,11,13,19: real Rust/native/JS coverage and SPDX root/predicate equality                         | AC-4       |
| R-FACTS                                                               | 4: strict full source, owning target/variant, external tag and unchanged service identity          | AC-5       |
| R-SOURCE, R-SIGN, R-PUBLISH                                           | 14–16: approved lightweight main/RC, exact-commit gates, isolated signer, immutable reconciliation | AC-6       |
| R-INDEX, R-EVIDENCE, R-POLICY                                         | 2,13,17,19: schema/semantic joins, subjects and independent policy identity                        | AC-4, AC-7 |
| R-BOOTSTRAP, R-BOUNDS, R-PUBLICATION                                  | 18–20: trustworthy first use, limits/pipe pressure, exact REST/signed membership                   | AC-7       |
| R-OFFLINE                                                             | 19,20: approved build proof only; offline official deploy rejected                                 | AC-7       |
| R-DEPLOY/R-PACK manual portion, R-MANUAL                              | 19,21: verified bytes before mutations, explicit manual limits and live submission                 | AC-9       |
| R-DEPLOY/R-PACK integrated portion, R-CONTEXT, R-EDGEZERO, R-RECOVERY | Linked upstream plan, not first-release closure                                                    | AC-8       |
| Operator/CLI/changelog guidance                                       | 20–22: source-header identity, bootstrap and manual-route instructions (D8)                        | AC-9       |
| R-FOLLOWUP                                                            | 1,20–22: explicit unsupported adapters/reconstruction/offline; January supersession                | AC-10      |

A completed task means its described tests pass and reviewed code exists, not merely that its checkbox was checked. Recipe pins/qualification are concrete outputs of Task 8, not assumed values. An unavailable required host blocks the selected contract; it does not authorize weakening it. For optional CLI B only, bounded unavailability is an honest nonblocking result.

## Planning-only verification and handoff

For these Markdown plans, validate formatting, links, shell syntax, requirement coverage, task dependencies and independent review. Do not claim that runtime tests verify implementation that has not been written. Continue in the existing main checkout on branch `spec/1235-release-attestation` and PR #1262. Automated review is not maintainer approval.
