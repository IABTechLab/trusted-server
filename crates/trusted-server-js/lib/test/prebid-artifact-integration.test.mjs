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

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const libDir = path.resolve(__dirname, '..');

let outputDirectory;
let bundleCode;
let shimCode;
let prebidVersion;

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
  const manifest = JSON.parse(fs.readFileSync(path.join(outputDirectory, 'manifest.json'), 'utf8'));
  bundleCode = fs.readFileSync(path.join(outputDirectory, manifest.filename), 'utf8');
  prebidVersion = manifest.prebidVersion;

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
    expect(shimCode.length).toBeLessThan(32_000);
    expect(shimCode).toContain('markWinningBidAsUsed');
  });
});

describe('external bundle + served shim evaluated together', () => {
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
    expect(pageWindow.__tsjs_prebid_bundle.adapters).toEqual(['adf']);
    expect([...pageWindow.__tsjs_prebid_bundle.bidderCodes]).toEqual([
      'adf',
      'adform',
      'adformOpenRTB',
    ]);
    expect([...pageWindow.__tsjs_prebid_bundle.userIdModules]).toEqual(['sharedIdSystem']);

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
