# Attested release binaries and verified consumption Implementation Plan

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
- [ ] **M4: Restore canonical recipes and inspection (Tasks 8–9).** Compose actual Cargo preparation, compile, final acceptance and copying; collect Task 10 metadata/tree/native evidence through that same boundary before hosted qualification. Local tiny artifacts cannot qualify the full product or actual macOS 15/GNU cohort.
- [ ] **Then Tasks 10–22.** Implement the remaining Rust producer modules, workflow contracts, consumer verification and manual route in dependency order. Complete every selected qualification and full CI gate before release enablement/handoff.

The producer helper MUST itself be built and executable inside the selected GNU builder at glibc <=2.34, using an independently approved image/toolchain/material channel. An ambient Ubuntu 24 build cannot be assumed to load in UBI 9. Bind helper target, loader/runtime, source/dependency closure and actual native executable identity before execution. macOS helpers require the approved native CPU/SDK/floor-compatible chain. Initial shell/hash/Rust/Cargo authority is independent of the tool; record and authenticate rustup only if it is actually used (direct signed-component installation does not require it); the tool cannot authenticate itself. Native Rust helper/staged POSIX drivers implement socket and lifecycle probes, without language substitutions.

Current implementation includes strict documents/policy, local source/preflight checks, bounded process supervision, owning build facts, checked JS inventory and Node composition. Original archive authentication, bounded private extraction and lifecycle declaration review are implemented. Final-artifact inspection and experimental native boundaries are implemented, with separate consumer evidence verification. The macOS profile still permits a scrubbed process-group escape and is not a qualified lifecycle-containment backend. Native boundary controls pass ordinary Ubuntu 24.04 and macOS 15 CI; they do not qualify complete release recipes or authenticated tool selections. Original npm and Cargo Git material verification and two-installation JS orchestration are implemented; the full Linux A/B execution test passed in ordinary PR CI. Canonical recipes, authenticated bootstrap/acquisition and hosted qualification remain incomplete. Production wrappers and qualified selection remain closed.

## Current task status — 2026-10-15

Count complete task exits separately from implemented code awaiting external
qualification. There are **7 complete tasks and 15 open tasks**. One open task
has its implementation finished; eight tasks therefore have completed code.
Task counts do not measure remaining effort.

| Status                                         | Count | Tasks                  | Remaining exit work                                                                                                                                                      |
| ---------------------------------------------- | ----- | ---------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Complete                                       | 7     | 2, 4, 5, 6, 17, 18, 20 | Implementation, local verification, real pinned-verifier public-fixture validation where required, and independent review completed                                      |
| Implementation complete; qualification pending | 1     | 19                     | Official Trusted Server signed-release first-use verification; upstream provenance/SPDX format compatibility has been measured                                           |
| Partial or prerequisite approval pending       | 5     | 1, 3, 7, 8, 9          | Maintainer scope approval; authenticated bootstrap/tool selection; macOS lifecycle-containment regressions; platform qualification; full Rust recipes/runtime acceptance |
| Pending implementation                         | 9     | 10–16, 21–22           | Cargo/native composition, SPDX, transfers, evidence collector, trusted workflows, immutable publication, manual Fastly route and final qualification                     |

Task 7's Linux JS exit (M3) passed: an actual ordinary-CI run completed both
fresh offline installations, lifecycle observations, missing-tool and download
controls, both full suites and final-byte equality before constructing
`JsBuildCompleted`. Task 7 stays open only for its macOS lifecycle-containment
regression bullet (the scrubbed process-group escape and macOS descendant
inheritance), which M2 and Tasks 8/9 carry. That candidate run is not hosted
qualification and opens no production selector. Task 9's
structural binary inspection does not prove a successful canonical build or
Fastly publisher request. Ordinary CI passes do not change the pending
qualification pins or authorize release publication.

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

**Files:** Modify this plan and the governing spec only for approved corrections; reference PR #1292's `docs/superpowers/specs/2026-10-12-863-cli-release-packaging-design.md` and `docs/superpowers/plans/2026-10-12-cli-release-packaging.md` after their owning branch is available.

