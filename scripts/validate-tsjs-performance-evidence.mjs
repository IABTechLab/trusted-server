#!/usr/bin/env node

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

const EXPECTED = Object.freeze({
  schemaVersion: 2,
  chromium: "145.0.7632.6",
  machineClass: "github-hosted:ubuntu-24.04",
  runnerImage: "ubuntu-24.04",
  fixture: "tsjs-core-placeholder-v1",
  controller: "generated-server-v1",
  node: "v24.12.0",
  npm: "11.6.2",
  typescript: "6.0.3",
  warmups: 5,
  samples: 50,
  percentile: 90,
  p90CeilingMs: 28.6,
  heapCeilings: Object.freeze({
    afterBoot: 1_329_697,
    afterFirstRender: 1_333_217,
    afterRefresh: 1_333_217,
    afterSpaNavigation: 1_341_419,
  }),
  workflowName: "TSJS Performance Gate",
  workflowFile: ".github/workflows/tsjs-performance-gate.yml",
});

function fail(message) {
  throw new Error(`invalid TSJS performance evidence: ${message}`);
}

function record(value, path) {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    fail(`${path} must be an object`);
  }
  return value;
}

function exactKeys(value, keys, path) {
  const actual = Object.keys(record(value, path)).sort();
  const expected = [...keys].sort();
  if (
    actual.length !== expected.length ||
    actual.some((key, index) => key !== expected[index])
  ) {
    fail(`${path} has an unexpected schema`);
  }
}

function exactString(value, expected, path) {
  if (typeof value !== "string" || value !== expected)
    fail(`${path} must equal ${expected}`);
}

function boolean(value, expected, path) {
  if (typeof value !== "boolean" || value !== expected)
    fail(`${path} must equal ${expected}`);
}

function finiteNumber(value, path, { integer = false, minimum = 0 } = {}) {
  if (
    typeof value !== "number" ||
    !Number.isFinite(value) ||
    value < minimum ||
    (integer && !Number.isSafeInteger(value))
  ) {
    fail(`${path} must be a finite${integer ? " safe integer" : " number"}`);
  }
  return value;
}

function nearestRank(values, percentile) {
  const ordered = [...values].sort((left, right) => left - right);
  return ordered[Math.ceil((percentile / 100) * ordered.length) - 1];
}

