#!/usr/bin/env node
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { check, compact, delta, normalize, text } from "./normalize.mjs";

export const BASELINE_COMMIT = "ca95a2dacb7fac1a47d7504ec0cc8d0dad8539bf";
export const CRATE = "trusted_server_core";
export const API_DIRECTORY =
  "crates/trusted-server-integration-tests/fixtures/integration_split_baseline/api";
export const PINNED_FILES = [
  "Cargo.lock",
  "rust-toolchain.toml",
  ".tool-versions",
];
export const FLAG_VARIABLES = [
  "RUSTFLAGS",
  "CARGO_ENCODED_RUSTFLAGS",
  "RUSTDOCFLAGS",
];
const TARGETS = {
  native: "x86_64-unknown-linux-gnu",
  wasi: "wasm32-wasip1",
  worker: "wasm32-unknown-unknown",
};
export const PROFILES = Object.fromEntries(
  Object.entries(TARGETS).flatMap(([family, target]) =>
    ["default", "test-utils"].map((mode) => [
      `${family}/${mode}`,
      {
        target,
        features: mode === "default" ? [] : ["test-utils"],
        snapshot: `${family}-${mode}.json`,
      },
    ]),
  ),
);
export const sha256 = (bytes) =>
  createHash("sha256").update(bytes).digest("hex");
const json = (file) => JSON.parse(readFileSync(file, "utf8"));
const digest = (value) =>
  typeof value === "string" && /^[a-f0-9]{64}$/.test(value);

export function rustdocArgs(label) {
  const profile = PROFILES[label];
  if (!profile) throw Error(`unknown API profile ${label}`);
  return [
    "rustdoc",
    "--locked",
    "-p",
    "trusted-server-core",
    "--lib",
    "--target",
    profile.target,
    ...(profile.features.length
      ? ["--features", profile.features.join(",")]
      : []),
    "--",
    "-Z",
    "unstable-options",
    "--output-format",
    "json",
    "--document-hidden-items",
  ];
}

export function command(
  program,
  args,
  { cwd, env = process.env, capture = true } = {},
) {
  const result = spawnSync(program, args, {
    cwd,
    env,
    encoding: "utf8",
    stdio: capture ? ["ignore", "pipe", "inherit"] : "inherit",
  });
  if (result.error) throw result.error;
  if (result.status !== 0)
    throw Error(`${program} failed with ${result.signal ?? result.status}`);
  return capture ? result.stdout : "";
}

export function environmentPins(repo, run = command) {
  return {
    node: process.version,
    rustc: run("rustc", ["--version", "--verbose"], { cwd: repo }).trim(),
    rustdoc: run("rustdoc", ["--version", "--verbose"], { cwd: repo }).trim(),
    files: Object.fromEntries(
      PINNED_FILES.map((file) => [
        file,
        sha256(readFileSync(path.join(repo, file))),
      ]),
    ),
    flags: Object.fromEntries(
      FLAG_VARIABLES.map((name) => [name, process.env[name] ?? ""]),
    ),
  };
}

export function baselineFilePins(repo, run = command) {
  return Object.fromEntries(
    PINNED_FILES.map((file) => [
      file,
      sha256(run("git", ["show", `${BASELINE_COMMIT}:${file}`], { cwd: repo })),
    ]),
  );
}

export function validateProvenance(provenance, repo, current, run = command) {
  if (
    provenance.schema !== 1 ||
    provenance.baselineCommit !== BASELINE_COMMIT ||
    provenance.crate !== CRATE ||
    provenance.rustdocFormat !== 60
  )
    throw Error("incorrect API baseline provenance");
  if (
    text(Object.keys(provenance.profiles ?? {}).sort()) !==
    text(Object.keys(PROFILES).sort())
  )
    throw Error("API provenance requires all six exact profiles");
  if (
    text(provenance.environment?.flags) !== text(current.flags) ||
    provenance.environment?.node !== current.node ||
    provenance.environment?.rustc !== current.rustc ||
    provenance.environment?.rustdoc !== current.rustdoc
  )
    throw Error("API compiler/Node/flags provenance mismatch");
  if (text(provenance.environment.files) !== text(baselineFilePins(repo, run)))
    throw Error("incorrect immutable baseline input pins");
  const toolVersions = readFileSync(path.join(repo, ".tool-versions"), "utf8");
  const node = toolVersions
    .split(/\r?\n/)
    .map((line) => line.trim().split(/\s+/))
    .find(([name]) => name === "nodejs")?.[1];
  if (current.node !== `v${node}`)
    throw Error("API inspection requires the repository-pinned Node");
  const toolchain = readFileSync(
    path.join(repo, "rust-toolchain.toml"),
    "utf8",
  );
  const release = toolchain.match(/channel\s*=\s*"([^"]+)"/)?.[1];
  if (
    !current.rustc.split("\n").includes(`release: ${release}`) ||
    !current.rustdoc.split("\n").includes(`release: ${release}`)
  )
    throw Error("API inspection requires the repository-pinned Rust");
  for (const [label, expected] of Object.entries(PROFILES)) {
    const profile = provenance.profiles[label];
    if (
      profile.target !== expected.target ||
      text(profile.features) !== text(expected.features) ||
      profile.snapshot !== expected.snapshot ||
      text(profile.rustdocArgs) !== text(rustdocArgs(label)) ||
      profile.bootstrap !== CRATE ||
      !digest(profile.rawSha256) ||
      !digest(profile.snapshotSha256)
    )
      throw Error(`incorrect provenance profile ${label}`);
  }
}

