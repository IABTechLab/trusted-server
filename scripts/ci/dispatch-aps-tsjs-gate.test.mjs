import assert from "node:assert/strict";
import test from "node:test";

import {
  createEvidenceId,
  dispatchGate,
  selectUniqueWorkflowRun,
} from "./dispatch-aps-tsjs-gate.mjs";

const HEAD = "a".repeat(40);
const BASE = "b".repeat(40);
const START = new Date("2026-08-20T12:00:00.000Z");

function fakeRunner(responses) {
  const calls = [];
  return {
    calls,
    async run(command, args, options = {}) {
      calls.push({ command, args, options });
      const key = [command, ...args].join(" ");
      const response = responses.get(key);
      if (response instanceof Error) throw response;
      if (response === undefined) return "";
      return typeof response === "function" ? response(calls) : response;
    },
  };
}

test("evidence ids bind gate, mode, head, UTC time, and random suffix", () => {
  const first = createEvidenceId({
    kind: "performance",
    mode: "postswitch",
    headSha: HEAD,
    now: START,
    randomSuffix: "0123abcd",
  });
  const second = createEvidenceId({
    kind: "performance",
    mode: "postswitch",
    headSha: HEAD,
    now: START,
    randomSuffix: "fedcba98",
  });
  assert.equal(
    first,
    "aps-tsjs-performance-postswitch-aaaaaaaaaaaa-20260820T120000Z-0123abcd",
  );
  assert.notEqual(first, second);
  assert.match(first, /^[A-Za-z0-9][A-Za-z0-9._-]{7,127}$/u);
});

test("run selection rejects stale, ambiguous, and wrong-head evidence", () => {
  const expected = {
    evidenceId:
      "aps-tsjs-performance-postswitch-aaaaaaaaaaaa-20260820T120000Z-0123abcd",
    headSha: HEAD,
    displayTitle:
      "TSJS Performance Gate / aps-tsjs-performance-postswitch-aaaaaaaaaaaa-20260820T120000Z-0123abcd / postswitch",
    dispatchStartedAt: START,
  };
  assert.equal(
    selectUniqueWorkflowRun(
      [
        {
          databaseId: 1,
          displayTitle: "unrelated",
          headSha: HEAD,
          createdAt: START.toISOString(),
        },
      ],
      expected,
    ),
    undefined,
  );
  assert.equal(
    selectUniqueWorkflowRun(
      [
        {
          databaseId: 2,
          displayTitle: expected.displayTitle,
          headSha: BASE,
          createdAt: START.toISOString(),
        },
      ],
      expected,
    ),
    undefined,
  );
  assert.equal(
    selectUniqueWorkflowRun(
      [
        {
          databaseId: 22,
          displayTitle: `${expected.displayTitle}-not-exact`,
          headSha: HEAD,
          createdAt: START.toISOString(),
        },
      ],
      expected,
    ),
    undefined,
  );
  assert.throws(
    () =>
      selectUniqueWorkflowRun(
        [
          {
            databaseId: 3,
            displayTitle: expected.displayTitle,
            headSha: HEAD,
            createdAt: "2026-08-20T11:59:59.999Z",
          },
        ],
        expected,
      ),
    /stale/u,
  );
  assert.throws(
    () =>
      selectUniqueWorkflowRun(
        [4, 5].map((databaseId) => ({
          databaseId,
          displayTitle: expected.displayTitle,
          headSha: HEAD,
          createdAt: START.toISOString(),
        })),
        expected,
      ),
    /multiple/u,
  );
});

test("performance dispatch binds the remote head, exact base, artifact, and validator", async () => {
  const ref = "feature/aps-tsjs-resilience-rc202608";
  const evidenceId =
    "aps-tsjs-performance-postswitch-aaaaaaaaaaaa-20260820T120000Z-0123abcd";
  const run = {
    databaseId: 42,
    displayTitle: `TSJS Performance Gate / ${evidenceId} / postswitch`,
    headSha: HEAD,
    createdAt: START.toISOString(),
  };
  const runner = fakeRunner(
    new Map([
      ["git status --porcelain=v1 --untracked-files=all", ""],
      ["git rev-parse HEAD", `${HEAD}\n`],
      [
        `git ls-remote --heads origin refs/heads/${ref}`,
        `${HEAD}\trefs/heads/${ref}\n`,
      ],
      [
        "gh run list --workflow .github/workflows/tsjs-performance-gate.yml --branch feature/aps-tsjs-resilience-rc202608 --event workflow_dispatch --limit 100 --json databaseId,displayTitle,headSha,createdAt",
        JSON.stringify([run]),
      ],
    ]),
  );

  const result = await dispatchGate(
    {
      kind: "performance",
      ref,
      baseSha: BASE,
      mode: "postswitch",
      outputDir: "target/performance-evidence",
    },
    {
      runner: runner.run,
      now: () => new Date("2026-08-20T12:00:00.999Z"),
      randomSuffix: () => "0123abcd",
      sleep: async () => {},
      pathExists: () => false,
    },
  );

  assert.deepEqual(result, { evidenceId, headSha: HEAD, runId: 42 });
  const commands = runner.calls.map(({ command, args }) => [command, ...args]);
  assert.ok(
    commands.some(
      (command) =>
        command.join(" ") ===
        `gh workflow run .github/workflows/tsjs-performance-gate.yml --ref ${ref} -f evidence_id=${evidenceId} -f mode=postswitch -f base_sha=${BASE}`,
    ),
  );
  assert.ok(
    commands.some(
      (command) =>
        command.join(" ") ===
        `gh run download 42 --name tsjs-performance-${evidenceId} --dir target/performance-evidence`,
    ),
  );
  assert.ok(
    commands.some(
      (command) =>
        command.join(" ") ===
        `node scripts/validate-tsjs-performance-evidence.mjs --file target/performance-evidence/tsjs-performance-postswitch.json --evidence-id ${evidenceId} --head-sha ${HEAD} --base-sha ${BASE} --mode postswitch`,
    ),
  );
});