export function validateEvidence(evidence, expected) {
  const expectedMode = expected.mode;
  if (expectedMode !== "preswitch" && expectedMode !== "postswitch") {
    fail("expected mode must be preswitch or postswitch");
  }
  if (!/^[A-Za-z0-9][A-Za-z0-9._-]{7,127}$/.test(expected.evidenceId ?? "")) {
    fail("expected evidence id is invalid");
  }
  if (!/^[0-9a-f]{40}$/.test(expected.headSha ?? ""))
    fail("expected head SHA is invalid");

  exactKeys(
    evidence,
    [
      "schemaVersion",
      "evidenceId",
      "mode",
      "headSha",
      "environment",
      "sampling",
      "marks",
      "performance",
      "heap",
      "requests",
      "assertions",
      "provenance",
      "result",
    ],
    "evidence",
  );
  if (evidence.schemaVersion !== EXPECTED.schemaVersion)
    fail("schemaVersion drifted");
  exactString(evidence.evidenceId, expected.evidenceId, "evidenceId");
  exactString(evidence.mode, expectedMode, "mode");
  exactString(evidence.headSha, expected.headSha, "headSha");
  exactString(evidence.result, "complete", "result");

  exactKeys(
    evidence.environment,
    [
      "chromium",
      "controller",
      "machineClass",
      "runnerImage",
      "fixture",
      "node",
      "npm",
      "typescript",
    ],
    "environment",
  );
  exactString(
    evidence.environment.chromium,
    EXPECTED.chromium,
    "environment.chromium",
  );
  exactString(
    evidence.environment.controller,
    EXPECTED.controller,
    "environment.controller",
  );
  exactString(
    evidence.environment.machineClass,
    EXPECTED.machineClass,
    "environment.machineClass",
  );
  exactString(
    evidence.environment.runnerImage,
    EXPECTED.runnerImage,
    "environment.runnerImage",
  );
  exactString(
    evidence.environment.fixture,
    EXPECTED.fixture,
    "environment.fixture",
  );
  for (const name of ["node", "npm", "typescript"])
    exactString(
      evidence.environment[name],
      EXPECTED[name],
      `environment.${name}`,
    );

  exactKeys(
    evidence.sampling,
    ["warmups", "samples", "percentile"],
    "sampling",
  );
  if (
    evidence.sampling.warmups !== EXPECTED.warmups ||
    evidence.sampling.samples !== EXPECTED.samples ||
    evidence.sampling.percentile !== EXPECTED.percentile
  ) {
    fail("sampling contract drifted");
  }

  exactKeys(
    evidence.marks,
    ["source", "bidsScript", "firstDisplay", "firstDisplayPaint"],
    "marks",
  );
  exactString(evidence.marks.source, "performance-entry", "marks.source");
  boolean(evidence.marks.bidsScript, true, "marks.bidsScript");
  boolean(evidence.marks.firstDisplay, true, "marks.firstDisplay");
  boolean(evidence.marks.firstDisplayPaint, true, "marks.firstDisplayPaint");

  exactKeys(evidence.performance, ["bootToFirstDisplayMs"], "performance");
  const timing = evidence.performance.bootToFirstDisplayMs;
  exactKeys(
    timing,
    ["samples", "percentile", "p90", "ceilingMs"],
    "performance timing",
  );
  if (
    !Array.isArray(timing.samples) ||
    timing.samples.length !== EXPECTED.samples
  ) {
    fail("performance samples must contain exactly 50 values");
  }
  const samples = timing.samples.map((value, index) =>
    finiteNumber(value, `performance sample ${index}`),
  );
  if (timing.percentile !== EXPECTED.percentile)
    fail("performance percentile drifted");
  if (timing.ceilingMs !== EXPECTED.p90CeilingMs)
    fail("performance ceiling drifted");
  const p90 = finiteNumber(timing.p90, "performance p90");
  if (!Object.is(p90, nearestRank(samples, EXPECTED.percentile))) {
    fail("performance p90 is inconsistent with the samples");
  }
  if (p90 > EXPECTED.p90CeilingMs) fail("performance p90 exceeds 28.6 ms");

  exactKeys(evidence.heap, ["collection", "checkpoints"], "heap");
  exactString(
    evidence.heap.collection,
    "one-collectGarbage-then-immediate-getHeapUsage",
    "heap.collection",
  );
  exactKeys(
    evidence.heap.checkpoints,
    Object.keys(EXPECTED.heapCeilings),
    "heap.checkpoints",
  );
  for (const [name, ceiling] of Object.entries(EXPECTED.heapCeilings)) {
    const checkpoint = evidence.heap.checkpoints[name];
    exactKeys(
      checkpoint,
      ["usedSize", "ceilingBytes"],
      `heap.checkpoints.${name}`,
    );
    if (checkpoint.ceilingBytes !== ceiling)
      fail(`${name} heap ceiling drifted`);
    if (
      finiteNumber(checkpoint.usedSize, `${name} usedSize`, { integer: true }) >
      ceiling
    ) {
      fail(`${name} retained heap exceeds its ceiling`);
    }
  }

  exactKeys(evidence.requests, ["critical", "deferred"], "requests");
  exactKeys(evidence.requests.critical, ["count"], "requests.critical");
  if (evidence.requests.critical.count !== 1)
    fail("critical request count must be exactly one");
  exactKeys(
    evidence.requests.deferred,
    [
      "count",
      "requestBeforePaintCount",
      "preloadBeforePaintCount",
      "preparationBeforePaintCount",
      "executionBeforePaintCount",
      "independentlyTriggered",
      "headOfLineBlocking",
    ],
    "requests.deferred",
  );
  finiteNumber(evidence.requests.deferred.count, "deferred count", {
    integer: true,
  });
  if (evidence.requests.deferred.count !== 2)
    fail("deferred module count must be exactly two");
  for (const name of [
    "requestBeforePaintCount",
    "preloadBeforePaintCount",
    "preparationBeforePaintCount",
    "executionBeforePaintCount",
  ]) {
    if (evidence.requests.deferred[name] !== 0)
      fail(`deferred ${name} must be zero`);
  }
  boolean(
    evidence.requests.deferred.independentlyTriggered,
    true,
    "deferred independentlyTriggered",
  );
  boolean(
    evidence.requests.deferred.headOfLineBlocking,
    false,
    "deferred headOfLineBlocking",
  );

  exactKeys(evidence.assertions, ["correctness", "loadOrder"], "assertions");
  boolean(evidence.assertions.correctness, true, "assertions.correctness");
  boolean(evidence.assertions.loadOrder, true, "assertions.loadOrder");

  exactKeys(
    evidence.provenance,
    [
      "workflowName",
      "workflowFile",
      "runId",
      "runAttempt",
      "artifactName",
      "headSha",
    ],
    "provenance",
  );
  exactString(
    evidence.provenance.workflowName,
    EXPECTED.workflowName,
    "provenance.workflowName",
  );
  exactString(
    evidence.provenance.workflowFile,
    EXPECTED.workflowFile,
    "provenance.workflowFile",
  );
  finiteNumber(evidence.provenance.runId, "provenance.runId", {
    integer: true,
    minimum: 1,
  });
  finiteNumber(evidence.provenance.runAttempt, "provenance.runAttempt", {
    integer: true,
    minimum: 1,
  });
  exactString(
    evidence.provenance.artifactName,
    `tsjs-performance-${expected.evidenceId}`,
    "provenance.artifactName",
  );
  exactString(
    evidence.provenance.headSha,
    expected.headSha,
    "provenance.headSha",
  );
  return evidence;
}

