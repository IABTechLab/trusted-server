import { execFileSync } from "node:child_process";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, isAbsolute, resolve } from "node:path";
import { test, expect, type Browser, type Page } from "@playwright/test";

const REPO_ROOT = execFileSync("git", ["rev-parse", "--show-toplevel"], {
  encoding: "utf8",
}).trim();
const TSJS_CRATE = resolve(REPO_ROOT, "crates/trusted-server-js");
const CORE_BUNDLE = resolve(TSJS_CRATE, "dist/tsjs-core.js");
const BUILD_METRICS = resolve(TSJS_CRATE, "dist/tsjs-build-metrics-v1.json");
const WARMUPS = 5;
const SAMPLES = 50;
const PERCENTILE = 90;

type HeapCheckpoint =
  | "afterBoot"
  | "afterFirstRender"
  | "afterRefresh"
  | "afterSpaNavigation";

interface PerfApi {
  addAdUnits(unit: {
    code: string;
    mediaTypes: { banner: { sizes: Array<[number, number]> } };
  }): void;
  renderAdUnit(code: string): void;
}

function fixtureDocument(): string {
  return `<!doctype html>
<meta charset="utf-8">
<title>TSJS deterministic performance fixture v1</title>
<div id="perf-slot"></div>
<script>
  window.__tsjsPerf = { bootStartedAt: performance.now(), firstDisplayAt: null };
  new MutationObserver(function recordFirstDisplay() {
    if (window.__tsjsPerf.firstDisplayAt === null) {
      window.__tsjsPerf.firstDisplayAt = performance.now();
    }
  }).observe(document.getElementById("perf-slot"), {
    childList: true,
    characterData: true,
    subtree: true
  });
</script>`;
}

async function openFixture(
  browser: Browser,
): Promise<{ page: Page; close(): Promise<void> }> {
  const context = await browser.newContext();
  const page = await context.newPage();
  await page.setContent(fixtureDocument(), { waitUntil: "load" });
  await page.addScriptTag({ path: CORE_BUNDLE });
  return { page, close: () => context.close() };
}

async function render(page: Page): Promise<number> {
  return page.evaluate(() => {
    const perfWindow = window as unknown as {
      tsjs: PerfApi;
      __tsjsPerf: { bootStartedAt: number; firstDisplayAt: number | null };
    };
    perfWindow.tsjs.addAdUnits({
      code: "perf-slot",
      mediaTypes: { banner: { sizes: [[300, 250]] } },
    });
    perfWindow.tsjs.renderAdUnit("perf-slot");
    const firstDisplayAt =
      perfWindow.__tsjsPerf.firstDisplayAt ?? performance.now();
    return firstDisplayAt - perfWindow.__tsjsPerf.bootStartedAt;
  });
}

async function collectHeap(page: Page): Promise<number> {
  const session = await page.context().newCDPSession(page);
  try {
    await session.send("HeapProfiler.collectGarbage");
    const usage = await session.send("Runtime.getHeapUsage");
    return usage.usedSize as number;
  } finally {
    await session.detach();
  }
}

function p90(values: number[]): number {
  const ordered = [...values].sort((left, right) => left - right);
  return ordered[Math.ceil((PERCENTILE / 100) * ordered.length) - 1]!;
}

function packageVersion(packagePath: string): string {
  const packageJson = JSON.parse(readFileSync(packagePath, "utf8")) as {
    version: string;
  };
  return packageJson.version;
}

