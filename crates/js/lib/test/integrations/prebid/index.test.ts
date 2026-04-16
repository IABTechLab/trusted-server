import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';

// Define mocks using vi.hoisted so they're available inside vi.mock factories
const {
  mockSetConfig,
  mockProcessQueue,
  mockRequestBids,
  mockRegisterBidAdapter,
  mockGetUserIdsAsEids,
  mockPbjs,
  mockGetBidAdapter,
  mockAdapterManager,
} = vi.hoisted(() => {
  const mockSetConfig = vi.fn();
  const mockProcessQueue = vi.fn();
  const mockRequestBids = vi.fn();
  const mockRegisterBidAdapter = vi.fn();
  const mockGetBidAdapter = vi.fn();
  const mockGetUserIdsAsEids = vi.fn(
    () => [] as Array<{ source: string; uids?: Array<{ id: string; atype?: number }> }>
  );
  const mockPbjs = {
    setConfig: mockSetConfig,
    processQueue: mockProcessQueue,
    requestBids: mockRequestBids,
    registerBidAdapter: mockRegisterBidAdapter,
    getUserIdsAsEids: mockGetUserIdsAsEids,
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
    mockPbjs,
    mockGetBidAdapter,
    mockAdapterManager,
  };
});

// Mock prebid.js before importing the module under test.
// The real prebid.js cannot run in jsdom, so we provide a minimal stub.
vi.mock('prebid.js', () => ({ default: mockPbjs }));
vi.mock('prebid.js/src/adapterManager.js', () => ({ default: mockAdapterManager }));

// Side-effect imports are no-ops in tests
vi.mock('prebid.js/modules/consentManagementTcf.js', () => ({}));
vi.mock('prebid.js/modules/consentManagementGpp.js', () => ({}));
vi.mock('prebid.js/modules/consentManagementUsp.js', () => ({}));
vi.mock('prebid.js/modules/userId.js', () => ({}));

// User ID Module core — no-op mock so jsdom does not try to execute the
// real Prebid code paths.
vi.mock('prebid.js/modules/userId.js', () => ({}));

// Mock the build-generated adapter and User ID submodule imports (no-op in tests)
vi.mock('../../../src/integrations/prebid/_adapters.generated', () => ({}));
vi.mock('../../../src/integrations/prebid/_user_ids.generated', () => ({}));

import {
  collectBidders,
  getInjectedConfig,
  auctionBidsToPrebidBids,
  installPrebidNpm,
} from '../../../src/integrations/prebid/index';
import type { AuctionBid } from '../../../src/core/auction';

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
    document.cookie = 'ts-eids=; Path=/; Max-Age=0';
    delete (window as any).__tsjs_prebid;
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
              atype: 999,
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