function validFixture() {
  const evidenceId = "aps-tsjs-preswitch-12345678";
  const headSha = "a".repeat(40);
  const samples = Array.from({ length: 50 }, () => 20);
  return {
    expected: { evidenceId, headSha, mode: "preswitch" },
    evidence: {
      schemaVersion: 2,
      evidenceId,
      mode: "preswitch",
      headSha,
      environment: {
        chromium: "145.0.7632.6",
        controller: "generated-server-v1",
        machineClass: "github-hosted:ubuntu-24.04",
        runnerImage: "ubuntu-24.04",
        fixture: "tsjs-core-placeholder-v1",
        node: "v24.12.0",
        npm: "11.6.2",
        typescript: "6.0.3",
      },
      sampling: { warmups: 5, samples: 50, percentile: 90 },
      marks: {
        source: "performance-entry",
        bidsScript: true,
        firstDisplay: true,
        firstDisplayPaint: true,
      },
      performance: {
        bootToFirstDisplayMs: {
          samples,
          percentile: 90,
          p90: 20,
          ceilingMs: 28.6,
        },
      },
      heap: {
        collection: "one-collectGarbage-then-immediate-getHeapUsage",
        checkpoints: Object.fromEntries(
          Object.entries(EXPECTED.heapCeilings).map(([name, ceilingBytes]) => [
            name,
            { usedSize: ceilingBytes, ceilingBytes },
          ]),
        ),
      },
      requests: {
        critical: { count: 1 },
        deferred: {
          count: 2,
          requestBeforePaintCount: 0,
          preloadBeforePaintCount: 0,
          preparationBeforePaintCount: 0,
          executionBeforePaintCount: 0,
          independentlyTriggered: true,
          headOfLineBlocking: false,
        },
      },
      assertions: { correctness: true, loadOrder: true },
      provenance: {
        workflowName: "TSJS Performance Gate",
        workflowFile: ".github/workflows/tsjs-performance-gate.yml",
        runId: 123,
        runAttempt: 1,
        artifactName: `tsjs-performance-${evidenceId}`,
        headSha,
      },
      result: "complete",
    },
  };
}

