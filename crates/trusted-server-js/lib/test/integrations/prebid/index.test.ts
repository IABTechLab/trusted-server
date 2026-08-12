import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';

function apsRenderer() {
  const bid = envelope.seatbid[0].bid[0];
  return {
    type: 'aps' as const,
    version: 1 as const,
    accountId: 'example-account-id',
    bidId: bid.id,
    creativeId: 'fictional-creative-id',
    tagType: 'iframe' as const,
    creativeUrl: bid.ext.creativeurl,
    aaxResponse: btoa(JSON.stringify(envelope)),
    width: bid.w,
    height: bid.h,
  };
}

/**
 * Default external-bundle manifest for tests. Mirrors what the real external
 * Prebid.js bundle stamps on `window.__tsjs_prebid_bundle` (see
 * build-prebid-external.mjs). Individual tests override and restore it.
 */
const DEFAULT_BUNDLE_MANIFEST = {
  adapters: ['rubicon', 'openx', 'exampleBrowser', 'appnexus'],
  bidderCodes: ['rubicon', 'openx', 'exampleBrowser', 'appnexus'],
  userIdModules: ['sharedIdSystem'],
};

/** Loose bid shape used by the requestBids shim tests. */
interface TestBid {
  bidder: string;
  params?: Record<string, unknown>;
}

/** Loose ad unit shape used by the requestBids shim tests. */
interface TestAdUnit {
  code?: string;
  bids?: TestBid[];
}

/** Window properties the prebid shim reads and writes in these tests. */
interface InjectedPrebidTestConfig {
  accountId?: string;
  timeout?: number;
  debug?: boolean;
  bidders?: string[];
  clientSideBidders?: string[];
  excludedGamAdUnitPathSuffixes?: unknown;
}

interface TestGoogletag {
  cmd: { push: (fn: () => void) => void };
  pubads: () => unknown;
}

interface ApsPrebidTestEntry {
  adUnitCode: string;
  markUsed(): void;
}

interface PrebidTestWindow {
  pbjs?: unknown;
  tsjs?: {
    apsPrebidRenderers?: Record<string, ApsPrebidTestEntry>;
    [key: string]: unknown;
  };
  googletag?: TestGoogletag;
  __tsjs_prebid?: InjectedPrebidTestConfig;
  __tsjsPrebidShimInstalled?: boolean;
  __tsjs_prebid_bundle?: unknown;
  __tsjs_prebid_diagnostics?: {
    userIdModules?: {
      includedModules: string[];
      configuredUserIdNames: string[];
      missingConfiguredUserIdNames: string[];
    };
  };
}

const testWindow = window as unknown as PrebidTestWindow;

/** Argument type accepted by the shimmed `pbjs.requestBids`. */
type RequestBidsArg = Parameters<ReturnType<typeof installPrebidNpm>['requestBids']>[0];

/** The bid adapter spec object registered via `pbjs.registerBidAdapter`. */
interface TestAdapterSpec {
  code: string;
  supportedMediaTypes: string[];
  isBidRequestValid: (bid: Record<string, unknown>) => boolean;
  buildRequests: (
    bidRequests: Array<Record<string, unknown>>,
    bidderRequest?: Record<string, unknown>
  ) => {
    method: string;
    url: string;
    data: Record<string, unknown>;
    options: Record<string, unknown>;
  };
  interpretResponse: (
    response: Record<string, unknown>,
    request?: Record<string, unknown>
  ) => Array<Record<string, unknown>>;
}

// Define mocks using vi.hoisted so they exist before the module under test is
// imported. The shim reads Prebid.js from the `window.pbjs` global (owned by
// the external bundle in production), so tests install the mock there instead
// of mocking module imports.
const {
  mockSetConfig,
  mockProcessQueue,
  mockRequestBids,
  mockRegisterBidAdapter,
  mockGetUserIdsAsEids,
  mockGetConfig,
  mockMarkWinningBidAsUsed,
  mockOnEvent,
  mockRemoveAdUnit,
  mockPbjs,
} = vi.hoisted(() => {
  const mockSetConfig = vi.fn();
  const mockProcessQueue = vi.fn();
  const mockRequestBids = vi.fn();
  const mockRegisterBidAdapter = vi.fn();
  const mockMarkWinningBidAsUsed = vi.fn();
  const mockOnEvent = vi.fn();
  const mockGetUserIdsAsEids = vi.fn(
    () => [] as Array<{ source: string; uids?: Array<{ id: string; atype?: number }> }>
  );
  const mockGetConfig = vi.fn();
  const mockRemoveAdUnit = vi.fn((adUnitCode?: string | string[]) => {
    if (!adUnitCode) {
      mockPbjs.adUnits = [];
      return;
    }
    const codes = new Set(Array.isArray(adUnitCode) ? adUnitCode : [adUnitCode]);
    mockPbjs.adUnits = mockPbjs.adUnits.filter((unit) => !codes.has(unit.code));
  });
  const mockPbjs: {
    setConfig: typeof mockSetConfig;
    processQueue: typeof mockProcessQueue;
    requestBids: typeof mockRequestBids;
    registerBidAdapter: typeof mockRegisterBidAdapter;
    getUserIdsAsEids: typeof mockGetUserIdsAsEids;
    getConfig: typeof mockGetConfig;
    removeAdUnit: ReturnType<typeof vi.fn>;
    markWinningBidAsUsed: typeof mockMarkWinningBidAsUsed;
    adUnits: TestAdUnit[];
    setTargetingForGPTAsync?: (adUnitCodes?: string[]) => void;
    [key: string]: unknown;
  } = {
    setConfig: mockSetConfig,
    processQueue: mockProcessQueue,
    requestBids: mockRequestBids,
    registerBidAdapter: mockRegisterBidAdapter,
    getUserIdsAsEids: mockGetUserIdsAsEids,
    getConfig: mockGetConfig,
    markWinningBidAsUsed: mockMarkWinningBidAsUsed,
    onEvent: mockOnEvent,
    removeAdUnit: mockRemoveAdUnit,
    adUnits: [] as TestAdUnit[],
    setTargetingForGPTAsync: undefined as ((adUnitCodes?: string[]) => void) | undefined,
    que: [] as Array<() => void>,
    cmd: [] as Array<() => void>,
  };

  // Install the mock global BEFORE the shim module evaluates — the shim
  // captures `window.pbjs` at module scope.
  const w = globalThis.window as unknown as {
    pbjs?: unknown;
    __tsjs_prebid_bundle?: unknown;
  };
  w.pbjs = mockPbjs;
  w.__tsjs_prebid_bundle = {
    adapters: ['rubicon', 'openx', 'exampleBrowser', 'appnexus'],
    bidderCodes: ['rubicon', 'openx', 'exampleBrowser', 'appnexus'],
    userIdModules: ['sharedIdSystem'],
  };

  return {
    mockSetConfig,
    mockProcessQueue,
    mockRequestBids,
    mockRegisterBidAdapter,
    mockGetUserIdsAsEids,
    mockGetConfig,
    mockMarkWinningBidAsUsed,
    mockOnEvent,
    mockRemoveAdUnit,
    mockMarkWinningBidAsUsed,
    mockOnEvent,
    mockPbjs,
  };
});

import {
  collectBidders,
  getInjectedConfig,
  auctionBidsToPrebidBids,
  installPrebidNpm,
  installRefreshHandler,
} from '../../../src/integrations/prebid/index';
import type { AuctionBid } from '../../../src/core/auction';
import { log } from '../../../src/core/log';
import envelope from '../../fixtures/aps-renderer-v1.json';
import { GptDiagnosticsObserver } from '../../../src/integrations/gpt_diagnostics/observer';
import { GptDiagnosticsStore } from '../../../src/integrations/gpt_diagnostics/store';
import envelope from '../../fixtures/aps-renderer-v1.json';

// installPrebidNpm is a per-page no-op once the sentinel is set (the module
// self-init above already set it), so every test starts from a clean page.
beforeEach(() => {
  delete testWindow.__tsjsPrebidShimInstalled;
});

describe('prebid/collectBidders', () => {
  it('returns empty array for empty ad units', () => {
    expect(collectBidders([])).toEqual([]);
  });

  it('returns empty array for ad units without bids', () => {
    expect(collectBidders([{}, { bids: [] }])).toEqual([]);
  });

  it('collects unique bidders from ad units', () => {
    const adUnits = [
      { bids: [{ bidder: 'appnexus' }, { bidder: 'rubicon' }] },
      { bids: [{ bidder: 'appnexus' }, { bidder: 'openx' }] },
    ];
    const result = collectBidders(adUnits);
    expect(result).toHaveLength(3);
    expect(result).toContain('appnexus');
    expect(result).toContain('rubicon');
    expect(result).toContain('openx');
  });

  it('skips bids without a bidder field', () => {
    const adUnits = [{ bids: [{ bidder: 'kargo' }, {}] }];
    expect(collectBidders(adUnits)).toEqual(['kargo']);
  });
});

describe('prebid/getInjectedConfig', () => {
  afterEach(() => {
    delete testWindow.__tsjs_prebid;
  });

  it('returns undefined when window.__tsjs_prebid is not set', () => {
    expect(getInjectedConfig()).toBeUndefined();
  });

  it('returns the injected config when present', () => {
    testWindow.__tsjs_prebid = { accountId: 'server-42', timeout: 2000 };
    expect(getInjectedConfig()).toEqual({ accountId: 'server-42', timeout: 2000 });
  });
});

describe('prebid/auctionBidsToPrebidBids', () => {
  it('maps AuctionBid[] to Prebid bid response objects', () => {
    const auctionBids: AuctionBid[] = [
      {
        impid: 'div-gpt-1',
        adm: '<div>Ad</div>',
        price: 3.5,
        width: 300,
        height: 250,
        seat: 'appnexus',
        creativeId: 'cr-123',
        adomain: ['example.com'],
      },
    ];
    const bidRequests = [{ adUnitCode: 'div-gpt-1', bidId: 'bid-abc' }];

    const result = auctionBidsToPrebidBids(auctionBids, bidRequests, true);

    expect(result).toHaveLength(1);
    expect(result[0]).toEqual({
      requestId: 'bid-abc',
      cpm: 3.5,
      width: 300,
      height: 250,
      ad: '<div>Ad</div>',
      ttl: 300,
      creativeId: 'cr-123',
      netRevenue: true,
      currency: 'USD',
      bidderCode: 'appnexus',
      meta: { advertiserDomains: ['example.com'] },
    });
  });

  it('preserves an APS renderer without converting it to executable markup', () => {
    const renderer = apsRenderer();
    const auctionBids: AuctionBid[] = [
      {
        impid: 'div-aps',
        adm: '<script>must not become Prebid ad markup</script>',
        renderer,
        price: 1.23,
        width: 300,
        height: 250,
        seat: 'aps',
        creativeId: 'fictional-creative-id',
        adomain: ['advertiser.example'],
      },
    ];

    const result = auctionBidsToPrebidBids(
      auctionBids,
      [{ adUnitCode: 'div-aps', bidId: 'prebid-request-id' }],
      true
    );

    expect(result).toHaveLength(1);
    expect(result[0]).toEqual(
      expect.objectContaining({
        requestId: 'prebid-request-id',
        bidderCode: 'aps',
        ad: '',
        trustedServerRenderer: renderer,
        meta: {
          advertiserDomains: ['advertiser.example'],
          trustedServerRenderer: renderer,
        },
      })
    );
  });

  it('drops an APS bid whose renderer fails admission validation', () => {
    const result = auctionBidsToPrebidBids(
      [
        {
          impid: 'div-aps',
          adm: '',
          renderer: { ...apsRenderer(), aaxResponse: 'invalid' },
          price: 1.23,
          width: 300,
          height: 250,
          seat: 'aps',
          creativeId: 'fictional-creative-id',
          adomain: [],
        },
      ],
      [{ adUnitCode: 'div-aps', bidId: 'prebid-request-id' }],
      true
    );

    expect(result).toEqual([]);
  });

  it('falls back to impid when no matching bidRequest found', () => {
    const auctionBids: AuctionBid[] = [
      {
        impid: 'div-gpt-2',
        adm: '<div>Ad2</div>',
        price: 2.0,
        width: 728,
        height: 90,
        seat: 'rubicon',
        creativeId: 'cr-456',
        adomain: [],
      },
    ];

    const result = auctionBidsToPrebidBids(auctionBids, [], true);

    expect(result).toHaveLength(1);
    expect(result[0].requestId).toBe('div-gpt-2');
    expect(result[0].cpm).toBe(2.0);
  });

  it('handles multiple bids across different impids', () => {
    const auctionBids: AuctionBid[] = [
      {
        impid: 'slot-a',
        adm: '<div>A</div>',
        price: 1.0,
        width: 300,
        height: 250,
        seat: 'bidderA',
        creativeId: 'cr-a',
        adomain: [],
      },
      {
        impid: 'slot-b',
        adm: '<div>B</div>',
        price: 2.0,
        width: 728,
        height: 90,
        seat: 'bidderB',
        creativeId: 'cr-b',
        adomain: ['b.com'],
      },
    ];
    const bidRequests = [
      { adUnitCode: 'slot-a', bidId: 'req-a' },
      { adUnitCode: 'slot-b', bidId: 'req-b' },
    ];

    const result = auctionBidsToPrebidBids(auctionBids, bidRequests, true);

    expect(result).toHaveLength(2);
    expect(result[0].requestId).toBe('req-a');
    expect(result[1].requestId).toBe('req-b');
  });
});

