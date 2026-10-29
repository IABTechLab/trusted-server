import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { describe, expect, it } from 'vitest';

import {
  collectBidderEids,
  fitEidCookie,
  MAX_AUCTION_EIDS_BYTES,
  MAX_EID_COOKIE_BYTES,
  type EidCollectionStats,
} from '../../../src/integrations/prebid/eids';
import { FIXTURE_EIDS } from '../../fixtures/eid-fixture-input';

function deepFreeze<T>(value: T, seen = new WeakSet<object>()): T {
  if (value !== null && typeof value === 'object' && !seen.has(value)) {
    seen.add(value);
    for (const child of Object.values(value)) {
      deepFreeze(child, seen);
    }
    Object.freeze(value);
  }
  return value;
}

/** A bidder request shaped as Prebid passes it: frozen `ortb2`, `auctionId` hidden. */
function bidderRequest(user: unknown): unknown {
  return deepFreeze({
    bidderCode: 'trustedServer',
    auctionId: null,
    ortb2: { user },
  });
}

function freshStats(): EidCollectionStats {
  return { forwarded: 0, unserializable: 0, overBudget: 0 };
}

function utf8Length(text: string): number {
  return new TextEncoder().encode(text).length;
}

/** An entry whose `JSON.stringify` is exactly `bytes` UTF-8 bytes long. */
function entryOfBytes(source: string, bytes: number, filler = 'x'): unknown {
  const base = JSON.stringify({ source, uids: [{ id: '' }] });
  const fillerBytes = utf8Length(filler);
  return {
    source,
    uids: [{ id: filler.repeat((bytes - utf8Length(base)) / fillerBytes) }],
  };
}

function decodeCookie(value: string | undefined): unknown {
  expect(value, 'should produce a cookie value').toBeDefined();
  return JSON.parse(
    new TextDecoder().decode(Uint8Array.from(atob(value!), (char) => char.charCodeAt(0)))
  );
}

function readFixture(name: string): string {
  return fs
    .readFileSync(
      path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../fixtures', name),
      'utf8'
    )
    .trimEnd();
}

const EXT_EID = {
  source: 'ext.example.com',
  uids: [{ id: 'ext-uid', atype: 1 }],
};
const USER_EID = {
  source: 'user.example.com',
  uids: [{ id: 'user-uid', atype: 1 }],
};

