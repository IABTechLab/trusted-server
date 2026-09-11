// @vitest-environment node

// Proves the generated external Prebid bundle actually ENFORCES the TCF signal
// it collects. `consentManagementTcf` only retrieves the consent string; the
// activity controls that act on it live in `tcfControl`. Without that module a
// TC string denying Purpose 1 changes nothing: User ID submodules still write
// browser storage and still call their vendor endpoints.
//
// This matters most for the managed LiveRamp entry, which Trusted Server
// configures on the operator's behalf: the publisher never wrote the page code
// that turns it on, so the bundle is the only place enforcement can come from.
//
// Runs in the node environment (vite/esbuild cannot run under jsdom globals)
// and evaluates the artifacts in an explicit JSDOM window instead.

import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { JSDOM } from 'jsdom';
import { afterAll, beforeAll, describe, expect, it, vi } from 'vitest';

import { main } from '../build-prebid-external.mjs';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const libDir = path.resolve(__dirname, '..');

const LIVE_RAMP_ENVELOPE_HOST = 'api.rlcdn.com';
const LIVE_RAMP_STORAGE_NAME = 'idl_env';
// LiveRamp's IAB Global Vendor List ID.
const LIVE_RAMP_GVL_VENDOR_ID = 97;

/**
 * Requests the page made to LiveRamp's envelope endpoint.
 *
 * Matches the parsed hostname rather than a substring: `includes()` would also
 * match an unrelated host that merely carries this one in its name or query
 * string, which could let the granted-consent assertion count the wrong
 * request.
 *
 * @returns the matching URLs
 */
function envelopeRequests(urls) {
  return urls.filter((url) => {
    try {
      return new URL(String(url), 'https://pub.example.com').hostname === LIVE_RAMP_ENVELOPE_HOST;
    } catch {
      return false;
    }
  });
}

let outputDirectory;
let bundleCode;
let shimCode;

beforeAll(async () => {
  outputDirectory = fs.mkdtempSync(path.join(os.tmpdir(), 'trusted-server-prebid-consent-'));

  await main([
    '--adapters',
    'adf',
    '--user-id-modules',
    'identityLinkIdSystem',
    '--out',
    outputDirectory,
  ]);
  const manifest = JSON.parse(fs.readFileSync(path.join(outputDirectory, 'manifest.json'), 'utf8'));
  bundleCode = fs.readFileSync(path.join(outputDirectory, manifest.filename), 'utf8');

  const { build } = await import('vite');
  await build({
    configFile: false,
    root: libDir,
    build: {
      emptyOutDir: false,
      outDir: outputDirectory,
      assetsDir: '.',
      sourcemap: false,
      minify: 'esbuild',
      rollupOptions: {
        input: path.join(libDir, 'src', 'integrations', 'prebid', 'index.ts'),
        output: {
          format: 'iife',
          dir: outputDirectory,
          entryFileNames: 'tsjs-prebid.js',
          inlineDynamicImports: true,
          extend: false,
          name: 'tsjs_prebid',
        },
      },
    },
    logLevel: 'warn',
  });
  shimCode = fs.readFileSync(path.join(outputDirectory, 'tsjs-prebid.js'), 'utf8');
}, 240_000);

afterAll(() => {
  fs.rmSync(outputDirectory, { recursive: true, force: true });
});

// `tcfControl` reads the CMP's structured `vendorData`, not the encoded string,
// so the purpose and vendor grants below are what the rules actually evaluate.
// The string only has to be present and non-empty.
function tcData(
  { purpose1 = true, purpose3 = true, purpose4 = true, vendor97 = true } = {},
  listenerId
) {
  return {
    gdprApplies: true,
    tcString: 'CPexampleTCStringForTests',
    eventStatus: 'tcloaded',
    cmpStatus: 'loaded',
    apiVersion: '2',
    purpose: {
      consents: { 1: purpose1, 3: purpose3, 4: purpose4 },
      legitimateInterests: {},
    },
    vendor: {
      consents: { [LIVE_RAMP_GVL_VENDOR_ID]: vendor97 },
      legitimateInterests: {},
    },
    publisher: { restrictions: {} },
    specialFeatureOptins: {},
    ...(listenerId === undefined ? {} : { listenerId }),
  };
}

