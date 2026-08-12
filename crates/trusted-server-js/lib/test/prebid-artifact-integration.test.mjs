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

function cloneAndDeepFreezeInWindow(pageWindow, value) {
  const cloned = pageWindow.JSON.parse(JSON.stringify(value));
  const freeze = (entry) => {
    if (entry && typeof entry === 'object') {
      for (const key of pageWindow.Object.getOwnPropertyNames(entry)) freeze(entry[key]);
      pageWindow.Object.freeze(entry);
    }
    return entry;
  };
  return freeze(cloned);
}

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

  it('reuses separately constructed identical artifacts without reporting a conflict', () => {
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
    const warn = vi.fn();
    pageWindow.console.warn = warn;
    const firstBytes = Buffer.from(bundleCode, 'utf8');
    const duplicateBytes = Buffer.from(bundleCode, 'utf8');
    expect(firstBytes).not.toBe(duplicateBytes);
    expect(firstBytes.equals(duplicateBytes)).toBe(true);

    pageWindow.eval(firstBytes.toString('utf8'));
    const firstBinding = pageWindow.pbjs;
    const firstRequestBids = firstBinding.requestBids;
    const firstRegisterBidAdapter = firstBinding.registerBidAdapter;
    const firstStamp = firstBinding.__trustedServerArtifactV1;
    pageWindow.eval(duplicateBytes.toString('utf8'));

    expect(pageWindow.pbjs).toBe(firstBinding);
    expect(pageWindow.pbjs.requestBids).toBe(firstRequestBids);
    expect(pageWindow.pbjs.registerBidAdapter).toBe(firstRegisterBidAdapter);
    expect(pageWindow.pbjs.__trustedServerArtifactV1).toBe(firstStamp);
    expect(warn).not.toHaveBeenCalled();
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
    pageWindow.__conflictingStamp = cloneAndDeepFreezeInWindow(pageWindow, conflictingStamp);
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
    pageWindow.__exactStamp = cloneAndDeepFreezeInWindow(pageWindow, {
      abi: artifactManifest.abi,
      artifactReleaseId: artifactManifest.artifactReleaseId,
      prebidVersion: artifactManifest.prebidVersion,
      moduleStems: artifactManifest.moduleStems,
      bidderCodes: artifactManifest.bidderCodes,
      bidderAliases: artifactManifest.bidderAliases,
      userIdModules: artifactManifest.userIdModules,
    });
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
    pageWindow.__boundaryStamp = cloneAndDeepFreezeInWindow(pageWindow, boundaryStamp);
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
    pageWindow.__malformedStamp = cloneAndDeepFreezeInWindow(pageWindow, malformedStamp);
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
});
