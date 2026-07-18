import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { buildAdRequest, parseAuctionResponse, sendAuction } from '../../src/core/auction';
import envelope from '../fixtures/aps-renderer-v1.json';

function apsRenderer(creativeId?: string) {
  const bid = envelope.seatbid[0].bid[0];
  return {
    type: 'aps' as const,
    version: 1 as const,
    accountId: 'example-account-id',
    bidId: bid.id,
    ...(creativeId ? { creativeId } : {}),
    tagType: 'iframe' as const,
    creativeUrl: bid.ext.creativeurl,
    aaxResponse: btoa(JSON.stringify(envelope)),
    width: bid.w,
    height: bid.h,
  };
}

describe('auction/buildAdRequest', () => {
  it('builds from tsjs AdUnit objects', () => {
    const units = [
      {
        code: 'div-1',
        mediaTypes: {
          banner: {
            sizes: [
              [300, 250],
              [728, 90],
            ],
          },
        },
        bids: [
          { bidder: 'appnexus', params: { placementId: 123 } },
          { bidder: 'rubicon', params: {} },
        ],
      },
    ];

    const result = buildAdRequest(units);

    expect(result.adUnits).toHaveLength(1);
    expect(result.adUnits[0].code).toBe('div-1');
    expect(result.adUnits[0].mediaTypes.banner?.sizes).toEqual([
      [300, 250],
      [728, 90],
    ]);
    expect(result.adUnits[0].bids).toHaveLength(2);
    expect(result.adUnits[0].bids[0]).toEqual({ bidder: 'appnexus', params: { placementId: 123 } });
    expect(result.adUnits[0].bids[1]).toEqual({ bidder: 'rubicon', params: {} });
  });

  it('builds from Prebid BidRequest objects (adUnitCode + bidder)', () => {
    const bidRequests = [
      {
        adUnitCode: 'div-gpt-1',
        bidder: 'appnexus',
        params: { placementId: 456 },
        mediaTypes: { banner: { sizes: [[300, 250]] } },
      },
      {
        adUnitCode: 'div-gpt-1',
        bidder: 'rubicon',
        params: { siteId: 789 },
        mediaTypes: { banner: { sizes: [[300, 250]] } },
      },
      {
        adUnitCode: 'div-gpt-2',
        bidder: 'openx',
        params: {},
        mediaTypes: { banner: { sizes: [[728, 90]] } },
      },
    ];

    const result = buildAdRequest(bidRequests);

    expect(result.adUnits).toHaveLength(2);

    const unit1 = result.adUnits.find((u) => u.code === 'div-gpt-1');
    expect(unit1).toBeDefined();
    expect(unit1!.bids).toHaveLength(2);
    expect(unit1!.bids[0].bidder).toBe('appnexus');
    expect(unit1!.bids[1].bidder).toBe('rubicon');

    const unit2 = result.adUnits.find((u) => u.code === 'div-gpt-2');
    expect(unit2).toBeDefined();
    expect(unit2!.bids).toHaveLength(1);
    expect(unit2!.bids[0].bidder).toBe('openx');
  });

  it('handles empty units array', () => {
    const result = buildAdRequest([]);
    expect(result.adUnits).toEqual([]);
  });

  it('includes auction-level eids when provided', () => {
    const result = buildAdRequest(
      [
        {
          code: 'div-1',
          mediaTypes: { banner: { sizes: [[300, 250]] } },
          bids: [{ bidder: 'appnexus', params: {} }],
        },
      ],
      {
        eids: [
          {
            source: 'adserver.org',
            uids: [
              {
                id: 'uid-123',
                atype: 1,
                ext: { provider: 'liveintent.com', rtiPartner: 'TDID' },
              },
            ],
          },
        ],
      }
    );

    expect(result.eids).toEqual([
      {
        source: 'adserver.org',
        uids: [
          {
            id: 'uid-123',
            atype: 1,
            ext: { provider: 'liveintent.com', rtiPartner: 'TDID' },
          },
        ],
      },
    ]);
  });

  it('handles units without mediaTypes', () => {
    const units = [{ code: 'div-1', bids: [{ bidder: 'appnexus' }] }];
    const result = buildAdRequest(units);

    expect(result.adUnits).toHaveLength(1);
    expect(result.adUnits[0].mediaTypes).toEqual({});
  });

  it('deduplicates by code/adUnitCode', () => {
    const units = [
      { code: 'slot-1', mediaTypes: { banner: { sizes: [[300, 250]] } }, bids: [{ bidder: 'a' }] },
      { code: 'slot-1', mediaTypes: { banner: { sizes: [[300, 250]] } }, bids: [{ bidder: 'b' }] },
    ];

    const result = buildAdRequest(units);
    expect(result.adUnits).toHaveLength(1);
    expect(result.adUnits[0].bids).toHaveLength(2);
    expect(result.adUnits[0].bids[0].bidder).toBe('a');
    expect(result.adUnits[0].bids[1].bidder).toBe('b');
  });
});