describe('prebid/installPrebidNpm', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    // Reset requestBids to the mock so each test starts fresh
    mockPbjs.requestBids = mockRequestBids;
    mockPbjs.adUnits = [];
    mockGetUserIdsAsEids.mockReset();
    mockGetUserIdsAsEids.mockReturnValue([]);
    mockGetConfig.mockReset();
    document.cookie = 'ts-eids=; Path=/; Max-Age=0';
    delete testWindow.__tsjs_prebid;
    delete testWindow.__tsjs_prebid_diagnostics;
    delete testWindow.tsjs;
    delete mockPbjs['__tsApsBidResponseListenerInstalled'];
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('registers the trustedServer bid adapter', () => {
    installPrebidNpm();

    expect(mockRegisterBidAdapter).toHaveBeenCalledTimes(1);
    expect(mockRegisterBidAdapter).toHaveBeenCalledWith(
      undefined,
      'trustedServer',
      expect.objectContaining({
        code: 'trustedServer',
        supportedMediaTypes: ['banner'],
        isBidRequestValid: expect.any(Function),
        buildRequests: expect.any(Function),
        interpretResponse: expect.any(Function),
      })
    );
  });

  it('registers accepted APS descriptors under Prebid generated ad IDs', () => {
    installPrebidNpm();

    const bidResponseListener = mockOnEvent.mock.calls.find(
      ([eventName]) => eventName === 'bidResponse'
    )?.[1] as ((bid: Record<string, unknown>) => void) | undefined;
    expect(bidResponseListener).toBeTypeOf('function');

    const renderer = apsRenderer();
    bidResponseListener!({
      adapterCode: 'trustedServer',
      bidderCode: 'aps',
      adId: 'prebid-generated-ad-id',
      adUnitCode: 'div-aps',
      ttl: 300,
      trustedServerRenderer: renderer,
    });

    const entry = testWindow.tsjs?.apsPrebidRenderers?.['prebid-generated-ad-id'];
    expect(entry).toEqual(
      expect.objectContaining({
        adUnitCode: 'div-aps',
        renderer,
        expiresAt: expect.any(Number),
        markUsed: expect.any(Function),
      })
    );

    entry?.markUsed();
    expect(mockMarkWinningBidAsUsed).toHaveBeenCalledWith({
      adId: 'prebid-generated-ad-id',
      events: true,
    });
  });

  it('registers a distinct renderer for each of multiple APS bids on one imp', () => {
    installPrebidNpm();

    const bidResponseListener = mockOnEvent.mock.calls.find(
      ([eventName]) => eventName === 'bidResponse'
    )?.[1] as ((bid: Record<string, unknown>) => void) | undefined;
    expect(bidResponseListener).toBeTypeOf('function');

    // Two APS bids for the same imp share a requestId; each built bid must carry
    // its own descriptor so neither registration is lost.
    const firstRenderer = { ...apsRenderer(), creativeId: 'cr-aps-first' };
    const secondRenderer = { ...apsRenderer(), creativeId: 'cr-aps-second' };
    const sharedBid = {
      impid: 'div-aps',
      price: 1.0,
      width: 300,
      height: 250,
      seat: 'aps',
      adomain: [],
    };
    const built = auctionBidsToPrebidBids(
      [
        { ...sharedBid, renderer: firstRenderer, creativeId: 'cr-aps-first' },
        { ...sharedBid, renderer: secondRenderer, creativeId: 'cr-aps-second' },
      ],
      [{ adUnitCode: 'div-aps', bidId: 'req-shared' }],
      true
    );
    expect(built).toHaveLength(2);

    for (const [index, bid] of built.entries()) {
      bidResponseListener!({
        adapterCode: 'trustedServer',
        bidderCode: 'aps',
        adId: `shared-imp-ad-id-${index}`,
        adUnitCode: 'div-aps',
        ttl: 300,
        requestId: bid.requestId,
        meta: bid.meta,
      });
    }

    const registry = testWindow.tsjs.apsPrebidRenderers;
    expect(registry['shared-imp-ad-id-0']).toEqual(
      expect.objectContaining({ renderer: firstRenderer })
    );
    expect(registry['shared-imp-ad-id-1']).toEqual(
      expect.objectContaining({ renderer: secondRenderer })
    );
  });

  it('does not register anything for a stripped bid that carries no meta descriptor', () => {
    installPrebidNpm();

    const bidResponseListener = mockOnEvent.mock.calls.find(
      ([eventName]) => eventName === 'bidResponse'
    )?.[1] as ((bid: Record<string, unknown>) => void) | undefined;
    expect(bidResponseListener).toBeTypeOf('function');

    // First bid registers through the surviving custom-field path.
    bidResponseListener!({
      adapterCode: 'trustedServer',
      bidderCode: 'aps',
      adId: 'surviving-field-ad-id',
      adUnitCode: 'div-aps',
      ttl: 300,
      requestId: 'req-reused',
      trustedServerRenderer: apsRenderer(),
    });
    expect(testWindow.tsjs.apsPrebidRenderers['surviving-field-ad-id']).toBeDefined();

    // A later field-stripped bid reusing the same requestId has no descriptor of its
    // own, so no stale renderer may be registered for it.
    bidResponseListener!({
      adapterCode: 'trustedServer',
      bidderCode: 'aps',
      adId: 'reused-request-ad-id',
      adUnitCode: 'div-aps',
      ttl: 300,
      requestId: 'req-reused',
      meta: { advertiserDomains: [] },
    });
    expect(testWindow.tsjs.apsPrebidRenderers['reused-request-ad-id']).toBeUndefined();
  });

  it('registers and scrubs on bidAccepted before later events can observe the descriptor', () => {
    installPrebidNpm();

    const bidAcceptedListener = mockOnEvent.mock.calls.find(
      ([eventName]) => eventName === 'bidAccepted'
    )?.[1] as ((bid: Record<string, unknown>) => void) | undefined;
    const bidResponseListener = mockOnEvent.mock.calls.find(
      ([eventName]) => eventName === 'bidResponse'
    )?.[1] as ((bid: Record<string, unknown>) => void) | undefined;
    expect(bidAcceptedListener).toBeTypeOf('function');
    expect(bidResponseListener).toBeTypeOf('function');

    const renderer = apsRenderer();
    const [built] = auctionBidsToPrebidBids(
      [
        {
          impid: 'div-aps',
          renderer,
          price: 1.0,
          width: 300,
          height: 250,
          seat: 'aps',
          creativeId: 'cr-aps',
          adomain: [],
        },
      ],
      [{ adUnitCode: 'div-aps', bidId: 'req-accepted' }],
      true
    );

    // Prebid emits bidAccepted and bidResponse with the same in-place-mutated
    // bid object; the bidAccepted pass must register and scrub both carriers.
    const accepted: Record<string, unknown> = {
      ...built,
      adapterCode: 'trustedServer',
      bidderCode: 'aps',
      adId: 'accepted-ad-id',
      adUnitCode: 'div-aps',
    };
    bidAcceptedListener!(accepted);

    expect(testWindow.tsjs.apsPrebidRenderers['accepted-ad-id']).toEqual(
      expect.objectContaining({ adUnitCode: 'div-aps', renderer })
    );
    expect(accepted).not.toHaveProperty('trustedServerRenderer');
    expect(accepted.meta).not.toHaveProperty('trustedServerRenderer');

    // The later bidResponse pass sees the already-scrubbed object and no-ops.
    const warnSpy = vi.spyOn(log, 'warn').mockImplementation(() => {});
    bidResponseListener!(accepted);
    expect(testWindow.tsjs.apsPrebidRenderers['accepted-ad-id']).toEqual(
      expect.objectContaining({ renderer })
    );
    expect(warnSpy).not.toHaveBeenCalled();
  });

  it('tolerates a non-object meta value on the bid', () => {
    installPrebidNpm();

    const bidResponseListener = mockOnEvent.mock.calls.find(
      ([eventName]) => eventName === 'bidResponse'
    )?.[1] as ((bid: Record<string, unknown>) => void) | undefined;
    expect(bidResponseListener).toBeTypeOf('function');

    // A module overwrote meta with a string and there is no top-level field:
    // nothing registers and nothing throws.
    bidResponseListener!({
      adapterCode: 'trustedServer',
      bidderCode: 'aps',
      adId: 'corrupt-meta-ad-id',
      adUnitCode: 'div-aps',
      ttl: 300,
      meta: 'corrupted',
    });
    expect(testWindow.tsjs?.apsPrebidRenderers?.['corrupt-meta-ad-id']).toBeUndefined();

    // With a surviving top-level field the corrupt meta must not block registration.
    bidResponseListener!({
      adapterCode: 'trustedServer',
      bidderCode: 'aps',
      adId: 'corrupt-meta-with-field-ad-id',
      adUnitCode: 'div-aps',
      ttl: 300,
      meta: 'corrupted',
      trustedServerRenderer: apsRenderer(),
    });
    expect(testWindow.tsjs.apsPrebidRenderers['corrupt-meta-with-field-ad-id']).toBeDefined();
  });

  it('makes failed APS renderer registrations ineligible when zero-CPM bids are allowed', () => {
    const warnSpy = vi.spyOn(log, 'warn').mockImplementation(() => {});
    installPrebidNpm();

    const bidResponseListener = mockOnEvent.mock.calls.find(
      ([eventName]) => eventName === 'bidResponse'
    )?.[1] as ((bid: Record<string, unknown>) => void) | undefined;
    const malformedBid: Record<string, unknown> = {
      adapterCode: 'trustedServer',
      bidderCode: 'aps',
      adId: 'malformed-ad-id',
      adUnitCode: 'div-aps',
      ttl: 300,
      cpm: 1.23,
      trustedServerRenderer: { ...apsRenderer(), aaxResponse: 'invalid' },
    };
    bidResponseListener!(malformedBid);
    bidResponseListener!({
      adapterCode: 'publisherAdapter',
      bidderCode: 'aps',
      adId: 'foreign-ad-id',
      adUnitCode: 'div-aps',
      trustedServerRenderer: apsRenderer(),
    });

    expect(testWindow.tsjs?.apsPrebidRenderers?.['malformed-ad-id']).toBeUndefined();
    expect(testWindow.tsjs?.apsPrebidRenderers?.['foreign-ad-id']).toBeUndefined();
    expect(malformedBid).not.toHaveProperty('trustedServerRenderer');
    // Prebid's allowZeroCpmBids path still requires cpm >= 0.
    expect(malformedBid['cpm']).toBe(-1);
    expect(warnSpy).toHaveBeenCalledWith(
      '[tsjs-prebid] rejected APS renderer capability that failed registration'
    );
  });

  it('registers APS renderer via meta when Prebid strips the custom top-level field', () => {
    installPrebidNpm();

    const bidResponseListener = mockOnEvent.mock.calls.find(
      ([eventName]) => eventName === 'bidResponse'
    )?.[1] as ((bid: Record<string, unknown>) => void) | undefined;
    expect(bidResponseListener).toBeTypeOf('function');

    const renderer = apsRenderer();
    const [built] = auctionBidsToPrebidBids(
      [
        {
          impid: 'div-aps',
          renderer,
          price: 1.0,
          width: 300,
          height: 250,
          seat: 'aps',
          creativeId: 'cr-aps',
          adomain: [],
        },
      ],
      [{ adUnitCode: 'div-aps', bidId: 'req-strip' }],
      true
    );

    // Prebid delivered the bid with the custom top-level field REMOVED — only
    // first-class fields (requestId, meta) survive normalization.
    const delivered: Record<string, unknown> = {
      adapterCode: 'trustedServer',
      bidderCode: 'aps',
      adId: 'stripped-field-ad-id',
      adUnitCode: 'div-aps',
      ttl: 300,
      requestId: built.requestId,
      meta: built.meta,
    };
    bidResponseListener!(delivered);

    const entry = testWindow.tsjs?.apsPrebidRenderers?.['stripped-field-ad-id'];
    expect(entry).toEqual(
      expect.objectContaining({ adUnitCode: 'div-aps', renderer, markUsed: expect.any(Function) })
    );
    // The capability is scrubbed from the delivered bid after registration.
    expect(delivered.meta).not.toHaveProperty('trustedServerRenderer');
  });

  it('does not register malformed or non-trusted APS renderer capabilities', () => {
    const warnSpy = vi.spyOn(log, 'warn').mockImplementation(() => {});
    installPrebidNpm();

    const bidResponseListener = mockOnEvent.mock.calls.find(
      ([eventName]) => eventName === 'bidResponse'
    )?.[1] as ((bid: Record<string, unknown>) => void) | undefined;
    const malformedBid: Record<string, unknown> = {
      adapterCode: 'trustedServer',
      bidderCode: 'aps',
      adId: 'malformed-ad-id',
      adUnitCode: 'div-aps',
      ttl: 300,
      trustedServerRenderer: { ...apsRenderer(), aaxResponse: 'invalid' },
    };
    bidResponseListener!(malformedBid);
    bidResponseListener!({
      adapterCode: 'publisherAdapter',
      bidderCode: 'aps',
      adId: 'foreign-ad-id',
      adUnitCode: 'div-aps',
      trustedServerRenderer: apsRenderer(),
    });

    expect(testWindow.tsjs?.apsPrebidRenderers?.['malformed-ad-id']).toBeUndefined();
    expect(testWindow.tsjs?.apsPrebidRenderers?.['foreign-ad-id']).toBeUndefined();
    expect(malformedBid).not.toHaveProperty('trustedServerRenderer');
    expect(warnSpy).toHaveBeenCalledWith(
      '[tsjs-prebid] rejected APS renderer capability that failed registration'
    );
  });

  it('calls setConfig with debug=false by default', () => {
    installPrebidNpm();

    expect(mockSetConfig).toHaveBeenCalledWith(expect.objectContaining({ debug: false }));
  });

  it('respects custom config values', () => {
    installPrebidNpm({
      endpoint: '/custom/auction',
      timeout: 2000,
      debug: true,
    });

    expect(mockSetConfig).toHaveBeenCalledWith(
      expect.objectContaining({ debug: true, bidderTimeout: 2000 })
    );
  });

  it('calls processQueue after configuration', () => {
    installPrebidNpm();
    expect(mockProcessQueue).toHaveBeenCalledTimes(1);
  });

  it('reports the User ID modules selected by the generated bundle', () => {
    installPrebidNpm();

    expect(testWindow.__tsjs_prebid_diagnostics.userIdModules).toEqual({
      includedModules: ['sharedIdSystem'],
      configuredUserIdNames: [],
      missingConfiguredUserIdNames: [],
    });
  });

  it('refreshes late User ID config without repeating missing-module warnings', () => {
    installPrebidNpm();
    mockGetConfig.mockImplementation((key?: string) =>
      key === 'userSync.userIds' ? [{ name: 'sharedId' }, { name: 'pairId' }] : {}
    );
    const warnSpy = vi.spyOn(log, 'warn').mockImplementation(() => {});

    mockPbjs.requestBids({ adUnits: [] });
    mockPbjs.requestBids({ adUnits: [] });

    expect(testWindow.__tsjs_prebid_diagnostics.userIdModules).toEqual({
      includedModules: ['sharedIdSystem'],
      configuredUserIdNames: ['pairId', 'sharedId'],
      missingConfiguredUserIdNames: ['pairId'],
    });
    expect(
      warnSpy.mock.calls.filter(([message]) => String(message).includes('"pairId"'))
    ).toHaveLength(1);
  });

  it('returns the pbjs instance', () => {
    const result = installPrebidNpm();
    expect(result).toBe(mockPbjs);
  });

  it('installs only once per page via the __tsjsPrebidShimInstalled sentinel', () => {
    const first = installPrebidNpm();
    const wrappedRequestBids = mockPbjs.requestBids;
    const second = installPrebidNpm();

    expect(second).toBe(first);
    expect(mockRegisterBidAdapter).toHaveBeenCalledTimes(1);
    expect(mockPbjs.requestBids).toBe(wrappedRequestBids);
    expect(testWindow.__tsjsPrebidShimInstalled).toBe(true);
  });

  it('warns once about an unstamped User ID manifest instead of once per module', () => {
    delete testWindow.__tsjs_prebid_bundle;
    mockGetConfig.mockImplementation((key?: string) =>
      key === 'userSync.userIds' ? [{ name: 'sharedId' }, { name: 'pairId' }] : {}
    );
    const warnSpy = vi.spyOn(log, 'warn').mockImplementation(() => {});

    installPrebidNpm();
    mockPbjs.requestBids({ adUnits: [] });

    expect(testWindow.__tsjs_prebid_diagnostics.userIdModules).toEqual({
      includedModules: [],
      configuredUserIdNames: ['pairId', 'sharedId'],
      missingConfiguredUserIdNames: [],
    });
    const manifestWarnings = warnSpy.mock.calls.filter(([message]) =>
      String(message).includes('did not stamp a User ID module manifest')
    );
    expect(manifestWarnings).toHaveLength(1);
    const moduleWarnings = warnSpy.mock.calls.filter(([message]) =>
      String(message).includes('is not included in the external bundle')
    );
    expect(moduleWarnings).toHaveLength(0);

    testWindow.__tsjs_prebid_bundle = DEFAULT_BUNDLE_MANIFEST;
  });

  describe('adapter spec', () => {
    function getAdapterSpec(): TestAdapterSpec {
      installPrebidNpm();
      return mockRegisterBidAdapter.mock.calls[0][2] as TestAdapterSpec;
    }

    it('isBidRequestValid always returns true', () => {
      const spec = getAdapterSpec();
      expect(spec.isBidRequestValid({})).toBe(true);
    });

    it('buildRequests creates a POST request to /auction', () => {
      const spec = getAdapterSpec();
      const bidRequests = [
        {
          adUnitCode: 'div-gpt-1',
          bidder: 'trustedServer',
          mediaTypes: { banner: { sizes: [[300, 250]] } },
          params: {},
        },
      ];

      const result = spec.buildRequests(bidRequests);

      expect(result.method).toBe('POST');
      expect(result.url).toBe('/auction');
      expect(result.options).toEqual({ contentType: 'application/json' });

      const payload = JSON.parse(result.data);
      expect(payload.adUnits).toHaveLength(1);
      expect(payload.adUnits[0].code).toBe('div-gpt-1');
      expect(payload.eids).toBeUndefined();
    });

    it('buildRequests includes current Prebid EIDs in the /auction payload', () => {
      const spec = getAdapterSpec();
      mockGetUserIdsAsEids.mockReturnValue([
        {
          source: 'id5-sync.com',
          uids: [{ id: 'ID5_abc', atype: 1 }],
        },
        {
          source: 'sharedid.org',
          uids: [{ id: 'shared_123' }, { id: 'shared_456', atype: 3 }],
        },
        {
          source: 'google.com',
          uids: [{ id: 'pair_123', atype: 571187 }],
        },
      ]);

      const result = spec.buildRequests([
        {
          adUnitCode: 'div-gpt-1',
          bidder: 'trustedServer',
          mediaTypes: { banner: { sizes: [[300, 250]] } },
          params: {},
        },
      ]);

      const payload = JSON.parse(result.data);
      expect(payload.eids).toEqual([
        {
          source: 'id5-sync.com',
          uids: [{ id: 'ID5_abc', atype: 1 }],
        },
        {
          source: 'sharedid.org',
          uids: [{ id: 'shared_123' }, { id: 'shared_456', atype: 3 }],
        },
        {
          source: 'google.com',
          uids: [{ id: 'pair_123', atype: 571187 }],
        },
      ]);
    });

    it('buildRequests clears stale ts-eids cookie when current Prebid EIDs are absent', () => {
      const spec = getAdapterSpec();
      document.cookie = 'ts-eids=stale-value';
      mockGetUserIdsAsEids.mockReturnValue([]);

      spec.buildRequests([
        {
          adUnitCode: 'div-gpt-1',
          bidder: 'trustedServer',
          mediaTypes: { banner: { sizes: [[300, 250]] } },
          params: {},
        },
      ]);

      expect(document.cookie).toBe('');
    });

    it('buildRequests preserves uid ext and sanitizes invalid atype values', () => {
      const spec = getAdapterSpec();
      mockGetUserIdsAsEids.mockReturnValue([
        {
          source: 'adserver.org',
          uids: [
            {
              id: 'uid-with-ext',
              atype: 1,
              ext: { provider: 'liveintent.com', rtiPartner: 'TDID' },
            },
            {
              id: 'uid-bad-atype',
              atype: 2_147_483_648,
              ext: { keep: true },
            },
            {
              id: 'uid-float-atype',
              atype: 1.5,
            },
          ],
        },
      ]);

      const result = spec.buildRequests([
        {
          adUnitCode: 'div-gpt-1',
          bidder: 'trustedServer',
          mediaTypes: { banner: { sizes: [[300, 250]] } },
          params: {},
        },
      ]);

      const payload = JSON.parse(result.data);
      expect(payload.eids).toEqual([
        {
          source: 'adserver.org',
          uids: [
            {
              id: 'uid-with-ext',
              atype: 1,
              ext: { provider: 'liveintent.com', rtiPartner: 'TDID' },
            },
            {
              id: 'uid-bad-atype',
              ext: { keep: true },
            },
            {
              id: 'uid-float-atype',
            },
          ],
        },
      ]);
    });

    it('buildRequests uses custom endpoint when configured', () => {
      mockRegisterBidAdapter.mockClear();
      installPrebidNpm({ endpoint: '/custom/auction' });
      const spec = mockRegisterBidAdapter.mock.calls[0][2];

      const result = spec.buildRequests([
        {
          adUnitCode: 'slot1',
          bidder: 'trustedServer',
          mediaTypes: { banner: { sizes: [[300, 250]] } },
        },
      ]);

      expect(result.url).toBe('/custom/auction');
    });

    it('interpretResponse parses seatbid and returns Prebid bids', () => {
      const spec = getAdapterSpec();

      const built = spec.buildRequests([
        {
          adUnitCode: 'div-gpt-1',
          bidId: 'bid-1',
          bidder: 'trustedServer',
          mediaTypes: { banner: { sizes: [[300, 250]] } },
        },
      ]);

      const serverResponse = {
        body: {
          seatbid: [
            {
              seat: 'appnexus',
              bid: [
                {
                  impid: 'div-gpt-1',
                  price: 4.5,
                  adm: '<div>Creative</div>',
                  w: 300,
                  h: 250,
                  crid: 'cr-789',
                  adomain: ['advertiser.com'],
                },
              ],
            },
          ],
        },
      };

      const bids = spec.interpretResponse(serverResponse, built);

      expect(bids).toHaveLength(1);
      expect(bids[0]).toEqual(
        expect.objectContaining({
          requestId: 'bid-1',
          cpm: 4.5,
          width: 300,
          height: 250,
          ad: '<div>Creative</div>',
          currency: 'USD',
          netRevenue: true,
          bidderCode: 'appnexus',
        })
      );
    });

    it('interpretResponse handles empty/missing seatbid', () => {
      const spec = getAdapterSpec();
      const built = spec.buildRequests([]);

      expect(spec.interpretResponse({ body: {} }, built)).toEqual([]);
      expect(spec.interpretResponse({ body: null }, built)).toEqual([]);
      expect(spec.interpretResponse({}, built)).toEqual([]);
    });

    it('keeps request mapping isolated across overlapping auctions', () => {
      const spec = getAdapterSpec();

      const requestA = spec.buildRequests([
        {
          adUnitCode: 'slot-a',
          bidId: 'bid-a',
          bidder: 'trustedServer',
          mediaTypes: { banner: { sizes: [[300, 250]] } },
        },
      ]);
      const requestB = spec.buildRequests([
        {
          adUnitCode: 'slot-b',
          bidId: 'bid-b',
          bidder: 'trustedServer',
          mediaTypes: { banner: { sizes: [[300, 250]] } },
        },
      ]);

      const responseA = {
        body: {
          seatbid: [
            {
              seat: 'appnexus',
              bid: [{ impid: 'slot-a', price: 1.1, adm: '<div>A</div>', w: 300, h: 250 }],
            },
          ],
        },
      };
      const responseB = {
        body: {
          seatbid: [
            {
              seat: 'rubicon',
              bid: [{ impid: 'slot-b', price: 2.2, adm: '<div>B</div>', w: 300, h: 250 }],
            },
          ],
        },
      };

      const bidsA = spec.interpretResponse(responseA, requestA);
      const bidsB = spec.interpretResponse(responseB, requestB);

      expect(bidsA[0].requestId).toBe('bid-a');
      expect(bidsB[0].requestId).toBe('bid-b');
    });
  });

  describe('requestBids shim', () => {
    it('injects trustedServer bidder into every ad unit', () => {
      const pbjs = installPrebidNpm();

      const adUnits = [
        { bids: [{ bidder: 'appnexus', params: {} }] },
        { bids: [{ bidder: 'rubicon', params: {} }] },
      ];
      pbjs.requestBids({ adUnits } as unknown as RequestBidsArg);

      // Each ad unit should have trustedServer added
      for (const unit of adUnits) {
        const hasTsBidder = unit.bids.some((b: TestBid) => b.bidder === 'trustedServer');
        expect(hasTsBidder).toBe(true);
      }

      const trustedServerBid = adUnits[0].bids.find((b: TestBid) => b.bidder === 'trustedServer');
      expect(trustedServerBid.params.bidderParams).toEqual({ appnexus: {} });
      expect(adUnits[0].bids.map((b: TestBid) => b.bidder)).toEqual(['trustedServer']);
      expect(adUnits[1].bids.map((b: TestBid) => b.bidder)).toEqual(['trustedServer']);

      // Should call through to original requestBids
      expect(mockRequestBids).toHaveBeenCalled();
    });

    it('does not duplicate trustedServer if already present', () => {
      const pbjs = installPrebidNpm();

      const adUnits = [{ bids: [{ bidder: 'trustedServer', params: {} }] }];
      pbjs.requestBids({ adUnits } as unknown as RequestBidsArg);

      const tsCount = adUnits[0].bids.filter((b: TestBid) => b.bidder === 'trustedServer').length;
      expect(tsCount).toBe(1);
    });

    it('captures per-bidder params on trustedServer bid', () => {
      const pbjs = installPrebidNpm();

      const adUnits = [
        {
          bids: [
            { bidder: 'appnexus', params: { placementId: 123 } },
            { bidder: 'rubicon', params: { accountId: 'abc' } },
          ],
        },
      ];
      pbjs.requestBids({ adUnits } as unknown as RequestBidsArg);

      const trustedServerBid = adUnits[0].bids.find((b: TestBid) => b.bidder === 'trustedServer');
      expect(trustedServerBid).toBeDefined();
      expect(trustedServerBid.params.bidderParams).toEqual({
        appnexus: { placementId: 123 },
        rubicon: { accountId: 'abc' },
      });
      expect(adUnits[0].bids.map((b: TestBid) => b.bidder)).toEqual(['trustedServer']);
    });

    it('preserves captured bidder params when requestBids runs twice on the same ad unit', () => {
      const pbjs = installPrebidNpm();

      // First auction: inline server-side params supplied by the publisher.
      const adUnits = [
        {
          code: 'div-1',
          bids: [
            { bidder: 'appnexus', params: { placementId: 123 } },
            { bidder: 'rubicon', params: { accountId: 'abc' } },
          ],
        },
      ];
      pbjs.requestBids({ adUnits } as unknown as RequestBidsArg);

      // Second auction (refresh/re-auction) with the SAME ad unit object: the
      // server-side bidder entries were already pruned, so the shim must not
      // overwrite the captured params with an empty object.
      pbjs.requestBids({ adUnits } as unknown as RequestBidsArg);

      const trustedServerBid = adUnits[0].bids.find(
        (b: TestBid) => b.bidder === 'trustedServer'
      ) as TestBid;
      expect(trustedServerBid.params.bidderParams).toEqual({
        appnexus: { placementId: 123 },
        rubicon: { accountId: 'abc' },
      });
    });

    it('adds bids array to ad units that have none', () => {
      const pbjs = installPrebidNpm();

      const adUnits = [{ code: 'div-1' }] as TestAdUnit[];
      pbjs.requestBids({ adUnits } as unknown as RequestBidsArg);

      expect(adUnits[0].bids).toHaveLength(1);
      expect(adUnits[0].bids[0].bidder).toBe('trustedServer');
    });

    it('normalizes a truthy non-array bids value without throwing', () => {
      const pbjs = installPrebidNpm();
      const adUnits = [
        { code: 'example-malformed-slot', bids: { malformed: true } },
      ] as TestAdUnit[];

      expect(() => pbjs.requestBids({ adUnits } as unknown as RequestBidsArg)).not.toThrow();

      expect(adUnits[0].bids).toEqual([{ bidder: 'trustedServer', params: { bidderParams: {} } }]);
    });

    it('includes zone from mediaTypes.banner.name in trustedServer params', () => {
      const pbjs = installPrebidNpm();

      const adUnits = [
        {
          code: 'ad-header-0',
          mediaTypes: { banner: { name: 'header', sizes: [[728, 90]] } },
          bids: [{ bidder: 'kargo', params: { placementId: '_abc' } }],
        },
        {
          code: 'ad-fixed_bottom-0',
          mediaTypes: { banner: { name: 'fixed_bottom', sizes: [[728, 90]] } },
          bids: [{ bidder: 'kargo', params: { placementId: '_def' } }],
        },
      ];
      pbjs.requestBids({ adUnits } as unknown as RequestBidsArg);

      const tsBid0 = adUnits[0].bids.find((b: TestBid) => b.bidder === 'trustedServer') as TestBid;
      expect(tsBid0.params.zone).toBe('header');

      const tsBid1 = adUnits[1].bids.find((b: TestBid) => b.bidder === 'trustedServer') as TestBid;
      expect(tsBid1.params.zone).toBe('fixed_bottom');
    });

    it('omits zone when mediaTypes.banner.name is not set', () => {
      const pbjs = installPrebidNpm();

      const adUnits = [
        {
          code: 'ad-header-0',
          mediaTypes: { banner: { sizes: [[300, 250]] } },
          bids: [{ bidder: 'appnexus', params: {} }],
        },
      ];
      pbjs.requestBids({ adUnits } as unknown as RequestBidsArg);

      const tsBid = adUnits[0].bids.find((b: TestBid) => b.bidder === 'trustedServer') as TestBid;
      expect(tsBid.params.zone).toBeUndefined();
    });

    it('omits zone when ad unit has no mediaTypes', () => {
      const pbjs = installPrebidNpm();

      const adUnits = [{ bids: [{ bidder: 'rubicon', params: {} }] }];
      pbjs.requestBids({ adUnits } as unknown as RequestBidsArg);

      const tsBid = adUnits[0].bids.find((b: TestBid) => b.bidder === 'trustedServer') as TestBid;
      expect(tsBid.params.zone).toBeUndefined();
    });

    it('clears stale zone when existing trustedServer bid is reused', () => {
      const pbjs = installPrebidNpm();

      const adUnits = [
        {
          code: 'ad-header-0',
          mediaTypes: { banner: { name: 'header', sizes: [[300, 250]] } },
          bids: [
            { bidder: 'trustedServer', params: { custom: 'keep' } },
            { bidder: 'kargo', params: { placementId: '_abc' } },
          ],
        },
      ];

      pbjs.requestBids({ adUnits } as unknown as RequestBidsArg);

      let tsBid = adUnits[0].bids.find((b: TestBid) => b.bidder === 'trustedServer') as TestBid;
      expect(tsBid.params.zone).toBe('header');
      expect(tsBid.params.custom).toBe('keep');

      delete adUnits[0].mediaTypes.banner.name;
      pbjs.requestBids({ adUnits } as unknown as RequestBidsArg);

      tsBid = adUnits[0].bids.find((b: TestBid) => b.bidder === 'trustedServer') as TestBid;
      expect(tsBid.params.zone).toBeUndefined();
      expect(tsBid.params.custom).toBe('keep');
    });

    it('falls back to pbjs.adUnits when requestObj has no adUnits', () => {
      const pbjs = installPrebidNpm();

      mockPbjs.adUnits = [{ bids: [{ bidder: 'openx', params: {} }] }] as TestAdUnit[];
      pbjs.requestBids({} as RequestBidsArg);

      const hasTsBidder = (mockPbjs.adUnits[0].bids ?? []).some(
        (b: TestBid) => b.bidder === 'trustedServer'
      );
      expect(hasTsBidder).toBe(true);
    });

    it('syncs a structured ts-eids cookie after bidsBackHandler', () => {
      mockRequestBids.mockImplementation((opts?: { bidsBackHandler?: () => void }) => {
        opts?.bidsBackHandler?.();
      });
      mockGetUserIdsAsEids.mockReturnValue([
        {
          source: 'sharedid.org',
          uids: [
            { id: 'shared_123', atype: 3 },
            { id: 'shared_456', ext: { provider: 'example' } },
          ],
        },
      ]);

      const pbjs = installPrebidNpm();
      pbjs.requestBids({
        adUnits: [{ bids: [{ bidder: 'appnexus', params: {} }] }],
      } as unknown as RequestBidsArg);

      const cookieValue = document.cookie.match(/(?:^|; )ts-eids=([^;]+)/)?.[1];
      expect(cookieValue).toBeDefined();
      expect(JSON.parse(atob(cookieValue!))).toEqual([
        {
          source: 'sharedid.org',
          uids: [
            { id: 'shared_123', atype: 3 },
            { id: 'shared_456', ext: { provider: 'example' } },
          ],
        },
      ]);
    });

    it('clears ts-eids cookie after bidsBackHandler when no current EIDs remain', () => {
      document.cookie = `ts-eids=${btoa(JSON.stringify([{ source: 'sharedid.org', uids: [{ id: 'stale' }] }]))}`;
      mockRequestBids.mockImplementation((opts?: { bidsBackHandler?: () => void }) => {
        opts?.bidsBackHandler?.();
      });
      mockGetUserIdsAsEids.mockReturnValue([]);

      const pbjs = installPrebidNpm();
      pbjs.requestBids({
        adUnits: [{ bids: [{ bidder: 'appnexus', params: {} }] }],
      } as unknown as RequestBidsArg);

      expect(document.cookie).toBe('');
    });
  });
});

