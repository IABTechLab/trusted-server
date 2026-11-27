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

import { moduleBuildOptions } from '../build-module-options.mjs';
import { main, verifyPrebidPackageVersion } from '../build-prebid-external.mjs';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const libDir = path.resolve(__dirname, '..');
const prebidVersion = verifyPrebidPackageVersion();

let outputDirectory;
let analyticsArtifact;
let noAnalyticsArtifact;
let managedUserIdArtifact;
let shimCode;
let coreCode;

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
  managedUserIdArtifact = await buildArtifact({
    bidder: ['rubiconBidAdapter'],
    userId: ['sharedIdSystem', 'identityLinkIdSystem'],
  });

  const { build } = await import('vite');
  await build(
    moduleBuildOptions({
      name: 'prebid',
      entryPath: path.join(libDir, 'src', 'integrations', 'prebid', 'index.ts'),
      outDir: outputDirectory,
    })
  );
  shimCode = fs.readFileSync(path.join(outputDirectory, 'tsjs-prebid.js'), 'utf8');
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
        input: path.join(libDir, 'src', 'core', 'index.ts'),
        output: {
          format: 'iife',
          dir: outputDirectory,
          entryFileNames: 'tsjs-core.js',
          inlineDynamicImports: true,
          extend: false,
          name: 'tsjs_core',
        },
      },
    },
    logLevel: 'warn',
  });
  coreCode = fs.readFileSync(path.join(outputDirectory, 'tsjs-core.js'), 'utf8');
  console.log('[trace-prebid-artifact] shim characters:', shimCode.length);
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
  expect(
    stubs.requests
      .map((entry) => new URL(requestUrl(entry.url ?? entry.resource), 'https://pub.example.com'))
      .filter((url) => url.origin !== 'https://pub.example.com')
      .map(String)
  ).toEqual([]);
}

function installServerState(pageWindow, { analytics = false, managedUserIds } = {}) {
  pageWindow.eval('window.pbjs = { que: [], cmd: [] };');
  pageWindow.__tsjs_prebid = {
    clientSideBidders: [],
    serverSideBidders: ['appnexus'],
    ...(managedUserIds ? { managedUserIds } : {}),
  };

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
        bids: [
          { bidder: 'appnexus', params: { placementId: 1 } },
          { bidder: 'trustedServer', params: { storedRequest: undefined } },
        ],
      },
      {
        code: 'ad-slot-2',
        mediaTypes: { banner: { sizes: [[300, 250]] } },
        bids: [],
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
  expect(trustedServerBid.params).not.toHaveProperty('storedRequest');
  const generated = payload.adUnits.find((unit) => unit.code === 'ad-slot-2');
  expect(generated.bids[0].params).toEqual({ bidderParams: {}, storedRequest: false });
}