- [ ] Read the current spec revision; record its commit and the same-branch/same-PR delivery direction. Work through reviewed tasks in PR #1262; obtain maintainer contract/code approval before merge. Do not require a separate spec merge or open another implementation PR.
- [ ] Record one publication owner, `.github/workflows/release.yml`, and one signing identity, same-revision `release-build.yml`. #1292 MUST NOT independently publish an overlapping immutable tag while #1235 assets are still pending.
- [ ] Confirm the current four-artifact cohort and three CLI baselines. Initial #1235 delivery may precede packaging on a different new tag. Adding package records or ARM64 requires the linked plan's approved delta, never a covert schema expansion here.
- [ ] Settle one strict producer control channel: `TRUSTED_SERVER__RELEASE_BUILD=1`, injected by trusted recipe after environment sanitation, recorded as an input, rejected if present with another value. This is a fact/input guard, not the prohibited parallel JS fallback mode. Name the checked prebuilt inventory input `TRUSTED_SERVER__TSJS_PREBUILT_INVENTORY`.
- [ ] Confirm permission policy remains runtime-variable; conflicting #1045/#1084 code blocks release enablement. Record the owner and dependency rather than designing a new permission transport.
- [ ] Review milestone exits with maintainers in PR #1262. Commit any approved documentation delta as a focused commit on this branch with `Clarify release implementation prerequisites`.

**Exit:** An engineer can identify approved source, scope, producer controls and the single release authority; no unspecified cross-PR ownership decision remains.

### Task 2: Establish Rust producer fixtures and strict document utilities

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

**Modify:** `crates/trusted-server-core/build.rs`, `build_support/git_version.rs`, `tests/git_version_resolve.rs`, `src/lib.rs`; `crates/trusted-server-cli/src/run.rs`, `src/prebid_bundle.rs`.
**Create:** core `src/build_info.rs`; CLI `build.rs`, `tests/release_build_facts.rs`.
**Modify when adding the owning facts surface:** `crates/trusted-server-adapter-fastly/src/main.rs`; retain existing version-header/service-version behavior.

- [x] Add strict full-commit tests: missing/malformed/branch/tag values fail in release mode without local-git fallback; development behavior stays covered. Add two external tags/same source yielding identical facts, owning-target/variant checks and compiled-path fallback rejection.
- [x] Run `cargo test -p trusted-server-core --test git_version_resolve` and `cargo test -p trusted-server-cli --target "$HOST_TARGET" --test release_build_facts`; expect the new behavior to fail.
- [x] Extend the merged #1213 helper using the approved release control. Derive target from Cargo `TARGET` through `TRUSTED_SERVER__BUILD_TARGET`, package version from the owning product, and default/reuse variant at the Fastly owner. Expose typed immutable facts without importing verifier/publisher keys into core. Do not embed tag, run ID, time or final checksum.
- [x] Set clap's version text to `ts package=<pkg> source=<identity> target=<triple> variant=default`. Read Fastly owning package/target/variant facts through existing startup diagnostics. Remove unread variant and source-owner re-emissions; core remains the source identity owner. Official releases compile out `find_js_lib_dir`'s literal developer checkout fallback; preserve explicit/current-checkout resource discovery. Keep the tracked example config as a fictional scaffold, never active settings.
- [x] Run core fact/template tests, CLI tests/clippy and Fastly version-header tests. Outside-checkout help/version/config-init MUST pass; Prebid source-dependent commands return documented prerequisites. Commit `Derive immutable release build facts`.

### Task 5: Authenticate the existing prebuilt JS interface

**Modify:** `crates/trusted-server-js/build.rs`, `build/bundle_set.rs`, `src/lib.rs` test inclusion.
**Create:** `crates/trusted-server-js/tests/release_prebuilt.rs`.

- [x] Write process/fixture tests for prebuilt mode plus any skip presence, absent inventory, wrong source/digest/size, duplicate IDs, unsafe filenames, missing/empty/extra modules and source changed during copy.
- [x] Run `cargo test -p trusted-server-js --target "$HOST_TARGET" --test release_prebuilt` with the nonzero-selection guard; expect current prebuilt behavior to fail these guards.
- [x] Extend the existing bundle-set validator; require `TRUSTED_SERVER__TSJS_PREBUILT_INVENTORY` in release mode and validate exact input and private copied bytes. Track both paths/control values with Cargo rerun directives. Reject skip before selecting prebuilt/source path; no TSJS_RELEASE_BUILD parallel mode and no read from old `dist/`.
- [x] Preserve ordinary development's supported fresh source build and #1214's inventory-absent prebuilt path, including unrelated non-bundle extras and supported directory aliases. An inventory-present development build validates inventory source syntax/hash but MUST NOT compare it with the override/tag/branch display label. In release Rust jobs require prebuilt inputs and prohibit npm invocation; a fake failing npm proves it is never run.
- [x] Run JS crate tests, Fastly/CLI target checks and prebuilt negatives. Commit `Bind release JS inputs to a checked inventory`.