describe('prebid/installPrebidNpm with server-injected config', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockPbjs.requestBids = mockRequestBids;
    mockPbjs.adUnits = [];
    mockGetUserIdsAsEids.mockReset();
    mockGetUserIdsAsEids.mockReturnValue([]);
    document.cookie = 'ts-eids=; Path=/; Max-Age=0';
    delete testWindow.__tsjs_prebid;
  });

  afterEach(() => {
    delete testWindow.__tsjs_prebid;
  });

  it('reads timeout and debug from window.__tsjs_prebid', () => {
    testWindow.__tsjs_prebid = { timeout: 1500, debug: true };

    installPrebidNpm();

    expect(mockSetConfig).toHaveBeenCalledWith(
      expect.objectContaining({ debug: true, bidderTimeout: 1500 })
    );
  });

  it('explicit config overrides server-injected values', () => {
    testWindow.__tsjs_prebid = { timeout: 1500, debug: true };

    installPrebidNpm({ timeout: 3000, debug: false });

    expect(mockSetConfig).toHaveBeenCalledWith(
      expect.objectContaining({ debug: false, bidderTimeout: 3000 })
    );
  });

  it('works with no config argument and no injected config', () => {
    installPrebidNpm();

    expect(mockSetConfig).toHaveBeenCalledWith(expect.objectContaining({ debug: false }));
    expect(mockProcessQueue).toHaveBeenCalled();
  });
});

