import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import type { Page } from "@playwright/test";

const TSJS_CRATE = resolve(__dirname, "../../../trusted-server-js");
const DIST = resolve(TSJS_CRATE, "dist");

interface ReleaseArtifact {
  id: string;
  role: "bootstrap" | "core" | "integration";
  phase: "critical" | "deferred" | null;
  file: string;
}

interface Release {
  version: 1;
  releaseId: string;
  artifacts: ReleaseArtifact[];
}

export interface CriticalTsjsFixture {
  releaseId: string;
  criticalBody: string;
  criticalSrc: string;
  manifest: {
    version: 1;
    releaseId: string;
    criticalSrc: string;
    integrations: Array<{ id: string; phase: "critical" }>;
  };
}

function exactArtifact(release: Release, id: string): ReleaseArtifact {
  const matches = release.artifacts.filter((artifact) => artifact.id === id);
  if (matches.length !== 1) throw new Error(`expected one TSJS artifact for ${id}`);
  return matches[0]!;
}

/** Build the same content-addressed critical response and manifest as production. */
export function criticalTsjsFixture(ids: readonly string[]): CriticalTsjsFixture {
  const release = JSON.parse(
    readFileSync(resolve(DIST, "tsjs-release-v1.json"), "utf8"),
  ) as Release;
  if (release.version !== 1 || !/^[0-9a-f]{64}$/u.test(release.releaseId)) {
    throw new Error("TSJS release manifest is invalid");
  }
  const artifacts = [exactArtifact(release, "core")].concat(
    ids.map((id) => exactArtifact(release, id)),
  );
  for (const artifact of artifacts.slice(1)) {
    if (artifact.phase !== "critical") {
      throw new Error(`TSJS fixture module ${artifact.id} is not critical`);
    }
  }
  const criticalBody = artifacts
    .map((artifact) => readFileSync(resolve(DIST, artifact.file), "utf8"))
    .join(";\n");
  const hash = createHash("sha256").update(criticalBody, "utf8").digest("hex");
  const criticalSrc = `/static/tsjs=tsjs-unified.min.js?v=${hash}`;
  return {
    releaseId: release.releaseId,
    criticalBody,
    criticalSrc,
    manifest: {
      version: 1,
      releaseId: release.releaseId,
      criticalSrc,
      integrations: ids.map((id) => ({ id, phase: "critical" as const })),
    },
  };
}

/** Serve a fixture from the exact content-addressed production route. */
export async function routeCriticalTsjsFixture(
  page: Page,
  fixture: CriticalTsjsFixture,
): Promise<void> {
  const absoluteUrl = new URL(fixture.criticalSrc, page.url()).toString();
  await page.route(absoluteUrl, (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/javascript; charset=utf-8",
      headers: { "x-content-type-options": "nosniff" },
      body: fixture.criticalBody,
    }),
  );
}

/** Execute a fixture through the authenticated parser-equivalent script boundary. */
export async function loadCriticalTsjsFixture(
  page: Page,
  fixture: CriticalTsjsFixture,
): Promise<void> {
  await routeCriticalTsjsFixture(page, fixture);
  await page.evaluate(
    (src) =>
      new Promise<void>((resolveLoad, rejectLoad) => {
        const script = document.createElement("script");
        script.id = "trustedserver-js";
        script.src = src;
        script.addEventListener("load", () => resolveLoad(), { once: true });
        script.addEventListener(
          "error",
          () => rejectLoad(new Error("critical TSJS fixture did not load")),
          { once: true },
        );
        document.head.appendChild(script);
      }),
    fixture.criticalSrc,
  );
}
