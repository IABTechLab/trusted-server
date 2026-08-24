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
function tcData({ purpose1 = true, purpose3 = true, purpose4 = true, vendor97 = true } = {}) {
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
  };
}

/**
 * Evaluates both artifacts on a GDPR page whose CMP grants or denies the
 * purpose and vendor grants, then runs one auction.
 *
 * @returns the URLs the page requested and the cookies it managed to set.
 */
async function runGdprPage(grants = {}) {
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

  const consentData = tcData(grants);
  pageWindow.__tcfapi = (command, _version, callback) => {
    if (command === 'addEventListener' || command === 'getTCData') {
      callback(consentData, true);
    } else if (command === 'removeEventListener') {
      callback(true, true);
    }
  };

  // Mirror the server's head-injected state, which always precedes the bundle
  // script in document order.
  pageWindow.eval('window.pbjs = { que: [], cmd: [] };');
  pageWindow.__tsjs_prebid = {
    clientSideBidders: [],
    liveRamp: {
      placementId: '999',
      notUse3P: false,
      storageType: 'cookie',
      expiresDays: 15,
      refreshInSeconds: 1800,
    },
  };

  pageWindow.eval(bundleCode);
  pageWindow.pbjs.setConfig({
    consentManagement: { gdpr: { cmpApi: 'iab', timeout: 500, defaultGdprScope: true } },
    // Resolve User IDs before the auction so one auction is enough to observe
    // whether IdentityLink ran.
    userSync: { auctionDelay: 300, syncEnabled: false },
  });
  pageWindow.eval(shimCode);

  pageWindow.pbjs.requestBids({ adUnits: [], bidsBackHandler: () => {} });
  await new Promise((resolve) => setTimeout(resolve, 50));
  await new Promise((resolve) => pageWindow.setTimeout(resolve, 400));
  await new Promise((resolve) => setTimeout(resolve, 50));

  return { requestedUrls, cookies: pageWindow.document.cookie };
}

describe('external bundle TCF enforcement', () => {
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
});
