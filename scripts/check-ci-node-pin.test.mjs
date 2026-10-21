import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { after, test } from "node:test";
import { fileURLToPath } from "node:url";

import { checkRepository, mapping, ParseError } from "./check-ci-node-pin.mjs";

const SCRIPT = fileURLToPath(
  new URL("./check-ci-node-pin.mjs", import.meta.url),
);
const REPOSITORY = path.dirname(path.dirname(SCRIPT));
// The last commit before the PR that added this guard: 8 of its jobs compile
// Rust before pinning Node.
const PRE_GUARD_COMMIT = "7a0ecb4cb";

const scratch = mkdtempSync(path.join(tmpdir(), "check-ci-node-pin-"));
after(() => rmSync(scratch, { recursive: true, force: true }));

let fixtureCount = 0;

/** Write `files` (relative path to text) into a new fixture repository. */
function fixture(files) {
  fixtureCount += 1;
  const root = path.join(scratch, `repo-${fixtureCount}`);
  for (const [relative, text] of Object.entries(files)) {
    const file = path.join(root, relative);
    mkdirSync(path.dirname(file), { recursive: true });
    writeFileSync(file, text);
  }
  return root;
}

/** Return a workflow with one `build` job whose steps are `steps`, indented as items. */
function workflow(steps) {
  const body = steps
    .join("\n")
    .split("\n")
    .map((line) => (line === "" ? line : `      ${line}`))
    .join("\n");
  return `name: CI\non:\n  pull_request:\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n${body}\n`;
}

/** Check a fixture repository holding one workflow with `steps`, plus `files`. */
function check(steps, files = {}) {
  const root = fixture({
    ".github/workflows/ci.yml": workflow(steps),
    ...files,
  });
  return checkRepository(root).violations;
}

const CHECKOUT = "- uses: actions/checkout@v4";
const PIN =
  "- uses: actions/setup-node@v4\n  with:\n    node-version-file: .tool-versions";
const BUILD = "- run: cargo build";

function assertClean(violations, message) {
  assert.deepEqual(violations, [], message);
}

function assertFlagged(violations, message) {
  assert.equal(violations.length, 1, message);
  assert.match(
    violations[0],
    /job `build` in \.github\/workflows\/ci\.yml compiles Rust/,
    message,
  );
}

// Ordering and conditions.

test("flags a compile before the pin", () => {
  assertFlagged(
    check([CHECKOUT, BUILD, PIN]),
    "should flag cargo build before setup-node",
  );
});

test("accepts a compile after the pin", () => {
  assertClean(
    check([CHECKOUT, PIN, BUILD]),
    "should accept cargo build after setup-node",
  );
});

test("flags a compile when no step pins Node", () => {
  assertFlagged(
    check([CHECKOUT, BUILD]),
    "should flag a job without setup-node",
  );
});

test("flags a compile with an if: before the pin", () => {
  assertFlagged(
    check([
      CHECKOUT,
      "- if: github.event_name == 'push'\n  run: cargo build",
      PIN,
    ]),
    "should count a conditional compile step",
  );
});

test("does not count a conditional pin", () => {
  assertFlagged(
    check([
      CHECKOUT,
      `- if: runner.os == 'Linux'\n  uses: actions/setup-node@v4\n  with:\n    node-version-file: .tool-versions`,
      BUILD,
    ]),
    "should not treat a conditional setup-node step as pinning Node",
  );
});

test("names the first compile step and reports each job once", () => {
  const violations = check([
    CHECKOUT,
    "- name: First build\n  run: cargo build",
    "- run: cargo test",
  ]);
  assert.equal(violations.length, 1, "should report the job once");
  assert.match(
    violations[0],
    /^\.github\/workflows\/ci\.yml:9: .*step `First build`/,
    "should point at the first compile step",
  );
});

test("skips jobs without steps", () => {
  const root = fixture({
    ".github/workflows/ci.yml":
      "on: push\njobs:\n  call:\n    uses: ./.github/workflows/other.yml\n",
  });
  assert.deepEqual(
    checkRepository(root),
    { workflowCount: 1, jobCount: 1, violations: [] },
    "should count but not check a reusable-workflow job",
  );
});

// What counts as a compile.