describe('prebid/syncPrebidEidsCookie (via bidsBackHandler)', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockPbjs.requestBids = mockRequestBids;
    mockPbjs.adUnits = [];
    mockGetUserIdsAsEids.mockReset();
    mockGetUserIdsAsEids.mockReturnValue([]);
    // Restore the pbjs→mock wiring in case a prior test blanked it out.
    (mockPbjs as any).getUserIdsAsEids = mockGetUserIdsAsEids;
    delete (window as any).__tsjs_prebid;
    // Wipe any leftover ts-eids cookie from previous tests.
    document.cookie = 'ts-eids=; Path=/; Max-Age=0';
  });

  afterEach(() => {
    document.cookie = 'ts-eids=; Path=/; Max-Age=0';
  });

  /**
   * Helper: make mockRequestBids actually invoke the injected bidsBackHandler
   * so the shim's post-auction sync path runs.
   */
  function wireBidsBackHandler(): void {
    mockRequestBids.mockImplementation((opts: any) => {
      if (typeof opts?.bidsBackHandler === 'function') {
        opts.bidsBackHandler();
      }
    });
  }

  function getTsEidsCookie(): string | undefined {
    const match = document.cookie.split('; ').find((c) => c.startsWith('ts-eids='));
    return match ? match.split('=').slice(1).join('=') : undefined;
  }

  it('writes no cookie when getUserIdsAsEids returns empty array', () => {
    wireBidsBackHandler();
    const pbjs = installPrebidNpm();
    mockGetUserIdsAsEids.mockReturnValue([]);

    pbjs.requestBids({ adUnits: [] } as any);

    expect(getTsEidsCookie()).toBeUndefined();
  });

  it('writes ts-eids cookie with base64-encoded flat JSON for normal payload', () => {
    wireBidsBackHandler();
    const pbjs = installPrebidNpm();
    mockGetUserIdsAsEids.mockReturnValue([
      { source: 'sharedid.org', uids: [{ id: 'shared-abc', atype: 1 }] },
      { source: 'id5-sync.com', uids: [{ id: 'id5-xyz', atype: 3 }] },
    ]);

    pbjs.requestBids({ adUnits: [] } as any);

    const encoded = getTsEidsCookie();
    expect(encoded).toBeDefined();
    const decoded = JSON.parse(atob(encoded!));
    expect(decoded).toEqual([
      { source: 'sharedid.org', id: 'shared-abc', atype: 1 },
      { source: 'id5-sync.com', id: 'id5-xyz', atype: 3 },
    ]);
  });

  it('defaults atype to 3 when the uid omits it', () => {
    wireBidsBackHandler();
    const pbjs = installPrebidNpm();
    mockGetUserIdsAsEids.mockReturnValue([{ source: 'example.com', uids: [{ id: 'no-atype' }] }]);

    pbjs.requestBids({ adUnits: [] } as any);

    const decoded = JSON.parse(atob(getTsEidsCookie()!));
    expect(decoded).toEqual([{ source: 'example.com', id: 'no-atype', atype: 3 }]);
  });

  it('skips EID entries that are missing id or source', () => {
    wireBidsBackHandler();
    const pbjs = installPrebidNpm();
    mockGetUserIdsAsEids.mockReturnValue([
      { source: 'good.example', uids: [{ id: 'keep', atype: 1 }] },
      { source: 'empty-uids.example', uids: [] },
      { source: '', uids: [{ id: 'no-source', atype: 1 }] },
      { source: 'no-id.example', uids: [{ id: '', atype: 1 }] },
    ]);

    pbjs.requestBids({ adUnits: [] } as any);

    const decoded = JSON.parse(atob(getTsEidsCookie()!));
    expect(decoded).toEqual([{ source: 'good.example', id: 'keep', atype: 1 }]);
  });

  it('takes the first uid per source when multiple are present', () => {
    wireBidsBackHandler();
    const pbjs = installPrebidNpm();
    mockGetUserIdsAsEids.mockReturnValue([
      {
        source: 'multi.example',
        uids: [
          { id: 'first', atype: 1 },
          { id: 'second', atype: 2 },
        ],
      },
    ]);

    pbjs.requestBids({ adUnits: [] } as any);

    const decoded = JSON.parse(atob(getTsEidsCookie()!));
    expect(decoded).toEqual([{ source: 'multi.example', id: 'first', atype: 1 }]);
  });

  it('trims EIDs from the tail when the cookie payload would exceed 3072 bytes', () => {
    wireBidsBackHandler();
    const pbjs = installPrebidNpm();

    // Build ~20 entries each ~200 bytes → definitely exceeds 3072-byte cap
    // once base64-encoded.
    const big = Array.from({ length: 20 }, (_, i) => ({
      source: `source-${i}.example`,
      uids: [{ id: 'x'.repeat(200) + String(i), atype: 3 }],
    }));
    mockGetUserIdsAsEids.mockReturnValue(big);

    pbjs.requestBids({ adUnits: [] } as any);

    const encoded = getTsEidsCookie();
    expect(encoded).toBeDefined();
    expect(encoded!.length).toBeLessThanOrEqual(3072);

    const decoded = JSON.parse(atob(encoded!));
    // At least one entry kept, strictly fewer than original count.
    expect(decoded.length).toBeGreaterThan(0);
    expect(decoded.length).toBeLessThan(big.length);
    // Head of the list is preserved (trimming happens from the tail).
    expect(decoded[0].source).toBe('source-0.example');
  });

  it('writes no cookie when a single entry alone exceeds the cap', () => {
    wireBidsBackHandler();
    const pbjs = installPrebidNpm();

    // Single entry large enough to blow past 3072 bytes after base64.
    mockGetUserIdsAsEids.mockReturnValue([
      { source: 'too-big.example', uids: [{ id: 'x'.repeat(4000), atype: 3 }] },
    ]);

    pbjs.requestBids({ adUnits: [] } as any);

    expect(getTsEidsCookie()).toBeUndefined();
  });

  it('does not throw when getUserIdsAsEids is undefined (pre-fix production state)', () => {
    wireBidsBackHandler();
    const pbjs = installPrebidNpm();
    // Simulate a build that forgot the userId core module.
    (mockPbjs as any).getUserIdsAsEids = undefined;

    expect(() => pbjs.requestBids({ adUnits: [] } as any)).not.toThrow();
    expect(getTsEidsCookie()).toBeUndefined();

    // Restore for subsequent tests.
    (mockPbjs as any).getUserIdsAsEids = mockGetUserIdsAsEids;
  });

  it('calls the original bidsBackHandler after syncing EIDs', () => {
    wireBidsBackHandler();
    const pbjs = installPrebidNpm();
    const originalHandler = vi.fn();

    pbjs.requestBids({ adUnits: [], bidsBackHandler: originalHandler } as any);

    expect(originalHandler).toHaveBeenCalledTimes(1);
  });
});

