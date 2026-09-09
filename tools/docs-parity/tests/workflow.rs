use docs_parity::workflow::{
    WorkflowScope, validate_action_references, validate_release_runbook, validate_workflow,
};

const FINAL_WORKFLOW: &str = include_str!("../../../.github/workflows/docs-links.yml");
const CAPTURE_WORKFLOW: &str = include_str!("../../../.github/workflows/test.yml");
const LINK_WRITER: &str = include_str!("../../../scripts/docs-links-reconcile.sh");
const DEPENDENCY_WRITER: &str = include_str!("../../../scripts/dependency-snapshot-submit.sh");

#[test]
fn final_policy_accepts_versioned_actions_and_repository_scripts() {
    validate_workflow(FINAL_WORKFLOW.as_bytes(), WorkflowScope::Final)
        .expect("closed final workflow should pass");
}

#[test]
fn final_policy_rejects_cross_event_and_cross_pull_request_concurrency() {
    for invalid in [
        FINAL_WORKFLOW.replace(
            "documentation-automation-${{ github.event_name == 'pull_request' && format('pr-{0}', github.event.pull_request.number) || 'default-branch-writers' }}",
            "documentation-automation",
        ),
        FINAL_WORKFLOW.replace(
            "cancel-in-progress: \"${{ github.event_name == 'pull_request' }}\"",
            "cancel-in-progress: false",
        ),
        FINAL_WORKFLOW.replace("github.event.pull_request.number", "github.ref"),
    ] {
        assert_ne!(invalid, FINAL_WORKFLOW, "negative fixture must mutate input");
        assert!(
            validate_workflow(invalid.as_bytes(), WorkflowScope::Final).is_err(),
            "concurrency must isolate pull requests and preserve default-branch writers"
        );
    }
}

#[test]
fn final_policy_rejects_trust_boundary_and_script_drift() {
    let invalid = [
        FINAL_WORKFLOW.replace("pull_request:", "pull_request_target:"),
        FINAL_WORKFLOW.replace("  schedule:\n", "  merge_group:\n  schedule:\n"),
        FINAL_WORKFLOW.replace("  workflow_dispatch:\n", "  workflow_dispatch:\n  push:\n"),
        FINAL_WORKFLOW.replacen("actions/checkout@v7.0.1", "actions/checkout@main", 1),
        FINAL_WORKFLOW.replacen(
            "actions-rust-lang/setup-rust-toolchain@v1.17.0",
            "example/unknown@v1",
            1,
        ),
        FINAL_WORKFLOW.replacen("contents: read", "statuses: write", 1),
        FINAL_WORKFLOW.replacen("persist-credentials: false", "persist-credentials: true", 1),
        FINAL_WORKFLOW.replacen("fetch-depth: 0", "fetch-depth: 1", 1),
        FINAL_WORKFLOW.replacen(
            "if: github.repository == 'IABTechLab/trusted-server'",
            "if: always() && github.repository == 'IABTechLab/trusted-server'",
            1,
        ),
        FINAL_WORKFLOW.replacen(
            "ref: \"${{ github.sha }}\"",
            "ref: \"${{ github.event.pull_request.head.sha }}\"",
            1,
        ),
        FINAL_WORKFLOW.replacen(
            "name: link-results\n          path:",
            "name: link-results\n          run-id: \"${{ github.run_id }}\"\n          path:",
            1,
        ),
        FINAL_WORKFLOW.replacen(
            "./scripts/generate-docs-link-results.sh",
            "cargo run --manifest-path tools/docs-parity/Cargo.toml -- links --external",
            1,
        ),
        FINAL_WORKFLOW.replacen(
            "./scripts/docs-links-reconcile.sh",
            "./scripts/unreviewed-writer.sh",
            1,
        ),
        FINAL_WORKFLOW.replacen(
            "./scripts/dependency-snapshot-submit.sh",
            "./scripts/unreviewed-writer.sh",
            1,
        ),
        FINAL_WORKFLOW.replacen(
            "DOCS_PARITY_CHECKED_AT: \"${{ needs.link-reader.outputs.checked-at }}\"",
            "DOCS_PARITY_CHECKED_AT: stale",
            1,
        ),
        FINAL_WORKFLOW.replacen(
            "EXPECTED_SHA256: \"${{ needs.link-reader.outputs.artifact-sha256 }}\"",
            "EXPECTED_SHA256: stale",
            1,
        ),
        FINAL_WORKFLOW.replacen("contents: write", "contents: read", 1),
    ];

    for (case, invalid) in invalid.into_iter().enumerate() {
        assert_ne!(
            invalid, FINAL_WORKFLOW,
            "negative fixture case {case} must mutate input"
        );
        assert!(
            validate_workflow(invalid.as_bytes(), WorkflowScope::Final).is_err(),
            "unsafe workflow fixture case {case} should fail"
        );
    }
}