test("counts cargo aliases, toolchain overrides and line continuations", () => {
  for (const run of [
    "cargo clippy-fastly",
    "cargo test-axum",
    "cargo +nightly build",
    "cargo -Zunstable-options check",
    "|\n    cargo \\\n      build --release",
    "cargo install --path crates/trusted-server-cli",
    "worker-build --release",
  ]) {
    assertFlagged(
      check([CHECKOUT, `- run: ${run}`, PIN]),
      `should flag \`${run}\``,
    );
  }
});

test("ignores cargo commands that do not compile", () => {
  for (const run of [
    "cargo fmt --all -- --check",
    "cargo metadata --format-version 1",
    "cargo tree",
    "cargo install cargo-nextest",
    "cargo",
  ]) {
    assertClean(
      check([CHECKOUT, `- run: ${run}`, PIN]),
      `should not flag \`${run}\``,
    );
  }
});

test("ignores cargo in comments", () => {
  assertClean(
    check([CHECKOUT, "- run: |\n    # cargo build\n    echo hi", PIN]),
    "should skip comment lines in run blocks",
  );
});

test("follows repository shell scripts, including nested ones", () => {
  const files = {
    "scripts/build.sh": "#!/usr/bin/env bash\nscripts/inner.sh\n",
    "scripts/inner.sh": "#!/usr/bin/env bash\ncargo build\n",
    "scripts/quiet.sh": "#!/usr/bin/env bash\n# cargo build\necho hi\n",
  };
  assertFlagged(
    check([CHECKOUT, "- run: ./scripts/build.sh", PIN], files),
    "should flag a script that runs another compiling script",
  );
  assertClean(
    check([CHECKOUT, "- run: scripts/quiet.sh", PIN], files),
    "should ignore cargo in script comments",
  );
  assertClean(
    check([CHECKOUT, "- run: scripts/missing.sh", PIN], files),
    "should ignore a script that does not exist",
  );
});

test("stops at script cycles", () => {
  const files = {
    "scripts/a.sh": "scripts/b.sh\n",
    "scripts/b.sh": "scripts/a.sh\n",
  };
  assertClean(
    check([CHECKOUT, "- run: scripts/a.sh", PIN], files),
    "should terminate on mutually recursive scripts",
  );
});

// What counts as a pin.

test("accepts a ./ prefix, quotes and a trailing comment on node-version-file", () => {
  for (const value of [
    "./.tool-versions",
    "'.tool-versions'",
    '".tool-versions" # pinned',
    ".tool-versions # pinned",
    "'./.tool-versions' # pinned",
  ]) {
    const pin = `- uses: actions/setup-node@v4\n  with:\n    node-version-file: ${value}`;
    assertClean(
      check([CHECKOUT, pin, BUILD]),
      `should accept node-version-file: ${value}`,
    );
  }
});

test("accepts a node-version read from a step output", () => {
  const pin =
    "- uses: actions/setup-node@v4\n  with:\n    node-version: ${{ steps.node-version.outputs.node-version }}";
  assertClean(
    check([CHECKOUT, pin, BUILD]),
    "should accept a step-output node-version",
  );
});

test("does not count a literal node-version or another version file", () => {
  for (const options of [
    "node-version: 24.12.0",
    "node-version-file: .nvmrc",
    "node-version: 20\n    node-version-file: .tool-versions",
  ]) {
    const pin = `- uses: actions/setup-node@v4\n  with:\n    ${options}`;
    assertFlagged(
      check([CHECKOUT, pin, BUILD]),
      `should not treat \`${options}\` as pinned`,
    );
  }
});

// Later setup-node steps (inline comment 4308405296).

test("resets the pin when a later step installs another version", () => {
  const other = "- uses: actions/setup-node@v4\n  with:\n    node-version: 20";
  assertFlagged(
    check([CHECKOUT, PIN, other, BUILD]),
    "should flag a later version change",
  );
  assertFlagged(
    check([
      CHECKOUT,
      PIN,
      `- if: runner.os == 'macOS'\n  uses: actions/setup-node@v4\n  with:\n    node-version: 20`,
      BUILD,
    ]),
    "should flag a later conditional version change",
  );
});

