import assert from "node:assert/strict";
import {
  mkdtempSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { after, test } from "node:test";
import { compact, normalize } from "./normalize.mjs";
import { compile, source, withAddedFunction } from "./test-fixture.mjs";
import {
  API_DIRECTORY,
  BASELINE_COMMIT,
  CRATE,
  PROFILES,
  baselineFilePins,
  captureProfile,
  checkProfile,
  environmentPins,
  loadManifest,
  main,
  parseOptions,
  rustdocArgs,
  sha256,
  validateProvenance,
} from "./check.mjs";

const scratch = mkdtempSync(path.join(tmpdir(), "integration-api-check-"));
after(() => rmSync(scratch, { recursive: true, force: true }));
const fixtures = new Map(
  Object.entries(PROFILES).map(([label, profile]) => [
    label,
    compile(
      source + '\n#[cfg(feature = "test-utils")] pub fn feature_only() {}\n',
      {
        privateItems: false,
        target: profile.target,
        cfg: profile.features.length ? 'feature="test-utils"' : undefined,
      },
    ),
  ]),
);
let count = 0;
function fixture() {
  const repo = path.join(scratch, `repo-${count++}`);
  const baselineDirectory = path.join(repo, API_DIRECTORY);
  mkdirSync(baselineDirectory, { recursive: true });
  mkdirSync(path.join(repo, "scripts/integration-api"), { recursive: true });
  const historical = {
    "Cargo.lock": "baseline lock\n",
    "rust-toolchain.toml": '[toolchain]\nchannel = "1.98.1"\n',
    ".tool-versions": `nodejs ${process.versions.node}\n`,
  };
  for (const [file, value] of Object.entries(historical))
    writeFileSync(path.join(repo, file), value);
  const calls = [];
  const overrides = new Map();
  const targetDirectory = path.join(repo, "target");
  function run(program, args, options) {
    calls.push({ program, args, options });
    if (program === "rustc" || program === "rustdoc")
      return `${program} 1.98.1\nrelease: 1.98.1\n`;
    if (program === "git") {
      assert.equal(args[0], "show");
      const [commit, file] = args[1].split(":");
      assert.equal(commit, BASELINE_COMMIT);
      return historical[file];
    }
    if (program === "cargo" && args[0] === "metadata")
      return JSON.stringify({ target_directory: targetDirectory });
    if (program === "cargo" && args[0] === "rustdoc") {
      assert.equal(options.env.RUSTC_BOOTSTRAP, CRATE);
      const label = Object.keys(PROFILES).find(
        (label) => JSON.stringify(rustdocArgs(label)) === JSON.stringify(args),
      );
      assert(label, "only exact canonical rustdoc commands may run");
      const directory = path.join(
        targetDirectory,
        PROFILES[label].target,
        "doc",
      );
      mkdirSync(directory, { recursive: true });
      writeFileSync(
        path.join(directory, `${CRATE}.json`),
        JSON.stringify(overrides.get(label) ?? fixtures.get(label)),
      );
      return "";
    }
    throw Error(
      `unexpected project command in isolated test: ${program} ${args}`,
    );
  }
  const provenance = {
    schema: 1,
    baselineCommit: BASELINE_COMMIT,
    crate: CRATE,
    rustdocFormat: 60,
    environment: environmentPins(repo, run),
    profiles: Object.fromEntries(
      Object.entries(PROFILES).map(([label, profile]) => {
        const doc = fixtures.get(label);
        const bytes = Buffer.from(
          JSON.stringify(compact(normalize(doc, label))) + "\n",
        );
        writeFileSync(path.join(baselineDirectory, profile.snapshot), bytes);
        return [
          label,
          {
            ...profile,
            rustdocArgs: rustdocArgs(label),
            bootstrap: CRATE,
            rawSha256: sha256(JSON.stringify(doc)),
            snapshotSha256: sha256(bytes),
          },
        ];
      }),
    ),
  };
  writeFileSync(
    path.join(baselineDirectory, "provenance.json"),
    JSON.stringify(provenance),
  );
  const manifest = path.join(
    repo,
    "scripts/integration-api/delta-manifest.json",
  );
  writeFileSync(
    manifest,
    JSON.stringify({ schema: 1, baselineCommit: BASELINE_COMMIT, changes: [] }),
  );
  return {
    repo,
    baselineDirectory,
    provenance,
    historical,
    run,
    calls,
    overrides,
    manifest,
  };
}
function capturedMain(t, args, f) {
  const output = [];
  t.mock.method(console, "log", (line) => output.push(line));
  const result = main(args, f.repo, f.run);
  return { result, output };
}

test("six exact production-default and test-utils profiles select distinct targets and features", () => {
  assert.equal(Object.keys(PROFILES).length, 6);
  for (const [label, profile] of Object.entries(PROFILES)) {
    const args = rustdocArgs(label);
    assert.equal(args.includes("--no-default-features"), false);
    assert.equal(args.includes("--all-features"), false);
    assert.equal(args[args.indexOf("--target") + 1], profile.target);
    assert.equal(args.includes("--features"), label.endsWith("/test-utils"));
    if (profile.features.length)
      assert.equal(args[args.indexOf("--features") + 1], "test-utils");
    const captured = captureProfile(
      label,
      Buffer.from(JSON.stringify(fixtures.get(label))),
    );
    assert.equal(
      captured.provenance.snapshotSha256,
      sha256(captured.snapshotBytes),
    );
    assert.equal(JSON.parse(captured.snapshotBytes).profile, label);
    assert.deepEqual(captured.provenance.rustdocArgs, args);
  }
  assert.throws(() => rustdocArgs("native/unknown"), /unknown API profile/);
});

test("options reject ambiguous, missing and unknown selections", () => {
  assert.deepEqual(parseOptions(["native/default", "--changes"]), {
    profiles: ["native/default"],
    changesOnly: true,
  });
  assert.equal(parseOptions(["--all"]).profiles.length, 6);
  for (const args of [
    [],
    ["native/nope"],
    ["--all", "native/default"],
    ["--all", "--all"],
    ["--approve"],
    ["--all", "--changes", "--changes"],
  ])
    assert.throws(() => parseOptions(args), /usage:/);
});

test("all six fresh profile checks run serial canonical rustdoc and do not modify frozen files", (t) => {
  const f = fixture();
  const hashes = Object.values(PROFILES).map((profile) =>
    sha256(readFileSync(path.join(f.baselineDirectory, profile.snapshot))),
  );
  const { result } = capturedMain(t, ["--all"], f);
  assert.equal(result.length, 6);
  assert(result.every((profile) => profile.changes.length === 0));
  assert.deepEqual(
    f.calls
      .filter((call) => call.program === "cargo" && call.args[0] === "rustdoc")
      .map((call) => call.args),
    Object.keys(PROFILES).map(rustdocArgs),
  );
  assert.deepEqual(
    Object.values(PROFILES).map((profile) =>
      sha256(readFileSync(path.join(f.baselineDirectory, profile.snapshot))),
    ),
    hashes,
  );
});

test("baseline inputs use immutable git blobs; a changed current lock is reported, not silently repinned", (t) => {
  const f = fixture();
  const original = readFileSync(
    path.join(f.baselineDirectory, "provenance.json"),
  );
  writeFileSync(path.join(f.repo, "Cargo.lock"), "reviewed candidate lock\n");
  assert.deepEqual(
    baselineFilePins(f.repo, f.run),
    f.provenance.environment.files,
  );
  const { output } = capturedMain(t, ["native/default"], f);
  assert(
    output.some((line) => line.includes(sha256("reviewed candidate lock\n"))),
  );
  assert.deepEqual(
    readFileSync(path.join(f.baselineDirectory, "provenance.json")),
    original,
  );
});

test("bad origin, schema, compiler, flags, profile commands and historical pins fail", () => {
  const f = fixture();
  const current = environmentPins(f.repo, f.run);
  const mutations = [
    (p) => {
      p.baselineCommit = "wrong";
    },
    (p) => {
      p.rustdocFormat = 61;
    },
    (p) => {
      p.environment.node = "v0";
    },
    (p) => {
      p.environment.rustc = "unapproved compiler";
    },
    (p) => {
      p.environment.rustdoc = "unapproved rustdoc";
    },
    (p) => {
      p.environment.flags.RUSTFLAGS += "--cfg unapproved";
    },
    (p) => {
      p.environment.files["Cargo.lock"] = "f".repeat(64);
    },
    (p) => {
      delete p.profiles["worker/default"];
    },
    (p) => {
      p.profiles["native/default"].features = ["test-utils"];
    },
    (p) => {
      p.profiles["native/default"].target = PROFILES["wasi/default"].target;
    },
    (p) => {
      p.profiles["native/default"].rustdocArgs.push("--all-features");
    },
    (p) => {
      p.profiles["native/default"].bootstrap = "1";
    },
  ];
  for (const mutate of mutations) {
    const candidate = structuredClone(f.provenance);
    mutate(candidate);
    assert.throws(() => validateProvenance(candidate, f.repo, current, f.run));
  }
});

test("swapped targets, missing test-utils and changed baseline bytes are rejected", (t) => {
  const f = fixture();
  f.overrides.set("worker/default", fixtures.get("wasi/default"));
  assert.throws(
    () => capturedMain(t, ["worker/default"], f),
    /incorrect current crate\/target/,
  );
  f.overrides.set("native/test-utils", fixtures.get("native/default"));
  assert.throws(
    () => capturedMain(t, ["native/test-utils"], f),
    /undeclared exact API delta/,
  );
  writeFileSync(path.join(f.baselineDirectory, "native-default.json"), "{}");
  assert.throws(
    () => capturedMain(t, ["native/default"], f),
    /API baseline hash mismatch/,
  );
});

test("candidate span/raw SHA changes are not compared with origin capture SHA", () => {
  const f = fixture();
  const doc = structuredClone(fixtures.get("native/default"));
  doc.index[doc.root].span.filename = "/different/checkout/lib.rs";
  assert.notEqual(
    sha256(JSON.stringify(doc)),
    f.provenance.profiles["native/default"].rawSha256,
  );
  assert.equal(
    checkProfile(
      "native/default",
      doc,
      readFileSync(path.join(f.baselineDirectory, "native-default.json")),
      f.provenance,
      [],
    ).changes.length,
    0,
  );
});

test("changes mode reports undeclared candidates without writing or granting approval", (t) => {
  const f = fixture();
  const candidate = withAddedFunction(
    fixtures.get("native/default"),
    "unapproved",
  );
  f.overrides.set("native/default", candidate);
  const ledger = readFileSync(f.manifest);
  const report = capturedMain(t, ["native/default", "--changes"], f);
  assert(
    report.result[0].changes.some((change) =>
      change.key.endsWith("::unapproved"),
    ),
  );
  assert.deepEqual(readFileSync(f.manifest), ledger);
  assert.throws(
    () => capturedMain(t, ["native/default"], f),
    /undeclared exact API delta/,
  );
});

test("manifest origin and profile names are exact", () => {
  const f = fixture();
  assert.deepEqual(loadManifest(f.manifest), []);
  for (const document of [
    { schema: 1, baselineCommit: "wrong", changes: [] },
    {
      schema: 1,
      baselineCommit: BASELINE_COMMIT,
      changes: [{ profile: "native/*" }],
    },
  ]) {
    writeFileSync(f.manifest, JSON.stringify(document));
    assert.throws(() => loadManifest(f.manifest));
  }
});
