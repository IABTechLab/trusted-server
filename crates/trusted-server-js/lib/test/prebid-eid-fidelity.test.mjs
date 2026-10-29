// @vitest-environment node

// Real Prebid.js arms for the trustedServer EID contract (spec 7.5). Unit
// tests mock window.pbjs; this suite evaluates the generated external bundle
// and the served shim together in a JSDOM page and captures /auction bodies.
// Every arm except the D4 control fails against a shim built from main.

import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { JSDOM, requestInterceptor } from 'jsdom';
import { afterAll, beforeAll, describe, expect, it, vi } from 'vitest';

import { moduleBuildOptions } from '../build-module-options.mjs';
import { main } from '../build-prebid-external.mjs';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const libDir = path.resolve(__dirname, '..');

const PAGE_ORIGIN = 'https://pub.example.com';
const HOLD_LOAD_PATH = '/hold-window-load.css';
const UNIFIED_ID_PATH = '/unified-id';
const UNIFIED_ID_DELAY_MS = 200;
const IDENTITY_LINK_GVL_ID = 97;

const CONTROL_EID = {
  source: 'control.example.com',
  uids: [{ id: 'control-uid', atype: 1 }],
};
const PRESET_COOKIE_EIDS = [{ source: 'preset.example.com', uids: [{ id: 'preset-uid' }] }];
const LIVERAMP_EID = {
  source: 'liveramp.com',
  uids: [{ id: 'fictional-idl-envelope', atype: 3 }],
};
const UNIFIED_ID_EID = {
  source: 'adserver.org',
  inserter: 'adserver.org',
  matcher: 'adserver.org',
  mm: 4,
  uids: [{ id: 'fictional-tdid', atype: 1, ext: { rtiPartner: 'TDID' } }],
};
// The only safe form of a trustedServer TCF exception (spec 3.7).
const FULL_BASIC_ADS_RULE = {
  purpose: 'basicAds',
  enforcePurpose: true,
  enforceVendor: true,
  softVendorExceptions: ['trustedServer'],
};

let outputDirectory;
let bundleCode;
let shimCode;

beforeAll(async () => {
  outputDirectory = fs.mkdtempSync(path.join(os.tmpdir(), 'trusted-server-prebid-eids-'));
  await main([
    '--modules-json',
    JSON.stringify({
      bidder: ['appnexusBidAdapter'],
      userId: ['sharedIdSystem', 'unifiedIdSystem', 'identityLinkIdSystem'],
    }),
    '--out',
    outputDirectory,
  ]);
  const manifest = JSON.parse(fs.readFileSync(path.join(outputDirectory, 'manifest.json'), 'utf8'));
  bundleCode = fs.readFileSync(path.join(outputDirectory, manifest.filename), 'utf8');

  const { build } = await import('vite');
  await build(
    moduleBuildOptions({
      name: 'prebid',
      entryPath: path.join(libDir, 'src', 'integrations', 'prebid', 'index.ts'),
      outDir: outputDirectory,
    })
  );
  shimCode = fs.readFileSync(path.join(outputDirectory, 'tsjs-prebid.js'), 'utf8');
}, 240_000);

afterAll(() => {
  fs.rmSync(outputDirectory, { recursive: true, force: true });
});

function tcfData(vendorConsents, purposeOneConsent) {
  return {
    gdprApplies: true,
    tcString: 'CPexampleTCStringForTests',
    eventStatus: 'tcloaded',
    cmpStatus: 'loaded',
    apiVersion: '2',
    purpose: {
      consents: { 1: purposeOneConsent, 2: true, 3: true, 4: true },
      legitimateInterests: {},
    },
    vendor: { consents: vendorConsents, legitimateInterests: {} },
    publisher: { restrictions: {} },
    specialFeatureOptins: {},
  };
}

/**
 * A CMP that answers at once: Purposes 2 to 4 consented, Purpose 1 as given,
 * the given vendors only.
 */
function tcfApi(vendorConsents, purposeOneConsent) {
  let nextListenerId = 1;
  return (command, _version, callback) => {
    if (command === 'addEventListener') {
      callback(
        { ...tcfData(vendorConsents, purposeOneConsent), listenerId: nextListenerId++ },
        true
      );
    } else if (command === 'getTCData') {
      callback(tcfData(vendorConsents, purposeOneConsent), true);
    } else if (command === 'removeEventListener') {
      callback(true, true);
    }
  };
}

/**
 * A publisher page whose window.load is held by a stylesheet until
 * releaseLoad(). Every fetch goes to a spy, installed before the bundle is
 * evaluated, that records /auction bodies and answers the unifiedId endpoint
 * after UNIFIED_ID_DELAY_MS.
 */
