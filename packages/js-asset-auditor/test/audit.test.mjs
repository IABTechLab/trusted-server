import assert from "node:assert/strict";
import test from "node:test";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { parseArgs, readPublisherDomain } from "../lib/audit.mjs";

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

test("parseArgs keeps optional --config path semantics", () => {
  const args = parseArgs([
    "node",
    "audit.mjs",
    "example.com",
    "--config",
    "--diff",
  ]);

  assert.equal(args.url, "https://example.com");
  assert.equal(args.config, "trusted-server.toml");
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
