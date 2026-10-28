# Phase 3 Step 1 source-port and execution record

**Issue:** #1235. **PR:** #1463, draft, stacked on Phase 2. **Frozen source:** `122b78664b1235c717e9208fd200f9a9b0c335df`.

Implementation commits: `faf6e8efb3d71a1254e7f256ae60f4a82221c2e1` restores fixtures and frozen-v1 guards; `4682306b1c7c68171da32b05e668e34df8cc69bf` ports the dependency-coupled source and shared assertions. Later record/spec changes are documentation-only.

## Scope and contract checks

The approved source-only slice imports 63 exclusive paths: 62 retain exact frozen bytes; `recipe/tests/candidate_entry.rs` uses the shared Phase 2 helper rather than duplicating it. Fourteen shared support/dependency/assertion paths complete the dependency closure. The [slice map](binary-attestations-slice-map.json) records source and destination SHA-256 plus a reason for every path; the tables below enumerate all 77 paths. Hashes describe source content, not a qualification result.

No workflow, release script, signer, publisher, v2 file or operator runtime changes. No rehearsal tag is required. `spdx::generate_from_completed` still returns an error; all seven `Qualified*` enums remain uninhabited. Candidate evidence cannot activate production generation, signing or index publication. GNU masking remains excluded. The [phase spec](../specs/2026-10-28-1235-binary-attestations-phase3-design.md) records two fresh independent review rounds.