test("keeps the pin across a conditional re-pin of the same file", () => {
  const repin = `- if: runner.os == 'macOS'\n  uses: actions/setup-node@v4\n  with:\n    node-version-file: .tool-versions`;
  assertClean(
    check([CHECKOUT, PIN, repin, BUILD]),
    "should keep the pin when a conditional step re-pins it",
  );
});

test("keeps the pin across setup-node steps that name no version", () => {
  assertClean(
    check([CHECKOUT, PIN, "- uses: actions/setup-node@v4", BUILD]),
    "should ignore a bare setup-node step",
  );
  assertClean(
    check([
      CHECKOUT,
      PIN,
      "- uses: actions/setup-node@v4\n  with:\n    cache: npm\n    cache-dependency-path: package-lock.json",
      BUILD,
    ]),
    "should ignore a cache-only setup-node step",
  );
});

test("a versionless setup-node step does not pin on its own", () => {
  assertFlagged(
    check([
      CHECKOUT,
      "- uses: actions/setup-node@v4\n  with:\n    cache: npm",
      BUILD,
    ]),
    "should not treat a cache-only step as a pin",
  );
});

// Local composite actions.

const COMPOSITE_PIN =
  "name: Pin\nruns:\n  using: composite\n  steps:\n    - uses: actions/setup-node@v4\n      with:\n        node-version-file: .tool-versions\n";
const COMPOSITE_BUILD =
  "name: Build\nruns:\n  using: composite\n  steps:\n    - name: Build in action\n      shell: bash\n      run: cargo build\n";

test("inlines a composite action that pins Node", () => {
  assertClean(
    check([CHECKOUT, "- uses: ./.github/actions/pin", BUILD], {
      ".github/actions/pin/action.yml": COMPOSITE_PIN,
    }),
    "should see the pin inside the action",
  );
});

test("inlines a composite action that compiles and reports the action's line", () => {
  const violations = check([CHECKOUT, "- uses: ./.github/actions/build", PIN], {
    ".github/actions/build/action.yaml": COMPOSITE_BUILD,
  });
  assertFlagged(violations, "should flag a compile inside the action");
  assert.match(
    violations[0],
    /^\.github\/actions\/build\/action\.yaml:5: .*step `Build in action`/,
    "should point at the action file",
  );
});

test("does not count a pin inside a conditional composite action", () => {
  assertFlagged(
    check(
      [
        CHECKOUT,
        "- if: runner.os == 'Linux'\n  uses: ./.github/actions/pin",
        BUILD,
      ],
      { ".github/actions/pin/action.yml": COMPOSITE_PIN },
    ),
    "should carry the caller's if: into the action's steps",
  );
});

test("fails on a missing or self-including local action", () => {
  assert.throws(
    () => check([CHECKOUT, "- uses: ./.github/actions/missing"]),
    (error) =>
      error instanceof ParseError &&
      /no action\.yml for `\.\/\.github\/actions\/missing`/.test(error.message),
    "should raise ParseError for a missing action",
  );
  const loop =
    "name: Loop\nruns:\n  using: composite\n  steps:\n    - uses: ./.github/actions/loop\n";
  assert.throws(
    () =>
      check([CHECKOUT, "- uses: ./.github/actions/loop"], {
        ".github/actions/loop/action.yml": loop,
      }),
    (error) =>
      error instanceof ParseError && /includes itself/.test(error.message),
    "should raise ParseError for a recursive action",
  );
});

test("fails on a workflow without jobs", () => {
  const root = fixture({ ".github/workflows/ci.yml": "on: push\n" });
  assert.throws(
    () => checkRepository(root),
    ParseError,
    "should raise ParseError without a jobs key",
  );
});

// Quoted scalars and comments (inline comment 4308405292).

test("resolves a quoted local action followed by a comment", () => {
  assertClean(
    check([CHECKOUT, '- uses: "./.github/actions/pin" # shared', BUILD], {
      ".github/actions/pin/action.yml": COMPOSITE_PIN,
    }),
    "should strip the comment after a quoted uses:",
  );
});

test("keeps YAML escapes inside quoted run values", () => {
  assertFlagged(
    check([CHECKOUT, "- run: 'echo ''hi'' && cargo build' # note", PIN]),
    "should keep text after a '' escape",
  );
  assertFlagged(
    check([CHECKOUT, '- run: "echo \\"hi\\" && cargo build" # note', PIN]),
    'should keep text after a \\" escape',
  );
});

