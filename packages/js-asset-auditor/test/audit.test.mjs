import assert from "node:assert/strict";
import test from "node:test";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import {
  ensureConfigPathWritable,
  ensurePathWritable,
  parseArgs,
  readPublisherDomain,
  resolvePublisherDomain,
} from "../lib/audit.mjs";

function captureExit(fn) {
  const originalExit = process.exit;
  const originalError = console.error;
  const calls = [];

  process.exit = (code) => {
    const error = new Error(`process.exit:${code}`);
    error.code = code;
    throw error;
  };
  console.error = (...args) => {
    calls.push(args.join(" "));
  };

  try {
    fn();
    assert.fail("Expected process.exit to be called");
  } catch (error) {
    assert.match(error.message, /^process\.exit:/);
    return { error, stderr: calls.join("\n") };
  } finally {
    process.exit = originalExit;
    console.error = originalError;
  }
}

test("parseArgs rejects missing values instead of swallowing the next flag", () => {
  const result = captureExit(() =>
    parseArgs([
      "node",
      "audit.mjs",
      "https://example.com",
      "--first-party",
      "--diff",
    ]),
  );

  assert.equal(result.error.code, 1);
  assert.match(result.stderr, /--first-party requires a value/);
  assert.match(result.stderr, /Usage: audit-js-assets/);
});

test("parseArgs normalizes --first-party URL values to hostnames", () => {
  const args = parseArgs([
    "node",
    "audit.mjs",
    "example.com",
    "--first-party",
    "https://www.example.com/path, cdn.example.com ",
  ]);

  assert.deepEqual(args.firstParty, ["www.example.com", "cdn.example.com"]);
});

test("parseArgs rejects invalid --first-party values", () => {
  const result = captureExit(() =>
    parseArgs([
      "node",
      "audit.mjs",
      "example.com",
      "--first-party",
      "not a host",
    ]),
  );

  assert.equal(result.error.code, 1);
  assert.match(result.stderr, /--first-party value is not a valid host/);
});

test("parseArgs keeps optional --config path semantics", () => {
  const args = parseArgs([
    "node",
    "audit.mjs",
    "example.com",
    "--config",
    "--diff",
  ]);

  assert.equal(args.url, "https://example.com");
  assert.equal(args.config, "trusted-server.generated.toml");
  assert.equal(args.diff, true);
});

test("parseArgs validates --settle as a non-negative integer", () => {
  const result = captureExit(() =>
    parseArgs([
      "node",
      "audit.mjs",
      "https://example.com",
      "--settle",
      "abc",
    ]),
  );

  assert.match(result.stderr, /--settle requires a non-negative integer value/);
});

test("readPublisherDomain reads a valid config and fails on malformed content", () => {
  const repoRoot = mkdtempSync(join(tmpdir(), "js-asset-auditor-"));
  writeFileSync(
    join(repoRoot, "trusted-server.toml"),
    "[publisher]\ndomain = \"publisher.com\"\n",
  );

  assert.equal(readPublisherDomain(repoRoot), "publisher.com");

  writeFileSync(
    join(repoRoot, "trusted-server.toml"),
    "[publisher]\ndomain = 'publisher.com'\n",
  );

  assert.throws(
    () => readPublisherDomain(repoRoot),
    /Could not find \[publisher\]\.domain/,
  );
});

test("resolvePublisherDomain reports the selected source", () => {
  const repoRoot = mkdtempSync(join(tmpdir(), "js-asset-auditor-"));
  writeFileSync(
    join(repoRoot, "trusted-server.toml"),
    "[publisher]\ndomain = \"config-domain.test\"\n",
  );

  const originalError = console.error;
  const calls = [];
  console.error = (...args) => {
    calls.push(args.join(" "));
  };

  try {
    const fromFlag = resolvePublisherDomain(
      { domain: "flag-domain.test", url: "https://example.com" },
      repoRoot,
    );
    const fromConfig = resolvePublisherDomain(
      { domain: null, url: "https://example.com" },
      repoRoot,
    );

    assert.equal(fromFlag, "flag-domain.test");
    assert.equal(fromConfig, "config-domain.test");
    assert.match(calls[0], /Using publisher domain from --domain: flag-domain\.test/);
    assert.match(
      calls[1],
      /Using publisher domain from trusted-server\.toml: config-domain\.test/,
    );
  } finally {
    console.error = originalError;
  }
});

test("ensurePathWritable exits when a target already exists without --force", () => {
  const repoRoot = mkdtempSync(join(tmpdir(), "js-asset-auditor-"));
  const outputPath = join(repoRoot, "js-assets.toml");
  writeFileSync(outputPath, "[[js_assets]]\n");

  const result = captureExit(() => ensurePathWritable(outputPath, false));

  assert.equal(result.error.code, 1);
  assert.match(result.stderr, /already exists\. Use --force to overwrite\./);
});

test("ensureConfigPathWritable exits when config already exists without --force", () => {
  const repoRoot = mkdtempSync(join(tmpdir(), "js-asset-auditor-"));
  const configPath = join(repoRoot, "trusted-server.generated.toml");
  writeFileSync(configPath, "[publisher]\n");

  const result = captureExit(() => ensureConfigPathWritable(configPath, false));

  assert.equal(result.error.code, 1);
  assert.match(result.stderr, /already exists\. Use --force to overwrite\./);
});