function createPage({ cookies = [], vendorConsents, purposeOneConsent = true } = {}) {
  let releaseHeldLoad;
  const loadHeld = new Promise((resolve) => {
    releaseHeldLoad = resolve;
  });
  const dom = new JSDOM(
    `<!doctype html><html><head><link rel="stylesheet" href="${PAGE_ORIGIN}${HOLD_LOAD_PATH}"></head><body></body></html>`,
    {
      url: `${PAGE_ORIGIN}/article`,
      runScripts: 'outside-only',
      pretendToBeVisual: true,
      resources: {
        interceptors: [
          requestInterceptor(async (request) => {
            if (new URL(request.url).pathname === HOLD_LOAD_PATH) {
              await loadHeld;
              return new Response('', {
                status: 200,
                headers: { 'Content-Type': 'text/css' },
              });
            }
            return new Response('', { status: 204 });
          }),
        ],
      },
    }
  );
  const pageWindow = dom.window;

  // jsdom drops Secure cookies on non-HTTPS origins (spec 7.1 rule 5).
  pageWindow.document.cookie = 'ts-eids-probe=1; Path=/; Secure; SameSite=Lax';
  expect(pageWindow.document.cookie, 'Secure cookies should work on the page origin').toContain(
    'ts-eids-probe=1'
  );
  pageWindow.document.cookie = 'ts-eids-probe=; Path=/; Max-Age=0';
  for (const cookie of cookies) {
    pageWindow.document.cookie = cookie;
  }

  const auctionBodies = [];
  pageWindow.fetch = vi.fn(async (resource, init) => {
    const url = new URL(typeof resource === 'string' ? resource : resource.url, PAGE_ORIGIN);
    if (url.pathname === '/auction') {
      const body =
        init?.body ?? (typeof resource === 'object' ? await resource.clone().text() : undefined);
      auctionBodies.push(JSON.parse(body));
    }
    if (url.pathname === UNIFIED_ID_PATH) {
      await new Promise((resolve) => setTimeout(resolve, UNIFIED_ID_DELAY_MS));
      return new Response(JSON.stringify({ TDID: 'fictional-tdid' }), {
        status: 200,
        headers: { 'Content-Type': 'application/json' },
      });
    }
    return new Response('{}', {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    });
  });
  pageWindow.Request = class PageRequest extends Request {
    constructor(resource, init) {
      super(typeof resource === 'string' ? new URL(resource, PAGE_ORIGIN).href : resource, init);
    }
  };
  pageWindow.Headers = Headers;
  pageWindow.Response = Response;
  pageWindow.AbortController = AbortController;
  if (!('isSecureContext' in pageWindow)) {
    pageWindow.isSecureContext = true;
  }
  if (vendorConsents) {
    pageWindow.__tcfapi = tcfApi(vendorConsents, purposeOneConsent);
  }

  // Mirror the server's head-injected state.
  pageWindow.eval('window.pbjs = { que: [], cmd: [] };');
  pageWindow.__tsjs_prebid = { clientSideBidders: [], serverSideBidders: [] };

  return {
    pageWindow,
    auctionBodies,
    install() {
      pageWindow.eval(bundleCode);
      pageWindow.eval(shimCode);
    },
    async releaseLoad() {
      releaseHeldLoad();
      await vi.waitFor(() => expect(pageWindow.document.readyState).toBe('complete'));
    },
    async close() {
      releaseHeldLoad();
      // Let in-flight User ID work settle before jsdom tears down.
      await new Promise((resolve) => setTimeout(resolve, UNIFIED_ID_DELAY_MS + 100));
      pageWindow.close();
    },
  };
}

function readTsEidsCookie(pageWindow) {
  const value = pageWindow.document.cookie.match(/(?:^|; )ts-eids=([^;]*)/)?.[1];
  if (!value) {
    return undefined;
  }
  return JSON.parse(
    new TextDecoder().decode(Uint8Array.from(atob(value), (char) => char.charCodeAt(0)))
  );
}

function presetTsEidsCookie() {
  return `ts-eids=${btoa(JSON.stringify(PRESET_COOKIE_EIDS))}; Path=/; Secure; SameSite=Lax`;
}

function requestBids(pageWindow, adUnits) {
  return new Promise((resolve) => {
    pageWindow.pbjs.requestBids({
      adUnits,
      timeout: 1000,
      bidsBackHandler: () => resolve(),
    });
  });
}

/** An ad unit with no publisher bidders: the shim adds only trustedServer. */
function trustedServerAdUnits() {
  return [
    {
      code: 'ad-slot-1',
      mediaTypes: { banner: { sizes: [[300, 250]] } },
      bids: [],
    },
  ];
}