describe('prebid/installRefreshHandler', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockRequestBids.mockReset();
    mockPbjs.requestBids = mockRequestBids;
    mockPbjs.adUnits = [];
    mockPbjs.setTargetingForGPTAsync = undefined;
    testWindow.tsjs = undefined;
    delete testWindow.googletag;
    delete testWindow.__tsjs_prebid;
  });

  afterEach(() => {
    testWindow.tsjs = undefined;
    delete testWindow.googletag;
    delete testWindow.__tsjs_prebid;
  });

  it('builds refresh ad units from injected slot metadata', () => {
    const originalRefresh = vi.fn();
    const gptSlot = {
      getSlotElementId: vi.fn(() => 'div-ad-homepage-header'),
      getTargeting: vi.fn(() => []),
    };
    const pubads = {
      refresh: originalRefresh,
      getSlots: vi.fn(() => [gptSlot]),
    };
    testWindow.googletag = {
      cmd: { push: (fn: () => void) => fn() },
      pubads: () => pubads,
    };
    testWindow.tsjs = {
      adSlots: [
        {
          id: 'homepage_header_ad',
          gam_unit_path: '/123/homepage',
          div_id: 'div-ad-homepage-header',
          formats: [
            [970, 250],
            [728, 90],
          ],
          targeting: { zone: 'homepage', pos: 'atf' },
        },
      ],
    };

    installRefreshHandler(750);
    pubads.refresh();

    expect(mockRequestBids).toHaveBeenCalledWith(
      expect.objectContaining({
        timeout: 750,
        adUnits: [
          expect.objectContaining({
            code: 'div-ad-homepage-header',
            mediaTypes: {
              banner: {
                name: 'homepage',
                sizes: [
                  [970, 250],
                  [728, 90],
                ],
              },
            },
            bids: [{ bidder: 'trustedServer', params: { zone: 'homepage' } }],
          }),
        ],
      })
    );
  });

  it('resolves the exact slot when div_ids share a prefix', () => {
    // Regression: a single find() with a startsWith() clause returned the
    // first slot whose div_id is a prefix of the element id. With div_ids
    // "div-ad" and "div-ad-header", refreshing the "div-ad-header" element
    // must resolve to the header slot, not the shorter prefix slot.
    const originalRefresh = vi.fn();
    const gptSlot = {
      getSlotElementId: vi.fn(() => 'div-ad-header'),
      getTargeting: vi.fn(() => []),
    };
    const pubads = {
      refresh: originalRefresh,
      getSlots: vi.fn(() => [gptSlot]),
    };
    testWindow.googletag = {
      cmd: { push: (fn: () => void) => fn() },
      pubads: () => pubads,
    };
    testWindow.tsjs = {
      adSlots: [
        {
          id: 'prefix_ad',
          gam_unit_path: '/123/prefix',
          div_id: 'div-ad',
          formats: [[300, 250]],
          targeting: { zone: 'prefix' },
        },
        {
          id: 'header_ad',
          gam_unit_path: '/123/header',
          div_id: 'div-ad-header',
          formats: [[970, 250]],
          targeting: { zone: 'header' },
        },
      ],
    };

    installRefreshHandler(750);
    pubads.refresh();

    expect(mockRequestBids).toHaveBeenCalledWith(
      expect.objectContaining({
        adUnits: [
          expect.objectContaining({
            code: 'div-ad-header',
            mediaTypes: {
              banner: {
                name: 'header',
                sizes: [[970, 250]],
              },
            },
          }),
        ],
      })
    );
  });

  it('scopes the GPT targeting call to the refreshed slot code', () => {
    const setTargetingForGPTAsync = vi.fn();
    mockPbjs.setTargetingForGPTAsync = setTargetingForGPTAsync;
    // Run the bidsBackHandler synchronously so the targeting call fires.
    mockRequestBids.mockImplementation((opts?: { bidsBackHandler?: () => void }) => {
      opts?.bidsBackHandler?.();
    });
    const originalRefresh = vi.fn();
    // Only the header slot is refreshed; the footer slot must be untouched.
    const headerSlot = {
      getSlotElementId: vi.fn(() => 'div-ad-header'),
      getTargeting: vi.fn(() => []),
      clearTargeting: vi.fn().mockReturnThis(),
    };
    const pubads = {
      refresh: originalRefresh,
      getSlots: vi.fn(() => [headerSlot]),
    };
    testWindow.googletag = {
      cmd: { push: (fn: () => void) => fn() },
      pubads: () => pubads,
    };
    testWindow.tsjs = {
      adSlots: [
        {
          id: 'header_ad',
          gam_unit_path: '/123/header',
          div_id: 'div-ad-header',
          formats: [[728, 90]],
          targeting: { zone: 'header' },
        },
        {
          id: 'footer_ad',
          gam_unit_path: '/123/footer',
          div_id: 'div-ad-footer',
          formats: [[728, 90]],
          targeting: { zone: 'footer' },
        },
      ],
    };

    installRefreshHandler(750);
    pubads.refresh([headerSlot]);

    expect(setTargetingForGPTAsync).toHaveBeenCalledTimes(1);
    expect(setTargetingForGPTAsync).toHaveBeenCalledWith(['div-ad-header']);
    expect(originalRefresh).toHaveBeenCalledWith([headerSlot], undefined);

    mockPbjs.setTargetingForGPTAsync = undefined;
  });

  it('includes configured client-side bidders in refresh ad units', () => {
    testWindow.__tsjs_prebid = { clientSideBidders: ['rubicon'] };
    // Original publisher ad unit carries a client-side rubicon bid.
    mockPbjs.adUnits = [
      {
        code: 'div-ad-homepage-header',
        bids: [
          { bidder: 'trustedServer', params: {} },
          { bidder: 'rubicon', params: { accountId: 1, siteId: 2, zoneId: 3 } },
        ],
      },
    ];
    const originalRefresh = vi.fn();
    const gptSlot = {
      getSlotElementId: vi.fn(() => 'div-ad-homepage-header'),
      getTargeting: vi.fn(() => []),
    };
    const pubads = {
      refresh: originalRefresh,
      getSlots: vi.fn(() => [gptSlot]),
    };
    testWindow.googletag = {
      cmd: { push: (fn: () => void) => fn() },
      pubads: () => pubads,
    };
    testWindow.tsjs = {
      adSlots: [
        {
          id: 'homepage_header_ad',
          gam_unit_path: '/123/homepage',
          div_id: 'div-ad-homepage-header',
          formats: [[728, 90]],
          targeting: { zone: 'homepage' },
        },
      ],
    };

    installRefreshHandler(750);
    pubads.refresh();

    expect(mockRequestBids).toHaveBeenCalledWith(
      expect.objectContaining({
        adUnits: [
          expect.objectContaining({
            code: 'div-ad-homepage-header',
            bids: [
              { bidder: 'trustedServer', params: { zone: 'homepage' } },
              { bidder: 'rubicon', params: { accountId: 1, siteId: 2, zoneId: 3 } },
            ],
          }),
        ],
      })
    );

    delete testWindow.__tsjs_prebid;
    mockPbjs.adUnits = [];
  });

  it('preserves raw server-side bidder params in refresh ad units', () => {
    // Original publisher ad unit carries an inline server-side appnexus bid that
    // the initial auction has not yet folded into the trustedServer bid.
    mockPbjs.adUnits = [
      {
        code: 'div-ad-homepage-header',
        bids: [{ bidder: 'appnexus', params: { placementId: 12345 } }],
      },
    ];
    const originalRefresh = vi.fn();
    const gptSlot = {
      getSlotElementId: vi.fn(() => 'div-ad-homepage-header'),
      getTargeting: vi.fn(() => []),
    };
    const pubads = {
      refresh: originalRefresh,
      getSlots: vi.fn(() => [gptSlot]),
    };
    testWindow.googletag = {
      cmd: { push: (fn: () => void) => fn() },
      pubads: () => pubads,
    };
    testWindow.tsjs = {
      adSlots: [
        {
          id: 'homepage_header_ad',
          gam_unit_path: '/123/homepage',
          div_id: 'div-ad-homepage-header',
          formats: [[728, 90]],
          targeting: { zone: 'homepage' },
        },
      ],
    };

    installRefreshHandler(750);
    pubads.refresh();

    expect(mockRequestBids).toHaveBeenCalledWith(
      expect.objectContaining({
        adUnits: [
          expect.objectContaining({
            code: 'div-ad-homepage-header',
            bids: [
              {
                bidder: 'trustedServer',
                params: {
                  zone: 'homepage',
                  bidderParams: { appnexus: { placementId: 12345 } },
                },
              },
            ],
          }),
        ],
      })
    );

    mockPbjs.adUnits = [];
  });

  it('recovers params and client-side bids for container-backed slots by injected div_id', () => {
    // A TS-owned GPT slot may be defined on `${div_id}-container`, but the
    // publisher's Prebid ad unit is keyed by the inner div_id. The synthetic
    // refresh code stays the GPT element id (so GPT can match it), while params
    // and client-side bids are recovered from the injected div_id candidate.
    testWindow.__tsjs_prebid = { clientSideBidders: ['rubicon'] };
    mockPbjs.adUnits = [
      {
        code: 'div-ad-x',
        bids: [
          { bidder: 'appnexus', params: { placementId: 12345 } },
          { bidder: 'rubicon', params: { accountId: 1 } },
        ],
      },
    ];
    const originalRefresh = vi.fn();
    const gptSlot = {
      getSlotElementId: vi.fn(() => 'div-ad-x-container'),
      getTargeting: vi.fn(() => []),
    };
    const pubads = {
      refresh: originalRefresh,
      getSlots: vi.fn(() => [gptSlot]),
    };
    testWindow.googletag = {
      cmd: { push: (fn: () => void) => fn() },
      pubads: () => pubads,
    };
    testWindow.tsjs = {
      adSlots: [
        {
          id: 'x_ad',
          gam_unit_path: '/123/x',
          div_id: 'div-ad-x',
          formats: [[728, 90]],
          targeting: { zone: 'homepage' },
        },
      ],
    };

    installRefreshHandler(750);
    pubads.refresh();

    expect(mockRequestBids).toHaveBeenCalledWith(
      expect.objectContaining({
        adUnits: [
          expect.objectContaining({
            // Synthetic refresh code stays the GPT element id, not the div_id.
            code: 'div-ad-x-container',
            bids: [
              {
                bidder: 'trustedServer',
                params: {
                  zone: 'homepage',
                  bidderParams: { appnexus: { placementId: 12345 } },
                },
              },
              { bidder: 'rubicon', params: { accountId: 1 } },
            ],
          }),
        ],
      })
    );

    delete testWindow.__tsjs_prebid;
    mockPbjs.adUnits = [];
  });

  it('recovers server-side bidder params already folded onto the original trustedServer bid', () => {
    // After the initial auction, the requestBids shim has folded the publisher's
    // server-side params into the original ad unit's trustedServer bid. A later
    // refresh must still recover them by code.
    mockPbjs.adUnits = [
      {
        code: 'div-ad-homepage-header',
        bids: [
          {
            bidder: 'trustedServer',
            params: { bidderParams: { appnexus: { placementId: 12345 } } },
          },
        ],
      },
    ];
    const originalRefresh = vi.fn();
    const gptSlot = {
      getSlotElementId: vi.fn(() => 'div-ad-homepage-header'),
      getTargeting: vi.fn(() => []),
    };
    const pubads = {
      refresh: originalRefresh,
      getSlots: vi.fn(() => [gptSlot]),
    };
    testWindow.googletag = {
      cmd: { push: (fn: () => void) => fn() },
      pubads: () => pubads,
    };
    testWindow.tsjs = {
      adSlots: [
        {
          id: 'homepage_header_ad',
          gam_unit_path: '/123/homepage',
          div_id: 'div-ad-homepage-header',
          formats: [[728, 90]],
          targeting: { zone: 'homepage' },
        },
      ],
    };

    installRefreshHandler(750);
    pubads.refresh();

    expect(mockRequestBids).toHaveBeenCalledWith(
      expect.objectContaining({
        adUnits: [
          expect.objectContaining({
            code: 'div-ad-homepage-header',
            bids: [
              {
                bidder: 'trustedServer',
                params: {
                  zone: 'homepage',
                  bidderParams: { appnexus: { placementId: 12345 } },
                },
              },
            ],
          }),
        ],
      })
    );

    mockPbjs.adUnits = [];
  });

  it('auctions refreshed TS initial slots and clears stale TS targeting before refresh', () => {
    const originalRefresh = vi.fn();
    const clearTargeting = vi.fn();
    const gptSlot = {
      getSlotElementId: vi.fn(() => 'div-ad-homepage-header'),
      getTargeting: vi.fn((key: string) => {
        if (key === 'ts_initial') return ['1'];
        if (key === 'zone') return ['homepage'];
        return [];
      }),
      getSizes: vi.fn(() => [
        { getWidth: () => 970, getHeight: () => 250 },
        { getWidth: () => 728, getHeight: () => 90 },
      ]),
      clearTargeting,
    };
    const pubads = {
      refresh: originalRefresh,
      getSlots: vi.fn(() => [gptSlot]),
    };
    const setTargetingForGPTAsync = vi.fn();
    mockPbjs.setTargetingForGPTAsync = setTargetingForGPTAsync;
    testWindow.googletag = {
      cmd: { push: (fn: () => void) => fn() },
      pubads: () => pubads,
    };
    testWindow.tsjs = {
      adSlots: [
        {
          id: 'homepage_header_ad',
          gam_unit_path: '/123/homepage',
          div_id: 'div-ad-homepage-header',
          formats: [
            [970, 250],
            [728, 90],
          ],
          targeting: { zone: 'homepage' },
        },
      ],
    };

    installRefreshHandler(750);
    pubads.refresh([gptSlot]);

    expect(mockRequestBids).toHaveBeenCalledWith(
      expect.objectContaining({
        timeout: 750,
        adUnits: [
          expect.objectContaining({
            code: 'div-ad-homepage-header',
            mediaTypes: {
              banner: {
                name: 'homepage',
                sizes: [
                  [970, 250],
                  [728, 90],
                ],
              },
            },
            bids: [{ bidder: 'trustedServer', params: { zone: 'homepage' } }],
          }),
        ],
      })
    );
    expect(clearTargeting).toHaveBeenCalledWith('ts_initial');
    expect(clearTargeting).toHaveBeenCalledWith('hb_pb');
    expect(clearTargeting).toHaveBeenCalledWith('hb_bidder');
    expect(clearTargeting).toHaveBeenCalledWith('hb_adid');
    expect(clearTargeting).toHaveBeenCalledWith('hb_cache_host');
    expect(clearTargeting).toHaveBeenCalledWith('hb_cache_path');
    expect(originalRefresh).not.toHaveBeenCalled();

    const bidsBackHandler = mockRequestBids.mock.calls[0][0].bidsBackHandler;
    bidsBackHandler();

    expect(setTargetingForGPTAsync).toHaveBeenCalled();
    expect(originalRefresh).toHaveBeenCalledWith([gptSlot], undefined);
  });

  it('passes an explicitly excluded path directly to GPT after clearing stale targeting', () => {
    const originalRefresh = vi.fn();
    const clearTargeting = vi.fn();
    const gptSlot = {
      getSlotElementId: vi.fn(() => 'div-ad-tracking'),
      getAdUnitPath: vi.fn(() => '/123/trackingonly'),
      getTargeting: vi.fn(() => []),
      clearTargeting,
    };
    const pubads = {
      refresh: originalRefresh,
      getSlots: vi.fn(() => [gptSlot]),
    };
    testWindow.googletag = {
      cmd: { push: (fn: () => void) => fn() },
      pubads: () => pubads,
    };
    testWindow.__tsjs_prebid = {
      excludedGamAdUnitPathSuffixes: ['/trackingonly'],
    };
    const options = { changeCorrelator: false };

    installRefreshHandler(750);
    pubads.refresh([gptSlot], options);

    expect(mockRequestBids).not.toHaveBeenCalled();
    expect(clearTargeting).toHaveBeenCalledWith('ts_initial');
    expect(clearTargeting).toHaveBeenCalledWith('hb_pb');
    expect(clearTargeting).toHaveBeenCalledWith('hb_bidder');
    expect(clearTargeting).toHaveBeenCalledWith('hb_adid');
    expect(clearTargeting).toHaveBeenCalledWith('hb_cache_host');
    expect(clearTargeting).toHaveBeenCalledWith('hb_cache_path');
    expect(originalRefresh).toHaveBeenCalledWith([gptSlot], options);
  });

  it('passes an all-excluded global refresh directly to GPT', () => {
    const originalRefresh = vi.fn();
    const trackingSlot = {
      getSlotElementId: vi.fn(() => 'div-ad-tracking'),
      getAdUnitPath: vi.fn(() => '/123/trackingonly'),
      getTargeting: vi.fn(() => []),
      clearTargeting: vi.fn(),
    };
    const measurementSlot = {
      getSlotElementId: vi.fn(() => 'div-ad-measurement'),
      getAdUnitPath: vi.fn(() => '/123/measurement-only'),
      getTargeting: vi.fn(() => []),
      clearTargeting: vi.fn(),
    };
    const targetSlots = [trackingSlot, measurementSlot];
    const pubads = {
      refresh: originalRefresh,
      getSlots: vi.fn(() => targetSlots),
    };
    testWindow.googletag = {
      cmd: { push: (fn: () => void) => fn() },
      pubads: () => pubads,
    };
    testWindow.__tsjs_prebid = {
      excludedGamAdUnitPathSuffixes: ['/trackingonly', '/measurement-only'],
    };
    const options = { changeCorrelator: false };

    installRefreshHandler(750);
    pubads.refresh(undefined, options);

    expect(mockRequestBids).not.toHaveBeenCalled();
    expect(trackingSlot.clearTargeting).toHaveBeenCalled();
    expect(measurementSlot.clearTargeting).toHaveBeenCalled();
    expect(originalRefresh).toHaveBeenCalledWith(undefined, options);
  });

  it('auctions eligible slots and refreshes every slot in a mixed global refresh', () => {
    const setTargetingForGPTAsync = vi.fn();
    mockPbjs.setTargetingForGPTAsync = setTargetingForGPTAsync;
    mockRequestBids.mockImplementation((opts?: { bidsBackHandler?: () => void }) => {
      opts?.bidsBackHandler?.();
    });
    const originalRefresh = vi.fn();
    const displaySlot = {
      getSlotElementId: vi.fn(() => 'div-ad-display'),
      getAdUnitPath: vi.fn(() => '/123/content'),
      getTargeting: vi.fn(() => []),
      clearTargeting: vi.fn(),
    };
    const trackingSlot = {
      getSlotElementId: vi.fn(() => 'div-ad-tracking'),
      getAdUnitPath: vi.fn(() => '/123/trackingonly'),
      getTargeting: vi.fn(() => []),
      clearTargeting: vi.fn(),
    };
    const targetSlots = [displaySlot, trackingSlot];
    const pubads = {
      refresh: originalRefresh,
      getSlots: vi.fn(() => targetSlots),
    };
    testWindow.googletag = {
      cmd: { push: (fn: () => void) => fn() },
      pubads: () => pubads,
    };
    testWindow.__tsjs_prebid = {
      excludedGamAdUnitPathSuffixes: ['/trackingonly'],
    };

    installRefreshHandler(750);
    pubads.refresh();

    expect(displaySlot.clearTargeting).toHaveBeenCalled();
    expect(trackingSlot.clearTargeting).toHaveBeenCalled();
    expect(mockRequestBids).toHaveBeenCalledWith(
      expect.objectContaining({
        adUnits: [expect.objectContaining({ code: 'div-ad-display' })],
      })
    );
    expect(setTargetingForGPTAsync).toHaveBeenCalledWith(['div-ad-display']);
    expect(originalRefresh).toHaveBeenCalledWith(targetSlots, undefined);

    mockPbjs.setTargetingForGPTAsync = undefined;
  });

  it.each([
    ['a missing path getter', {}],
    ['a non-string path', { getAdUnitPath: vi.fn(() => 123) }],
    [
      'a throwing path getter',
      {
        getAdUnitPath: vi.fn(() => {
          throw new Error('path unavailable');
        }),
      },
    ],
  ])('fails open to an auction for %s', (_description, pathBehavior) => {
    const originalRefresh = vi.fn();
    const gptSlot = {
      getSlotElementId: vi.fn(() => 'div-ad-display'),
      getTargeting: vi.fn(() => []),
      ...pathBehavior,
    };
    const pubads = {
      refresh: originalRefresh,
      getSlots: vi.fn(() => [gptSlot]),
    };
    testWindow.googletag = {
      cmd: { push: (fn: () => void) => fn() },
      pubads: () => pubads,
    };
    testWindow.__tsjs_prebid = {
      excludedGamAdUnitPathSuffixes: ['/trackingonly'],
    };

    installRefreshHandler(750);
    pubads.refresh([gptSlot]);

    expect(mockRequestBids).toHaveBeenCalled();
    expect(originalRefresh).not.toHaveBeenCalled();
  });

  it.each([
    ['an empty suffix', ['']],
    ['a non-array suffix list', {}],
  ])('ignores %s from injected config and runs the refresh auction', (_description, suffixes) => {
    const originalRefresh = vi.fn();
    const gptSlot = {
      getSlotElementId: vi.fn(() => 'div-ad-display'),
      getAdUnitPath: vi.fn(() => '/123/content'),
      getTargeting: vi.fn(() => []),
    };
    const pubads = {
      refresh: originalRefresh,
      getSlots: vi.fn(() => [gptSlot]),
    };
    testWindow.googletag = {
      cmd: { push: (fn: () => void) => fn() },
      pubads: () => pubads,
    };
    testWindow.__tsjs_prebid = { excludedGamAdUnitPathSuffixes: suffixes };

    installRefreshHandler(750);
    pubads.refresh([gptSlot]);

    expect(mockRequestBids).toHaveBeenCalled();
    expect(originalRefresh).not.toHaveBeenCalled();
  });

  it.each(['/123/TrackingOnly', '/123/trackingonly/'])(
    'uses literal case-sensitive suffix matching for %s',
    (adUnitPath) => {
      const originalRefresh = vi.fn();
      const gptSlot = {
        getSlotElementId: vi.fn(() => 'div-ad-display'),
        getAdUnitPath: vi.fn(() => adUnitPath),
        getTargeting: vi.fn(() => []),
      };
      const pubads = {
        refresh: originalRefresh,
        getSlots: vi.fn(() => [gptSlot]),
      };
      testWindow.googletag = {
        cmd: { push: (fn: () => void) => fn() },
        pubads: () => pubads,
      };
      testWindow.__tsjs_prebid = {
        excludedGamAdUnitPathSuffixes: ['/trackingonly'],
      };

      installRefreshHandler(750);
      pubads.refresh([gptSlot]);

      expect(mockRequestBids).toHaveBeenCalled();
      expect(originalRefresh).not.toHaveBeenCalled();
    }
  );

  it('passes the adInit internal refresh straight to GPT without a client-side auction', () => {
    const originalRefresh = vi.fn();
    const clearTargeting = vi.fn();
    const gptSlot = {
      getSlotElementId: vi.fn(() => 'div-ad-homepage-header'),
      getTargeting: vi.fn(() => []),
      clearTargeting,
    };
    const pubads = {
      refresh: originalRefresh,
      getSlots: vi.fn(() => [gptSlot]),
    };
    testWindow.googletag = {
      cmd: { push: (fn: () => void) => fn() },
      pubads: () => pubads,
    };
    testWindow.tsjs = { adInitRefreshInProgress: true };

    installRefreshHandler(750);
    pubads.refresh([gptSlot]);

    expect(mockRequestBids).not.toHaveBeenCalled();
    expect(clearTargeting).not.toHaveBeenCalled();
    expect(originalRefresh).toHaveBeenCalledWith([gptSlot], undefined);
  });

  it('runs a client-side auction for publisher refreshes after adInit completes', () => {
    const originalRefresh = vi.fn();
    const gptSlot = {
      getSlotElementId: vi.fn(() => 'div-ad-homepage-header'),
      getTargeting: vi.fn(() => []),
      clearTargeting: vi.fn(),
    };
    const pubads = {
      refresh: originalRefresh,
      getSlots: vi.fn(() => [gptSlot]),
    };
    testWindow.googletag = {
      cmd: { push: (fn: () => void) => fn() },
      pubads: () => pubads,
    };
    testWindow.tsjs = { adInitRefreshInProgress: false };

    installRefreshHandler(750);
    pubads.refresh([gptSlot]);

    expect(mockRequestBids).toHaveBeenCalled();
    expect(originalRefresh).not.toHaveBeenCalled();
  });

  it('keeps nested Prebid refreshes Prebid-only and restores the diagnostics context', () => {
    const listeners = new Map<string, (event: { slot: object }) => void>();
    const store = new GptDiagnosticsStore({ defer: () => undefined });
    const explicitSlot = {
      getSlotElementId: () => 'nested-explicit',
      getTargeting: () => [],
      clearTargeting: vi.fn(),
    };
    const bareSlot = {
      getSlotElementId: () => 'nested-bare',
      getTargeting: () => [],
      clearTargeting: vi.fn(),
    };
    let throwRefresh = false;
    let getSlots: () => object[] = () => [];
    const originalRefresh = vi.fn((slots?: unknown[]) => {
      for (const slot of slots ?? getSlots()) listeners.get('slotRequested')?.({ slot });
      if (throwRefresh) throw new Error('delegated refresh failed');
      return 'delegated refresh result';
    });
    const pubads = {
      addEventListener: vi.fn((name: string, listener: (event: { slot: object }) => void) => {
        listeners.set(name, listener);
      }),
      refresh: originalRefresh,
      getSlots: vi.fn(() => [bareSlot]),
    };
    getSlots = pubads.getSlots;
    testWindow.googletag = {
      cmd: { push: (fn: () => void) => fn() },
      pubads: () => pubads,
    };
    testWindow.tsjs = {
      gptDiagnosticsRecorder: {
        recordPrebidRefresh: (slots: object[]) => store.recordPrebidRefresh(slots),
      },
    };

    new GptDiagnosticsObserver(store).install();
    installRefreshHandler(750);
    const pbjs = installPrebidNpm();

    const prepareDelivery = (code: string) => {
      mockRequestBids.mockImplementationOnce((options) => {
        options.bidsBackHandler?.();
      });
      pbjs.requestBids({
        adUnits: [{ code, bids: [{ bidder: 'exampleServer', params: {} }] }],
        bidsBackHandler: vi.fn(),
      } as unknown as RequestBidsArg);
    };

    prepareDelivery('nested-explicit');
    expect(pubads.refresh([explicitSlot])).toBe('delegated refresh result');
    expect(store.snapshot().slots[0].requests[0].requestPath).toBe('prebid_refresh');
    expect(Object.hasOwn(testWindow.tsjs as object, 'prebidRefreshDispatchInProgress')).toBe(false);

    prepareDelivery('nested-bare');
    expect(pubads.refresh()).toBe('delegated refresh result');
    expect(store.snapshot().slots[1].requests[0].requestPath).toBe('prebid_refresh');
    expect(Object.hasOwn(testWindow.tsjs as object, 'prebidRefreshDispatchInProgress')).toBe(false);

    prepareDelivery('nested-explicit');
    throwRefresh = true;
    expect(() => pubads.refresh([explicitSlot])).toThrow('delegated refresh failed');
    expect(Object.hasOwn(testWindow.tsjs as object, 'prebidRefreshDispatchInProgress')).toBe(false);

    testWindow.tsjs = {
      adInitRefreshInProgress: true,
      gptDiagnosticsRecorder: {
        recordPrebidRefresh: (slots: object[]) => store.recordPrebidRefresh(slots),
      },
    };
    throwRefresh = false;
    expect(pubads.refresh([explicitSlot])).toBe('delegated refresh result');
    expect(store.snapshot().slots[0].requests[2].requestPath).toBe('unattributed');
  });

  it.each([
    { order: 'diagnostics observer first', diagnosticsFirst: true, expectedPath: 'prebid_refresh' },
    { order: 'Prebid wrapper first', diagnosticsFirst: false, expectedPath: 'competing' },
  ])(
    'attributes a Prebid-consumed refresh as $expectedPath when installed with the $order',
    ({ diagnosticsFirst, expectedPath }) => {
      const listeners = new Map<string, (event: { slot: object }) => void>();
      const store = new GptDiagnosticsStore({ defer: () => undefined });
      const slot = {
        getSlotElementId: () => 'install-order',
        getTargeting: () => [],
        clearTargeting: vi.fn(),
      };
      const originalRefresh = vi.fn((slots?: unknown[]) => {
        for (const refreshed of slots ?? []) listeners.get('slotRequested')?.({ slot: refreshed });
        return 'delegated refresh result';
      });
      const pubads = {
        addEventListener: vi.fn((name: string, listener: (event: { slot: object }) => void) => {
          listeners.set(name, listener);
        }),
        refresh: originalRefresh as (slots?: unknown[], opts?: unknown) => unknown,
        getSlots: vi.fn(() => [slot]),
      };
      testWindow.googletag = {
        cmd: { push: (fn: () => void) => fn() },
        pubads: () => pubads,
      };
      testWindow.tsjs = {
        gptDiagnosticsRecorder: {
          recordPrebidRefresh: (slots: object[]) => store.recordPrebidRefresh(slots),
        },
      };

      // Only the bundle evaluation order enforces this today, so pin both
      // outcomes: the diagnostics wrapper must sit inside the Prebid one to see
      // the dispatch context that marks a refresh as Prebid's.
      if (diagnosticsFirst) {
        new GptDiagnosticsObserver(store).install();
        installRefreshHandler(750);
      } else {
        installRefreshHandler(750);
        new GptDiagnosticsObserver(store).install();
      }
      const pbjs = installPrebidNpm();
      mockRequestBids.mockImplementationOnce((options) => options.bidsBackHandler?.());
      pbjs.requestBids({
        adUnits: [{ code: 'install-order', bids: [{ bidder: 'exampleServer', params: {} }] }],
        bidsBackHandler: vi.fn(),
      } as unknown as RequestBidsArg);

      pubads.refresh([slot]);

      expect(store.snapshot().slots[0].requests[0].requestPath).toBe(expectedPath);
    }
  );

  it('keeps the outer dispatch context set across a nested Prebid refresh', () => {
    const store = new GptDiagnosticsStore({ defer: () => undefined });
    const slot = {
      getSlotElementId: () => 'nested-reentrant',
      getTargeting: () => [],
      clearTargeting: vi.fn(),
    };
    const contextAfterInner: Array<boolean | undefined> = [];
    let reentered = false;
    const originalRefresh = vi.fn(() => {
      if (!reentered) {
        reentered = true;
        pubads.refresh([slot]);
        contextAfterInner.push(
          (testWindow.tsjs as { prebidRefreshDispatchInProgress?: boolean })
            .prebidRefreshDispatchInProgress
        );
      }
      return 'delegated refresh result';
    });
    const pubads = {
      refresh: originalRefresh as (slots?: unknown[], opts?: unknown) => unknown,
      getSlots: vi.fn(() => [slot]),
    };
    testWindow.googletag = {
      cmd: { push: (fn: () => void) => fn() },
      pubads: () => pubads,
    };
    testWindow.tsjs = {
      gptDiagnosticsRecorder: {
        recordPrebidRefresh: (slots: object[]) => store.recordPrebidRefresh(slots),
      },
    };

    const pbjs = installPrebidNpm();
    installRefreshHandler(750);
    mockRequestBids.mockImplementation((options) => options.bidsBackHandler?.());
    pbjs.requestBids({
      adUnits: [{ code: 'nested-reentrant', bids: [{ bidder: 'exampleServer', params: {} }] }],
      bidsBackHandler: vi.fn(),
    } as unknown as RequestBidsArg);

    expect(pubads.refresh([slot])).toBe('delegated refresh result');
    // The inner dispatch owns the flag while it runs and must hand it back, or
    // the observer would stop attributing every later publisher refresh.
    expect(contextAfterInner).toEqual([true]);
    expect(
      originalRefresh,
      'the nested refresh must reach the delegated call'
    ).toHaveBeenCalledTimes(2);
    expect(Object.hasOwn(testWindow.tsjs as object, 'prebidRefreshDispatchInProgress')).toBe(false);
  });

  it('restores diagnostics context when its setter mutates and then throws', () => {
    const slot = {
      getSlotElementId: () => 'mutating-context-setter',
      getTargeting: () => [],
      clearTargeting: vi.fn(),
    };
    const originalRefresh = vi.fn();
    const pubads = {
      refresh: originalRefresh,
      getSlots: vi.fn(() => [slot]),
    };
    const contextTarget: Record<string, unknown> = {};
    let throwAfterMutation = true;
    testWindow.tsjs = new Proxy(contextTarget, {
      set(target, property, value) {
        Reflect.set(target, property, value);
        if (property === 'prebidRefreshDispatchInProgress' && throwAfterMutation) {
          throwAfterMutation = false;
          throw new Error('example mutating context setter failure');
        }
        return true;
      },
    });
    testWindow.googletag = {
      cmd: { push: (fn: () => void) => fn() },
      pubads: () => pubads,
    };
    mockRequestBids.mockImplementation((options) => options.bidsBackHandler?.());

    installPrebidNpm();
    installRefreshHandler(750);
    pubads.refresh([slot]);
    pubads.refresh([slot]);

    expect(originalRefresh).toHaveBeenCalledTimes(2);
    expect(
      Object.prototype.hasOwnProperty.call(contextTarget, 'prebidRefreshDispatchInProgress')
    ).toBe(false);
  });
});