describe('prebid User ID Module imports (regression guard)', () => {
  // `userId.js` is the core module — bundled unconditionally via a static
  // import in index.ts, never operator-configurable. Guard it there.
  const INDEX_PATH = resolve(process.cwd(), 'src/integrations/prebid/index.ts');
  const indexSource = readFileSync(INDEX_PATH, 'utf8');

  it('index.ts statically imports the User ID core module', () => {
    expect(indexSource).toMatch(/import\s+['"]prebid\.js\/modules\/userId\.js['"]/);
  });

  it('index.ts statically imports the generated User ID submodule file', () => {
    expect(indexSource).toMatch(/import\s+['"]\.\/_user_ids\.generated['"]/);
  });

  // The submodule list is operator-controlled via TSJS_PREBID_USER_IDS, but
  // the default ship-set must keep resolving without env var action. Read
  // the generated file produced by `node build-all.mjs` with no env override
  // and assert every default submodule is imported. If this file is missing,
  // the developer has not yet run the build — skip with a clear message.
  const GENERATED_PATH = resolve(process.cwd(), 'src/integrations/prebid/_user_ids.generated.ts');
  const DEFAULT_SUBMODULES = [
    'sharedIdSystem',
    'criteoIdSystem',
    '33acrossIdSystem',
    'pubProvidedIdSystem',
    'quantcastIdSystem',
    'id5IdSystem',
    'identityLinkIdSystem',
    'uid2IdSystem',
    'euidIdSystem',
    'intentIqIdSystem',
    'lotamePanoramaIdSystem',
    'connectIdSystem',
    'merkleIdSystem',
  ];

  for (const name of DEFAULT_SUBMODULES) {
    it(`_user_ids.generated.ts imports ${name}.js by default`, () => {
      const generated = readFileSync(GENERATED_PATH, 'utf8');
      const pattern = new RegExp(
        `import\\s+['"]prebid\\.js/modules/${name.replace(/\./g, '\\.')}\\.js['"]`
      );
      expect(generated).toMatch(pattern);
    });
  }
});