#[test]
fn final_workflow_keeps_multiline_logic_out_of_yaml_and_python_out_of_writers() {
    assert!(!FINAL_WORKFLOW.contains("run: |"));
    assert!(!FINAL_WORKFLOW.contains("run: >"));
    assert!(!FINAL_WORKFLOW.contains("python"));
    assert!(!LINK_WRITER.contains("python"));
    assert!(!DEPENDENCY_WRITER.contains("python"));
}

#[test]
fn release_runbook_requires_post_main_boundary_owner_sla_and_receipts() {
    let fixture =
        include_str!("../../../docs/internal/runbooks/documentation-automation-release.md");
    validate_release_runbook(fixture.as_bytes()).expect("complete release runbook should pass");

    for required in [
        "only after PR #1049 has reached `main`",
        "The release owner is `aram356`.",
        "within one business day",
        "canonical capture destination",
        "HTTP 201 response",
        "authenticated `github.sha`",
        "credentials disabled",
        "`scripts/dependency-snapshot-submit.sh`",
        "No branch-protection change is selected",
    ] {
        let invalid = fixture.replace(required, "omitted");
        assert!(
            validate_release_runbook(invalid.as_bytes()).is_err(),
            "missing runbook contract should fail: {required}"
        );
    }
}

#[test]
fn capture_scope_requires_exact_read_only_head_checkout_and_script_steps() {
    validate_workflow(CAPTURE_WORKFLOW.as_bytes(), WorkflowScope::CaptureJob)
        .expect("capture job should satisfy the narrow policy");

    for (case, invalid) in [
        CAPTURE_WORKFLOW.replace("persist-credentials: false", "persist-credentials: true"),
        CAPTURE_WORKFLOW.replace("fetch-depth: 0", "fetch-depth: 1"),
        CAPTURE_WORKFLOW.replace("cli-help-${{ matrix.platform }}.zip", "cli-help-*.zip"),
        CAPTURE_WORKFLOW.replacen("compression-level: 0", "compression-level: 6", 1),
        CAPTURE_WORKFLOW.replacen(
            "./scripts/read-tool-versions.sh",
            "echo rust=stable >> \"$GITHUB_OUTPUT\"",
            1,
        ),
        CAPTURE_WORKFLOW.replacen("./scripts/build-trusted-server-js.sh", "npm run build", 1),
        CAPTURE_WORKFLOW.replacen("actions/checkout@v7.0.1", "actions/checkout@main", 1),
    ]
    .into_iter()
    .enumerate()
    {
        assert_ne!(
            invalid, CAPTURE_WORKFLOW,
            "capture case {case} must mutate input"
        );
        assert!(
            validate_workflow(invalid.as_bytes(), WorkflowScope::CaptureJob).is_err(),
            "capture policy should reject drift case {case}"
        );
    }
}

#[test]
fn repository_action_references_require_release_versions() {
    let versioned = concat!(
        "jobs:\n  check:\n    steps:\n",
        "      - uses: actions/checkout@v7.0.1\n",
        "      - uses: fastly/compute-actions/setup@v14\n",
        "      - uses: ./.github/actions/setup-integration-test-env\n",
    );
    validate_action_references(versioned.as_bytes())
        .expect("release-version and local actions should pass");

    for invalid in [
        versioned.replace("v7.0.1", "3d3c42e5aac5ba805825da76410c181273ba90b1"),
        versioned.replace("v7.0.1", "main"),
        versioned.replace("v7.0.1", "V7.0.1"),
        versioned.replace("v7.0.1", "v7.0.1.2"),
        versioned.replace("v7.0.1", "v07.0.1"),
        versioned.replace(
            "./.github/actions/setup-integration-test-env",
            "./.github/actions/../outside",
        ),
    ] {
        assert!(
            validate_action_references(invalid.as_bytes()).is_err(),
            "commit, branch, malformed version, or traversing reference must fail"
        );
    }
}