### Task 6: Collect JS contributions without changing bundle bytes

**Modify:** `crates/trusted-server-js/lib/build-module-options.mjs`, `build-all.mjs`.
**Create:** `lib/test/release-composition.test.mjs`, `scripts/release/collect-js.mjs`.

- [x] Write temporary IIFE fixtures for type-only/external Prebid import, eliminated module, transitive npm contributor and plugin/virtual helper. Build with/without recording and assert identical final JS bytes.
- [x] Run `cd crates/trusted-server-js/lib && npx vitest run test/release-composition.test.mjs`; expect missing recording behavior.
- [x] Add an opt-in Rollup contribution hook at `moduleBuildOptions()`. Join source/package origins to the lockfile, retain installed/lifecycle/build-tool materials separately, and hash final files after Vite/esbuild. Suppress ambient `.env` loading through the reviewed release invocation; explicit production mode alone is not sufficient.
- [x] Emit sorted version-1 inventory with source, module names/lengths/digests and origins; ambiguous contributing ownership fails. State conservative post-Rollup reachability limits.
- [x] Run byte-preservation and ownership negatives plus the full JS test/format checks. Commit `Record release JS composition evidence`.

### Task 7: Prepare and compare two independent JS builds

**Retain/modify:** `scripts/release/build-js.sh`, `bootstrap.sh`, `collect-js.mjs`. **Create:** release tool `src/materials.rs`, `process.rs`, `boundary.rs`, `preparation.rs`, `javascript.rs` and private registered tests; add public process fixtures to `tests/commands.rs`.

- [x] Write tests for archive/revision mismatch, stale extracted tree, prohibited install download, two bundles differing by one byte, test failure and changed source. Assert no generated output exchange between A/B. Prohibited download and missing-tool denial are actual runtime controls exercised in the full Linux fixture; that fixture also asserts each side exposes only its own private roots and shares only read-only originals and tools.
- [x] Run `cargo test -p trusted-server-release --target "$HOST_TARGET" materials::tests`, `process::tests`, `boundary::tests`, `preparation::tests` and `javascript::tests` separately with the nonzero-selection guard; expect missing Rust orchestration.
- [x] Preparation verifies registry/git/npm materials and required native esbuild package archives, then freshly materializes each installation. Run pinned `npm ci --offline --include=dev --no-audit --no-fund` with verified freshly prepared caches in two independent workspaces under the selected denial boundary; review/record lifecycle scripts and fail any lifecycle tool fetch. Rust migration MUST reprove this with actual full local execution and fresh fixtures; Task 8 MUST qualify the actual hosted JS boundary before Task 15 may use it officially. No restored node_modules/generated JS. Read-only original sources/object stores and authenticated archive/tool inputs remain outside private installation outputs. Timeouts/cancellation/pipe limits kill and reap descendant process groups; bounded Docker stop/remove and all temporary-profile/resource cleanup run on success and failure. Bundle under qualified network denial; compilation-time downloads fail. The passing run used an ordinary-CI Docker denial boundary; qualifying that boundary remains Task 8.
- [x] Compare final core and all tracked integration files byte-for-byte. Retain only A plus its original inventory after equality. Construct a private typed `JsBuildCompleted` only from the actual successful two-install/test/build/control/comparison path, binding authenticated source/tools/settings, inventory raw digest and retained module bytes. Rehash at handoff; a checked inventory or JSON completion assertion cannot substitute. Unconditionally inject `TRUSTED_SERVER__RELEASE_JS_TESTS=1` into BOTH actual A/B Vitest processes, independent of inherited developer flags; run every registered test/type check and reject a zero selected suite; verify source-only trees remained intact.
- [x] Prove a missing prepared tool cannot fetch a substitute; rerun negatives and full JS suite. Require both actual fresh full registered JS/type-check suites and all tracked final bundles before `Build and compare shared Rust release JS inputs`; fixture-only counts remain separate. Landed as `Build and compare two fresh offline JavaScript installations` plus follow-up fixes; Linux CI evidence is in the [two-build evidence report](../../../scripts/release/qualification/2026-10-15-js-two-builds.md).

