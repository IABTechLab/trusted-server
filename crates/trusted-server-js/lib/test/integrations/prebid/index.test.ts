import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import envelope from '../../fixtures/aps-renderer-v1.json';

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

// Define mocks using vi.hoisted so they're available inside vi.mock factories
const {
  mockSetConfig,
  mockProcessQueue,
  mockRequestBids,
  mockRegisterBidAdapter,
  mockGetUserIdsAsEids,
  mockGetConfig,
  mockMarkBidAsRendered,
  mockMarkWinner,
  mockOnEvent,
  mockPbjs,
  mockGetBidAdapter,
  mockAdapterManager,
} = vi.hoisted(() => {
  const mockSetConfig = vi.fn();
  const mockProcessQueue = vi.fn();
  const mockRequestBids = vi.fn();
  const mockRegisterBidAdapter = vi.fn();
  const mockGetBidAdapter = vi.fn();
  const mockMarkBidAsRendered = vi.fn();
  const mockMarkWinner = vi.fn();
  const mockOnEvent = vi.fn();
  const mockGetUserIdsAsEids = vi.fn(
    () => [] as Array<{ source: string; uids?: Array<{ id: string; atype?: number }> }>
  );
  const mockGetConfig = vi.fn();
  const mockPbjs = {
    setConfig: mockSetConfig,
    processQueue: mockProcessQueue,
    requestBids: mockRequestBids,
    registerBidAdapter: mockRegisterBidAdapter,
    getUserIdsAsEids: mockGetUserIdsAsEids,
    getConfig: mockGetConfig,
    onEvent: mockOnEvent,
    adUnits: [] as any[],
  };
  const mockAdapterManager = {
    getBidAdapter: mockGetBidAdapter,
  };
  return {
    mockSetConfig,
    mockProcessQueue,
    mockRequestBids,
    mockRegisterBidAdapter,
    mockGetUserIdsAsEids,
    mockGetConfig,
    mockMarkBidAsRendered,
    mockMarkWinner,
    mockOnEvent,
    mockPbjs,
    mockGetBidAdapter,
    mockAdapterManager,
  };
});

// Mock prebid.js before importing the module under test.
// The real prebid.js cannot run in jsdom, so we provide a minimal stub.
vi.mock('prebid.js', () => ({ default: mockPbjs }));
vi.mock('prebid.js/src/adapterManager.js', () => ({ default: mockAdapterManager }));
vi.mock('prebid.js/src/adRendering.js', () => ({
  markBidAsRendered: mockMarkBidAsRendered,
  markWinner: mockMarkWinner,
}));

// Side-effect imports are no-ops in tests
vi.mock('prebid.js/modules/consentManagementTcf.js', () => ({}));
vi.mock('prebid.js/modules/consentManagementGpp.js', () => ({}));
vi.mock('prebid.js/modules/consentManagementUsp.js', () => ({}));
vi.mock('prebid.js/modules/userId.js', () => ({}));

// Mock the build-generated imports in tests.
vi.mock('../../../src/integrations/prebid/_adapters.generated', () => ({}));
vi.mock('../../../src/integrations/prebid/_user_ids.generated', () => ({
  INCLUDED_PREBID_USER_ID_MODULES: ['sharedIdSystem'],
}));