describe('prebid publisher snapshots and delivery refreshes', () => {
  let deliveryAdIds = new WeakMap<object, string>();
  let installedGptSlots: Array<Record<string, unknown>> = [];
  let auctionSequence = 0;

  beforeEach(() => {
    vi.clearAllMocks();
    deliveryAdIds = new WeakMap();
    installedGptSlots = [];
    auctionSequence = 0;
    mockRequestBids.mockReset();
    mockPbjs.requestBids = mockRequestBids;
    mockPbjs.removeAdUnit = mockRemoveAdUnit;
    delete (mockPbjs as unknown as Record<string, unknown>).__tsRemoveAdUnitWrapped;
    mockPbjs.adUnits = [];
    mockGetUserIdsAsEids.mockReset();
    mockGetUserIdsAsEids.mockReturnValue([]);
    // By default the manifest declares all adapters compiled in.
    (window as unknown as { __tsjs_prebid_bundle?: unknown }).__tsjs_prebid_bundle =
      DEFAULT_BUNDLE_MANIFEST;
    mockPbjs.setTargetingForGPTAsync = undefined;
    delete testWindow.__tsjs_prebid;
    testWindow.tsjs = undefined;
    delete testWindow.googletag;
  });

  afterEach(() => {
    delete testWindow.__tsjs_prebid;
    testWindow.tsjs = undefined;
    delete testWindow.googletag;
  });

  function installGpt(slots: Array<Record<string, unknown>>) {
    installedGptSlots = slots;
    for (const slot of slots) {
      if (!slot || typeof slot !== 'object') continue;
      const originalGetTargeting = slot.getTargeting?.bind(slot);
      slot.getTargeting = (key: string) => {
        const deliveryAdId = deliveryAdIds.get(slot);
        if (key === 'hb_adid' && deliveryAdId) return [deliveryAdId];
        return originalGetTargeting?.(key) ?? [];
      };
    }

    const originalRefresh = vi.fn();
    const pubads = {
      refresh: originalRefresh,
      getSlots: vi.fn(() => slots),
    };
    testWindow.googletag = {
      cmd: { push: (fn: () => void) => fn() },
      pubads: () => pubads,
    };
    installRefreshHandler(640);
    return { originalRefresh, pubads };
  }

  function refreshAdUnitFromLastRequest():
    | (Record<string, unknown> & { code?: string; bids?: TestBid[] })
    | undefined {
    const lastCall = mockRequestBids.mock.calls[mockRequestBids.mock.calls.length - 1];
    return lastCall?.[0]?.adUnits?.[0];
  }

  function completePublisherAuction(
    opts?: { adUnits?: Array<{ code?: string }>; bidsBackHandler?: (...args: unknown[]) => void },
    options: { auctionId?: string; applyTargeting?: boolean } = {}
  ): void {
    const auctionId = options.auctionId ?? `example-auction-${auctionSequence++}`;
    const bidResponses: Record<string, { bids: Array<Record<string, unknown>> }> = {};

    for (const unit of opts?.adUnits ?? []) {
      if (!unit.code) continue;
      const adId = `${auctionId}-${unit.code}`;
      bidResponses[unit.code] = {
        bids: [{ adId, adUnitCode: unit.code, auctionId }],
      };
      if (options.applyTargeting !== false) {
        const slot = installedGptSlots.find((candidate) => {
          const elementId = candidate?.getSlotElementId?.();
          return elementId === unit.code || elementId === `${unit.code}-container`;
        });
        if (slot) deliveryAdIds.set(slot, adId);
      }
    }

    opts?.bidsBackHandler?.(bidResponses, false, auctionId);
  }

  function installPrebidRefreshDiagnostics(
    implementation?: (slots: Array<Record<string, unknown>>) => void
  ) {
    const recordPrebidRefresh = vi.fn(implementation);
    testWindow.tsjs = { gptDiagnosticsRecorder: { recordPrebidRefresh } };
    return recordPrebidRefresh;
  }

  it('records a publisher delivery refresh immediately before its GPT request', () => {
    const slot = {
      getSlotElementId: () => 'example-delivery-marker',
      getTargeting: () => [],
      clearTargeting: vi.fn(),
    };
    const recordPrebidRefresh = installPrebidRefreshDiagnostics();
    const { originalRefresh, pubads } = installGpt([slot]);
    mockRequestBids.mockImplementation((opts) => completePublisherAuction(opts));
    const pbjs = installPrebidNpm();

    pbjs.requestBids({
      adUnits: [
        { code: 'example-delivery-marker', bids: [{ bidder: 'exampleServer', params: {} }] },
      ],
      bidsBackHandler: () => pubads.refresh([slot]),
    } as unknown as RequestBidsArg);

    expect(recordPrebidRefresh).toHaveBeenCalledTimes(1);
    expect(recordPrebidRefresh).toHaveBeenCalledWith([slot]);
    expect(recordPrebidRefresh.mock.invocationCallOrder[0]).toBeLessThan(
      originalRefresh.mock.invocationCallOrder[0]
    );
    expect(originalRefresh).toHaveBeenCalledTimes(1);
    expect(originalRefresh).toHaveBeenCalledWith([slot], undefined);
  });

  it('records a completed synthetic refresh immediately before its GPT request', () => {
    const slot = {
      getSlotElementId: () => 'example-synthetic-marker',
      getTargeting: () => [],
      clearTargeting: vi.fn(),
    };
    const recordPrebidRefresh = installPrebidRefreshDiagnostics();
    const { originalRefresh, pubads } = installGpt([slot]);
    mockRequestBids.mockImplementation((opts) => completePublisherAuction(opts));
    installPrebidNpm();

    pubads.refresh([slot]);

    expect(recordPrebidRefresh).toHaveBeenCalledTimes(1);
    expect(recordPrebidRefresh).toHaveBeenCalledWith([slot]);
    expect(recordPrebidRefresh.mock.invocationCallOrder[0]).toBeLessThan(
      originalRefresh.mock.invocationCallOrder[0]
    );
    expect(originalRefresh).toHaveBeenCalledTimes(1);
    expect(originalRefresh).toHaveBeenCalledWith([slot], undefined);
  });

  it('records every slot in a mixed SRA refresh before its GPT request', () => {
    const deliverySlot = {
      getSlotElementId: () => 'example-mixed-delivery-marker',
      getTargeting: () => [],
      clearTargeting: vi.fn(),
    };
    const independentSlot = {
      getSlotElementId: () => 'example-mixed-independent-marker',
      getTargeting: () => [],
      clearTargeting: vi.fn(),
    };
    const targetSlots = [deliverySlot, independentSlot];
    const recordPrebidRefresh = installPrebidRefreshDiagnostics();
    const { originalRefresh, pubads } = installGpt(targetSlots);
    mockRequestBids.mockImplementation((opts) => completePublisherAuction(opts));
    const pbjs = installPrebidNpm();

    pbjs.requestBids({
      adUnits: [
        {
          code: 'example-mixed-delivery-marker',
          bids: [{ bidder: 'exampleServer', params: {} }],
        },
      ],
      bidsBackHandler: () => pubads.refresh(targetSlots),
    } as unknown as RequestBidsArg);

    expect(recordPrebidRefresh).toHaveBeenCalledTimes(1);
    expect(recordPrebidRefresh).toHaveBeenCalledWith(targetSlots);
    expect(recordPrebidRefresh.mock.calls[0][0][0]).toBe(deliverySlot);
    expect(recordPrebidRefresh.mock.calls[0][0][1]).toBe(independentSlot);
    expect(recordPrebidRefresh.mock.invocationCallOrder[0]).toBeLessThan(
      originalRefresh.mock.invocationCallOrder[0]
    );
    expect(originalRefresh).toHaveBeenCalledTimes(1);
    expect(originalRefresh).toHaveBeenCalledWith(targetSlots, undefined);
  });

  it('records one synthetic timeout fallback before one GPT request', () => {
    vi.useFakeTimers();
    try {
      const slot = {
        getSlotElementId: () => 'example-timeout-marker',
        getTargeting: () => [],
        clearTargeting: vi.fn(),
      };
      const recordPrebidRefresh = installPrebidRefreshDiagnostics();
      const { originalRefresh, pubads } = installGpt([slot]);
      mockRequestBids.mockImplementation(() => undefined);
      installPrebidNpm();

      pubads.refresh([slot]);
      expect(recordPrebidRefresh).not.toHaveBeenCalled();
      expect(originalRefresh).not.toHaveBeenCalled();

      vi.advanceTimersByTime(640);

      expect(recordPrebidRefresh).toHaveBeenCalledTimes(1);
      expect(recordPrebidRefresh).toHaveBeenCalledWith([slot]);
      expect(recordPrebidRefresh.mock.invocationCallOrder[0]).toBeLessThan(
        originalRefresh.mock.invocationCallOrder[0]
      );
      expect(originalRefresh).toHaveBeenCalledTimes(1);
    } finally {
      vi.runOnlyPendingTimers();
      vi.useRealTimers();
    }
  });

  it('records a caught synthetic auction failure before one GPT fallback request', () => {
    const slot = {
      getSlotElementId: () => 'example-failure-marker',
      getTargeting: () => [],
      clearTargeting: vi.fn(),
    };
    const recordPrebidRefresh = installPrebidRefreshDiagnostics();
    const { originalRefresh, pubads } = installGpt([slot]);
    mockRequestBids.mockImplementation(() => {
      throw new Error('example auction failure');
    });
    installPrebidNpm();

    pubads.refresh([slot]);

    expect(recordPrebidRefresh).toHaveBeenCalledTimes(1);
    expect(recordPrebidRefresh).toHaveBeenCalledWith([slot]);
    expect(recordPrebidRefresh.mock.invocationCallOrder[0]).toBeLessThan(
      originalRefresh.mock.invocationCallOrder[0]
    );
    expect(originalRefresh).toHaveBeenCalledTimes(1);
    expect(originalRefresh).toHaveBeenCalledWith([slot], undefined);
  });

  it('does not record or refresh again for a late callback after timeout', () => {
    vi.useFakeTimers();
    try {
      const slot = {
        getSlotElementId: () => 'example-late-marker',
        getTargeting: () => [],
        clearTargeting: vi.fn(),
      };
      const recordPrebidRefresh = installPrebidRefreshDiagnostics();
      const { originalRefresh, pubads } = installGpt([slot]);
      let bidsBackHandler: (() => void) | undefined;
      mockRequestBids.mockImplementation((opts) => {
        bidsBackHandler = opts.bidsBackHandler;
      });
      installPrebidNpm();

      pubads.refresh([slot]);
      vi.advanceTimersByTime(640);
      bidsBackHandler?.();

      expect(recordPrebidRefresh).toHaveBeenCalledTimes(1);
      expect(recordPrebidRefresh).toHaveBeenCalledWith([slot]);
      expect(recordPrebidRefresh.mock.invocationCallOrder[0]).toBeLessThan(
        originalRefresh.mock.invocationCallOrder[0]
      );
      expect(originalRefresh).toHaveBeenCalledTimes(1);
    } finally {
      vi.runOnlyPendingTimers();
      vi.useRealTimers();
    }
  });

  it('does not record an adInit refresh bypass', () => {
    const slot = {
      getSlotElementId: () => 'example-adinit-marker-bypass',
      getTargeting: () => [],
      clearTargeting: vi.fn(),
    };
    const recordPrebidRefresh = vi.fn();
    testWindow.tsjs = {
      adInitRefreshInProgress: true,
      gptDiagnosticsRecorder: { recordPrebidRefresh },
    };
    const { originalRefresh, pubads } = installGpt([slot]);

    pubads.refresh([slot]);

    expect(recordPrebidRefresh).not.toHaveBeenCalled();
    expect(mockRequestBids).not.toHaveBeenCalled();
    expect(originalRefresh).toHaveBeenCalledTimes(1);
    expect(originalRefresh).toHaveBeenCalledWith([slot], undefined);
  });

  it('does not record empty or invalid refresh passthroughs', () => {
    const slot = {
      getSlotElementId: () => 'example-invalid-marker-bypass',
      getTargeting: () => [],
      clearTargeting: vi.fn(),
    };
    const recordPrebidRefresh = installPrebidRefreshDiagnostics();
    const { originalRefresh, pubads } = installGpt([slot]);
    const invalidSlots = [slot, null];

    pubads.refresh([]);
    pubads.refresh(invalidSlots);

    expect(recordPrebidRefresh).not.toHaveBeenCalled();
    expect(mockRequestBids).not.toHaveBeenCalled();
    expect(originalRefresh).toHaveBeenCalledTimes(2);
    expect(originalRefresh).toHaveBeenNthCalledWith(1, [], undefined);
    expect(originalRefresh).toHaveBeenNthCalledWith(2, invalidSlots, undefined);
  });

  it('does not record a bare refresh when GPT cannot resolve its slot list', () => {
    const recordPrebidRefresh = installPrebidRefreshDiagnostics();
    const originalRefresh = vi.fn();
    const pubads = { refresh: originalRefresh };
    testWindow.googletag = {
      cmd: { push: (fn: () => void) => fn() },
      pubads: () => pubads,
    };
    installRefreshHandler(640);

    pubads.refresh();

    expect(recordPrebidRefresh).not.toHaveBeenCalled();
    expect(mockRequestBids).not.toHaveBeenCalled();
    expect(originalRefresh).toHaveBeenCalledTimes(1);
    expect(originalRefresh).toHaveBeenCalledWith(undefined, undefined);
  });

  it('does not record while a synthetic refresh is still waiting for its auction', () => {
    vi.useFakeTimers();
    try {
      const slot = {
        getSlotElementId: () => 'example-waiting-marker',
        getTargeting: () => [],
        clearTargeting: vi.fn(),
      };
      const recordPrebidRefresh = installPrebidRefreshDiagnostics();
      const { originalRefresh, pubads } = installGpt([slot]);
      let bidsBackHandler: (() => void) | undefined;
      mockRequestBids.mockImplementation((opts) => {
        bidsBackHandler = opts.bidsBackHandler;
      });
      installPrebidNpm();

      pubads.refresh([slot]);

      expect(recordPrebidRefresh).not.toHaveBeenCalled();
      expect(originalRefresh).not.toHaveBeenCalled();

      bidsBackHandler?.();
      expect(recordPrebidRefresh).toHaveBeenCalledTimes(1);
      expect(recordPrebidRefresh.mock.invocationCallOrder[0]).toBeLessThan(
        originalRefresh.mock.invocationCallOrder[0]
      );
    } finally {
      vi.runOnlyPendingTimers();
      vi.useRealTimers();
    }
  });

  it('still refreshes with unchanged arguments when diagnostics throws', () => {
    const slot = {
      getSlotElementId: () => 'example-throwing-marker',
      getTargeting: () => [],
      clearTargeting: vi.fn(),
    };
    const refreshOptions = { changeCorrelator: false };
    const recordPrebidRefresh = installPrebidRefreshDiagnostics(() => {
      throw new Error('example diagnostics failure');
    });
    const { originalRefresh, pubads } = installGpt([slot]);
    mockRequestBids.mockImplementation((opts) => completePublisherAuction(opts));
    installPrebidNpm();

    pubads.refresh([slot], refreshOptions);

    expect(recordPrebidRefresh).toHaveBeenCalledTimes(1);
    expect(recordPrebidRefresh).toHaveBeenCalledWith([slot]);
    expect(recordPrebidRefresh.mock.invocationCallOrder[0]).toBeLessThan(
      originalRefresh.mock.invocationCallOrder[0]
    );
    expect(originalRefresh).toHaveBeenCalledTimes(1);
    expect(originalRefresh).toHaveBeenCalledWith([slot], refreshOptions);
  });

  it('recovers inline params, ordered client bids, and zone when pbjs.adUnits is empty', () => {
    testWindow.__tsjs_prebid = { clientSideBidders: ['exampleBrowser'] };
    const runtimeInstance = 'example-runtime-instance';
    const code = `example-slot-${runtimeInstance}`;
    const slot = {
      getSlotElementId: () => code,
      getTargeting: () => [],
      getSizes: () => [{ getWidth: () => 320, getHeight: () => 100 }],
      clearTargeting: vi.fn(),
    };
    const { pubads } = installGpt([slot]);
    const pbjs = installPrebidNpm();
    const firstParams = { placement: 'first' };
    const effectiveParams = { placement: 'effective' };

    pbjs.requestBids({
      adUnits: [
        {
          code,
          mediaTypes: { banner: { name: 'example-zone', sizes: [[320, 100]] } },
          bids: [
            { bidder: 'exampleServer', params: firstParams },
            { bidder: 'exampleBrowser', params: { placement: 'browser-one' } },
            { bidder: 'exampleServer', params: effectiveParams },
            { bidder: 'exampleBrowser', params: { placement: 'browser-two' } },
          ],
        },
      ],
    } as unknown as RequestBidsArg);
    effectiveParams.placement = 'changed-after-auction';

    pubads.refresh([slot]);

    expect(mockPbjs.adUnits).toEqual([]);
    expect(mockRequestBids).toHaveBeenCalledTimes(2);
    expect(refreshAdUnitFromLastRequest()).toEqual({
      code,
      mediaTypes: { banner: { name: 'example-zone', sizes: [[320, 100]] } },
      bids: [
        {
          bidder: 'trustedServer',
          params: {
            bidderParams: { exampleServer: { placement: 'effective' } },
            zone: 'example-zone',
          },
        },
        { bidder: 'exampleBrowser', params: { placement: 'browser-one' } },
        { bidder: 'exampleBrowser', params: { placement: 'browser-two' } },
      ],
    });
  });

  it('isolates nested bidder-param objects and arrays from later publisher mutation', () => {
    testWindow.__tsjs_prebid = { clientSideBidders: ['exampleBrowser'] };
    const code = 'example-nested-params-slot';
    const slot = {
      getSlotElementId: () => code,
      getTargeting: () => [],
      getSizes: () => [[300, 250]],
      clearTargeting: vi.fn(),
    };
    const { pubads } = installGpt([slot]);
    const pbjs = installPrebidNpm();
    const serverParams = {
      placement: {
        rules: [{ label: 'original-rule' }],
        sizes: [300, 250],
      },
    };
    const browserParams = {
      groups: [{ values: ['original-value'] }],
    };

    pbjs.requestBids({
      adUnits: [
        {
          code,
          bids: [
            { bidder: 'exampleServer', params: serverParams },
            { bidder: 'exampleBrowser', params: browserParams },
          ],
        },
      ],
    } as unknown as RequestBidsArg);
    serverParams.placement.rules[0].label = 'changed-rule';
    serverParams.placement.sizes.push(999);
    browserParams.groups[0].values[0] = 'changed-value';

    pubads.refresh([slot]);

    const expectedBids = [
      {
        bidder: 'trustedServer',
        params: {
          bidderParams: {
            exampleServer: {
              placement: {
                rules: [{ label: 'original-rule' }],
                sizes: [300, 250],
              },
            },
          },
        },
      },
      {
        bidder: 'exampleBrowser',
        params: { groups: [{ values: ['original-value'] }] },
      },
    ];
    const firstRefreshBids = refreshAdUnitFromLastRequest().bids;
    expect(firstRefreshBids).toEqual(expectedBids);

    firstRefreshBids[0].params.bidderParams.exampleServer.placement.rules[0].label =
      'changed-refresh-rule';
    firstRefreshBids[0].params.bidderParams.exampleServer.placement.sizes.push(777);
    firstRefreshBids[1].params.groups[0].values[0] = 'changed-refresh-value';
    pubads.refresh([slot]);

    expect(refreshAdUnitFromLastRequest().bids).toEqual(expectedBids);
  });

  it('keeps snapshots across repeated synthetic refreshes and overwrites newer publisher config', () => {
    const code = 'example-dynamic-slot';
    const slot = {
      getSlotElementId: () => code,
      getTargeting: () => [],
      getSizes: () => [[300, 250]],
      clearTargeting: vi.fn(),
    };
    const { pubads } = installGpt([slot]);
    const pbjs = installPrebidNpm();

    pbjs.requestBids({
      adUnits: [
        {
          code,
          mediaTypes: { banner: { name: 'example-zone-one', sizes: [[300, 250]] } },
          bids: [{ bidder: 'exampleServer', params: { placement: 'one' } }],
        },
      ],
    } as unknown as RequestBidsArg);
    pubads.refresh([slot]);
    expect(refreshAdUnitFromLastRequest().bids[0].params).toEqual({
      bidderParams: { exampleServer: { placement: 'one' } },
      zone: 'example-zone-one',
    });

    pubads.refresh([slot]);
    expect(refreshAdUnitFromLastRequest().bids[0].params).toEqual({
      bidderParams: { exampleServer: { placement: 'one' } },
      zone: 'example-zone-one',
    });

    pbjs.requestBids({
      adUnits: [
        {
          code,
          mediaTypes: { banner: { name: 'example-zone-two', sizes: [[300, 250]] } },
          bids: [{ bidder: 'exampleServer', params: { placement: 'two' } }],
        },
      ],
    } as unknown as RequestBidsArg);
    pubads.refresh([slot]);

    expect(refreshAdUnitFromLastRequest().bids[0].params).toEqual({
      bidderParams: { exampleServer: { placement: 'two' } },
      zone: 'example-zone-two',
    });
  });

  it('does not cross-contaminate dynamic-code snapshots and retains the global fallback', () => {
    const slotOne = {
      getSlotElementId: () => 'example-code-one',
      getTargeting: () => [],
      getSizes: () => [[300, 250]],
      clearTargeting: vi.fn(),
    };
    const slotTwo = {
      getSlotElementId: () => 'example-code-two',
      getTargeting: () => [],
      getSizes: () => [[300, 250]],
      clearTargeting: vi.fn(),
    };
    const globalSlot = {
      getSlotElementId: () => 'example-global-code',
      getTargeting: () => [],
      getSizes: () => [[300, 250]],
      clearTargeting: vi.fn(),
    };
    const { pubads } = installGpt([slotOne, slotTwo, globalSlot]);
    const pbjs = installPrebidNpm();

    pbjs.requestBids({
      adUnits: [
        {
          code: 'example-code-one',
          bids: [{ bidder: 'exampleServer', params: { placement: 'one' } }],
        },
        {
          code: 'example-code-two',
          bids: [{ bidder: 'exampleServer', params: { placement: 'two' } }],
        },
      ],
    } as unknown as RequestBidsArg);
    mockPbjs.adUnits = [
      {
        code: 'example-global-code',
        bids: [{ bidder: 'exampleFallback', params: { placement: 'global' } }],
      },
    ];

    pubads.refresh([slotOne]);
    expect(refreshAdUnitFromLastRequest().bids[0].params.bidderParams).toEqual({
      exampleServer: { placement: 'one' },
    });
    pubads.refresh([slotTwo]);
    expect(refreshAdUnitFromLastRequest().bids[0].params.bidderParams).toEqual({
      exampleServer: { placement: 'two' },
    });
    pubads.refresh([globalSlot]);
    expect(refreshAdUnitFromLastRequest().bids[0].params.bidderParams).toEqual({
      exampleFallback: { placement: 'global' },
    });
  });

  it('prefers a rich live unit when a fresh same-code request overwrites the snapshot with empty bids', () => {
    testWindow.__tsjs_prebid = { clientSideBidders: ['exampleBrowser'] };
    const code = 'example-live-rich-slot';
    const slot = {
      getSlotElementId: () => code,
      getTargeting: () => [],
      getSizes: () => [[300, 250]],
      clearTargeting: vi.fn(),
    };
    const { pubads } = installGpt([slot]);
    const liveUnit = {
      code,
      bids: [
        { bidder: 'exampleServer', params: { placement: 'live-server' } },
        { bidder: 'exampleBrowser', params: { placement: 'live-browser' } },
      ],
    };
    mockPbjs.adUnits = [liveUnit];
    const pbjs = installPrebidNpm();

    pbjs.requestBids();
    pbjs.requestBids({ adUnits: [{ code, bids: [] }] } as unknown as RequestBidsArg);
    pubads.refresh([slot]);

    expect(refreshAdUnitFromLastRequest().bids).toEqual([
      {
        bidder: 'trustedServer',
        params: { bidderParams: { exampleServer: { placement: 'live-server' } } },
      },
      { bidder: 'exampleBrowser', params: { placement: 'live-browser' } },
    ]);
  });

  it('does not resurrect an older snapshot when the live unit is intentionally empty', () => {
    const code = 'example-live-empty-slot';
    const slot = {
      getSlotElementId: () => code,
      getTargeting: () => [],
      getSizes: () => [[300, 250]],
      clearTargeting: vi.fn(),
    };
    const { pubads } = installGpt([slot]);
    const pbjs = installPrebidNpm();

    pbjs.requestBids({
      adUnits: [{ code, bids: [{ bidder: 'exampleServer', params: { placement: 'snapshot' } }] }],
    } as unknown as RequestBidsArg);
    mockPbjs.adUnits = [{ code, bids: [] }];
    pubads.refresh([slot]);

    expect(refreshAdUnitFromLastRequest().bids).toEqual([
      { bidder: 'trustedServer', params: { bidderParams: {} } },
    ]);
  });

  it('evicts snapshots with the matching removeAdUnit lifecycle', () => {
    const codes = ['example-remove-one', 'example-remove-two', 'example-remove-all'];
    const slots = codes.map((code) => ({
      getSlotElementId: () => code,
      getTargeting: () => [],
      getSizes: () => [[300, 250]],
      clearTargeting: vi.fn(),
    }));
    const { pubads } = installGpt(slots);
    const pbjs = installPrebidNpm();

    pbjs.requestBids({
      adUnits: codes.map((code) => ({
        code,
        bids: [{ bidder: 'exampleServer', params: { placement: code } }],
      })),
    } as unknown as RequestBidsArg);
    (pbjs as unknown as { removeAdUnit: (adUnitCode?: string | string[]) => void }).removeAdUnit(
      codes[0]
    );
    (pbjs as unknown as { removeAdUnit: (adUnitCode?: string | string[]) => void }).removeAdUnit([
      codes[1],
    ]);

    pubads.refresh([slots[0]]);
    expect(refreshAdUnitFromLastRequest().bids[0].params).toEqual({ bidderParams: {} });
    pubads.refresh([slots[1]]);
    expect(refreshAdUnitFromLastRequest().bids[0].params).toEqual({ bidderParams: {} });
    pubads.refresh([slots[2]]);
    expect(refreshAdUnitFromLastRequest().bids[0].params.bidderParams).toEqual({
      exampleServer: { placement: codes[2] },
    });

    (pbjs as unknown as { removeAdUnit: (adUnitCode?: string | string[]) => void }).removeAdUnit();
    pubads.refresh([slots[2]]);
    expect(refreshAdUnitFromLastRequest().bids[0].params).toEqual({ bidderParams: {} });
  });

  it('bounds snapshots with LRU eviction while retaining a recently refreshed entry', () => {
    const capacity = 256;
    const oldestCode = 'example-lru-0';
    const activeCode = `example-lru-${capacity - 1}`;
    const oldestSlot = {
      getSlotElementId: () => oldestCode,
      getTargeting: () => [],
      getSizes: () => [[300, 250]],
      clearTargeting: vi.fn(),
    };
    const activeSlot = {
      getSlotElementId: () => activeCode,
      getTargeting: () => [],
      getSizes: () => [[300, 250]],
      clearTargeting: vi.fn(),
    };
    const { pubads } = installGpt([oldestSlot, activeSlot]);
    const pbjs = installPrebidNpm();

    for (let index = 0; index < capacity; index += 1) {
      pbjs.requestBids({
        adUnits: [
          {
            code: `example-lru-${index}`,
            bids: [{ bidder: 'exampleServer', params: { placement: index } }],
          },
        ],
      } as unknown as RequestBidsArg);
    }

    pubads.refresh([activeSlot]);
    pbjs.requestBids({
      adUnits: [
        {
          code: `example-lru-${capacity}`,
          bids: [{ bidder: 'exampleServer', params: { placement: capacity } }],
        },
      ],
    } as unknown as RequestBidsArg);

    pubads.refresh([oldestSlot]);
    expect(refreshAdUnitFromLastRequest().bids[0].params).toEqual({ bidderParams: {} });
    pubads.refresh([activeSlot]);
    expect(refreshAdUnitFromLastRequest().bids[0].params.bidderParams).toEqual({
      exampleServer: { placement: capacity - 1 },
    });
  });

  it('bypasses explicit covered subset delivery refreshes without clearing targeting', () => {
    const slotOne = {
      getSlotElementId: () => 'example-covered-one',
      getTargeting: () => [],
      clearTargeting: vi.fn(),
    };
    const slotTwo = {
      getSlotElementId: () => 'example-covered-two-container',
      getTargeting: () => [],
      clearTargeting: vi.fn(),
    };
    testWindow.tsjs = {
      adSlots: [{ div_id: 'example-covered-two', formats: [[300, 250]], targeting: {} }],
    };
    const { originalRefresh, pubads } = installGpt([slotOne, slotTwo]);
    mockRequestBids.mockImplementation((opts) => completePublisherAuction(opts));
    const pbjs = installPrebidNpm();

    pbjs.requestBids({
      adUnits: [
        { code: 'example-covered-one', bids: [{ bidder: 'exampleServer', params: {} }] },
        { code: 'example-covered-two', bids: [{ bidder: 'exampleServer', params: {} }] },
      ],
      bidsBackHandler: () => {
        pubads.refresh([slotOne]);
        pubads.refresh([slotTwo]);
      },
    } as unknown as RequestBidsArg);

    expect(mockRequestBids).toHaveBeenCalledTimes(1);
    expect(slotOne.clearTargeting).not.toHaveBeenCalled();
    expect(slotTwo.clearTargeting).not.toHaveBeenCalled();
    expect(originalRefresh).toHaveBeenCalledTimes(2);
    expect(originalRefresh).toHaveBeenNthCalledWith(1, [slotOne], undefined);
    expect(originalRefresh).toHaveBeenNthCalledWith(2, [slotTwo], undefined);
  });

  it('registers delivery state for a publisher auction without a bidsBackHandler', () => {
    const code = 'example-handlerless-delivery';
    const slot = {
      getSlotElementId: () => code,
      getTargeting: () => [],
      clearTargeting: vi.fn(),
    };
    const { originalRefresh, pubads } = installGpt([slot]);
    mockRequestBids.mockImplementation((opts) => completePublisherAuction(opts));
    const pbjs = installPrebidNpm();

    pbjs.requestBids({
      adUnits: [{ code, bids: [{ bidder: 'exampleServer', params: {} }] }],
    } as unknown as RequestBidsArg);
    pubads.refresh([slot]);

    expect(mockRequestBids).toHaveBeenCalledTimes(1);
    expect(slot.clearTargeting).not.toHaveBeenCalled();
    expect(originalRefresh).toHaveBeenCalledWith([slot], undefined);
  });

  it('preserves one mixed refresh request and its original options', () => {
    const deliverySlot = {
      getSlotElementId: () => 'example-sra-delivery',
      getTargeting: () => [],
      clearTargeting: vi.fn(),
    };
    const independentSlot = {
      getSlotElementId: () => 'example-sra-independent',
      getTargeting: () => [],
      clearTargeting: vi.fn(),
    };
    const refreshOptions = { changeCorrelator: true };
    const { originalRefresh, pubads } = installGpt([deliverySlot, independentSlot]);
    let syntheticBidsBackHandler: (() => void) | undefined;
    mockRequestBids.mockImplementation((opts) => {
      if (mockRequestBids.mock.calls.length === 1) {
        completePublisherAuction(opts);
      } else {
        syntheticBidsBackHandler = opts.bidsBackHandler;
      }
    });
    const pbjs = installPrebidNpm();

    pbjs.requestBids({
      adUnits: [{ code: 'example-sra-delivery', bids: [{ bidder: 'exampleServer', params: {} }] }],
      bidsBackHandler: () => pubads.refresh([deliverySlot, independentSlot], refreshOptions),
    } as unknown as RequestBidsArg);

    expect(originalRefresh).not.toHaveBeenCalled();
    expect(independentSlot.clearTargeting).toHaveBeenCalledWith('hb_adid');

    syntheticBidsBackHandler?.();

    expect(originalRefresh).toHaveBeenCalledTimes(1);
    expect(originalRefresh).toHaveBeenCalledWith([deliverySlot, independentSlot], refreshOptions);
  });

  it('partitions a bare delivery refresh from an unmatched GPT slot', () => {
    const coveredSlot = {
      getSlotElementId: () => 'example-covered',
      getTargeting: () => [],
      clearTargeting: vi.fn(),
    };
    const gamOnlySlot = {
      getSlotElementId: () => 'example-gam-only-interstitial',
      getTargeting: () => [],
      clearTargeting: vi.fn(),
    };
    const { originalRefresh, pubads } = installGpt([coveredSlot, gamOnlySlot]);
    mockRequestBids.mockImplementation((opts) => completePublisherAuction(opts));
    const pbjs = installPrebidNpm();

    pbjs.requestBids({
      adUnits: [{ code: 'example-covered', bids: [{ bidder: 'exampleServer', params: {} }] }],
      bidsBackHandler: () => pubads.refresh(),
    } as unknown as RequestBidsArg);

    expect(mockRequestBids).toHaveBeenCalledTimes(2);
    expect(coveredSlot.clearTargeting).not.toHaveBeenCalled();
    expect(gamOnlySlot.clearTargeting).toHaveBeenCalledWith('hb_adid');
    expect(originalRefresh).toHaveBeenCalledTimes(1);
    expect(originalRefresh).toHaveBeenCalledWith(undefined, undefined);
  });

  it('keeps explicit unrelated lists synthetic and partitions mixed delivery lists', () => {
    const coveredSlot = {
      getSlotElementId: () => 'example-covered',
      getTargeting: () => [],
      getSizes: () => [[300, 250]],
      clearTargeting: vi.fn(),
    };
    const unrelatedSlot = {
      getSlotElementId: () => 'example-unrelated',
      getTargeting: () => [],
      getSizes: () => [[300, 250]],
      clearTargeting: vi.fn(),
    };
    const { originalRefresh, pubads } = installGpt([coveredSlot, unrelatedSlot]);
    mockRequestBids.mockImplementation((opts) => completePublisherAuction(opts));
    const pbjs = installPrebidNpm();

    pbjs.requestBids({
      adUnits: [{ code: 'example-covered', bids: [{ bidder: 'exampleServer', params: {} }] }],
      bidsBackHandler: () => {
        pubads.refresh([unrelatedSlot]);
        pubads.refresh([coveredSlot, unrelatedSlot]);
      },
    } as unknown as RequestBidsArg);

    expect(mockRequestBids).toHaveBeenCalledTimes(3);
    expect(
      mockRequestBids.mock.calls[1][0].adUnits.map((unit: { code?: string }) => unit.code)
    ).toEqual(['example-unrelated']);
    expect(
      mockRequestBids.mock.calls[2][0].adUnits.map((unit: { code?: string }) => unit.code)
    ).toEqual(['example-unrelated']);
    expect(coveredSlot.clearTargeting).not.toHaveBeenCalled();
    expect(unrelatedSlot.clearTargeting).toHaveBeenCalledWith('ts_initial');
    expect(unrelatedSlot.clearTargeting).toHaveBeenCalledWith('hb_cache_path');
    expect(originalRefresh).toHaveBeenCalledTimes(2);
    expect(originalRefresh).toHaveBeenNthCalledWith(1, [unrelatedSlot], undefined);
    expect(originalRefresh).toHaveBeenNthCalledWith(2, [coveredSlot, unrelatedSlot], undefined);
  });

  it('partitions four delivered slots from an unmatched explicit slot', () => {
    const coveredSlots = Array.from({ length: 4 }, (_, index) => ({
      getSlotElementId: () => `example-covered-${index}`,
      getTargeting: () => [],
      clearTargeting: vi.fn(),
    }));
    const gamOnlySlot = {
      getSlotElementId: () => 'example-gam-only-interstitial',
      getTargeting: () => [],
      clearTargeting: vi.fn(),
    };
    const refreshSlots = [...coveredSlots, gamOnlySlot];
    const { originalRefresh, pubads } = installGpt(refreshSlots);
    mockRequestBids.mockImplementation((opts) => completePublisherAuction(opts));
    const pbjs = installPrebidNpm();

    pbjs.requestBids({
      adUnits: coveredSlots.map((_, index) => ({
        code: `example-covered-${index}`,
        bids: [{ bidder: 'exampleServer', params: { placement: index } }],
      })),
      bidsBackHandler: () => pubads.refresh(refreshSlots),
    } as unknown as RequestBidsArg);

    expect(mockRequestBids).toHaveBeenCalledTimes(2);
    coveredSlots.forEach((slot) => expect(slot.clearTargeting).not.toHaveBeenCalled());
    expect(gamOnlySlot.clearTargeting).toHaveBeenCalledWith('hb_adid');
    expect(originalRefresh).toHaveBeenCalledTimes(1);
    expect(originalRefresh).toHaveBeenCalledWith(refreshSlots, undefined);
  });

  it('expires an unconsumed publisher delivery before a later refresh', () => {
    vi.useFakeTimers();
    try {
      const code = 'example-expired-delivery';
      const slot = {
        getSlotElementId: () => code,
        getTargeting: () => [],
        getSizes: () => [[300, 250]],
        clearTargeting: vi.fn(),
      };
      const { originalRefresh, pubads } = installGpt([slot]);
      mockRequestBids.mockImplementation((opts) =>
        completePublisherAuction(opts, { applyTargeting: false })
      );
      const pbjs = installPrebidNpm();

      pbjs.requestBids({
        adUnits: [{ code, bids: [{ bidder: 'exampleServer', params: {} }] }],
      } as unknown as RequestBidsArg);
      vi.advanceTimersByTime(5001);
      pubads.refresh([slot]);

      expect(mockRequestBids).toHaveBeenCalledTimes(2);
      expect(slot.clearTargeting).toHaveBeenCalledWith('hb_adid');
      expect(originalRefresh).toHaveBeenCalledWith([slot], undefined);
    } finally {
      vi.runOnlyPendingTimers();
      vi.useRealTimers();
    }
  });

  it('expires an unconsumed targeted delivery before a later refresh', () => {
    vi.useFakeTimers();
    try {
      const code = 'example-expired-targeted-delivery';
      const slot = {
        getSlotElementId: () => code,
        getTargeting: () => [],
        getSizes: () => [[300, 250]],
        clearTargeting: vi.fn(),
      };
      const { originalRefresh, pubads } = installGpt([slot]);
      mockRequestBids.mockImplementation((opts) => completePublisherAuction(opts));
      const pbjs = installPrebidNpm();

      pbjs.requestBids({
        adUnits: [{ code, bids: [{ bidder: 'exampleServer', params: {} }] }],
      } as unknown as RequestBidsArg);
      vi.advanceTimersByTime(5001);
      pubads.refresh([slot]);

      expect(mockRequestBids).toHaveBeenCalledTimes(2);
      expect(slot.clearTargeting).toHaveBeenCalledWith('hb_adid');
      expect(originalRefresh).toHaveBeenCalledWith([slot], undefined);
    } finally {
      vi.runOnlyPendingTimers();
      vi.useRealTimers();
    }
  });

  it('correlates a targeted delivery refresh after more than one second without a timer race', () => {
    vi.useFakeTimers();
    try {
      const code = 'example-delayed-delivery';
      const auctionId = 'example-delayed-auction';
      const slot = {
        getSlotElementId: () => code,
        getTargeting: () => [],
        getSizes: () => [[300, 250]],
        clearTargeting: vi.fn(),
      };
      const { originalRefresh, pubads } = installGpt([slot]);
      mockRequestBids.mockImplementation((opts) =>
        completePublisherAuction(opts, { auctionId, applyTargeting: false })
      );
      const pbjs = installPrebidNpm();

      pbjs.requestBids({
        adUnits: [{ code, bids: [{ bidder: 'exampleServer', params: {} }] }],
        bidsBackHandler: () => {
          setTimeout(() => {
            deliveryAdIds.set(slot, `${auctionId}-${code}`);
            pubads.refresh([slot]);
          }, 1500);
        },
      } as unknown as RequestBidsArg);

      vi.advanceTimersByTime(1500);

      expect(mockRequestBids).toHaveBeenCalledTimes(1);
      expect(slot.clearTargeting).not.toHaveBeenCalled();
      expect(originalRefresh).toHaveBeenCalledTimes(1);
      expect(originalRefresh).toHaveBeenCalledWith([slot], undefined);

      pubads.refresh([slot]);
      expect(mockRequestBids).toHaveBeenCalledTimes(2);
      expect(slot.clearTargeting).toHaveBeenCalledWith('hb_adid');
      expect(originalRefresh).toHaveBeenCalledTimes(2);
    } finally {
      vi.runOnlyPendingTimers();
      vi.useRealTimers();
    }
  });

  it('correlates null and no-argument targeting with a custom GPT slot match', () => {
    const code = 'example-custom-matched-code';
    const slot = {
      getSlotElementId: () => 'example-different-gpt-slot',
      getTargeting: () => [],
      clearTargeting: vi.fn(),
    };
    const { originalRefresh, pubads } = installGpt([slot]);
    let auctionId = 'example-null-auction';
    const setTargetingForGPTAsync = vi.fn(() => {
      deliveryAdIds.set(slot, `${auctionId}-${code}`);
    });
    mockPbjs.setTargetingForGPTAsync = setTargetingForGPTAsync;
    mockRequestBids.mockImplementation((opts) =>
      completePublisherAuction(opts, { auctionId, applyTargeting: false })
    );
    const pbjs = installPrebidNpm();

    pbjs.requestBids({
      adUnits: [{ code, bids: [{ bidder: 'exampleServer', params: {} }] }],
      bidsBackHandler: () => {
        (
          pbjs as unknown as { setTargetingForGPTAsync: (codes?: string[]) => void }
        ).setTargetingForGPTAsync(null, () => () => true);
        pubads.refresh([slot]);
      },
    } as unknown as RequestBidsArg);

    auctionId = 'example-no-argument-auction';
    pbjs.requestBids({
      adUnits: [{ code, bids: [{ bidder: 'exampleServer', params: {} }] }],
      bidsBackHandler: () => {
        (
          pbjs as unknown as { setTargetingForGPTAsync: (codes?: string[]) => void }
        ).setTargetingForGPTAsync();
        pubads.refresh([slot]);
      },
    } as unknown as RequestBidsArg);

    expect(setTargetingForGPTAsync).toHaveBeenNthCalledWith(1, null, expect.any(Function));
    expect(setTargetingForGPTAsync).toHaveBeenNthCalledWith(2);
    expect(mockRequestBids).toHaveBeenCalledTimes(2);
    expect(slot.clearTargeting).not.toHaveBeenCalled();
    expect(originalRefresh).toHaveBeenNthCalledWith(1, [slot], undefined);
    expect(originalRefresh).toHaveBeenNthCalledWith(2, [slot], undefined);
    mockPbjs.setTargetingForGPTAsync = undefined;
  });

  it('correlates requested no-bid slots without manufacturing unrelated bid state', () => {
    const slot = {
      getSlotElementId: () => 'example-no-bid-delivery',
      getTargeting: () => [],
      getSizes: () => [[300, 250]],
      clearTargeting: vi.fn(),
    };
    const { originalRefresh, pubads } = installGpt([slot]);
    mockRequestBids.mockImplementation(
      (opts?: { bidsBackHandler?: (...args: unknown[]) => void }) => {
        opts?.bidsBackHandler?.({ 'example-no-bid-delivery': { bids: [null, {}] } }, false, 'bad');
      }
    );
    const pbjs = installPrebidNpm();

    pbjs.requestBids({
      adUnits: [
        { code: 'example-no-bid-delivery', bids: [{ bidder: 'exampleServer', params: {} }] },
      ],
      bidsBackHandler: () => pubads.refresh([slot]),
    } as unknown as RequestBidsArg);

    expect(mockRequestBids).toHaveBeenCalledTimes(1);
    expect(slot.clearTargeting).not.toHaveBeenCalled();
    expect(originalRefresh).toHaveBeenCalledWith([slot], undefined);
  });

  it('bounds code-only delivery correlation to one suppressed independent refresh', () => {
    const code = 'example-code-only-delivery';
    const slot = {
      getSlotElementId: () => code,
      getTargeting: () => [],
      getSizes: () => [[300, 250]],
      clearTargeting: vi.fn(),
    };
    const { originalRefresh, pubads } = installGpt([slot]);
    mockRequestBids.mockImplementation((opts) =>
      completePublisherAuction(opts, { applyTargeting: false })
    );
    const pbjs = installPrebidNpm();

    // Model an initial impression rendered with display() after an auction
    // that did not apply hb_adid targeting. Its code-only state is unconsumed.
    pbjs.requestBids({
      adUnits: [{ code, bids: [{ bidder: 'exampleServer', params: {} }] }],
    } as unknown as RequestBidsArg);

    pubads.refresh([slot]);
    expect(mockRequestBids).toHaveBeenCalledTimes(1);
    expect(slot.clearTargeting).not.toHaveBeenCalled();
    expect(originalRefresh).toHaveBeenCalledTimes(1);

    pubads.refresh([slot]);
    expect(mockRequestBids).toHaveBeenCalledTimes(2);
    expect(slot.clearTargeting).toHaveBeenCalledWith('hb_adid');
    expect(originalRefresh).toHaveBeenCalledTimes(2);
  });

  it('does not use code fallback when a slot has an unmatched hb_adid', () => {
    const code = 'example-stale-targeting';
    const slot = {
      getSlotElementId: () => code,
      getTargeting: (key: string) => (key === 'hb_adid' ? ['example-stale-ad-id'] : []),
      getSizes: () => [[300, 250]],
      clearTargeting: vi.fn(),
    };
    const { originalRefresh, pubads } = installGpt([slot]);
    mockRequestBids.mockImplementation((opts) =>
      completePublisherAuction(opts, { applyTargeting: false })
    );
    const pbjs = installPrebidNpm();

    pbjs.requestBids({
      adUnits: [{ code, bids: [{ bidder: 'exampleServer', params: {} }] }],
      bidsBackHandler: () => pubads.refresh([slot]),
    } as unknown as RequestBidsArg);

    expect(mockRequestBids).toHaveBeenCalledTimes(2);
    expect(slot.clearTargeting).toHaveBeenCalledWith('hb_adid');
    expect(originalRefresh).toHaveBeenCalledWith([slot], undefined);
  });

  it('uses an independent auction when a pending hb_adid exceeds the capacity bound', () => {
    const capacity = 2048;
    const code = 'example-capacity-delivery';
    const oldestAdId = 'example-capacity-ad-0';
    const slot = {
      getSlotElementId: () => code,
      getTargeting: () => [],
      getSizes: () => [[300, 250]],
      clearTargeting: vi.fn(),
    };
    const { originalRefresh, pubads } = installGpt([slot]);
    mockRequestBids.mockImplementation((opts) => {
      if (mockRequestBids.mock.calls.length === 1) {
        opts.bidsBackHandler?.({
          [code]: {
            bids: Array.from({ length: capacity + 1 }, (_, index) => ({
              adId: `example-capacity-ad-${index}`,
              adUnitCode: code,
            })),
          },
        });
        return;
      }
      completePublisherAuction(opts);
    });
    const pbjs = installPrebidNpm();

    pbjs.requestBids({
      adUnits: [{ code, bids: [{ bidder: 'exampleServer', params: {} }] }],
    } as unknown as RequestBidsArg);
    deliveryAdIds.set(slot, oldestAdId);
    pubads.refresh([slot]);

    expect(mockRequestBids).toHaveBeenCalledTimes(2);
    expect(slot.clearTargeting).toHaveBeenCalledWith('hb_adid');
    expect(originalRefresh).toHaveBeenCalledWith([slot], undefined);
  });

  it('bypasses a mixed explicit delivery list spanning nested contexts', () => {
    const outerSlot = {
      getSlotElementId: () => 'example-outer-delivery',
      getTargeting: () => [],
      clearTargeting: vi.fn(),
    };
    const innerSlot = {
      getSlotElementId: () => 'example-inner-delivery',
      getTargeting: () => [],
      clearTargeting: vi.fn(),
    };
    const gamOnlySlot = {
      getSlotElementId: () => 'example-gam-only-interstitial',
      getTargeting: () => [],
      clearTargeting: vi.fn(),
    };
    const refreshSlots = [innerSlot, outerSlot, gamOnlySlot];
    const { originalRefresh, pubads } = installGpt(refreshSlots);
    mockRequestBids.mockImplementation((opts) => completePublisherAuction(opts));
    const pbjs = installPrebidNpm();

    pbjs.requestBids({
      adUnits: [
        { code: 'example-outer-delivery', bids: [{ bidder: 'exampleServer', params: {} }] },
      ],
      bidsBackHandler: () => {
        pbjs.requestBids({
          adUnits: [
            { code: 'example-inner-delivery', bids: [{ bidder: 'exampleServer', params: {} }] },
          ],
          bidsBackHandler: () => pubads.refresh(refreshSlots),
        } as unknown as RequestBidsArg);
      },
    } as unknown as RequestBidsArg);

    expect(mockRequestBids).toHaveBeenCalledTimes(3);
    expect(innerSlot.clearTargeting).not.toHaveBeenCalled();
    expect(outerSlot.clearTargeting).not.toHaveBeenCalled();
    expect(gamOnlySlot.clearTargeting).toHaveBeenCalledWith('hb_adid');
    expect(originalRefresh).toHaveBeenCalledTimes(1);
    expect(originalRefresh).toHaveBeenCalledWith(refreshSlots, undefined);
  });

  it('correlates a microtask refresh by its requested code without targeting', async () => {
    const slot = {
      getSlotElementId: () => 'example-deferred-refresh',
      getTargeting: () => [],
      getSizes: () => [[300, 250]],
      clearTargeting: vi.fn(),
    };
    const { originalRefresh, pubads } = installGpt([slot]);
    mockRequestBids.mockImplementation((opts) =>
      completePublisherAuction(opts, { applyTargeting: false })
    );
    const pbjs = installPrebidNpm();
    let deferredRefresh: Promise<void> | undefined;

    pbjs.requestBids({
      adUnits: [
        { code: 'example-deferred-refresh', bids: [{ bidder: 'exampleServer', params: {} }] },
      ],
      bidsBackHandler: () => {
        deferredRefresh = Promise.resolve().then(() => pubads.refresh([slot]));
      },
    } as unknown as RequestBidsArg);
    await deferredRefresh;

    expect(mockRequestBids).toHaveBeenCalledTimes(1);
    expect(slot.clearTargeting).not.toHaveBeenCalled();
    expect(originalRefresh).toHaveBeenCalledTimes(1);
    expect(originalRefresh).toHaveBeenCalledWith([slot], undefined);
  });

  it('correlates targeting and refresh deferred together to a microtask', async () => {
    const code = 'example-targeted-microtask';
    const auctionId = 'example-targeted-microtask-auction';
    const slot = {
      getSlotElementId: () => code,
      getTargeting: () => [],
      getSizes: () => [[300, 250]],
      clearTargeting: vi.fn(),
    };
    const { originalRefresh, pubads } = installGpt([slot]);
    mockRequestBids.mockImplementation((opts) =>
      completePublisherAuction(opts, { auctionId, applyTargeting: false })
    );
    const pbjs = installPrebidNpm();
    let deferredRefresh: Promise<void> | undefined;

    pbjs.requestBids({
      adUnits: [{ code, bids: [{ bidder: 'exampleServer', params: {} }] }],
      bidsBackHandler: () => {
        deferredRefresh = Promise.resolve().then(() => {
          deliveryAdIds.set(slot, `${auctionId}-${code}`);
          pubads.refresh([slot]);
        });
      },
    } as unknown as RequestBidsArg);
    await deferredRefresh;

    expect(mockRequestBids).toHaveBeenCalledTimes(1);
    expect(slot.clearTargeting).not.toHaveBeenCalled();
    expect(originalRefresh).toHaveBeenCalledWith([slot], undefined);
  });

  it('consumes all overlapping pending bids for the same ad-unit code', () => {
    const code = 'example-overlapping-code';
    const slot = {
      getSlotElementId: () => code,
      getTargeting: () => [],
      getSizes: () => [[300, 250]],
      clearTargeting: vi.fn(),
    };
    const { originalRefresh, pubads } = installGpt([slot]);
    mockRequestBids.mockImplementation((opts) => completePublisherAuction(opts));
    const pbjs = installPrebidNpm();

    pbjs.requestBids({
      adUnits: [{ code, bids: [{ bidder: 'exampleServer', params: {} }] }],
      bidsBackHandler: () => {},
    } as unknown as RequestBidsArg);
    pbjs.requestBids({
      adUnits: [{ code, bids: [{ bidder: 'exampleServer', params: {} }] }],
      bidsBackHandler: () => {},
    } as unknown as RequestBidsArg);

    pubads.refresh([slot]);
    expect(mockRequestBids).toHaveBeenCalledTimes(2);
    expect(slot.clearTargeting).not.toHaveBeenCalled();

    deliveryAdIds.set(slot, `example-auction-0-${code}`);
    pubads.refresh([slot]);

    expect(mockRequestBids).toHaveBeenCalledTimes(3);
    expect(slot.clearTargeting).toHaveBeenCalledWith('hb_adid');
    expect(originalRefresh).toHaveBeenNthCalledWith(1, [slot], undefined);
    expect(originalRefresh).toHaveBeenNthCalledWith(2, [slot], undefined);
  });

  it('filters invalid explicit entries without duplicating or leaking a valid delivery', () => {
    const code = 'example-valid-delivery';
    const slot = {
      getSlotElementId: () => code,
      getTargeting: () => [],
      getSizes: () => [[300, 250]],
      clearTargeting: vi.fn(),
    };
    const { originalRefresh, pubads } = installGpt([slot]);
    mockRequestBids.mockImplementation((opts) => completePublisherAuction(opts));
    const pbjs = installPrebidNpm();

    pbjs.requestBids({
      adUnits: [{ code, bids: [{ bidder: 'exampleServer', params: {} }] }],
      bidsBackHandler: () =>
        pubads.refresh([slot, undefined, null] as unknown as Array<Record<string, unknown>>),
    } as unknown as RequestBidsArg);

    expect(mockRequestBids).toHaveBeenCalledTimes(1);
    expect(slot.clearTargeting).not.toHaveBeenCalled();
    expect(originalRefresh).toHaveBeenCalledWith([slot, undefined, null], undefined);

    pubads.refresh([slot]);
    expect(mockRequestBids).toHaveBeenCalledTimes(1);
    expect(slot.clearTargeting).not.toHaveBeenCalled();
  });

  it('does not mutate reused publisher request options', () => {
    const code = 'example-reused-request';
    const slot = {
      getSlotElementId: () => code,
      getTargeting: () => [],
      clearTargeting: vi.fn(),
    };
    const { originalRefresh, pubads } = installGpt([slot]);
    mockRequestBids.mockImplementation((opts) => completePublisherAuction(opts));
    const pbjs = installPrebidNpm();
    const request = {
      adUnits: [{ code, bids: [{ bidder: 'exampleServer', params: {} }] }],
    };

    pbjs.requestBids(request as unknown as RequestBidsArg);
    pbjs.requestBids(request as unknown as RequestBidsArg);
    pubads.refresh([slot]);

    expect(request).not.toHaveProperty('bidsBackHandler');
    expect(mockRequestBids).toHaveBeenCalledTimes(2);
    expect(originalRefresh).toHaveBeenCalledWith([slot], undefined);
  });

  it('falls back to one GPT refresh when a synthetic auction throws', () => {
    const slot = {
      getSlotElementId: () => 'example-throwing-refresh',
      getTargeting: () => [],
      clearTargeting: vi.fn(),
    };
    const { originalRefresh, pubads } = installGpt([slot]);
    const setTargetingForGPTAsync = vi.fn();
    mockPbjs.setTargetingForGPTAsync = setTargetingForGPTAsync;
    mockRequestBids.mockImplementation(() => {
      throw new Error('example synthetic failure');
    });
    installPrebidNpm();

    pubads.refresh([slot]);

    expect(slot.clearTargeting).toHaveBeenCalledWith('hb_adid');
    expect(setTargetingForGPTAsync).not.toHaveBeenCalled();
    expect(originalRefresh).toHaveBeenCalledTimes(1);
    expect(originalRefresh).toHaveBeenCalledWith([slot], undefined);
  });

  it('applies targeting before falling back when a synthetic auction never calls back', () => {
    vi.useFakeTimers();
    try {
      const slot = {
        getSlotElementId: () => 'example-missing-refresh-callback',
        getTargeting: () => [],
        clearTargeting: vi.fn(),
      };
      const { originalRefresh, pubads } = installGpt([slot]);
      const setTargetingForGPTAsync = vi.fn();
      mockPbjs.setTargetingForGPTAsync = setTargetingForGPTAsync;
      mockRequestBids.mockImplementation(() => undefined);
      installPrebidNpm();

      pubads.refresh([slot]);
      expect(originalRefresh).not.toHaveBeenCalled();
      vi.advanceTimersByTime(640);

      expect(setTargetingForGPTAsync).toHaveBeenCalledWith(['example-missing-refresh-callback']);
      expect(setTargetingForGPTAsync.mock.invocationCallOrder[0]).toBeLessThan(
        originalRefresh.mock.invocationCallOrder[0]
      );
      expect(originalRefresh).toHaveBeenCalledTimes(1);
      expect(originalRefresh).toHaveBeenCalledWith([slot], undefined);
    } finally {
      vi.runOnlyPendingTimers();
      vi.useRealTimers();
    }
  });

  it('applies fallback targeting once and ignores a late synthetic callback', () => {
    vi.useFakeTimers();
    try {
      const slot = {
        getSlotElementId: () => 'example-late-refresh-callback',
        getTargeting: () => [],
        clearTargeting: vi.fn(),
      };
      const { originalRefresh, pubads } = installGpt([slot]);
      const setTargetingForGPTAsync = vi.fn();
      mockPbjs.setTargetingForGPTAsync = setTargetingForGPTAsync;
      let syntheticBidsBackHandler: (() => void) | undefined;
      mockRequestBids.mockImplementation((opts) => {
        syntheticBidsBackHandler = opts.bidsBackHandler;
      });
      installPrebidNpm();

      pubads.refresh([slot]);
      vi.advanceTimersByTime(640);
      syntheticBidsBackHandler?.();

      expect(setTargetingForGPTAsync).toHaveBeenCalledTimes(1);
      expect(setTargetingForGPTAsync).toHaveBeenCalledWith(['example-late-refresh-callback']);
      expect(setTargetingForGPTAsync.mock.invocationCallOrder[0]).toBeLessThan(
        originalRefresh.mock.invocationCallOrder[0]
      );
      expect(originalRefresh).toHaveBeenCalledTimes(1);
    } finally {
      vi.runOnlyPendingTimers();
      vi.useRealTimers();
    }
  });

  it('completes a synthetic refresh when targeting throws', () => {
    const slot = {
      getSlotElementId: () => 'example-throwing-targeting',
      getTargeting: () => [],
      clearTargeting: vi.fn(),
    };
    const { originalRefresh, pubads } = installGpt([slot]);
    mockPbjs.setTargetingForGPTAsync = vi.fn(() => {
      throw new Error('example targeting failure');
    });
    mockRequestBids.mockImplementation((opts) => completePublisherAuction(opts));
    installPrebidNpm();

    pubads.refresh([slot]);

    expect(originalRefresh).toHaveBeenCalledTimes(1);
    expect(originalRefresh).toHaveBeenCalledWith([slot], undefined);
  });

  it('does not stack the removeAdUnit lifecycle wrapper across installation', () => {
    const pbjs = installPrebidNpm();
    installPrebidNpm();

    (pbjs as unknown as { removeAdUnit: (adUnitCode?: string | string[]) => void }).removeAdUnit(
      'example-reinstalled-slot'
    );

    expect(mockRemoveAdUnit).toHaveBeenCalledTimes(1);
  });

  it('keeps nested publisher delivery contexts isolated during reentrant auctions', () => {
    const outerSlot = {
      getSlotElementId: () => 'example-outer-delivery',
      getTargeting: () => [],
      clearTargeting: vi.fn(),
    };
    const innerSlot = {
      getSlotElementId: () => 'example-inner-delivery',
      getTargeting: () => [],
      clearTargeting: vi.fn(),
    };
    const { originalRefresh, pubads } = installGpt([outerSlot, innerSlot]);
    mockRequestBids.mockImplementation((opts) => completePublisherAuction(opts));
    const pbjs = installPrebidNpm();

    pbjs.requestBids({
      adUnits: [
        { code: 'example-outer-delivery', bids: [{ bidder: 'exampleServer', params: {} }] },
      ],
      bidsBackHandler: () => {
        pbjs.requestBids({
          adUnits: [
            { code: 'example-inner-delivery', bids: [{ bidder: 'exampleServer', params: {} }] },
          ],
          bidsBackHandler: () => pubads.refresh([innerSlot]),
        } as unknown as RequestBidsArg);
        pubads.refresh([outerSlot]);
      },
    } as unknown as RequestBidsArg);

    expect(mockRequestBids).toHaveBeenCalledTimes(2);
    expect(innerSlot.clearTargeting).not.toHaveBeenCalled();
    expect(outerSlot.clearTargeting).not.toHaveBeenCalled();
    expect(originalRefresh).toHaveBeenNthCalledWith(1, [innerSlot], undefined);
    expect(originalRefresh).toHaveBeenNthCalledWith(2, [outerSlot], undefined);
  });

  it('cleans delivery context after a publisher callback throws', () => {
    const slot = {
      getSlotElementId: () => 'example-throwing-callback',
      getTargeting: () => [],
      getSizes: () => [[300, 250]],
      clearTargeting: vi.fn(),
    };
    const { originalRefresh, pubads } = installGpt([slot]);
    mockRequestBids.mockImplementation((opts) =>
      completePublisherAuction(opts, { applyTargeting: false })
    );
    const pbjs = installPrebidNpm();

    expect(() =>
      pbjs.requestBids({
        adUnits: [
          {
            code: 'example-throwing-callback',
            bids: [{ bidder: 'exampleServer', params: {} }],
          },
        ],
        bidsBackHandler: () => {
          throw new Error('example callback failure');
        },
      } as unknown as RequestBidsArg)
    ).toThrow('example callback failure');

    pubads.refresh([slot]);

    expect(mockRequestBids).toHaveBeenCalledTimes(2);
    expect(slot.clearTargeting).toHaveBeenCalled();
    expect(originalRefresh).toHaveBeenCalledTimes(1);
  });

  it('completes an internal synthetic refresh once without recursion', () => {
    const slot = {
      getSlotElementId: () => 'example-independent-refresh',
      getTargeting: () => [],
      getSizes: () => [[300, 250]],
      clearTargeting: vi.fn(),
    };
    const { originalRefresh, pubads } = installGpt([slot]);
    mockRequestBids.mockImplementation((opts) => completePublisherAuction(opts));
    installPrebidNpm();

    pubads.refresh([slot]);

    expect(mockRequestBids).toHaveBeenCalledTimes(1);
    expect(originalRefresh).toHaveBeenCalledTimes(1);
    expect(originalRefresh).toHaveBeenCalledWith([slot], undefined);
  });
});

