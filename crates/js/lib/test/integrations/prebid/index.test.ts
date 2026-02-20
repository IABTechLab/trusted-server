import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';

// Define mocks using vi.hoisted so they're available inside vi.mock factories
const { mockSetConfig, mockProcessQueue, mockRequestBids, mockRegisterBidAdapter, mockPbjs } =
  vi.hoisted(() => {
    const mockSetConfig = vi.fn();
    const mockProcessQueue = vi.fn();
    const mockRequestBids = vi.fn();
    const mockRegisterBidAdapter = vi.fn();
    const mockPbjs = {
      setConfig: mockSetConfig,
      processQueue: mockProcessQueue,
      requestBids: mockRequestBids,
      registerBidAdapter: mockRegisterBidAdapter,
      adUnits: [] as any[],
    };
    return {
      mockSetConfig,
      mockProcessQueue,
      mockRequestBids,
      mockRegisterBidAdapter,
      mockPbjs,
    };
  });

// Mock prebid.js before importing the module under test.
// The real prebid.js cannot run in jsdom, so we provide a minimal stub.
vi.mock('prebid.js', () => ({ default: mockPbjs }));

// Side-effect imports are no-ops in tests
vi.mock('prebid.js/modules/consentManagementTcf.js', () => ({}));
vi.mock('prebid.js/modules/consentManagementGpp.js', () => ({}));
vi.mock('prebid.js/modules/consentManagementUsp.js', () => ({}));

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
    });

    it('adds bids array to ad units that have none', () => {
      const pbjs = installPrebidNpm();

      const adUnits = [{ code: 'div-1' }] as any[];
      pbjs.requestBids({ adUnits } as any);

      expect(adUnits[0].bids).toHaveLength(1);
      expect(adUnits[0].bids[0].bidder).toBe('trustedServer');
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
  });
});

describe('prebid/installPrebidNpm with server-injected config', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockPbjs.requestBids = mockRequestBids;
    mockPbjs.adUnits = [];
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
