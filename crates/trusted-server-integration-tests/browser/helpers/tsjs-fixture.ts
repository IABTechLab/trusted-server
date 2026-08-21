import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import type { Page } from "@playwright/test";

const TSJS_CRATE = resolve(__dirname, "../../../trusted-server-js");
const DIST = resolve(TSJS_CRATE, "dist");

interface ReleaseArtifact {
  id: string;
  role: "bootstrap" | "core" | "integration";
  phase: "takeover" | "deferred" | null;
  file: string;
}

interface Release {
  version: 1;
  releaseId: string;
  artifacts: ReleaseArtifact[];
}

export interface RuntimeTsjsFixture {
  releaseId: string;
  bootstrapBody: string;
  runtimeBody: string;
  runtimeSrc: string;
  manifest: {
    version: 1;
    releaseId: string;
    firstDisplay: null;
    runtimeSrc: string;
    integrations: Array<{ id: string; phase: "takeover" }>;
  };
}

interface ServerBootDigestInputV1 {
  readonly auctionProjection: unknown;
  readonly integrations: unknown;
  readonly [key: string]: unknown;
}

/** Serialize the exact string-only server boot carrier consumed by bootstrap. */
export function serverBootTransportLiteralV1(
  boot: ServerBootDigestInputV1,
  outline: unknown = null,
): string {
  const projection = JSON.stringify(boot.auctionProjection);
  const integrationConfigs = JSON.stringify(boot.integrations);
  if (projection === undefined || integrationConfigs === undefined) {
    throw new Error("TSJS server boot digest input is not JSON");
  }
  const transport = JSON.stringify({
    version: 1,
    boot,
    integrity: {
      version: 1,
      projectionDigest: createHash("sha256")
        .update(projection, "utf8")
        .digest("hex"),
      integrationConfigDigest: createHash("sha256")
        .update(integrationConfigs, "utf8")
        .digest("hex"),
    },
    outline,
  });
  return JSON.stringify(transport);
}

function exactArtifact(release: Release, id: string): ReleaseArtifact {
  const matches = release.artifacts.filter((artifact) => artifact.id === id);
  if (matches.length !== 1)
    throw new Error(`expected one TSJS artifact for ${id}`);
  return matches[0]!;
}

/** Build the same content-addressed persistent-runtime response and manifest as production. */
export function runtimeTsjsFixture(ids: readonly string[]): RuntimeTsjsFixture {
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
    if (artifact.phase !== "takeover") {
      throw new Error(`TSJS fixture module ${artifact.id} is not takeover`);
    }
  }
  const runtimeBody = artifacts
    .map((artifact) => readFileSync(resolve(DIST, artifact.file), "utf8"))
    .join(";\n");
  const hash = createHash("sha256").update(runtimeBody, "utf8").digest("hex");
  const runtimeSrc = `/static/tsjs=tsjs-unified.min.js?v=${hash}`;
  const bootstrapBody = readFileSync(
    resolve(DIST, exactArtifact(release, "bootstrap").file),
    "utf8",
  );
  return {
    releaseId: release.releaseId,
    bootstrapBody,
    runtimeBody,
    runtimeSrc,
    manifest: {
      version: 1,
      releaseId: release.releaseId,
      firstDisplay: null,
      runtimeSrc,
      integrations: ids.map((id) => ({ id, phase: "takeover" as const })),
    },
  };
}

/** Serve a fixture from the exact content-addressed production route. */
export async function routeRuntimeTsjsFixture(
  page: Page,
  fixture: RuntimeTsjsFixture,
): Promise<void> {
  const absoluteUrl = new URL(fixture.runtimeSrc, page.url()).toString();
  await page.route(absoluteUrl, (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/javascript; charset=utf-8",
      headers: { "x-content-type-options": "nosniff" },
      body: fixture.runtimeBody,
    }),
  );
}

/** Execute a fixture through the authenticated parser-equivalent script boundary. */
export async function loadRuntimeTsjsFixture(
  page: Page,
  fixture: RuntimeTsjsFixture,
): Promise<void> {
  await routeRuntimeTsjsFixture(page, fixture);
  const boot = await page.evaluate(
    () => (window as unknown as { tsjs: Record<string, unknown> }).tsjs.boot,
  );
  if (typeof boot !== "object" || boot === null || Array.isArray(boot)) {
    throw new Error("TSJS runtime fixture boot is unavailable");
  }
  await page.addScriptTag({
    content: `const __TSJS_SERVER_BOOT_TRANSPORT_V1__=${serverBootTransportLiteralV1(boot as ServerBootDigestInputV1)};${fixture.bootstrapBody}`,
  });
  await page.evaluate(
    (src) =>
      new Promise<void>((resolveLoad, rejectLoad) => {
        const script = document.createElement("script");
        script.id = "trustedserver-js";
        script.src = src;
        script.addEventListener("load", () => resolveLoad(), { once: true });
        script.addEventListener(
          "error",
          () =>
            rejectLoad(
              new Error("persistent TSJS runtime fixture did not load"),
            ),
          { once: true },
        );
        document.head.appendChild(script);
      }),
    fixture.runtimeSrc,
  );
}