describe('auction/parseAuctionResponse', () => {
  it('parses a standard OpenRTB seatbid response', () => {
    const body = {
      seatbid: [
        {
          seat: 'appnexus',
          bid: [
            {
              impid: 'div-1',
              price: 3.5,
              adm: '<div>Creative</div>',
              w: 300,
              h: 250,
              crid: 'cr-123',
              adomain: ['example.com'],
            },
          ],
        },
      ],
    };

    const bids = parseAuctionResponse(body);

    expect(bids).toHaveLength(1);
    expect(bids[0]).toEqual({
      impid: 'div-1',
      adm: '<div>Creative</div>',
      price: 3.5,
      width: 300,
      height: 250,
      seat: 'appnexus',
      creativeId: 'cr-123',
      adomain: ['example.com'],
    });
  });

  it('parses an APS typed renderer without requiring adm', () => {
    const renderer = apsRenderer('fictional-creative-id');
    const bids = parseAuctionResponse({
      seatbid: [
        {
          seat: 'aps',
          bid: [
            {
              id: renderer.bidId,
              impid: 'fictional-slot',
              price: 1.23,
              crid: renderer.creativeId,
              w: 300,
              h: 250,
              ext: { trusted_server: { renderer } },
            },
          ],
        },
      ],
    });

    expect(bids).toHaveLength(1);
    expect(bids[0]).toEqual(
      expect.objectContaining({
        impid: 'fictional-slot',
        adm: '',
        renderer,
        width: 300,
        height: 250,
        creativeId: 'fictional-creative-id',
      })
    );
  });

  it('parses an APS renderer with optional creativeId omitted', () => {
    const renderer = apsRenderer();
    const bids = parseAuctionResponse({
      seatbid: [
        {
          seat: 'aps',
          bid: [
            {
              impid: 'fictional-slot',
              price: 1.23,
              w: 300,
              h: 250,
              ext: { trusted_server: { renderer } },
            },
          ],
        },
      ],
    });

    expect(bids[0].renderer).toEqual(renderer);
    expect(bids[0].creativeId).toBe('aps-fictional-slot');
  });

  it('ignores unrelated or malformed renderer extensions while retaining ordinary adm', () => {
    const bids = parseAuctionResponse({
      seatbid: [
        {
          seat: 'ordinary',
          bid: [
            {
              impid: 'slot-1',
              adm: '<div>ordinary</div>',
              ext: { trusted_server: { renderer: { type: 'aps', version: 99 } } },
            },
          ],
        },
      ],
    });

    expect(bids[0].renderer).toBeUndefined();
    expect(bids[0].adm).toBe('<div>ordinary</div>');
  });

  it('handles multiple seatbids with multiple bids', () => {
    const body = {
      seatbid: [
        {
          seat: 'bidderA',
          bid: [
            { impid: 'slot-1', price: 1.0, adm: '<div>A1</div>', w: 300, h: 250, crid: 'a1' },
            { impid: 'slot-2', price: 2.0, adm: '<div>A2</div>', w: 728, h: 90, crid: 'a2' },
          ],
        },
        {
          seat: 'bidderB',
          bid: [{ impid: 'slot-1', price: 3.0, adm: '<div>B1</div>', w: 300, h: 250, crid: 'b1' }],
        },
      ],
    };

    const bids = parseAuctionResponse(body);
    expect(bids).toHaveLength(3);
  });

  it('returns empty array for null/undefined body', () => {
    expect(parseAuctionResponse(null)).toEqual([]);
    expect(parseAuctionResponse(undefined)).toEqual([]);
    expect(parseAuctionResponse({})).toEqual([]);
  });

  it('returns empty array for empty seatbid', () => {
    expect(parseAuctionResponse({ seatbid: [] })).toEqual([]);
  });

  it('defaults missing fields gracefully', () => {
    const body = {
      seatbid: [{ bid: [{ impid: 'slot-1', price: 1.5 }] }],
    };

    const bids = parseAuctionResponse(body);
    expect(bids).toHaveLength(1);
    expect(bids[0].seat).toBe('unknown');
    expect(bids[0].adm).toBe('');
    expect(bids[0].width).toBe(300);
    expect(bids[0].height).toBe(250);
    expect(bids[0].adomain).toEqual([]);
  });

  it('retains the auction id from the response top-level id', () => {
    const body = {
      id: 'auction-uuid-1',
      seatbid: [{ seat: 'kargo', bid: [{ impid: 'slot-1', price: 1.0, adm: '<div>A</div>' }] }],
    };

    const bids = parseAuctionResponse(body);
    expect(bids[0].auctionId).toBe('auction-uuid-1');
    expect(bids[0].admHash).toBeUndefined();
  });

  it('prefers bid-level ext.ts trace fields over the top-level id', () => {
    const body = {
      id: 'auction-uuid-1',
      seatbid: [
        {
          seat: 'kargo',
          bid: [
            {
              impid: 'slot-1',
              price: 1.0,
              adm: '<div>A</div>',
              ext: { ts: { auction_id: 'auction-uuid-2', adm_hash: 'a1b2c3d4e5f60718' } },
            },
          ],
        },
      ],
    };

    const bids = parseAuctionResponse(body);
    expect(bids[0].auctionId).toBe('auction-uuid-2');
    expect(bids[0].admHash).toBe('a1b2c3d4e5f60718');
  });
});