- [ ] Port actual regressions for immutable macOS source/object regular files with `nlink == 1`; reject pre-existing hard links and prove write-through/restoration cannot bypass the boundary. Restrict signals to `same-sandbox`: own-child positive, outside same-user sibling and orchestrator denial negatives. Parent/child/grandchild install lifecycle, Cargo/compiler/proc-macro/native children MUST inherit the same OS-denied controls. Partial: `nlink == 1` scans, hard-link rejection, run-time mutation detection, `same-sandbox` signal positives/negatives and grandchild socket denial pass on macOS 15 CI. Open: a descendant that leaves its process group with a scrubbed environment is not contained, and no macOS install-lifecycle path exists (R-JS uses the single shared Linux JS job). This bullet stays open under M2 and Tasks 8/9.
- [x] Port Docker boundary regressions: use Docker's default private PID namespace (no invalid `--pid=private`), and actually execute positive/denial/recipe controls instead of testing argv alone. Keep BOTH host seccomp profiles and an empty Docker config disjoint from every container-writable root; scan before creating them and rehash BOTH before EVERY host command. Select fixed authenticated Docker client/config/Unix daemon, no TTY/plugins/context or environment overrides, and read-only anchors. Reject escaping and dangling aliases in the CONTAINER namespace. Linux container tests execute actual positive, denial and confined commands in ordinary CI. Cleanup `stop`/`rm` still rehashes every input first: a changed client or config blocks the command, while a changed seccomp profile (which those commands do not read) is recorded as a failure and cleanup proceeds.
- [x] Bound child pipes/deadlines; timeout/cancellation/group kill MUST reap descendants and clear token/provider/injection environment. Record real errno without inventing `ETIMEDOUT`. Use host-independent tool/path/environment fixtures; no ambient shim or interpreter supplies trust.

### Task 8: Qualify real platform environments before fixing recipes

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

**Retain/modify:** `scripts/release/build.sh` and approved native bootstrap. **Create:** release tool `src/recipe.rs`, `inspect.rs`, private registered recipe/native/final-Fastly tests and public-command process fixtures.

- [ ] Test unknown asset/variant, reused target output, source/material mutation, unselected native library, GLIBC symbol ceiling violations, runner-only paths and Mach-O deployment-target mismatch. Add role-specific variation fixtures: missing selected B variation, normalized values misreported as varied, unsupported user variation claimed as passed and incorrect observed concurrency/umask.
- [ ] Run `cargo test -p trusted-server-release --target "$HOST_TARGET" recipe::tests` and `inspect::tests` separately with the nonzero-selection guard; expect missing Rust recipe/inspection behavior.
- [ ] Implement a closed interface: `build.sh --asset <fixed-id> --role A|B --source-commit <approved> --js-dir <absolute> --js-inventory <absolute> --materials <fresh-root> --output <fresh-root>`. Select package/target/features from reviewed source; JSON never supplies executable commands.
- [ ] Implement checked A/B ambient profiles in the recipe and record them in `release-tools.json`: different external checkout roots, A umask 022/B umask 027, UTC versus a qualified alternate timezone, distinct qualified locale settings, different observed start times and A jobs 4/B jobs 2 within the qualified insensitive envelope. Vary execution user only where Task 8 proves support; otherwise record it unsupported without claiming it changed. Capture both ambient inputs and effective compiler values after sanitation/remapping; fixed internal locale/path/epoch values are normalized, not varied. No unreviewed caller overrides can choose profiles. Tests assert genuinely observed supported differences and reject dishonest/omitted records.
- [ ] Apply Task 8's qualified OS/container denial profile in the actual production `build.sh`, not just the qualification prototype. Every production invocation MUST record a successful unconfined socket control and denied confined socket probe, then execute Cargo, compiler, proc-macro, build-script and native-tool children inside that same denied boundary. Authenticated archive/tool acquisition alone may fetch materials; offline installation lifecycle hooks and all compilation run under denial. A build-script/lifecycle download-attempt fixture MUST fail, and a missing or ineffective confinement proof rejects the output; `--frozen` is not the boundary.
- [ ] Build explicit packages with `--release --frozen --message-format=json-render-diagnostics` after preparation. Keep `debug = 1`. Capture compiler/build-script output separately from diagnostic stderr; copy only final output after any reviewed transformations. Do not invoke Fastly source build/annotation. Never strip after evidence/signing.
- [ ] Inspect ELF/Mach-O libraries and paths, run clean Linux startup fixtures and real ARM/Intel macOS startup outside checkout. With differing fictional operator config removed by preflight, require identical sanitized effective source/tool/build inputs for every artifact and reject embedded operator sentinels/config fallback. Require complete executable equality for Fastly; preserve native nonmatching outcomes as measurements rather than adding a native repeatability shipping gate. Separately show runtime configuration can change without rebuilding.
- [ ] Before accepting each final Fastly A/B output, validate actual core-Wasm format with a pinned maintained validator and run a positive publisher-origin response/injection fixture through Viceroy using those exact final bytes. The fixture MUST NOT invoke Cargo/Fastly build or modify the wasm. Assert final hash unchanged, retain runtime outcome, and test non-wasm/component/wrong-ABI/health-only success rejection. These are required producer gates, separate from Task 8 throwaway qualification and later live deployment.
- [ ] Run recipe/native negatives and one actual artifact per selected environment. Commit `Build and inspect canonical release executables`.