describe('collectBidderEids', () => {
  it('reads ortb2.user.ext.eids', () => {
    expect(collectBidderEids(bidderRequest({ ext: { eids: [EXT_EID] } }))).toEqual([EXT_EID]);
  });

  it('reads ortb2.user.eids, where RTD modules can write after normalization', () => {
    expect(collectBidderEids(bidderRequest({ eids: [USER_EID] }))).toEqual([USER_EID]);
  });

  it('reads user.ext.eids before user.eids', () => {
    const request = bidderRequest({
      ext: { eids: [EXT_EID] },
      eids: [USER_EID],
    });
    expect(
      Object.isFrozen((request as { ortb2: { user: object } }).ortb2.user),
      'the input should be frozen as Prebid passes it'
    ).toBe(true);

    expect(collectBidderEids(request)).toEqual([EXT_EID, USER_EID]);
  });

  it.each([
    ['no user', bidderRequest(undefined)],
    ['an empty user', bidderRequest({})],
    ['non-array paths', bidderRequest({ ext: { eids: 'x' }, eids: { source: 'x' } })],
    ['no ortb2', deepFreeze({ bidderCode: 'trustedServer' })],
  ])('returns undefined for %s', (_label, request) => {
    expect(collectBidderEids(request)).toBeUndefined();
  });

  it.each([undefined, null, 'request', 7])(
    'returns undefined for a bidder request that is not an object: %s',
    (request) => {
      expect(collectBidderEids(request)).toBeUndefined();
    }
  );

  it('returns plain-data copies without undefined fields', () => {
    const openPair = {
      source: 'openpair.example.com',
      inserter: undefined,
      matcher: undefined,
      mm: 3,
      uids: [{ id: 'pair-uid', atype: 571187 }],
    };

    const result = collectBidderEids(bidderRequest({ ext: { eids: [openPair] } }));

    expect(result, 'undefined provenance fields should disappear').toStrictEqual([
      {
        source: 'openpair.example.com',
        mm: 3,
        uids: [{ id: 'pair-uid', atype: 571187 }],
      },
    ]);
    expect(result![0], 'the result should be a copy, not Prebid’s object').not.toBe(openPair);
  });

  it('drops an entry whose serialization throws and keeps the others', () => {
    const circular: Record<string, unknown> = {};
    circular.self = circular;
    const stats = freshStats();

    const result = collectBidderEids(
      bidderRequest({
        ext: {
          eids: [
            EXT_EID,
            {
              source: 'circular.example.com',
              ext: circular,
              uids: [{ id: 'c' }],
            },
            {
              source: 'bigint.example.com',
              uids: [{ id: 'b', atype: BigInt(10) }],
            },
            USER_EID,
          ],
        },
      }),
      stats
    );

    expect(result).toEqual([EXT_EID, USER_EID]);
    expect(stats).toEqual({ forwarded: 2, unserializable: 2, overBudget: 0 });
  });

  it('keeps a set that fits the UTF-8 budget exactly', () => {
    const first = entryOfBytes('first.example.com', MAX_AUCTION_EIDS_BYTES - 100);
    const second = entryOfBytes('second.example.com', 100);
    const stats = freshStats();

    expect(collectBidderEids(bidderRequest({ ext: { eids: [first, second] } }), stats)).toEqual([
      first,
      second,
    ]);
    expect(stats).toEqual({ forwarded: 2, unserializable: 0, overBudget: 0 });
  });

  it('drops the entry that crosses the budget and every entry after it', () => {
    const first = entryOfBytes('first.example.com', MAX_AUCTION_EIDS_BYTES - 100);
    const second = entryOfBytes('second.example.com', 101);
    const third = entryOfBytes('third.example.com', 60);
    const stats = freshStats();

    expect(
      collectBidderEids(bidderRequest({ ext: { eids: [first, second, third] } }), stats)
    ).toEqual([first]);
    expect(stats).toEqual({ forwarded: 1, unserializable: 0, overBudget: 2 });
  });

  it('measures the budget in UTF-8 bytes, not JavaScript string length', () => {
    const ascii = entryOfBytes('ascii.example.com', 40_000);
    // `€` is one UTF-16 code unit and three UTF-8 bytes: 53 bytes of JSON
    // around 10,000 of them.
    const multibyte = entryOfBytes('multibyte.example.com', 30_053, '€');
    expect(
      JSON.stringify(ascii).length + JSON.stringify(multibyte).length,
      'precondition: the set fits by JavaScript length'
    ).toBeLessThan(MAX_AUCTION_EIDS_BYTES);

    expect(collectBidderEids(bidderRequest({ ext: { eids: [ascii, multibyte] } }))).toEqual([
      ascii,
    ]);
  });

  it('keeps order and repeated sources', () => {
    const a1 = { source: 'a.example.com', mm: 1, uids: [{ id: 'u' }] };
    const b = { source: 'b.example.com', uids: [{ id: 'u' }] };
    const a2 = { source: 'a.example.com', mm: 2, uids: [{ id: 'u' }] };

    expect(collectBidderEids(bidderRequest({ ext: { eids: [a1, b, a2] } }))).toEqual([a1, b, a2]);
  });

  it('forwards a lone surrogate unchanged for the server to drop', () => {
    const result = collectBidderEids(bidderRequest({ ext: { eids: FIXTURE_EIDS } }));

    expect(result).toEqual(FIXTURE_EIDS);
    expect(JSON.stringify(result), 'JSON.stringify should write the escape').toContain(
      '"bad-\\ud800-uid"'
    );
  });

  it('passes the Prebid module shapes from spec 3.5 through as plain data', () => {
    const fiftyOneDegrees = [
      { source: '51d.es', mm: 1, uids: [{ id: 'device-1', atype: 1 }] },
      { source: '51d.es', mm: 2, uids: [{ id: 'device-2', atype: 1 }] },
    ];
    const openPair = {
      source: 'openpair.example.com',
      inserter: undefined,
      matcher: undefined,
      uids: [{ id: 'p' }],
    };
    const liveIntent = {
      source: 'liveintent.com',
      ext: { segments: ['s1', 's2'] },
      uids: [{ id: 'li', atype: 3 }],
    };
    const oneKey = {
      source: 'paf.example.com',
      ext: { preferences: { use_browsing_for_personalization: true } },
      uids: [{ id: 'ok', atype: 1 }],
    };
    const pair = {
      source: 'google.com',
      uids: [{ id: 'pair-id', atype: 571187 }],
    };

    const result = collectBidderEids(
      bidderRequest({
        ext: { eids: [openPair, liveIntent, oneKey, pair] },
        eids: fiftyOneDegrees,
      })
    );

    expect(result).toStrictEqual([
      { source: 'openpair.example.com', uids: [{ id: 'p' }] },
      liveIntent,
      oneKey,
      pair,
      ...fiftyOneDegrees,
    ]);
  });
});

