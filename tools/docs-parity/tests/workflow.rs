use docs_parity::workflow::{WorkflowScope, validate_workflow};

const CHECKOUT_SHA: &str = "3d3c42e5aac5ba805825da76410c181273ba90b1";
const SETUP_RUST_SHA: &str = "166cdcfd11aee3cb47222f9ddb555ce30ddb9659";
const SETUP_NODE_SHA: &str = "820762786026740c76f36085b0efc47a31fe5020";
const UPLOAD_SHA: &str = "043fb46d1a93c77aae656e7c1c64a875d1fc6a0a";
const DOWNLOAD_SHA: &str = "3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c";

fn final_fixture() -> String {
    r#"
name: Documentation automation
permissions: {}
on:
  pull_request:
  schedule:
    - cron: "17 9 * * 1"
  workflow_dispatch:
concurrency:
  group: documentation-automation
  cancel-in-progress: false
jobs:
  pull-request:
    name: Documentation parity
    if: github.event_name == 'pull_request'
    runs-on: ubuntu-latest
    timeout-minutes: 30
    permissions:
      contents: read
    steps:
      - uses: actions/checkout@__CHECKOUT_SHA__
        with:
          ref: "${{ github.event.pull_request.head.sha }}"
          persist-credentials: false
          fetch-depth: 0
      - name: Read pinned Node version
        id: node-version
        run: echo "node=$(awk '$1 == \"nodejs\" { print $2 }' .tool-versions)" >> "$GITHUB_OUTPUT"
      - uses: actions-rust-lang/setup-rust-toolchain@__SETUP_RUST_SHA__
        with:
          toolchain: "1.95.0"
          cache: false
      - uses: actions/setup-node@__SETUP_NODE_SHA__
        with:
          node-version: "${{ steps.node-version.outputs.node }}"
      - run: cargo run --manifest-path tools/docs-parity/Cargo.toml -- check --all
  link-reader:
    name: Check external documentation links
    if: github.repository == 'IABTechLab/trusted-server' && github.ref == 'refs/heads/main' && (github.event_name == 'schedule' || github.event_name == 'workflow_dispatch')
    runs-on: ubuntu-latest
    timeout-minutes: 30
    permissions:
      contents: read
    outputs:
      artifact-sha256: "${{ steps.digest.outputs.sha256 }}"
    steps:
      - uses: actions/checkout@__CHECKOUT_SHA__
        with:
          ref: "${{ github.sha }}"
          persist-credentials: false
          fetch-depth: 0
      - uses: actions-rust-lang/setup-rust-toolchain@__SETUP_RUST_SHA__
        with:
          toolchain: "1.95.0"
          cache: false
      - name: Generate closed link result
        run: |
          checked_at="$(date -u +'%Y-%m-%dT%H:%M:%SZ')"
          DOCS_PARITY_CHECKED_AT="$checked_at" cargo run --manifest-path tools/docs-parity/Cargo.toml -- links --external --artifact "$RUNNER_TEMP/link-results.zip"
      - name: Record link artifact digest
        id: digest
        run: echo "sha256=$(sha256sum "$RUNNER_TEMP/link-results.zip" | cut -d ' ' -f 1)" >> "$GITHUB_OUTPUT"
      - uses: actions/upload-artifact@__UPLOAD_SHA__
        with:
          name: link-results
          path: "${{ runner.temp }}/link-results.zip"
          if-no-files-found: error
          retention-days: 7
          compression-level: 0
  issue-writer:
    name: Reconcile external-link issue
    if: github.repository == 'IABTechLab/trusted-server' && github.ref == 'refs/heads/main' && (github.event_name == 'schedule' || github.event_name == 'workflow_dispatch')
    needs: link-reader
    runs-on: ubuntu-latest
    timeout-minutes: 5
    permissions:
      issues: write
    steps:
      - uses: actions/download-artifact@__DOWNLOAD_SHA__
        with:
          name: link-results
          path: "${{ runner.temp }}/link-results"
      - name: Validate LinkResultsV1 and reconcile one owned issue
        env:
          EXPECTED_SHA256: "${{ needs.link-reader.outputs.artifact-sha256 }}"
          EXPECTED_REPOSITORY: "${{ github.repository }}"
          EXPECTED_REF: "${{ github.ref }}"
          EXPECTED_SOURCE_SHA: "${{ github.sha }}"
          EXPECTED_RUN_ID: "${{ github.run_id }}"
          EXPECTED_RUN_ATTEMPT: "${{ github.run_attempt }}"
          GH_TOKEN: "${{ github.token }}"
        run: |
          set -euo pipefail
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
  dependency-reader:
    name: Generate dependency snapshot
    if: github.repository == 'IABTechLab/trusted-server' && github.ref == 'refs/heads/main' && (github.event_name == 'schedule' || github.event_name == 'workflow_dispatch')
    runs-on: ubuntu-latest
    timeout-minutes: 20
    permissions:
      contents: read
    outputs:
      artifact-sha256: "${{ steps.digest.outputs.sha256 }}"
    steps:
      - uses: actions/checkout@__CHECKOUT_SHA__
        with:
          ref: "${{ github.sha }}"
          persist-credentials: false
          fetch-depth: 0
      - uses: actions-rust-lang/setup-rust-toolchain@__SETUP_RUST_SHA__
        with:
          toolchain: "1.95.0"
          cache: false
      - name: Generate closed dependency snapshot
        run: cargo run --manifest-path tools/docs-parity/Cargo.toml -- dependency-snapshot generate --output "$RUNNER_TEMP/dependency-snapshot.zip"
      - name: Record dependency artifact digest
        id: digest
        run: echo "sha256=$(sha256sum "$RUNNER_TEMP/dependency-snapshot.zip" | cut -d ' ' -f 1)" >> "$GITHUB_OUTPUT"
      - uses: actions/upload-artifact@__UPLOAD_SHA__
        with:
          name: dependency-snapshot
          path: "${{ runner.temp }}/dependency-snapshot.zip"
          if-no-files-found: error
          retention-days: 7
          compression-level: 0
  dependency-writer:
    name: Submit dependency snapshot
    if: github.repository == 'IABTechLab/trusted-server' && github.ref == 'refs/heads/main' && (github.event_name == 'schedule' || github.event_name == 'workflow_dispatch')
    needs: dependency-reader
    runs-on: ubuntu-latest
    timeout-minutes: 5
    permissions:
      contents: write
    steps:
      - uses: actions/download-artifact@__DOWNLOAD_SHA__
        with:
          name: dependency-snapshot
          path: "${{ runner.temp }}/dependency-snapshot"
      - name: Validate and submit unchanged version-0 snapshot
        env:
          EXPECTED_SHA256: "${{ needs.dependency-reader.outputs.artifact-sha256 }}"
          EXPECTED_REF: "${{ github.ref }}"
          EXPECTED_SOURCE_SHA: "${{ github.sha }}"
          EXPECTED_RUN_ID: "${{ github.run_id }}"
          EXPECTED_RUN_ATTEMPT: "${{ github.run_attempt }}"
          GH_TOKEN: "${{ github.token }}"
        run: |
          set -euo pipefail
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
"#
    .replace("__CHECKOUT_SHA__", CHECKOUT_SHA)
    .replace("__SETUP_RUST_SHA__", SETUP_RUST_SHA)
    .replace("__SETUP_NODE_SHA__", SETUP_NODE_SHA)
    .replace("__UPLOAD_SHA__", UPLOAD_SHA)
    .replace("__DOWNLOAD_SHA__", DOWNLOAD_SHA)
}