function runSelfTest() {
  const fixture = validFixture();
  validateEvidence(fixture.evidence, fixture.expected);
  const mutations = [
    ["evidence id", (value) => (value.evidenceId = "wrong-evidence")],
    ["head SHA", (value) => (value.headSha = "b".repeat(40))],
    ["mode", (value) => (value.mode = "postswitch")],
    ["environment", (value) => (value.environment.chromium = "145.0.0.0")],
    ["controller", (value) => (value.environment.controller = "handwritten")],
    ["fixture", (value) => (value.environment.fixture = "drifted")],
    ["node", (value) => (value.environment.node = "v24.11.0")],
    ["npm", (value) => (value.environment.npm = "11.6.1")],
    ["typescript", (value) => (value.environment.typescript = "6.0.2")],
    ["warmups", (value) => (value.sampling.warmups = 4)],
    [
      "sample count",
      (value) => value.performance.bootToFirstDisplayMs.samples.pop(),
    ],
    ["percentile", (value) => (value.sampling.percentile = 95)],
    ["real marks", (value) => (value.marks.source = "synthetic")],
    ["missing mark", (value) => (value.marks.firstDisplay = false)],
    [
      "p90 limit",
      (value) => {
        value.performance.bootToFirstDisplayMs.samples.fill(29);
        value.performance.bootToFirstDisplayMs.p90 = 29;
      },
    ],
    [
      "p90 consistency",
      (value) => (value.performance.bootToFirstDisplayMs.p90 = 19),
    ],
    [
      "finite sample",
      (value) => (value.performance.bootToFirstDisplayMs.samples[0] = null),
    ],
    [
      "heap",
      (value) => (value.heap.checkpoints.afterBoot.usedSize = 1_329_698),
    ],
    ["critical count", (value) => (value.requests.critical.count = 2)],
    ["deferred count", (value) => (value.requests.deferred.count = 1)],
    ["excess deferred count", (value) => (value.requests.deferred.count = 3)],
    [
      "deferred request",
      (value) => (value.requests.deferred.requestBeforePaintCount = 1),
    ],
    [
      "deferred preload",
      (value) => (value.requests.deferred.preloadBeforePaintCount = 1),
    ],
    [
      "deferred prepare",
      (value) => (value.requests.deferred.preparationBeforePaintCount = 1),
    ],
    [
      "deferred execute",
      (value) => (value.requests.deferred.executionBeforePaintCount = 1),
    ],
    ["HOL", (value) => (value.requests.deferred.headOfLineBlocking = true)],
    [
      "independent",
      (value) => (value.requests.deferred.independentlyTriggered = false),
    ],
    ["correctness", (value) => (value.assertions.correctness = false)],
    ["load order", (value) => (value.assertions.loadOrder = false)],
    ["incomplete", (value) => (value.result = "failed")],
    [
      "workflow",
      (value) => (value.provenance.workflowFile = ".github/workflows/test.yml"),
    ],
    ["artifact", (value) => (value.provenance.artifactName = "wrong")],
    ["schema", (value) => (value.extra = true)],
  ];
  for (const [name, mutate] of mutations) {
    const candidate = structuredClone(fixture.evidence);
    mutate(candidate);
    assert.throws(
      () => validateEvidence(candidate, fixture.expected),
      /invalid TSJS performance evidence/u,
      `${name} mutation should be rejected`,
    );
  }
  assert.throws(
    () =>
      parseArguments([
        "--file",
        "evidence.json",
        "--evidence-id",
        fixture.expected.evidenceId,
        "--head-sha",
        fixture.expected.headSha,
        "--mode",
        "preswitch",
        "--mode",
        "postswitch",
      ]),
    /invalid TSJS performance evidence/u,
    "duplicate CLI bindings must be rejected rather than overwritten",
  );
  const repositoryRoot = new URL("../", import.meta.url);
  const performanceTest = readFileSync(
    new URL(
      "crates/trusted-server-integration-tests/browser/tests/shared/tsjs-performance.spec.ts",
      repositoryRoot,
    ),
    "utf8",
  );
  const performanceWorkflow = readFileSync(
    new URL(".github/workflows/tsjs-performance-gate.yml", repositoryRoot),
    "utf8",
  );
  const generalTestWorkflow = readFileSync(
    new URL(".github/workflows/test.yml", repositoryRoot),
    "utf8",
  );
  assert.match(
    performanceTest,
    /generate-tsjs-prospective-fixture/u,
    "the browser gate must consume the generated prospective server controller",
  );
  assert.match(
    performanceTest,
    /TSJS_SKIP_BUILD/u,
    "the controller generator must consume the workflow's one canonical artifact build",
  );
  assert.doesNotMatch(
    performanceTest,
    /__tsjsPerf/u,
    "the browser gate must read real performance entries, never the placeholder API",
  );
  assert.match(
    performanceTest,
    /preparationBeforePaintCount/u,
    "the browser gate must record deferred preparation timing",
  );
  assert.match(
    performanceTest,
    /executionBeforePaintCount/u,
    "the browser gate must record deferred activation timing",
  );
  assert.match(
    performanceTest,
    /preloadTimes/u,
    "the browser gate must retain transient deferred preload observations",
  );
  assert.match(
    performanceTest,
    /publisherRefresh/u,
    "the retained-heap refresh checkpoint must use the publisher GPT refresh path",
  );
  assert.match(
    performanceTest,
    /auctionId: "performance-navigation"[\s\S]*results: \[\{ slot: "perf-slot", outcome: "no_bid" \}\]/u,
    "the SPA heap checkpoint must use a projection with real GPT reconciliation",
  );
  assert.match(
    performanceTest,
    /expect\(await response\.finished\(\)\)\.toBeNull\(\)[\s\S]*getSlots\(\)[\s\S]*afterSpaNavigation/u,
    "the SPA heap checkpoint must await the response body and reconciled GPT slot",
  );
  assert.doesNotMatch(
    performanceWorkflow,
    /setup-integration-test-env|VICEROY|WASM_ARTIFACT|build-test-images/u,
    "the hermetic performance fixture must not build unused runtime infrastructure",
  );
  assert.doesNotMatch(
    generalTestWorkflow,
    /tsjs-performance-gate\.yml/u,
    "general CI must not redefine or automatically rerun immutable performance evidence",
  );
  console.log(
    `TSJS performance evidence self-test passed (${mutations.length} mutations)`,
  );
}

