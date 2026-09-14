import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';

import {
  buildAdRequest,
  MAX_BROWSER_AUCTION_PROJECTION_BYTES,
  parseAuctionResponse,
  parseBrowserAuctionProjectionV1,
  parseTrustedServerAuctionResponseV1,
  sendAuction,
} from '../../src/core/auction';
import envelope from '../fixtures/aps-renderer-v1.json';
import type { BrowserAuctionProjectionV1 } from '../../src/core/types';

function apsRenderer(creativeId?: string) {
  const bid = envelope.seatbid[0]!.bid[0]!;
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

function candidateId(index = 0): string {
  return index.toString(36).padStart(12, 'A');
}

function reservationId(index = 0): string {
  return `r1_${index.toString(36).padStart(22, 'A')}`;
}

function browserSlot(slot: string) {
  return {
    slot,
    gamUnitPath: `/123/${slot}`,
    divId: `div-${slot}`,
    formats: [[300, 250]] as Array<[number, number]>,
    targeting: { pos: slot } as Record<string, string>,
  };
}

function browserProjection() {
  const renderer = apsRenderer('fictional-creative-id');
  return {
    version: 1,
    auction: {
      version: 1,
      auctionId: 'auction-1',
      results: [
        { slot: 'slot-1', outcome: 'winner', candidateId: candidateId() },
        { slot: 'slot-2', outcome: 'no_bid' },
        { slot: 'slot-3', outcome: 'failed', reason: 'provider_timeout' },
      ],
    },
    slots: [browserSlot('slot-1'), browserSlot('slot-2'), browserSlot('slot-3')],
    bids: [
      {
        candidateId: candidateId(),
        slot: 'slot-1',
        provider: 'aps',
        upstreamBidId: renderer.bidId,
        cpm: 1.25,
        currency: 'USD',
        targeting: { hb_bidder: 'aps', hb_pb: '1.25' } as Record<string, string>,
        rendererReservationId: reservationId(),
        renderSource: renderer,
      },
    ],
  };
}

function largeAdmProjection(admLengths: number[]): BrowserAuctionProjectionV1 {
  return {
    version: 1,
    auction: {
      version: 1,
      auctionId: 'auction-large',
      results: admLengths.map((_, index) => ({
        slot: `slot-${index}`,
        outcome: 'winner' as const,
        candidateId: candidateId(index),
      })),
    },
    slots: admLengths.map((_, index) => browserSlot(`slot-${index}`)),
    bids: admLengths.map((length, index) => ({
      candidateId: candidateId(index),
      slot: `slot-${index}`,
      provider: 'prebid',
      upstreamBidId: `upstream-${index}`,
      cpm: index,
      currency: 'USD',
      targeting: {} as Record<string, string>,
      rendererReservationId: reservationId(index),
      renderSource: {
        type: 'adm' as const,
        version: 1 as const,
        adm: 'x'.repeat(length),
        width: 300,
        height: 250,
      },
    })),
  };
}

describe('auction/buildAdRequest', () => {
  it('builds from direct-auction programmatic units', () => {
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
    expect(result.adUnits[0]!.code).toBe('div-1');
    expect(result.adUnits[0]!.mediaTypes.banner?.sizes).toEqual([
      [300, 250],
      [728, 90],
    ]);
    expect(result.adUnits[0]!.bids).toHaveLength(2);
    expect(result.adUnits[0]!.bids[0]).toEqual({
      bidder: 'appnexus',
      params: { placementId: 123 },
    });
    expect(result.adUnits[0]!.bids[1]).toEqual({ bidder: 'rubicon', params: {} });
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
    expect(unit1!.bids[0]!.bidder).toBe('appnexus');
    expect(unit1!.bids[1]!.bidder).toBe('rubicon');

    const unit2 = result.adUnits.find((u) => u.code === 'div-gpt-2');
    expect(unit2).toBeDefined();
    expect(unit2!.bids).toHaveLength(1);
    expect(unit2!.bids[0]!.bidder).toBe('openx');
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
    expect(result.adUnits[0]!.mediaTypes).toEqual({});
  });

  it('deduplicates by code/adUnitCode', () => {
    const units = [
      { code: 'slot-1', mediaTypes: { banner: { sizes: [[300, 250]] } }, bids: [{ bidder: 'a' }] },
      { code: 'slot-1', mediaTypes: { banner: { sizes: [[300, 250]] } }, bids: [{ bidder: 'b' }] },
    ];

    const result = buildAdRequest(units);
    expect(result.adUnits).toHaveLength(1);
    expect(result.adUnits[0]!.bids).toHaveLength(2);
    expect(result.adUnits[0]!.bids[0]!.bidder).toBe('a');
    expect(result.adUnits[0]!.bids[1]!.bidder).toBe('b');
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

    expect(bids[0]!.renderer).toEqual(renderer);
    expect(bids[0]!.creativeId).toBe('aps-fictional-slot');
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

    expect(bids[0]!.renderer).toBeUndefined();
    expect(bids[0]!.adm).toBe('<div>ordinary</div>');
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
    expect(bids[0]!.seat).toBe('unknown');
    expect(bids[0]!.adm).toBe('');
    expect(bids[0]!.width).toBe(300);
    expect(bids[0]!.height).toBe(250);
    expect(bids[0]!.adomain).toEqual([]);
  });
});

describe('auction/parseBrowserAuctionProjectionV1', () => {
  it('accepts one exact ordered decision per slot and deep-copies the projection', () => {
    const input = browserProjection();
    const parsed = parseBrowserAuctionProjectionV1(input);

    expect(parsed).toEqual(input);
    expect(parsed).not.toBe(input);
    expect(parsed!.auction.results.map((result) => result.slot)).toEqual([
      'slot-1',
      'slot-2',
      'slot-3',
    ]);
    expect(Object.keys(parsed!.bids[0]!.targeting)).toEqual(['hb_bidder', 'hb_pb']);
  });

  it('rejects duplicate, missing, extra, and mismatched decision/bid joins', () => {
    const cases: unknown[] = [];

    const duplicateResult = browserProjection();
    duplicateResult.auction.results.push({
      slot: 'slot-1',
      outcome: 'no_bid',
    });
    cases.push(duplicateResult);

    const missingBid = browserProjection();
    missingBid.bids = [];
    cases.push(missingBid);

    const extraBid = browserProjection();
    extraBid.bids.push({ ...extraBid.bids[0]!, candidateId: candidateId(1) });
    cases.push(extraBid);

    const mismatchedSlot = browserProjection();
    mismatchedSlot.bids[0]!.slot = 'slot-other';
    cases.push(mismatchedSlot);

    const duplicateCandidate = browserProjection();
    duplicateCandidate.auction.results.push({
      slot: 'slot-4',
      outcome: 'winner',
      candidateId: candidateId(),
    });
    duplicateCandidate.bids.push({ ...duplicateCandidate.bids[0]!, slot: 'slot-4' });
    cases.push(duplicateCandidate);

    for (const value of cases) {
      expect(parseBrowserAuctionProjectionV1(value)).toBeUndefined();
    }
  });

  it('enforces exact objects, own data properties, and ordinary prototypes', () => {
    const unknownTopLevel = { ...browserProjection(), unknown: true };
    const unknownDecision = browserProjection();
    Object.assign(unknownDecision.auction.results[0]!, { unknown: true });
    const accessor = browserProjection();
    Object.defineProperty(accessor.bids[0]!, 'provider', {
      enumerable: true,
      get: () => 'aps',
    });
    const inherited = browserProjection();
    Object.setPrototypeOf(inherited.bids[0]!, { inherited: true });

    for (const value of [unknownTopLevel, unknownDecision, accessor, inherited]) {
      expect(parseBrowserAuctionProjectionV1(value)).toBeUndefined();
    }
  });

  it('requires exact GAM slot definitions in the canonical projection', () => {
    const missingSlots = browserProjection() as Record<string, unknown>;
    delete missingSlots['slots'];
    expect(parseBrowserAuctionProjectionV1(missingSlots)).toBeUndefined();

    const emptySlots = browserProjection();
    emptySlots.slots = [];
    expect(parseBrowserAuctionProjectionV1(emptySlots)).toBeUndefined();

    const valid = browserProjection();
    expect(parseBrowserAuctionProjectionV1(valid)?.slots).toEqual(valid.slots);
  });

  it('enforces result and bid count boundaries', () => {
    expect(
      parseBrowserAuctionProjectionV1({
        version: 1,
        auction: { version: 1, auctionId: 'auction-empty', results: [] },
        slots: [],
        bids: [],
      })
    ).toBeDefined();

    const atLimit = browserProjection();
    atLimit.auction.results = [];
    atLimit.slots = [];
    atLimit.bids = [];
    for (let index = 0; index < 256; index += 1) {
      const slot = `slot-${index}`;
      const id = candidateId(index);
      atLimit.auction.results.push({ slot, outcome: 'winner', candidateId: id });
      atLimit.slots.push(browserSlot(slot));
      atLimit.bids.push({
        ...browserProjection().bids[0]!,
        slot,
        candidateId: id,
        upstreamBidId: `upstream-${index}`,
        rendererReservationId: reservationId(index),
      });
    }
    expect(parseBrowserAuctionProjectionV1(atLimit)).toBeDefined();

    const tooManyResults = structuredClone(atLimit);
    tooManyResults.auction.results.push({ slot: 'overflow', outcome: 'no_bid' });
    expect(parseBrowserAuctionProjectionV1(tooManyResults)).toBeUndefined();

    const tooManyBids = structuredClone(atLimit);
    tooManyBids.bids.push({
      ...tooManyBids.bids[0]!,
      slot: 'overflow',
      candidateId: candidateId(300),
      rendererReservationId: reservationId(300),
    });
    expect(parseBrowserAuctionProjectionV1(tooManyBids)).toBeUndefined();
  });

  it('enforces identity, price, currency, and targeting boundaries', () => {
    const valid = browserProjection();
    valid.auction.auctionId = 'A'.repeat(128);
    valid.auction.results[0]!.slot = 'é'.repeat(128);
    valid.slots[0]!.slot = 'é'.repeat(128);
    valid.bids[0]!.slot = 'é'.repeat(128);
    valid.bids[0]!.upstreamBidId = 'é'.repeat(32);
    valid.bids[0]!.targeting = Object.fromEntries(
      Array.from({ length: 32 }, (_, index) => [
        `key_${String(index).padStart(2, '0')}`,
        index === 0 ? '😀'.repeat(40) : 'v',
      ])
    );
    expect(parseBrowserAuctionProjectionV1(valid)).toBeDefined();

    const mutations: Array<(value: ReturnType<typeof browserProjection>) => void> = [
      (value) => {
        value.auction.auctionId = 'A'.repeat(129);
      },
      (value) => {
        value.auction.auctionId = 'contains space';
      },
      (value) => {
        value.auction.results[0]!.candidateId = 'short';
      },
      (value) => {
        value.auction.results[0]!.slot = `bad\u0000slot`;
      },
      (value) => {
        value.bids[0]!.provider = '-aps';
      },
      (value) => {
        value.bids[0]!.upstreamBidId = 'é'.repeat(33);
      },
      (value) => {
        value.bids[0]!.cpm = Number.POSITIVE_INFINITY;
      },
      (value) => {
        value.bids[0]!.cpm = -0.01;
      },
      (value) => {
        value.bids[0]!.currency = 'EUR';
      },
      (value) => {
        value.bids[0]!.rendererReservationId = 'r1_short';
      },
      (value) => {
        value.bids[0]!.targeting = { hb_adid: reservationId() };
      },
      (value) => {
        value.bids[0]!.targeting = { ['k'.repeat(21)]: 'v' };
      },
      (value) => {
        value.bids[0]!.targeting = { key: '😀'.repeat(41) };
      },
      (value) => {
        value.bids[0]!.targeting = { key: 'é'.repeat(81) };
      },
      (value) => {
        value.bids[0]!.targeting = { key: 'bad\u0001value' };
      },
      (value) => {
        value.bids[0]!.targeting = { key: String.fromCharCode(0xd800) };
      },
    ];

    for (const mutate of mutations) {
      const value = browserProjection();
      mutate(value);
      expect(parseBrowserAuctionProjectionV1(value)).toBeUndefined();
    }
  });

  it('enforces exact targeting entry, key, scalar, and UTF-8 byte boundaries', () => {
    for (const count of [31, 32]) {
      const value = browserProjection();
      value.bids[0]!.targeting = Object.fromEntries(
        Array.from({ length: count }, (_, index) => [`k_${index}`, 'v'])
      );
      expect(parseBrowserAuctionProjectionV1(value)).toBeDefined();
    }
    const tooManyEntries = browserProjection();
    tooManyEntries.bids[0]!.targeting = Object.fromEntries(
      Array.from({ length: 33 }, (_, index) => [`k_${index}`, 'v'])
    );
    expect(parseBrowserAuctionProjectionV1(tooManyEntries)).toBeUndefined();

    for (const length of [19, 20]) {
      const value = browserProjection();
      value.bids[0]!.targeting = { ['k'.repeat(length)]: 'v' };
      expect(parseBrowserAuctionProjectionV1(value)).toBeDefined();
    }
    const keyTooLong = browserProjection();
    keyTooLong.bids[0]!.targeting = { ['k'.repeat(21)]: 'v' };
    expect(parseBrowserAuctionProjectionV1(keyTooLong)).toBeUndefined();

    for (const scalars of [39, 40]) {
      const value = browserProjection();
      value.bids[0]!.targeting = { key: 'a'.repeat(scalars) };
      expect(parseBrowserAuctionProjectionV1(value)).toBeDefined();
    }
    const tooManyScalars = browserProjection();
    tooManyScalars.bids[0]!.targeting = { key: 'a'.repeat(41) };
    expect(parseBrowserAuctionProjectionV1(tooManyScalars)).toBeUndefined();

    for (const valueText of ['😀'.repeat(39) + '€', '😀'.repeat(40)]) {
      const value = browserProjection();
      value.bids[0]!.targeting = { key: valueText };
      expect(parseBrowserAuctionProjectionV1(value)).toBeDefined();
    }
    const tooManyBytes = browserProjection();
    tooManyBytes.bids[0]!.targeting = { key: '😀'.repeat(40) + 'a' };
    expect(parseBrowserAuctionProjectionV1(tooManyBytes)).toBeUndefined();
  });

  it('deep-copies every admitted targeting key as own data, including __proto__', () => {
    const value = browserProjection();
    value.bids[0]!.targeting = JSON.parse('{"__proto__":"publisher-value"}') as Record<
      string,
      string
    >;

    const parsed = parseBrowserAuctionProjectionV1(value);

    expect(parsed).toBeDefined();
    expect(Object.getPrototypeOf(parsed!.bids[0]!.targeting)).toBe(Object.prototype);
    expect(Object.prototype.hasOwnProperty.call(parsed!.bids[0]!.targeting, '__proto__')).toBe(
      true
    );
    expect(parsed!.bids[0]!.targeting['__proto__']).toBe('publisher-value');
  });

  it('enforces canonical UTF-8 JSON just below, at, and above 8 MiB', () => {
    const lengths = Array.from({ length: 16 }, () => 512 * 1024);
    lengths[15] = 1;
    const baseline = largeAdmProjection(lengths);
    const baselineBytes = new TextEncoder().encode(JSON.stringify(baseline)).length;
    const exactTail = 1 + MAX_BROWSER_AUCTION_PROJECTION_BYTES - baselineBytes;
    expect(exactTail).toBeLessThanOrEqual(512 * 1024);

    for (const [delta, accepted] of [
      [-1, true],
      [0, true],
      [1, false],
    ] as const) {
      lengths[15] = exactTail + delta;
      const value = largeAdmProjection(lengths);
      expect(new TextEncoder().encode(JSON.stringify(value)).length).toBe(
        MAX_BROWSER_AUCTION_PROJECTION_BYTES + delta
      );
      expect(parseBrowserAuctionProjectionV1(value) !== undefined).toBe(accepted);
    }
  });

  it('uses captured validation intrinsics after platform prototypes are poisoned', () => {
    const valid = largeAdmProjection([16]);
    const invalid = { ...largeAdmProjection([16]), unknown: true };
    const mismatchedWinner = largeAdmProjection([16]);
    mismatchedWinner.auction.results[0]!.slot = 'mismatched-slot';
    const iteratorDescriptor = Object.getOwnPropertyDescriptor(Array.prototype, Symbol.iterator);
    const everyDescriptor = Object.getOwnPropertyDescriptor(Array.prototype, 'every');
    const includesDescriptor = Object.getOwnPropertyDescriptor(Array.prototype, 'includes');
    const someDescriptor = Object.getOwnPropertyDescriptor(Array.prototype, 'some');
    const encodeDescriptor = Object.getOwnPropertyDescriptor(TextEncoder.prototype, 'encode');
    const testDescriptor = Object.getOwnPropertyDescriptor(RegExp.prototype, 'test');
    const calls = { encode: 0, every: 0, includes: 0, iterator: 0, some: 0, test: 0 };
    let parsed: BrowserAuctionProjectionV1 | undefined;
    let rejected: BrowserAuctionProjectionV1 | undefined;
    let rejectedMismatch: BrowserAuctionProjectionV1 | undefined;
    Object.defineProperty(Array.prototype, Symbol.iterator, {
      configurable: true,
      value: () => {
        calls.iterator += 1;
        throw new Error('poisoned array iterator');
      },
    });
    Object.defineProperty(TextEncoder.prototype, 'encode', {
      configurable: true,
      value: () => {
        calls.encode += 1;
        throw new Error('poisoned text encoder');
      },
    });
    Object.defineProperty(RegExp.prototype, 'test', {
      configurable: true,
      value: () => {
        calls.test += 1;
        throw new Error('poisoned regular expression');
      },
    });
    Object.defineProperty(Array.prototype, 'every', {
      configurable: true,
      value: () => {
        calls.every += 1;
        throw new Error('poisoned array every');
      },
    });
    Object.defineProperty(Array.prototype, 'includes', {
      configurable: true,
      value: () => {
        calls.includes += 1;
        throw new Error('poisoned array includes');
      },
    });
    Object.defineProperty(Array.prototype, 'some', {
      configurable: true,
      value: () => {
        calls.some += 1;
        throw new Error('poisoned array some');
      },
    });
    try {
      parsed = parseBrowserAuctionProjectionV1(valid);
      rejected = parseBrowserAuctionProjectionV1(invalid);
      rejectedMismatch = parseBrowserAuctionProjectionV1(mismatchedWinner);
    } finally {
      if (iteratorDescriptor) {
        Object.defineProperty(Array.prototype, Symbol.iterator, iteratorDescriptor);
      }
      if (encodeDescriptor)
        Object.defineProperty(TextEncoder.prototype, 'encode', encodeDescriptor);
      if (testDescriptor) Object.defineProperty(RegExp.prototype, 'test', testDescriptor);
      if (everyDescriptor) Object.defineProperty(Array.prototype, 'every', everyDescriptor);
      if (includesDescriptor)
        Object.defineProperty(Array.prototype, 'includes', includesDescriptor);
      if (someDescriptor) Object.defineProperty(Array.prototype, 'some', someDescriptor);
    }

    expect(parsed).toBeDefined();
    expect(rejected).toBeUndefined();
    expect(rejectedMismatch).toBeUndefined();
    expect(calls).toEqual({ encode: 0, every: 0, includes: 0, iterator: 0, some: 0, test: 0 });
  });

  it('accepts only the exact opaque pbs_cache carrier without a reservation identity', () => {
    const cacheId = 'f47447a0-b759-4f2f-9887-af458b79b570';
    const cacheProjection = () => {
      const value = structuredClone(browserProjection());
      const bid = value.bids[0]!;
      delete (bid as { rendererReservationId?: string }).rendererReservationId;
      (bid as { renderSource: unknown }).renderSource = {
        type: 'pbs_cache',
        version: 1,
        cacheId,
        cacheHost: 'cache.example:8443',
        cachePath: '/pbc/v1/cache',
        width: 0,
        height: 0xffff_ffff,
      };
      return value;
    };

    const parsed = parseBrowserAuctionProjectionV1(cacheProjection());
    expect(parsed?.bids[0]).toEqual(cacheProjection().bids[0]);
    expect(parsed?.bids[0]).not.toHaveProperty('rendererReservationId');

    const withReservation = cacheProjection();
    Object.assign(withReservation.bids[0]!, { rendererReservationId: reservationId() });
    expect(parseBrowserAuctionProjectionV1(withReservation)).toBeUndefined();

    const withExtraSourceField = cacheProjection();
    Object.assign(withExtraSourceField.bids[0]!.renderSource, { fetchUrl: 'https://forbidden' });
    expect(parseBrowserAuctionProjectionV1(withExtraSourceField)).toBeUndefined();
  });
});

describe('auction/parseTrustedServerAuctionResponseV1', () => {
  interface MutableWireBid {
    id: string;
    impid: string;
    price: number;
    w: number;
    h: number;
    adm?: string;
    ext: {
      trusted_server: {
        candidate_id: string;
        slot_id: string;
        render_source: unknown;
        extra?: boolean;
      };
    };
  }

  function response(): {
    id: string;
    cur: string;
    seatbid: Array<{ seat: string; bid: MutableWireBid[] }>;
    ext: { trusted_server: { slot_results: unknown } };
  } {
    const projection = browserProjection();
    const winner = projection.bids[0]!;
    return {
      id: projection.auction.auctionId,
      cur: 'USD',
      seatbid: [
        {
          seat: winner.provider,
          bid: [
            {
              id: winner.rendererReservationId,
              impid: winner.slot,
              price: winner.cpm,
              w: winner.renderSource.width,
              h: winner.renderSource.height,
              ext: {
                trusted_server: {
                  candidate_id: winner.candidateId,
                  slot_id: winner.slot,
                  render_source: winner.renderSource,
                },
              },
            },
          ],
        },
      ],
      ext: { trusted_server: { slot_results: projection.auction } },
    };
  }

  function admResponse(admLengths: number[]) {
    const projected = largeAdmProjection(admLengths);
    const canonical: BrowserAuctionProjectionV1 = {
      version: 1,
      auction: projected.auction,
      slots: [],
      bids: projected.bids.map((bid) => {
        if (!('rendererReservationId' in bid)) throw new Error('expected owned ADM bid');
        return { ...bid, upstreamBidId: bid.rendererReservationId };
      }),
    };
    return {
      canonical,
      wire: {
        id: canonical.auction.auctionId,
        cur: 'USD',
        seatbid: [
          {
            seat: 'prebid',
            bid: canonical.bids.map((bid) => {
              if (bid.renderSource.type !== 'adm' || !('rendererReservationId' in bid)) {
                throw new Error('expected owned ADM source');
              }
              return {
                id: bid.rendererReservationId,
                impid: bid.slot,
                price: bid.cpm,
                adm: bid.renderSource.adm,
                w: bid.renderSource.width,
                h: bid.renderSource.height,
                ext: {
                  trusted_server: {
                    candidate_id: bid.candidateId,
                    slot_id: bid.slot,
                    render_source: bid.renderSource,
                  },
                },
              };
            }),
          },
        ],
        ext: { trusted_server: { slot_results: canonical.auction } },
      },
    };
  }

  it('accepts the exact four-way decision/candidate/impid/slot join', () => {
    const parsed = parseTrustedServerAuctionResponseV1(response());

    expect(parsed?.auction.results).toEqual(browserProjection().auction.results);
    expect(parsed?.bids[0]).toEqual(
      expect.objectContaining({
        candidateId: candidateId(),
        rendererReservationId: reservationId(),
        impid: 'slot-1',
        renderSource: apsRenderer('fictional-creative-id'),
      })
    );
  });

  it('caps the deduplicated canonical projection instead of duplicated ADM wire bytes', () => {
    const lengths = Array.from({ length: 16 }, () => 512 * 1024);
    lengths[15] = 1;
    const baseline = admResponse(lengths).canonical;
    const baselineBytes = new TextEncoder().encode(JSON.stringify(baseline)).byteLength;
    const exactTail = 1 + MAX_BROWSER_AUCTION_PROJECTION_BYTES - baselineBytes;
    expect(exactTail).toBeLessThanOrEqual(512 * 1024);

    for (const [delta, accepted] of [
      [0, true],
      [1, false],
    ] as const) {
      lengths[15] = exactTail + delta;
      const { canonical, wire } = admResponse(lengths);
      expect(new TextEncoder().encode(JSON.stringify(canonical)).byteLength).toBe(
        MAX_BROWSER_AUCTION_PROJECTION_BYTES + delta
      );
      expect(new TextEncoder().encode(JSON.stringify(wire)).byteLength).toBeGreaterThan(
        MAX_BROWSER_AUCTION_PROJECTION_BYTES
      );
      expect(
        new TextEncoder().encode(JSON.stringify(canonical)).length <=
          MAX_BROWSER_AUCTION_PROJECTION_BYTES
      ).toBe(accepted);
      expect(parseTrustedServerAuctionResponseV1(wire) !== undefined).toBe(accepted);
    }
  });

  it.each([
    ['Object', Object.prototype],
    ['Array', Array.prototype],
  ] as const)(
    'measures projection and response own data without inherited %s.prototype.toJSON',
    (_name, prototype) => {
      const lengths = Array.from({ length: 16 }, () => 512 * 1024);
      lengths[15] = 1;
      const baselineBytes = new TextEncoder().encode(
        JSON.stringify(largeAdmProjection(lengths))
      ).length;
      lengths[15] = 2 + MAX_BROWSER_AUCTION_PROJECTION_BYTES - baselineBytes;
      const oversizedProjection = largeAdmProjection(lengths);
      expect(new TextEncoder().encode(JSON.stringify(oversizedProjection)).length).toBe(
        MAX_BROWSER_AUCTION_PROJECTION_BYTES + 1
      );
      const acceptedResponse = response();

      const descriptor = Object.getOwnPropertyDescriptor(prototype, 'toJSON');
      const inheritedToJson = vi.fn(() => ({}));
      try {
        Object.defineProperty(prototype, 'toJSON', {
          configurable: true,
          value: inheritedToJson,
          writable: true,
        });

        expect(parseBrowserAuctionProjectionV1(oversizedProjection)).toBeUndefined();
        expect(inheritedToJson).not.toHaveBeenCalled();
        expect(parseTrustedServerAuctionResponseV1(acceptedResponse)).toBeDefined();
        expect(inheritedToJson).not.toHaveBeenCalled();
      } finally {
        if (descriptor) Object.defineProperty(prototype, 'toJSON', descriptor);
        else Reflect.deleteProperty(prototype, 'toJSON');
      }
    }
  );

  it('returns direct winners in decision order regardless of response order', () => {
    const value = response();
    const first = value.seatbid[0]!.bid[0]!;
    const second = structuredClone(first);
    second.id = reservationId(1);
    second.impid = 'slot-2';
    second.ext.trusted_server.candidate_id = candidateId(1);
    second.ext.trusted_server.slot_id = 'slot-2';
    value.seatbid[0]!.bid = [second, first];
    const decisions = value.ext.trusted_server.slot_results as ReturnType<
      typeof browserProjection
    >['auction'];
    decisions.results[1] = {
      slot: 'slot-2',
      outcome: 'winner',
      candidateId: candidateId(1),
    };

    expect(parseTrustedServerAuctionResponseV1(value)?.bids.map((bid) => bid.candidateId)).toEqual([
      candidateId(),
      candidateId(1),
    ]);
  });

  it('rejects missing, duplicate, extra, and mismatched joins transactionally', () => {
    const missing = response();
    missing.seatbid = [];
    const duplicate = response();
    duplicate.seatbid[0]!.bid.push(structuredClone(duplicate.seatbid[0]!.bid[0]!));
    const mismatchedImpid = response();
    mismatchedImpid.seatbid[0]!.bid[0]!.impid = 'slot-other';
    const mismatchedSlot = response();
    mismatchedSlot.seatbid[0]!.bid[0]!.ext.trusted_server.slot_id = 'slot-other';
    const unknownTrustedKey = response();
    Object.assign(unknownTrustedKey.seatbid[0]!.bid[0]!.ext.trusted_server, { extra: true });

    for (const value of [missing, duplicate, mismatchedImpid, mismatchedSlot, unknownTrustedKey]) {
      expect(parseTrustedServerAuctionResponseV1(value)).toBeUndefined();
    }
  });

  it('rejects non-USD currency, duplicate reservations, and unknown outer wire keys', () => {
    const nonUsd = response();
    nonUsd.cur = 'EUR';

    const duplicateReservation = response();
    const second = structuredClone(duplicateReservation.seatbid[0]!.bid[0]!);
    second.impid = 'slot-2';
    second.ext.trusted_server.slot_id = 'slot-2';
    second.ext.trusted_server.candidate_id = candidateId(1);
    duplicateReservation.seatbid[0]!.bid.push(second);
    const decisions = duplicateReservation.ext.trusted_server.slot_results as ReturnType<
      typeof browserProjection
    >['auction'];
    decisions.results[1] = {
      slot: 'slot-2',
      outcome: 'winner',
      candidateId: candidateId(1),
    };

    const unknownBody = response() as ReturnType<typeof response> & { unknown?: boolean };
    unknownBody.unknown = true;
    const unknownBid = response();
    Object.assign(unknownBid.seatbid[0]!.bid[0]!, { unknown: true });
    const emptySeat = response();
    emptySeat.seatbid[0]!.bid = [];

    for (const value of [nonUsd, duplicateReservation, unknownBody, unknownBid, emptySeat]) {
      expect(parseTrustedServerAuctionResponseV1(value)).toBeUndefined();
    }
  });

  it('permits matching adm only for ADM sources', () => {
    const adm = response();
    const source = {
      type: 'adm' as const,
      version: 1 as const,
      adm: '<div>ok</div>',
      width: 1,
      height: 1,
    };
    const bid = adm.seatbid[0]!.bid[0]!;
    bid.w = 1;
    bid.h = 1;
    bid.adm = source.adm;
    bid.ext.trusted_server.render_source = source;
    expect(parseTrustedServerAuctionResponseV1(adm)).toBeDefined();

    const admWithoutStandardField = structuredClone(adm);
    delete admWithoutStandardField.seatbid[0]!.bid[0]!.adm;
    expect(parseTrustedServerAuctionResponseV1(admWithoutStandardField)).toBeDefined();

    const mismatch = structuredClone(adm);
    mismatch.seatbid[0]!.bid[0]!.adm = '<div>different</div>';
    expect(parseTrustedServerAuctionResponseV1(mismatch)).toBeUndefined();

    const apsWithAdm = response();
    apsWithAdm.seatbid[0]!.bid[0]!.adm = '<div>forbidden</div>';
    expect(parseTrustedServerAuctionResponseV1(apsWithAdm)).toBeUndefined();
  });

  it('binds direct pbs_cache winners to the exact cache id and opaque host/path carrier', () => {
    const cacheId = 'f47447a0-b759-4f2f-9887-af458b79b570';
    const value = response();
    const bid = value.seatbid[0]!.bid[0]!;
    bid.id = cacheId;
    bid.ext.trusted_server.render_source = {
      type: 'pbs_cache',
      version: 1,
      cacheId,
      cacheHost: 'cache.example:8443',
      cachePath: '/pbc/v1/cache',
      width: bid.w,
      height: bid.h,
    };

    const parsed = parseTrustedServerAuctionResponseV1(value);
    expect(parsed?.bids[0]).not.toHaveProperty('rendererReservationId');
    expect(parsed?.bids[0]?.renderSource).toEqual(bid.ext.trusted_server.render_source);

    const mismatched = structuredClone(value);
    mismatched.seatbid[0]!.bid[0]!.id = 'different-cache-id';
    expect(parseTrustedServerAuctionResponseV1(mismatched)).toBeUndefined();
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
    globalThis.fetch = vi.fn().mockResolvedValue(mockResponse) as unknown as typeof fetch;

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
    expect(bids[0]!.price).toBe(2.5);
  });

  it('returns empty array on network error', async () => {
    globalThis.fetch = vi
      .fn()
      .mockRejectedValue(new Error('network error')) as unknown as typeof fetch;

    const bids = await sendAuction('/auction', { adUnits: [] });
    expect(bids).toEqual([]);
  });

  it('returns empty array for non-JSON response', async () => {
    globalThis.fetch = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      headers: { get: () => 'text/html' },
      json: async () => ({}),
    }) as unknown as typeof fetch;

    const bids = await sendAuction('/auction', { adUnits: [] });
    expect(bids).toEqual([]);
  });

  it('returns empty array for non-OK response', async () => {
    globalThis.fetch = vi.fn().mockResolvedValue({
      ok: false,
      status: 500,
      headers: { get: () => 'application/json' },
      json: async () => ({}),
    }) as unknown as typeof fetch;

    const bids = await sendAuction('/auction', { adUnits: [] });
    expect(bids).toEqual([]);
  });
});