test.describe("TSJS deterministic performance evidence", () => {
  test("records bundle, p90 display, and forced-GC heap baselines", async ({
    browser,
    browserName,
  }) => {
    test.setTimeout(180_000);
    const mode = process.env.TSJS_PERF_MODE;
    test.skip(
      mode !== "baseline" && mode !== "gate",
      "performance evidence run only",
    );
    expect(browserName).toBe("chromium");

    for (let index = 0; index < WARMUPS; index += 1) {
      const fixture = await openFixture(browser);
      try {
        await render(fixture.page);
      } finally {
        await fixture.close();
      }
    }

    const displaySamplesMs: number[] = [];
    for (let index = 0; index < SAMPLES; index += 1) {
      const fixture = await openFixture(browser);
      try {
        displaySamplesMs.push(await render(fixture.page));
      } finally {
        await fixture.close();
      }
    }

    const heapFixture = await openFixture(browser);
    const retainedHeapBytes = {} as Record<HeapCheckpoint, number>;
    try {
      retainedHeapBytes.afterBoot = await collectHeap(heapFixture.page);
      await render(heapFixture.page);
      retainedHeapBytes.afterFirstRender = await collectHeap(heapFixture.page);
      await heapFixture.page.evaluate(() => {
        const perfWindow = window as unknown as { tsjs: PerfApi };
        perfWindow.tsjs.renderAdUnit("perf-slot");
      });
      retainedHeapBytes.afterRefresh = await collectHeap(heapFixture.page);
      await heapFixture.page.evaluate(() => {
        location.hash = "performance-fixture-navigation";
        const oldSlot = document.getElementById("perf-slot");
        const replacement = document.createElement("div");
        replacement.id = "perf-slot";
        oldSlot?.replaceWith(replacement);
        const perfWindow = window as unknown as { tsjs: PerfApi };
        perfWindow.tsjs.renderAdUnit("perf-slot");
      });
      retainedHeapBytes.afterSpaNavigation = await collectHeap(
        heapFixture.page,
      );
    } finally {
      await heapFixture.close();
    }

    const outputArgument = process.env.TSJS_PERF_OUTPUT;
    expect(outputArgument, "TSJS_PERF_OUTPUT is required").toBeTruthy();
    const outputPath = isAbsolute(outputArgument!)
      ? outputArgument!
      : resolve(REPO_ROOT, outputArgument!);
    const buildMetrics = JSON.parse(readFileSync(BUILD_METRICS, "utf8")) as {
      schemaVersion: number;
      sets: Record<string, unknown>;
    };
    expect(buildMetrics.schemaVersion).toBe(1);

    const npmVersion = execFileSync("npm", ["--version"], {
      encoding: "utf8",
    }).trim();
    const artifact = {
      schemaVersion: 1,
      mode,
      source: {
        ref: execFileSync("git", ["branch", "--show-current"], {
          cwd: REPO_ROOT,
          encoding: "utf8",
        }).trim(),
        sha: execFileSync("git", ["rev-parse", "HEAD"], {
          cwd: REPO_ROOT,
          encoding: "utf8",
        }).trim(),
      },
      environment: {
        node: process.version,
        npm: npmVersion,
        typescript: packageVersion(
          resolve(
            REPO_ROOT,
            "crates/trusted-server-js/lib/node_modules/typescript/package.json",
          ),
        ),
        chromium: browser.version(),
        ciMachineClass:
          process.env.TSJS_PERF_MACHINE_CLASS ??
          (process.env.CI
            ? "github-hosted:ubuntu-latest"
            : `local:${process.platform}-${process.arch}`),
        fixture: "tsjs-core-placeholder-v1",
      },
      sampling: {
        warmups: WARMUPS,
        samples: SAMPLES,
        percentile: PERCENTILE,
      },
      bundles: buildMetrics.sets,
      performance: {
        bootToFirstDisplayMs: {
          samples: displaySamplesMs,
          p90: p90(displaySamplesMs),
        },
        retainedHeapBytes,
      },
      evidence: {
        evidenceId: process.env.TSJS_EVIDENCE_ID ?? null,
        workflowRunId: process.env.GITHUB_RUN_ID
          ? Number(process.env.GITHUB_RUN_ID)
          : null,
      },
    };

    mkdirSync(dirname(outputPath), { recursive: true });
    writeFileSync(outputPath, `${JSON.stringify(artifact, null, 2)}\n`);
    expect(displaySamplesMs).toHaveLength(SAMPLES);
    expect(Object.values(retainedHeapBytes)).toHaveLength(4);
  });
});