### Task 10: Record selected Cargo units and native materials

**Create:** release tool `src/composition.rs`, private `composition::tests`, `crates/trusted-server-release/tests/fixtures/composition/`.

- [ ] Add fixtures for host/target dual-role packages with different unit features, ambiguous source IDs, dev-only graph entries, unknown native origin, system substitution and prepared-but-discarded CLI JS.
- [ ] Run `cargo test -p trusted-server-release --target "$HOST_TARGET" composition::tests` with the nonzero-selection guard; expect absent Rust collector.
- [ ] Join explicit-target compiler/build-script messages with metadata/tree and selected material identities; preserve each observed unit's features/context. Package union features cannot stand in for effective per-unit features. Retain raw observations for audit; do not claim an exact compiler dependency DAG.
- [ ] Capture AWS-LC/ring/SQLite native source and output archive hashes/link directives, Rust sysroot and external platform libraries. Validate generic/target-specific selection against the recipe. Distinguish embedded components, external requirements and build tools.
- [ ] Inspect final files conservatively for complete JS module strings. Record absence explicitly; Fastly and CLI do not need the same retained set. Do not force unused JS into CLI. Commit `Collect artifact-specific build composition` after negatives pass.

### Task 11: Generate complete artifact-specific SPDX

**Create:** release tool `src/spdx.rs`, private `spdx::tests`, `crates/trusted-server-release/tests/fixtures/spdx/`.

- [ ] Write cases for incorrect root digest, missing native coverage, ambiguous package identity, unknown license, duplicate SPDX IDs, dangling relationships, uncompiled dev dependencies, type-only/virtual JS and oversize predicates.
- [ ] Run `cargo test -p trusted-server-release --target "$HOST_TARGET" spdx::tests` with the nonzero-selection guard; expect missing Rust generator.
- [ ] Emit SPDX 2.3 JSON from Task 10 normalized evidence. Root checksum is final executable SHA-256; contributors/materials/tools/external requirements have distinct relationships. Unknown licenses use NOASSERTION; missing contributing origin/coverage fails. Validator uses a pinned authoritative SPDX schema.
- [ ] Generate full real Fastly and all three CLI SBOMs; schema-valid toy fixtures are not sufficient. Validate size <=16 MiB and document equality semantics: reject duplicate keys, ignore object order, preserve array order.
- [ ] Re-run fixtures plus actual coverage inspection. Commit `Generate validated per-artifact SPDX evidence`.

### Task 12: Bind transfers and enforce selected comparisons

**Create:** release tool `src/collection.rs`, private `collection::tests` and exact transfer fixture data.

- [ ] Test stale/wrong-run/source transfers, reused A/B artifact IDs, transfer hash warning, traversal/duplicate ZIP members, missing A, changed inventory, missing supported B variation or dishonest normalized/unsupported claims, absent final-byte runtime proof, missing/failed production socket controls or an unconfined build invocation, and a one-byte final wasm difference.
- [ ] Run `cargo test -p trusted-server-release --target "$HOST_TARGET" collection::tests` with the nonzero-selection guard; expect missing validator.
- [ ] Accept only expected upload-step outputs: IDs/archive digests mapped by trusted workflow dependencies to job/attempt/role/recipe. Re-download exact current-run artifacts and validate REST repository/run/source plus transfer bytes. Artifact API does not independently identify the producing job/attempt. Verify bounded declared ZIP members before extraction.
- [ ] Match expected role-specific ambient profiles from the approved recipe against observed A/B records before signing. For mandatory Fastly A/B, all qualified supported dimensions MUST differ as selected; normalized values remain explicitly identified and omitted supported dimensions fail. Successful native comparisons use their selected observed profile and result binding, but a bounded native B failed/unavailable before environment setup records unavailable fields honestly and cannot be rejected for lacking observations it never produced. Add that exact nonblocking fixture; all native A qualification/composition remains mandatory. Verify final-byte core-format/publisher-request results for both Fastly outputs. Fresh-job identity alone cannot satisfy the variation or runtime contract.
- [ ] Compare complete final Fastly files with no normalized/excluded regions. For native B produce exactly matched/mismatched/failed/unavailable within its deadline; native A failures always block. Record ambient variations and normalized internal values honestly.
- [ ] Preserve A as canonical only after all mandatory requirements pass. Add explicit tests that optional B failure cannot hide any A/composition/JS failure. Commit `Validate release transfers and comparison outcomes`.

