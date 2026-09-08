//! Static policy for trusted GitHub Actions documentation workflows.

use std::path::Path;

use error_stack::Report;
use serde_yaml::{Mapping, Value};

use crate::repository::{NormalizedRelativePath, Repository};

const MAXIMUM_WORKFLOW_BYTES: usize = 512 * 1024;
const MAXIMUM_JOBS: usize = 64;
const CAPTURE_JOB: &str = "cli-help-capture";
const CHECKOUT_ACTION: &str = "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1";
const SETUP_RUST_ACTION: &str =
    "actions-rust-lang/setup-rust-toolchain@166cdcfd11aee3cb47222f9ddb555ce30ddb9659";
const SETUP_NODE_ACTION: &str = "actions/setup-node@820762786026740c76f36085b0efc47a31fe5020";
const UPLOAD_ACTION: &str = "actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a";
const DOWNLOAD_ACTION: &str = "actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c";
const FINAL_GUARD: &str = "github.repository == 'IABTechLab/trusted-server' && github.ref == 'refs/heads/main' && (github.event_name == 'schedule' || github.event_name == 'workflow_dispatch')";
const LINK_WRITER_SCRIPT: &str = r#"set -euo pipefail
archive="$RUNNER_TEMP/link-results/link-results.zip"
json="$RUNNER_TEMP/link-results.json"
test "$(find "$RUNNER_TEMP/link-results" -mindepth 1 -maxdepth 1 -print | wc -l)" -eq 1
test -f "$archive"
test ! -L "$archive"
test "$(stat -c '%a' "$archive")" = 644
test "$(stat -c '%s' "$archive")" -le 2097152
test "$(sha256sum "$archive" | cut -d ' ' -f 1)" = "$EXPECTED_SHA256"
python3 -c 'import pathlib, struct, sys, zipfile; data = pathlib.Path(sys.argv[1]).read_bytes(); assert len(data) >= 22 and data[:4] == b"PK\x03\x04"; signature, disk, central_disk, disk_entries, total_entries, central_size, central_offset, comment_length = struct.unpack("<4s4H2LH", data[-22:]); assert signature == b"PK\x05\x06" and disk == central_disk == 0 and disk_entries == total_entries == 1 and central_size != 0xffffffff and central_offset != 0xffffffff and comment_length == 0 and central_offset + central_size == len(data) - 22; archive = zipfile.ZipFile(sys.argv[1]); infos = archive.infolist(); assert len(infos) == 1 and infos[0].filename == sys.argv[2] and not infos[0].is_dir() and infos[0].external_attr >> 16 in (0o644, 0o100644) and infos[0].file_size <= int(sys.argv[3])' "$archive" "link-results.json" 1048576
test "$(unzip -Z1 "$archive")" = "link-results.json"
test "$(zipinfo -l "$archive" | awk '/link-results.json$/ { print $1 }')" = "-rw-r--r--"
unzip -p "$archive" link-results.json > "$json"
test "$(stat -c '%s' "$json")" -le 1048576
jq -e --arg repository "$EXPECTED_REPOSITORY" --arg ref "$EXPECTED_REF" --arg sha "$EXPECTED_SOURCE_SHA" --argjson run_id "$EXPECTED_RUN_ID" --argjson run_attempt "$EXPECTED_RUN_ATTEMPT" 'def bounded: type == "string" and utf8bytelength <= 2048; keys == ["checked_at", "findings", "repository", "run_attempt", "run_id", "schema_version", "source_ref", "source_sha"] and .schema_version == 1 and .repository == $repository and .source_ref == $ref and .source_sha == $sha and .run_id == $run_id and .run_attempt == $run_attempt and (.checked_at | bounded) and (.findings | type == "array" and length <= 500) and all(.. | strings; utf8bytelength <= 2048) and all(.findings[]; keys == ["diagnostic", "final_url", "kind", "requested_url", "status"] and (.kind == "unreachable" or .kind == "http_status" or .kind == "redirect") and (.requested_url | bounded) and (.diagnostic | bounded) and (.final_url == null or (.final_url | bounded)) and (.status == null or (.status | type == "number" and floor == . and . >= 100 and . <= 599)))' "$json" >/dev/null
python3 - "$json" <<'PY'
import datetime
import json
import re
import sys
import unicodedata
import urllib.parse

with open(sys.argv[1], encoding="utf-8") as source:
    result = json.load(source)

checked_at = result["checked_at"]
assert re.fullmatch(r"[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z", checked_at)
assert int(checked_at[:4]) >= 1970
datetime.datetime.strptime(checked_at, "%Y-%m-%dT%H:%M:%SZ")

def valid_url(value):
    if not value or len(value.encode("utf-8")) > 2048 or "\\" in value:
        return False
    if any(unicodedata.category(character) == "Cc" for character in value):
        return False
    parsed = urllib.parse.urlsplit(value)
    try:
        parsed.port
    except ValueError:
        return False
    return (
        parsed.scheme == "https"
        and parsed.hostname is not None
        and parsed.username is None
        and parsed.password is None
        and not any(character.isspace() for character in parsed.netloc)
    )

for finding in result["findings"]:
    assert finding["diagnostic"].strip()
    assert valid_url(finding["requested_url"])
    if finding["final_url"] is not None:
        assert valid_url(finding["final_url"])
