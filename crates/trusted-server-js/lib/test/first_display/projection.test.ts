import { describe, expect, it } from 'vitest';

import { parseBrowserAuctionProjectionV1 } from '../../src/core/contracts/auction_projection';
import {
  acceptServerFirstDisplayBatchV1,
  snapshotFirstDisplayBatchV1,
} from '../../src/first_display/leaf/projection';

function freezeTree<Value>(value: Value): Value {
  if (typeof value !== 'object' || value === null || Object.isFrozen(value)) return value;
  for (const child of Object.values(value as Record<string, unknown>)) freezeTree(child);
  return Object.freeze(value);
}

function winnerFixture(options: { provider?: string; source?: 'adm' | 'aps' | 'pbs_cache' } = {}) {
  const source = options.source ?? 'adm';
  const renderSource =
    source === 'adm'
      ? {
          type: 'adm',
          version: 1,
          adm: '<div>example</div>',
          width: 300,
          height: 250,
        }
      : source === 'aps'
        ? {
            type: 'aps',
            version: 1,
            accountId: 'account-1',
            bidId: 'bid-1',
            tagType: 'iframe',
            creativeUrl: 'https://creative.example/render',
            aaxResponse: '',
            width: 300,
            height: 250,
          }
        : {
            type: 'pbs_cache',
            version: 1,
            cacheId: 'cache-1',
            cacheHost: 'cache.example',
            cachePath: '/cache',
            width: 300,
            height: 250,
          };
  const bid = {
    candidateId: 'candidate001',
    slot: 'slot-1',
    provider: options.provider ?? 'example',
    upstreamBidId: 'upstream-1',
    cpm: 1.25,
    currency: 'USD',
    targeting: { hb_pb: '1.25' } as Record<string, string>,
    ...(source === 'pbs_cache' ? {} : { rendererReservationId: `r1_${'a'.repeat(22)}` }),
    renderSource,
  };
  return {
    version: 1,
    projectionDigest: 'b'.repeat(64),
    projection: {
      version: 1,
      auction: {
        version: 1,
        auctionId: 'initial',
        results: [{ slot: 'slot-1', outcome: 'winner', candidateId: 'candidate001' }],
      },
      slots: [
        {
          slot: 'slot-1',
          gamUnitPath: '/123/example',
          divId: 'slot-1',
          formats: [[300, 250]],
          targeting: { placement: 'article' },
        },
      ],
      bids: [bid],
    },
  };
}

describe('first-display projection snapshot', () => {
  it('accepts only the already-frozen server envelope used by the production agent', () => {
    const candidate = freezeTree(winnerFixture({ provider: 'prebid' }));

    expect(acceptServerFirstDisplayBatchV1(candidate)).toMatchObject({
      requiredProtocols: ['gpt', 'prebid'],
      outcomes: [{ slotId: 'slot-1', kind: 'gpt_adm' }],
    });
    expect(acceptServerFirstDisplayBatchV1(winnerFixture())).toBeUndefined();
    expect(
      acceptServerFirstDisplayBatchV1(freezeTree(winnerFixture({ source: 'pbs_cache' })))
    ).toBeUndefined();
  });

  it('derives canonical protocol coverage and remains equal to the persistent ADM parser', () => {
    const candidate = freezeTree(winnerFixture({ provider: 'prebid' }));
    const snapshot = snapshotFirstDisplayBatchV1(candidate);

    expect(snapshot?.requiredProtocols).toEqual(['gpt', 'prebid']);
    expect(snapshot?.outcomes).toEqual([{ slotId: 'slot-1', kind: 'gpt_adm' }]);
    expect(snapshot?.projection).toEqual(parseBrowserAuctionProjectionV1(candidate.projection));
    expect(Object.isFrozen(snapshot)).toBe(true);
    expect(Object.isFrozen(snapshot?.projection.bids[0]?.renderSource)).toBe(true);
  });

  it('derives APS plus GPT and never admits PBS Cache into the agent', () => {
    const aps = snapshotFirstDisplayBatchV1(freezeTree(winnerFixture({ source: 'aps' })));
    expect(aps?.requiredProtocols).toEqual(['aps', 'gpt']);
    expect(aps?.outcomes).toEqual([{ slotId: 'slot-1', kind: 'aps' }]);

    expect(
      snapshotFirstDisplayBatchV1(freezeTree(winnerFixture({ source: 'pbs_cache' })))
    ).toBeUndefined();
  });

  it('rejects duplicated summaries, mutable input, and non-data or extra fields', () => {
    expect(
      snapshotFirstDisplayBatchV1(
        Object.freeze({
          version: 1,
          projectionDigest: 'b'.repeat(64),
          requiredProtocols: Object.freeze(['gpt']),
          outcomes: Object.freeze([{ slotId: 'slot-1', kind: 'gpt_adm' }]),
        })
      )
    ).toBeUndefined();
    expect(snapshotFirstDisplayBatchV1(winnerFixture())).toBeUndefined();

    const extra = winnerFixture();
    Object.assign(extra.projection.slots[0]!, { extra: true });
    expect(snapshotFirstDisplayBatchV1(freezeTree(extra))).toBeUndefined();

    const accessor = winnerFixture();
    Object.defineProperty(accessor.projection.slots[0]!, 'slot', {
      enumerable: true,
      get: () => 'slot-1',
    });
    expect(snapshotFirstDisplayBatchV1(freezeTree(accessor))).toBeUndefined();
  });

  it('rejects broken winner joins, unknown failures, and bounded targeting overflow', () => {
    const missingBid = winnerFixture();
    missingBid.projection.bids = [];
    expect(snapshotFirstDisplayBatchV1(freezeTree(missingBid))).toBeUndefined();

    const failed = winnerFixture();
    Object.assign(failed.projection.auction, {
      results: [{ slot: 'slot-1', outcome: 'failed', reason: 'unknown' }],
    });
    failed.projection.bids = [];
    expect(snapshotFirstDisplayBatchV1(freezeTree(failed))).toBeUndefined();

    const targeting = winnerFixture();
    targeting.projection.bids[0]!.targeting = Object.fromEntries(
      Array.from({ length: 33 }, (_, index) => [`key_${index}`, 'value'])
    );
    expect(snapshotFirstDisplayBatchV1(freezeTree(targeting))).toBeUndefined();
  });
});