#[test]
fn final_policy_accepts_read_only_reader_and_no_checkout_writer() {
    validate_workflow(final_fixture().as_bytes(), WorkflowScope::Final)
        .expect("closed final workflow fixture should pass");
}

#[test]
fn workflow_policy_rejects_untrusted_events_actions_permissions_and_writers() {
    let fixture = final_fixture();
    let safe_local_action = fixture.replacen(
        "      - name: Read pinned Node version",
        "      - uses: ./.github/actions/setup-integration-test-env\n      - name: Read pinned Node version",
        1,
    );
    validate_workflow(safe_local_action.as_bytes(), WorkflowScope::Final)
        .expect("a normalized local action should be permitted in the read-only PR job");
    let node_step = format!(
        "      - uses: actions/setup-node@{SETUP_NODE_SHA}\n        with:\n          node-version: \"${{{{ steps.node-version.outputs.node }}}}\"\n"
    );
    let missing_node = fixture.replacen(&node_step, "", 1);

    for (case, invalid) in [
        fixture.replace("pull_request:", "pull_request_target:"),
        fixture.replace("  schedule:\n", "  merge_group:\n  schedule:\n"),
        fixture.replace("  workflow_dispatch:\n", "  workflow_dispatch:\n  push:\n"),
        fixture.replace(
            "  workflow_dispatch:\n",
            "  workflow_dispatch:\n    inputs:\n      sha:\n        required: true\n",
        ),
        fixture.replacen(
            &format!("actions/checkout@{CHECKOUT_SHA}"),
            "actions/checkout@v4",
            1,
        ),
        fixture.replacen(
            &format!("actions-rust-lang/setup-rust-toolchain@{SETUP_RUST_SHA}"),
            "example/unknown@0123456789abcdef0123456789abcdef01234567",
            1,
        ),
        fixture.replacen(
            &format!("actions/setup-node@{SETUP_NODE_SHA}"),
            "actions/setup-node@0123456789abcdef0123456789abcdef01234567",
            1,
        ),
        missing_node,
        fixture.replacen(
            "node-version: \"${{ steps.node-version.outputs.node }}\"",
            "node-version: \"20\"",
            1,
        ),
        fixture.replacen("contents: read", "statuses: write", 1),
        fixture.replacen(
            "if: github.repository == 'IABTechLab/trusted-server'",
            "if: always() && github.repository == 'IABTechLab/trusted-server'",
            1,
        ),
        fixture.replacen(
            "ref: \"${{ github.sha }}\"",
            "ref: \"${{ github.event.pull_request.head.sha }}\"",
            1,
        ),
        fixture.replacen(
            "          name: link-results\n          path:",
            "          name: link-results\n          run-id: \"${{ github.run_id }}\"\n          path:",
            1,
        ),
        fixture.replacen(
            "    steps:\n      - uses: actions/download-artifact@",
            &format!(
                "    steps:\n      - uses: actions/checkout@{CHECKOUT_SHA}\n        with:\n          ref: \"${{{{ github.sha }}}}\"\n          persist-credentials: false\n          fetch-depth: 0\n      - uses: actions/download-artifact@"
            ),
            1,
        ),
        safe_local_action.replace(
            "./.github/actions/setup-integration-test-env",
            "./.github/actions/../outside",
        ),
        fixture.replacen(
            "      - run: cargo run --manifest-path tools/docs-parity/Cargo.toml -- check --all",
            "      - uses: actions/cache@0123456789abcdef0123456789abcdef01234567\n      - run: cargo run --manifest-path tools/docs-parity/Cargo.toml -- check --all",
            1,
        ),
        fixture.replacen(
            "    timeout-minutes: 30\n    permissions:",
            "    services:\n      database:\n        image: example.invalid/db\n    timeout-minutes: 30\n    permissions:",
            1,
        ),
        fixture.replacen("link-results.json", "unexpected.json", 1),
        fixture.replacen("EXPECTED_SOURCE_SHA: \"${{ github.sha }}\"", "EXPECTED_SOURCE_SHA: stale", 1),
        fixture.replacen(".schema_version == 1", ".schema_version == 2", 1),
        fixture.replacen("assert finding[\"diagnostic\"].strip()", "assert True", 1),
        fixture.replacen("parsed.scheme == \"https\"", "parsed.scheme in (\"http\", \"https\")", 1),
        fixture.replacen("-le 2097152", "-le 2097153", 1),
        fixture.replacen("test ! -L \"$archive\"", "test -L \"$archive\"", 1),
        fixture.replacen("-rw-r--r--", "-rwxr-xr-x", 1),
        fixture.replacen("gh issue create", "true # gh issue create", 1),
        fixture.replacen("gh issue close", "true # gh issue close", 1),
        fixture.replacen("<!-- docs-parity:external-links:v1 -->", "<!-- unowned -->", 1),
        fixture.replacen("gh api --paginate --slurp", "gh api", 1),
        fixture.replacen(
            " > \"$issues_json\"",
            " || true > \"$issues_json\"",
            1,
        ),
        fixture.replacen("dependency-snapshot.json", "extra.json", 1),
        fixture.replacen(".version == 0", ".version == 1", 1),
        fixture.replacen("--input \"$json\"", "--input \"$archive\"", 1),
        fixture.replacen(
            "status=\"$(gh api",
            "cargo run --manifest-path tools/docs-parity/Cargo.toml -- submit\n          status=\"$(gh api",
            1,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        assert_ne!(invalid, fixture, "negative fixture case {case} must mutate input");
        assert!(
            validate_workflow(invalid.as_bytes(), WorkflowScope::Final).is_err(),
            "unsafe workflow fixture case {case} should fail"
        );
    }
}

#[test]
fn capture_scope_requires_exact_read_only_head_checkout_and_artifact_shape() {
    let fixture = format!(
        r#"
name: Capture
permissions: {{}}
on: [pull_request]
jobs:
  cli-help-capture:
    name: Capture CLI help (${{{{ matrix.platform }}}})
    if: github.event_name == 'pull_request'
    runs-on: "${{{{ matrix.os }}}}"
    timeout-minutes: 30
    permissions:
      contents: read
    strategy:
      fail-fast: false
      matrix:
        include:
          - os: ubuntu-latest
            platform: linux
          - os: macos-latest
            platform: macos
    steps:
      - uses: actions/checkout@{CHECKOUT_SHA}
        with:
          ref: "${{{{ github.event.pull_request.head.sha }}}}"
          persist-credentials: false
          fetch-depth: 0
      - name: Assert exact pull-request head
        run: test "$(git rev-parse HEAD)" = "${{{{ github.event.pull_request.head.sha }}}}"
      - name: Read tool versions
        id: tool-versions
        run: |
          echo "rust=$(awk '$1 == \"rust\" {{ print $2 }}' .tool-versions)" >> "$GITHUB_OUTPUT"
          echo "node=$(awk '$1 == \"nodejs\" {{ print $2 }}' .tool-versions)" >> "$GITHUB_OUTPUT"
      - uses: actions-rust-lang/setup-rust-toolchain@{SETUP_RUST_SHA}
        with:
          toolchain: "${{{{ steps.tool-versions.outputs.rust }}}}"
          cache: false
      - uses: actions/setup-node@{SETUP_NODE_SHA}
        with:
          node-version: "${{{{ steps.tool-versions.outputs.node }}}}"
      - name: Build Trusted Server JavaScript
        working-directory: crates/trusted-server-js/lib
        run: |
          npm ci
          npm run build
      - name: Capture recursive native CLI help
        env:
          TSJS_SKIP_BUILD: "1"
        run: |
          cargo run --manifest-path tools/docs-parity/Cargo.toml -- cli-help capture --output "${{{{ runner.temp }}}}/cli-help-${{{{ matrix.platform }}}}.zip"
      - uses: actions/upload-artifact@{UPLOAD_SHA}
        with:
          name: cli-help-${{{{ matrix.platform }}}}
          path: "${{{{ runner.temp }}}}/cli-help-${{{{ matrix.platform }}}}.zip"
          if-no-files-found: error
          retention-days: 7
          compression-level: 0
"#
    );
    validate_workflow(fixture.as_bytes(), WorkflowScope::CaptureJob)
        .expect("capture job should satisfy the narrow policy");

    for (case, invalid) in [
        fixture.replace("persist-credentials: false", "persist-credentials: true"),
        fixture.replace("fetch-depth: 0", "fetch-depth: 1"),
        fixture.replace("cli-help-${{ matrix.platform }}.zip", "*.zip"),
        fixture.replace("compression-level: 0", "compression-level: 6"),
    ]
    .into_iter()
    .enumerate()
    {
        assert!(
            validate_workflow(invalid.as_bytes(), WorkflowScope::CaptureJob).is_err(),
            "capture policy should reject drift case {case}"
        );
    }

    let decoy_checkout = fixture
        .replace("persist-credentials: false", "persist-credentials: true")
        .replace(
            "      - run: cargo run --manifest-path",
            "      - name: persist-credentials: false\n        run: cargo run --manifest-path",
        );
    let duplicate_checkout = fixture.replace(
        "      - name: Assert exact pull-request head",
        &format!(
            "      - uses: actions/checkout@{CHECKOUT_SHA}\n        with:\n          ref: \"${{{{ github.event.pull_request.head.sha }}}}\"\n          persist-credentials: false\n          fetch-depth: 0\n      - name: Assert exact pull-request head"
        ),
    );
    let extra_event = fixture.replace(
        "on: [pull_request]",
        "on: [pull_request, workflow_dispatch]",
    );
    let arbitrary_action = fixture.replace(
        "      - name: Assert exact pull-request head",
        "      - uses: example/unknown@0123456789abcdef0123456789abcdef01234567\n      - name: Assert exact pull-request head",
    );
    let traversing_local_action = fixture.replace(
        "      - name: Assert exact pull-request head",
        "      - uses: ./../outside\n      - name: Assert exact pull-request head",
    );
    for (case, invalid) in [
        decoy_checkout,
        duplicate_checkout,
        extra_event,
        arbitrary_action,
        traversing_local_action,
    ]
    .into_iter()
    .enumerate()
    {
        assert!(
            validate_workflow(invalid.as_bytes(), WorkflowScope::CaptureJob).is_err(),
            "capture policy must reject structural or trust-boundary drift case {case}"
        );
    }
}
