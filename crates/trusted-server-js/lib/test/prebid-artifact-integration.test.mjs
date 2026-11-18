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
    // Trace lifecycle, identity capture and EC route consent reporting keep the
    // shim below 66.5 KB. Importing the separate Prebid runtime (above 200 KB)
    // must fail this guard.
    expect(shimCode.length).toBeLessThan(66_500);
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

describe('identity-only mode through the production Prebid bidder consumer', () => {
  it('captures native IDs without an auction and follows actual native consent filtering', async () => {
    const dom = createPage();
    try {
      const pageWindow = dom.window;
      const stubs = installNetworkAndConsoleStubs(pageWindow);
      installServerState(pageWindow);
      pageWindow.__tsjs_prebid.mode = 'identity';
      pageWindow.eval(managedUserIdArtifact.bundleCode);
      // A publisher ATS callback that has not completed keeps Prebid's actual
      // async initialization pending while the synchronous shared ID is usable.
      pageWindow.ats = { retrieveEnvelope: vi.fn() };
      const pbjs = pageWindow.pbjs;
      const consent = (allowed) => ({
        consentManagement: {
          gdpr: {
            cmpApi: 'static',
            consentData: {
              gdprApplies: true,
              tcString: 'CPexampleConsent',
              purpose: { consents: { 1: allowed }, legitimateInterests: {} },
              vendor: { consents: { 123: allowed }, legitimateInterests: {} },
              publisher: { restrictions: {} },
              specialFeatureOptins: {},
            },
            rules: [{ purpose: 'storage', enforcePurpose: true, enforceVendor: true }],
          },
        },
      });
      pbjs.setConfig({
        ...consent(true),
        gvlMapping: { sharedId: 123, identityLink: 123 },
        userSync: {
          auctionDelay: 0,
          syncDelay: 3000,
          autoRefresh: true,
          userIds: [
            { name: 'sharedId', value: { pubcid: 'no-auction-native' } },
            { name: 'identityLink', params: { pid: 'example-publisher-id', notUse3P: true } },
          ],
        },
      });
      const requestBids = vi.spyOn(pbjs, 'requestBids');
      let asyncIdsSettled = false;
      pbjs.getUserIdsAsync().then(() => {
        asyncIdsSettled = true;
      });
      pageWindow.eval(shimCode);
      const cookieEids = () => {
        const encoded = pageWindow.document.cookie
          .split('; ')
          .find((part) => part.startsWith('ts-eids='))
          ?.slice('ts-eids='.length);
        return encoded === undefined ? undefined : JSON.parse(pageWindow.atob(encoded));
      };
      await expect
        .poll(cookieEids, { timeout: 5000 })
        .toEqual([{ source: 'pubcid.org', uids: [{ id: 'no-auction-native', atype: 1 }] }]);
      expect(asyncIdsSettled).toBe(false);
      expect(pbjs.getConfig('userSync.auctionDelay')).toBe(0);
      const mirror = [{ source: 'mirror.example', uids: [{ id: 'not-native', atype: 1 }] }];
      pageWindow.document.cookie = `__Host-ts-eids-mirror=${Buffer.from(JSON.stringify({ v: 1, eids: mirror })).toString('base64url')}; Path=/; Secure`;
      pbjs.setConfig(consent(false));
      await expect.poll(() => pbjs.getUserIdsAsEids(), { timeout: 5000 }).toEqual([]);
      await expect.poll(cookieEids, { timeout: 5000 }).toBeUndefined();
      pbjs.setConfig(consent(true));
      await expect
        .poll(cookieEids, { timeout: 5000 })
        .toEqual([{ source: 'pubcid.org', uids: [{ id: 'no-auction-native', atype: 1 }] }]);
      expect(requestBids).not.toHaveBeenCalled();
      expect(asyncIdsSettled).toBe(false);
      expect(stubs.requests).toEqual([]);
      expectNoUnexpectedNetworkActivity(stubs);
    } finally {
      dom.window.dispatchEvent(new dom.window.PageTransitionEvent('pagehide'));
      dom.window.close();
    }
  }, 20000);

  it.each([false, true])(
    'keeps mirror data request-local and honors transmitEids denial=%s',
    async (denied) => {
      const dom = createPage();
      try {
        const pageWindow = dom.window;
        const stubs = installNetworkAndConsoleStubs(pageWindow);
        installServerState(pageWindow);
        pageWindow.TextDecoder = TextDecoder;
        pageWindow.__tsjs_prebid.mode = 'identity';
        pageWindow.eval(noAnalyticsArtifact.bundleCode);
        const pbjs = pageWindow.pbjs;
        const received = [];
        const adapterCode = 'exampleIdentityConsumer';
        pbjs.registerBidAdapter(undefined, adapterCode, {
          code: adapterCode,
          gvlid: 123,
          supportedMediaTypes: ['banner'],
          isBidRequestValid: () => true,
          buildRequests(bids, bidderRequest) {
            received.push({
              eids: structuredClone(bidderRequest.ortb2.user?.ext?.eids ?? []),
              bidEids: structuredClone(bids[0].userIdAsEids ?? []),
            });
            return [];
          },
          interpretResponse: () => [],
        });
        pbjs.setConfig({
          bidderTimeout: 50,
          gvlMapping: { sharedId: 123 },
          consentManagement: {
            gdpr: {
              cmpApi: 'static',
              consentData: {
                getTCData: {
                  gdprApplies: true,
                  tcString: 'CPexampleTCStringForTests',
                  purpose: { consents: { 1: true, 2: true, 4: !denied }, legitimateInterests: {} },
                  vendor: { consents: { 123: true }, legitimateInterests: {} },
                  publisher: { restrictions: {} },
                  specialFeatureOptins: {},
                },
              },
              rules: [
                {
                  purpose: 'personalizedAds',
                  enforcePurpose: true,
                  enforceVendor: true,
                  eidsRequireP4Consent: true,
                },
              ],
            },
          },
          userSync: { userIds: [{ name: 'sharedId', value: { pubcid: 'native-fixture-id' } }] },
        });
        const globalPublisher = {
          source: 'global-overlap.example',
          ext: { owner: 'publisher' },
          uids: [{ id: 'global-overlap', atype: 1 }],
        };
        const requestPublisher = {
          source: 'request-overlap.example',
          ext: { owner: 'publisher' },
          uids: [{ id: 'request-overlap', atype: 1 }],
        };
        pbjs.setConfig({ ortb2: { user: { eids: [globalPublisher] } } });
        const originalGlobal = structuredClone(pbjs.getConfig('ortb2'));
        const registration = vi.spyOn(pbjs, 'registerBidAdapter');
        const refresh = vi.fn();
        pageWindow.googletag = {
          cmd: { push: (callback) => callback() },
          pubads: () => ({ refresh }),
        };
        pageWindow.eval(shimCode);
        const mirror = [
          {
            source: 'mirror.example',
            ext: { origin: 'fixture' },
            uids: [{ id: 'server-fixture-id', atype: 1 }],
          },
        ];
        mirror.push(
          { ...globalPublisher, ext: { owner: 'mirror' } },
          { ...requestPublisher, ext: { owner: 'mirror' } }
        );
        pageWindow.document.cookie = `__Host-ts-eids-mirror=${Buffer.from(JSON.stringify({ v: 1, eids: mirror })).toString('base64url')}; Path=/; Secure`;
        const adUnits = [
          {
            code: 'identity-unit',
            mediaTypes: { banner: { sizes: [[300, 250]] } },
            bids: [{ bidder: adapterCode, params: {} }],
          },
        ];
        const requestData = { user: { ext: { eids: [requestPublisher] } } };
        const originalRequest = structuredClone(requestData);
        const run = () =>
          new Promise((resolve) =>
            pbjs.requestBids({ adUnits, ortb2: requestData, bidsBackHandler: resolve })
          );
        await run();
        expect(received).toHaveLength(1);
        const native = pbjs.getUserIdsAsEids();
        expect(native.flatMap((eid) => eid.uids).map((uid) => uid.id)).toContain(
          'native-fixture-id'
        );
        if (denied) {
          expect(received[0]).toEqual({ eids: [], bidEids: [] });
        } else {
          expect(received[0].eids).toEqual(
            expect.arrayContaining([mirror[0], globalPublisher, requestPublisher, ...native])
          );
          expect(received[0].bidEids).toEqual(received[0].eids);
          for (const publisher of [globalPublisher, requestPublisher]) {
            expect(received[0].eids.filter((eid) => eid.source === publisher.source)).toEqual([
              publisher,
            ]);
          }
        }
        const ingestCookie = pageWindow.document.cookie
          .split('; ')
          .find((part) => part.startsWith('ts-eids='));
        expect(JSON.parse(pageWindow.atob(ingestCookie.slice('ts-eids='.length)))).toEqual(native);
        pageWindow.document.cookie = '__Host-ts-eids-mirror=; Path=/; Secure; Max-Age=0';
        await run();
        expect(received).toHaveLength(2);
        expect(received[1].eids.some((eid) => eid.source === 'mirror.example')).toBe(false);
        expect(pbjs.getConfig('ortb2')).toEqual(originalGlobal);
        expect(requestData).toEqual(originalRequest);
        expect(registration).not.toHaveBeenCalled();
        expect(pageWindow.googletag.pubads().refresh).toBe(refresh);
        expect(adUnits[0].bids.map((bid) => bid.bidder)).toEqual([adapterCode]);
        expect(
          stubs.requests.some((request) =>
            requestUrl(request.url ?? request.resource).includes('/auction')
          )
        ).toBe(false);
        expectNoUnexpectedNetworkActivity(stubs);
      } finally {
        dom.window.close();
      }
    }
  );
});
