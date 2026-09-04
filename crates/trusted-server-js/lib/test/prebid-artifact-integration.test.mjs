// @vitest-environment node

// Build and evaluate both production Prebid artifacts together. Unit tests mock
// window.pbjs; this suite proves registration and queue behavior in the actual
// generated IIFE and the server-served shim.

import crypto from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { JSDOM, requestInterceptor } from 'jsdom';
import { afterAll, beforeAll, describe, expect, it, vi } from 'vitest';

import { main } from '../build-prebid-external.mjs';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const libDir = path.resolve(__dirname, '..');

let outputDirectory;
let analyticsArtifact;
let noAnalyticsArtifact;
let shimCode;

async function buildArtifact(modules) {
  await main(['--modules-json', JSON.stringify(modules), '--out', outputDirectory]);
  const manifest = JSON.parse(fs.readFileSync(path.join(outputDirectory, 'manifest.json'), 'utf8'));
  const bundlePath = path.join(outputDirectory, manifest.filename);
  return {
    manifest,
    bundleCode: fs.readFileSync(bundlePath, 'utf8'),
    bundleBytes: fs.readFileSync(bundlePath),
  };
}

beforeAll(async () => {
  outputDirectory = fs.mkdtempSync(path.join(os.tmpdir(), 'trusted-server-prebid-artifacts-'));

  analyticsArtifact = await buildArtifact({
    bidder: ['rubiconBidAdapter'],
    userId: ['sharedIdSystem'],
    analytics: ['atsAnalyticsAdapter'],
  });
  noAnalyticsArtifact = await buildArtifact({
    bidder: ['rubiconBidAdapter'],
    userId: ['sharedIdSystem'],
  });

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

function createPage() {
  const blockedResourceRequests = [];
  const dom = new JSDOM('<!doctype html><html><head></head><body></body></html>', {
    url: 'https://pub.example.com/article',
    runScripts: 'outside-only',
    pretendToBeVisual: true,
    resources: {
      interceptors: [
        requestInterceptor(async (request) => {
          blockedResourceRequests.push(request.url);
          return new Response('', { status: 204 });
        }),
      ],
    },
  });
  dom.window.__blockedResourceRequests = blockedResourceRequests;
  return dom;
}

function installNetworkAndConsoleStubs(pageWindow) {
  const requests = [];
  const unexpectedTransports = [];
  const fetchSpy = vi.fn(async (resource, init) => {
    requests.push({ transport: 'fetch', resource, init });
    return new Response('{}', {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    });
  });
  pageWindow.fetch = fetchSpy;
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

  pageWindow.XMLHttpRequest = class StubXmlHttpRequest {
    readyState = 0;
    status = 0;
    responseText = '';
    onreadystatechange;
    onload;
    onerror;

    open(method, url) {
      this.method = method;
      this.url = new URL(url, 'https://pub.example.com').href;
      this.readyState = 1;
    }

    setRequestHeader() {}

    send(body) {
      requests.push({ transport: 'xhr', method: this.method, url: this.url, body });
      this.readyState = 4;
      this.status = 200;
      this.responseText = '{}';
      queueMicrotask(() => {
        this.onreadystatechange?.();
        this.onload?.();
      });
    }

    abort() {}
  };

  Object.defineProperty(pageWindow.navigator, 'sendBeacon', {
    configurable: true,
    value: vi.fn((url, body) => {
      requests.push({ transport: 'beacon', url, body });
      return true;
    }),
  });

  pageWindow.Image = class StubImage {
    set src(url) {
      requests.push({ transport: 'image', url });
    }
  };
  pageWindow.WebSocket = class BlockedWebSocket {
    constructor(url) {
      unexpectedTransports.push({ transport: 'websocket', url: String(url) });
      throw new Error('WebSocket is blocked in the Prebid artifact test');
    }
  };
  pageWindow.EventSource = class BlockedEventSource {
    constructor(url) {
      unexpectedTransports.push({ transport: 'eventsource', url: String(url) });
      throw new Error('EventSource is blocked in the Prebid artifact test');
    }
  };

  if (!('isSecureContext' in pageWindow)) {
    pageWindow.isSecureContext = true;
  }

  const consoleErrors = [];
  const consoleWarnings = [];
  pageWindow.console.error = vi.fn((...args) => consoleErrors.push(args.map(String).join(' ')));
  pageWindow.console.warn = vi.fn((...args) => consoleWarnings.push(args.map(String).join(' ')));

  return {
    fetchSpy,
    requests,
    consoleErrors,
    consoleWarnings,
    unexpectedTransports,
    blockedResourceRequests: pageWindow.__blockedResourceRequests,
  };
}

function expectNoUnexpectedNetworkActivity(stubs) {
  expect(stubs.unexpectedTransports).toEqual([]);
  expect(stubs.blockedResourceRequests).toEqual([]);
}

function installServerState(pageWindow, { analytics = false } = {}) {
  pageWindow.eval('window.pbjs = { que: [], cmd: [] };');
  pageWindow.__tsjs_prebid = { clientSideBidders: [] };

  if (analytics) {
    pageWindow.__analyticsLifecycle = {
      started: false,
      completed: false,
      error: undefined,
    };
    pageWindow.eval(`
      window.pbjs.que.push(function () {
        const state = window.__analyticsLifecycle;
        state.started = true;
        try {
          window.pbjs.enableAnalytics({
            provider: 'atsAnalytics',
            options: { pid: 'example-publisher-id' },
          });
          state.completed = true;
        } catch (error) {
          state.error = String(error && error.message ? error.message : error);
        }
      });
    `);
  }
}

function requestUrl(resource) {
  return typeof resource === 'string' ? resource : String(resource?.url ?? resource);
}

async function runAuction(pageWindow, fetchSpy) {
  const slot = pageWindow.document.createElement('div');
  slot.id = 'ad-slot-1';
  pageWindow.document.body.appendChild(slot);

  pageWindow.pbjs.requestBids({
    adUnits: [
      {
        code: 'ad-slot-1',
        mediaTypes: { banner: { sizes: [[300, 250]] } },
        bids: [{ bidder: 'appnexus', params: { placementId: 1 } }],
      },
    ],
    timeout: 1000,
  });

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
  const trustedServerBid = adUnit.bids.find((bid) => bid.bidder === 'trustedServer');
  expect(trustedServerBid.params.bidderParams).toEqual({ appnexus: { placementId: 1 } });
}

function expectManifest(manifest, analytics) {
  expect(manifest).toMatchObject({
    schemaVersion: 1,
    prebidVersion: '10.26.0',
    modules: {
      bidder: ['rubiconBidAdapter'],
      userId: ['sharedIdSystem'],
      analytics: analytics ? ['atsAnalyticsAdapter'] : [],
    },
    runtimeCodes: {
      bidder: ['rubicon'],
      analytics: analytics ? ['atsAnalytics'] : [],
    },
  });
}

describe('tsjs-prebid production artifacts', () => {
  it('keeps the served shim Prebid-free', () => {
    expect(analyticsArtifact.bundleCode).toContain(analyticsArtifact.manifest.prebidVersion);
    expect(analyticsArtifact.bundleCode).toContain('_pbjsGlobals');
    expect(shimCode).not.toContain(analyticsArtifact.manifest.prebidVersion);
    expect(shimCode).not.toContain('_pbjsGlobals');
    expect(analyticsArtifact.bundleCode.length).toBeGreaterThan(200_000);
    expect(shimCode.length).toBeLessThan(30_000);
    expect(shimCode).toContain('markWinningBidAsUsed');
  });

  it('registers ATS before the shim processes publisher callbacks', async () => {
    const dom = createPage();
    try {
      const pageWindow = dom.window;
      const stubs = installNetworkAndConsoleStubs(pageWindow);
      installServerState(pageWindow, { analytics: true });

      pageWindow.eval(analyticsArtifact.bundleCode);
      expect(pageWindow.__tsjs_prebid_bundle).toEqual({
        schemaVersion: 1,
        modules: {
          bidder: ['rubiconBidAdapter'],
          userId: ['sharedIdSystem'],
          analytics: ['atsAnalyticsAdapter'],
        },
        runtimeCodes: {
          bidder: ['rubicon'],
          analytics: ['atsAnalytics'],
        },
      });

      const originalRegisterBidAdapter = pageWindow.pbjs.registerBidAdapter.bind(pageWindow.pbjs);
      const registerSpy = vi.fn(originalRegisterBidAdapter);
      pageWindow.pbjs.registerBidAdapter = registerSpy;

      pageWindow.eval(shimCode);
      const wrappedRequestBids = pageWindow.pbjs.requestBids;
      pageWindow.eval(shimCode);

      await vi.waitFor(() => {
        const state = pageWindow.__analyticsLifecycle;
        expect(state.error).toBeUndefined();
        expect(state.completed).toBe(true);
      });

      expect(pageWindow.__analyticsLifecycle.started).toBe(true);
      expect(
        stubs.consoleErrors.some((message) =>
          message.includes("no analytics adapter found in registry for 'atsAnalytics'")
        )
      ).toBe(false);
      expect(
        [...stubs.consoleErrors, ...stubs.consoleWarnings].some((message) =>
          message.includes('Error processing command')
        )
      ).toBe(false);

      const trustedServerRegistrations = registerSpy.mock.calls.filter(
        ([, bidderCode]) => bidderCode === 'trustedServer'
      );
      expect(trustedServerRegistrations).toHaveLength(1);
      expect(pageWindow.pbjs.requestBids).toBe(wrappedRequestBids);
      expect(pageWindow.__tsjsPrebidShimInstalled).toBe(true);

      await runAuction(pageWindow, stubs.fetchSpy);
      expectNoUnexpectedNetworkActivity(stubs);
    } finally {
      dom.window.close();
    }
  }, 60_000);

  it('preserves auction behavior when analytics is omitted', async () => {
    const dom = createPage();
    try {
      const pageWindow = dom.window;
      const stubs = installNetworkAndConsoleStubs(pageWindow);
      installServerState(pageWindow);

      pageWindow.eval(noAnalyticsArtifact.bundleCode);
      pageWindow.eval(shimCode);

      expect(pageWindow.__tsjs_prebid_bundle.modules.analytics).toEqual([]);
      expect(pageWindow.__tsjs_prebid_bundle.runtimeCodes.analytics).toEqual([]);
      await runAuction(pageWindow, stubs.fetchSpy);
      expectNoUnexpectedNetworkActivity(stubs);
    } finally {
      dom.window.close();
    }
  }, 60_000);

  it('drains the publisher queue through the watchdog when the shim is absent', async () => {
    vi.useFakeTimers();
    const dom = createPage();
    try {
      const pageWindow = dom.window;
      const stubs = installNetworkAndConsoleStubs(pageWindow);
      pageWindow.setTimeout = globalThis.setTimeout;
      pageWindow.clearTimeout = globalThis.clearTimeout;
      installServerState(pageWindow);
      pageWindow.__watchdogCallbackRan = false;
      pageWindow.eval(
        'window.pbjs.que.push(function () { window.__watchdogCallbackRan = true; });'
      );

      pageWindow.eval(noAnalyticsArtifact.bundleCode);
      const originalProcessQueue = pageWindow.pbjs.processQueue.bind(pageWindow.pbjs);
      const processQueueSpy = vi.fn(originalProcessQueue);
      pageWindow.pbjs.processQueue = processQueueSpy;

      await vi.advanceTimersByTimeAsync(4999);
      expect(pageWindow.__watchdogCallbackRan).toBe(false);
      await vi.advanceTimersByTimeAsync(1);

      expect(processQueueSpy).toHaveBeenCalledTimes(1);
      expect(pageWindow.__watchdogCallbackRan).toBe(true);
      expect(pageWindow.__tsjs_prebid_bundle.modules.analytics).toEqual([]);
      expect(pageWindow.__tsjs_prebid_bundle.runtimeCodes.analytics).toEqual([]);
      expectNoUnexpectedNetworkActivity(stubs);
    } finally {
      vi.useRealTimers();
      dom.window.close();
    }
  });

  it('records hashes and SRI for the no-analytics artifact', () => {
    expectManifest(analyticsArtifact.manifest, true);
    expectManifest(noAnalyticsArtifact.manifest, false);

    const sha256 = crypto
      .createHash('sha256')
      .update(noAnalyticsArtifact.bundleBytes)
      .digest('hex');
    const sri = `sha384-${crypto
      .createHash('sha384')
      .update(noAnalyticsArtifact.bundleBytes)
      .digest('base64')}`;
    expect(noAnalyticsArtifact.manifest.sha256).toBe(sha256);
    expect(noAnalyticsArtifact.manifest.filename).toBe(`trusted-prebid-${sha256}.js`);
    expect(noAnalyticsArtifact.manifest.sri).toBe(sri);
  });
});
