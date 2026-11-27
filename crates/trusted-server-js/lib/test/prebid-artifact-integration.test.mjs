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
let gptCode;

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
  await build(
    moduleBuildOptions({
      name: 'gpt',
      entryPath: path.join(libDir, 'src/integrations/gpt/index.ts'),
      outDir: outputDirectory,
    })
  );
  gptCode = fs.readFileSync(path.join(outputDirectory, 'tsjs-gpt.js'), 'utf8');
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

  function serverResponse(cpm = price, targeting) {
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
              ...(targeting ? { ext: { prebid: { targeting } } } : {}),
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
    beforeShim,
    core = true,
    adUnitTargeting,
    respondClient,
  } = {}) {
    const dom = createPage();
    const pageWindow = dom.window;
    const stubs = installNetworkAndConsoleStubs(pageWindow);
    installServerState(pageWindow);
    const element = pageWindow.document.createElement('div');
    element.id = code;
    pageWindow.document.body.appendChild(element);
    const nativeRefresh = vi.fn();
    const pubads = { getSlots: () => slots, refresh: nativeRefresh };
    pageWindow.googletag = {
      apiReady: true,
      pubadsReady: true,
      cmd: { push: (callback) => callback() },
      pubads: () => pubads,
    };
    if (core) pageWindow.eval(coreCode);
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
                ...(clientBid.dealId ? { dealId: clientBid.dealId } : {}),
              },
            ],
    });
    let responseCount = 0;

    stubs.fetchSpy.mockImplementation(async (resource, init) => {
      stubs.requests.push({ transport: 'fetch', resource, init });
      const pathname = new URL(requestUrl(resource), 'https://pub.example.com').pathname;
      if (pathname === '/example-client-bid') {
        if (respondClient) await respondClient();
        return new Response('{}');
      }
      if (pathname === '/_ts/page-bids')
        return new Response(JSON.stringify({ slots: [], bids: {} }), {
          headers: { 'Content-Type': 'application/json' },
        });
      if (pathname !== '/auction') throw new Error(`Unexpected fixture path: ${pathname}`);
      const requestBody = init?.body ?? (await resource.text());
      const payload = JSON.parse(requestBody);
      const body = respond ? await respond(payload, responseCount++) : serverResponse();
      return new Response(JSON.stringify(body), {
        status: 200,
        headers: { 'Content-Type': 'application/json' },
      });
    });
    beforeShim?.(pageWindow);
    pageWindow.eval(shimCode);
    const targeted = [];
    pageWindow.pbjs.onEvent('setTargeting', (value) => targeted.push(value));
    return {
      dom,
      pageWindow,
      stubs,
      slots,
      targeted,
      nativeRefresh,
      setClientBid: (value) => {
        clientBid = value;
      },
      auction: () =>
        new Promise((resolve) =>
          pageWindow.pbjs.requestBids({
            adUnits: [
              {
                code,
                ...(adUnitTargeting ? { adserverTargeting: adUnitTargeting } : {}),
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

  it('delivers finalized engine targeting through all public read APIs and GPT', async () => {
    const engine = { hb_pb: '8.88', example_engine: 'selected', example_long_engine_key: 'long' };
    const observed = [];
    const page = targetingPage({
      respond: () => serverResponse(price, engine),
      beforeShim: (win) =>
        win.pbjs.onEvent('setTargeting', () => {
          observed.push(win.googletag.pubads().getSlots()[0].getTargeting('hb_pb'));
        }),
    });
    try {
      await page.auction();
      const prebid = page.pageWindow.pbjs;
      const bid = page.bids()[0];
      expect(bid).toMatchObject({ bidderCode: seat, adapterCode: 'trustedServer', cpm: price });
      expect(prebid.getAdserverTargeting([code])[code]).toMatchObject(engine);
      expect(prebid.getAdserverTargetingForAdUnitCode(code)).toMatchObject(engine);
      expect(
        new URLSearchParams(prebid.getAdserverTargetingForAdUnitCodeStr(code)).get('hb_pb')
      ).toBe('8.88');
      expect((await page.target()).hb_pb).toEqual(['8.88']);
      expect(page.slots[0].values.example_long_engine_key).toEqual(['long']);
      expect(page.slots[0].values.hb_adid).toEqual([bid.adId]);
      expect(observed).toEqual([['8.88']]);
    } finally {
      page.close();
    }
  });

  it('refuses a targeting-dependent bid if early core installation is unavailable', async () => {
    const page = targetingPage({
      core: false,
      respond: () => serverResponse(price, { hb_pb: '8.88' }),
    });
    try {
      await page.auction();
      expect(page.bids().length).toBe(0);
      expect(await page.target()).not.toHaveProperty('hb_pb');
    } finally {
      page.close();
    }
  });

  it('does not project a losing local map onto a same-seat client winner', async () => {
    const page = targetingPage({
      config: { targetingControls: { allBidsCustomTargeting: true } },
      client: { cpm: 3.39 },
      respond: () => serverResponse(price, { hb_pb: '8.88', example_engine: 'loser' }),
    });
    try {
      await page.auction();
      const target = await page.target();
      expect(target.hb_pb).toEqual(['3.30']);
      expect(target).not.toHaveProperty('example_engine');
      expect(page.pageWindow.pbjs.getAdserverTargetingForAdUnitCode(code)).not.toHaveProperty(
        'example_engine'
      );
    } finally {
      page.close();
    }
  });

  it('uses the native cached winning bid rather than the latest response', async () => {
    const page = targetingPage({
      config: { useBidCache: true },
      respond: (_payload, index) =>
        index === 0
          ? serverResponse(4.39, { hb_pb: '7.77', example_engine: 'cached' })
          : serverResponse(1.01),
    });
    try {
      await page.auction();
      const cachedId = page.bids()[0].adId;
      await page.auction();
      expect(page.pageWindow.pbjs.getAdserverTargetingForAdUnitCode(code)).toMatchObject({
        hb_adid: cachedId,
        hb_pb: '7.77',
      });
      expect((await page.target()).hb_adid).toEqual([cachedId]);
      expect(page.slots[0].values.example_engine).toEqual(['cached']);
    } finally {
      page.close();
    }
  });

  it('cleans dynamic keys through remote and no-bid refreshes while preserving publisher changes', async () => {
    const slot = gptSlot();
    const page = targetingPage({
      slots: [slot],
      respond: (_payload, index) =>
        index === 0
          ? serverResponse(price, {
              hb_pb: '8.88',
              section: 'engine',
              example_engine: 'selected',
              example_changed: 'selected',
            })
          : index === 1
            ? serverResponse(2.39)
            : { seatbid: [] },
    });
    try {
      await page.auction();
      await page.target();
      expect(slot.values.section).toEqual(['engine']);
      slot.setTargeting('example_changed', 'publisher-change');
      await page.auction();
      await page.target();
      expect(slot.values.hb_pb).toEqual(['2.30']);
      expect(slot.values.section).toEqual(['example-section']);
      expect(slot.values.example_changed).toEqual(['publisher-change']);
      expect(slot.values).not.toHaveProperty('example_engine');
      await page.auction();
      await page.target();
      expect(slot.values).not.toHaveProperty('hb_adid');
      expect(slot.values.section).toEqual(['example-section']);
    } finally {
      page.close();
    }
  });

  it('binds finalized maps to actual path-matched and custom-matched GPT slots', async () => {
    const pathSlot = gptSlot('example-path-div');
    pathSlot.getAdUnitPath = () => code;
    const customSlot = gptSlot('example-custom-div');
    const page = targetingPage({
      slots: [pathSlot, customSlot],
      respond: () => serverResponse(price, { hb_pb: '8.88' }),
    });
    try {
      await page.auction();
      await page.target();
      expect(pathSlot.values.hb_pb).toEqual(['8.88']);
      expect(customSlot.values).not.toHaveProperty('hb_pb');
      const predicates = [];
      const matching = vi.fn(
        (slot) =>
          function (...args) {
            predicates.push(args);
            return slot === customSlot && args[0] === code;
          }
      );
      await page.target(matching);
      expect(matching).toHaveBeenCalledTimes(2);
      expect(predicates).toEqual([
        [code, 0, [code]],
        [code, 0, [code]],
      ]);
      expect(customSlot.values.hb_pb).toEqual(['8.88']);
    } finally {
      page.close();
    }
  });

  it('falls back to native default matching when a custom builder returns a non-function', async () => {
    const page = targetingPage({ respond: () => serverResponse(price, { hb_pb: '8.88' }) });
    try {
      await page.auction();
      expect((await page.target(() => null)).hb_pb).toEqual(['8.88']);
    } finally {
      page.close();
    }
  });

  it('keeps host render identity and own prototype-looking targeting keys', async () => {
    const engine = JSON.parse(
      '{"hb_adid":"engine-id","hb_cache_host":"engine-host","hb_cache_path":"engine-path","ts_initial":"engine-marker","__proto__":"example-own-value","hb_pb":"8.88"}'
    );
    const page = targetingPage({
      settings: { [seat]: { adserverTargeting: [{ key: 'hb_adid', val: 'publisher-id' }] } },
      respond: () => serverResponse(price, engine),
    });
    try {
      await page.auction();
      const targeting = page.pageWindow.pbjs.getAdserverTargetingForAdUnitCode(code);
      expect(targeting.hb_adid).toBe(page.bids()[0].adId);
      expect(Object.hasOwn(targeting, '__proto__')).toBe(true);
      expect(targeting.__proto__).toBe('example-own-value');
      expect(targeting).not.toHaveProperty('hb_cache_host');
      expect(targeting).not.toHaveProperty('hb_cache_path');
      expect(targeting).not.toHaveProperty('ts_initial');
      await page.target();
      expect(page.slots[0].values.hb_adid).toEqual([page.bids()[0].adId]);
    } finally {
      page.close();
    }
  });

  it.each([
    ['removed render ID', { targetingControls: { allowTargetingKeys: ['PRICE_BUCKET'] } }, {}],
    ['targeting character budget', { targetingControls: { auctionKeyMaxChars: 1 } }, {}],
    ['disabled standards', {}, { [seat]: { sendStandardTargeting: false, adserverTargeting: [] } }],
    ['changed targeting lock', { targetingControls: { lock: 'hb_pb' } }, {}],
  ])(
    'refuses only local map bids for %s and keeps the client eligible',
    async (_name, config, settings) => {
      const page = targetingPage({
        config,
        settings,
        client: { cpm: 0.99 },
        clientCode: 'exampleClient',
        respond: () => serverResponse(3.39, { hb_pb: '8.88' }),
      });
      try {
        await page.auction();
        const target = page.pageWindow.pbjs.getAdserverTargetingForAdUnitCode(code);
        expect(target.hb_pb).not.toBe('8.88');
        const local = page.bids().find((bid) => bid.adapterCode === 'trustedServer');
        expect(local === undefined || local.cpm < 0).toBe(true);
        expect(
          page.bids().some((bid) => bid.bidderCode === 'exampleClient' && bid.cpm === 0.99)
        ).toBe(true);
      } finally {
        page.close();
      }
    }
  );

  it('keeps an unchanged canonical render-ID lock compatible', async () => {
    const page = targetingPage({
      config: { targetingControls: { lock: 'hb_adid' } },
      respond: () => serverResponse(price, { hb_pb: '8.88' }),
    });
    try {
      await page.auction();
      expect((await page.target()).hb_pb).toEqual(['8.88']);
    } finally {
      page.close();
    }
  });

  it('invalidates map-dependent preferred deals after a configuration change', async () => {
    const page = targetingPage({
      config: { targetingControls: { alwaysIncludeDeals: true } },
      respond: () => {
        const body = serverResponse(price, { hb_pb: '8.88' });
        body.seatbid[0].bid[0].dealid = 'example-deal';
        return body;
      },
      beforeShim: (win) =>
        win.pbjs.onEvent('bidResponse', (bid) => {
          bid.dealId = 'example-deal';
        }),
    });
    try {
      await page.auction();
      page.pageWindow.pbjs.setConfig({
        targetingControls: { allowTargetingKeys: ['PRICE_BUCKET'], alwaysIncludeDeals: true },
      });
      const target = page.pageWindow.pbjs.getAdserverTargetingForAdUnitCode(code);
      expect(target).not.toHaveProperty('hb_pb');
      expect(Object.keys(target).some((key) => key.startsWith('hb_deal'))).toBe(false);
    } finally {
      page.close();
    }
  });

  it('refuses a response from an older navigation generation', async () => {
    let release;
    const page = targetingPage({
      respond: async () => {
        await new Promise((resolve) => {
          release = resolve;
        });
        return serverResponse(price, { hb_pb: '8.88' });
      },
    });
    try {
      const auction = page.auction();
      await vi.waitFor(() => expect(release).toEqual(expect.any(Function)));
      page.pageWindow.tsjs.navGeneration = 1;
      release();
      await auction;
      expect(page.bids().length).toBe(0);
    } finally {
      page.close();
    }
  });

  it('cleans old slot keys and excludes cached local bids after navigation', async () => {
    const page = targetingPage({
      config: { useBidCache: true },
      respond: (_payload, index) =>
        index === 0
          ? serverResponse(4.39, { hb_pb: '8.88', example_engine: 'selected' })
          : serverResponse(price),
    });
    try {
      await page.auction();
      await page.target();
      page.pageWindow.tsjs.navGeneration = 1;
      await page.auction();
      expect((await page.target()).hb_pb).toEqual(['1.20']);
      expect(page.slots[0].values).not.toHaveProperty('example_engine');
    } finally {
      page.close();
    }
  });

  it('refuses map-dependent bids if core loads after the controlled external bundle', async () => {
    const page = targetingPage({
      core: false,
      beforeShim: (win) => win.eval(coreCode),
      respond: () => serverResponse(price, { hb_pb: '8.88' }),
    });
    try {
      await page.auction();
      expect(page.bids().length).toBe(0);
    } finally {
      page.close();
    }
  });

  it('preserves comma-containing and empty finalized string values in GPT', async () => {
    const engine = { example_comma: 'one,two', example_empty: '' };
    const page = targetingPage({ respond: () => serverResponse(price, engine) });
    try {
      await page.auction();
      expect(page.pageWindow.pbjs.getAdserverTargetingForAdUnitCode(code)).toMatchObject(engine);
      await page.target();
      expect(page.slots[0].values.example_comma).toEqual(['one,two']);
      expect(page.slots[0].values.example_empty).toEqual(['']);
    } finally {
      page.close();
    }
  });

  it('does not reuse a GPT receipt for an AST event from a publisher listener', async () => {
    let nested = false;
    const page = targetingPage({
      config: { targetingControls: { lock: 'hb_adid' } },
      respond: () => serverResponse(price, { hb_pb: '8.88', example_engine: 'selected' }),
      beforeShim: (win) => {
        win.apntag = { setKeywords: vi.fn(), getTag: () => ({ keywords: {} }) };
        win.pbjs.onEvent('setTargeting', () => {
          if (nested) return;
          nested = true;
          win.pbjs.setTargetingForAst([code]);
        });
      },
    });
    try {
      await page.auction();
      page.pageWindow.pbjs.setTargetingForGPTAsync([code]);
      expect(page.targeted).toHaveLength(2);
      expect(page.slots[0].values.example_engine).toEqual(['selected']);
      expect(page.slots[0].values.hb_pb).toEqual(['8.88']);
    } finally {
      page.close();
    }
  });

  it('keeps nested GPT invocations bound to their own matched slots', async () => {
    const first = gptSlot();
    const second = gptSlot('example-nested-div');
    const page = targetingPage({
      slots: [first, second],
      respond: () => serverResponse(price, { hb_pb: '8.88', example_engine: 'selected' }),
    });
    try {
      await page.auction();
      let nested = false;
      page.pageWindow.pbjs.setTargetingForGPTAsync([code], (slot) => {
        if (!nested) {
          nested = true;
          page.pageWindow.pbjs.setTargetingForGPTAsync(
            [code],
            (candidate) => () => candidate === second
          );
        }
        return () => slot === first;
      });
      expect(page.targeted).toHaveLength(2);
      expect(first.values.example_engine).toEqual(['selected']);
      expect(second.values.example_engine).toEqual(['selected']);
      expect(first.values.hb_pb).toEqual(['8.88']);
      expect(second.values.hb_pb).toEqual(['8.88']);
    } finally {
      page.close();
    }
  });

  it('ignores AST events raised inside a custom GPT matching callback', async () => {
    const page = targetingPage({ respond: () => serverResponse(price, { hb_pb: '8.88' }) });
    try {
      await page.auction();
      page.pageWindow.apntag = { setKeywords: vi.fn(), getTag: () => ({ keywords: {} }) };
      page.pageWindow.pbjs.setTargetingForGPTAsync([code], () => {
        page.pageWindow.pbjs.setTargetingForAst([code]);
        return () => true;
      });
      expect(page.slots[0].values.hb_pb).toEqual(['8.88']);
    } finally {
      page.close();
    }
  });

  it.each(['navigation', 'identity', 'lock'])(
    'refuses selected native fallback if matching changes %s prerequisites',
    async (change) => {
      const page = targetingPage({
        respond: () => serverResponse(price, { hb_pb: '8.88', example_engine: 'selected' }),
      });
      try {
        await page.auction();
        page.pageWindow.pbjs.setTargetingForGPTAsync([code], () => {
          if (change === 'navigation') page.pageWindow.tsjs.navGeneration = 1;
          else
            page.pageWindow.pbjs.setConfig({
              targetingControls:
                change === 'identity'
                  ? { allowTargetingKeys: ['PRICE_BUCKET'] }
                  : { lock: 'hb_pb' },
            });
          return () => true;
        });
        expect(page.slots[0].values).not.toHaveProperty('hb_adid');
        expect(page.slots[0].values).not.toHaveProperty('hb_pb');
        expect(page.targeted[0][code]).not.toHaveProperty('hb_pb');
        expect(page.slots[0].values.section).toEqual(['example-section']);
      } finally {
        page.close();
      }
    }
  );

  it('refuses a local map when public ad-unit targeting replaces native render identity', async () => {
    const page = targetingPage({
      adUnitTargeting: { hb_adid: 'example-publisher-id' },
      respond: () => serverResponse(price, { hb_pb: '8.88' }),
    });
    try {
      await page.auction();
      expect(page.bids().every((bid) => bid.adapterCode !== 'trustedServer' || bid.cpm < 0)).toBe(
        true
      );
      const target = page.pageWindow.pbjs.getAdserverTargetingForAdUnitCode(code);
      expect(target.hb_pb).not.toBe('8.88');
      expect(target.hb_pb).not.toBe('1.20');
    } finally {
      page.close();
    }
  });

  it('bounds finalized maps and unit admission without evicting an admitted unit', async () => {
    const page = targetingPage({ respond: () => serverResponse(price, { hb_pb: '8.88' }) });
    try {
      const bridge = page.pageWindow.tsjs.prebidTargeting;
      const prebid = page.pageWindow.pbjs;
      for (let index = 0; index < 1023; index++)
        expect(bridge.admit(prebid, `example-unit-${index}`, {}, 0)).toBeDefined();
      await page.auction();
      expect(bridge.admit(prebid, 'example-overflow', {}, 0)).toBeUndefined();
      expect(bridge.admit(prebid, code, { example_value: 'still-admitted' }, 0)).toBeDefined();
      expect(
        bridge.admit(
          prebid,
          code,
          Object.fromEntries(
            Array.from({ length: 129 }, (_, index) => [`example_${index}`, 'value'])
          ),
          0
        )
      ).toBeUndefined();
      expect(bridge.admit(prebid, code, { example_value: 'x'.repeat(8193) }, 0)).toBeUndefined();
      expect(
        bridge.admit(
          prebid,
          code,
          Object.fromEntries(
            Array.from({ length: 9 }, (_, index) => [`example_${index}`, 'x'.repeat(8192)])
          ),
          0
        )
      ).toBeUndefined();
      expect(bridge.admit(prebid, code, { example_value: 'x'.repeat(8192) }, 0)).toBeDefined();
      expect((await page.target()).hb_pb).toEqual(['8.88']);
    } finally {
      page.close();
    }
  });

  it('invalidates a local bid when navigation changes before its pending auction completes', async () => {
    let release;
    const page = targetingPage({
      config: { useBidCache: true },
      client: { cpm: 0.99 },
      clientCode: 'exampleClient',
      respondClient: () =>
        new Promise((resolve) => {
          release = resolve;
        }),
      respond: () => serverResponse(4.39, { hb_pb: '8.88' }),
    });
    try {
      let received = false;
      page.pageWindow.pbjs.onEvent('bidResponse', (bid) => {
        if (bid.adapterCode === 'trustedServer') received = true;
      });
      const auction = page.auction();
      await vi.waitFor(() => {
        expect(received).toBe(true);
        expect(release).toEqual(expect.any(Function));
      });
      page.pageWindow.tsjs.navGeneration = 1;
      page.pageWindow.pbjs.getAdserverTargetingForAdUnitCode(code);
      release();
      await auction;
      expect((await page.target()).hb_pb).toEqual(['0.90']);
      expect(page.bids().find((bid) => bid.adapterCode === 'trustedServer').cpm).toBeLessThan(0);
    } finally {
      page.close();
    }
  });

  it('retains in-flight refusal identity when matching calls a targeting getter', async () => {
    const page = targetingPage({
      adUnitTargeting: { section: 'example-section' },
      respond: () => serverResponse(price, { hb_pb: '8.88' }),
    });
    try {
      await page.auction();
      page.pageWindow.pbjs.setTargetingForGPTAsync([code], () => {
        page.pageWindow.tsjs.navGeneration = 1;
        page.pageWindow.pbjs.getAdserverTargetingForAdUnitCode(code);
        return () => true;
      });
      expect(page.slots[0].values).not.toHaveProperty('hb_adid');
      expect(page.slots[0].values).not.toHaveProperty('hb_pb');
      expect(page.slots[0].values.section).toEqual(['example-section']);
      expect(page.targeted[0][code].section).toBe('example-section');
    } finally {
      page.close();
    }
  });

  it('preserves publisher ad-unit context when refusing a late local delivery', async () => {
    const page = targetingPage({
      adUnitTargeting: { section: 'example-section', hb_size: 'publisher-size' },
      respond: () => serverResponse(price, { hb_pb: '8.88' }),
    });
    try {
      await page.auction();
      page.pageWindow.pbjs.setTargetingForGPTAsync([code], () => {
        page.pageWindow.tsjs.navGeneration = 1;
        return () => true;
      });
      expect(page.slots[0].values).not.toHaveProperty('hb_adid');
      expect(page.slots[0].values).not.toHaveProperty('hb_pb');
      expect(page.slots[0].values.section).toEqual(['example-section']);
      expect(page.slots[0].values.hb_size).toEqual(['publisher-size']);
    } finally {
      page.close();
    }
  });

  it.each([
    [seat, 'exampleClient'],
    ['exampleLongBidderSeat', 'exampleClient'],
    ['example', 'exampleClient'],
    ['exampleClient', 'example'],
  ])(
    'removes a refused %s native suffix namespace and preserves %s',
    async (localSeat, clientCode) => {
      const page = targetingPage({
        config: { enableSendAllBids: true },
        client: { cpm: 0.99 },
        clientCode,
        respond: () => {
          const body = serverResponse(price, { hb_pb: '8.88' });
          body.seatbid[0].seat = localSeat;
          return body;
        },
        beforeShim: (win) =>
          win.pbjs.onEvent('bidResponse', (bid) => {
            if (bid.adapterCode === 'trustedServer') {
              bid.dealId = 'example-deal';
              bid.adserverTargeting.hb_deal = 'example-deal';
            }
          }),
      });
      try {
        await page.auction();
        const localId = page.bids().find((bid) => bid.adapterCode === 'trustedServer').adId;
        const clientId = page.bids().find((bid) => bid.bidderCode === clientCode).adId;
        page.pageWindow.pbjs.setTargetingForGPTAsync([code], () => {
          page.pageWindow.tsjs.navGeneration = 1;
          page.pageWindow.pbjs.getAdserverTargetingForAdUnitCode(code);
          return () => true;
        });
        const values = Object.values(page.slots[0].values).flat();
        expect(values).toContain('0.90');
        expect(values).toContain(clientId);
        expect(values).toContain(clientCode);
        expect(values).not.toContain(localId);
        expect(values).not.toContain(localSeat);
        expect(values).not.toContain('1.20');
        expect(values).not.toContain('example-deal');
      } finally {
        page.close();
      }
    }
  );

  it('cleans engine keys immediately on actual SPA navigation without another Prebid targeting call', async () => {
    const page = targetingPage({
      adUnitTargeting: { section: 'example-section' },
      respond: () =>
        serverResponse(price, { hb_pb: '8.88', section: 'engine', example_engine: 'selected' }),
    });
    try {
      await page.auction();
      await page.target();
      page.pageWindow.eval(gptCode);
      page.pageWindow.history.pushState({}, '', '/example-next-page');
      expect(page.pageWindow.tsjs.navGeneration).toBe(1);
      expect(page.slots[0].values).not.toHaveProperty('example_engine');
      expect(page.slots[0].values.section).toEqual(['example-section']);
      await vi.waitFor(() =>
        expect(
          page.stubs.requests.some((entry) => requestUrl(entry.resource).includes('/_ts/page-bids'))
        ).toBe(true)
      );
    } finally {
      page.close();
    }
  });

  it('cleans engine keys before forwarding GPT refresh after a synchronous auction failure', async () => {
    const page = targetingPage({
      adUnitTargeting: { section: 'example-section' },
      respond: () =>
        serverResponse(price, { hb_pb: '8.88', section: 'engine', example_engine: 'selected' }),
    });
    try {
      await page.auction();
      await page.target();
      // Consume the publisher's first-impression refresh before exercising its
      // later replacement auction and synchronous-failure fallback.
      page.pageWindow.googletag.pubads().refresh([page.slots[0]]);
      page.nativeRefresh.mockClear();
      const requestBids = vi.fn(() => {
        throw new Error('example refresh failure');
      });
      page.pageWindow.pbjs.requestBids = requestBids;
      page.pageWindow.googletag.pubads().refresh([page.slots[0]]);
      expect(requestBids).toHaveBeenCalledTimes(1);
      expect(page.nativeRefresh).toHaveBeenCalledTimes(1);
      expect(page.slots[0].values).not.toHaveProperty('example_engine');
      expect(page.slots[0].values.section).toEqual(['example-section']);
      expect(page.stubs.consoleErrors).toHaveLength(1);
      expect(page.stubs.consoleErrors[0]).toContain('example refresh failure');
      page.stubs.consoleErrors.splice(0);
    } finally {
      page.close();
    }
  });

  it.each(['adid_exampleClient', 'pb_exampleClient'])(
    'refuses ambiguous native custom bases for local seat %s while preserving foreign targeting',
    async (localSeat) => {
      const page = targetingPage({
        settings: {
          [localSeat]: { adserverTargeting: [{ key: 'hb', val: 'example-custom-value' }] },
        },
        client: { cpm: 0.99 },
        clientCode: 'exampleClient',
        respond: () => {
          const body = serverResponse(price, { hb_pb: '8.88' });
          body.seatbid[0].seat = localSeat;
          return body;
        },
      });
      try {
        await page.auction();
        expect(page.bids().find((bid) => bid.adapterCode === 'trustedServer').cpm).toBeLessThan(0);
        const clientId = page.bids().find((bid) => bid.bidderCode === 'exampleClient').adId;
        const target = page.pageWindow.pbjs.getAdserverTargetingForAdUnitCode(code);
        expect(target).toMatchObject({ hb_pb: '0.90', hb_adid: clientId });
        await page.target();
        const values = Object.values(page.slots[0].values).flat();
        expect(values).toContain('0.90');
        expect(values).toContain(clientId);
        expect(values).not.toContain('8.88');
        expect(values).not.toContain('example-custom-value');
      } finally {
        page.close();
      }
    }
  );

  it.each([
    ['navigation', seat, true],
    ['navigation', 'exampleLongBidderSeat', true],
    ['navigation', seat, false],
    ['failed refresh', seat, true],
    ['failed refresh', 'exampleLongBidderSeat', true],
    ['failed refresh', seat, false],
  ])(
    'cleans native local delivery on %s for %s with sendAll=%s',
    async (lifecycle, localSeat, sendAll) => {
      const page = targetingPage({
        config: { enableSendAllBids: sendAll },
        adUnitTargeting: { section: 'example-section', hb_size: 'publisher-size' },
        client: { cpm: 0.99 },
        clientCode: 'exampleClient',
        respond: () => {
          const body = serverResponse(price, {
            example_engine_pb: '8.88',
            example_engine: 'selected',
            section: 'engine',
            hb_size: 'engine-size',
          });
          body.seatbid[0].seat = localSeat;
          return body;
        },
        beforeShim: (win) =>
          win.pbjs.onEvent('bidResponse', (bid) => {
            if (bid.adapterCode !== 'trustedServer') return;
            bid.dealId = 'example-local-deal';
            Object.assign(bid.adserverTargeting, {
              hb_deal: 'example-local-deal',
              hb_cache_host: 'cache.example.com',
              hb_cache_path: '/example-cache',
              hb_uuid: 'example-local-cache-id',
              hb_cache_id: 'example-local-cache-id',
              hb_acat: 'example-local-category',
              hb_dsp: 'example-local-dsp',
            });
          }),
      });
      try {
        await page.auction();
        await page.target();
        const localId = page.bids().find((bid) => bid.adapterCode === 'trustedServer').adId;
        const clientId = page.bids().find((bid) => bid.bidderCode === 'exampleClient').adId;
        expect(page.slots[0].values.hb_adid).toEqual([localId]);
        expect(page.slots[0].values.hb_pb).toEqual(['1.20']);
        expect(page.slots[0].values.example_engine_pb).toEqual(['8.88']);
        let refreshSnapshot;
        if (lifecycle === 'navigation') {
          page.pageWindow.eval(gptCode);
          page.pageWindow.history.pushState({}, '', '/example-next-owned-page');
          expect(page.pageWindow.tsjs.navGeneration).toBe(1);
        } else {
          page.pageWindow.googletag.pubads().refresh([page.slots[0]]);
          page.nativeRefresh.mockClear();
          page.nativeRefresh.mockImplementation(() => {
            refreshSnapshot = Object.fromEntries(
              Object.entries(page.slots[0].values).map(([key, items]) => [key, [...items]])
            );
          });
          const requestBids = vi.fn(() => {
            throw new Error('example owned refresh failure');
          });
          page.pageWindow.pbjs.requestBids = requestBids;
          page.pageWindow.googletag.pubads().refresh([page.slots[0]]);
          expect(requestBids).toHaveBeenCalledTimes(1);
          expect(page.nativeRefresh).toHaveBeenCalledTimes(1);
          expect(page.stubs.consoleErrors).toHaveLength(1);
          expect(page.stubs.consoleErrors[0]).toContain('example owned refresh failure');
          page.stubs.consoleErrors.splice(0);
        }
        const target = lifecycle === 'failed refresh' ? refreshSnapshot : page.slots[0].values;
        for (const key of [
          'hb_adid',
          'hb_pb',
          'hb_bidder',
          'hb_format',
          'hb_source',
          'hb_deal',
          'hb_cache_host',
          'hb_cache_path',
          'hb_uuid',
          'hb_cache_id',
          'hb_adomain',
          'hb_acat',
          'hb_crid',
          'hb_dsp',
          'example_engine_pb',
          'example_engine',
        ])
          expect(target).not.toHaveProperty(key);
        const values = Object.values(target).flat();
        expect(values).not.toContain(localId);
        expect(values).not.toContain(localSeat);
        expect(values).not.toContain('1.20');
        expect(values).not.toContain('example-local-deal');
        expect(target.section).toEqual(['example-section']);
        expect(target.hb_size).toEqual(['publisher-size']);
        if (sendAll) {
          expect(values).toContain(clientId);
          expect(values).toContain('0.90');
          expect(values).toContain('exampleClient');
        }
      } finally {
        page.close();
      }
    }
  );

  it('clears unsuffixed native winner fields when sendAll is disabled', async () => {
    const page = targetingPage({
      config: { enableSendAllBids: false },
      respond: () => serverResponse(price, { example_engine: 'selected' }),
    });
    try {
      await page.auction();
      await page.target();
      expect(page.slots[0].values.hb_size).toEqual(['300x250']);
      expect(page.slots[0].values.hb_format).toEqual(['banner']);
      page.pageWindow.eval(gptCode);
      page.pageWindow.history.pushState({}, '', '/example-no-suffix-page');
      for (const key of ['hb_size', 'hb_format', 'hb_source', 'hb_bidder', 'hb_pb', 'hb_adid'])
        expect(page.slots[0].values).not.toHaveProperty(key);
    } finally {
      page.close();
    }
  });

  it.each(['client', 'local'])(
    'preserves native custom aggregates with foreign value %s during local cleanup',
    async (foreignValue) => {
      const page = targetingPage({
        config: { targetingControls: { allBidsCustomTargeting: true } },
        settings: { [seat]: { adserverTargeting: [{ key: 'example_shared', val: 'local' }] } },
        client: { cpm: 0.99, targeting: { example_shared: foreignValue } },
        clientCode: 'exampleClient',
        respond: () => serverResponse(price, { example_engine: 'selected' }),
      });
      try {
        await page.auction();
        await page.target();
        const before = [...page.slots[0].values.example_shared];
        expect(before).toContain(foreignValue);
        page.pageWindow.eval(gptCode);
        page.pageWindow.history.pushState({}, '', '/example-shared-page');
        expect(page.slots[0].values.example_shared).toEqual(before);
        expect(page.slots[0].values).not.toHaveProperty('example_engine');
      } finally {
        page.close();
      }
    }
  );

  it('restores a foreign native custom baseline after an engine collision', async () => {
    const page = targetingPage({
      config: { targetingControls: { allBidsCustomTargeting: true } },
      client: { cpm: 0.99, targeting: { example_shared: 'client' } },
      clientCode: 'exampleClient',
      respond: () => serverResponse(price, { example_shared: 'engine' }),
    });
    try {
      await page.auction();
      await page.target();
      expect(page.slots[0].values.example_shared).toEqual(['engine']);
      page.pageWindow.eval(gptCode);
      page.pageWindow.history.pushState({}, '', '/example-foreign-page');
      expect(page.slots[0].values.example_shared).toEqual(['client']);
    } finally {
      page.close();
    }
  });

  it('preserves a new render owner even when its price equals the previous engine value', async () => {
    const page = targetingPage({
      client: { cpm: 0.99 },
      clientCode: 'exampleClient',
      respond: () => serverResponse(price, { hb_pb: '8.88', example_engine: 'selected' }),
    });
    try {
      await page.auction();
      await page.target();
      const clientId = page.bids().find((bid) => bid.bidderCode === 'exampleClient').adId;
      page.slots[0].setTargeting('hb_adid', clientId);
      page.slots[0].setTargeting('hb_pb', '8.88');
      page.pageWindow.eval(gptCode);
      page.pageWindow.history.pushState({}, '', '/example-new-owner-page');
      expect(page.slots[0].values.hb_adid).toEqual([clientId]);
      expect(page.slots[0].values.hb_pb).toEqual(['8.88']);
      expect(page.slots[0].values).not.toHaveProperty('example_engine');
    } finally {
      page.close();
    }
  });

  it('keeps publisher changes to a managed native key during local cleanup', async () => {
    const page = targetingPage({
      respond: () => serverResponse(price, { example_engine: 'selected' }),
    });
    try {
      await page.auction();
      await page.target();
      page.slots[0].setTargeting('hb_pb', 'example-publisher-price');
      page.pageWindow.eval(gptCode);
      page.pageWindow.history.pushState({}, '', '/example-publisher-page');
      expect(page.slots[0].values.hb_pb).toEqual(['example-publisher-price']);
      expect(page.slots[0].values).not.toHaveProperty('hb_adid');
    } finally {
      page.close();
    }
  });

  it.each([false, true])('cleans uniquely local fields when bidder namespaces collide (local suffix last %s)', async (localSuffixLast) => {
    const page = targetingPage({
      config: { enableSendAllBids: true },
      beforeShim: localSuffixLast ? (win) => win.pbjs.onEvent('bidResponse', (bid) => {
        // Equal native CPMs keep response order; native latency selects the local winner.
        bid.timeToRespond = bid.adapterCode === 'trustedServer' ? 1 : 100;
      }) : undefined,
      client: { cpm: localSuffixLast ? price : 0.99 }, clientCode: 'exampleAliasClient',
      respond: () => { const body = serverResponse(price, { example_engine: 'selected' }); body.seatbid[0].seat = 'exampleAliasLocal'; return body; },
    });
    try {
      await page.auction();
      const local = page.bids().find((bid) => bid.adapterCode === 'trustedServer');
      const client = page.bids().find((bid) => bid.bidderCode === 'exampleAliasClient');
      expect(local.cpm).toBe(price);
      expect(page.pageWindow.pbjs.getAdserverTargetingForAdUnitCode(code)).toMatchObject({ hb_pb: '1.20', hb_adid: local.adId, example_engine: 'selected' });
      await page.target();
      const shared = [...page.slots[0].values.hb_adid_exampleAlias];
      expect(shared).toEqual([localSuffixLast ? local.adId : client.adId]);
      page.pageWindow.eval(gptCode);
      page.pageWindow.history.pushState({}, '', '/example-colliding-namespace-page');
      if (shared.includes(local.adId)) expect(page.slots[0].values).not.toHaveProperty('hb_adid_exampleAlias');
      else expect(page.slots[0].values.hb_adid_exampleAlias).toEqual(shared);
      expect(page.slots[0].values).not.toHaveProperty('example_engine');
      const values = Object.values(page.slots[0].values).flat();
      expect(values).not.toContain(local.adId);
      expect(page.slots[0].values).not.toHaveProperty('hb_pb');
      expect(page.slots[0].values).not.toHaveProperty('hb_pb_exampleAliasLo');
      expect(page.slots[0].values.hb_pb_exampleAliasCl).toEqual([localSuffixLast ? '1.20' : '0.90']);
      if (!localSuffixLast) expect(values).not.toContain('1.20');
      else expect(values).not.toContain(client.adId);
      if (!localSuffixLast) expect(values).toContain(client.adId);
    } finally { page.close(); }
  });

  it('preserves a shared bidder metadata key when native render-ID namespaces are distinct', async () => {
    const page = targetingPage({
      config: { enableSendAllBids: true },
      client: { cpm: 0.99 }, clientCode: 'exampleAAA_client',
      respond: () => { const body = serverResponse(price, { example_engine: 'selected' }); body.seatbid[0].seat = 'exampleAAA_local'; return body; },
    });
    try {
      await page.auction(); await page.target();
      const local = page.bids().find((bid) => bid.adapterCode === 'trustedServer');
      const client = page.bids().find((bid) => bid.bidderCode === 'exampleAAA_client');
      expect(local.cpm).toBe(price);
      const shared = [...page.slots[0].values.hb_bidder_exampleAAA];
      page.pageWindow.eval(gptCode);
      page.pageWindow.history.pushState({}, '', '/example-shared-namespace-page');
      expect(page.slots[0].values.hb_bidder_exampleAAA).toEqual(shared);
      const values = Object.values(page.slots[0].values).flat();
      expect(values).not.toContain(local.adId); expect(values).not.toContain('1.20');
      expect(values).toContain(client.adId); expect(values).toContain('0.90');
    } finally { page.close(); }
  });

  it('keeps canonical-only targeting available when sendAll is disabled despite alias prefixes', async () => {
    const page = targetingPage({
      config: { enableSendAllBids: false },
      client: { cpm: 0.99 }, clientCode: 'exampleAliasClient',
      respond: () => { const body = serverResponse(price, { example_engine: 'selected' }); body.seatbid[0].seat = 'exampleAliasLocal'; return body; },
    });
    try {
      await page.auction(); await page.target();
      const local = page.bids().find((bid) => bid.adapterCode === 'trustedServer');
      expect(local.cpm).toBe(price); expect(page.slots[0].values.hb_adid).toEqual([local.adId]);
      expect(page.slots[0].values.example_engine).toEqual(['selected']);
      page.pageWindow.eval(gptCode);
      page.pageWindow.history.pushState({}, '', '/example-canonical-only-page');
      expect(page.slots[0].values).not.toHaveProperty('hb_adid');
      expect(page.slots[0].values).not.toHaveProperty('example_engine');
    } finally { page.close(); }
  });

  it('clears uniquely local suffixes if navigation invalidates a colliding bid during slot matching', async () => {
    const page = targetingPage({
      config: { enableSendAllBids: true },
      client: { cpm: 0.99 }, clientCode: 'exampleAliasClient',
      respond: () => { const body = serverResponse(price, { example_engine: 'selected' }); body.seatbid[0].seat = 'exampleAliasLocal'; return body; },
    });
    try {
      await page.auction();
      const local = page.bids().find((bid) => bid.adapterCode === 'trustedServer');
      const client = page.bids().find((bid) => bid.bidderCode === 'exampleAliasClient');
      await page.target(() => () => { page.pageWindow.tsjs.navGeneration += 1; return true; });
      expect(local.cpm).toBeLessThan(0);
      expect(page.slots[0].values).not.toHaveProperty('example_engine');
      const values = Object.values(page.slots[0].values).flat();
      expect(values).not.toContain(local.adId); expect(values).not.toContain('1.20');
      expect(values).toContain(client.adId); expect(values).toContain('0.90');
    } finally { page.close(); }
  });

  it('retains a current local result despite a historical bidder namespace when bid cache is off', async () => {
    const page = targetingPage({
      config: { enableSendAllBids: true, useBidCache: false },
      client: { cpm: 0.99 }, clientCode: 'exampleAliasClient',
      respond: (_payload, count) => {
        if (count === 0) return { id: 'example-first-auction', seatbid: [] };
        const body = serverResponse(price, { example_engine: 'selected' });
        body.seatbid[0].seat = 'exampleAliasLocal'; return body;
      },
    });
    try {
      await page.auction(); page.setClientBid(undefined); await page.auction();
      const local = page.bids().find((bid) => bid.adapterCode === 'trustedServer');
      expect(page.pageWindow.pbjs.getAdserverTargetingForAdUnitCode(code)).toMatchObject({ hb_adid: local.adId, example_engine: 'selected' });
      await page.target(); expect(local.cpm).toBe(price);
      expect(page.slots[0].values.example_engine).toEqual(['selected']);
    } finally { page.close(); }
  });

  it('delivers nondeal bids with alwaysIncludeDeals and sendAll disabled despite alias prefixes', async () => {
    const page = targetingPage({
      config: { enableSendAllBids: false, targetingControls: { alwaysIncludeDeals: true } },
      client: { cpm: 0.99 }, clientCode: 'exampleAliasClient',
      respond: () => { const body = serverResponse(price, { example_engine: 'selected' }); body.seatbid[0].seat = 'exampleAliasLocal'; return body; },
    });
    try {
      await page.auction(); await page.target();
      const local = page.bids().find((bid) => bid.adapterCode === 'trustedServer');
      expect(local.cpm).toBe(price); expect(page.slots[0].values.hb_adid).toEqual([local.adId]);
      expect(page.slots[0].values.example_engine).toEqual(['selected']);
      expect(page.slots[0].values).not.toHaveProperty('hb_adid_exampleAlias');
    } finally { page.close(); }
  });

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
      const prebid = page.pageWindow.pbjs;
      expect(prebid.getAdserverTargeting([code])[code].hb_pb).toBe(expected);
      expect(prebid.getAdserverTargetingForAdUnitCode(code).hb_pb).toBe(expected);
      expect(
        new URLSearchParams(prebid.getAdserverTargetingForAdUnitCodeStr(code)).get('hb_pb')
      ).toBe(expected);
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

  it.each([
    [
      'standard replacement',
      { standard: { adserverTargeting: [{ key: 'example_custom', val: 'publisher-value' }] } },
      {},
    ],
    [
      'suppressed standard keys',
      {
        [seat]: {
          sendStandardTargeting: false,
          adserverTargeting: [{ key: 'example_custom', val: 'publisher-value' }],
        },
      },
      {},
    ],
    [
      'filtered standard identity',
      {},
      { targetingControls: { allowTargetingKeys: ['PRICE_BUCKET'] } },
    ],
    ['targeting character budget', {}, { targetingControls: { auctionKeyMaxChars: 1 } }],
  ])(
    'preserves publisher omission of native identity through %s',
    async (_label, settings, config) => {
      const page = targetingPage({ settings, config });
      try {
        const receipt = await page.auction();
        expect(receipt.timedOut).toBe(false);
        expect(page.bids().length).toBe(1);
        expect(typeof page.bids()[0].adId).toBe('string');
        const prebid = page.pageWindow.pbjs;
        expect(prebid.getAdserverTargeting([code])[code]).not.toHaveProperty('hb_adid');
        expect(prebid.getAdserverTargetingForAdUnitCode(code)).not.toHaveProperty('hb_adid');
        expect(
          new URLSearchParams(prebid.getAdserverTargetingForAdUnitCodeStr(code)).has('hb_adid')
        ).toBe(false);
        expect(await page.target()).not.toHaveProperty('hb_adid');
      } finally {
        page.close();
      }
    }
  );

  it('preserves publisher-authored render identity without treating it as the generated bid identity', async () => {
    const page = targetingPage({
      settings: {
        [seat]: {
          adserverTargeting: [{ key: 'hb_adid', val: 'example-publisher-identity' }],
        },
      },
    });
    try {
      await page.auction();
      const actual = page.bids()[0].adId;
      expect(actual).not.toBe('example-publisher-identity');
      const prebid = page.pageWindow.pbjs;
      expect(prebid.getAdserverTargeting([code])[code].hb_adid).toBe('example-publisher-identity');
      expect(prebid.getAdserverTargetingForAdUnitCode(code).hb_adid).toBe(
        'example-publisher-identity'
      );
      expect(
        new URLSearchParams(prebid.getAdserverTargetingForAdUnitCodeStr(code)).get('hb_adid')
      ).toBe('example-publisher-identity');
      expect((await page.target()).hb_adid).toEqual(['example-publisher-identity']);
    } finally {
      page.close();
    }
  });

  it('notifies an earlier publisher callback before a later targeting-event overlay', async () => {
    let publisherObserved;
    const page = targetingPage({
      beforeShim: (prebidWindow) => {
        prebidWindow.pbjs.onEvent('setTargeting', () => {
          publisherObserved = [
            ...prebidWindow.googletag.pubads().getSlots()[0].getTargeting('hb_pb'),
          ];
        });
        prebidWindow.pbjs.onEvent('setTargeting', () => {
          prebidWindow.googletag.pubads().getSlots()[0].setTargeting('hb_pb', '9.99');
        });
      },
    });
    try {
      await page.auction();
      expect((await page.target()).hb_pb).toEqual(['9.99']);
      expect(publisherObserved).toEqual(['1.20']);
      expect(page.bids()[0].cpm).toBe(price);
    } finally {
      page.close();
    }
  });

  it('retains preferred deal targeting after an event listener makes its CPM negative', async () => {
    const page = targetingPage({
      clientCode: 'exampleClient',
      client: { cpm: 2.5, dealId: 'example-deal' },
      config: { targetingControls: { alwaysIncludeDeals: true } },
      beforeShim: (prebidWindow) =>
        prebidWindow.pbjs.onEvent('bidResponse', (bid) => {
          if (bid.adapterCode === 'exampleClient') bid.cpm = -1;
        }),
    });
    try {
      await page.auction();
      expect(page.bids().length).toBe(2);
      const client = page.bids().find((bid) => bid.adapterCode === 'exampleClient');
      expect(client.cpm).toBe(-1);
      expect(client.dealId).toBe('example-deal');
      const targeting = await page.target();
      expect(
        Object.entries(targeting)
          .filter(([key]) => key.startsWith('hb_deal_'))
          .flatMap(([, value]) => value)
      ).toContain('example-deal');
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
