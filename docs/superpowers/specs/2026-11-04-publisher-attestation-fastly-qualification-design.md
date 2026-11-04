# Publisher attestation: Fastly executable-binding qualification

**Date:** 2026-11-04. **Status:** Proposed read-only investigation contract and conditional experiment design; no production provider qualification or live mutation authorized.

**Parent contract:** [Publisher attestation](2026-11-04-publisher-attestation-design.md). **Registry protocol:** [assessments and operations](2026-11-04-publisher-attestation-registry-design.md). **Scope:** authenticate the join from an enrolled publisher host and service/version to an installed selected executable, then to independently verified Layer 1 bytes. This is a feasibility/assurance gate, not auction/bid attestation or a new release producer.

## 1. Question and non-assumptions

Can an independent verifier, with read-only Fastly credentials, establish that the executable selected for an enrolled host's active production service/version is byte-for-byte an eligible official Trusted Server WASM?

The verifier starts with authenticated official raw WASM bytes, not merely a checksum string, plus existing enrolled service/host context. It must not infer execution from source headers, service environment variables, upload success, CLI exit zero, publisher-supplied receipts, or the attestation endpoint's own report. No public API for retrieving deployed executable bytes has been established by this investigation.

The strong publisher profile remains disabled until this question has a defensible method. A provider fingerprint observation can still be recorded at lower assurance. A matching fingerprint must not be labelled an official installed binary merely because it appears plausible.

## 2. Confirmed client algorithm