### Task 13: Assemble and validate the exact evidence graph

**Create:** release tool `src/distribution.rs`, private `distribution::tests`, `crates/trusted-server-release/tests/fixtures/index/`.
**Schema boundary:** governing index v1 raw bytes remain immutable; any change requires an explicit reviewed amendment.

- [ ] Port positive and adversarial schema fixtures: exact four artifacts, unknown capabilities and structurally compatible but currently forbidden `fastly-staging`, duplicate names/keys, source/cohort/recipe mismatch, unsafe names/newlines, altered lengths/digests and role-specific limits.
- [ ] Run `cargo test -p trusted-server-release --target "$HOST_TARGET" distribution::tests` with the nonzero-selection guard; expect missing assembly/semantic checks.
- [ ] Assemble unsigned companions from successful observed outputs, then artifact provenance/SBOM bundles, then the signed index. Index names/hashes all companions and shared JS inventory; exclude own digest/bundle. Recipe/rebuild records are authenticated informational data, not executable programs or independent trust.
- [ ] Require SPDX parsed predicate/document equality plus separate file hashes. Producer verifies every exact expected provenance subject; no additional unapproved code subject. SHA256SUMS covers every other published asset and not itself.
- [ ] Cross-check schema and semantic joins on all real artifacts before signing/rehearsal. Commit `Assemble the authenticated release distribution`.

### Task 14: Expose trusted same-commit validation

**Create:** `.github/workflows/release-validation.yml`, release tool private workflow-contract tests (register alongside the production policy/dispatch module; do not expose a test-only public API).
**Modify:** `.github/workflows/test.yml`, `format.yml` only to extract/reuse existing required jobs without losing existing triggers/gates.

- [ ] Add static workflow fixtures for wrong SHA/event/ref, unlisted RC branch, malicious candidate allowlist, forged local remote refs, missing governance/ruleset/bypass approval, skipped/path-filtered gates, arbitrary commands, excessive permissions and any floating external action ref in the complete release/validation invocation.
- [ ] Run `cargo test -p trusted-server-release --target "$HOST_TARGET" workflow_contract` with the nonzero-selection guard; expect absent callable validation.
- [ ] SHA-pin every external action in release.yml, release-build.yml and all called release validation jobs, including checkout/setup/cache/upload/download and attest. Preserve readable version comments, audit dependencies and reject floating @v\*/main refs. Local workflow calls are allowed only at the caller's checked source revision. Existing test/format action refs MUST NOT be inherited unreviewed into this path.
- [ ] Expose reviewed workflow_call or explicit jobs running every current AGENTS gate at the tag SHA. Maintain PR/main tests; RC is permitted only by exact reviewed cohort branch entries. Validate strict SemVer/lightweight tag independently; annotated tags fail.
- [ ] Wait up to 90 minutes for this invocation's required terminal validation results; fail cancelled/skipped/timeout results with rerun guidance. Do not trust arbitrary named successful checks or unrelated main commits.
- [ ] Include JS build/tests/format, all Rust aliases/parity/CLI/build-digest, the explicit host-only release-tool test/clippy gate (or reviewed aliases), docs/Markdown and `scripts/lint-wasm-clock.sh`. Commit `Validate release source at the exact tag commit`.

### Task 15: Orchestrate independent builds and isolated signing

**Create:** `.github/workflows/release-build.yml`; extend workflow contract fixtures.