function expectManifest(manifest, analytics) {
  expect(manifest).toMatchObject({
    schemaVersion: 1,
    prebidVersion,
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
    // The original shim measures 40,354 characters. Shared strict trace JSON
    // validation, request-ID hook association, and bounded pending lifecycle add
    // 11,676 characters (52,030 total). Keep a narrow 52.5 KB guard: Prebid remains a separate artifact above 200 KB, and importing
    // its runtime or private rendering helpers must still fail this regression.
    expect(shimCode.length).toBeLessThan(52_500);
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

  it('owns the managed User ID entry against publisher setConfig and mergeConfig', async () => {
    const dom = createPage();
    try {
      const pageWindow = dom.window;
      installNetworkAndConsoleStubs(pageWindow);
      installServerState(pageWindow, {
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
      });

      pageWindow.eval(managedUserIdArtifact.bundleCode);
      expect(pageWindow.__tsjs_prebid_bundle.modules.userId).toEqual([
        'sharedIdSystem',
        'identityLinkIdSystem',
      ]);

      pageWindow.eval(shimCode);

      pageWindow.pbjs.setConfig({ userSync: { userIds: [{ name: 'sharedId' }] } });

      // Managed IDs remain deferred until the publisher supplies its consent policy.
      expect(
        pageWindow.pbjs.getConfig('userSync.userIds').some(({ name }) => name === 'identityLink')
      ).toBe(false);

      pageWindow.pbjs.setConfig({
        consentManagement: {
          gdpr: { cmpApi: 'static', consentData: { gdprApplies: false } },
        },
      });

      pageWindow.pbjs.requestBids({ adUnits: [] });

      expect(pageWindow.pbjs.getConfig('userSync.userIds')).toEqual(
        expect.arrayContaining([
          expect.objectContaining({ name: 'sharedId' }),
          expect.objectContaining({
            name: 'identityLink',
            params: { pid: '999', notUse3P: false },
            storage: expect.objectContaining({ name: 'idl_env' }),
          }),
        ])
      );

      // Characterize the pinned Prebid artifact: partial userSync updates retain
      // its effective User ID list, including the operator-managed entry.
      pageWindow.pbjs.setConfig({ userSync: { syncDelay: 50 } });

      const userIdsAfterPartialUpdate = pageWindow.pbjs.getConfig('userSync.userIds');
      expect(userIdsAfterPartialUpdate.filter(({ name }) => name === 'identityLink')).toEqual([
        expect.objectContaining({
          name: 'identityLink',
          params: { pid: '999', notUse3P: false },
        }),
      ]);
      expect(userIdsAfterPartialUpdate).toEqual(
        expect.arrayContaining([expect.objectContaining({ name: 'sharedId' })])
      );
      expect(pageWindow.pbjs.getConfig('userSync.syncDelay')).toBe(50);

      // Exercise the real Prebid mergeConfig implementation. It closes over
      // Prebid's internal setConfig, so the shim must guard mergeConfig itself
      // to prevent a publisher-owned duplicate from bypassing the setConfig guard.
      pageWindow.pbjs.mergeConfig({
        userSync: {
          userIds: [
            { name: 'sharedId' },
            { name: 'identityLink', params: { pid: 'publisher-value' } },
          ],
        },
      });

      const mergedUserIds = pageWindow.pbjs.getConfig('userSync.userIds');
      expect(mergedUserIds.filter(({ name }) => name === 'identityLink')).toEqual([
        expect.objectContaining({
          name: 'identityLink',
          params: { pid: '999', notUse3P: false },
        }),
      ]);
      expect(mergedUserIds).toEqual(
        expect.arrayContaining([expect.objectContaining({ name: 'sharedId' })])
      );

      // requestBids leaves User ID submodule work in flight. Let it settle
      // before closing the window, or jsdom tears down under it and the
      // rejection surfaces as an unhandled error.
      await new Promise((resolve) => setTimeout(resolve, 100));
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

  it('records both manifests and validates the no-analytics artifact hash and SRI', () => {
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

describe('active trace through real Prebid 10.26 registered adapters', () => {
  function tracedPage(mode) {
    const dom = createPage();
    const pageWindow = dom.window;
    const stubs = installNetworkAndConsoleStubs(pageWindow);
    installServerState(pageWindow);
    pageWindow.TextEncoder = TextEncoder;
    pageWindow.__tsjs_trace_active = true;
    pageWindow.eval(coreCode);
    pageWindow.eval(noAnalyticsArtifact.bundleCode);
    pageWindow.pbjs.setConfig({ bidderTimeout: 25 });
    const register = pageWindow.pbjs.registerBidAdapter.bind(pageWindow.pbjs);
    let spec;
    pageWindow.pbjs.registerBidAdapter = (adapter, code, registered) => {
      if (code === 'trustedServer') {
        spec = registered;
        if (typeof registered.onTimeout === 'function')
          registered.onTimeout = vi.fn(registered.onTimeout);
        if (typeof registered.onBidderError === 'function')
          registered.onBidderError = vi.fn(registered.onBidderError);
      }
      return register(adapter, code, registered);
    };
    if (mode === 'timeout') stubs.fetchSpy.mockImplementation(() => new Promise(() => {}));
    if (mode === 'error') stubs.fetchSpy.mockRejectedValue(new Error('private-network-error'));
    pageWindow.eval(shimCode);
    return { dom, pageWindow, stubs, spec };
  }
  function auction(pageWindow, timeout = 25, bidId) {
    return new Promise((resolve) =>
      pageWindow.pbjs.requestBids({
        adUnits: [
          {
            code: 'example-trace-unit',
            mediaTypes: { banner: { sizes: [[300, 250]] } },
            bids: [{ bidder: 'trustedServer', params: {}, ...(bidId ? { bid_id: bidId } : {}) }],
          },
        ],
        timeout,
        bidsBackHandler: resolve,
      })
    );
  }
  it.each(['timeout', 'error'])(
    'routes actual %s through registerBidAdapter/newBidder/adapterManager exactly once',
    async (mode) => {
      const { dom, pageWindow, stubs, spec } = tracedPage(mode);
      try {
        expect(spec.onTimeout).toEqual(expect.any(Function));
        expect(spec.onBidderError).toEqual(expect.any(Function));
        const bidderEvents = [];
        pageWindow.pbjs.onEvent(mode === 'timeout' ? 'bidTimeout' : 'bidderError', (event) =>
          bidderEvents.push(event)
        );
        const completed = auction(pageWindow);
        await vi.waitFor(
          () => expect(mode === 'timeout' ? spec.onTimeout : spec.onBidderError).toHaveBeenCalled(),
          { timeout: 5000 }
        );
        await completed;
        const captured = pageWindow.tsjs.traceEvidence.snapshot();
        expect(pageWindow.tsjs.traceEvidence.captureStatus()).toBe('unavailable');
        expect(captured.value.issues).toEqual(['evidence_transport_failed']);
        expect(captured.value.slotCorrelations).toEqual([]);
        expect(bidderEvents.length).toBeGreaterThan(0);
        expect(JSON.stringify(captured)).not.toContain('private-network-error');
        const callback = mode === 'timeout' ? spec.onTimeout : spec.onBidderError;
        callback(...callback.mock.calls[0]);
        expect(pageWindow.tsjs.traceEvidence.snapshot()).toEqual(captured);
        expectNoUnexpectedNetworkActivity(stubs);
      } finally {
        dom.window.dispatchEvent(new dom.window.PageTransitionEvent('pagehide'));
        dom.window.close();
      }
    },
    10000
  );
  it('consumes readable optional evidence in the real registered interpretResponse before the ordinary no-bid callback', async () => {
    const { dom, pageWindow, stubs } = tracedPage('response');
    try {
      stubs.fetchSpy.mockImplementation(async (_resource, init) => {
        const payload = JSON.parse(init?.body ?? (await _resource.text()));
        const ref = payload.adUnits[0].ext.trusted_server.trace_slot_ref;
        return new Response(
          JSON.stringify({
            seatbid: [],
            ext: {
              trusted_server: {
                trace_auction: {
                  schema_version: 1,
                  evidence: {
                    schema_version: 1,
                    diagnostic_auction_id: 'ts-auc-1234567812344abc8def123456789abc',
                    source: 'auction_api',
                    terminal_status: 'completed',
                    provider_calls: [],
                    slots: [
                      {
                        slot_number: 1,
                        slot_ref: ref,
                        requested_sizes: [[300, 250]],
                        returned_bid_count: 0,
                        candidate: 'no_candidate',
                      },
                    ],
                    truncation: {
                      omitted_provider_calls: 0,
                      omitted_slots: 0,
                      omitted_nested_values: 0,
                    },
                    coverage: { provider_to_slot_no_bid: 'unavailable' },
                  },
                },
              },
            },
          }),
          { status: 200, headers: { 'Content-Type': 'application/json' } }
        );
      });
      await auction(pageWindow, 1000);
      expect(pageWindow.tsjs.traceEvidence.captureStatus()).toBe('complete');
      const captured = pageWindow.tsjs.traceEvidence.snapshot().value;
      expect(captured.serverAuctions).toHaveLength(1);
      expect(captured.issues).toEqual(['correlation_unavailable']);
      expect(captured.slotCorrelations).toEqual([]);
      expectNoUnexpectedNetworkActivity(stubs);
    } finally {
      dom.window.dispatchEvent(new dom.window.PageTransitionEvent('pagehide'));
      dom.window.close();
    }
  }, 10000);
  it.each(['timeout', 'error'])(
    'preserves a newer reused bid ID through a repeated old actual %s hook',
    async (mode) => {
      const { dom, pageWindow, stubs, spec } = tracedPage(mode);
      try {
        const callback = mode === 'timeout' ? spec.onTimeout : spec.onBidderError;
        const first = auction(pageWindow, 25, 'example-reused-bid');
        await vi.waitFor(() => expect(callback).toHaveBeenCalled(), { timeout: 5000 });
        await first;
        const oldArguments = callback.mock.calls[0];
        const oldBids = mode === 'timeout' ? oldArguments[0] : oldArguments[0].bidderRequest.bids;
        expect(oldBids[0].bidId).toBe('example-reused-bid');
        expect(oldBids[0].bidderRequestId).toEqual(expect.any(String));
        let deliver;
        let nextPayload;
        stubs.fetchSpy.mockImplementation(async (resource, init) => {
          nextPayload = JSON.parse(init?.body ?? (await resource.text()));
          return await new Promise((resolve) => {
            deliver = resolve;
          });
        });
        const build = spec.buildRequests;
        let nextBids;
        spec.buildRequests = (...args) => {
          nextBids = args[0];
          return build(...args);
        };
        const next = auction(pageWindow, 2000, 'example-reused-bid');
        await vi.waitFor(() => expect(deliver).toEqual(expect.any(Function)));
        expect(nextBids[0].bidId).toBe(oldBids[0].bidId);
        expect(nextBids[0].bidderRequestId).not.toBe(oldBids[0].bidderRequestId);
        callback(...oldArguments);
        deliver(
          new Response(
            JSON.stringify({
              seatbid: [],
              ext: {
                trusted_server: {
                  trace_auction: {
                    schema_version: 1,
                    evidence: {
                      schema_version: 1,
                      diagnostic_auction_id: 'ts-auc-1234567812344abc8def123456789abc',
                      source: 'auction_api',
                      terminal_status: 'completed',
                      provider_calls: [],
                      slots: [
                        {
                          slot_number: 1,
                          slot_ref: nextPayload.adUnits[0].ext.trusted_server.trace_slot_ref,
                          requested_sizes: [[300, 250]],
                          returned_bid_count: 0,
                          candidate: 'no_candidate',
                        },
                      ],
                      truncation: {
                        omitted_provider_calls: 0,
                        omitted_slots: 0,
                        omitted_nested_values: 0,
                      },
                      coverage: { provider_to_slot_no_bid: 'unavailable' },
                    },
                  },
                },
              },
            }),
            { status: 200, headers: { 'Content-Type': 'application/json' } }
          )
        );
        await next;
        const capture = pageWindow.tsjs.traceEvidence.snapshot().value;
        expect(capture.serverAuctions).toHaveLength(1);
        expect(capture.issues).toEqual(['evidence_transport_failed', 'correlation_unavailable']);
        expect(capture.slotCorrelations).toEqual([]);
        expectNoUnexpectedNetworkActivity(stubs);
      } finally {
        dom.window.dispatchEvent(new dom.window.PageTransitionEvent('pagehide'));
        dom.window.close();
      }
    },
    10000
  );
});

// These are native compatibility controls, not Auctioneer serving acceptance.
// A future selected-result projection must preserve these publisher/client
// semantics without relying on private Prebid methods or copying its buckets.
describe('pinned Prebid public targeting contract', () => {
  const code = 'example-targeting-slot';
  const seat = 'exampleSeat';
  const price = 1.239;

  function gptSlot(id = code) {
    const values = { section: ['example-section'] };
    return {
      values,
      getSlotElementId: () => id,
      getAdUnitPath: () => `/example/${id}`,
      getSizes: () => [{ getWidth: () => 300, getHeight: () => 250 }],
      getTargeting: (key) => values[key] ?? [],
      getTargetingKeys: () => Object.keys(values),
      setTargeting(key, value) {
        values[key] = Array.isArray(value) ? value.map(String) : [String(value)];
        return this;
      },
      clearTargeting(key) {
        delete values[key];
        return this;
      },
      updateTargetingFromMap: vi.fn(function (map) {
        for (const [key, value] of Object.entries(map)) {
          if (value === null) this.clearTargeting(key);
          else this.setTargeting(key, value);
        }
        return this;
      }),
    };
  }

  function serverResponse(cpm = price) {
    return {
      id: 'example-auction',
      seatbid: [
        {
          seat,
          bid: [
            {
              id: 'example-server-bid',
              impid: code,
              price: cpm,
              w: 300,
              h: 250,
              crid: 'example-creative',
              adomain: ['example.com'],
              adm: '<div>Example server creative</div>',
            },
          ],
        },
      ],
    };
  }

  function targetingPage({
    config = {},
    settings = {},
    client,
    clientCode = seat,
    slots = [gptSlot()],
    respond,
  } = {}) {
    const dom = createPage();
    const pageWindow = dom.window;
    const stubs = installNetworkAndConsoleStubs(pageWindow);
    installServerState(pageWindow);
    const element = pageWindow.document.createElement('div');
    element.id = code;
    pageWindow.document.body.appendChild(element);
    const pubads = { getSlots: () => slots, refresh: vi.fn() };
    pageWindow.googletag = {
      apiReady: true,
      pubadsReady: true,
      cmd: { push: (callback) => callback() },
      pubads: () => pubads,
    };
    pageWindow.eval(noAnalyticsArtifact.bundleCode);
    pageWindow.pbjs.setConfig({ bidderTimeout: 1000, ...config });
    pageWindow.pbjs.bidderSettings = settings;
    let clientBid = client;
    pageWindow.pbjs.registerBidAdapter(undefined, clientCode, {
      code: clientCode,
      supportedMediaTypes: ['banner'],
      isBidRequestValid: () => true,
      buildRequests: (requests) => ({
        method: 'GET',
        url: 'https://pub.example.com/example-client-bid',
        exampleRequestId: requests[0].bidId,
      }),
      interpretResponse: (_response, request) =>
        clientBid === undefined
          ? []
          : [
              {
                requestId: request.exampleRequestId,
                cpm: clientBid.cpm,
                width: 300,
                height: 250,
                ad: '<div>Example client creative</div>',
                ttl: 300,
                creativeId: 'example-client-creative',
                netRevenue: true,
                currency: 'USD',
                ...(clientBid.targeting ? { adserverTargeting: clientBid.targeting } : {}),
              },
            ],
    });
    let responseCount = 0;

    stubs.fetchSpy.mockImplementation(async (resource, init) => {
      stubs.requests.push({ transport: 'fetch', resource, init });
      const pathname = new URL(requestUrl(resource), 'https://pub.example.com').pathname;
      if (pathname === '/example-client-bid') return new Response('{}');
      if (pathname !== '/auction') throw new Error(`Unexpected fixture path: ${pathname}`);
      const requestBody = init?.body ?? (await resource.text());
      const payload = JSON.parse(requestBody);
      const body = respond ? await respond(payload, responseCount++) : serverResponse();
      return new Response(JSON.stringify(body), {
        status: 200,
        headers: { 'Content-Type': 'application/json' },
      });
    });
    pageWindow.eval(shimCode);
    const targeted = [];
    pageWindow.pbjs.onEvent('setTargeting', (value) => targeted.push(value));
    return {
      dom,
      pageWindow,
      stubs,
      slots,
      targeted,
      setClientBid: (value) => {
        clientBid = value;
      },
      auction: () =>
        new Promise((resolve) =>
          pageWindow.pbjs.requestBids({
            adUnits: [
              {
                code,
                mediaTypes: { banner: { sizes: [[300, 250]] } },
                bids: [
                  ...(client === undefined ? [] : [{ bidder: clientCode, params: {} }]),
                  { bidder: 'trustedServer', params: {} },
                ],
              },
            ],
            timeout: 1000,
            bidsBackHandler: (responses, timedOut, auctionId) => {
              resolve({ responses, timedOut, auctionId });
            },
          })
        ),
      bids: () => pageWindow.pbjs.getBidResponsesForAdUnitCode(code).bids,
      async target(matching) {
        const previous = targeted.length;
        pageWindow.pbjs.setTargetingForGPTAsync([code], matching);
        await vi.waitFor(() => expect(targeted.length).toBe(previous + 1));
        return slots[0].values;
      },
      close() {
        try {
          expectNoUnexpectedNetworkActivity(stubs);
          expect(stubs.consoleErrors).toEqual([]);
        } finally {
          dom.window.close();
        }
      },
    };
  }

  it.each([
    ['low', '1.00'],
    ['medium', '1.20'],
    ['high', '1.23'],
    ['auto', '1.20'],
    ['dense', '1.23'],
    [{ buckets: [{ max: 20, increment: 0.25 }] }, '1.00'],
  ])('preserves native remote buckets for %j granularity', async (granularity, expected) => {
    const page = targetingPage({ config: { priceGranularity: granularity } });
    try {
      await page.auction();
      const bids = page.bids();
      expect(bids).toHaveLength(1);
      expect(bids[0]).toMatchObject({ bidderCode: seat, adapterCode: 'trustedServer', cpm: price });
      const targeting = await page.target();
      expect(targeting.hb_pb).toEqual([expected]);
      expect(targeting.hb_bidder).toEqual([seat]);
      expect(targeting.hb_adid).toEqual([bids[0].adId]);
      expect(targeting.section).toEqual(['example-section']);
      expect(page.slots[0].updateTargetingFromMap).toHaveBeenCalled();
    } finally {
      page.close();
    }
  });

  it.each([
    ['low', 5.01, '5.00'],
    ['medium', 20.01, '20.00'],
    ['high', 20.01, '20.00'],
    ['auto', 5, '5.00'],
    ['auto', 5.05, '5.00'],
    ['dense', 3, '3.00'],
    ['dense', 3.049, '3.00'],
    [{ buckets: [{ max: 20, increment: 0.25 }] }, 1.25, '1.25'],
    [{ buckets: [{ max: 20, increment: 0.25 }] }, 1.249999, '1.00'],
  ])('preserves native %j boundary at CPM %s', async (granularity, cpm, expected) => {
    const page = targetingPage({
      config: { priceGranularity: granularity },
      respond: () => serverResponse(cpm),
    });
    try {
      const receipt = await page.auction();
      expect(receipt.timedOut).toBe(false);
      expect(page.bids()[0].cpm).toBe(cpm);
      expect((await page.target()).hb_pb).toEqual([expected]);
    } finally {
      page.close();
    }
  });

  it.each([false, true])(
    'preserves the publisher zero-CPM targeting policy %s',
    async (allowZeroCpmBids) => {
      const page = targetingPage({
        settings: { [seat]: { allowZeroCpmBids } },
        respond: () => serverResponse(0),
      });
      try {
        const receipt = await page.auction();
        expect(receipt.timedOut).toBe(false);
        expect(page.bids()[0].cpm).toBe(0);
        const targeting = await page.target();
        if (allowZeroCpmBids) {
          expect(targeting.hb_pb).toEqual(['0.00']);
          expect(targeting.hb_adid).toEqual([page.bids()[0].adId]);
        } else {
          expect(targeting).not.toHaveProperty('hb_pb');
          expect(targeting).not.toHaveProperty('hb_adid');
        }
      } finally {
        page.close();
      }
    }
  );

  it('preserves publisher replacement of the complete standard targeting definition', async () => {
    const publisherPrice = vi.fn((bid) => bid.pbHg);
    const page = targetingPage({
      settings: {
        standard: {
          adserverTargeting: [
            { key: 'example_price', val: publisherPrice },
            { key: 'hb_adid', val: (bid) => bid.adId },
          ],
        },
      },
    });
    try {
      await page.auction();
      const targeting = await page.target();
      expect(publisherPrice).toHaveBeenCalledTimes(1);
      expect(targeting.example_price).toEqual(['1.23']);
      expect(targeting.hb_adid).toEqual([page.bids()[0].adId]);
      expect(targeting).not.toHaveProperty('hb_pb');
      expect(targeting).not.toHaveProperty('hb_bidder');
    } finally {
      page.close();
    }
  });

  it('preserves native banner granularity overrides', async () => {
    const page = targetingPage({
      config: {
        priceGranularity: 'low',
        mediaTypePriceGranularity: { banner: 'high' },
      },
    });
    try {
      await page.auction();
      expect((await page.target()).hb_pb).toEqual(['1.23']);
      expect(page.bids()[0].cpm).toBe(price);
    } finally {
      page.close();
    }
  });

  it('preserves publisher CPM adjustment and the native bucket without rounding CPM', async () => {
    const adjustment = vi.fn((cpm) => cpm * 2);
    const page = targetingPage({
      config: { priceGranularity: 'high' },
      settings: { [seat]: { bidCpmAdjustment: adjustment } },
    });
    try {
      await page.auction();
      expect(adjustment).toHaveBeenCalled();
      expect(page.bids()[0].cpm).toBeCloseTo(2.478, 12);
      expect((await page.target()).hb_pb).toEqual(['2.47']);
    } finally {
      page.close();
    }
  });

  it('looks up targeting by returned seat rather than the Trusted Server adapter code', async () => {
    const adapterPrice = vi.fn(() => '9.99');
    const page = targetingPage({
      settings: {
        trustedServer: { adserverTargeting: [{ key: 'hb_pb', val: adapterPrice }] },
      },
    });
    try {
      await page.auction();
      expect((await page.target()).hb_pb).toEqual(['1.20']);
      expect(page.bids()[0].bidderCode).toBe(seat);
      expect(adapterPrice).not.toHaveBeenCalled();
    } finally {
      page.close();
    }
  });

  it('applies publisher seat callbacks to both remote and same-seat client bids', async () => {
    const publisherPrice = vi.fn((bid) => (bid.adapterCode === 'trustedServer' ? '3.33' : '4.44'));
    const page = targetingPage({
      settings: { [seat]: { adserverTargeting: [{ key: 'hb_pb', val: publisherPrice }] } },
      client: { cpm: 2.5 },
    });
    try {
      await page.auction();
      expect(page.bids()).toHaveLength(2);
      const remote = page.bids().find((bid) => bid.adapterCode === 'trustedServer');
      const client = page.bids().find((bid) => bid.adapterCode === seat);
      expect(remote.bidderCode).toBe(seat);
      expect(client.bidderCode).toBe(seat);
      expect(remote.adserverTargeting.hb_pb).toBe('3.33');
      expect(client.adserverTargeting.hb_pb).toBe('4.44');
      expect(publisherPrice).toHaveBeenCalledTimes(2);
      expect((await page.target()).hb_pb).toEqual(['4.44']);
      expect(page.slots[0].values.hb_adid).toEqual([client.adId]);
    } finally {
      page.close();
    }
  });

  it.each([false, true])(
    'preserves publisher undefined-callback semantics with suppression %s',
    async (suppressEmptyKeys) => {
      const page = targetingPage({
        settings: {
          [seat]: {
            suppressEmptyKeys,
            adserverTargeting: [{ key: 'hb_pb', val: () => undefined }],
          },
        },
      });
      try {
        await page.auction();
        expect(page.bids()[0].adserverTargeting.hb_pb).toBe(suppressEmptyKeys ? '1.20' : undefined);
        await page.target();
        expect(page.slots[0].values.hb_adid).toEqual([page.bids()[0].adId]);
      } finally {
        page.close();
      }
    }
  );

  it('lets native keys replace adapter defaults and truncates custom keys at GPT delivery', async () => {
    const page = targetingPage({
      client: {
        cpm: 2.5,
        targeting: {
          hb_pb: '9.99',
          hb_bidder: 'example-other-seat',
          hb_adid: 'example-adapter-render-id',
          example_price_bucket_long: 'selected-price',
        },
      },
    });
    try {
      await page.auction();
      const client = page.bids().find((bid) => bid.adapterCode === seat);
      const targeting = await page.target();
      expect(client.cpm).toBe(2.5);
      expect(client.bidderCode).toBe(seat);
      expect(targeting.hb_pb).toEqual(['2.50']);
      expect(targeting.hb_bidder).toEqual([seat]);
      expect(targeting.hb_adid).toEqual([client.adId]);
      expect(targeting.example_price_bucket).toEqual(['selected-price']);
      expect(targeting).not.toHaveProperty('example_price_bucket_long');
    } finally {
      page.close();
    }
  });

  it.each([
    [false, seat, false],
    [true, seat, false],
    [false, 'exampleClient', false],
    [true, 'exampleClient', true],
  ])(
    'preserves losing-client custom policy %s for client seat %s',
    async (allBidsCustomTargeting, clientCode, includesCustom) => {
      const page = targetingPage({
        config: { targetingControls: { allBidsCustomTargeting } },
        clientCode,
        client: { cpm: 0.5, targeting: { example_custom: 'publisher-value' } },
      });
      try {
        await page.auction();
        const remote = page.bids().find((bid) => bid.adapterCode === 'trustedServer');
        const targeting = await page.target();
        expect(targeting.hb_adid).toEqual([remote.adId]);
        expect(targeting.hb_pb).toEqual(['1.20']);
        expect(page.bids().length).toBe(2);
        const client = page.bids().find((bid) => bid.adapterCode === clientCode);
        expect(client.cpm).toBe(0.5);
        expect(client.adserverTargeting.example_custom).toBe('publisher-value');
        if (includesCustom) expect(targeting.example_custom).toEqual(['publisher-value']);
        else expect(targeting).not.toHaveProperty('example_custom');
        expect(page.pageWindow.pbjs.getConfig('targetingControls.allBidsCustomTargeting')).toBe(
          allBidsCustomTargeting
        );
      } finally {
        page.close();
      }
    }
  );

  it('applies custom slot matching without exposing matched slot IDs in the public targeting event', async () => {
    const original = gptSlot();
    const custom = gptSlot('example-custom-gpt-slot');
    const page = targetingPage({ slots: [original, custom] });
    try {
      await page.auction();
      await page.target((slot) => (adUnitCode) => slot === custom && adUnitCode === code);
      expect(original.updateTargetingFromMap).not.toHaveBeenCalled();
      expect(custom.values.hb_adid).toEqual([page.bids()[0].adId]);
      expect(Object.keys(page.targeted.at(-1))).toEqual([code]);
      expect(page.targeted.at(-1)[code].hb_adid).toBe(page.bids()[0].adId);
    } finally {
      page.close();
    }
  });

  it('clears native custom keys on refresh while preserving publisher slot context', async () => {
    const page = targetingPage({
      client: { cpm: 2.5, targeting: { example_custom: 'first-winner' } },
    });
    try {
      await page.auction();
      const first = page.bids().find((bid) => bid.adapterCode === seat);
      expect((await page.target()).example_custom).toEqual(['first-winner']);
      page.setClientBid(undefined);
      const second = await page.auction();
      expect(second.timedOut).toBe(false);
      const remote = second.responses[code].bids.find((bid) => bid.adapterCode === 'trustedServer');
      const targeting = await page.target();
      expect(targeting.hb_adid).toEqual([remote.adId]);
      expect(remote.adId).not.toBe(first.adId);
      expect(targeting).not.toHaveProperty('example_custom');
      expect(targeting.section).toEqual(['example-section']);
    } finally {
      page.close();
    }
  });

  it('keeps the newer auction binding when an older same-slot response completes later', async () => {
    let finishFirst;
    const page = targetingPage({
      respond: (_payload, index) =>
        index === 0
          ? new Promise((resolve) => {
              finishFirst = resolve;
            })
          : serverResponse(2.5),
    });
    try {
      const older = page.auction();
      await vi.waitFor(() => expect(finishFirst).toEqual(expect.any(Function)));
      const second = await page.auction();
      expect(second.timedOut).toBe(false);
      const newer = second.responses[code].bids[0];
      expect(newer.cpm).toBe(2.5);
      finishFirst(serverResponse(5));
      const first = await older;
      expect(first.timedOut).toBe(false);
      expect(first.responses[code].bids).toHaveLength(1);
      expect(first.responses[code].bids[0].cpm).toBe(5);
      expect(first.auctionId).not.toBe(second.auctionId);
      expect(first.responses[code].bids[0].adId).not.toBe(newer.adId);
      expect((await page.target()).hb_adid).toEqual([newer.adId]);
      expect(page.slots[0].values.hb_pb).toEqual(['2.50']);
    } finally {
      page.close();
    }
  });
});