The repository pins Fastly CLI 16.0.0, whose annotated tag resolves to commit `34cef1b0bdbc35d0f1af883a1955d7fe9b3a5ad5`. At [hashfiles.go](https://github.com/fastly/cli/blob/34cef1b0bdbc35d0f1af883a1955d7fe9b3a5ad5/pkg/commands/compute/hashfiles.go), `getFilesHash`:

1. Extracts nondirectory package entries through `packageFiles`.
2. Stores each entry's contents by `NameInArchive` in a map; a repeated exact name replaces the earlier value.
3. Sorts the map's names lexicographically.
4. Concatenates contents in that order into SHA-512. Names and lengths are not included in the hashed stream.

In symbols, for sorted unique entry names `n1 ... nk`:

```text
files_hash = hex(SHA-512(contents[n1] || ... || contents[nk]))
```

For a package with precisely one `bin/main.wasm` and one `fastly.toml` under the same conventional root, their lexical order yields `SHA-512(wasm_bytes || manifest_bytes)`. Additional files also contribute. Tar headers, gzip metadata, and directory entries are not part of that client stream.

At [validate.go](https://github.com/fastly/cli/blob/34cef1b0bdbc35d0f1af883a1955d7fe9b3a5ad5/pkg/commands/compute/validate.go), `packageFiles` skips directories and delegates archive extraction; this code is not a security specification for unique paths/member types. Package validation checks expected basenames and must not substitute for a strict verifier grammar.

The [provider API](https://www.fastly.com/documentation/reference/api/services/package/) describes `metadata.files_hash` as a hash of files within the package and exposes metadata GET and package PUT. It does not by itself establish server acceptance/selection grammar, identical hash calculation, or selected-module readback. The obsolete package `hashsum` is a different field and must not be substituted.

## 3. Security consequence

SHA-512 collision resistance does not make this concatenation an injective package encoding. Different names or divisions between file contents can represent the same hashed byte stream without any SHA-512 collision. Duplicate names and parser differences add ambiguity. Whether an attacker can exploit those properties while satisfying Fastly's actual package/WASM selection rules is not established here.

Strictly inspecting a publisher-supplied canonical archive only proves properties of that supplied archive. An attacker controlling upload can submit a different provider-accepted archive. A locally enforced two-file grammar does not constrain Fastly's remote accepted set. Nor does a matching provider hash prove the publisher supplied the same archive that was uploaded.

Strong qualification therefore requires one of:

| Method                                        | Evidence necessary                                                                                                                                                                                                                                |
| --------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Provider-authenticated selected-module digest | Documented/authenticated provider measurement naming exact service/version and selected raw module bytes, with semantics and algorithm qualified                                                                                                  |
| Authenticated selected-byte readback          | Retrieve provider-authenticated deployed package/module for exact service/version, independently parse selection, hash exact module bytes, and verify its association with the active host                                                        |
| Qualified package-hash binding                | Documented provider acceptance/selection and hashing contract, corroborated by experiments, plus a security argument that no accepted alternate package with a different selected module can produce the same expected stream under that contract |

Bounded positive and negative experiments cannot by themselves prove the final method safe for all accepted packages. If provider contract or selected-byte evidence is missing, retain `fingerprint_only` assurance. Operator-controlled upload tooling, a package comment, or a receipt signed by the publisher is not an independent provider invariant.

## 4. Required observation chain

Each link needs a concrete authenticated evidence source, its retrieval time, version scope, coverage, and failure behavior:

```text
explicitly enrolled publisher host
  → provider host/service mapping and TLS/routing context
  → production active service version
  → package identity for that exact version
  → selected executable bytes or qualified equivalent binding
  → authenticated eligible official release bytes
```

Investigate both versioned and versionless domain mechanisms supported by the actual account, staging, attached resources, resource linkage where needed by the qualified method, and edge propagation. V1 does not require app-config payload reads for identity consistency; the signed public projection remains a publisher declaration. Resolve the production environment explicitly through supported active/environment associations, not latest version or a legacy staging boolean. The [version API](https://www.fastly.com/documentation/reference/api/services/version/) documents some historical booleans as unused; distinguish those management fields from runtime staging identity. Provider API reads may describe desired/control-plane state rather than execution at every POP. An HTTPS challenge observes one serving path; a browser-visible page or response header cannot authenticate the management-plane mapping alone.

Read active provider version/host mapping before and after the challenge, and compare fresh signed public identity bindings when needed. Do not treat those declared values as independently observed configuration. Reject inconsistent intervals. Record that matching observations do not exclude a change-and-restore or prove simultaneous global convergence. State whether a supported profile describes control-plane configured deployment, sampled response, or both.

## 5. Strict local package inspection

For any method consuming an archive, the verifier must use a safe streaming/parser strategy without extracting attacker-selected paths to the filesystem. Enforce a qualified allowlisted archive layout, one selected executable, exact member names/root, unique canonical paths, allowed regular-file/directory types, and exact manifest bytes. Reject absolute paths, traversal, backslash aliases, normalized duplicate names, links, special files, nested archives, multiple executable candidates, unexpected files, malformed headers, truncated streams, and unsupported extensions. If PAX/GNU metadata is necessary, specify its allowed interpretation and reject contradictory names/sizes.

Initial parser safety limits are 100 MB compressed, 256 MiB total expanded regular contents, 128 MiB per member, 16 entries including directories, depth 4, 512-byte names, and a 30-second inspection deadline. These are verifier bounds, not claims of identical provider limits. Confirm actual official artifact fit before adoption; a larger official artifact requires an explicit reviewed profile limit increase, not silent expansion. Hash while reading; do not hold several attacker-controlled expanded copies unnecessarily.

Retain the exact inspected file inventory, content digests/sizes, selected-member identity, and archive digest privately. The archive SHA-256 identifies the submitted envelope; it is not the canonical release executable identity. Publisher `fastly.toml` can disclose private deployment metadata and must not be published in full as attestation evidence. Never execute its scripts or invoke a build-capable command to inspect it.

## 6. Investigation and controlled experiments

### Q1: source and provider contract review, read-only

- Pin the examined CLI version and source commit; inspect `getFilesHash`, archive parser, package validation, upload, skip-same-hash, and activation behavior.
- Identify actual provider metadata and active-version APIs, host mapping APIs, resource links, selector observation, and read permissions.
- Verify the parent contract's existing-store namespaces against the pinned EdgeZero capability model. Do not add a second portable logical secret store or require empty metadata-store association; disabled attestation must preserve strict validation/config-push/provision behavior and existing store quotas. Check exact entry ownership/collision and physical link resolution before enabling keys.
- Ask whether authenticated selected-module digest/readback exists and under what access/retention conditions. Record documentation or provider confirmation; do not invent an API.
- Document what each field measures and whether it is reported, derived, mutable, versionless, or sampled.
- Separate supported facts from hypotheses in a method report. Runtime reports and client source do not establish server semantics.

### Q2: local fixtures, provider-free

- Compute client fingerprints from canonical official-byte packages with independently retained manifests and compare against pinned CLI output.
- Vary compression, tar timestamps/order/root prefixes, manifest whitespace, contents, extra files, duplicate/alias paths, links, and headers. Record fingerprint and local-parser outcomes separately.
- Demonstrate generic concatenation/name ambiguity with harmless fixture bytes. Do not claim a successful provider exploit from a local hash equality alone.
- Validate safe archive rejection and resource bounds with malicious small fixtures. No real credentials or publisher domains enter fixtures.

### Q3: disposable provider observations, requires explicit resource authorization

Before any upload, activation, token creation, or domain/resource change, obtain authorization naming the disposable account/service, allowed actions, cost bounds, and cleanup owner. Use fictional fixture data; do not modify an existing publisher service. Test instrumentation and a reviewable experiment plan are prepared before requesting that authorization.

- Upload canonical packages containing retained official or harmless test module bytes; independently read provider metadata for exact allocated versions.
- Compare local/client/provider hashes; vary only one property per trial. Distinguish successful upload, selected module, activation, and observed response.
- Exercise provider-accepted naming/root/member rules, duplicate/alias handling, extra entries, and safe selected-module distinguishers. A provider rejection is evidence about that fixture only.
- Check staging versus production, new-version activation, same-hash upload skipping, domain mappings, and before/after read races.
- Change a declared public identity value without a new service version and verify the endpoint's loaded snapshot/renewal mismatch behavior separately; no claim of independent full-config appraisal is made.
- Exercise credential restrictions/revocation and audit-event coverage for relevant changes. Missing events or inaccessible resources must remain explicit gaps.
- Check structural/enrollment consistency of the signed public identity projection without reading the full app-config blob for that purpose, while treating `ts-publisher-key-` metadata entries and `ts-publisher-` secret entries as separate namespaced records. Do not publish their private linkage or infer code-exclusive access from a shared store.
- Retain sanitized observations and raw private evidence under restricted access. Clean up only resources the experiment owns; report uncertain outcomes without guessing or retrying mutations blindly.

### Q4: method security review and disposition

The report must contain source/API identity, provider acceptance/selection contract, exact join algorithm, hostile-operator threat argument, retained positive/negative evidence, release verification/eligibility path, host/version/config coverage, permissions, time/propagation limits, and unsupported states. Distinguish absence of an observed counterexample from a justified invariant.

Two independent reviews are required before enabling a strong method: package/provider security review and runtime/operational integration review. Reviewers must assess alternate accepted archives and control-plane versus serving-state limitations, not just canonical equality.

## 7. Gate outcomes and versioning

| Outcome                                                                                            | Permitted publisher result                                                                                |
| -------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------- |
| Authenticated selected executable and all parent-profile checks established                        | `artifact_bound`; `official` only after eligible release-byte verification                                |
| Provider hash matches a locally reconstructed candidate, but selected module is not securely bound | `fingerprint_only`, null installed executable digest and official release; official profile indeterminate |
| Only publisher build report available                                                              | `declared`; no independent installed-artifact claim                                                       |
| Observation fails or races                                                                         | `unavailable`, `fail`, or `indeterminate` as appropriate; no positive official renewal                    |

An approved method gets a stable versioned identifier naming its provider evidence, semantics, and limits. Consumers allowlist that identifier. CLI/provider algorithm changes, archive parser changes, provider acceptance-rule changes, new package layouts, or broadened resource selectors require requalification; they cannot silently inherit the method's approval.

The parent contract defines policy failure and expiry. This investigation must not weaken them to fit an available API. If only lower assurance is feasible, maintainers explicitly decide whether that narrower feature meets their objective; the original strong claim stays unresolved.

## 8. Acceptance

- **Q-AC1:** A reviewed evidence graph distinguishes official raw WASM, publisher archive, provider fingerprint, selected executable, active service/version, host, and the publisher-declared actual request-used identity snapshot.
- **Q-AC2:** The observed client hash algorithm and its ambiguity are documented with exact source and harmless fixtures.
- **Q-AC3:** Provider contract and observations justify the selected method, or an explicit lower-assurance disposition prevents official claims.
- **Q-AC4:** Hostile packages, parser/resource limits, permissions, staging, version/config changes, and failures have retained evidence at the applicable scope.
- **Q-AC5:** No unapproved live provider changes, invented readback capability, or inference from canonical success to universal safety is accepted.
- **Q-AC6:** The qualified path uses the compatible existing-store layout, with provider-valid initialization, collision checks, and no disabled-deployment resource/inventory regression.

No controlled provider experiment has been executed as part of writing this specification.

## Review log

- **Round 1 (Codex, independent security and runtime review):** Checked package ambiguity and remote-provider invariants; no blocking companion issue found.
- **Round 2 (Codex, independent security and runtime review):** Confirmed the selected-executable qualification gate; no blocking companion issue found.
- **Round 3 (Codex, independent security and runtime review):** No blocking companion issue remained.
- **Follow-up correction pass (Codex, independent security and feasibility review):** Added existing-store provisioning qualification and Q-AC6; no blocking companion issue remained. Controlled provider experiments and strong-method qualification remain unexecuted.