- [ ] Test never-started/queued or scheduler-cancelled optional native B jobs as well as pre-step failure: the trusted collector MUST emit a bounded failed/unavailable measurement with absent observations represented honestly and no fabricated upload identity. Confirm the workflow cannot wait indefinitely or hide a mandatory A failure.
- [ ] Test topology: shared JS A/B, separate fresh Fastly A/B, three mandatory native A jobs, bounded optional native B jobs, validated current-run collector and no signing credentials in executable/lifecycle jobs. Require job configurations to apply Task 9 role profiles and publish observed variation/runtime records; tests reject default-only A/B jobs or collectors that bypass these gates.
- [ ] Run `cargo test -p trusted-server-release --target "$HOST_TARGET" workflow_contract` with the nonzero-selection guard; expect absent graph.
- [ ] Call local reusable workflow at source revision. Build jobs are read-only. Collector uses Task 12 outputs; signing grants only contents-read/id-token-write/attestations-write, plus actions-read when needed. No artifact-metadata write for file attestations. Caller grants enough permissions for called signing jobs but its own build jobs explicitly narrow them.
- [ ] Invoke pinned attest 4.2.2 twice per canonical asset: exact executable/SBOM/evidence/recipe/rebuild provenance subjects, then executable subject + SPDX predicate. Save named bundles, validate all proofs/joins and sign index after collection. Do not construct caller predicates or sign source supplied commands.
- [ ] Rehearse actual certificates/toolkit fields in a disposable repository with test-only policy fixtures. Verify repository IDs, exact SAN, issuer, build/caller/source/event/run fields for both predicate types. Commit `Sign validated release outputs through the reusable builder`.

### Task 16: Publish through one immutable-release owner

**Create:** `.github/workflows/release.yml`, release tool `src/publication.rs` and private `publication::tests`.

- [ ] Add fake API cases for matching/conflicting draft, missing/altered asset, concurrent rerun, lost response, already published tag, mutable/unknown final state and permission differences on approved RC commits.
- [ ] Run `cargo test -p trusted-server-release --target "$HOST_TARGET" publication::tests` and `workflow_contract` separately with the nonzero-selection guard; expect absent publication owner.
- [ ] Keep tag-scoped concurrency without cancellation. Draft, upload, re-download and validate the exact asset set before publication. Match source/bytes for draft resume; never overwrite published assets/tags. Publication has contents-write only, no OIDC/provider/settings token.
- [ ] Require maintainer governance approval and immutable/tag-rule setup before enabling tags; afterward read immutable=true and verify GitHub release statement/membership with the normal token. A confirmed mutable release fails publication, is removed only when the workflow release-write role authorizes that still-mutable cleanup, and produces an incident with the cleanup outcome. Never delete an immutable release. Unknown state produces an incident and read-only reconciliation; it MUST NOT trigger blind deletion. A lost publish response also triggers read-only reconciliation before another mutation.
- [ ] Exercise all failure paths in an authorized disposable public repository and record measured evidence. Production tag/permissions require separate release authorization. Commit `Publish complete immutable attested releases`.

### Task 17: Implement strict consumer documents and embedded policy

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

**Create:** CLI `src/release/gh.rs`, `src/release/tests/gh.rs`.

**Modify:** CLI `src/release/mod.rs`, `src/release/tests/mod.rs`.

- [x] Register the new gh production modules and `mod gh` test module before the first focused run. Apply the nonzero-selection guard; missing module registration or zero matches MUST never report a passing security check.

- [x] Write fake-child tests for hangs/cancellation, simultaneous stdout/stderr saturation, malformed/truncated JSON, exit failure, multiple attestations, token precedence, host/config overrides and log redaction.
- [x] Run `cargo test -p trusted-server-cli --target "$HOST_TARGET" release::tests::gh`; expect absent runner.
- [x] Resolve gh once through trusted PATH/explicit trusted installer configuration, require 2.97.0, and retain that executable target. Pass fixed argument vectors with exact certificate identity; never arbitrary forwarded verifier flags. Accept explicit GH_TOKEN then GITHUB_TOKEN, pass only GH_TOKEN, isolate home/config/state, disable prompts, reject loader injection and keep provider credentials out.
- [x] Concurrently drain bounded pipes: 120 seconds per child, 15 minutes per attempt, 128 MiB stdout/1 MiB stderr. Kill/reap on either limit/cancellation. Use real OS fake-child fixtures, not mocks that omit pipe pressure. Failed exit never yields trusted parsed claims.
- [x] Re-run runner negatives and an approved signed public fixture under the pinned gh binary. Actual attest 4.2.2 provenance and SPDX signatures, isolated token-free replay, offline SPDX replay and signed release output were measured; see [consumer qualification report](../../../scripts/release/qualification/2026-10-15-consumer-real-fixtures.md). Commit `Bound and isolate release verification subprocesses`.

### Task 19: Verify signatures, evidence joins and official publication

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

