import { expect, test, type Page } from "@playwright/test";
import {
  firstDisplayTsjsFixture,
  runtimeTsjsFixture,
  serverBootTransportLiteralV1,
} from "../../helpers/tsjs-fixture.js";

const NONCE = "tsjs-policy-nonce";
const POLICY_SLOT = "policy-slot";
const DIRECT = runtimeTsjsFixture(["render_runtime"]);
const DEFERRED = firstDisplayTsjsFixture({
  firstDisplayIds: ["first_display"],
  takeoverIds: ["render_runtime"],
  deferredIds: ["diagnostics_presentation"],
  auctionProjection: {
    version: 1,
    auction: {
      version: 1,
      auctionId: "policy",
      results: [{ slot: POLICY_SLOT, outcome: "no_bid" }],
    },
    slots: [
      {
        slot: POLICY_SLOT,
        gamUnitPath: `/123/${POLICY_SLOT}`,
        divId: POLICY_SLOT,
        formats: [[300, 250]],
        targeting: {},
      },
    ],
    bids: [],
  },
  integrations: { version: 1, entries: [] },
  diagnostics: {
    version: 1,
    renderTraceOverlay: true,
    gpt: { active: false },
  },
});

function directBoot() {
  return {
    abi: 1,
    releaseId: DIRECT.releaseId,
    manifest: DIRECT.manifest,
    auctionProjection: {
      version: 1,
      auction: {
        version: 1,
        auctionId: "policy-direct",
        results: [{ slot: POLICY_SLOT, outcome: "no_bid" }],
      },
      slots: [
        {
          slot: POLICY_SLOT,
          gamUnitPath: `/123/${POLICY_SLOT}`,
          divId: POLICY_SLOT,
          formats: [[300, 250]],
          targeting: {},
        },
      ],
      bids: [],
    },
    integrations: { version: 1, entries: [] },
    creative: {
      version: 1,
      enabled: false,
      clickGuard: false,
      renderGuard: false,
    },
    diagnostics: {
      version: 1,
      renderTraceOverlay: false,
      gpt: { active: false },
    },
  };
}

function deferredBoot() {
  return {
    ...DEFERRED.boot,
    manifest: {
      ...(DEFERRED.boot.manifest as Record<string, unknown>),
      firstDisplay: null,
    },
  };
}

interface PolicyPageOptions {
  readonly path: string;
  readonly csp: string;
  readonly nonce?: string;
  readonly deferred?: boolean;
  readonly takeover?: boolean;
  readonly prelude?: string;
  readonly tail?: string;
  readonly removeDeferred?: boolean;
}

async function servePolicyPage(page: Page, options: PolicyPageOptions) {
  const fixture = options.deferred ? DEFERRED : DIRECT;
  const boot = options.takeover
    ? DEFERRED.boot
    : options.deferred
      ? deferredBoot()
      : directBoot();
  const runtimeUrl = new URL(
    fixture.runtimeSrc,
    "https://runtime.test",
  ).toString();
  const firstDisplayUrl = options.takeover
    ? new URL(DEFERRED.firstDisplaySrc, "https://runtime.test").toString()
    : undefined;
  const deferredResource = options.deferred ? DEFERRED.deferred[0] : undefined;
  const deferredUrl = deferredResource
    ? new URL(deferredResource.src, "https://runtime.test").toString()
    : undefined;
  const runtimeRequests: string[] = [];
  const deferredRequests: string[] = [];
  const routeHits = { deferred: 0, firstDisplay: 0, runtime: 0 };
  page.on("request", (request) => {
    if (request.url() === runtimeUrl) runtimeRequests.push(request.url());
    if (request.url() === deferredUrl) deferredRequests.push(request.url());
  });
  await page.route(runtimeUrl, (route) => {
    routeHits.runtime += 1;
    return route.fulfill({
      status: 200,
      contentType: "application/javascript; charset=utf-8",
      headers: { "x-content-type-options": "nosniff" },
      body: fixture.runtimeBody,
    });
  });
  if (firstDisplayUrl) {
    await page.route(firstDisplayUrl, (route) => {
      routeHits.firstDisplay += 1;
      return route.fulfill({
        status: 200,
        contentType: "application/javascript; charset=utf-8",
        headers: { "x-content-type-options": "nosniff" },
        body: DEFERRED.firstDisplayBody,
      });
    });
  }
  if (deferredResource && deferredUrl) {
    await page.route(deferredUrl, (route) => {
      routeHits.deferred += 1;
      return route.fulfill({
        status: 200,
        contentType: "application/javascript; charset=utf-8",
        headers: { "x-content-type-options": "nosniff" },
        body: deferredResource.body,
      });
    });
  }
  const nonce = options.nonce ? ` nonce="${options.nonce}"` : "";
  const probe = options.deferred
    ? `window.__policyProbe={insertions:0,removals:0};const __policyObserver=new MutationObserver(records=>{for(const record of records){for(const node of record.addedNodes){if(!(node instanceof HTMLScriptElement)||!node.src.includes("tsjs-diagnostics_presentation.min.js"))continue;window.__policyProbe.insertions+=1;${options.removeDeferred ? 'node.replaceWith(document.createElement("script"));window.__policyProbe.removals+=1;' : ""}}}});__policyObserver.observe(document,{childList:true,subtree:true});`
    : "";
  const controllerBody = `window.tsjs={que:[]};${probe}${options.prelude ?? ""}const __TSJS_SERVER_BOOT_TRANSPORT_V1__=${serverBootTransportLiteralV1(boot, options.takeover ? DEFERRED.outline : null)};${fixture.bootstrapBody}`;
  await page.route(`https://runtime.test${options.path}`, (route) =>
    route.fulfill({
      status: 200,
      contentType: "text/html; charset=utf-8",
      headers: { "content-security-policy": options.csp },
      body: `<!doctype html><html><head><meta charset="utf-8"><script${nonce}>${controllerBody}</script></head><body><div id="${POLICY_SLOT}"></div><script${nonce} src="${options.takeover ? DEFERRED.firstDisplaySrc : fixture.runtimeSrc}" id="trustedserver-js"></script>${options.tail ? `<script${nonce}>${options.tail}</script>` : ""}</body></html>`,
    }),
  );
  await page.goto(`https://runtime.test${options.path}`);
  return {
    deferredRequests,
    routeHits,
    runtimeRequests,
  };
}