/** An ad unit with a client-side bidder that has a GVL ID (appnexus, 32). */
function clientBidderAdUnits() {
  return [
    {
      code: 'ad-slot-1',
      mediaTypes: { banner: { sizes: [[300, 250]] } },
      bids: [{ bidder: 'appnexus', params: { placementId: 1 } }],
    },
  ];
}

function tcfEnforcementResults(pageWindow) {
  return pageWindow.pbjs
    .getEvents()
    .filter((event) => event.eventType === 'tcf2Enforcement')
    .map((event) => event.args);
}

describe('trustedServer EIDs with real Prebid.js', () => {
  it('D1: never forwards an ID restricted to another bidder', async () => {
    const page = createPage({
      cookies: ['_sharedid=fictional-shared-id; Path=/'],
    });
    try {
      page.install();
      expect(page.pageWindow.document.readyState, 'install runs before load').toBe('loading');
      page.pageWindow.pbjs.setConfig({
        userSync: {
          auctionDelay: 300,
          userIds: [
            {
              name: 'sharedId',
              bidders: ['appnexus'],
              storage: { type: 'cookie', name: '_sharedid', expires: 30 },
            },
          ],
        },
        ortb2: { user: { ext: { eids: [CONTROL_EID] } } },
      });

      await requestBids(page.pageWindow, trustedServerAdUnits());
      expect(page.pageWindow.document.readyState, 'the auction runs before load').toBe('loading');

      expect(
        page.pageWindow.pbjs.getUserIdsAsEids().map((eid) => eid.source),
        'precondition: the restricted module produced its ID'
      ).toContain('pubcid.org');
      expect(
        page.auctionBodies.map((body) => body.eids),
        'the /auction body should carry the control and not the restricted ID'
      ).toEqual([[CONTROL_EID]]);
      expect(readTsEidsCookie(page.pageWindow), 'ts-eids should copy the body set').toEqual([
        CONTROL_EID,
      ]);
    } finally {
      await page.close();
    }
  }, 60_000);

  it('D3: forwards same-source entries separately with their own EID-level fields', async () => {
    const first = {
      source: 'ids.example.com',
      inserter: 'inserter-a.example.com',
      mm: 2,
      ext: { segments: ['s1'] },
      uids: [{ id: 'ids-uid', atype: 1 }],
    };
    const second = {
      source: 'ids.example.com',
      inserter: 'inserter-b.example.com',
      matcher: 'matcher-b.example.com',
      mm: 3,
      uids: [{ id: 'ids-uid', atype: 1 }],
    };
    const page = createPage();
    try {
      page.install();
      expect(page.pageWindow.document.readyState, 'install runs before load').toBe('loading');
      page.pageWindow.pbjs.setConfig({
        ortb2: { user: { ext: { eids: [first, second] } } },
      });

      await requestBids(page.pageWindow, trustedServerAdUnits());
      expect(page.pageWindow.document.readyState, 'the auction runs before load').toBe('loading');

      expect(page.auctionBodies.map((body) => body.eids)).toEqual([[first, second]]);
      const { ext: _eidExt, ...firstWithoutExt } = first;
      expect(
        readTsEidsCookie(page.pageWindow),
        'ts-eids should hold both entries without EID-level ext'
      ).toEqual([firstWithoutExt, second]);
    } finally {
      await page.close();
    }
  }, 60_000);

  it('D4: the first auction after window.load waits for a pending User ID module', async () => {
    const page = createPage();
    try {
      page.install();
      expect(page.pageWindow.document.readyState, 'step 1 starts before load').toBe('loading');
      await page.releaseLoad();

      page.pageWindow.pbjs.setConfig({
        userSync: {
          userIds: [
            {
              name: 'unifiedId',
              params: { url: `${PAGE_ORIGIN}${UNIFIED_ID_PATH}` },
            },
          ],
        },
      });
      expect(page.pageWindow.document.readyState, 'step 2 runs after load').toBe('complete');
      expect(
        page.pageWindow.pbjs.getConfig('userSync').auctionDelay,
        "Prebid's default auctionDelay should apply"
      ).toBe(500);

      await requestBids(page.pageWindow, trustedServerAdUnits());

      expect(
        page.auctionBodies.map((body) => body.eids),
        'the first /auction should carry the delayed unifiedId'
      ).toEqual([[UNIFIED_ID_EID]]);
    } finally {
      await page.close();
    }
  }, 60_000);

  it('D4 control: the first auction before window.load waits, on every build', async () => {
    const page = createPage();
    try {
      page.install();
      page.pageWindow.pbjs.setConfig({
        userSync: {
          userIds: [
            {
              name: 'unifiedId',
              params: { url: `${PAGE_ORIGIN}${UNIFIED_ID_PATH}` },
            },
          ],
        },
      });
      expect(page.pageWindow.document.readyState, 'the auction runs before load').toBe('loading');

      await requestBids(page.pageWindow, trustedServerAdUnits());

      // A projection, not the full entry: main drops EID-level fields, and
      // this control must pass on main and on the branch alike.
      expect(
        page.auctionBodies.map((body) =>
          (body.eids ?? []).map((eid) => [eid.source, eid.uids.map((uid) => uid.id)])
        ),
        'the spy and the module should deliver unifiedId to the first auction'
      ).toEqual([[['adserver.org', ['fictional-tdid']]]]);
    } finally {
      await page.close();
    }
  }, 60_000);

  async function runTcfPage({ withRule, purposeOneConsent = true }) {
    const page = createPage({
      cookies: ['idl_env=fictional-idl-envelope; Path=/', presetTsEidsCookie()],
      vendorConsents: { [IDENTITY_LINK_GVL_ID]: true },
      purposeOneConsent,
    });
    page.install();
    expect(page.pageWindow.document.readyState, 'install runs before load').toBe('loading');
    expect(readTsEidsCookie(page.pageWindow), 'precondition: ts-eids is preset').toEqual(
      PRESET_COOKIE_EIDS
    );
    page.pageWindow.pbjs.setConfig({
      consentManagement: {
        gdpr: {
          cmpApi: 'iab',
          timeout: 500,
          ...(withRule ? { rules: [FULL_BASIC_ADS_RULE] } : {}),
        },
      },
      userSync: {
        auctionDelay: 300,
        userIds: [
          {
            name: 'identityLink',
            params: { pid: '999', notUse3P: true },
            storage: { type: 'cookie', name: 'idl_env', expires: 15 },
          },
        ],
      },
      ortb2: { user: { ext: { eids: [CONTROL_EID] } } },
    });
    await requestBids(page.pageWindow, clientBidderAdUnits());
    expect(page.pageWindow.document.readyState, 'the auction runs before load').toBe('loading');
    return page;
  }

  it('TCF: without an exception no client EIDs reach TS and ts-eids is cleared', async () => {
    const page = await runTcfPage({ withRule: false });
    try {
      expect(
        tcfEnforcementResults(page.pageWindow).some((result) =>
          result.biddersBlocked.includes('trustedServer')
        ),
        'positive control: Prebid blocked trustedServer'
      ).toBe(true);
      expect(page.auctionBodies, 'no /auction request should be sent').toEqual([]);
      expect(readTsEidsCookie(page.pageWindow), 'ts-eids should be cleared').toBeUndefined();
    } finally {
      await page.close();
    }
  }, 60_000);

  it('TCF: with the full basicAds rule TS gets its EIDs and other bidders stay blocked', async () => {
    const page = await runTcfPage({ withRule: true });
    try {
      // Prebid appends module EIDs after ortb2 entries (spec 3.2 step 3).
      expect(page.auctionBodies.map((body) => body.eids)).toEqual([[CONTROL_EID, LIVERAMP_EID]]);
      const results = tcfEnforcementResults(page.pageWindow);
      expect(
        results.some((result) => result.biddersBlocked.includes('appnexus')),
        'the rule must not switch off enforcement for other bidders'
      ).toBe(true);
      expect(
        results.some((result) => result.biddersBlocked.includes('trustedServer')),
        'trustedServer should not be blocked with the rule'
      ).toBe(false);
      expect(readTsEidsCookie(page.pageWindow), 'ts-eids should copy the body set').toEqual([
        CONTROL_EID,
        LIVERAMP_EID,
      ]);
    } finally {
      await page.close();
    }
  }, 60_000);

  it('TCF: with the full basicAds rule and Purpose 1 denied, TS gets its ortb2 EIDs and ts-eids is cleared', async () => {
    const page = await runTcfPage({ withRule: true, purposeOneConsent: false });
    try {
      // Purpose 1 gates the User ID module (enrichEids), not ortb2 EIDs
      // (transmitEids), so only the control reaches TS.
      expect(
        page.auctionBodies.map((body) => body.eids),
        'Prebid should still let trustedServer forward the ortb2 EID'
      ).toEqual([[CONTROL_EID]]);
      expect(
        tcfEnforcementResults(page.pageWindow).some((result) =>
          result.biddersBlocked.includes('trustedServer')
        ),
        'trustedServer should not be blocked with the rule'
      ).toBe(false);
      expect(
        readTsEidsCookie(page.pageWindow),
        'ts-eids should be cleared without Purpose 1'
      ).toBeUndefined();
    } finally {
      await page.close();
    }
  }, 60_000);
});