describe('prebid/client-side bidders', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockPbjs.requestBids = mockRequestBids;
    mockPbjs.adUnits = [];
    mockGetUserIdsAsEids.mockReset();
    mockGetUserIdsAsEids.mockReturnValue([]);
    // By default the manifest declares all adapters compiled in.
    testWindow.__tsjs_prebid_bundle = DEFAULT_BUNDLE_MANIFEST;
    delete testWindow.__tsjs_prebid;
  });

  afterEach(() => {
    delete testWindow.__tsjs_prebid;
  });

  it('excludes client-side bidders from trustedServer bidderParams', () => {
    testWindow.__tsjs_prebid = { clientSideBidders: ['rubicon'] };

    const pbjs = installPrebidNpm();

    const adUnits = [
      {
        bids: [
          { bidder: 'appnexus', params: { placementId: 123 } },
          { bidder: 'rubicon', params: { accountId: 'abc' } },
          { bidder: 'kargo', params: { placementId: 'k1' } },
        ],
      },
    ];
    pbjs.requestBids({ adUnits } as unknown as RequestBidsArg);

    const tsBid = adUnits[0].bids.find((b: TestBid) => b.bidder === 'trustedServer') as TestBid;
    expect(tsBid).toBeDefined();
    // rubicon should NOT be in bidderParams — it runs client-side
    expect(tsBid.params.bidderParams).toEqual({
      appnexus: { placementId: 123 },
      kargo: { placementId: 'k1' },
    });
  });

  it('preserves client-side bidder bids as standalone entries', () => {
    testWindow.__tsjs_prebid = { clientSideBidders: ['rubicon'] };

    const pbjs = installPrebidNpm();

    const adUnits = [
      {
        bids: [
          { bidder: 'appnexus', params: { placementId: 123 } },
          { bidder: 'rubicon', params: { accountId: 'abc' } },
        ],
      },
    ];
    pbjs.requestBids({ adUnits } as unknown as RequestBidsArg);

    // rubicon bid should remain untouched as a standalone entry
    const rubiconBid = adUnits[0].bids.find((b: TestBid) => b.bidder === 'rubicon') as TestBid;
    expect(rubiconBid).toBeDefined();
    expect(rubiconBid.params).toEqual({ accountId: 'abc' });
    expect(adUnits[0].bids.find((b: TestBid) => b.bidder === 'appnexus')).toBeUndefined();
  });

  it('handles multiple client-side bidders', () => {
    testWindow.__tsjs_prebid = { clientSideBidders: ['rubicon', 'openx'] };

    const pbjs = installPrebidNpm();

    const adUnits = [
      {
        bids: [
          { bidder: 'appnexus', params: { placementId: 123 } },
          { bidder: 'rubicon', params: { accountId: 'abc' } },
          { bidder: 'openx', params: { unit: '456' } },
        ],
      },
    ];
    pbjs.requestBids({ adUnits } as unknown as RequestBidsArg);

    const tsBid = adUnits[0].bids.find((b: TestBid) => b.bidder === 'trustedServer') as TestBid;
    // Only appnexus should be in bidderParams
    expect(tsBid.params.bidderParams).toEqual({
      appnexus: { placementId: 123 },
    });

    // Both client-side bidders should remain
    expect(adUnits[0].bids.find((b: TestBid) => b.bidder === 'rubicon')).toBeDefined();
    expect(adUnits[0].bids.find((b: TestBid) => b.bidder === 'openx')).toBeDefined();
    expect(adUnits[0].bids.find((b: TestBid) => b.bidder === 'appnexus')).toBeUndefined();
  });

  it('behaves normally when no client-side bidders are configured', () => {
    // No __tsjs_prebid at all — all bidders go server-side
    const pbjs = installPrebidNpm();

    const adUnits = [
      {
        bids: [
          { bidder: 'appnexus', params: { placementId: 123 } },
          { bidder: 'rubicon', params: { accountId: 'abc' } },
        ],
      },
    ];
    pbjs.requestBids({ adUnits } as unknown as RequestBidsArg);

    const tsBid = adUnits[0].bids.find((b: TestBid) => b.bidder === 'trustedServer') as TestBid;
    expect(tsBid.params.bidderParams).toEqual({
      appnexus: { placementId: 123 },
      rubicon: { accountId: 'abc' },
    });
  });

  it('behaves normally when client-side bidders list is empty', () => {
    testWindow.__tsjs_prebid = { clientSideBidders: [] };

    const pbjs = installPrebidNpm();

    const adUnits = [
      {
        bids: [
          { bidder: 'appnexus', params: { placementId: 123 } },
          { bidder: 'rubicon', params: { accountId: 'abc' } },
        ],
      },
    ];
    pbjs.requestBids({ adUnits } as unknown as RequestBidsArg);

    const tsBid = adUnits[0].bids.find((b: TestBid) => b.bidder === 'trustedServer') as TestBid;
    expect(tsBid.params.bidderParams).toEqual({
      appnexus: { placementId: 123 },
      rubicon: { accountId: 'abc' },
    });
  });

  it('still injects trustedServer when all bidders are client-side', () => {
    testWindow.__tsjs_prebid = { clientSideBidders: ['rubicon', 'appnexus'] };

    const pbjs = installPrebidNpm();

    const adUnits = [
      {
        bids: [
          { bidder: 'rubicon', params: { accountId: 'abc' } },
          { bidder: 'appnexus', params: { placementId: 123 } },
        ],
      },
    ];
    pbjs.requestBids({ adUnits } as unknown as RequestBidsArg);

    // trustedServer should still be present (even with empty bidderParams)
    const tsBid = adUnits[0].bids.find((b: TestBid) => b.bidder === 'trustedServer') as TestBid;
    expect(tsBid).toBeDefined();
    expect(tsBid.params.bidderParams).toEqual({});
  });

  it('logs error when a client-side bidder has no adapter in the external bundle', () => {
    // rubicon is compiled into the external bundle, but openx is not
    testWindow.__tsjs_prebid_bundle = {
      ...DEFAULT_BUNDLE_MANIFEST,
      adapters: ['rubicon'],
      bidderCodes: ['rubicon'],
    };
    testWindow.__tsjs_prebid = { clientSideBidders: ['rubicon', 'openx'] };

    const errorSpy = vi.spyOn(console, 'error').mockImplementation(() => {});

    installPrebidNpm();

    // Should log an error for the missing adapter.
    // log.error() uses styled console output: console.error('%c[tsjs]%c ...:', style, reset, ...args)
    // so the actual message is the 4th argument.
    const errorCalls = errorSpy.mock.calls;
    const hasOpenxError = errorCalls.some((args) =>
      args.some(
        (a) =>
          typeof a === 'string' &&
          a.includes('client-side bidder "openx" has no adapter in the external Prebid bundle')
      )
    );
    expect(hasOpenxError).toBe(true);

    // The error should point at the operator surface: the CLI config key,
    // not the internal build script.
    const pointsAtBundleConfig = errorCalls.some((args) =>
      args.some((a) => typeof a === 'string' && a.includes('[integrations.prebid.bundle].adapters'))
    );
    expect(pointsAtBundleConfig).toBe(true);

    // Should NOT log an error for the compiled-in adapter
    const hasRubiconError = errorCalls.some((args) =>
      args.some((a) => typeof a === 'string' && a.includes('client-side bidder "rubicon"'))
    );
    expect(hasRubiconError).toBe(false);

    errorSpy.mockRestore();
    testWindow.__tsjs_prebid_bundle = DEFAULT_BUNDLE_MANIFEST;
  });

  it('accepts alias bidder codes stamped in bidderCodes', () => {
    // The adf module registers adf plus the adform/adformOpenRTB aliases;
    // the module-name list alone would flag them as missing.
    testWindow.__tsjs_prebid_bundle = {
      ...DEFAULT_BUNDLE_MANIFEST,
      adapters: ['adf'],
      bidderCodes: ['adf', 'adform', 'adformOpenRTB'],
    };
    testWindow.__tsjs_prebid = { clientSideBidders: ['adform'] };

    const errorSpy = vi.spyOn(console, 'error').mockImplementation(() => {});

    installPrebidNpm();

    const hasAdapterError = errorSpy.mock.calls.some((args) =>
      args.some(
        (a) => typeof a === 'string' && a.includes('has no adapter in the external Prebid bundle')
      )
    );
    expect(hasAdapterError).toBe(false);

    errorSpy.mockRestore();
    testWindow.__tsjs_prebid_bundle = DEFAULT_BUNDLE_MANIFEST;
  });

  it('rejects a module file stem that is not a registered bidder code', () => {
    // a1MediaBidAdapter.js registers a1media — configuring the file stem
    // must be flagged even though the module itself is compiled in.
    testWindow.__tsjs_prebid_bundle = {
      ...DEFAULT_BUNDLE_MANIFEST,
      adapters: ['a1Media'],
      bidderCodes: ['a1media'],
    };
    testWindow.__tsjs_prebid = { clientSideBidders: ['a1Media'] };

    const errorSpy = vi.spyOn(console, 'error').mockImplementation(() => {});

    installPrebidNpm();

    const hasAdapterError = errorSpy.mock.calls.some((args) =>
      args.some(
        (a) =>
          typeof a === 'string' &&
          a.includes('client-side bidder "a1Media" has no adapter in the external Prebid bundle')
      )
    );
    expect(hasAdapterError).toBe(true);

    errorSpy.mockRestore();
    testWindow.__tsjs_prebid_bundle = DEFAULT_BUNDLE_MANIFEST;
  });

  it('treats a malformed manifest as unstamped instead of throwing', () => {
    // The manifest is a plain window global any page script can overwrite.
    testWindow.__tsjs_prebid_bundle = { adapters: 'rubicon', userIdModules: 42 };
    testWindow.__tsjs_prebid = { clientSideBidders: ['rubicon'] };

    const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});

    expect(() => installPrebidNpm()).not.toThrow();

    const hasManifestWarn = warnSpy.mock.calls.some((args) =>
      args.some((a) => typeof a === 'string' && a.includes('did not stamp an adapter manifest'))
    );
    expect(hasManifestWarn).toBe(true);

    warnSpy.mockRestore();
    testWindow.__tsjs_prebid_bundle = DEFAULT_BUNDLE_MANIFEST;
  });

  it('warns when the external bundle stamped no adapter manifest', () => {
    delete testWindow.__tsjs_prebid_bundle;
    testWindow.__tsjs_prebid = { clientSideBidders: ['rubicon'] };

    const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});

    installPrebidNpm();

    const hasManifestWarn = warnSpy.mock.calls.some((args) =>
      args.some((a) => typeof a === 'string' && a.includes('did not stamp an adapter manifest'))
    );
    expect(hasManifestWarn).toBe(true);

    warnSpy.mockRestore();
    testWindow.__tsjs_prebid_bundle = DEFAULT_BUNDLE_MANIFEST;
  });

  it('does not log errors when all client-side bidders have adapters', () => {
    testWindow.__tsjs_prebid = { clientSideBidders: ['rubicon'] };

    const errorSpy = vi.spyOn(console, 'error').mockImplementation(() => {});

    installPrebidNpm();

    const hasAdapterError = errorSpy.mock.calls.some((args) =>
      args.some(
        (a) => typeof a === 'string' && a.includes('has no adapter in the external Prebid bundle')
      )
    );
    expect(hasAdapterError).toBe(false);

    errorSpy.mockRestore();
  });
});

