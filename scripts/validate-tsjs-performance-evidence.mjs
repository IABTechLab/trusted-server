#!/usr/bin/env node

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

const EXPECTED = Object.freeze({
  schemaVersion: 5,
  chromium: "145.0.7632.6",
  machineClass: "github-hosted:ubuntu-24.04",
  runnerImage: "ubuntu-24.04",
  fixture: "tsjs-main-paired-network-v2",
  controller: "generated-server-v1+production-main-v1",
  node: "v24.12.0",
  npm: "11.6.2",
  typescript: "6.0.3",
  warmupsPerVariant: 5,
  samplesPerVariant: 50,
  percentile: 90,
  interleaving: "alternating-main-candidate",
  networkProfile: Object.freeze({
    mechanism: "cdp-Network.emulateNetworkConditions",
    appliedBeforeNavigation: true,
    latencyMs: 150,
    downloadThroughputBytesPerSecond: 200_000,
    uploadThroughputBytesPerSecond: 93_750,
    packetLossPercent: 0,
  }),
  maximumRatio: 1.1,
  heapCheckpoints: Object.freeze([
    "afterBoot",
    "afterFirstRender",
    "afterRefresh",
    "afterSpaNavigation",
  ]),
  heapMaximumRatio: 1.1,
  heapHardCeilingBytes: 4 * 1024 * 1024,
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
  if (
    expectedMode !== "preswitch" &&
    expectedMode !== "postswitch" &&
    expectedMode !== "pull-request"
  ) {
    fail("expected mode must be preswitch, postswitch, or pull-request");
  }
  if (!/^[A-Za-z0-9][A-Za-z0-9._-]{7,127}$/.test(expected.evidenceId ?? "")) {
    fail("expected evidence id is invalid");
  }
  if (!/^[0-9a-f]{40}$/.test(expected.headSha ?? ""))
    fail("expected head SHA is invalid");
  if (!/^[0-9a-f]{40}$/.test(expected.mainSha ?? ""))
    fail("expected main SHA is invalid");

  exactKeys(
    evidence,
    [
      "schemaVersion",
      "evidenceId",
      "mode",
      "headSha",
      "environment",
      "sampling",
      "networkProfile",
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
    ["warmupsPerVariant", "samplesPerVariant", "percentile", "interleaving"],
    "sampling",
  );
  if (
    evidence.sampling.warmupsPerVariant !== EXPECTED.warmupsPerVariant ||
    evidence.sampling.samplesPerVariant !== EXPECTED.samplesPerVariant ||
    evidence.sampling.percentile !== EXPECTED.percentile ||
    evidence.sampling.interleaving !== EXPECTED.interleaving
  ) {
    fail("sampling contract drifted");
  }

  exactKeys(
    evidence.networkProfile,
    [
      "mechanism",
      "appliedBeforeNavigation",
      "latencyMs",
      "downloadThroughputBytesPerSecond",
      "uploadThroughputBytesPerSecond",
      "packetLossPercent",
    ],
    "networkProfile",
  );
  for (const [name, expectedValue] of Object.entries(EXPECTED.networkProfile)) {
    if (evidence.networkProfile[name] !== expectedValue)
      fail(`networkProfile.${name} drifted`);
  }

  exactKeys(
    evidence.marks,
    [
      "source",
      "comparisonStart",
      "firstObservableAction",
      "candidateBidsScript",
      "candidateFirstDisplay",
      "candidateFirstDisplayPaint",
    ],
    "marks",
  );
  exactString(
    evidence.marks.source,
    "fixture-first-observable-action",
    "marks.source",
  );
  for (const name of [
    "comparisonStart",
    "firstObservableAction",
    "candidateBidsScript",
    "candidateFirstDisplay",
    "candidateFirstDisplayPaint",
  ]) {
    boolean(evidence.marks[name], true, `marks.${name}`);
  }

  exactKeys(evidence.performance, ["requestToFirstActionMs"], "performance");
  const timing = evidence.performance.requestToFirstActionMs;
  exactKeys(
    timing,
    ["main", "candidate", "percentile", "maximumRatio", "observedRatio"],
    "performance timing",
  );
  if (timing.percentile !== EXPECTED.percentile)
    fail("performance percentile drifted");
  if (timing.maximumRatio !== EXPECTED.maximumRatio)
    fail("performance ratio limit drifted");
  exactKeys(
    timing.main,
    ["sha", "artifactModel", "criticalTransferBytes", "samples", "p90"],
    "main performance timing",
  );
  exactString(timing.main.sha, expected.mainSha, "performance main SHA");
  if (
    timing.main.artifactModel !== "legacy-main-v1" &&
    timing.main.artifactModel !== "release-v1"
  ) {
    fail("performance main artifact model is invalid");
  }
  finiteNumber(
    timing.main.criticalTransferBytes,
    "main critical transfer bytes",
    { integer: true, minimum: 1 },
  );
  exactKeys(
    timing.candidate,
    ["artifactModel", "criticalTransferBytes", "samples", "p90"],
    "candidate performance timing",
  );
  exactString(
    timing.candidate.artifactModel,
    "release-v1",
    "performance candidate artifact model",
  );
  finiteNumber(
    timing.candidate.criticalTransferBytes,
    "candidate critical transfer bytes",
    { integer: true, minimum: 1 },
  );
  const validateVariant = (variant, path) => {
    if (
      !Array.isArray(variant.samples) ||
      variant.samples.length !== EXPECTED.samplesPerVariant
    ) {
      fail(`${path} samples must contain exactly 50 values`);
    }
    const samples = variant.samples.map((value, index) =>
      finiteNumber(value, `${path} performance sample ${index}`),
    );
    const variantP90 = finiteNumber(variant.p90, `${path} performance p90`);
    if (!Object.is(variantP90, nearestRank(samples, EXPECTED.percentile))) {
      fail(`${path} performance p90 is inconsistent with the samples`);
    }
    return variantP90;
  };
  const mainP90 = validateVariant(timing.main, "main");
  const candidateP90 = validateVariant(timing.candidate, "candidate");
  if (mainP90 <= 0) fail("main performance p90 must be positive");
  const observedRatio = finiteNumber(
    timing.observedRatio,
    "performance observed ratio",
  );
  if (Math.abs(observedRatio - candidateP90 / mainP90) > Number.EPSILON) {
    fail("performance observed ratio is inconsistent with the p90 values");
  }
  if (candidateP90 > mainP90 * EXPECTED.maximumRatio)
    fail("candidate performance p90 exceeds the paired 10% limit");

  exactKeys(
    evidence.heap,
    ["collection", "maximumRatio", "hardCeilingBytes", "main", "candidate"],
    "heap",
  );
  exactString(
    evidence.heap.collection,
    "one-collectGarbage-then-immediate-getHeapUsage",
    "heap.collection",
  );
  if (evidence.heap.maximumRatio !== EXPECTED.heapMaximumRatio)
    fail("heap ratio limit drifted");
  if (evidence.heap.hardCeilingBytes !== EXPECTED.heapHardCeilingBytes)
    fail("heap hard ceiling drifted");
  exactKeys(evidence.heap.main, ["sha", "checkpoints"], "heap.main");
  exactString(evidence.heap.main.sha, expected.mainSha, "heap main SHA");
  exactKeys(evidence.heap.candidate, ["checkpoints"], "heap.candidate");
  exactKeys(
    evidence.heap.main.checkpoints,
    EXPECTED.heapCheckpoints,
    "heap.main.checkpoints",
  );
  exactKeys(
    evidence.heap.candidate.checkpoints,
    EXPECTED.heapCheckpoints,
    "heap.candidate.checkpoints",
  );
  for (const name of EXPECTED.heapCheckpoints) {
    const mainUsedSize = finiteNumber(
      evidence.heap.main.checkpoints[name],
      `heap.main.checkpoints.${name}`,
      { integer: true, minimum: 1 },
    );
    const candidateUsedSize = finiteNumber(
      evidence.heap.candidate.checkpoints[name],
      `heap.candidate.checkpoints.${name}`,
      { integer: true, minimum: 1 },
    );
    if (
      mainUsedSize > EXPECTED.heapHardCeilingBytes ||
      candidateUsedSize > EXPECTED.heapHardCeilingBytes
    ) {
      fail(`${name} retained heap exceeds the hard ceiling`);
    }
    if (candidateUsedSize > mainUsedSize * EXPECTED.heapMaximumRatio) {
      fail(`${name} retained heap exceeds the paired 10% limit`);
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
  const mainSha = "c".repeat(40);
  const mainSamples = Array.from({ length: 50 }, () => 200);
  const candidateSamples = Array.from({ length: 50 }, () => 210);
  return {
    expected: { evidenceId, headSha, mainSha, mode: "preswitch" },
    evidence: {
      schemaVersion: 5,
      evidenceId,
      mode: "preswitch",
      headSha,
      environment: {
        chromium: "145.0.7632.6",
        controller: "generated-server-v1+production-main-v1",
        machineClass: "github-hosted:ubuntu-24.04",
        runnerImage: "ubuntu-24.04",
        fixture: "tsjs-main-paired-network-v2",
        node: "v24.12.0",
        npm: "11.6.2",
        typescript: "6.0.3",
      },
      sampling: {
        warmupsPerVariant: 5,
        samplesPerVariant: 50,
        percentile: 90,
        interleaving: "alternating-main-candidate",
      },
      networkProfile: {
        mechanism: "cdp-Network.emulateNetworkConditions",
        appliedBeforeNavigation: true,
        latencyMs: 150,
        downloadThroughputBytesPerSecond: 200_000,
        uploadThroughputBytesPerSecond: 93_750,
        packetLossPercent: 0,
      },
      marks: {
        source: "fixture-first-observable-action",
        comparisonStart: true,
        firstObservableAction: true,
        candidateBidsScript: true,
        candidateFirstDisplay: true,
        candidateFirstDisplayPaint: true,
      },
      performance: {
        requestToFirstActionMs: {
          main: {
            sha: mainSha,
            artifactModel: "legacy-main-v1",
            criticalTransferBytes: 82_000,
            samples: mainSamples,
            p90: 200,
          },
          candidate: {
            artifactModel: "release-v1",
            criticalTransferBytes: 220_000,
            samples: candidateSamples,
            p90: 210,
          },
          percentile: 90,
          maximumRatio: 1.1,
          observedRatio: 210 / 200,
        },
      },
      heap: {
        collection: "one-collectGarbage-then-immediate-getHeapUsage",
        maximumRatio: 1.1,
        hardCeilingBytes: 4 * 1024 * 1024,
        main: {
          sha: mainSha,
          checkpoints: Object.fromEntries(
            EXPECTED.heapCheckpoints.map((name) => [name, 1_600_000]),
          ),
        },
        candidate: {
          checkpoints: Object.fromEntries(
            EXPECTED.heapCheckpoints.map((name) => [name, 1_650_000]),
          ),
        },
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
    ["warmups", (value) => (value.sampling.warmupsPerVariant = 4)],
    ["interleaving", (value) => (value.sampling.interleaving = "sequential")],
    ["network latency", (value) => (value.networkProfile.latencyMs = 0)],
    [
      "network ordering",
      (value) => (value.networkProfile.appliedBeforeNavigation = false),
    ],
    [
      "candidate sample count",
      (value) =>
        value.performance.requestToFirstActionMs.candidate.samples.pop(),
    ],
    [
      "candidate transfer bytes",
      (value) =>
        (value.performance.requestToFirstActionMs.candidate.criticalTransferBytes = 0),
    ],
    [
      "main sample count",
      (value) => value.performance.requestToFirstActionMs.main.samples.pop(),
    ],
    [
      "main transfer bytes",
      (value) =>
        (value.performance.requestToFirstActionMs.main.criticalTransferBytes = 0),
    ],
    ["percentile", (value) => (value.sampling.percentile = 95)],
    ["real marks", (value) => (value.marks.source = "synthetic")],
    ["missing mark", (value) => (value.marks.firstObservableAction = false)],
    [
      "paired p90 limit",
      (value) => {
        value.performance.requestToFirstActionMs.candidate.samples.fill(221);
        value.performance.requestToFirstActionMs.candidate.p90 = 221;
        value.performance.requestToFirstActionMs.observedRatio = 221 / 200;
      },
    ],
    [
      "p90 consistency",
      (value) => (value.performance.requestToFirstActionMs.candidate.p90 = 19),
    ],
    [
      "finite sample",
      (value) =>
        (value.performance.requestToFirstActionMs.candidate.samples[0] = null),
    ],
    [
      "main SHA",
      (value) =>
        (value.performance.requestToFirstActionMs.main.sha = "b".repeat(40)),
    ],
    [
      "main artifact model",
      (value) =>
        (value.performance.requestToFirstActionMs.main.artifactModel =
          "unknown-v1"),
    ],
    [
      "candidate artifact model",
      (value) =>
        (value.performance.requestToFirstActionMs.candidate.artifactModel =
          "legacy-main-v1"),
    ],
    [
      "observed ratio",
      (value) => (value.performance.requestToFirstActionMs.observedRatio = 1),
    ],
    [
      "heap ratio",
      (value) => (value.heap.candidate.checkpoints.afterBoot = 1_800_000),
    ],
    [
      "heap hard ceiling",
      (value) => {
        value.heap.main.checkpoints.afterBoot = 4 * 1024 * 1024 + 1;
        value.heap.candidate.checkpoints.afterBoot = 4 * 1024 * 1024 + 1;
      },
    ],
    ["heap main SHA", (value) => (value.heap.main.sha = "b".repeat(40))],
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
  const generatorSource = readFileSync(
    new URL(
      "crates/trusted-server-integration-tests/src/bin/generate-tsjs-prospective-fixture.rs",
      repositoryRoot,
    ),
    "utf8",
  );
  const integrationWorkflow = readFileSync(
    new URL(".github/workflows/integration-tests.yml", repositoryRoot),
    "utf8",
  );
  const browserTestScript = readFileSync(
    new URL("scripts/integration-tests-browser.sh", repositoryRoot),
    "utf8",
  );
  const apsProxyScript = readFileSync(
    new URL("scripts/integration-tests-aps-runner-proxy.sh", repositoryRoot),
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
    /from "node:http"/u,
    "the browser gate must serve the fixture through node:http",
  );
  assert.match(
    performanceTest,
    /\.listen\(0, "127\.0\.0\.1"/u,
    "the browser gate must listen on an ephemeral IPv4 loopback port",
  );
  assert.match(
    performanceTest,
    /FIXTURE_ID = "tsjs-main-paired-network-v2"/u,
    "the browser gate must identify the paired network-shaped fixture",
  );
  assert.doesNotMatch(
    performanceTest,
    /62421ee44c62f24534ea8782a46dfa5bfbcea950/u,
    "the browser gate must not retain the obsolete frozen reference SHA",
  );
  assert.match(
    performanceTest,
    /Network\.emulateNetworkConditions[\s\S]*latency: 150[\s\S]*downloadThroughput: 200_000[\s\S]*uploadThroughput: 93_750/u,
    "the browser gate must apply the fixed CDP network profile before navigation",
  );
  assert.ok(
    performanceTest.indexOf(
      'networkSession.send("Network.emulateNetworkConditions"',
    ) < performanceTest.indexOf("await page.goto(fixtureUrl"),
    "the browser gate must install network shaping before either variant navigates",
  );
  assert.match(
    performanceTest,
    /loadLegacyMainFixtureResources[\s\S]*tsjs-core\.js[\s\S]*tsjs-creative\.js[\s\S]*tsjs-gpt\.js/u,
    "the browser gate must consume main's actual legacy core, creative, and GPT artifact shape",
  );
  assert.match(
    performanceTest,
    /CRITICAL_IDS = \["render_runtime", "creative", "gpt"\]/u,
    "the release-v1 comparison must use the same core, render, creative, and GPT shape",
  );
  assert.match(
    generatorSource,
    /creative:[\s\S]*enabled: true/u,
    "the generated candidate controller must enable main's default creative policy",
  );
  assert.match(
    performanceTest,
    /loadMainFixtureResources[\s\S]*tsjs-release-v1\.json[\s\S]*loadReleaseFixtureResources[\s\S]*loadLegacyMainFixtureResources/u,
    "the browser gate must detect main's actual legacy or release-v1 artifact shape",
  );
  assert.match(
    performanceTest,
    /performance\.mark\("tsjs:first-observable-action"\)[\s\S]*display\(target:[\s\S]*markFirstObservableAction\(\)/u,
    "the cross-version endpoint must be the first observable GPT display or refresh action",
  );
  assert.match(
    performanceTest,
    /criticalTransferBytes: mainResources\.criticalTransferBytes[\s\S]*criticalTransferBytes: candidateResources\.criticalTransferBytes/u,
    "the evidence must record each variant's exact served critical bytes",
  );
  assert.match(
    performanceWorkflow,
    /git fetch origin main[\s\S]*main_sha="\$\(git rev-parse origin\/main\)"[\s\S]*TSJS_PERF_MAIN_SHA/u,
    "the performance workflow must resolve and export the exact current main SHA",
  );
  assert.match(
    performanceWorkflow,
    /pull_request:[\s\S]*paths:/u,
    "the performance workflow must run automatically for relevant PR changes",
  );
  assert.doesNotMatch(
    performanceWorkflow,
    /62421ee44c62f24534ea8782a46dfa5bfbcea950/u,
    "the performance workflow must never build a frozen reference instead of current main",
  );
  assert.doesNotMatch(
    performanceTest,
    /\.route(?:FromHAR)?\(|\.fulfill\(/u,
    "the browser gate must not intercept or fulfill measured requests through Playwright",
  );
  assert.match(
    performanceTest,
    /server\.closeAllConnections\(\)/u,
    "the browser gate must force-close Chromium keepalive connections during cleanup",
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
  assert.match(
    performanceWorkflow,
    /node_version="\$\(awk '\$1 == "nodejs" \{ print \$2 \}' \.tool-versions\)"/u,
    "the performance workflow must extract the pinned Node.js version with valid awk quoting",
  );
  assert.match(
    performanceWorkflow,
    /rust_version="\$\(awk '\$1 == "rust" \{ print \$2 \}' \.tool-versions\)"/u,
    "the performance workflow must extract the pinned Rust version with valid awk quoting",
  );
  assert.match(
    performanceWorkflow,
    /test -n "\$node_version"[\s\S]*test -n "\$rust_version"/u,
    "the performance workflow must reject empty toolchain pins",
  );
  assert.match(
    performanceWorkflow,
    /test "\$\(node --version\)" = "v24\.12\.0"[\s\S]*test "\$\(npm --version\)" = "11\.6\.2"[\s\S]*test "\$\(rustc --version \| awk '\{ print \$2 \}'\)" = "1\.95\.0"/u,
    "the performance workflow must verify the installed Node.js, npm, and Rust versions",
  );
  assert.match(
    integrationWorkflow,
    /uses: fermyon\/actions\/spin\/setup@v1[\s\S]{0,100}version: "v4\.0\.2"/u,
    "the integration workflow must retain Spin's required v-prefixed version",
  );
  for (const job of [
    "prepare-artifacts",
    "integration-tests",
    "integration-tests-fastly-ec",
    "aps-runner-proxy",
    "browser-tests",
    "browser-tests-aps-tsjs-conformance",
  ]) {
    assert.match(
      integrationWorkflow,
      new RegExp(
        `^  ${job}:\\n(?:    .+\\n)*?    if: github\\.event_name == 'pull_request'$`,
        "mu",
      ),
      `${job} must stay PR-only so manual evidence dispatches run the immutable performance job once`,
    );
  }
  assert.deepEqual(
    browserTestScript.match(/^cd .+$/gmu),
    ['cd "$REPO_ROOT"'],
    "the browser test launcher must remain at the repository root",
  );
  assert.equal(
    browserTestScript.match(/Building TSJS browser fixtures/gu)?.length,
    1,
    "the browser test launcher must build TSJS fixtures exactly once",
  );
  assert.match(
    browserTestScript,
    /npm --prefix "\$BROWSER_DIR" exec --[\s\S]*--config "\$REPO_ROOT\/\$BROWSER_DIR\/playwright\.config\.ts"/u,
    "the browser test launcher must pass Playwright an absolute config path",
  );
  assert.match(
    browserTestScript,
    /export ARTIFACTS_DIR="\$\{ARTIFACTS_DIR:-\$REPO_ROOT\/target\/integration-test-artifacts\}"/u,
    "the browser test launcher must establish one effective artifacts directory",
  );
  assert.match(
    browserTestScript,
    /GENERATED_VICEROY_CONFIG_PATH="\$ARTIFACTS_DIR\/configs\/viceroy\.toml"/u,
    "the browser test launcher must consume the config generated in the effective artifacts directory",
  );
  assert.match(
    apsProxyScript,
    /ARTIFACTS_DIR="\$\{ARTIFACTS_DIR:-\$REPO_ROOT\/target\/integration-test-artifacts\}"/u,
    "the APS proxy launcher must establish one effective artifacts directory",
  );
  assert.match(
    apsProxyScript,
    /VICEROY_CONFIG_PATH="\$ARTIFACTS_DIR\/configs\/viceroy\.toml"/u,
    "the APS proxy launcher must use the generator's effective artifacts directory",
  );
  assert.doesNotMatch(
    generalTestWorkflow,
    /tsjs-performance-gate\.yml/u,
    "general CI must not redefine or automatically rerun immutable performance evidence",
  );
  assert.match(
    generalTestWorkflow,
    /test-typescript:[\s\S]*?uses: actions\/checkout@v4\n        with:\n          fetch-depth: 0/u,
    "the rc/july adoption contract must receive the pinned baseline commit",
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
  for (const name of [
    "--file",
    "--evidence-id",
    "--head-sha",
    "--main-sha",
    "--mode",
  ]) {
    if (!values.has(name)) fail(`missing ${name}`);
  }
  if (values.size !== 5) fail("unknown or duplicate CLI arguments");
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
    mainSha: arguments_.get("--main-sha"),
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