PY
title="[docs-parity] External link findings"
owner_marker="<!-- docs-parity:external-links:v1 -->"
issues_json="$RUNNER_TEMP/external-link-issues.json"
issue_body="$RUNNER_TEMP/external-link-issue.md"
gh api --paginate --slurp "repos/$EXPECTED_REPOSITORY/issues?state=all&per_page=100" > "$issues_json"
jq -e 'type == "array" and all(.[]; type == "array" and all(.[]; type == "object" and (.number | type == "number" and floor == . and . > 0) and (.title | type == "string") and (.body == null or (.body | type == "string")) and (.state == "open" or .state == "closed")))' "$issues_json" >/dev/null
owned_count="$(jq -er --arg title "$title" --arg marker "$owner_marker" '[.[][] | select((has("pull_request") | not) and .title == $title and ((.body // "") | startswith($marker)))] | length' "$issues_json")"
foreign_count="$(jq -er --arg title "$title" --arg marker "$owner_marker" '[.[][] | select((has("pull_request") | not) and .title == $title and (((.body // "") | startswith($marker)) | not))] | length' "$issues_json")"
test "$owned_count" -le 1
test "$foreign_count" -eq 0
issue_number="$(jq -er --arg title "$title" --arg marker "$owner_marker" '[.[][] | select((has("pull_request") | not) and .title == $title and ((.body // "") | startswith($marker)))] | if length == 1 then .[0].number | tostring else "" end' "$issues_json")"
issue_state="$(jq -er --arg title "$title" --arg marker "$owner_marker" '[.[][] | select((has("pull_request") | not) and .title == $title and ((.body // "") | startswith($marker)))] | if length == 1 then .[0].state else "" end' "$issues_json")"
findings="$(jq '.findings | length' "$json")"
printf '%s\n\n' "$owner_marker" > "$issue_body"
cat "$json" >> "$issue_body"
if test "$findings" -gt 0; then
  if test "$owned_count" -eq 0; then
    gh issue create --repo "$EXPECTED_REPOSITORY" --title "$title" --body-file "$issue_body"
  else
    if test "$issue_state" = closed; then
      gh issue reopen --repo "$EXPECTED_REPOSITORY" "$issue_number"
    else
      test "$issue_state" = open
    fi
    gh issue comment --repo "$EXPECTED_REPOSITORY" "$issue_number" --body-file "$issue_body"
  fi
elif test "$owned_count" -eq 1; then
  if test "$issue_state" = open; then
    gh issue close --repo "$EXPECTED_REPOSITORY" "$issue_number" --comment "The latest complete scan is clean."
  else
    test "$issue_state" = closed
  fi
fi
"#;
const DEPENDENCY_WRITER_SCRIPT: &str = r#"set -euo pipefail
archive="$RUNNER_TEMP/dependency-snapshot/dependency-snapshot.zip"
json="$RUNNER_TEMP/dependency-snapshot.json"
test "$(find "$RUNNER_TEMP/dependency-snapshot" -mindepth 1 -maxdepth 1 -print | wc -l)" -eq 1
test -f "$archive"
test ! -L "$archive"
test "$(stat -c '%a' "$archive")" = 644
test "$(stat -c '%s' "$archive")" -le 4194304
test "$(sha256sum "$archive" | cut -d ' ' -f 1)" = "$EXPECTED_SHA256"
python3 -c 'import pathlib, struct, sys, zipfile; data = pathlib.Path(sys.argv[1]).read_bytes(); assert len(data) >= 22 and data[:4] == b"PK\x03\x04"; signature, disk, central_disk, disk_entries, total_entries, central_size, central_offset, comment_length = struct.unpack("<4s4H2LH", data[-22:]); assert signature == b"PK\x05\x06" and disk == central_disk == 0 and disk_entries == total_entries == 1 and central_size != 0xffffffff and central_offset != 0xffffffff and comment_length == 0 and central_offset + central_size == len(data) - 22; archive = zipfile.ZipFile(sys.argv[1]); infos = archive.infolist(); assert len(infos) == 1 and infos[0].filename == sys.argv[2] and not infos[0].is_dir() and infos[0].external_attr >> 16 in (0o644, 0o100644) and infos[0].file_size <= int(sys.argv[3])' "$archive" "dependency-snapshot.json" 2097152
test "$(unzip -Z1 "$archive")" = "dependency-snapshot.json"
test "$(zipinfo -l "$archive" | awk '/dependency-snapshot.json$/ { print $1 }')" = "-rw-r--r--"
unzip -p "$archive" dependency-snapshot.json > "$json"
test "$(stat -c '%s' "$json")" -le 2097152
jq -e --arg ref "$EXPECTED_REF" --arg sha "$EXPECTED_SOURCE_SHA" --arg id "$EXPECTED_RUN_ID.$EXPECTED_RUN_ATTEMPT" 'def package($key): keys == ["package_url", "relationship", "scope"] and .package_url == ("pkg:cargo/" + $key) and ($key | test("^[A-Za-z0-9._+-]+@[A-Za-z0-9._+-]+$")) and (.relationship == "direct" or .relationship == "indirect") and (.scope == "runtime" or .scope == "development"); keys == ["detector", "job", "manifests", "ref", "sha", "version"] and .version == 0 and .sha == $sha and .ref == $ref and .job == {"correlator":"trusted-server-docs-parity-v1","id":$id} and .detector == {"name":"trusted-server-docs-parity","url":"https://github.com/IABTechLab/trusted-server/tree/main/tools/docs-parity","version":"0.1.0"} and (.manifests | keys == ["Cargo.lock", "tools/docs-parity/Cargo.lock"]) and all(.manifests | to_entries[]; (.value | keys == ["file", "name", "resolved"]) and .value.name == .key and .value.file == {"source_location":.key} and (.value.resolved | type == "object") and all(.value.resolved | to_entries[]; .key as $key | .value | package($key))) and ([.manifests[].resolved | length] | add <= 5000) and all(.. | strings; utf8bytelength <= 2048)' "$json" >/dev/null
status="$(gh api --include --method POST "repos/IABTechLab/trusted-server/dependency-graph/snapshots" --input "$json" | sed -n '1s/.* \([0-9][0-9][0-9]\).*/\1/p')"
test "$status" = 201
"#;

/// Repository workflow policy activated for a validation call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkflowScope {
    /// Validate only the permanent native CLI capture job in an existing workflow.
    CaptureJob,
    /// Validate the complete steady-state documentation workflow fixture.
    Final,
}

/// Static workflow-policy failure.
#[derive(Debug, derive_more::Display)]
#[display("invalid documentation workflow: {detail}")]
pub struct WorkflowError {
    detail: String,
}

impl core::error::Error for WorkflowError {}