The workflow-contract unit is deferred as a whole: eleven paths, `check-workflow-contract` and three named process tests. See the [full Gate 4 deferral table](binary-attestations-phases-2-4.md#approved-phase-3-step-1-scope-and-gate-4-deferrals). Ownership remains Phase 3; Gate 4 must check the actual integrated release workflows, without a substitute fixture. No imported source references or compiles against the deferred unit.

## Exclusive frozen-source imports

| Path                                                                                                           | Frozen-byte disposition                                                                               |
| -------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------- |
| `crates/trusted-server-release/src/recipe/tests/candidate_entry.rs`                                            | Retain Phase 2 shared js_inventory_sha256 in candidate.rs; remove duplicate and import shared helper. |
| `crates/trusted-server-release/src/recipe/tests/candidate_index.rs`                                            | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/src/recipe/tests/spdx_candidate.rs`                                             | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/src/spdx.rs`                                                                    | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/src/spdx/candidate.rs`                                                          | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/src/spdx/candidate/tests.rs`                                                    | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/src/spdx/cargo.rs`                                                              | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/src/spdx/cargo/tests.rs`                                                        | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/src/spdx/javascript.rs`                                                         | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/src/spdx/javascript/tests.rs`                                                   | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/src/spdx/native.rs`                                                             | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/src/spdx/native/tests.rs`                                                       | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/src/spdx/platform.rs`                                                           | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/src/spdx/platform/tests.rs`                                                     | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/src/spdx/tests.rs`                                                              | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/src/collection.rs`                                                              | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/src/collection/archive.rs`                                                      | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/src/collection/producer.rs`                                                     | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/src/collection/producer/strict_record.rs`                                       | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/src/collection/producer/strict_record/tests.rs`                                 | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/src/collection/records.rs`                                                      | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/src/collection/records/receipt.rs`                                              | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/src/collection/tests.rs`                                                        | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/src/collection/transfer.rs`                                                     | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/src/collection/transfer/ordinary.rs`                                            | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/src/collection/transfer/tests.rs`                                               | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/src/collection/workflow_jobs.rs`                                                | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/src/collection/workflow_jobs/tests.rs`                                          | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/src/collection/workflow_jobs/tests/support.rs`                                  | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/src/collection/workflow_jobs/tests/transfer.rs`                                 | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/src/distribution.rs`                                                            | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/src/distribution/entry.rs`                                                      | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/src/distribution/joins.rs`                                                      | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/src/distribution/ordinary.rs`                                                   | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/src/distribution/summary.rs`                                                    | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/src/distribution/tests.rs`                                                      | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/src/distribution/tests/golden.rs`                                               | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/src/distribution/tests/summary.rs`                                              | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/src/distribution/tests/transfer.rs`                                             | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/src/distribution/transfer.rs`                                                   | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/tests/fixtures/collection/README.md`                                            | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/tests/fixtures/collection/build-record.json`                                    | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/tests/fixtures/collection/composition.json`                                     | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/tests/fixtures/index/candidate/candidate-release-index.json`                    | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/tests/fixtures/index/candidate/trusted-server-fastly.wasm.build-evidence.json`  | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/tests/fixtures/index/candidate/trusted-server-fastly.wasm.build-recipe.json`    | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/tests/fixtures/index/candidate/trusted-server-fastly.wasm.rebuild-check.json`   | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/tests/fixtures/index/candidate/trusted-server-fastly.wasm.spdx.json`            | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/tests/fixtures/index/candidate/ts-aarch64-apple-darwin.build-evidence.json`     | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/tests/fixtures/index/candidate/ts-aarch64-apple-darwin.build-recipe.json`       | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/tests/fixtures/index/candidate/ts-aarch64-apple-darwin.rebuild-check.json`      | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/tests/fixtures/index/candidate/ts-aarch64-apple-darwin.spdx.json`               | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/tests/fixtures/index/candidate/ts-x86_64-apple-darwin.build-evidence.json`      | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/tests/fixtures/index/candidate/ts-x86_64-apple-darwin.build-recipe.json`        | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/tests/fixtures/index/candidate/ts-x86_64-apple-darwin.rebuild-check.json`       | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/tests/fixtures/index/candidate/ts-x86_64-apple-darwin.spdx.json`                | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/tests/fixtures/index/candidate/ts-x86_64-unknown-linux-gnu.build-evidence.json` | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/tests/fixtures/index/candidate/ts-x86_64-unknown-linux-gnu.build-recipe.json`   | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/tests/fixtures/index/candidate/ts-x86_64-unknown-linux-gnu.rebuild-check.json`  | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/tests/fixtures/index/candidate/ts-x86_64-unknown-linux-gnu.spdx.json`           | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/tests/fixtures/index/candidate/tsjs-inventory.json`                             | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-cli/tests/fixtures/release/release-index.json`                                          | Exact frozen bytes.                                                                                   |
| `crates/trusted-server-release/tests/fixtures/README.md`                                                       | Exact frozen bytes.                                                                                   |

## Shared support and restored assertion paths

| Path                                                            | Integration reason                                                                                                                                                                                                             |
| --------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `crates/trusted-server-release/Cargo.toml`                      | Restore only reqwest hickory-dns, tokio, rcgen and rustls via current workspace pins; preserve Rust version 1.98; defer serde_yaml_ng.                                                                                         |
| `crates/trusted-server-release/src/main.rs`                     | Register collection, distribution and spdx with dead-code allowance while no production callers exist; preserve ordinary commands and all existing modules; defer workflow_contract and publication.                           |
| `crates/trusted-server-release/src/error.rs`                    | Restore exact frozen Collection, Spdx and Distribution error variants and displays; defer Publication.                                                                                                                         |
| `crates/trusted-server-release/src/recipe.rs`                   | Restore only frozen evidence-consumed reexports; retain approved ordinary remapping reexports.                                                                                                                                 |
| `crates/trusted-server-release/src/recipe/startup.rs`           | Remove test-only gate on accepted NativeStartupAccepted reexport for restored collection/distribution imports; preserve current startup implementation.                                                                        |
| `crates/trusted-server-release/src/recipe/tests.rs`             | Register frozen tests; retain current ordinary-only Axum exclusion from controlled table.                                                                                                                                      |
| `crates/trusted-server-release/src/recipe/tests/candidate.rs`   | Restore frozen export helpers while retaining approved moved js_inventory_sha256 helper.                                                                                                                                       |
| `crates/trusted-server-release/src/recipe/tests/complete.rs`    | Restore frozen assertions and imports; preserve Phase 2 shared JS inventory helper location.                                                                                                                                   |
| `crates/trusted-server-release/src/recipe/tests/full.rs`        | Restore frozen assertions and imports; preserve Phase 2 shared JS inventory helper location. Retain existing named ignores and annotate reviewed esbuild hook 0.27.3 versus current JS lock 0.28.2; no hook policy adaptation. |
| `crates/trusted-server-release/src/recipe/tests/cli.rs`         | Restore frozen assertions and imports; preserve Phase 2 shared JS inventory helper location. Retain existing named ignores and annotate reviewed esbuild hook 0.27.3 versus current JS lock 0.28.2; no hook policy adaptation. |
| `crates/trusted-server-release/src/recipe/tests/macos_cli.rs`   | Restore frozen assertions and imports; preserve Phase 2 shared JS inventory helper location. Retain existing named ignores and annotate reviewed esbuild hook 0.27.3 versus current JS lock 0.28.2; no hook policy adaptation. |
| `crates/trusted-server-release/src/composition/native/tests.rs` | Restore frozen SPDX/collection assertions removed for Phase 2.                                                                                                                                                                 |
| `Cargo.lock`                                                    | Add only graph entries needed by reqwest hickory-dns and restored release-crate dependencies; preserve every existing package identity/version.                                                                                |
| `crates/trusted-server-release/src/documents/tests.rs`          | Add pure SHA-256 regression guards for frozen v1 policy/schema before source import, then exact v1 CLI and candidate index fixtures after import. No contract edits.                                                           |

## Frozen v1 guards and dependency graph

Two pure regression tests check raw bytes, without rewriting the policy, schema or fixtures:

| Guard input                     | SHA-256                                                            |
| ------------------------------- | ------------------------------------------------------------------ |
| CLI v1 policy                   | `f08ce10cd16ff2d1acc482db2b85eb53b667ec2b4753792f277d12f41d1f636c` |
| Release-index v1 schema         | `06a7e598f1d14b499d087ec0c894829b3aab26a690b35e5922d96987a877bb9e` |
| CLI release-index fixture       | `4ff7a4b64e2724172e19d8ef0b02f500f133719411a15b0ce74d8fce4c8b7034` |
| Candidate release-index fixture | `2e35dd6998ec958eae1f23454c50a5b6306be29d2f5eabdb60fd7360f0ca4203` |

The carried reqwest DNS feature and release-crate dependencies add eleven lockfile package identities: critical-section 1.2.0, crossbeam-channel 0.5.17, crossbeam-epoch 0.9.21, enum-as-inner 0.6.1, hickory-proto/resolver 0.25.2, ipconfig 0.3.4, moka 0.12.16, resolv-conf 0.7.6, tagptr 0.2.0 and widestring 1.2.1. No existing package identity/version is removed or changed. Existing once_cell/reqwest dependency edges expand through feature resolution. Rust/current workspace pins and Phase 2 ordinary normalization stay intact; the deferred workflow-contract dependency is not added.

## Local execution and retained coverage limits

On Apple Silicon, host-target `--all-targets --all-features --locked` check and strict clippy passed with Rust 1.98.1. Formatting, whitespace and an explicit-path domain scan passed. Exactly 198 audited pure unit tests ran individually with exact selectors: 198 passed, zero failed, zero ignored. This is a selected pure suite, not the full process/transport suite.

| Executed pure family                                      |     Count |
| --------------------------------------------------------- | --------: |
| SPDX                                                      |        70 |
| Collection                                                |        63 |
| Distribution                                              |        45 |
| Native composition/SPDX joins                             |         4 |
| Frozen document guards                                    |         2 |
| Recipe candidate-entry / candidate-index / SPDX-candidate | 1 / 4 / 6 |
| Recipe incomplete-composition/export negative cases       |         2 |
| Fixed-table recipe asset selection                        |         1 |

The restored pure assertions include the four `native_spdx_*` join tests, candidate-index identity/reproduction/acceptance guards, all six SPDX-candidate cases, `a_completed_candidate_without_complete_composition_cannot_generate_spdx` and `pure_completion_fixtures_cannot_export_candidate_transfers`. Existing startup-acceptance tests were already exact frozen bytes; this slice does not replace or claim new execution for them.

The restored real-build export/projection/cleanup assertions remain inside these explicitly ignored owners; they were **not executed** and are **not reported as passing**:

| Ignored test                                                                       | Named prerequisites / limitation                                                                                                 |
| ---------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------- |
| `recipe::tests::full::fastly_canonical_and_reproduction_builds_are_byte_identical` | Explicit JS/Cargo fixtures, Linux Docker and pinned Viceroy; controlled observer accepts esbuild 0.27.3, current lock is 0.28.2. |
| `recipe::tests::cli::linux_cli_candidate_builds_inspects_and_starts_cleanly`       | Explicit JS/Cargo/CLI fixtures and Linux Docker; the same controlled-observer mismatch.                                          |
| `recipe::tests::macos_cli::macos_cli_candidate_builds_inspects_and_starts_cleanly` | Explicit macOS Cargo fixture, exported Linux JS handoff, Xcode and native macOS 15; the same controlled-observer mismatch.       |

The existing ignored Cargo-child denial and full preparation collector fixtures retain their named prerequisites. No assertion is deleted to turn an unavailable fixture into a passing test. Ordinary release builds do not use the frozen esbuild observer. Adapting the observer remains separately gated.

Socket/HTTP/TLS, process and command tests retain their existing non-ignored status and run through the existing hosted producer test job. They were not run locally. Hosted results must be read at the exact pushed head before this slice is declared complete; current results belong in the PR test plan.

## Independent reviews and maintainer review

The scope review checked the 63-path import, all shared support/restorations, raw source hashes, closed authority and the eleven-path deferral; no blocking findings. A subsequent quality/security review inspected dependency changes and ownership, archive, credential, transfer and candidate joins; no actionable blocking findings. These were source reviews, not production qualification or execution of ignored fixtures.

The unchanged frozen `collection/transfer.rs` carries one `allow-domain: api.github.com` suppression on its API root. It needs maintainer review under the domain maintenance policy; this slice adds no global allowlist host.

Gate 2's [full-cohort feasibility assessment](binary-attestations-phase3-full-cohort-feasibility.md) is available for the next rollout/assurance decision. Its recommendation and identity alternative remain unadopted; Step 1 does not authorize capture code or v2 Steps 2–5.

## Exact locally executed pure selectors

Each selector below was audited before execution and run separately with the host-target producer binary and `-- --exact`. Every invocation required one passing test and zero ignored tests; no process or transport fixture was selected.

<details>
<summary>198 exact selectors</summary>

```text
collection::producer::strict_record::tests::each_changed_producer_fact_is_rejected_by_the_collector
collection::producer::strict_record::tests::each_changed_producer_receipt_fact_is_rejected_by_the_collector
collection::producer::strict_record::tests::producer_composition_receipt_passes_the_strict_collector
collection::producer::strict_record::tests::producer_job_identity_matches_the_collector_upload_check_and_checkout_root_is_bounded
collection::producer::strict_record::tests::producer_rendered_records_pass_the_unchanged_strict_collector
collection::producer::strict_record::tests::rendered_claim_and_runtime_follow_owned_policy_and_acceptance
collection::producer::tests::each_changed_export_member_blocks_handoff
collection::producer::tests::flat_diagnostic_members_survive_without_original_build_roots
collection::producer::tests::linked_export_members_block_handoff_even_when_bytes_match
collection::producer::tests::missing_and_extra_export_members_block_handoff
collection::tests::bounded_zip_accepts_exact_members_and_streaming_descriptor
collection::tests::candidate_rebuild_details_preserve_observations_and_absence_without_authority
collection::tests::collection_compares_complete_fastly_final_bytes_without_exclusions
collection::tests::collection_rejects_missing_a_and_every_mandatory_gate_even_with_optional_b_absent
collection::tests::collection_rejects_unchanged_supported_variations_and_dishonest_claims
collection::tests::composition_receipt_shape_is_strict
collection::tests::contradictory_premature_or_unbounded_optional_job_states_are_rejected
collection::tests::dependency_channels_only_accept_their_own_build_roles
collection::tests::dependency_repository_cannot_change_the_fixed_api_path
collection::tests::every_composition_receipt_gate_rejects_its_changed_fact
collection::tests::every_optional_reproduction_state_is_accepted_with_honest_outcomes
collection::tests::exact_current_run_archives_feed_the_complete_candidate_comparison
collection::tests::fastly_receipt_must_embed_every_js_module_while_native_may_not
collection::tests::fastly_runtime_proof_is_required_for_both_roles_and_absent_from_native
collection::tests::fastly_runtime_proof_must_belong_to_its_own_build_role
collection::tests::mandatory_fastly_and_native_a_pass_with_never_started_native_b_absent
collection::tests::missing_mandatory_canonical_rejects_under_every_optional_reproduction_state
collection::tests::native_b_records_match_mismatch_failed_and_unavailable_without_masking_a
collection::tests::no_optional_reproduction_state_hides_a_mandatory_canonical_failure
collection::tests::optional_native_b_cannot_hide_any_canonical_composition_or_js_failure
collection::tests::optional_reproduction_job_states_map_to_bounded_honest_outcomes
collection::tests::optional_upload_identity_requires_a_started_successful_job
collection::tests::receipt_coverage_claims_apple_evidence_and_runtime_format_follow_the_target
collection::tests::receipt_entry_point_binds_producer_facts_for_every_fixed_asset
collection::tests::reproduction_reusing_the_canonical_boundary_instance_is_rejected
collection::tests::reproduction_reusing_the_canonical_exact_invocation_is_rejected
collection::tests::reproduction_selected_without_a_supported_variation_is_rejected_by_the_pair_check
collection::tests::transfer_checks_exact_dependency_id_and_archive_bytes
collection::tests::transfer_rejects_stale_metadata_warnings_reused_ids_and_changed_bytes
collection::tests::unix_zip32_deflate_upload_profile_accepts_flat_files_and_rejects_nested_names
collection::tests::uploaded_build_deadlines_are_bounded_before_rest_or_comparison
collection::tests::zip_deflate_preserves_full_bytes_and_rejects_extra_compressed_data
collection::tests::zip_rejects_oversized_declarations_and_unsupported_features
collection::tests::zip_rejects_paths_duplicates_hidden_data_and_corrupt_descriptors
collection::tests::zip_rejects_symlinks_zip64_and_local_central_disagreement
collection::tests::zip_requires_deflate_stream_end_even_when_crc_size_and_consumption_match
collection::workflow_jobs::tests::a_job_without_a_conclusion_is_rejected_before_the_deadline_and_accepted_at_it
collection::workflow_jobs::tests::a_truncated_default_page_is_rejected_as_incomplete
collection::workflow_jobs::tests::completed_job_without_a_conclusion_is_rejected_for_every_role
collection::workflow_jobs::tests::duplicate_missing_or_extra_job_names_are_rejected
collection::workflow_jobs::tests::expected_topology_must_name_every_fixed_build_exactly_once
collection::workflow_jobs::tests::jobs_from_another_run_or_source_are_rejected
collection::workflow_jobs::tests::malformed_oversized_incomplete_or_duplicate_key_responses_are_rejected
collection::workflow_jobs::tests::mandatory_jobs_must_start_succeed_and_upload_under_every_wait
collection::workflow_jobs::tests::native_reproduction_job_states_derive_bounded_honest_outcomes
collection::workflow_jobs::tests::only_a_single_fresh_attempt_is_attestable
collection::workflow_jobs::tests::only_the_wasm_asset_reproduction_is_mandatory
collection::workflow_jobs::tests::started_at_without_an_executed_step_is_not_started
collection::workflow_jobs::tests::successful_run_binds_every_fixed_asset_to_its_exact_job_and_upload
collection::workflow_jobs::tests::transfer::adapted_jobs_feed_exact_uploads_and_absent_reproductions_into_transfer_validation
collection::workflow_jobs::tests::unknown_or_inconsistent_github_states_are_rejected
collection::workflow_jobs::tests::unsuccessful_optional_reproduction_upload_output_is_dropped_without_blocking
collection::workflow_jobs::tests::upload_outputs_must_name_exactly_one_expected_job
composition::native::tests::native_spdx_join_rejects_changed_source_features_context_and_duplicate_contributors
composition::native::tests::native_spdx_rejects_aliases_of_the_same_verified_original_context
composition::native::tests::native_spdx_rejects_missing_or_conflicting_verified_package_facts
composition::native::tests::native_spdx_requires_each_reviewed_consuming_context_and_unambiguous_features
distribution::tests::artifact_validator_rejects_a_fastly_entry_with_the_cli_capability
distribution::tests::assembly_is_deterministic_across_entry_order
distribution::tests::awaiting_signature_renders_the_true_empty_file_record
distribution::tests::build_evidence_lists_sorted_prepared_modules_and_the_embedded_subset
distribution::tests::entry_rejects_a_wrong_spdx_root_or_missing_gaps
distribution::tests::every_informational_companion_is_unqualified_with_gaps_and_null_transfers
distribution::tests::fastly_entry_rejects_a_js_build_material_module
distribution::tests::fixed_identity_accessor_equals_the_schema_row_for_every_asset
distribution::tests::four_valid_entries_assemble_a_schema_valid_candidate_index
distribution::tests::golden::committed_fixture_names_the_actual_tracked_consumer_policy
distribution::tests::golden::producer_rendering_equals_the_committed_candidate_fixtures
distribution::tests::inventory_join_uses_the_consumer_inventory_byte_limit
distribution::tests::join_rejects_companion_size_or_digest_mismatch
distribution::tests::join_rejects_missing_extra_and_duplicate_assets
distribution::tests::join_rejects_mixed_source_policy_or_inventory
distribution::tests::join_rejects_unsafe_colliding_or_misnamed_companions
distribution::tests::policy_identity_derives_version_and_digest_from_the_same_bytes
distribution::tests::policy_identity_rejects_mismatched_missing_zero_or_non_integer_versions
distribution::tests::rebuild_check_is_matched_only_with_fastly_equality
distribution::tests::release_index_validator_accepts_the_existing_positive_fixture
distribution::tests::rendered_check_rejects_fastly_staging_and_non_sentinel_bundles
distribution::tests::summary::fastly_entry_summary_records_matched_outcome_and_recomputed_companions
distribution::tests::summary::native_entry_summary_records_unavailable_outcome_and_reproduction_gap
distribution::tests::summary::recheck_rejects_a_companion_stating_other_gaps
distribution::tests::summary::recheck_rejects_companion_bytes_differing_from_their_record
distribution::tests::summary::recheck_reruns_joins_on_re_recorded_companions
distribution::tests::transfer::collector_rejects_members_or_entry_digest_differing_from_the_descriptor
distribution::tests::transfer::collector_rejects_repeated_upload_ids
distribution::tests::transfer::descriptor_accepts_the_exact_upload_outputs
distribution::tests::transfer::descriptor_rejects_bad_identity_digest_or_name
distribution::tests::transfer::descriptor_rejects_unknown_or_missing_keys_and_another_asset
distribution::tests::transfer::descriptor_rejects_wrong_member_names_or_sizes
distribution::tests::transfer::export_refuses_existing_paths_nested_transfer_or_other_executable_bytes
distribution::tests::transfer::export_writes_exactly_the_six_declared_members
distribution::tests::transfer::exported_entries_rebuild_into_exactly_the_golden_candidate_index
distribution::tests::transfer::index_writer_writes_the_index_inventory_and_sixteen_companions_once
distribution::tests::transfer::rebuild_enforces_the_comparison_rule_per_asset
distribution::tests::transfer::rebuild_rejects_another_policy_version_or_an_oversized_companion
distribution::tests::transfer::rebuild_rejects_changed_executable_inventory_or_spdx_root
distribution::tests::transfer::rebuild_rejects_differing_or_incomplete_gaps
distribution::tests::transfer::rebuild_rejects_evidence_for_another_asset_target_or_commit
distribution::tests::transfer::rebuild_rejects_missing_extra_or_misnamed_members
distribution::tests::transfer::rebuild_rejects_qualified_or_workflow_bound_companions
distribution::tests::transfer::rebuilt_entries_still_reject_mixed_commits_or_inventories
distribution::tests::transfer::rebuilt_entry_rechecks_to_the_original_entry_digest
documents::tests::phase3_preserves_frozen_v1_policy_and_release_index_schema
documents::tests::phase3_restores_exact_frozen_v1_release_index_fixtures
recipe::tests::asset_selection_comes_from_fixed_table_not_json
recipe::tests::candidate_entry::only_the_fastly_asset_expects_a_matched_rebuild_check
recipe::tests::candidate_index::candidate_owner_accepts_the_canonical_build_with_its_acceptance_family
recipe::tests::candidate_index::candidate_owner_rejects_a_reproduction_build
recipe::tests::candidate_index::candidate_owner_rejects_a_source_at_another_commit
recipe::tests::candidate_index::candidate_owner_rejects_the_wrong_acceptance_family
recipe::tests::complete::a_completed_candidate_without_complete_composition_cannot_generate_spdx
recipe::tests::complete::pure_completion_fixtures_cannot_export_candidate_transfers
recipe::tests::spdx_candidate::apple_requires_exclusions_and_the_unmeasured_closure
recipe::tests::spdx_candidate::candidate_measurement_records_identity_without_gating
recipe::tests::spdx_candidate::candidate_rejects_a_completion_without_retained_composition
recipe::tests::spdx_candidate::candidate_rejects_an_approved_source_at_another_commit
recipe::tests::spdx_candidate::fastly_requires_every_unresolved_tool_role
recipe::tests::spdx_candidate::linux_requires_the_gnu_privileged_helper_gap
spdx::candidate::tests::candidate_bounds_reject_documents_over_sixteen_mebibytes
spdx::candidate::tests::candidate_comment_names_the_candidate_and_lists_sorted_unique_gaps
spdx::candidate::tests::candidate_document_validates_and_binds_root_and_document_digests
spdx::candidate::tests::candidate_rendering_rejects_an_empty_gap_list
spdx::candidate::tests::candidate_rendering_rejects_an_unplain_gap
spdx::candidate::tests::merge_counts_equal_the_sum_of_the_parts
spdx::candidate::tests::merge_rejects_a_cross_adapter_duplicate_identity
spdx::candidate::tests::merge_rejects_an_empty_required_part
spdx::candidate::tests::merge_rejects_edges_that_dangle_or_cross_parts
spdx::candidate::tests::root_identity_accepts_the_asset_package_workspace_origin
spdx::candidate::tests::root_identity_rejects_a_registry_origin
spdx::candidate::tests::root_identity_rejects_another_manifest_path_or_commit
spdx::candidate::tests::root_identity_rejects_name_or_version_mismatches
spdx::candidate::tests::spdx_created_rejects_years_beyond_four_digits
spdx::candidate::tests::spdx_created_renders_a_leap_day
spdx::candidate::tests::spdx_created_renders_the_unix_epoch_start
spdx::candidate::tests::spdx_created_renders_time_of_day_and_the_last_four_digit_second
spdx::candidate::tests::spdx_created_skips_february_29_in_a_non_leap_century
spdx::cargo::tests::compiler_roles_fail_closed_and_preserve_host_target_distinction
spdx::cargo::tests::malformed_originals_and_noncanonical_paths_are_rejected
spdx::cargo::tests::multiple_library_outputs_preserve_host_and_target_roles
spdx::cargo::tests::multiple_library_outputs_reject_mixed_duplicate_and_incompatible_kinds
spdx::cargo::tests::original_identity_distinguishes_complete_registry_and_git_facts
spdx::cargo::tests::package_edges_require_both_observed_endpoints_and_skip_dev_kinds
spdx::cargo::tests::per_unit_feature_receipts_remain_separate_for_multiple_roles
spdx::cargo::tests::registry_locked_source_requires_an_actual_https_authority
spdx::cargo::tests::workspace_logical_identity_ignores_relocated_opaque_ids
spdx::javascript::tests::checked_originals_render_distinct_archive_tree_and_generated_evidence
spdx::javascript::tests::equal_module_bytes_do_not_substitute_for_exact_original_inventory
spdx::javascript::tests::javascript_originals_accept_only_their_single_supported_roles
spdx::javascript::tests::javascript_originals_reject_unmeasured_declared_licenses
spdx::javascript::tests::materials_cannot_render_complete_module_containment
spdx::javascript::tests::relocated_owners_preserve_stable_component_identities
spdx::javascript::tests::retained_and_absent_modules_have_different_contributor_roles
spdx::javascript::tests::valid_javascript_role_document_bytes_remain_unchanged
spdx::native::tests::native_license_declarations_require_native_license_evidence
spdx::native::tests::native_logical_ids_do_not_depend_on_private_source_and_output_roots
spdx::native::tests::native_original_archive_checksum_and_measured_outputs_remain_distinct
spdx::native::tests::target_and_host_native_inputs_keep_distinct_roles_and_unknown_implementation_versions
spdx::platform::tests::apple_projection_preserves_raw_file_identity_and_logical_runtime_without_private_paths
spdx::platform::tests::compiler_runtime_archive_is_a_build_dependency_with_no_tool_or_retention_claim
spdx::platform::tests::coverage_gaps_do_not_invent_gnu_measurements_for_other_platforms
spdx::platform::tests::coverage_gaps_name_unresolved_fastly_tools_and_incomplete_native_materials
spdx::platform::tests::coverage_gaps_retain_apple_exclusions_and_unmeasured_tool_closure
spdx::platform::tests::coverage_gaps_retain_gnu_tool_closure_without_unreadable_helpers
spdx::platform::tests::coverage_gaps_retain_gnu_unverified_acquisition_without_unreadable_helpers
spdx::platform::tests::detached_sdk_is_structured_material_without_raw_file_checksum
spdx::platform::tests::fastly_selected_tools_project_with_owner_binding_and_conservative_gaps
spdx::platform::tests::gnu_projection_requires_the_owning_boundary_and_plan_and_calls_candidates_honestly
spdx::platform::tests::libtapi_projects_only_present_measured_bytes_as_a_build_tool
spdx::platform::tests::measured_file_renderer_asserts_only_bytes_and_rejects_runtime_roles
spdx::platform::tests::measured_toolchain_projects_library_resource_and_helper_trees_and_retains_closure_gaps
spdx::platform::tests::new_gnu_file_roles_require_review_before_projection
spdx::platform::tests::projects_measured_trees_without_raw_checksums_and_runtime_without_byte_claims
spdx::platform::tests::rejects_another_artifact_target_rust_tree_or_source_owner
spdx::platform::tests::rendered_projection_preserves_runtime_constraints_without_runtime_byte_checksums
spdx::tests::component_roles_must_be_nonempty_unique_and_compatible_with_origin
spdx::tests::declared_license_facts_remain_exact_without_becoming_license_conclusions
spdx::tests::distinguishes_observed_contributors_retained_js_materials_tools_and_external_requirements
spdx::tests::generated_js_requires_a_known_producer_and_retains_generated_bytes_identity
spdx::tests::javascript_preminification_contributors_do_not_claim_complete_module_retention
spdx::tests::normal_and_build_dependencies_preserve_direction_and_reject_duplicates
spdx::tests::one_cargo_identity_can_be_both_observed_target_and_host_build_tool
spdx::tests::rejects_an_spdx_document_exceeding_the_attestation_byte_limit
spdx::tests::rejects_invalid_source_identity_and_creation_time
spdx::tests::rejects_root_digest_changes_duplicate_identities_missing_origins_and_dangling_edges
spdx::tests::renders_schema_valid_document_with_exact_executable_root_and_document_digest
spdx::tests::stable_ids_and_document_bytes_do_not_depend_on_input_component_order
spdx::tests::unknown_licenses_use_noassertion_and_never_become_unsupported_expressions
spdx::tests::verified_cargo_originals_render_without_inventing_download_locations
```

</details>
