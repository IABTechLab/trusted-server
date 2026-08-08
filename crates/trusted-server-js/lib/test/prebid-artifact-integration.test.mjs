// @vitest-environment node

// Builds and evaluates both production Prebid artifacts together: the
// external Prebid.js bundle (build-prebid-external.mjs) and the server-served
// tsjs shim (the same vite invocation build-all.mjs uses). This is the only
// coverage that proves the generated bundle entry populates the public API
// the real shim consumes — unit suites mock window.pbjs entirely.
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
import { createBrowserPrebidAdapter } from '../src/adapters/prebid';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const libDir = path.resolve(__dirname, '..');

let outputDirectory;
let bundleCode;
let shimCode;
let prebidVersion;
let artifactManifest;

beforeAll(async () => {
  outputDirectory = fs.mkdtempSync(path.join(os.tmpdir(), 'trusted-server-prebid-artifacts-'));

  await main([
    '--adapters',
    'adf',
    '--user-id-modules',
    'sharedIdSystem',
    '--out',
    outputDirectory,
  ]);
  artifactManifest = JSON.parse(
    fs.readFileSync(path.join(outputDirectory, 'manifest.json'), 'utf8')
  );
  bundleCode = fs.readFileSync(path.join(outputDirectory, artifactManifest.filename), 'utf8');
  prebidVersion = artifactManifest.prebidVersion;

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

describe('tsjs-prebid shim artifact', () => {
  it('stays Prebid-free and uses only the external bundle public API', () => {
    // The embedded version string and `_pbjsGlobals` are core markers. Prove
    // they appear in the external artifact first so this test fails loudly if
    // either marker rots instead of silently passing.
    expect(bundleCode).toContain(prebidVersion);
    expect(bundleCode).toContain('_pbjsGlobals');
    expect(shimCode).not.toContain(prebidVersion);
    expect(shimCode).not.toContain('_pbjsGlobals');

    // A value-import of Prebid or a private rendering helper would multiply
    // the shim size; retain a margin above the normal compact shim output.
    expect(bundleCode.length).toBeGreaterThan(200_000);
    expect(shimCode.length).toBeLessThan(30_000);
    expect(shimCode).toContain('markWinningBidAsUsed');
  });
});

describe('external bundle + served shim evaluated together', () => {
  it('reuses an exact artifact without replaying factories and keeps one watchdog per wrapper', () => {
    const dom = new JSDOM('<!doctype html><html><head></head><body></body></html>', {
      url: 'https://pub.example.com/article',
      runScripts: 'outside-only',
    });
    const pageWindow = dom.window;
    const watchdogs = [];
    const originalSetTimeout = pageWindow.setTimeout.bind(pageWindow);
    pageWindow.setTimeout = (callback, delay, ...arguments_) => {
      if (delay === 5_000 && String(callback).includes('__tsWatchdogFired')) {
        watchdogs.push(callback);
        return 1;
      }
      return originalSetTimeout(callback, delay, ...arguments_);
    };
    pageWindow.fetch = vi.fn(async () => new Response('{}'));
    pageWindow.Request = Request;
    pageWindow.Headers = Headers;
    pageWindow.Response = Response;
    pageWindow.AbortController = AbortController;
    if (!('isSecureContext' in pageWindow)) pageWindow.isSecureContext = true;
    pageWindow.eval('window.pbjs = { que: [], cmd: [] };');

    pageWindow.eval(bundleCode);
    const firstBinding = pageWindow.pbjs;
    const firstRequestBids = firstBinding.requestBids;
    const firstStamp = firstBinding.__trustedServerArtifactV1;
    pageWindow.eval(bundleCode);

    expect(pageWindow.pbjs).toBe(firstBinding);
    expect(pageWindow.pbjs.requestBids).toBe(firstRequestBids);
    expect(pageWindow.pbjs.__trustedServerArtifactV1).toBe(firstStamp);
    expect(watchdogs).toHaveLength(2);

    const processQueue = vi.fn(firstBinding.processQueue.bind(firstBinding));
    firstBinding.processQueue = processQueue;
    for (const watchdog of watchdogs) {
      watchdog();
      watchdog();
    }
    expect(processQueue).toHaveBeenCalledTimes(2);
    dom.window.close();
  });

  it('refuses a different valid artifact without disturbing the working binding', () => {
    const dom = new JSDOM('<!doctype html><html></html>', {
      url: 'https://pub.example.com/article',
      runScripts: 'outside-only',
    });
    const pageWindow = dom.window;
    const conflictingStamp = {
      abi: artifactManifest.abi,
      artifactReleaseId: 'f'.repeat(64),
      prebidVersion: artifactManifest.prebidVersion,
      moduleStems: artifactManifest.moduleStems,
      bidderCodes: artifactManifest.bidderCodes,
      bidderAliases: artifactManifest.bidderAliases,
      userIdModules: artifactManifest.userIdModules,
    };
    pageWindow.eval(
      `window.__conflictingRequestBids=function conflictingRequestBids(){};window.pbjs={que:[],cmd:[]};["addAdUnits","getBidResponsesForAdUnitCode","getHighestCpmBids","offEvent","onEvent","processQueue","registerBidAdapter","renderAd","requestBids","setTargetingForGPTAsync"].forEach(function(name){window.pbjs[name]=name==="requestBids"?window.__conflictingRequestBids:function(){};});`
    );
    pageWindow.eval(
      `window.__conflictingStamp=(function freeze(value){if(value&&typeof value==='object'){Object.getOwnPropertyNames(value).forEach(function(key){freeze(value[key]);});Object.freeze(value);}return value;})(${JSON.stringify(conflictingStamp)});`
    );
    pageWindow.Object.defineProperty(pageWindow.pbjs, '__trustedServerArtifactV1', {
      value: pageWindow.__conflictingStamp,
      enumerable: false,
      writable: false,
      configurable: false,
    });
    const binding = pageWindow.pbjs;
    const warn = vi.fn();
    pageWindow.console.warn = warn;

    expect(() => pageWindow.eval(bundleCode)).not.toThrow();
    expect(pageWindow.pbjs).toBe(binding);
    expect(pageWindow.pbjs.requestBids).toBe(pageWindow.__conflictingRequestBids);
    expect(pageWindow.pbjs.__trustedServerArtifactV1).toBe(pageWindow.__conflictingStamp);
    expect(warn).toHaveBeenCalledTimes(1);
    dom.window.close();
  });

  it('does not mistake an exact stamp on a Prebid stub for an initialized duplicate', () => {
    const dom = new JSDOM('<!doctype html><html></html>', {
      url: 'https://pub.example.com/article',
      runScripts: 'outside-only',
    });
    const pageWindow = dom.window;
    pageWindow.fetch = vi.fn(async () => new Response('{}'));
    pageWindow.Request = Request;
    pageWindow.Headers = Headers;
    pageWindow.Response = Response;
    pageWindow.AbortController = AbortController;
    if (!('isSecureContext' in pageWindow)) pageWindow.isSecureContext = true;
    pageWindow.eval('window.pbjs = { que: [], cmd: [] };');
    pageWindow.eval(
      `window.__exactStamp=(function freeze(value){if(value&&typeof value==='object'){Object.getOwnPropertyNames(value).forEach(function(key){freeze(value[key]);});Object.freeze(value);}return value;})(${JSON.stringify(
        {
          abi: artifactManifest.abi,
          artifactReleaseId: artifactManifest.artifactReleaseId,
          prebidVersion: artifactManifest.prebidVersion,
          moduleStems: artifactManifest.moduleStems,
          bidderCodes: artifactManifest.bidderCodes,
          bidderAliases: artifactManifest.bidderAliases,
          userIdModules: artifactManifest.userIdModules,
        }
      )});`
    );
    pageWindow.Object.defineProperty(pageWindow.pbjs, '__trustedServerArtifactV1', {
      value: pageWindow.__exactStamp,
      enumerable: false,
      writable: false,
      configurable: false,
    });

    expect(() => pageWindow.eval(bundleCode)).not.toThrow();
    expect(typeof pageWindow.pbjs.requestBids).toBe('function');
    expect(pageWindow.pbjs.__trustedServerArtifactV1).toBe(pageWindow.__exactStamp);
    dom.window.close();
  });

  it('accepts an exact 128-byte non-ASCII artifact name on a real stamped binding', () => {
    const dom = new JSDOM('<!doctype html><html></html>', {
      url: 'https://pub.example.com/article',
      runScripts: 'outside-only',
    });
    const pageWindow = dom.window;
    const boundaryName = 'é'.repeat(64);
    const boundaryStamp = {
      abi: artifactManifest.abi,
      artifactReleaseId: 'e'.repeat(64),
      prebidVersion: artifactManifest.prebidVersion,
      moduleStems: [...artifactManifest.moduleStems, boundaryName].sort(),
      bidderCodes: artifactManifest.bidderCodes,
      bidderAliases: artifactManifest.bidderAliases,
      userIdModules: artifactManifest.userIdModules,
    };
    pageWindow.eval(
      `window.__fakeRequestBids=function fakeRequestBids(){};window.pbjs={que:[],cmd:[]};["addAdUnits","getBidResponsesForAdUnitCode","getHighestCpmBids","offEvent","onEvent","processQueue","registerBidAdapter","renderAd","requestBids","setTargetingForGPTAsync"].forEach(function(name){window.pbjs[name]=name==="requestBids"?window.__fakeRequestBids:function(){};});`
    );
    pageWindow.eval(
      `window.__boundaryStamp=(function freeze(value){if(value&&typeof value==='object'){Object.getOwnPropertyNames(value).forEach(function(key){freeze(value[key]);});Object.freeze(value);}return value;})(${JSON.stringify(
        boundaryStamp
      )});`
    );
    pageWindow.Object.defineProperty(pageWindow.pbjs, '__trustedServerArtifactV1', {
      value: pageWindow.__boundaryStamp,
      enumerable: false,
      writable: false,
      configurable: false,
    });

    expect(() => pageWindow.eval(bundleCode)).not.toThrow();
    expect(pageWindow.pbjs.requestBids).toBe(pageWindow.__fakeRequestBids);
    expect(pageWindow.pbjs.__trustedServerArtifactV1).toBe(pageWindow.__boundaryStamp);
    dom.window.close();
  });

  it('does not accept a UTF-8-overlong artifact name on a real stamped binding', () => {
    const dom = new JSDOM('<!doctype html><html></html>', {
      url: 'https://pub.example.com/article',
      runScripts: 'outside-only',
    });
    const pageWindow = dom.window;
    pageWindow.fetch = vi.fn(async () => new Response('{}'));
    pageWindow.Request = Request;
    pageWindow.Headers = Headers;
    pageWindow.Response = Response;
    pageWindow.AbortController = AbortController;
    if (!('isSecureContext' in pageWindow)) pageWindow.isSecureContext = true;
    const overlongName = `${'é'.repeat(64)}a`;
    const malformedStamp = {
      abi: artifactManifest.abi,
      artifactReleaseId: 'f'.repeat(64),
      prebidVersion: artifactManifest.prebidVersion,
      moduleStems: [...artifactManifest.moduleStems, overlongName].sort(),
      bidderCodes: artifactManifest.bidderCodes,
      bidderAliases: artifactManifest.bidderAliases,
      userIdModules: artifactManifest.userIdModules,
    };
    pageWindow.eval(
      `window.__fakeRequestBids=function fakeRequestBids(){};window.pbjs={que:[],cmd:[]};["addAdUnits","getBidResponsesForAdUnitCode","getHighestCpmBids","offEvent","onEvent","processQueue","registerBidAdapter","renderAd","requestBids","setTargetingForGPTAsync"].forEach(function(name){window.pbjs[name]=name==="requestBids"?window.__fakeRequestBids:function(){};});`
    );
    pageWindow.eval(
      `window.__malformedStamp=(function freeze(value){if(value&&typeof value==='object'){Object.getOwnPropertyNames(value).forEach(function(key){freeze(value[key]);});Object.freeze(value);}return value;})(${JSON.stringify(
        malformedStamp
      )});`
    );
    pageWindow.Object.defineProperty(pageWindow.pbjs, '__trustedServerArtifactV1', {
      value: pageWindow.__malformedStamp,
      enumerable: false,
      writable: false,
      configurable: false,
    });

    expect(() => pageWindow.eval(bundleCode)).not.toThrow();
    expect(typeof pageWindow.pbjs.requestBids).toBe('function');
    expect(pageWindow.pbjs.requestBids).not.toBe(pageWindow.__fakeRequestBids);
    expect(pageWindow.pbjs.__trustedServerArtifactV1).toBe(pageWindow.__malformedStamp);
    dom.window.close();
  });

  it('keeps publisher Prebid usable when a hostile stamp cannot be replaced', () => {
    const dom = new JSDOM('<!doctype html><html></html>', {
      url: 'https://pub.example.com/article',
      runScripts: 'outside-only',
    });
    const pageWindow = dom.window;
    pageWindow.fetch = vi.fn(async () => new Response('{}'));
    pageWindow.Request = Request;
    pageWindow.Headers = Headers;
    pageWindow.Response = Response;
    pageWindow.AbortController = AbortController;
    if (!('isSecureContext' in pageWindow)) pageWindow.isSecureContext = true;
    pageWindow.eval('window.pbjs = { que: [], cmd: [] };');
    const hostileStamp = Object.freeze({ abi: 99 });
    pageWindow.Object.defineProperty(pageWindow.pbjs, '__trustedServerArtifactV1', {
      value: hostileStamp,
      enumerable: true,
      writable: false,
      configurable: false,
    });
    const warn = vi.fn();
    pageWindow.console.warn = warn;

    expect(() => pageWindow.eval(bundleCode)).not.toThrow();
    expect(typeof pageWindow.pbjs.requestBids).toBe('function');
    expect(pageWindow.pbjs.__trustedServerArtifactV1).toBe(hostileStamp);
    expect(warn).toHaveBeenCalledTimes(1);
    dom.window.close();
  });

  it('admits one exact TS bid through the real 10.26.0 response callback', async () => {
    const dom = new JSDOM('<!doctype html><html><body></body></html>', {
      url: 'https://pub.example.com/article',
      runScripts: 'outside-only',
      pretendToBeVisual: true,
    });
    const pageWindow = dom.window;
    pageWindow.fetch = vi.fn(async () => new Response('{}'));
    pageWindow.Request = Request;
    pageWindow.Headers = Headers;
    pageWindow.Response = Response;
    pageWindow.AbortController = AbortController;
    if (!('isSecureContext' in pageWindow)) pageWindow.isSecureContext = true;
    pageWindow.eval('window.pbjs = { que: [], cmd: [] };');
    pageWindow.eval(bundleCode);

    const adapter = createBrowserPrebidAdapter(pageWindow);
    let resolveAuction;
    const auctionReady = new Promise((resolve) => {
      resolveAuction = resolve;
    });
    let resolveBidsBack;
    const bidsBack = new Promise((resolve) => {
      resolveBidsBack = resolve;
    });
    const operation = adapter.run((prebid) => {
      prebid.registerTrustedServerBidder(resolveAuction);
      return prebid.requestBids({
        adUnits: [
          {
            code: 'slot-one',
            mediaTypes: { banner: { sizes: [[300, 250]] } },
            bids: [{ bidder: 'trustedServer', params: {} }],
          },
        ],
        timeout: 1_000,
        bidsBackHandler: resolveBidsBack,
      });
    });
    await operation.result;
    const auction = await auctionReady;
    expect(Object.isFrozen(auction)).toBe(true);
    expect(auction.bids).toHaveLength(1);

    const request = auction.bids[0];
    const reservationId = `r1_${'z'.repeat(22)}`;
    const prepared = Object.freeze({
      auctionId: auction.auctionId,
      adUnitCode: request.adUnitCode,
      bid: Object.freeze({
        requestId: request.requestId,
        adId: reservationId,
        cpm: 1.25,
        width: 300,
        height: 250,
        ad: '',
        ttl: 300,
        creativeId: 'creative-one',
        netRevenue: true,
        currency: 'USD',
        bidderCode: 'trustedServer',
        meta: Object.freeze({
          advertiserDomains: Object.freeze([]),
          tsAuctionId: auction.auctionId,
          tsBidId: 'server-bid-one',
        }),
      }),
    });

    const beforeAdmission = pageWindow.pbjs.getBidResponsesForAdUnitCode('slot-one');
    expect(Array.isArray(beforeAdmission)).toBe(true);
    expect(Array.isArray(beforeAdmission.bids)).toBe(true);
    expect(beforeAdmission.bids).toHaveLength(0);
    expect(adapter.admitTrustedBid(prepared)).toBe('admitted');
    const stored = pageWindow.pbjs.getBidResponsesForAdUnitCode('slot-one').bids;
    const admitted = stored.filter((bid) => bid.adId === reservationId);
    expect(admitted).toHaveLength(1);
    expect(admitted[0]).toMatchObject({
      adId: reservationId,
      adUnitCode: 'slot-one',
      auctionId: auction.auctionId,
      requestId: request.requestId,
      adserverTargeting: { hb_adid: reservationId },
    });
    auction.complete();
    await bidsBack;
    adapter.dispose();
    dom.window.close();
  }, 60_000);

  it('populates the public API, installs the shim exactly once, and routes an /auction request', async () => {
    const dom = new JSDOM('<!doctype html><html><head></head><body></body></html>', {
      url: 'https://pub.example.com/article',
      runScripts: 'outside-only',
      pretendToBeVisual: true,
    });
    const pageWindow = dom.window;

    // Stub the network before any artifact runs: Prebid's ajax module
    // captures window.fetch at evaluation time and builds Request objects.
    // jsdom ships none of the fetch API, so lend it Node's — with relative
    // URLs resolved against the page, as a browser Request would.
    const fetchSpy = vi.fn(
      async () =>
        new Response('{}', {
          status: 200,
          headers: { 'Content-Type': 'application/json' },
        })
    );
    pageWindow.fetch = fetchSpy;
    pageWindow.Request = class PageRequest extends Request {
      constructor(resource, init) {
        super(
          typeof resource === 'string'
            ? new URL(resource, 'https://pub.example.com').href
            : resource,
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

    // Mirror the server's head-injected state, which always precedes the
    // bundle script in document order.
    pageWindow.eval('window.pbjs = { que: [], cmd: [] };');
    pageWindow.__tsjs_prebid = {
      clientSideBidders: [],
      serverSideBidders: ['appnexus'],
    };

    pageWindow.eval(bundleCode);

    expect(typeof pageWindow.pbjs.requestBids).toBe('function');
    expect(typeof pageWindow.pbjs.registerBidAdapter).toBe('function');
    expect(pageWindow.__tsjs_prebid_bundle).toBeUndefined();
    expect(pageWindow.__tsjsPrebidShimInstalled).toBeUndefined();
    const artifactDescriptor = Object.getOwnPropertyDescriptor(
      pageWindow.pbjs,
      '__trustedServerArtifactV1'
    );
    expect(artifactDescriptor).toMatchObject({
      enumerable: false,
      writable: false,
      configurable: false,
    });
    expect(artifactDescriptor.value).toEqual(
      expect.objectContaining({
        abi: 1,
        artifactReleaseId: artifactManifest.artifactReleaseId,
        prebidVersion: '10.26.0',
      })
    );
    expect([...artifactDescriptor.value.bidderCodes]).toEqual(['adf', 'adform', 'adformOpenRTB']);
    expect([...artifactDescriptor.value.bidderAliases]).toEqual([
      { code: 'adform', moduleStem: 'adf' },
      { code: 'adformOpenRTB', moduleStem: 'adf' },
    ]);
    expect([...artifactDescriptor.value.userIdModules]).toEqual([
      {
        moduleName: 'sharedIdSystem',
        configNames: ['pubCommonId', 'sharedId'],
        eidSources: ['pubcid.org'],
      },
    ]);
    expect(Object.isFrozen(artifactDescriptor.value)).toBe(true);
    expect(Object.isFrozen(artifactDescriptor.value.moduleStems)).toBe(true);
    expect(Object.isFrozen(artifactDescriptor.value.bidderCodes)).toBe(true);
    expect(Object.isFrozen(artifactDescriptor.value.bidderAliases)).toBe(true);
    expect(Object.isFrozen(artifactDescriptor.value.bidderAliases[0])).toBe(true);
    expect(Object.isFrozen(artifactDescriptor.value.userIdModules)).toBe(true);
    expect(Object.isFrozen(artifactDescriptor.value.userIdModules[0])).toBe(true);
    expect(Object.isFrozen(artifactDescriptor.value.userIdModules[0].configNames)).toBe(true);
    expect(Object.isFrozen(artifactDescriptor.value.userIdModules[0].eidSources)).toBe(true);
    const adapter = createBrowserPrebidAdapter(pageWindow);
    expect(adapter.bindingStatus()).toBe('present');
    adapter.dispose();

    // Count trustedServer registrations across repeated shim evaluations.
    const originalRegisterBidAdapter = pageWindow.pbjs.registerBidAdapter.bind(pageWindow.pbjs);
    const registerSpy = vi.fn(originalRegisterBidAdapter);
    pageWindow.pbjs.registerBidAdapter = registerSpy;

    pageWindow.eval(shimCode);
    const wrappedRequestBids = pageWindow.pbjs.requestBids;

    // A second evaluation (double script inclusion, or a legacy bundle that
    // still carries a baked-in shim running after this one) must be a no-op.
    pageWindow.eval(shimCode);

    const trustedServerRegistrations = registerSpy.mock.calls.filter(
      ([, bidderCode]) => bidderCode === 'trustedServer'
    );
    expect(trustedServerRegistrations).toHaveLength(1);
    expect(pageWindow.pbjs.requestBids).toBe(wrappedRequestBids);
    expect(pageWindow.__tsjsPrebidShimInstalled).toBe(true);

    // Drive one real auction through the wrapped requestBids and assert the
    // transformed request reaches /auction.
    const slot = pageWindow.document.createElement('div');
    slot.id = 'ad-slot-1';
    pageWindow.document.body.appendChild(slot);

    pageWindow.pbjs.requestBids({
      adUnits: [
        {
          code: 'ad-slot-1',
          mediaTypes: { banner: { sizes: [[300, 250]] } },
          bids: [
            {
              bidder: 'trustedServer',
              params: {
                bidderParams: {
                  appnexus: { placementId: 1 },
                  pbsProviderId: { placementId: 2 },
                  returnedSeatAlias: { placementId: 3 },
                },
              },
            },
          ],
        },
      ],
      timeout: 1000,
    });

    const requestUrl = (resource) =>
      typeof resource === 'string' ? resource : String(resource?.url ?? resource);

    await vi.waitFor(
      () => {
        expect(
          fetchSpy.mock.calls.some(([resource]) => requestUrl(resource).includes('/auction'))
        ).toBe(true);
      },
      { timeout: 10_000 }
    );

    const [resource, init] = fetchSpy.mock.calls.find(([target]) =>
      requestUrl(target).includes('/auction')
    );
    const body = init?.body ?? (typeof resource === 'object' ? await resource.text() : undefined);
    const method = init?.method ?? resource?.method;
    expect(method).toBe('POST');
    const payload = JSON.parse(body);
    const adUnit = payload.adUnits[0];
    expect(adUnit.code).toBe('ad-slot-1');
    // Stored trustedServer params retain only authoritative server-side route
    // codes; provider IDs and returned aliases cannot reach /auction.
    const trustedServerBid = adUnit.bids.find((bid) => bid.bidder === 'trustedServer');
    expect(trustedServerBid.params.bidderParams).toEqual({ appnexus: { placementId: 1 } });

    dom.window.close();
  }, 60_000);
});