function parseArguments(arguments_) {
  const values = new Map();
  for (let index = 0; index < arguments_.length; index += 2) {
    const name = arguments_[index];
    const value = arguments_[index + 1];
    if (!name?.startsWith("--") || value === undefined)
      fail("invalid CLI arguments");
    if (values.has(name)) fail(`duplicate ${name}`);
    values.set(name, value);
  }
  for (const name of ["--file", "--evidence-id", "--head-sha", "--mode"]) {
    if (!values.has(name)) fail(`missing ${name}`);
  }
  if (values.size !== 4) fail("unknown or duplicate CLI arguments");
  return values;
}

function main() {
  if (process.argv.length === 3 && process.argv[2] === "--self-test") {
    runSelfTest();
    return;
  }
  const arguments_ = parseArguments(process.argv.slice(2));
  let evidence;
  try {
    evidence = JSON.parse(readFileSync(arguments_.get("--file"), "utf8"));
  } catch (error) {
    fail(
      `cannot read JSON evidence: ${error instanceof Error ? error.message : String(error)}`,
    );
  }
  validateEvidence(evidence, {
    evidenceId: arguments_.get("--evidence-id"),
    headSha: arguments_.get("--head-sha"),
    mode: arguments_.get("--mode"),
  });
  console.log("TSJS performance evidence is valid");
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    main();
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  }
}