/**
 * Evaluates both artifacts on a GDPR page whose CMP grants or denies the
 * purpose and vendor grants, then runs one auction.
 *
 * @returns the URLs the page requested and the cookies it managed to set.
 */
async function runGdprPage(
  grants = {},
  {
    publisherConsentManagement,
    latePublisherConsentManagement,
    cmpEventAfterLateConfig,
    deferInitialCmpResponse = false,
    deferPrebidCmpResponse = false,
    lateCmp = false,
    queueAuction = true,
    replaceTcfApiBeforeLateEvent = false,
    tcDataOverrides,
    lateTcDataOverrides,
    waitPastConsentTimeoutMs = 0,
  } = {}
) {
  const dom = new JSDOM('<!doctype html><html><head></head><body></body></html>', {
    url: 'https://pub.example.com/article',
    runScripts: 'outside-only',
    pretendToBeVisual: true,
  });
  const pageWindow = dom.window;

  const requestedUrls = [];
  pageWindow.fetch = vi.fn(async (resource) => {
    requestedUrls.push(typeof resource === 'string' ? resource : resource?.url);
    return new Response(JSON.stringify({ envelope: 'opaque-test-envelope' }), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    });
  });
  pageWindow.Request = class PageRequest extends Request {
    constructor(resource, init) {
      super(
        typeof resource === 'string' ? new URL(resource, 'https://pub.example.com').href : resource,
        init
      );
    }
  };
  pageWindow.Headers = Headers;
  pageWindow.Response = Response;
  pageWindow.AbortController = AbortController;
  if (!('isSecureContext' in pageWindow)) {
    pageWindow.isSecureContext = true;
  }

  const consentListeners = new Map();
  let nextListenerId = 1;
  let registeredConsentListenerCount = 0;
  let removedConsentListenerCount = 0;
  let replacementApiRemoveCount = 0;
  const event = (listenerId) => ({ ...tcData(grants, listenerId), ...tcDataOverrides });
  const consentData = event();
  const cmp = (command, _version, callback, parameter) => {
    if (command === 'addEventListener') {
      const listenerId = nextListenerId++;
      consentListeners.set(listenerId, callback);
      registeredConsentListenerCount += 1;
      // The shim's own consent gate subscribes before it activates automatic
      // IAB consent, so Prebid's subscription is always a later one.
      const isShimConsentGate = registeredConsentListenerCount === 1;
      const deferred = deferInitialCmpResponse || (deferPrebidCmpResponse && !isShimConsentGate);
      if (!deferred) {
        callback(event(listenerId), true);
      }
    } else if (command === 'getTCData') {
      callback(consentData, true);
    } else if (command === 'removeEventListener') {
      if (consentListeners.delete(parameter)) {
        removedConsentListenerCount += 1;
      }
      callback(true, true);
    }
  };

  if (!lateCmp) pageWindow.__tcfapi = cmp;

  // Mirror the server's head-injected state, which always precedes the bundle
  // script in document order.
  pageWindow.eval('window.pbjs = { que: [], cmd: [] };');
  pageWindow.__tsjs_prebid = {
    clientSideBidders: [],
    managedUserIds: [
      {
        name: 'identityLink',
        params: { pid: '999', notUse3P: false },
        storage: {
          type: 'cookie',
          name: 'idl_env',
          expires: 15,
          refreshInSeconds: 1800,
        },
      },
    ],
  };

  pageWindow.eval(bundleCode);
  const publisherConfig = {
    // Resolve User IDs before the auction so one auction is enough to observe
    // whether IdentityLink ran.
    userSync: { auctionDelay: 300, syncEnabled: false },
  };
  if (publisherConsentManagement !== undefined) {
    publisherConfig.consentManagement = publisherConsentManagement;
  }
  pageWindow.pbjs.setConfig(publisherConfig);
  if (lateCmp && queueAuction) {
    pageWindow.pbjs.que.push(() => {
      pageWindow.pbjs.requestBids({ adUnits: [], bidsBackHandler: () => {} });
    });
  }
  pageWindow.eval(shimCode);
  if (lateCmp) {
    await new Promise((resolve) => setTimeout(resolve, 25));
    pageWindow.__tcfapi = cmp;
  }

  if (latePublisherConsentManagement !== undefined) {
    pageWindow.pbjs.setConfig({ consentManagement: latePublisherConsentManagement });
  }
  if (replaceTcfApiBeforeLateEvent) {
    pageWindow.__tcfapi = (command, _version, callback) => {
      if (command === 'removeEventListener') {
        replacementApiRemoveCount += 1;
        callback(true, true);
      }
    };
  }
  if (cmpEventAfterLateConfig !== undefined) {
    for (const [listenerId, callback] of consentListeners) {
      callback(
        {
          ...tcData(cmpEventAfterLateConfig, listenerId),
          ...tcDataOverrides,
          ...lateTcDataOverrides,
        },
        true
      );
    }
  }

  pageWindow.pbjs.requestBids({ adUnits: [], bidsBackHandler: () => {} });
  await new Promise((resolve) => setTimeout(resolve, 50));
  await new Promise((resolve) => pageWindow.setTimeout(resolve, 400));
  await new Promise((resolve) => setTimeout(resolve, 50));
  // Prebid's own GDPR handler resolves with null consent after its default
  // ten-second timeout. Waiting past that is the only way to observe what a
  // stalled CMP actually produces.
  if (waitPastConsentTimeoutMs > 0) {
    await new Promise((resolve) => pageWindow.setTimeout(resolve, waitPastConsentTimeoutMs));
    await new Promise((resolve) => setTimeout(resolve, 100));
  }

  let configuredUserIds;
  try {
    configuredUserIds = pageWindow.pbjs.getConfig('userSync.userIds');
  } catch {
    configuredUserIds = undefined;
  }

  return {
    requestedUrls,
    cookies: pageWindow.document.cookie,
    consentManagement: pageWindow.pbjs.getConfig('consentManagement'),
    userIdNames: (Array.isArray(configuredUserIds) ? configuredUserIds : []).map(
      (entry) => entry?.name
    ),
    registeredConsentListenerCount,
    removedConsentListenerCount,
    remainingConsentListenerCount: consentListeners.size,
    replacementApiRemoveCount,
  };
}