test("remote-head mismatch fails before any workflow dispatch", async () => {
  const ref = "feature/aps-tsjs-resilience-rc202608";
  const runner = fakeRunner(
    new Map([
      ["git status --porcelain=v1 --untracked-files=all", ""],
      ["git rev-parse HEAD", `${HEAD}\n`],
      [
        `git ls-remote --heads origin refs/heads/${ref}`,
        `${BASE}\trefs/heads/${ref}\n`,
      ],
    ]),
  );
  await assert.rejects(
    dispatchGate(
      {
        kind: "performance",
        ref,
        baseSha: BASE,
        mode: "postswitch",
        outputDir: "target/performance-evidence",
      },
      { runner: runner.run, pathExists: () => false },
    ),
    /remote branch must resolve to local HEAD/u,
  );
  assert.equal(
    runner.calls.some(({ command }) => command === "gh"),
    false,
  );
});

test("zero matching workflow runs are rejected after bounded polling", async () => {
  const ref = "feature/aps-tsjs-resilience-rc202608";
  const runner = fakeRunner(
    new Map([
      ["git status --porcelain=v1 --untracked-files=all", ""],
      ["git rev-parse HEAD", `${HEAD}\n`],
      [
        `git ls-remote --heads origin refs/heads/${ref}`,
        `${HEAD}\trefs/heads/${ref}\n`,
      ],
      [
        "gh run list --workflow .github/workflows/tsjs-performance-gate.yml --branch feature/aps-tsjs-resilience-rc202608 --event workflow_dispatch --limit 100 --json databaseId,displayTitle,headSha,createdAt",
        "[]",
      ],
    ]),
  );
  await assert.rejects(
    dispatchGate(
      {
        kind: "performance",
        ref,
        baseSha: BASE,
        mode: "postswitch",
        outputDir: "target/performance-evidence",
      },
      {
        runner: runner.run,
        now: () => START,
        randomSuffix: () => "0123abcd",
        sleep: async () => {},
        pathExists: () => false,
      },
    ),
    /zero workflow runs/u,
  );
  assert.equal(
    runner.calls.filter(
      ({ command, args }) =>
        command === "gh" && args[0] === "run" && args[1] === "list",
    ).length,
    30,
  );
});

test("real-GAM dispatch derives release id and validates the exact run artifact", async () => {
  const ref = "feature/aps-tsjs-resilience-rc202608";
  const releaseId = "c".repeat(64);
  const previousArtifactId = "rollback-artifact-123";
  const evidenceId =
    "aps-tsjs-real-gam-cutover-aaaaaaaaaaaa-20260820T120000Z-0123abcd";
  const runner = fakeRunner(
    new Map([
      ["git status --porcelain=v1 --untracked-files=all", ""],
      ["git rev-parse HEAD", `${HEAD}\n`],
      [
        `git ls-remote --heads origin refs/heads/${ref}`,
        `${HEAD}\trefs/heads/${ref}\n`,
      ],
      [
        "npm --prefix crates/trusted-server-js/lib run --silent print:release-id",
        `${releaseId}\n`,
      ],
      [
        "gh run list --workflow .github/workflows/aps-real-gam.yml --branch feature/aps-tsjs-resilience-rc202608 --event workflow_dispatch --limit 100 --json databaseId,displayTitle,headSha,createdAt",
        JSON.stringify([
          {
            databaseId: 77,
            displayTitle: `APS real-GAM / ${evidenceId} / ${releaseId}`,
            headSha: HEAD,
            createdAt: START.toISOString(),
          },
        ]),
      ],
    ]),
  );

  await dispatchGate(
    {
      kind: "real-gam",
      ref,
      previousArtifactId,
      outputDir: "target/real-gam-evidence",
    },
    {
      runner: runner.run,
      now: () => START,
      randomSuffix: () => "0123abcd",
      sleep: async () => {},
      pathExists: () => false,
    },
  );
  const commands = runner.calls.map(({ command, args }) =>
    [command, ...args].join(" "),
  );
  assert.ok(
    commands.includes(
      `gh workflow run .github/workflows/aps-real-gam.yml --ref ${ref} -f evidence_id=${evidenceId} -f release_id=${releaseId} -f previous_artifact_id=${previousArtifactId}`,
    ),
  );
  assert.ok(
    commands.includes(
      "gh run download 77 --name aps-real-gam-77 --dir target/real-gam-evidence",
    ),
  );
  assert.ok(
    commands.includes(
      `node scripts/ci/aps-tsjs-evidence.mjs validate-real-gam target/real-gam-evidence ${evidenceId} ${releaseId} ${HEAD} 77 ${previousArtifactId}`,
    ),
  );
});