import {
  collectBidders,
  getInjectedConfig,
  auctionBidsToPrebidBids,
  installPrebidNpm,
  installRefreshHandler,
} from '../../../src/integrations/prebid/index';
import type { AuctionBid } from '../../../src/core/auction';
import { log } from '../../../src/core/log';

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
    delete (window as any).__tsjs_prebid;
  });

  it('returns undefined when window.__tsjs_prebid is not set', () => {
    expect(getInjectedConfig()).toBeUndefined();
  });

  it('returns the injected config when present', () => {
    (window as any).__tsjs_prebid = { accountId: 'server-42', timeout: 2000 };
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

    const result = auctionBidsToPrebidBids(auctionBids, bidRequests);

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

    const result = auctionBidsToPrebidBids(auctionBids, [
      { adUnitCode: 'div-aps', bidId: 'prebid-request-id' },
    ]);

    expect(result).toHaveLength(1);
    expect(result[0]).toEqual(
      expect.objectContaining({
        requestId: 'prebid-request-id',
        bidderCode: 'aps',
        ad: '',
        trustedServerRenderer: renderer,
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
      [{ adUnitCode: 'div-aps', bidId: 'prebid-request-id' }]
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

    const result = auctionBidsToPrebidBids(auctionBids, []);

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

    const result = auctionBidsToPrebidBids(auctionBids, bidRequests);

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
    delete (window as any).__tsjs_prebid;
    delete (window as any).__tsjs_prebid_diagnostics;
    delete (window as any).tsjs;
    delete (mockPbjs as any).__tsApsBidResponseListenerInstalled;
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

    const entry = (window as any).tsjs.apsPrebidRenderers['prebid-generated-ad-id'];
    expect(entry).toEqual(
      expect.objectContaining({
        adUnitCode: 'div-aps',
        renderer,
        expiresAt: expect.any(Number),
        markRendered: expect.any(Function),
        markWinner: expect.any(Function),
      })
    );

    entry.markWinner();
    entry.markRendered();
    expect(mockMarkWinner).toHaveBeenCalledWith(
      expect.objectContaining({ adId: 'prebid-generated-ad-id' })
    );
    expect(mockMarkBidAsRendered).toHaveBeenCalledWith(
      expect.objectContaining({ adId: 'prebid-generated-ad-id' })
    );
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

    expect((window as any).tsjs?.apsPrebidRenderers?.['malformed-ad-id']).toBeUndefined();
    expect((window as any).tsjs?.apsPrebidRenderers?.['foreign-ad-id']).toBeUndefined();
    expect(malformedBid).toHaveProperty('trustedServerRenderer');
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

    expect((window as any).__tsjs_prebid_diagnostics.userIdModules).toEqual({
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

    expect((window as any).__tsjs_prebid_diagnostics.userIdModules).toEqual({
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

  describe('adapter spec', () => {
    function getAdapterSpec(): any {
      installPrebidNpm();
      return mockRegisterBidAdapter.mock.calls[0][2];
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
      pbjs.requestBids({ adUnits } as any);

      // Each ad unit should have trustedServer added
      for (const unit of adUnits) {
        const hasTsBidder = unit.bids.some((b: any) => b.bidder === 'trustedServer');
        expect(hasTsBidder).toBe(true);
      }

      const trustedServerBid = adUnits[0].bids.find((b: any) => b.bidder === 'trustedServer');
      expect(trustedServerBid.params.bidderParams).toEqual({ appnexus: {} });
      expect(adUnits[0].bids.map((b: any) => b.bidder)).toEqual(['trustedServer']);
      expect(adUnits[1].bids.map((b: any) => b.bidder)).toEqual(['trustedServer']);

      // Should call through to original requestBids
      expect(mockRequestBids).toHaveBeenCalled();
    });

    it('does not duplicate trustedServer if already present', () => {
      const pbjs = installPrebidNpm();

      const adUnits = [{ bids: [{ bidder: 'trustedServer', params: {} }] }];
      pbjs.requestBids({ adUnits } as any);

      const tsCount = adUnits[0].bids.filter((b: any) => b.bidder === 'trustedServer').length;
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
      pbjs.requestBids({ adUnits } as any);

      const trustedServerBid = adUnits[0].bids.find((b: any) => b.bidder === 'trustedServer');
      expect(trustedServerBid).toBeDefined();
      expect(trustedServerBid.params.bidderParams).toEqual({
        appnexus: { placementId: 123 },
        rubicon: { accountId: 'abc' },
      });
      expect(adUnits[0].bids.map((b: any) => b.bidder)).toEqual(['trustedServer']);
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
      pbjs.requestBids({ adUnits } as any);

      // Second auction (refresh/re-auction) with the SAME ad unit object: the
      // server-side bidder entries were already pruned, so the shim must not
      // overwrite the captured params with an empty object.
      pbjs.requestBids({ adUnits } as any);

      const trustedServerBid = adUnits[0].bids.find(
        (b: any) => b.bidder === 'trustedServer'
      ) as any;
      expect(trustedServerBid.params.bidderParams).toEqual({
        appnexus: { placementId: 123 },
        rubicon: { accountId: 'abc' },
      });
    });

    it('adds bids array to ad units that have none', () => {
      const pbjs = installPrebidNpm();

      const adUnits = [{ code: 'div-1' }] as any[];
      pbjs.requestBids({ adUnits } as any);

      expect(adUnits[0].bids).toHaveLength(1);
      expect(adUnits[0].bids[0].bidder).toBe('trustedServer');
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
      pbjs.requestBids({ adUnits } as any);

      const tsBid0 = adUnits[0].bids.find((b: any) => b.bidder === 'trustedServer') as any;
      expect(tsBid0.params.zone).toBe('header');

      const tsBid1 = adUnits[1].bids.find((b: any) => b.bidder === 'trustedServer') as any;
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
      pbjs.requestBids({ adUnits } as any);

      const tsBid = adUnits[0].bids.find((b: any) => b.bidder === 'trustedServer') as any;
      expect(tsBid.params.zone).toBeUndefined();
    });

    it('omits zone when ad unit has no mediaTypes', () => {
      const pbjs = installPrebidNpm();

      const adUnits = [{ bids: [{ bidder: 'rubicon', params: {} }] }];
      pbjs.requestBids({ adUnits } as any);

      const tsBid = adUnits[0].bids.find((b: any) => b.bidder === 'trustedServer') as any;
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

      pbjs.requestBids({ adUnits } as any);

      let tsBid = adUnits[0].bids.find((b: any) => b.bidder === 'trustedServer') as any;
      expect(tsBid.params.zone).toBe('header');
      expect(tsBid.params.custom).toBe('keep');

      delete adUnits[0].mediaTypes.banner.name;
      pbjs.requestBids({ adUnits } as any);

      tsBid = adUnits[0].bids.find((b: any) => b.bidder === 'trustedServer') as any;
      expect(tsBid.params.zone).toBeUndefined();
      expect(tsBid.params.custom).toBe('keep');
    });

    it('falls back to pbjs.adUnits when requestObj has no adUnits', () => {
      const pbjs = installPrebidNpm();

      mockPbjs.adUnits = [{ bids: [{ bidder: 'openx', params: {} }] }] as any[];
      pbjs.requestBids({} as any);

      const hasTsBidder = (mockPbjs.adUnits[0] as any).bids.some(
        (b: any) => b.bidder === 'trustedServer'
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
      pbjs.requestBids({ adUnits: [{ bids: [{ bidder: 'appnexus', params: {} }] }] } as any);

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
      pbjs.requestBids({ adUnits: [{ bids: [{ bidder: 'appnexus', params: {} }] }] } as any);

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
    delete (window as any).__tsjs_prebid;
  });

  afterEach(() => {
    delete (window as any).__tsjs_prebid;
  });

  it('reads timeout and debug from window.__tsjs_prebid', () => {
    (window as any).__tsjs_prebid = { timeout: 1500, debug: true };

    installPrebidNpm();

    expect(mockSetConfig).toHaveBeenCalledWith(
      expect.objectContaining({ debug: true, bidderTimeout: 1500 })
    );
  });

  it('explicit config overrides server-injected values', () => {
    (window as any).__tsjs_prebid = { timeout: 1500, debug: true };

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
    (window as any).tsjs = undefined;
    delete (window as any).googletag;
  });

  afterEach(() => {
    (window as any).tsjs = undefined;
    delete (window as any).googletag;
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
    (window as any).googletag = {
      cmd: { push: (fn: () => void) => fn() },
      pubads: () => pubads,
    };
    (window as any).tsjs = {
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
    (window as any).googletag = {
      cmd: { push: (fn: () => void) => fn() },
      pubads: () => pubads,
    };
    (window as any).tsjs = {
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
    (mockPbjs as any).setTargetingForGPTAsync = setTargetingForGPTAsync;
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
    (window as any).googletag = {
      cmd: { push: (fn: () => void) => fn() },
      pubads: () => pubads,
    };
    (window as any).tsjs = {
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

    delete (mockPbjs as any).setTargetingForGPTAsync;
  });

  it('includes configured client-side bidders in refresh ad units', () => {
    (window as any).__tsjs_prebid = { clientSideBidders: ['rubicon'] };
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
    (window as any).googletag = {
      cmd: { push: (fn: () => void) => fn() },
      pubads: () => pubads,
    };
    (window as any).tsjs = {
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

    delete (window as any).__tsjs_prebid;
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
    (window as any).googletag = {
      cmd: { push: (fn: () => void) => fn() },
      pubads: () => pubads,
    };
    (window as any).tsjs = {
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
    (window as any).__tsjs_prebid = { clientSideBidders: ['rubicon'] };
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
    (window as any).googletag = {
      cmd: { push: (fn: () => void) => fn() },
      pubads: () => pubads,
    };
    (window as any).tsjs = {
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

    delete (window as any).__tsjs_prebid;
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
    (window as any).googletag = {
      cmd: { push: (fn: () => void) => fn() },
      pubads: () => pubads,
    };
    (window as any).tsjs = {
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
    (mockPbjs as any).setTargetingForGPTAsync = setTargetingForGPTAsync;
    (window as any).googletag = {
      cmd: { push: (fn: () => void) => fn() },
      pubads: () => pubads,
    };
    (window as any).tsjs = {
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
    (window as any).googletag = {
      cmd: { push: (fn: () => void) => fn() },
      pubads: () => pubads,
    };
    (window as any).tsjs = { adInitRefreshInProgress: true };

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
    (window as any).googletag = {
      cmd: { push: (fn: () => void) => fn() },
      pubads: () => pubads,
    };
    (window as any).tsjs = { adInitRefreshInProgress: false };

    installRefreshHandler(750);
    pubads.refresh([gptSlot]);

    expect(mockRequestBids).toHaveBeenCalled();
    expect(originalRefresh).not.toHaveBeenCalled();
  });
});

describe('prebid/client-side bidders', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockPbjs.requestBids = mockRequestBids;
    mockPbjs.adUnits = [];
    mockGetUserIdsAsEids.mockReset();
    mockGetUserIdsAsEids.mockReturnValue([]);
    // By default, pretend all adapters are registered
    mockGetBidAdapter.mockReturnValue({});
    delete (window as any).__tsjs_prebid;
  });

  afterEach(() => {
    delete (window as any).__tsjs_prebid;
  });

  it('excludes client-side bidders from trustedServer bidderParams', () => {
    (window as any).__tsjs_prebid = { clientSideBidders: ['rubicon'] };

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
    pbjs.requestBids({ adUnits } as any);

    const tsBid = adUnits[0].bids.find((b: any) => b.bidder === 'trustedServer') as any;
    expect(tsBid).toBeDefined();
    // rubicon should NOT be in bidderParams — it runs client-side
    expect(tsBid.params.bidderParams).toEqual({
      appnexus: { placementId: 123 },
      kargo: { placementId: 'k1' },
    });
  });

  it('preserves client-side bidder bids as standalone entries', () => {
    (window as any).__tsjs_prebid = { clientSideBidders: ['rubicon'] };

    const pbjs = installPrebidNpm();

    const adUnits = [
      {
        bids: [
          { bidder: 'appnexus', params: { placementId: 123 } },
          { bidder: 'rubicon', params: { accountId: 'abc' } },
        ],
      },
    ];
    pbjs.requestBids({ adUnits } as any);

    // rubicon bid should remain untouched as a standalone entry
    const rubiconBid = adUnits[0].bids.find((b: any) => b.bidder === 'rubicon') as any;
    expect(rubiconBid).toBeDefined();
    expect(rubiconBid.params).toEqual({ accountId: 'abc' });
    expect(adUnits[0].bids.find((b: any) => b.bidder === 'appnexus')).toBeUndefined();
  });

  it('handles multiple client-side bidders', () => {
    (window as any).__tsjs_prebid = { clientSideBidders: ['rubicon', 'openx'] };

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
    pbjs.requestBids({ adUnits } as any);

    const tsBid = adUnits[0].bids.find((b: any) => b.bidder === 'trustedServer') as any;
    // Only appnexus should be in bidderParams
    expect(tsBid.params.bidderParams).toEqual({
      appnexus: { placementId: 123 },
    });

    // Both client-side bidders should remain
    expect(adUnits[0].bids.find((b: any) => b.bidder === 'rubicon')).toBeDefined();
    expect(adUnits[0].bids.find((b: any) => b.bidder === 'openx')).toBeDefined();
    expect(adUnits[0].bids.find((b: any) => b.bidder === 'appnexus')).toBeUndefined();
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
    pbjs.requestBids({ adUnits } as any);

    const tsBid = adUnits[0].bids.find((b: any) => b.bidder === 'trustedServer') as any;
    expect(tsBid.params.bidderParams).toEqual({
      appnexus: { placementId: 123 },
      rubicon: { accountId: 'abc' },
    });
  });

  it('behaves normally when client-side bidders list is empty', () => {
    (window as any).__tsjs_prebid = { clientSideBidders: [] };

    const pbjs = installPrebidNpm();

    const adUnits = [
      {
        bids: [
          { bidder: 'appnexus', params: { placementId: 123 } },
          { bidder: 'rubicon', params: { accountId: 'abc' } },
        ],
      },
    ];
    pbjs.requestBids({ adUnits } as any);

    const tsBid = adUnits[0].bids.find((b: any) => b.bidder === 'trustedServer') as any;
    expect(tsBid.params.bidderParams).toEqual({
      appnexus: { placementId: 123 },
      rubicon: { accountId: 'abc' },
    });
  });

  it('still injects trustedServer when all bidders are client-side', () => {
    (window as any).__tsjs_prebid = { clientSideBidders: ['rubicon', 'appnexus'] };

    const pbjs = installPrebidNpm();

    const adUnits = [
      {
        bids: [
          { bidder: 'rubicon', params: { accountId: 'abc' } },
          { bidder: 'appnexus', params: { placementId: 123 } },
        ],
      },
    ];
    pbjs.requestBids({ adUnits } as any);

    // trustedServer should still be present (even with empty bidderParams)
    const tsBid = adUnits[0].bids.find((b: any) => b.bidder === 'trustedServer') as any;
    expect(tsBid).toBeDefined();
    expect(tsBid.params.bidderParams).toEqual({});
  });

  it('logs error when a client-side bidder has no adapter loaded', () => {
    // rubicon is registered, but openx is not
    mockGetBidAdapter.mockImplementation((bidder: string) =>
      bidder === 'rubicon' ? {} : undefined
    );
    (window as any).__tsjs_prebid = { clientSideBidders: ['rubicon', 'openx'] };

    const errorSpy = vi.spyOn(console, 'error').mockImplementation(() => {});

    installPrebidNpm();

    // Should have been called to check both bidders
    expect(mockGetBidAdapter).toHaveBeenCalledWith('rubicon');
    expect(mockGetBidAdapter).toHaveBeenCalledWith('openx');

    // Should log an error for the missing adapter.
    // log.error() uses styled console output: console.error('%c[tsjs]%c ...:', style, reset, ...args)
    // so the actual message is the 4th argument.
    const errorCalls = errorSpy.mock.calls;
    const hasOpenxError = errorCalls.some((args) =>
      args.some(
        (a) =>
          typeof a === 'string' && a.includes('client-side bidder "openx" has no adapter loaded')
      )
    );
    expect(hasOpenxError).toBe(true);

    // Should NOT log an error for the registered adapter
    const hasRubiconError = errorCalls.some((args) =>
      args.some((a) => typeof a === 'string' && a.includes('client-side bidder "rubicon"'))
    );
    expect(hasRubiconError).toBe(false);

    errorSpy.mockRestore();
  });

  it('does not log errors when all client-side bidders have adapters', () => {
    mockGetBidAdapter.mockReturnValue({});
    (window as any).__tsjs_prebid = { clientSideBidders: ['rubicon'] };

    const errorSpy = vi.spyOn(console, 'error').mockImplementation(() => {});

    installPrebidNpm();

    const hasAdapterError = errorSpy.mock.calls.some((args) =>
      args.some((a) => typeof a === 'string' && a.includes('has no adapter loaded'))
    );
    expect(hasAdapterError).toBe(false);

    errorSpy.mockRestore();
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