**Create:** CLI `src/commands/release.rs`, `tests/release_cli.rs`, `src/release/offline.rs`, `src/release/tests/offline.rs`, `docs/guide/release-verification.md`.
**Modify:** CLI `src/commands/mod.rs`, `src/run.rs`, `src/release/mod.rs`, `src/release/tests/mod.rs`; `docs/guide/cli.md`, `.vitepress/config.mts`.

- [x] Register `mod offline` in both the private production and test roots before its first focused run. Register the public command module/dispatch before process tests; run `release::tests::offline` with the nonzero-selection guard in addition to the CLI process target.
- [x] Test the required parse path `ts release verify v1.2.0 --asset ts-aarch64-apple-darwin --expected-source-commit <approved-commit>`, invalid source/asset/unsupported offline-deploy flags and no provider/config/build hooks on all failure paths.
- [x] Run `cargo test -p trusted-server-cli --target "$HOST_TARGET" --test release_cli`; expect unknown command.
- [x] Add the command adapter to the reusable host verifier. The initial public surface stays the spec's verify/tag/asset/expected-source contract; any cache/export/output flag MUST be documented and reviewed, not smuggled into an implementation example.
- [x] Document trusted gh installation and independent source/previous-verifier bootstrap before first executing downloaded ts. A source-built verifier provides approved-source trust, not official binary provenance for itself. Describe exact policy checks after primitive gh signatures; do not instruct self-verification or checksum-only installation.
- [x] Add an internal build-evidence-only offline verification path, reusing parser/policy/subject/SPDX joins and bounded gh runner with independently approved roots/source/digest. Test it with networking denied and token absent; its result cannot construct publication/deployment authorization. No initial public offline flag is added. Document the pinned primitive commands and the manual companion/document/policy checks needed beyond their exit status; offline success is build evidence only, no official release membership or offline deploy. Run CLI helper, docs lint/format/build and malformed/timeout/zero-provider-write fixtures. Commit `Expose official release verification and trusted bootstrap`.

### Task 21: Validate and document manual Fastly production consumption

**Create:** release tool `src/package.rs`, private `package::tests`, bounded manual Fastly archive fixtures and public command tests.
**Modify:** `docs/guide/release-verification.md`, `docs/guide/cli.md`, `CHANGELOG.md`; correct obsolete config-embedding comments in source/test/setup files identified below.

- [ ] Test accepted four-member layout, generic manifest-only archive, swept-in files, reserved/empty package name, traversal, links, duplicates, wrong module/manifest, malformed/non-core wasm and max-size bounds.
- [ ] Run `cargo test -p trusted-server-release --target "$HOST_TARGET" package::tests` with the nonzero-selection guard; expect absent inspector.
- [ ] Use a fresh private directory, independently bootstrapped approved-source `trusted-server-release inspect-fastly-package`, an allowlisted deployment-only manifest and verified wasm. `compute pack --wasm-binary` has two output archives; select named archive only after exact layout/content checks. Pack/validate accepting a file does not prove valid Wasm or application execution. Verify valid core format and positive publisher-request fixture separately.
- [ ] Inspector takes the approved expected executable digest and manifest identity from the successful verification/context procedure, rehashes both the retained archive and its actual module member immediately before submission, and never trusts a digest supplied by the archive. Document trusted-source acquisition for the helper; it is not a separately authenticated release asset.
- [ ] Document observed service/base-version bindings and explicit production command with --service-id, --version and --no-default-domain. Explain manifest precedence, automatic clone/activation, possible same-hash skip and inability to supply integrated staging/preactivation guarantees. Serialize manual operator changes. No compute publish/source build is permitted.
- [ ] Run a separately authorized disposable Fastly service submission; compare submitted member and actual returned version, including skip/uncertain cases. Record live proof before declaring AC-9 passed. No customer context or provider credentials enter producer jobs.
- [ ] Correct false config-embedding text in `.github/actions/setup-integration-test-env/action.yml`, `.github/workflows/test.yml`, integration runtime/framework/parity helpers, browser infra helper and Spin app/middleware/route comments; behavior changes remain separate. Commit `Document and validate verified-byte Fastly deployment`.

- [ ] Update `docs/guide/getting-started.md`, `edgezero.md` and `error-reference.md`: canonical `x-ts-version` is full source identity, external tag belongs to the index, and development retains #1213 display fallback. Annotate the historical #1213 goals. Validate manual retained-byte packaging/submission as AC-9/D6 and operator guidance as AC-9/D8; upstream EdgeZero delay MUST NOT block this manual route. Context/preactivation/staging/recovery remain AC-8.

### Task 22: Full validation, qualification evidence and release handoff

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