describe('fitEidCookie', () => {
  it('removes EID-level ext and drops the duplicates that creates, comparing with sorted keys', () => {
    const withExt = {
      source: 'a.example.com',
      ext: { segments: ['s1'] },
      uids: [{ id: 'a' }],
    };
    const reordered = { uids: [{ id: 'a' }], source: 'a.example.com' };
    const other = {
      source: 'b.example.com',
      uids: [{ id: 'b', ext: { kept: true } }],
    };

    const fitted = fitEidCookie([withExt, reordered, other]);

    expect(decodeCookie(fitted.value)).toEqual([
      { source: 'a.example.com', uids: [{ id: 'a' }] },
      other,
    ]);
    expect(fitted.droppedSources).toEqual([]);
    expect(fitted.trimmedSources).toEqual([]);
  });

  it('does not modify the entries it is given', () => {
    const entry = {
      source: 'a.example.com',
      ext: { segments: ['s1'] },
      uids: [{ id: 'a' }, { id: 'b' }],
    };
    const snapshot = structuredClone(entry);

    fitEidCookie([entry]);

    expect(entry).toEqual(snapshot);
  });

  it('drops whole tail sources until the cookie fits', () => {
    const long = 'x'.repeat(400);
    const entries = [0, 1, 2, 3, 4, 5, 6].map((index) => ({
      source: `s${index}.example.com`,
      uids: [{ id: long, atype: 1 }],
    }));

    const fitted = fitEidCookie(entries);

    expect(fitted.value!.length).toBeLessThanOrEqual(MAX_EID_COOKIE_BYTES);
    expect(decodeCookie(fitted.value)).toEqual(entries.slice(0, 5));
    expect(fitted.droppedSources).toEqual(['s6.example.com', 's5.example.com']);
    expect(fitted.trimmedSources).toEqual([]);
  });

  it('trims UIDs from the last source before dropping it', () => {
    const long = 'x'.repeat(400);
    const uids = Array.from({ length: 10 }, (_, index) => ({
      id: `${long}${index}`,
      atype: 1,
    }));

    const fitted = fitEidCookie([{ source: 'ids.example.com', uids }]);

    expect(decodeCookie(fitted.value)).toEqual([
      { source: 'ids.example.com', uids: uids.slice(0, 5) },
    ]);
    expect(fitted.droppedSources).toEqual([]);
    expect(fitted.trimmedSources).toEqual(['ids.example.com']);
  });

  it('reports a source trimmed and then dropped only as dropped', () => {
    const huge = 'y'.repeat(2500);
    const kept = {
      source: 'kept.example.com',
      uids: [{ id: 'kept', atype: 1 }],
    };

    const fitted = fitEidCookie([
      kept,
      {
        source: 'dropped.example.com',
        uids: Array.from({ length: 3 }, (_, index) => ({
          id: `${huge}${index}`,
          atype: 1,
        })),
      },
    ]);

    expect(decodeCookie(fitted.value)).toEqual([kept]);
    expect(fitted.droppedSources).toEqual(['dropped.example.com']);
    expect(fitted.trimmedSources).toEqual([]);
  });

  it.each([
    ['a string', 'x'.repeat(4000), '(malformed entry)'],
    [
      'an object without a uids array',
      { source: 'no-uids.example.com', blob: 'x'.repeat(4000) },
      'no-uids.example.com',
    ],
  ])('removes a last entry that is %s whole', (_label, last, droppedLabel) => {
    const small = { source: 'small.example.com', uids: [{ id: 's' }] };

    const fitted = fitEidCookie([small, last]);

    expect(decodeCookie(fitted.value)).toEqual([small]);
    expect(fitted.droppedSources).toEqual([droppedLabel]);
  });

  it('removes a null last entry whole before trimming the entry before it', () => {
    const big = {
      source: 'big.example.com',
      uids: [{ id: 'y'.repeat(1500) }, { id: 'z'.repeat(1500) }],
    };

    const fitted = fitEidCookie([big, null]);

    expect(decodeCookie(fitted.value)).toEqual([
      { source: 'big.example.com', uids: [big.uids[0]] },
    ]);
    expect(fitted.droppedSources).toEqual(['(malformed entry)']);
    expect(fitted.trimmedSources).toEqual(['big.example.com']);
  });

  it('returns no value when nothing fits', () => {
    const fitted = fitEidCookie([{ source: 'huge.example.com', uids: [{ id: 'h'.repeat(5000) }] }]);

    expect(fitted).toEqual({
      value: undefined,
      droppedSources: ['huge.example.com'],
      trimmedSources: [],
    });
  });

  it('encodes non-Latin-1 text as UTF-8 base64 that round-trips', () => {
    const entries = [
      {
        source: 'ids.example.com',
        inserter: 'inserter-€',
        uids: [{ id: '日本' }],
      },
    ];
    expect(() => btoa(JSON.stringify(entries)), 'precondition: btoa alone throws here').toThrow();

    expect(decodeCookie(fitEidCookie(entries).value)).toEqual(entries);
  });

  it('matches btoa output for ASCII payloads', () => {
    const entries = [{ source: 'ids.example.com', uids: [{ id: 'ascii-uid', atype: 1 }] }];

    expect(fitEidCookie(entries).value).toBe(btoa(JSON.stringify(entries)));
  });

  it('produces exactly the ts-eids-cookie fixture for the fixture input', () => {
    const collected = collectBidderEids(bidderRequest({ ext: { eids: FIXTURE_EIDS } }));

    expect(fitEidCookie(collected!).value).toBe(readFixture('ts-eids-cookie.txt'));
  });
});