async function runtimeState(page: Page): Promise<string | undefined> {
  return page.evaluate(
    () =>
      (
        window as unknown as {
          tsjs?: { _internal?: { state?: string } };
        }
      ).tsjs?._internal?.state,
  );
}

async function releaseDeferredGate(page: Page): Promise<void> {
  await page.evaluate(async (slot) => {
    const target = (
      window as unknown as {
        tsjs: {
          requestAds(options: {
            slots: readonly string[];
            timeoutMs: number;
          }): Promise<unknown>;
        };
      }
    ).tsjs;
    await target.requestAds({ slots: [slot], timeoutMs: 100 });
  }, POLICY_SLOT);
}

for (const fixture of [
  {
    name: "same-origin allowlisting",
    path: "/policy-same-origin",
    csp: "default-src 'none'; script-src 'self' 'unsafe-inline'",
  },
  {
    name: "matching nonce",
    path: "/policy-matching-nonce",
    csp: `default-src 'none'; script-src 'self' 'nonce-${NONCE}'`,
    nonce: NONCE,
  },
  {
    name: "nonce-only",
    path: "/policy-nonce-only",
    csp: `default-src 'none'; script-src 'nonce-${NONCE}'`,
    nonce: NONCE,
  },
  {
    name: "strict-dynamic",
    path: "/policy-strict-dynamic",
    csp: `default-src 'none'; script-src 'nonce-${NONCE}' 'strict-dynamic'`,
    nonce: NONCE,
  },
] as const) {
  test(`boots the authenticated direct runtime under ${fixture.name}`, async ({
    page,
  }) => {
    const requests = await servePolicyPage(page, fixture);
    await expect.poll(() => runtimeState(page)).toBe("kernel");
    expect(requests.runtimeRequests).toHaveLength(1);
  });
}

test("a full script policy block starts no TSJS execution or request", async ({
  page,
}) => {
  const requests = await servePolicyPage(page, {
    path: "/policy-blocked",
    csp: "default-src 'none'; script-src 'none'",
  });
  expect(await runtimeState(page)).toBeUndefined();
  expect(requests.routeHits.runtime).toBe(0);
});

for (const fixture of [
  {
    name: "the allowed fixed Trusted Types policy",
    path: "/policy-tt-named",
    csp: `default-src 'none'; script-src 'nonce-${NONCE}'; require-trusted-types-for 'script'; trusted-types trusted-server#tsjs-v1`,
  },
  {
    name: "a rejected fixed name and exact-preserving publisher default",
    path: "/policy-tt-default",
    csp: `default-src 'none'; script-src 'nonce-${NONCE}'; require-trusted-types-for 'script'; trusted-types default`,
    prelude:
      'trustedTypes.createPolicy("default",{createScriptURL:value=>value});',
  },
] as const) {
  test(`loads a deferred module with ${fixture.name}`, async ({
    browserName,
    page,
  }) => {
    test.skip(
      browserName !== "chromium",
      "Trusted Types enforcement is Chromium-only",
    );
    const requests = await servePolicyPage(page, {
      ...fixture,
      nonce: NONCE,
      deferred: true,
    });
    await expect.poll(() => runtimeState(page)).toBe("kernel");
    await releaseDeferredGate(page);
    await expect.poll(() => requests.deferredRequests.length).toBe(1);
    await expect
      .poll(() => page.locator("#ts-render-trace-panel").count())
      .toBe(1);
  });
}