describe('prebid/self-init without the external bundle', () => {
  afterEach(() => {
    // Restore the module registry and the full mock global for later suites.
    testWindow.pbjs = mockPbjs;
    delete testWindow.googletag;
    vi.resetModules();
  });

  it('keeps basic Prebid enabled when only the APS lifecycle API is unavailable', async () => {
    vi.resetModules();
    const registerBidAdapter = vi.fn();
    const onEvent = vi.fn();
    const originalRequestBids = vi.fn();
    const compatiblePbjs = {
      ...mockPbjs,
      registerBidAdapter,
      onEvent,
      requestBids: originalRequestBids,
      markWinningBidAsUsed: undefined,
      que: [] as Array<() => void>,
      cmd: [] as Array<() => void>,
    };
    testWindow.pbjs = compatiblePbjs;
    const pubads = { refresh: vi.fn() };
    const cmdPush = vi.fn((callback: () => void) => callback());
    testWindow.googletag = { cmd: { push: cmdPush }, pubads: () => pubads };
    const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});

    await import('../../../src/integrations/prebid/index');

    expect(registerBidAdapter).toHaveBeenCalledTimes(1);
    expect(compatiblePbjs.requestBids).not.toBe(originalRequestBids);

    const adapter = registerBidAdapter.mock.calls[0][2] as TestAdapterSpec;
    const convertedBids = adapter.interpretResponse(
      {
        body: {
          seatbid: [
            {
              seat: 'appnexus',
              bid: [
                {
                  impid: 'ordinary-slot',
                  price: 2.5,
                  adm: '<div>ordinary creative</div>',
                  w: 300,
                  h: 250,
                },
              ],
            },
            {
              seat: 'aps',
              bid: [
                {
                  impid: 'aps-slot',
                  price: 3.5,
                  ext: { trusted_server: { renderer: apsRenderer() } },
                },
              ],
            },
          ],
        },
      },
      {
        tsjsBidRequests: [
          { adUnitCode: 'ordinary-slot', bidId: 'ordinary-request' },
          { adUnitCode: 'aps-slot', bidId: 'aps-request' },
        ],
      }
    );

    expect(convertedBids).toHaveLength(1);
    expect(convertedBids[0]).toEqual(
      expect.objectContaining({
        requestId: 'ordinary-request',
        bidderCode: 'appnexus',
        ad: '<div>ordinary creative</div>',
      })
    );
    expect(convertedBids).not.toEqual(
      expect.arrayContaining([expect.objectContaining({ bidderCode: 'aps' })])
    );
    expect(onEvent).not.toHaveBeenCalledWith('bidResponse', expect.any(Function));
    expect(
      warnSpy.mock.calls.some((args) =>
        args.some(
          (value) => typeof value === 'string' && value.includes('APS renderer bids disabled')
        )
      )
    ).toBe(true);
    expect(testWindow.__tsjsPrebidShimInstalled).toBe(true);

    warnSpy.mockRestore();
  });
});

