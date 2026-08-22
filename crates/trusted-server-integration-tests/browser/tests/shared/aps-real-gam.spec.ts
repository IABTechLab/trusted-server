import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { expect, test } from "@playwright/test";
import {
  REAL_GAM_CASES,
  REAL_GAM_PUC_RELEASE,
  classifyRealGamNetworkUrl,
  getRealGamEnvironment,
  openProtectedRealGamPage,
  runAttestedRealGamCase,
} from "../../helpers/gam-test-network.js";

const browserPackage = resolve(__dirname, "../..");
const realGamConfig = resolve(browserPackage, "playwright.real-gam.config.ts");
const browserPackageJson = resolve(browserPackage, "package.json");
const browserPackageLock = resolve(browserPackage, "package-lock.json");
const realGamConfigured = [
  process.env.TS_REAL_GAM_PAGE_URL,
  process.env.TS_REAL_GAM_AUTH_HEADER,
  process.env.TS_REAL_GAM_EXPECTED_RELEASE_ID,
].every(Boolean);

test.describe("protected real-GAM test network", () => {
  test.describe.configure({ mode: "serial" });

  test("the dedicated config cannot acquire local test infrastructure", () => {
    const source = readFileSync(realGamConfig, "utf8");

    expect(source).not.toMatch(/from\s+["'].\/playwright\.config/u);
    expect(source).not.toContain("globalSetup");
    expect(source).not.toContain("globalTeardown");
    expect(source).not.toMatch(/viceroy|docker|wasm/iu);
    expect(source).toContain('testMatch: "aps-real-gam.spec.ts"');
  });

  test("the protected contract enumerates every required flow exactly once", () => {
    const caseIds = REAL_GAM_CASES.map(({ caseId }) => caseId);
    expect(caseIds).toEqual([
      "ssat-aps-puc",
      "trusted-server-prebid-aps-puc",
      "page-bids-aps-puc",
      "direct-aps",
      "direct-adm",
      "direct-cache",
      "attributable-empty-gam-fallback",
      "sra-aps-puc",
      "refresh-aps-puc",
      "spa-navigation",
      "gpt-handoff",
      "hydrated-dom-replacement",
      "collapsed-shell-resize",
      "wrong-id",
      "wrong-source",
      "invalid-descriptor",
      "no-outer-claim",
      "no-owner-registration",
      "no-document-ack",
      "aps-runner-failure",
    ]);
    expect(new Set(caseIds).size).toBe(caseIds.length);
  });

  test("the network classifier recognizes only the live APS runner route", () => {
    const pageOrigin = "https://real-gam.example";

    expect(
      classifyRealGamNetworkUrl(
        `${pageOrigin}/integrations/aps/runner.js?slot=leaderboard`,
        pageOrigin,
      ),
    ).toBe("runner-proxy");
    expect(
      classifyRealGamNetworkUrl(
        `${pageOrigin}/integrations/aps/runner/v1.js?slot=leaderboard`,
        pageOrigin,
      ),
    ).toBe("first-party");
    expect(
      classifyRealGamNetworkUrl(
        "https://third-party.example/integrations/aps/runner.js",
        pageOrigin,
      ),
    ).toBe("third-party");
    expect(
      classifyRealGamNetworkUrl(
        "https://third-party.example/integrations/aps/renderer/v2",
        pageOrigin,
      ),
    ).toBe("third-party");
  });

  test("the protected contract pins the externally hosted PUC release", () => {
    expect(REAL_GAM_PUC_RELEASE).toBe("1.17.2");
  });

  test("PUC remains external to the locally authored browser harness", () => {
    const packageJson = readFileSync(browserPackageJson, "utf8");
    const packageLock = readFileSync(browserPackageLock, "utf8");

    expect(packageJson).not.toContain("prebid-universal-creative");
    expect(packageLock).not.toContain("prebid-universal-creative");
  });

  test("the protected contract rejects an unpinned PUC release", async ({
    page,
  }) => {
    const pageUrl = "https://real-gam.example/contract";
    const expectedReleaseId = "a".repeat(64);

    await page.route(pageUrl, (route) =>
      route.fulfill({ status: 200, contentType: "text/html", body: "<!doctype html>" }),
    );
    await page.addInitScript(
      ({ caseIds, releaseId }) => {
        Object.defineProperty(window, "__tsRealGamTestNetwork", {
          value: Object.freeze({
            version: 1,
            releaseId,
            pucRelease: "latest",
            caseIds: Object.freeze(caseIds),
            run: () => undefined,
          }),
        });
      },
      {
        caseIds: REAL_GAM_CASES.map(({ caseId }) => caseId),
        releaseId: expectedReleaseId,
      },
    );

    await expect(
      openProtectedRealGamPage(page, {
        pageUrl,
        authorization: "not-used-by-this-contract-test",
        expectedReleaseId,
      }),
    ).rejects.toThrow(/exact frozen test-network contract/u);
  });

  for (const contract of REAL_GAM_CASES) {
    test(`${contract.caseId} satisfies its attested contract`, async ({
      page,
      browserName,
    }, testInfo) => {
      test.skip(
        !realGamConfigured,
        "protected real-GAM cases run only through playwright.real-gam.config.ts",
      );
      test.setTimeout(contract.deadlineMs + 30_000);
      await runAttestedRealGamCase({
        page,
        browserName,
        testInfo,
        environment: getRealGamEnvironment(),
        contract,
      });
    });
  }
});