test("the first-display owner creates the fixed Trusted Types policy once and leases it to deferred loading", async ({
  browserName,
  page,
}) => {
  test.skip(
    browserName !== "chromium",
    "Trusted Types enforcement is Chromium-only",
  );
  const requests = await servePolicyPage(page, {
    path: "/policy-tt-takeover",
    csp: `default-src 'none'; script-src 'nonce-${NONCE}' 'strict-dynamic'; require-trusted-types-for 'script'; trusted-types trusted-server#tsjs-v1`,
    nonce: NONCE,
    deferred: true,
    takeover: true,
    prelude:
      'window.__policyCreates=[];const __nativeCreatePolicy=trustedTypes.createPolicy.bind(trustedTypes);Object.defineProperty(trustedTypes,"createPolicy",{value:(name,rules)=>{if(window.__policyCreates.includes(name))throw new TypeError("duplicate TSJS policy");window.__policyCreates.push(name);return __nativeCreatePolicy(name,rules);}});',
  });
  await expect.poll(() => runtimeState(page)).toBe("kernel");
  await releaseDeferredGate(page);
  await expect.poll(() => requests.deferredRequests.length).toBe(1);
  await expect
    .poll(() => page.locator("#ts-render-trace-panel").count())
    .toBe(1);
  expect(requests.routeHits.firstDisplay).toBe(1);
  expect(requests.routeHits.runtime).toBe(1);
  expect(
    await page.evaluate(
      () =>
        (
          window as unknown as {
            __policyCreates: string[];
          }
        ).__policyCreates,
    ),
  ).toEqual(["trusted-server#tsjs-v1"]);
});

for (const fixture of [
  {
    name: "mutates the URL",
    path: "/policy-tt-mutate",
    prelude:
      'trustedTypes.createPolicy("default",{createScriptURL:value=>value+"&publisher=mutated"});',
  },
  {
    name: "throws synchronously",
    path: "/policy-tt-throw",
    prelude:
      'trustedTypes.createPolicy("default",{createScriptURL:()=>{throw new TypeError("publisher policy");}});',
  },
] as const) {
  test(`a publisher default policy that ${fixture.name} blocks before insertion`, async ({
    browserName,
    page,
  }) => {
    test.skip(
      browserName !== "chromium",
      "Trusted Types enforcement is Chromium-only",
    );
    const requests = await servePolicyPage(page, {
      ...fixture,
      csp: `default-src 'none'; script-src 'nonce-${NONCE}'; require-trusted-types-for 'script'; trusted-types default`,
      nonce: NONCE,
      deferred: true,
    });
    await expect.poll(() => runtimeState(page)).toBe("kernel");
    await releaseDeferredGate(page);
    await page.waitForTimeout(150);
    const probe = await page.evaluate(
      () =>
        (
          window as unknown as {
            __policyProbe: { insertions: number; removals: number };
          }
        ).__policyProbe,
    );
    expect(probe.insertions).toBe(0);
    expect(requests.deferredRequests).toEqual([]);
    expect(await page.locator("#ts-render-trace-panel").count()).toBe(0);
  });
}

test("a missing propagated nonce blocks after insertion without replacing the kernel", async ({
  page,
}) => {
  const requests = await servePolicyPage(page, {
    path: "/policy-missing-deferred-nonce",
    csp: `default-src 'none'; script-src 'nonce-${NONCE}'`,
    nonce: NONCE,
    deferred: true,
    tail: 'document.querySelector("script#trustedserver-js").nonce="";',
  });
  await expect.poll(() => runtimeState(page)).toBe("kernel");
  await releaseDeferredGate(page);
  await page.waitForTimeout(150);
  const insertions = await page.evaluate(
    () =>
      (
        window as unknown as {
          __policyProbe: { insertions: number };
        }
      ).__policyProbe.insertions,
  );
  expect(insertions).toBe(1);
  expect(requests.routeHits.deferred).toBe(0);
  expect(await page.locator("#ts-render-trace-panel").count()).toBe(0);
  expect(await runtimeState(page)).toBe("kernel");
});

test("removing the authenticated deferred node cannot register or replace the kernel", async ({
  page,
}) => {
  const requests = await servePolicyPage(page, {
    path: "/policy-deferred-replaced",
    csp: "default-src 'none'; script-src 'self' 'unsafe-inline'",
    deferred: true,
    removeDeferred: true,
  });
  await expect.poll(() => runtimeState(page)).toBe("kernel");
  await releaseDeferredGate(page);
  await page.waitForTimeout(150);
  const probe = await page.evaluate(
    () =>
      (
        window as unknown as {
          __policyProbe: { insertions: number; removals: number };
        }
      ).__policyProbe,
  );
  expect(probe).toEqual({ insertions: 1, removals: 1 });
  expect(requests.deferredRequests.length).toBeLessThanOrEqual(1);
  expect(await page.locator("#ts-render-trace-panel").count()).toBe(0);
  expect(await runtimeState(page)).toBe("kernel");
});