/// Parse a workflow as YAML data and apply the selected closed policy.
///
/// # Errors
///
/// Returns an error for malformed or oversized YAML, untrusted events,
/// expanded permissions, mutable actions, privileged checkout, unsafe
/// services/caches/local actions, unbounded jobs or artifacts, or writer jobs
/// that execute repository code.
pub fn validate_workflow(bytes: &[u8], scope: WorkflowScope) -> Result<(), Report<WorkflowError>> {
    if bytes.len() > MAXIMUM_WORKFLOW_BYTES {
        return Err(workflow_error("workflow exceeds 524288 bytes"));
    }
    let text =
        core::str::from_utf8(bytes).map_err(|_error| workflow_error("workflow is not UTF-8"))?;
    let root = serde_yaml::from_str::<Value>(text)
        .map_err(|_error| workflow_error("workflow YAML cannot be parsed"))?;
    let mapping = value_mapping(&root, "workflow root")?;
    require_allowed_keys(
        mapping,
        &["name", "permissions", "on", "concurrency", "jobs"],
        "workflow root",
    )?;
    let root_permissions = required(mapping, "permissions")?;
    if scope == WorkflowScope::Final {
        validate_default_permissions(root_permissions)?;
    } else {
        let root_permissions = value_mapping(root_permissions, "workflow permissions")?;
        let entries = string_entries(root_permissions, "workflow permissions")?;
        if !entries.is_empty()
            && (entries.len() != 1
                || entries[0].0 != "contents"
                || scalar_string(entries[0].1) != Some("read"))
        {
            return Err(workflow_error(
                "capture workflow permissions may be only contents: read",
            ));
        }
    }
    let jobs = value_mapping(required(mapping, "jobs")?, "jobs")?;
    if jobs.is_empty() || jobs.len() > MAXIMUM_JOBS {
        return Err(workflow_error("job count is outside bounds"));
    }
    match scope {
        WorkflowScope::CaptureJob => {
            validate_capture_events(required(mapping, "on")?)?;
            let capture = required(jobs, CAPTURE_JOB)?;
            validate_capture_job(value_mapping(capture, CAPTURE_JOB)?)
        }
        WorkflowScope::Final => validate_final(mapping, jobs),
    }
}

/// Require every YAML `uses:` value to be a normalized local action or an
/// external action pinned to a lowercase 40-hex commit.
///
/// # Errors
///
/// Returns an error for malformed or oversized YAML, mutable or abbreviated
/// external references, and unsafe local-action paths.
pub fn validate_action_references(bytes: &[u8]) -> Result<(), Report<WorkflowError>> {
    if bytes.len() > MAXIMUM_WORKFLOW_BYTES {
        return Err(workflow_error("workflow exceeds 524288 bytes"));
    }
    let text =
        core::str::from_utf8(bytes).map_err(|_error| workflow_error("workflow is not UTF-8"))?;
    let root = serde_yaml::from_str::<Value>(text)
        .map_err(|_error| workflow_error("workflow YAML cannot be parsed"))?;
    visit_action_references(&root)
}

/// Validate the committed post-merge documentation-automation runbook.
///
/// # Errors
///
/// Returns an error when the runbook is oversized, non-UTF-8, or omits a
/// required release boundary, owner, SLA, capture rule, or verification step.
pub fn validate_release_runbook(bytes: &[u8]) -> Result<(), Report<WorkflowError>> {
    if bytes.len() > MAXIMUM_WORKFLOW_BYTES {
        return Err(workflow_error("release runbook exceeds 524288 bytes"));
    }
    let text = core::str::from_utf8(bytes)
        .map_err(|_error| workflow_error("release runbook is not UTF-8"))?;
    for fragment in [
        "only after PR #1049 has reached `main`",
        "The release owner is `aram356`.",
        "within one business day",
        "within two business days",
        "canonical capture destination",
        "UTF-8 byte length and SHA-256",
        "State remains `release-pending`",
        "HTTP 201 response",
        "expected GitHub App",
        "No branch-protection change is selected",
    ] {
        if !text.contains(fragment) {
            return Err(workflow_error(format!(
                "release runbook is missing `{fragment}`"
            )));
        }
    }
    Ok(())
}