describe('auction/sendAuction', () => {
  let originalFetch: typeof globalThis.fetch;

  beforeEach(() => {
    originalFetch = globalThis.fetch;
  });

  afterEach(() => {
    globalThis.fetch = originalFetch;
  });

  it('POSTs AdRequest and returns parsed bids', async () => {
    const mockResponse = {
      ok: true,
      status: 200,
      headers: { get: () => 'application/json' },
      json: async () => ({
        seatbid: [
          {
            seat: 'appnexus',
            bid: [
              { impid: 'slot-1', price: 2.5, adm: '<div>Ad</div>', w: 300, h: 250, crid: 'c1' },
            ],
          },
        ],
      }),
    };
    globalThis.fetch = vi.fn().mockResolvedValue(mockResponse) as any;

    const request = {
      adUnits: [
        {
          code: 'slot-1',
          mediaTypes: { banner: { sizes: [[300, 250]] } },
          bids: [{ bidder: 'appnexus', params: {} }],
        },
      ],
    };

    const bids = await sendAuction('/auction', request);

    expect(globalThis.fetch).toHaveBeenCalledWith(
      '/auction',
      expect.objectContaining({
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(request),
      })
    );
    expect(bids).toHaveLength(1);
    expect(bids[0].price).toBe(2.5);
  });

  it('returns empty array on network error', async () => {
    globalThis.fetch = vi.fn().mockRejectedValue(new Error('network error')) as any;

    const bids = await sendAuction('/auction', { adUnits: [] });
    expect(bids).toEqual([]);
  });

  it('returns empty array for non-JSON response', async () => {
    globalThis.fetch = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      headers: { get: () => 'text/html' },
      json: async () => ({}),
    }) as any;

    const bids = await sendAuction('/auction', { adUnits: [] });
    expect(bids).toEqual([]);
  });

  it('returns empty array for non-OK response', async () => {
    globalThis.fetch = vi.fn().mockResolvedValue({
      ok: false,
      status: 500,
      headers: { get: () => 'application/json' },
      json: async () => ({}),
    }) as any;

    const bids = await sendAuction('/auction', { adUnits: [] });
    expect(bids).toEqual([]);
  });
});