describe('external bundle TCF enforcement', () => {
  it('keeps a queued auction safe before a late denying CMP appears', async () => {
    const { requestedUrls, cookies, consentManagement } = await runGdprPage(
      { purpose1: false, vendor97: false },
      { lateCmp: true }
    );
    expect(envelopeRequests(requestedUrls)).toEqual([]);
    expect(cookies).not.toContain(LIVE_RAMP_STORAGE_NAME);
    expect(cookies).not.toContain('_lr_retry_request');
    expect(consentManagement.gdpr.cmpApi).toBe('iab');
  });

  it('resolves IDs after a queued auction when a late CMP grants consent', async () => {
    const { requestedUrls, cookies, consentManagement } = await runGdprPage({}, { lateCmp: true });
    expect(envelopeRequests(requestedUrls)).toHaveLength(1);
    expect(cookies).toContain(LIVE_RAMP_STORAGE_NAME);
    expect(consentManagement.gdpr.cmpApi).toBe('iab');
  });

  it('resolves late managed IDs when publisher initialization precedes the first auction', async () => {
    const { requestedUrls, cookies } = await runGdprPage(
      {},
      { lateCmp: true, queueAuction: false }
    );
    expect(envelopeRequests(requestedUrls)).toHaveLength(1);
    expect(cookies).toContain(LIVE_RAMP_STORAGE_NAME);
  });

  it('bundles the activity-control module alongside the consent collectors', () => {
    // A bundle that collects consent but cannot act on it is the failure mode
    // this whole suite exists to prevent.
    expect(bundleCode).toContain('consentManagementTcf');
    expect(bundleCode).toContain('tcfControl');
  });

  it('blocks IdentityLink storage and vendor calls when Purpose 1 is denied', async () => {
    const { requestedUrls, cookies } = await runGdprPage({ purpose1: false });

    expect(envelopeRequests(requestedUrls)).toEqual([]);
    expect(cookies).not.toContain(LIVE_RAMP_STORAGE_NAME);
    expect(cookies).not.toContain('_lr_retry_request');
  });

  it('blocks IdentityLink storage and vendor calls when vendor 97 is denied', async () => {
    const { requestedUrls, cookies } = await runGdprPage({ vendor97: false });

    expect(envelopeRequests(requestedUrls)).toEqual([]);
    expect(cookies).not.toContain(LIVE_RAMP_STORAGE_NAME);
    expect(cookies).not.toContain('_lr_retry_request');
  });

  it('still resolves IdentityLink when Purpose 3 alone is denied', async () => {
    const { requestedUrls, cookies } = await runGdprPage({ purpose3: false });

    expect(envelopeRequests(requestedUrls)).toHaveLength(1);
    expect(cookies).toContain(LIVE_RAMP_STORAGE_NAME);
  });

  it('still resolves IdentityLink when Purpose 4 alone is denied', async () => {
    const { requestedUrls, cookies } = await runGdprPage({ purpose4: false });

    expect(envelopeRequests(requestedUrls)).toHaveLength(1);
    expect(cookies).toContain(LIVE_RAMP_STORAGE_NAME);
  });

  it('resolves IdentityLink when all relevant grants are present', async () => {
    const { requestedUrls, cookies } = await runGdprPage();

    expect(envelopeRequests(requestedUrls)).toHaveLength(1);
    expect(cookies).toContain(LIVE_RAMP_STORAGE_NAME);
  });

  it('never seeds a managed module while the CMP has not answered', async () => {
    // A callable `__tcfapi` is not a consent decision. The managed entry must
    // stay out of every configuration Prebid sees, and no automatic IAB
    // activation may claim `consentManagement` on its behalf.
    const { userIdNames, consentManagement } = await runGdprPage(
      {},
      { deferInitialCmpResponse: true }
    );
    expect(userIdNames).not.toContain('identityLink');
    expect(consentManagement?.gdpr?.cmpApi).not.toBe('iab');
  });

  it('never calls the vendor when the CMP stalls past the Prebid consent timeout', async () => {
    // The direct reproduction: Prebid's GDPR handler gives up after its default
    // ten seconds and proceeds with null consent and `gdprApplies: false`,
    // which `tcfControl` cannot distinguish from a user outside GDPR scope. An
    // unseeded managed module has nothing to run on that result.
    const { requestedUrls, cookies, userIdNames } = await runGdprPage(
      {},
      { deferInitialCmpResponse: true, waitPastConsentTimeoutMs: 11_000 }
    );
    expect(envelopeRequests(requestedUrls)).toEqual([]);
    expect(cookies).not.toContain(LIVE_RAMP_STORAGE_NAME);
    expect(cookies).not.toContain('_lr_retry_request');
    expect(userIdNames).not.toContain('identityLink');
  }, 60_000);

  it('never seeds a managed module while the CMP UI still awaits the user', async () => {
    const { userIdNames, consentManagement } = await runGdprPage(
      {},
      { tcDataOverrides: { eventStatus: 'cmpuishown' } }
    );
    expect(userIdNames).not.toContain('identityLink');
    expect(consentManagement?.gdpr?.cmpApi).not.toBe('iab');
  });

  it('resolves managed IDs once the user completes the CMP UI', async () => {
    const { requestedUrls, cookies, userIdNames, consentManagement } = await runGdprPage(
      {},
      {
        tcDataOverrides: { eventStatus: 'cmpuishown' },
        cmpEventAfterLateConfig: {},
        lateTcDataOverrides: { eventStatus: 'useractioncomplete' },
      }
    );
    expect(userIdNames).toContain('identityLink');
    expect(envelopeRequests(requestedUrls)).toHaveLength(1);
    expect(cookies).toContain(LIVE_RAMP_STORAGE_NAME);
    expect(consentManagement.gdpr.cmpApi).toBe('iab');
  });

  it('resolves managed IDs when the CMP reports that GDPR does not apply', async () => {
    // Out of scope is a terminal answer even without a settled event status.
    const { requestedUrls, cookies, userIdNames } = await runGdprPage(
      {},
      { tcDataOverrides: { gdprApplies: false, eventStatus: 'cmpuishown' } }
    );
    expect(userIdNames).toContain('identityLink');
    expect(envelopeRequests(requestedUrls)).toHaveLength(1);
    expect(cookies).toContain(LIVE_RAMP_STORAGE_NAME);
  });

  it('preserves publisher-owned GDPR configuration in the generated bundle', async () => {
    const publisherConsentManagement = {
      gdpr: { cmpApi: 'iab', timeout: 123, defaultGdprScope: true },
    };

    const { consentManagement } = await runGdprPage({}, { publisherConsentManagement });

    expect(consentManagement.gdpr).toEqual(publisherConsentManagement.gdpr);
  });

  it('retires automatic IAB consent when late static GDPR configuration takes ownership', async () => {
    const deniedStaticConsent = {
      gdpr: {
        cmpApi: 'static',
        consentData: tcData({ purpose1: false }),
      },
    };

    const {
      requestedUrls,
      cookies,
      registeredConsentListenerCount,
      removedConsentListenerCount,
      remainingConsentListenerCount,
    } = await runGdprPage(
      {},
      {
        latePublisherConsentManagement: deniedStaticConsent,
        cmpEventAfterLateConfig: {},
      }
    );

    // Two subscriptions exist on this page: the shim's own consent gate and
    // the automatic IAB activation it then performs. Both must be retired.
    expect(registeredConsentListenerCount).toBe(2);
    expect(removedConsentListenerCount).toBe(registeredConsentListenerCount);
    expect(remainingConsentListenerCount).toBe(0);
    expect(envelopeRequests(requestedUrls)).toEqual([]);
    expect(cookies).not.toContain(LIVE_RAMP_STORAGE_NAME);
    expect(cookies).not.toContain('_lr_retry_request');
  });

  it('removes its own pending consent subscription when publisher configuration takes over', async () => {
    // The shim's consent gate subscribes before anything can answer it. When
    // publisher configuration claims ownership first, the gate's listener id is
    // still unknown, so the removal can only happen on the CMP's first event.
    const staticConsent = {
      gdpr: {
        cmpApi: 'static',
        consentData: tcData({ purpose1: false }),
      },
    };

    const {
      registeredConsentListenerCount,
      removedConsentListenerCount,
      remainingConsentListenerCount,
    } = await runGdprPage(
      {},
      {
        deferInitialCmpResponse: true,
        latePublisherConsentManagement: staticConsent,
        cmpEventAfterLateConfig: {},
      }
    );

    expect(registeredConsentListenerCount).toBe(1);
    expect(removedConsentListenerCount).toBe(registeredConsentListenerCount);
    expect(remainingConsentListenerCount).toBe(0);
  });

  it('ignores a delayed initial IAB response after static GDPR configuration takes ownership', async () => {
    const deniedStaticConsent = {
      gdpr: {
        cmpApi: 'static',
        consentData: tcData({ purpose1: false }),
      },
    };

    const { requestedUrls, cookies, replacementApiRemoveCount } = await runGdprPage(
      {},
      {
        latePublisherConsentManagement: deniedStaticConsent,
        cmpEventAfterLateConfig: {},
        deferPrebidCmpResponse: true,
        replaceTcfApiBeforeLateEvent: true,
      }
    );

    expect(replacementApiRemoveCount).toBe(1);
    expect(envelopeRequests(requestedUrls)).toEqual([]);
    expect(cookies).not.toContain(LIVE_RAMP_STORAGE_NAME);
    expect(cookies).not.toContain('_lr_retry_request');
  });
});