fn visit_action_references(value: &Value) -> Result<(), Report<WorkflowError>> {
    match value {
        Value::Mapping(mapping) => {
            for (key, child) in mapping {
                if scalar_string(key) == Some("uses") {
                    let action = scalar_string(child)
                        .ok_or_else(|| workflow_error("uses value must be a string"))?;
                    validate_action_reference(action)?;
                }
                visit_action_references(child)?;
            }
        }
        Value::Sequence(sequence) => {
            for child in sequence {
                visit_action_references(child)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_action_reference(action: &str) -> Result<(), Report<WorkflowError>> {
    if let Some(relative) = action.strip_prefix("./") {
        if relative.is_empty()
            || relative.contains(['\\', '@'])
            || relative
                .split('/')
                .any(|component| component.is_empty() || matches!(component, "." | ".."))
        {
            return Err(workflow_error("local action path is not normalized"));
        }
        return Ok(());
    }
    let (name, revision) = action
        .split_once('@')
        .ok_or_else(|| workflow_error("external action lacks an immutable revision"))?;
    if action.matches('@').count() != 1
        || !lower_hex_sha(revision)
        || name.split('/').count() < 2
        || name.split('/').any(|component| {
            component.is_empty()
                || !component
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        })
    {
        return Err(workflow_error(
            "external action must use a normalized lowercase 40-hex pin",
        ));
    }
    Ok(())
}

fn lower_hex_sha(value: &str) -> bool {
    value.len() == 40
        && value
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

pub(crate) fn check_capture_repository(
    repository: &Repository,
) -> Result<(), Report<WorkflowError>> {
    let path = NormalizedRelativePath::new(Path::new(".github/workflows/test.yml"))
        .map_err(|_error| workflow_error("test workflow path is invalid"))?;
    let bytes = repository
        .read_tracked_bounded(&path, MAXIMUM_WORKFLOW_BYTES)
        .map_err(|_error| workflow_error("cannot read test workflow"))?;
    validate_workflow(&bytes, WorkflowScope::CaptureJob)
}

/// Validate the final documentation workflow and repository automation pins.
///
/// # Errors
///
/// Returns an error when a required workflow, action, tool pin, cache input,
/// Dependabot root, or workspace lint declaration is absent or unsafe.
pub(crate) fn check_repository(repository: &Repository) -> Result<(), Report<WorkflowError>> {
    check_capture_repository(repository)?;
    let final_workflow = read_repository_file(repository, ".github/workflows/docs-links.yml")?;
    validate_workflow(&final_workflow, WorkflowScope::Final)?;
    let release_runbook = read_repository_file(
        repository,
        "docs/internal/runbooks/documentation-automation-release.md",
    )?;
    validate_release_runbook(&release_runbook)?;

    for path in repository
        .tracked_paths()
        .map_err(|_error| workflow_error("cannot enumerate tracked automation files"))?
    {
        let text = path
            .as_utf8()
            .map_err(|_error| workflow_error("automation path is not UTF-8"))?;
        if is_automation_yaml(text) {
            let bytes = repository
                .read_tracked_bounded(&path, MAXIMUM_WORKFLOW_BYTES)
                .map_err(|_error| workflow_error(format!("cannot read automation file: {text}")))?;
            validate_action_references(&bytes)?;
        }
    }

    for (path, fragments) in [
        (
            ".github/workflows/codeql.yml",
            &["branches: [\"main\", \"rc/*\"]"][..],
        ),
        (
            ".github/workflows/deploy-docs.yml",
            &["- \".tool-versions\"", "docs/package-lock.json"][..],
        ),
        (
            ".github/workflows/format.yml",
            &[
                "documentation-parity:",
                "tools/docs-parity -> tools/docs-parity/target",
                "crates/trusted-server-js/lib/package-lock.json",
                "docs/package-lock.json",
            ][..],
        ),
        (
            ".github/workflows/test.yml",
            &[
                "documentation-rustdoc:",
                "fetch-depth: 0",
                "crates/trusted-server-js/lib/package-lock.json",
            ][..],
        ),
        (
            ".github/workflows/integration-tests.yml",
            &[
                "crates/trusted-server-integration-tests/browser/package-lock.json",
                "crates/trusted-server-js/lib/package-lock.json",
                "wrangler@$WRANGLER_VERSION",
            ][..],
        ),
        (
            ".github/dependabot.yml",
            &[
                "package-ecosystem: \"github-actions\"",
                "directory: \"/tools/docs-parity\"",
                "directory: \"/crates/trusted-server-integration-tests/browser\"",
                "directory: \"/crates/trusted-server-integration-tests/fixtures/frameworks/nextjs\"",
                "target-branch: \"main\"",
            ][..],
        ),
        (".tool-versions", &["wrangler 4.129.0"][..]),
        (
            "crates/trusted-server-openrtb-codegen/Cargo.toml",
            &["[lints]", "workspace = true"][..],
        ),
    ] {
        let bytes = read_repository_file(repository, path)?;
        let text = core::str::from_utf8(&bytes)
            .map_err(|_error| workflow_error(format!("automation file is not UTF-8: {path}")))?;
        for fragment in fragments {
            if !text.contains(fragment) {
                return Err(workflow_error(format!(
                    "automation contract is missing `{fragment}` in {path}"
                )));
            }
        }
    }
    Ok(())
}

fn read_repository_file(
    repository: &Repository,
    path_text: &str,
) -> Result<Vec<u8>, Report<WorkflowError>> {
    let path = NormalizedRelativePath::new(Path::new(path_text))
        .map_err(|_error| workflow_error(format!("automation path is invalid: {path_text}")))?;
    repository
        .read_tracked_bounded(&path, MAXIMUM_WORKFLOW_BYTES)
        .map_err(|_error| workflow_error(format!("cannot read automation file: {path_text}")))
}

fn is_automation_yaml(path: &str) -> bool {
    (path.starts_with(".github/workflows/") || path.starts_with(".github/actions/"))
        && (path.ends_with(".yml") || path.ends_with(".yaml"))
}

fn validate_final(root: &Mapping, jobs: &Mapping) -> Result<(), Report<WorkflowError>> {
    require_exact_keys(
        root,
        &["name", "permissions", "on", "concurrency", "jobs"],
        "final workflow root",
    )?;
    require_exact_string(root, "name", "Documentation automation")?;
    validate_final_events(required(root, "on")?)?;
    let concurrency = value_mapping(required(root, "concurrency")?, "concurrency")?;
    require_exact_keys(concurrency, &["group", "cancel-in-progress"], "concurrency")?;
    require_exact_string(concurrency, "group", "documentation-automation")?;
    if scalar_bool(required(concurrency, "cancel-in-progress")?) != Some(false) {
        return Err(workflow_error(
            "final concurrency must not cancel in progress",
        ));
    }
    require_exact_keys(
        jobs,
        &[
            "pull-request",
            "link-reader",
            "issue-writer",
            "dependency-reader",
            "dependency-writer",
        ],
        "final jobs",
    )?;
    validate_pull_request_job(value_mapping(
        required(jobs, "pull-request")?,
        "pull-request",
    )?)?;
    validate_link_reader(value_mapping(
        required(jobs, "link-reader")?,
        "link-reader",
    )?)?;
    validate_issue_writer(value_mapping(
        required(jobs, "issue-writer")?,
        "issue-writer",
    )?)?;
    validate_dependency_reader(value_mapping(
        required(jobs, "dependency-reader")?,
        "dependency-reader",
    )?)?;
    validate_dependency_writer(value_mapping(
        required(jobs, "dependency-writer")?,
        "dependency-writer",
    )?)
}

fn validate_final_events(value: &Value) -> Result<(), Report<WorkflowError>> {
    let events = value_mapping(value, "final events")?;
    require_exact_keys(
        events,
        &["pull_request", "schedule", "workflow_dispatch"],
        "final events",
    )?;
    if !required(events, "pull_request")?.is_null()
        || !required(events, "workflow_dispatch")?.is_null()
    {
        return Err(workflow_error(
            "pull_request and workflow_dispatch must not declare options",
        ));
    }
    let schedule = required(events, "schedule")?
        .as_sequence()
        .ok_or_else(|| workflow_error("schedule must be a sequence"))?;
    if schedule.len() != 1 {
        return Err(workflow_error("schedule must contain exactly one entry"));
    }
    let schedule = value_mapping(&schedule[0], "schedule entry")?;
    require_exact_keys(schedule, &["cron"], "schedule entry")?;
    require_exact_string(schedule, "cron", "17 9 * * 1")
}

fn validate_pull_request_job(job: &Mapping) -> Result<(), Report<WorkflowError>> {
    require_exact_keys(
        job,
        &[
            "name",
            "if",
            "runs-on",
            "timeout-minutes",
            "permissions",
            "steps",
        ],
        "pull-request job",
    )?;
    validate_job_header(
        job,
        "Documentation parity",
        "github.event_name == 'pull_request'",
        30,
        "contents",
        "read",
    )?;
    let steps = steps(job, "pull-request")?;
    if !matches!(steps.len(), 6 | 7) {
        return Err(workflow_error(
            "pull-request job must contain six steps and at most one local action",
        ));
    }
    validate_checkout_step(&steps[0], "${{ github.event.pull_request.head.sha }}")?;
    let read_node_index = if steps.len() == 7 {
        validate_local_action_step(&steps[1])?;
        2
    } else {
        1
    };
    validate_run_step(
        &steps[read_node_index],
        Some("Read pinned Node version"),
        Some("node-version"),
        None,
        "echo \"node=$(awk '$1 == \\\"nodejs\\\" { print $2 }' .tool-versions)\" >> \"$GITHUB_OUTPUT\"",
    )?;
    validate_setup_rust_step(&steps[read_node_index + 1])?;
    validate_action_step(
        &steps[read_node_index + 2],
        SETUP_NODE_ACTION,
        &[(
            "node-version",
            StringValue::Text("${{ steps.node-version.outputs.node }}"),
        )],
    )?;
    validate_run_step(
        &steps[read_node_index + 3],
        Some("Install JSDoc lint dependencies"),
        None,
        Some("crates/trusted-server-js/lib"),
        "npm ci",
    )?;
    validate_run_step(
        &steps[read_node_index + 4],
        None,
        None,
        None,
        "cargo run --manifest-path tools/docs-parity/Cargo.toml -- check --all",
    )
}

fn validate_link_reader(job: &Mapping) -> Result<(), Report<WorkflowError>> {
    require_exact_keys(
        job,
        &[
            "name",
            "if",
            "runs-on",
            "timeout-minutes",
            "permissions",
            "outputs",
            "steps",
        ],
        "link-reader job",
    )?;
    validate_job_header(
        job,
        "Check external documentation links",
        FINAL_GUARD,
        30,
        "contents",
        "read",
    )?;
    validate_digest_output(job)?;
    let steps = steps(job, "link-reader")?;
    if steps.len() != 5 {
        return Err(workflow_error(
            "link-reader must contain exactly five steps",
        ));
    }
    validate_checkout_step(&steps[0], "${{ github.sha }}")?;
    validate_setup_rust_step(&steps[1])?;
    validate_run_step(
        &steps[2],
        Some("Generate closed link result"),
        None,
        None,
        concat!(
            "checked_at=\"$(date -u +'%Y-%m-%dT%H:%M:%SZ')\"\n",
            "DOCS_PARITY_CHECKED_AT=\"$checked_at\" cargo run --manifest-path tools/docs-parity/Cargo.toml -- links --external --artifact \"$RUNNER_TEMP/link-results.zip\"\n",
        ),
    )?;
    validate_run_step(
        &steps[3],
        Some("Record link artifact digest"),
        Some("digest"),
        None,
        "echo \"sha256=$(sha256sum \"$RUNNER_TEMP/link-results.zip\" | cut -d ' ' -f 1)\" >> \"$GITHUB_OUTPUT\"",
    )?;
    validate_artifact_upload(&steps[4], "link-results", "link-results.zip")
}

fn validate_dependency_reader(job: &Mapping) -> Result<(), Report<WorkflowError>> {
    require_exact_keys(
        job,
        &[
            "name",
            "if",
            "runs-on",
            "timeout-minutes",
            "permissions",
            "outputs",
            "steps",
        ],
        "dependency-reader job",
    )?;
    validate_job_header(
        job,
        "Generate dependency snapshot",
        FINAL_GUARD,
        20,
        "contents",
        "read",
    )?;
    validate_digest_output(job)?;
    let steps = steps(job, "dependency-reader")?;
    if steps.len() != 5 {
        return Err(workflow_error(
            "dependency-reader must contain exactly five steps",
        ));
    }
    validate_checkout_step(&steps[0], "${{ github.sha }}")?;
    validate_setup_rust_step(&steps[1])?;
    validate_run_step(
        &steps[2],
        Some("Generate closed dependency snapshot"),
        None,
        None,
        "cargo run --manifest-path tools/docs-parity/Cargo.toml -- dependency-snapshot generate --output \"$RUNNER_TEMP/dependency-snapshot.zip\"",
    )?;
    validate_run_step(
        &steps[3],
        Some("Record dependency artifact digest"),
        Some("digest"),
        None,
        "echo \"sha256=$(sha256sum \"$RUNNER_TEMP/dependency-snapshot.zip\" | cut -d ' ' -f 1)\" >> \"$GITHUB_OUTPUT\"",
    )?;
    validate_artifact_upload(&steps[4], "dependency-snapshot", "dependency-snapshot.zip")
}

fn validate_issue_writer(job: &Mapping) -> Result<(), Report<WorkflowError>> {
    validate_writer_job_header(
        job,
        "issue-writer",
        "Reconcile external-link issue",
        "link-reader",
        "issues",
    )?;
    let steps = steps(job, "issue-writer")?;
    if steps.len() != 2 {
        return Err(workflow_error(
            "issue-writer must contain exactly two steps",
        ));
    }
    validate_artifact_download(&steps[0], "link-results")?;
    validate_writer_command_step(
        &steps[1],
        "Validate LinkResultsV1 and reconcile one owned issue",
        &[
            (
                "EXPECTED_SHA256",
                "${{ needs.link-reader.outputs.artifact-sha256 }}",
            ),
            ("EXPECTED_REPOSITORY", "${{ github.repository }}"),
            ("EXPECTED_REF", "${{ github.ref }}"),
            ("EXPECTED_SOURCE_SHA", "${{ github.sha }}"),
            ("EXPECTED_RUN_ID", "${{ github.run_id }}"),
            ("EXPECTED_RUN_ATTEMPT", "${{ github.run_attempt }}"),
            ("GH_TOKEN", "${{ github.token }}"),
        ],
        LINK_WRITER_SCRIPT,
    )
}

fn validate_dependency_writer(job: &Mapping) -> Result<(), Report<WorkflowError>> {
    validate_writer_job_header(
        job,
        "dependency-writer",
        "Submit dependency snapshot",
        "dependency-reader",
        "contents",
    )?;
    let steps = steps(job, "dependency-writer")?;
    if steps.len() != 2 {
        return Err(workflow_error(
            "dependency-writer must contain exactly two steps",
        ));
    }
    validate_artifact_download(&steps[0], "dependency-snapshot")?;
    validate_writer_command_step(
        &steps[1],
        "Validate and submit unchanged version-0 snapshot",
        &[
            (
                "EXPECTED_SHA256",
                "${{ needs.dependency-reader.outputs.artifact-sha256 }}",
            ),
            ("EXPECTED_REF", "${{ github.ref }}"),
            ("EXPECTED_SOURCE_SHA", "${{ github.sha }}"),
            ("EXPECTED_RUN_ID", "${{ github.run_id }}"),
            ("EXPECTED_RUN_ATTEMPT", "${{ github.run_attempt }}"),
            ("GH_TOKEN", "${{ github.token }}"),
        ],
        DEPENDENCY_WRITER_SCRIPT,
    )
}

fn validate_job_header(
    job: &Mapping,
    name: &str,
    condition: &str,
    timeout: u64,
    permission: &str,
    level: &str,
) -> Result<(), Report<WorkflowError>> {
    require_exact_string(job, "name", name)?;
    require_exact_string(job, "if", condition)?;
    require_exact_string(job, "runs-on", "ubuntu-latest")?;
    if scalar_u64(required(job, "timeout-minutes")?) != Some(timeout) {
        return Err(workflow_error(format!("job {name} has the wrong timeout")));
    }
    if permissions(job)? != [(permission.to_owned(), level.to_owned())] {
        return Err(workflow_error(format!(
            "job {name} has the wrong permissions"
        )));
    }
    Ok(())
}

fn validate_writer_job_header(
    job: &Mapping,
    context: &str,
    name: &str,
    needs: &str,
    permission: &str,
) -> Result<(), Report<WorkflowError>> {
    require_exact_keys(
        job,
        &[
            "name",
            "if",
            "needs",
            "runs-on",
            "timeout-minutes",
            "permissions",
            "steps",
        ],
        context,
    )?;
    validate_job_header(job, name, FINAL_GUARD, 5, permission, "write")?;
    require_exact_string(job, "needs", needs)
}

fn steps<'a>(job: &'a Mapping, context: &str) -> Result<&'a [Value], Report<WorkflowError>> {
    required(job, "steps")?
        .as_sequence()
        .map(Vec::as_slice)
        .ok_or_else(|| workflow_error(format!("{context} steps must be a sequence")))
}

fn validate_setup_rust_step(value: &Value) -> Result<(), Report<WorkflowError>> {
    validate_action_step(
        value,
        SETUP_RUST_ACTION,
        &[
            ("toolchain", StringValue::Text("1.95.0")),
            ("cache", StringValue::Boolean(false)),
        ],
    )
}

fn validate_digest_output(job: &Mapping) -> Result<(), Report<WorkflowError>> {
    let outputs = value_mapping(required(job, "outputs")?, "job outputs")?;
    require_exact_keys(outputs, &["artifact-sha256"], "job outputs")?;
    require_exact_string(
        outputs,
        "artifact-sha256",
        "${{ steps.digest.outputs.sha256 }}",
    )
}

fn validate_artifact_upload(
    value: &Value,
    artifact: &str,
    filename: &str,
) -> Result<(), Report<WorkflowError>> {
    let path = format!("${{{{ runner.temp }}}}/{filename}");
    validate_action_step(
        value,
        UPLOAD_ACTION,
        &[
            ("name", StringValue::Text(artifact)),
            ("path", StringValue::Text(&path)),
            ("if-no-files-found", StringValue::Text("error")),
            ("retention-days", StringValue::Unsigned(7)),
            ("compression-level", StringValue::Unsigned(0)),
        ],
    )
}

fn validate_artifact_download(value: &Value, artifact: &str) -> Result<(), Report<WorkflowError>> {
    let path = format!("${{{{ runner.temp }}}}/{artifact}");
    validate_action_step(
        value,
        DOWNLOAD_ACTION,
        &[
            ("name", StringValue::Text(artifact)),
            ("path", StringValue::Text(&path)),
        ],
    )
}

fn validate_local_action_step(value: &Value) -> Result<(), Report<WorkflowError>> {
    let step = value_mapping(value, "local action step")?;
    require_exact_keys(step, &["uses"], "local action step")?;
    let uses = scalar_string(required(step, "uses")?)
        .ok_or_else(|| workflow_error("local action reference must be a string"))?;
    let path = uses
        .strip_prefix("./")
        .ok_or_else(|| workflow_error("local action must begin with ./"))?;
    if !path.starts_with(".github/actions/") || path.contains('@') {
        return Err(workflow_error(
            "local action must be an unversioned repository action path",
        ));
    }
    NormalizedRelativePath::new(Path::new(path))
        .map_err(|_error| workflow_error("local action path is not normalized"))?;
    Ok(())
}

fn validate_writer_command_step(
    value: &Value,
    name: &str,
    expected_environment: &[(&str, &str)],
    expected_script: &str,
) -> Result<(), Report<WorkflowError>> {
    let step = value_mapping(value, "writer command step")?;
    require_exact_keys(step, &["name", "env", "run"], "writer command step")?;
    require_exact_string(step, "name", name)?;
    let environment = value_mapping(required(step, "env")?, "writer environment")?;
    let keys = expected_environment
        .iter()
        .map(|(key, _value)| *key)
        .collect::<Vec<_>>();
    require_exact_keys(environment, &keys, "writer environment")?;
    for (key, expected) in expected_environment {
        require_exact_string(environment, key, expected)?;
    }
    require_exact_string(step, "run", expected_script)
}

fn validate_capture_job(job: &Mapping) -> Result<(), Report<WorkflowError>> {
    require_exact_keys(
        job,
        &[
            "name",
            "if",
            "runs-on",
            "timeout-minutes",
            "permissions",
            "strategy",
            "steps",
        ],
        CAPTURE_JOB,
    )?;
    require_exact_string(job, "name", "Capture CLI help (${{ matrix.platform }})")?;
    require_exact_string(job, "if", "github.event_name == 'pull_request'")?;
    require_exact_string(job, "runs-on", "${{ matrix.os }}")?;
    let permissions = permissions(job)?;
    if permissions != [("contents".to_owned(), "read".to_owned())] {
        return Err(workflow_error(
            "capture permissions must be exactly contents: read",
        ));
    }
    if scalar_u64(required(job, "timeout-minutes")?) != Some(30) {
        return Err(workflow_error("capture timeout must be 30 minutes"));
    }
    validate_capture_matrix(required(job, "strategy")?)?;
    validate_capture_steps(required(job, "steps")?)
}

fn validate_capture_events(value: &Value) -> Result<(), Report<WorkflowError>> {
    if let Some(sequence) = value.as_sequence() {
        if sequence.len() == 1 && scalar_string(&sequence[0]) == Some("pull_request") {
            return Ok(());
        }
        return Err(workflow_error(
            "capture workflow event sequence must be exactly pull_request",
        ));
    }
    let events = value_mapping(value, "capture events")?;
    require_allowed_keys(events, &["pull_request", "push"], "capture events")?;
    if optional(events, "pull_request").is_none() {
        return Err(workflow_error("capture workflow requires pull_request"));
    }
    if !required(events, "pull_request")?.is_null() {
        return Err(workflow_error(
            "capture pull_request event must not declare filters",
        ));
    }
    if let Some(push) = optional(events, "push") {
        let push = value_mapping(push, "capture push event")?;
        require_exact_keys(push, &["branches"], "capture push event")?;
        let branches = required(push, "branches")?
            .as_sequence()
            .ok_or_else(|| workflow_error("capture push branches must be a sequence"))?;
        if branches.len() != 1 || scalar_string(&branches[0]) != Some("main") {
            return Err(workflow_error(
                "capture push branches must contain only main",
            ));
        }
    }
    Ok(())
}

fn validate_capture_matrix(value: &Value) -> Result<(), Report<WorkflowError>> {
    let strategy = value_mapping(value, "capture strategy")?;
    require_exact_keys(strategy, &["fail-fast", "matrix"], "capture strategy")?;
    if scalar_bool(required(strategy, "fail-fast")?) != Some(false) {
        return Err(workflow_error("capture strategy must use fail-fast=false"));
    }
    let matrix = value_mapping(required(strategy, "matrix")?, "capture matrix")?;
    require_exact_keys(matrix, &["include"], "capture matrix")?;
    let include = required(matrix, "include")?
        .as_sequence()
        .ok_or_else(|| workflow_error("capture matrix include must be a sequence"))?;
    if include.len() != 2 {
        return Err(workflow_error(
            "capture matrix must contain exactly Linux and macOS",
        ));
    }
    let expected = [("ubuntu-latest", "linux"), ("macos-latest", "macos")];
    for (entry, (operating_system, platform)) in include.iter().zip(expected) {
        let entry = value_mapping(entry, "capture matrix entry")?;
        require_exact_keys(entry, &["os", "platform"], "capture matrix entry")?;
        require_exact_string(entry, "os", operating_system)?;
        require_exact_string(entry, "platform", platform)?;
    }
    Ok(())
}

fn validate_capture_steps(value: &Value) -> Result<(), Report<WorkflowError>> {
    let steps = value
        .as_sequence()
        .ok_or_else(|| workflow_error("capture steps must be a sequence"))?;
    if steps.len() != 8 {
        return Err(workflow_error(
            "capture job must contain exactly eight steps",
        ));
    }
    validate_checkout_step(&steps[0], "${{ github.event.pull_request.head.sha }}")?;
    validate_run_step(
        &steps[1],
        Some("Assert exact pull-request head"),
        None,
        None,
        "test \"$(git rev-parse HEAD)\" = \"${{ github.event.pull_request.head.sha }}\"",
    )?;
    validate_run_step(
        &steps[2],
        Some("Read tool versions"),
        Some("tool-versions"),
        None,
        concat!(
            "echo \"rust=$(awk '$1 == \\\"rust\\\" { print $2 }' .tool-versions)\" >> \"$GITHUB_OUTPUT\"\n",
            "echo \"node=$(awk '$1 == \\\"nodejs\\\" { print $2 }' .tool-versions)\" >> \"$GITHUB_OUTPUT\"\n",
        ),
    )?;
    validate_action_step(
        &steps[3],
        SETUP_RUST_ACTION,
        &[
            (
                "toolchain",
                StringValue::Text("${{ steps.tool-versions.outputs.rust }}"),
            ),
            ("cache", StringValue::Boolean(false)),
        ],
    )?;
    validate_action_step(
        &steps[4],
        SETUP_NODE_ACTION,
        &[(
            "node-version",
            StringValue::Text("${{ steps.tool-versions.outputs.node }}"),
        )],
    )?;
    validate_run_step(
        &steps[5],
        Some("Build Trusted Server JavaScript"),
        None,
        Some("crates/trusted-server-js/lib"),
        "npm ci\nnpm run build\n",
    )?;
    validate_capture_command_step(&steps[6])?;
    validate_action_step(
        &steps[7],
        UPLOAD_ACTION,
        &[
            ("name", StringValue::Text("cli-help-${{ matrix.platform }}")),
            (
                "path",
                StringValue::Text("${{ runner.temp }}/cli-help-${{ matrix.platform }}.zip"),
            ),
            ("if-no-files-found", StringValue::Text("error")),
            ("retention-days", StringValue::Unsigned(7)),
            ("compression-level", StringValue::Unsigned(0)),
        ],
    )
}

#[derive(Clone, Copy)]
enum StringValue<'a> {
    Text(&'a str),
    Boolean(bool),
    Unsigned(u64),
}

fn validate_checkout_step(value: &Value, checkout_ref: &str) -> Result<(), Report<WorkflowError>> {
    validate_action_step(
        value,
        CHECKOUT_ACTION,
        &[
            ("ref", StringValue::Text(checkout_ref)),
            ("persist-credentials", StringValue::Boolean(false)),
            ("fetch-depth", StringValue::Unsigned(0)),
        ],
    )
}

fn validate_action_step(
    value: &Value,
    action: &str,
    inputs: &[(&str, StringValue<'_>)],
) -> Result<(), Report<WorkflowError>> {
    let step = value_mapping(value, "action step")?;
    require_exact_keys(step, &["uses", "with"], "action step")?;
    require_exact_string(step, "uses", action)?;
    let actual_inputs = value_mapping(required(step, "with")?, "action inputs")?;
    let input_names = inputs
        .iter()
        .map(|(name, _value)| *name)
        .collect::<Vec<_>>();
    require_exact_keys(actual_inputs, &input_names, "action inputs")?;
    for (name, expected) in inputs {
        let actual = required(actual_inputs, name)?;
        let matches = match expected {
            StringValue::Text(expected) => scalar_string(actual) == Some(*expected),
            StringValue::Boolean(expected) => scalar_bool(actual) == Some(*expected),
            StringValue::Unsigned(expected) => scalar_u64(actual) == Some(*expected),
        };
        if !matches {
            return Err(workflow_error(format!(
                "action input {name} has the wrong value"
            )));
        }
    }
    Ok(())
}

fn validate_run_step(
    value: &Value,
    name: Option<&str>,
    id: Option<&str>,
    working_directory: Option<&str>,
    run: &str,
) -> Result<(), Report<WorkflowError>> {
    let step = value_mapping(value, "run step")?;
    let mut keys = vec!["run"];
    if name.is_some() {
        keys.push("name");
    }
    if id.is_some() {
        keys.push("id");
    }
    if working_directory.is_some() {
        keys.push("working-directory");
    }
    require_exact_keys(step, &keys, "run step")?;
    require_exact_string(step, "run", run)?;
    if let Some(name) = name {
        require_exact_string(step, "name", name)?;
    }
    if let Some(id) = id {
        require_exact_string(step, "id", id)?;
    }
    if let Some(working_directory) = working_directory {
        require_exact_string(step, "working-directory", working_directory)?;
    }
    Ok(())
}

fn validate_capture_command_step(value: &Value) -> Result<(), Report<WorkflowError>> {
    let step = value_mapping(value, "capture command step")?;
    require_exact_keys(step, &["name", "env", "run"], "capture command step")?;
    require_exact_string(step, "name", "Capture recursive native CLI help")?;
    let environment = value_mapping(required(step, "env")?, "capture environment")?;
    require_exact_keys(environment, &["TSJS_SKIP_BUILD"], "capture environment")?;
    require_exact_string(environment, "TSJS_SKIP_BUILD", "1")?;
    require_exact_string(
        step,
        "run",
        "cargo run --manifest-path tools/docs-parity/Cargo.toml -- cli-help capture --output \"${{ runner.temp }}/cli-help-${{ matrix.platform }}.zip\"\n",
    )
}

fn permissions(job: &Mapping) -> Result<Vec<(String, String)>, Report<WorkflowError>> {
    let mapping = value_mapping(required(job, "permissions")?, "permissions")?;
    let mut result = string_entries(mapping, "permissions")?
        .into_iter()
        .map(|(name, value)| {
            let level = scalar_string(value)
                .ok_or_else(|| workflow_error("permission level must be a string"))?;
            if !matches!(level, "read" | "write" | "none") {
                return Err(workflow_error("permission level is not closed"));
            }
            Ok((name, level.to_owned()))
        })
        .collect::<Result<Vec<_>, Report<WorkflowError>>>()?;
    result.sort_unstable();
    Ok(result)
}

fn validate_default_permissions(value: &Value) -> Result<(), Report<WorkflowError>> {
    if !value_mapping(value, "workflow permissions")?.is_empty() {
        return Err(workflow_error(
            "workflow permissions must default deny with {}",
        ));
    }
    Ok(())
}

fn require_allowed_keys(
    mapping: &Mapping,
    allowed: &[&str],
    context: &str,
) -> Result<(), Report<WorkflowError>> {
    for key in mapping.keys() {
        let key = scalar_string(key)
            .ok_or_else(|| workflow_error(format!("{context} key is not a string")))?;
        if !allowed.contains(&key) {
            return Err(workflow_error(format!("unknown {context} field: {key}")));
        }
    }
    Ok(())
}

fn require_exact_keys(
    mapping: &Mapping,
    expected: &[&str],
    context: &str,
) -> Result<(), Report<WorkflowError>> {
    require_allowed_keys(mapping, expected, context)?;
    if mapping.len() != expected.len()
        || expected
            .iter()
            .any(|name| optional(mapping, name).is_none())
    {
        return Err(workflow_error(format!(
            "{context} must contain exactly the required fields"
        )));
    }
    Ok(())
}

fn require_exact_string(
    mapping: &Mapping,
    name: &str,
    expected: &str,
) -> Result<(), Report<WorkflowError>> {
    if scalar_string(required(mapping, name)?) != Some(expected) {
        return Err(workflow_error(format!("field {name} has the wrong value")));
    }
    Ok(())
}

fn required<'a>(mapping: &'a Mapping, name: &str) -> Result<&'a Value, Report<WorkflowError>> {
    optional(mapping, name).ok_or_else(|| workflow_error(format!("missing field: {name}")))
}

fn optional<'a>(mapping: &'a Mapping, name: &str) -> Option<&'a Value> {
    mapping.get(Value::String(name.to_owned()))
}

fn string_entries<'a>(
    mapping: &'a Mapping,
    context: &str,
) -> Result<Vec<(String, &'a Value)>, Report<WorkflowError>> {
    mapping
        .iter()
        .map(|(key, value)| {
            scalar_string(key)
                .map(|key| (key.to_owned(), value))
                .ok_or_else(|| workflow_error(format!("{context} key is not a string")))
        })
        .collect()
}

fn value_mapping<'a>(
    value: &'a Value,
    context: &str,
) -> Result<&'a Mapping, Report<WorkflowError>> {
    value
        .as_mapping()
        .ok_or_else(|| workflow_error(format!("{context} must be a mapping")))
}

fn scalar_string(value: &Value) -> Option<&str> {
    value.as_str()
}

fn scalar_bool(value: &Value) -> Option<bool> {
    value.as_bool()
}

fn scalar_u64(value: &Value) -> Option<u64> {
    value.as_u64()
}

fn workflow_error(detail: impl Into<String>) -> Report<WorkflowError> {
    Report::new(WorkflowError {
        detail: detail.into(),
    })
}