export function loadManifest(file) {
  const manifest = json(file);
  if (
    manifest.schema !== 1 ||
    manifest.baselineCommit !== BASELINE_COMMIT ||
    !Array.isArray(manifest.changes)
  )
    throw Error("incorrect cumulative API delta manifest");
  for (const row of manifest.changes)
    if (!Object.hasOwn(PROFILES, row.profile))
      throw Error(`unknown manifest profile ${row.profile}`);
  return manifest.changes;
}

function validateDocumentProfile(label, document) {
  if (
    !Object.hasOwn(PROFILES, label) ||
    document.index[document.root]?.name !== CRATE ||
    document.target.triple !== PROFILES[label].target ||
    document.includes_private !== false
  )
    throw Error(`incorrect current crate/target/view: ${label}`);
}

// Pure capture helper. Only the baseline owner writes its returned bytes.
export function captureProfile(label, rawBytes) {
  const document = JSON.parse(rawBytes);
  validateDocumentProfile(label, document);
  const snapshotBytes = Buffer.from(
    JSON.stringify(compact(normalize(document, label)), null, 2) + "\n",
  );
  return {
    snapshotBytes,
    provenance: {
      ...PROFILES[label],
      rustdocArgs: rustdocArgs(label),
      bootstrap: CRATE,
      rawSha256: sha256(rawBytes),
      snapshotSha256: sha256(snapshotBytes),
    },
  };
}

export function checkProfile(
  label,
  document,
  baselineBytes,
  provenance,
  manifest,
  changesOnly = false,
) {
  const profile = provenance.profiles[label];
  if (sha256(baselineBytes) !== profile.snapshotSha256)
    throw Error(`API baseline hash mismatch: ${label}`);
  const baseline = JSON.parse(baselineBytes);
  validateDocumentProfile(label, document);
  const current = normalize(document, label);
  const rows = manifest.filter((row) => row.profile === label);
  const changes = changesOnly
    ? delta(baseline, current)
    : check(baseline, current, rows);
  return {
    profile: label,
    records: Object.keys(current.records).length,
    changes,
  };
}

export function parseOptions(args) {
  const labels = args.filter((arg) => !arg.startsWith("--"));
  const flags = args.filter((arg) => arg.startsWith("--"));
  if (
    flags.some((flag) => !["--all", "--changes"].includes(flag)) ||
    new Set(flags).size !== flags.length ||
    (flags.includes("--all") ? labels.length !== 0 : labels.length !== 1) ||
    labels.some((label) => !Object.hasOwn(PROFILES, label))
  )
    throw Error(
      "usage: scripts/check-integration-api-delta.sh <profile> [--changes] | --all [--changes]",
    );
  return {
    profiles: flags.includes("--all") ? Object.keys(PROFILES) : labels,
    changesOnly: flags.includes("--changes"),
  };
}

export function main(
  args,
  repo = fileURLToPath(new URL("../../", import.meta.url)),
  run = command,
) {
  const options = parseOptions(args);
  const directory = path.join(repo, API_DIRECTORY);
  const provenance = json(path.join(directory, "provenance.json"));
  const current = environmentPins(repo, run);
  validateProvenance(provenance, repo, current, run);
  const manifest = loadManifest(
    path.join(repo, "scripts/integration-api/delta-manifest.json"),
  );
  const metadata = JSON.parse(
    run(
      "cargo",
      ["metadata", "--locked", "--no-deps", "--format-version", "1"],
      { cwd: repo },
    ),
  );
  const results = [];
  for (const label of options.profiles) {
    const profile = PROFILES[label];
    run("cargo", rustdocArgs(label), {
      cwd: repo,
      env: { ...process.env, RUSTC_BOOTSTRAP: CRATE },
      capture: false,
    });
    const document = json(
      path.join(
        metadata.target_directory,
        profile.target,
        "doc",
        `${CRATE}.json`,
      ),
    );
    results.push(
      checkProfile(
        label,
        document,
        readFileSync(path.join(directory, profile.snapshot)),
        provenance,
        manifest,
        options.changesOnly,
      ),
    );
  }
  if (options.changesOnly)
    console.log(
      JSON.stringify(
        { candidateLockSha256: current.files["Cargo.lock"], profiles: results },
        null,
        2,
      ),
    );
  else {
    console.log(`candidate Cargo.lock SHA-256: ${current.files["Cargo.lock"]}`);
    for (const result of results)
      console.log(
        `${result.profile}: ${result.records} API records, exact delta approved`,
      );
  }
  return results;
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href
) {
  try {
    main(process.argv.slice(2));
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
