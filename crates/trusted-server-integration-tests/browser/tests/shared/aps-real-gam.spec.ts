import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { expect, test } from "@playwright/test";
import {
  REAL_GAM_CASES,
  getRealGamEnvironment,
  runAttestedRealGamCase,
} from "../../helpers/gam-test-network.js";

const browserPackage = resolve(__dirname, "../..");
const realGamConfig = resolve(browserPackage, "playwright.real-gam.config.ts");

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

  for (const contract of REAL_GAM_CASES) {
    test(`${contract.caseId} satisfies its attested contract`, async ({
      page,
      browserName,
    }, testInfo) => {
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