describe('prebid self-init user ID module timing', () => {
  const userSyncCallCount = () =>
    mockSetConfig.mock.calls.filter(([arg]) => arg && typeof arg === 'object' && 'userSync' in arg)
      .length;

  const setReadyState = (value: DocumentReadyState) => {
    Object.defineProperty(document, 'readyState', { value, configurable: true });
  };

  beforeEach(() => {
    vi.resetModules();
    mockSetConfig.mockClear();
  });

  afterEach(() => {
    setReadyState('complete');
  });

  it('installs user ID modules immediately when the bundle loads after window load', async () => {
    // The GPT slim loader appends this bundle from a window.load handler, so
    // the document is already complete — a load listener would never fire.
    setReadyState('complete');

    await import('../../../src/integrations/prebid/index');

    expect(userSyncCallCount()).toBeGreaterThan(0);
  });

  it('defers user ID modules to window load when the document is still loading', async () => {
    setReadyState('loading');

    await import('../../../src/integrations/prebid/index');

    expect(userSyncCallCount()).toBe(0);

    window.dispatchEvent(new Event('load'));
    expect(userSyncCallCount()).toBe(1);

    // { once: true } — a second load event must not reinstall.
    window.dispatchEvent(new Event('load'));
    expect(userSyncCallCount()).toBe(1);
  });
});