test("mapping reads quoted, commented and unterminated values", () => {
  const lines = [
    "plain: value # comment",
    "single: 'it''s # kept' # dropped",
    'double: "say \\"hi\\" # kept" # dropped',
    "hash: a#b",
    'open: "cargo build # kept',
    '  still open"',
    "empty:",
  ];
  const values = Object.fromEntries(
    Array.from(mapping(lines, 0, lines.length), ([key, entry]) => [
      key,
      entry.value,
    ]),
  );
  assert.deepEqual(
    values,
    {
      plain: "value",
      single: "'it''s # kept'",
      double: '"say \\"hi\\" # kept"',
      hash: "a#b",
      open: '"cargo build # kept',
      empty: "",
    },
    "should keep quoted scalars whole, drop trailing comments and leave unterminated quotes as is",
  );
});

// Command line and real trees.

test("the command line exits 0 on a clean tree and 1 with violations", () => {
  const clean = fixture({
    ".github/workflows/ci.yml": workflow([CHECKOUT, PIN, BUILD]),
  });
  const dirty = fixture({
    ".github/workflows/ci.yml": workflow([CHECKOUT, BUILD]),
  });
  const empty = fixture({ "README.md": "empty\n" });

  const ok = spawnSync(process.execPath, [SCRIPT, "--root", clean], {
    encoding: "utf8",
  });
  assert.equal(ok.status, 0, "should exit 0 on a clean tree");
  assert.equal(
    ok.stdout,
    "ok: 1 jobs in 1 workflows pin Node before compiling Rust\n",
    "should report the counts",
  );

  const bad = spawnSync(process.execPath, [SCRIPT, "--root", dirty], {
    encoding: "utf8",
  });
  assert.equal(bad.status, 1, "should exit 1 with violations");
  assert.match(
    bad.stderr,
    /1 job\(s\) compile Rust before pinning Node\n$/,
    "should summarize the violations",
  );

  const none = spawnSync(process.execPath, [SCRIPT, "--root", empty], {
    encoding: "utf8",
  });
  assert.equal(none.status, 1, "should exit 1 without workflows");
  assert.match(
    none.stderr,
    /^no workflows found under /,
    "should explain the failure",
  );
});

test("the current tree pins Node in every job", () => {
  const result = checkRepository(REPOSITORY);
  assert.deepEqual(
    result.violations,
    [],
    "should report no violations on this checkout",
  );
  assert.ok(result.jobCount > 0, "should check at least one job");
});

function hasCommit(commit) {
  return (
    spawnSync("git", ["-C", REPOSITORY, "cat-file", "-e", `${commit}^{commit}`])
      .status === 0
  );
}

test("the pre-guard tree has 8 jobs that compile before pinning Node", (context) => {
  if (!hasCommit(PRE_GUARD_COMMIT)) {
    // CI fetches full history for this job, so only skip in a local shallow clone.
    assert.ok(
      !process.env.CI,
      `should have ${PRE_GUARD_COMMIT} in CI; check out with fetch-depth: 0`,
    );
    context.skip(`${PRE_GUARD_COMMIT} is not in this clone`);
    return;
  }
  const root = path.join(scratch, "pre-guard");
  mkdirSync(root);
  const archive = execFileSync(
    "git",
    ["-C", REPOSITORY, "archive", PRE_GUARD_COMMIT, ".github", "scripts"],
    { maxBuffer: 64 * 1024 * 1024 },
  );
  execFileSync("tar", ["-x", "-C", root], { input: archive });

  const jobs = checkRepository(root).violations.map((violation) =>
    violation
      .match(/job `([\w-]+)` in (\S+)/)
      .slice(1)
      .join(" "),
  );
  assert.deepEqual(
    jobs,
    [
      "format-rust .github/workflows/format.yml",
      "prepare-artifacts .github/workflows/integration-tests.yml",
      "integration-tests .github/workflows/integration-tests.yml",
      "integration-tests-fastly-ec .github/workflows/integration-tests.yml",
      "browser-tests .github/workflows/integration-tests.yml",
      "test-cloudflare .github/workflows/test.yml",
      "test-spin .github/workflows/test.yml",
      "test-parity .github/workflows/test.yml",
    ],
    "should flag the 8 jobs that compiled before pinning Node",
  );
});
