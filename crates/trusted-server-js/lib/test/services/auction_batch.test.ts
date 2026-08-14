import { describe, expect, it, vi } from 'vitest';

import { parseTrustedServerAuctionResponseV1 } from '../../src/core/auction';
import { createTestNavigationIdentityIssuer } from '../../src/kernel/identity';
import {
  createRuntimeSession,
  type NavigationSession,
  type RenderAttemptScope,
} from '../../src/kernel/sessions';
import {
  createAuctionBatchService,
  type AuctionBatchFetcher,
  type AuctionBatchServiceOptions,
} from '../../src/services/auction_batch';
import type {
  RenderAttempt,
  RenderCancellationReason,
  RenderFailureReason,
  RenderOutcome,
} from '../../src/services/render';

function navigation(): NavigationSession {
  const runtime = createRuntimeSession({
    createIdentityIssuer: () =>
      createTestNavigationIdentityIssuer({
        getRandomValues: (target) => {
          target.fill(7);
          return target;
        },
      }),
  });
  const result = runtime.startInitialNavigation();
  if (!result.ok) throw new Error(result.reason);
  return result.value;
}

interface AttemptHarness {
  readonly attempt: RenderAttempt;
  readonly outcomes: readonly RenderOutcome[];
}

function attemptHarness(owner: RenderAttemptScope): AttemptHarness {
  const outcomes: RenderOutcome[] = [];
  const observers: Array<(outcome: RenderOutcome) => void> = [];
  let outcome: RenderOutcome | undefined;
  const settle = (next: RenderOutcome): boolean => {
    if (outcome) return false;
    outcome = Object.freeze(next);
    outcomes.push(outcome);
    owner.dispose();
    observers.splice(0).forEach((observer) => observer(outcome!));
    return true;
  };
  owner.onDispose('test-render-lifecycle', () => {
    if (!outcome) settle({ outcome: 'cancelled', reason: 'navigation_disposed' });
  });
  const attempt = {
    id: owner.id,
    slot: owner.slot,
    generation: owner.generation,
    navigationGeneration: owner.navigationGeneration,
    parentAttemptId: undefined,
    renderSource: undefined,
    winnerContext: undefined,
    admitDirectWinner: vi.fn(() => true),
    admitClaimedWinner: vi.fn(() => false),
    beginGamClaim: vi.fn(() => false),
    ownerClaimed: vi.fn(() => false),
    ownerRegistered: vi.fn(() => false),
    beginDirect: vi.fn(() => false),
    beginApsDocument: vi.fn(() => false),
    beginAdm: vi.fn(() => false),
    apsDocumentAccepted: vi.fn(() => false),
    accept: () => settle({ outcome: 'accepted' }),
    noBid: () => settle({ outcome: 'no_bid' }),
    fail: (reason: RenderFailureReason) => settle({ outcome: 'failed', reason }),
    cancel: (reason: RenderCancellationReason) => settle({ outcome: 'cancelled', reason }),
    onSettled: (observer: (terminal: RenderOutcome) => void) => {
      if (outcome) observer(outcome);
      else observers.push(observer);
      return true;
    },
    snapshot: () => ({
      history: Object.freeze(outcome ? ['created', outcome.outcome] : ['created']),
      outcome,
      state: outcome?.outcome ?? ('created' as const),
    }),
  } as RenderAttempt;
  return { attempt, outcomes };
}

function candidateId(index: number): string {
  return index.toString(36).padStart(12, 'A');
}

function reservationId(index: number): string {
  return `r1_${index.toString(36).padStart(22, 'A')}`;
}

type Decision =
  | { slot: string; outcome: 'winner'; candidateId: string }
  | { slot: string; outcome: 'no_bid' }
  | { slot: string; outcome: 'failed'; reason: 'provider_timeout' };

function response(decisions: readonly Decision[]): unknown {
  const winners = decisions.filter(
    (decision): decision is Extract<Decision, { outcome: 'winner' }> =>
      decision.outcome === 'winner'
  );
  return {
    id: 'auction-1',
    cur: 'USD',
    seatbid:
      winners.length === 0
        ? []
        : [
            {
              seat: 'prebid',
              bid: winners.map((winner, index) => {
                const source = {
                  type: 'adm',
                  version: 1,
                  adm: `<div>${winner.slot}</div>`,
                  width: 300,
                  height: 250,
                };
                return {
                  id: reservationId(index),
                  impid: winner.slot,
                  price: index + 1,
                  adm: source.adm,
                  w: source.width,
                  h: source.height,
                  ext: {
                    trusted_server: {
                      candidate_id: winner.candidateId,
                      slot_id: winner.slot,
                      render_source: source,
                    },
                  },
                };
              }),
            },
          ],
    ext: {
      trusted_server: {
        slot_results: { version: 1, auctionId: 'auction-1', results: decisions },
      },
    },
  };
}

function successfulFetcher(body: unknown): AuctionBatchFetcher {
  return vi.fn(async () => ({ ok: true, json: async () => body }));
}

function createService(options: Omit<AuctionBatchServiceOptions, 'parseResponse'>) {
  return createAuctionBatchService({
    ...options,
    parseResponse: parseTrustedServerAuctionResponseV1,
  });
}

function abortablePendingFetcher(): {
  readonly fetcher: AuctionBatchFetcher;
  readonly signals: AbortSignal[];
} {
  const signals: AbortSignal[] = [];
  const fetcher: AuctionBatchFetcher = vi.fn(
    (_input, init) =>
      new Promise((_resolve, reject) => {
        const signal = init.signal;
        if (!signal) throw new Error('Expected a fetch signal');
        signals.push(signal);
        signal.addEventListener('abort', () => reject(new DOMException('Aborted', 'AbortError')), {
          once: true,
        });
      })
  );
  return { fetcher, signals };
}

describe('auction batch service', () => {
  it('rejects a response whose decisions reverse immutable request order', async () => {
    const attempts = new Map<string, AttemptHarness>();
    const fetcher = successfulFetcher(
      response([
        { slot: 'slot-a', outcome: 'no_bid' },
        { slot: 'slot-b', outcome: 'winner', candidateId: candidateId(0) },
      ])
    );
    const service = createService({
      createAttempt: (owner) => {
        const harness = attemptHarness(owner);
        attempts.set(owner.slot, harness);
        return { ok: true, value: harness.attempt };
      },
      fetcher,
      renderWinner: (attempt) => attempt.accept(),
    });

    const batch = service.create({
      navigation: navigation(),
      requestBody: '{"adUnits":[]}',
      slots: Object.freeze(['slot-b', 'slot-a']),
      timeoutMs: 10_000,
    });

    await expect(batch.result).resolves.toEqual({
      slots: [
        { slot: 'slot-b', path: 'primary', outcome: 'failed', reason: 'invalid_response' },
        { slot: 'slot-a', path: 'primary', outcome: 'failed', reason: 'invalid_response' },
      ],
    });
    expect(fetcher).toHaveBeenCalledOnce();
    expect(fetcher).toHaveBeenCalledWith(
      '/auction',
      expect.objectContaining({
        method: 'POST',
        body: '{"adUnits":[]}',
        signal: expect.any(AbortSignal),
      })
    );
    expect(attempts.size).toBe(2);
    expect(attempts.get('slot-b')?.attempt.admitDirectWinner).not.toHaveBeenCalled();
    expect(Object.isFrozen(await batch.result)).toBe(true);
    expect(Object.isFrozen((await batch.result).slots)).toBe(true);
  });

  it('fails only live children on the shared response deadline and aborts the fetch', async () => {
    vi.useFakeTimers();
    try {
      const pending = abortablePendingFetcher();
      const service = createService({
        createAttempt: (owner) => ({ ok: true, value: attemptHarness(owner).attempt }),
        fetcher: pending.fetcher,
        renderWinner: () => false,
      });
      const batch = service.create({
        navigation: navigation(),
        requestBody: '{}',
        slots: Object.freeze(['slot-a', 'slot-b']),
        timeoutMs: 100,
      });

      await vi.advanceTimersByTimeAsync(99);
      expect(pending.signals[0]?.aborted).toBe(false);
      await vi.advanceTimersByTimeAsync(1);

      await expect(batch.result).resolves.toEqual({
        slots: [
          { slot: 'slot-a', path: 'primary', outcome: 'failed', reason: 'auction_timeout' },
          { slot: 'slot-b', path: 'primary', outcome: 'failed', reason: 'auction_timeout' },
        ],
      });
      expect(pending.signals[0]?.aborted).toBe(true);
    } finally {
      vi.useRealTimers();
    }
  });

  it('cancels issued children without fetching for an already-aborted caller', async () => {
    const fetcher = successfulFetcher(response([{ slot: 'slot-a', outcome: 'no_bid' }]));
    const createAttempt = vi.fn((owner: RenderAttemptScope) => ({
      ok: true as const,
      value: attemptHarness(owner).attempt,
    }));
    const service = createService({
      createAttempt,
      fetcher,
      renderWinner: () => false,
    });
    const caller = new AbortController();
    caller.abort();

    await expect(
      service.create({
        navigation: navigation(),
        requestBody: '{}',
        signal: caller.signal,
        slots: Object.freeze(['slot-a']),
        timeoutMs: 10_000,
      }).result
    ).resolves.toEqual({
      slots: [{ slot: 'slot-a', path: 'primary', outcome: 'cancelled', reason: 'caller_aborted' }],
    });
    expect(createAttempt).toHaveBeenCalledOnce();
    expect(fetcher).not.toHaveBeenCalled();
  });

  it('supersedes only overlapping children and retains the old fetch until all old children settle', async () => {
    const firstFetch = abortablePendingFetcher();
    const secondFetch = successfulFetcher(response([{ slot: 'slot-a', outcome: 'no_bid' }]));
    const fetchers = [firstFetch.fetcher, secondFetch] as const;
    let fetchIndex = 0;
    const service = createService({
      createAttempt: (owner) => ({ ok: true, value: attemptHarness(owner).attempt }),
      fetcher: (input, init) => fetchers[fetchIndex++]!(input, init),
      renderWinner: () => false,
    });
    const firstAbort = new AbortController();
    const owner = navigation();
    const first = service.create({
      navigation: owner,
      requestBody: '{}',
      signal: firstAbort.signal,
      slots: Object.freeze(['slot-a', 'slot-b']),
      timeoutMs: 10_000,
    });
    const second = service.create({
      navigation: owner,
      requestBody: '{}',
      slots: Object.freeze(['slot-a']),
      timeoutMs: 10_000,
    });

    expect(firstFetch.signals[0]?.aborted).toBe(false);
    await expect(second.result).resolves.toEqual({
      slots: [{ slot: 'slot-a', path: 'primary', outcome: 'no_bid' }],
    });
    firstAbort.abort();
    await expect(first.result).resolves.toEqual({
      slots: [
        { slot: 'slot-a', path: 'primary', outcome: 'cancelled', reason: 'superseded' },
        { slot: 'slot-b', path: 'primary', outcome: 'cancelled', reason: 'caller_aborted' },
      ],
    });
    expect(firstFetch.signals[0]?.aborted).toBe(true);
  });

  it.each([
    {
      name: 'network rejection',
      fetcher: vi.fn(async () => Promise.reject(new Error('offline'))),
      reason: 'network_error',
    },
    {
      name: 'non-success response',
      fetcher: vi.fn(async () => ({ ok: false, json: async () => ({}) })),
      reason: 'http_error',
    },
    {
      name: 'invalid JSON body',
      fetcher: vi.fn(async () => ({
        ok: true,
        json: async () => Promise.reject(new SyntaxError('invalid JSON')),
      })),
      reason: 'invalid_response',
    },
    {
      name: 'missing slot decision',
      fetcher: successfulFetcher(response([{ slot: 'slot-a', outcome: 'no_bid' }])),
      reason: 'invalid_response',
    },
    {
      name: 'extra slot decision',
      fetcher: successfulFetcher(
        response([
          { slot: 'slot-a', outcome: 'no_bid' },
          { slot: 'slot-b', outcome: 'no_bid' },
          { slot: 'slot-extra', outcome: 'no_bid' },
        ])
      ),
      reason: 'invalid_response',
    },
  ] as const)('preserves $name as $reason for every live child', async ({ fetcher, reason }) => {
    const service = createService({
      createAttempt: (owner) => ({ ok: true, value: attemptHarness(owner).attempt }),
      fetcher,
      renderWinner: () => false,
    });

    await expect(
      service.create({
        navigation: navigation(),
        requestBody: '{}',
        slots: Object.freeze(['slot-a', 'slot-b']),
        timeoutMs: 10_000,
      }).result
    ).resolves.toEqual({
      slots: [
        { slot: 'slot-a', path: 'primary', outcome: 'failed', reason },
        { slot: 'slot-b', path: 'primary', outcome: 'failed', reason },
      ],
    });
  });

  it('passes through an exact server failure without inferring no-bid', async () => {
    const service = createService({
      createAttempt: (owner) => ({ ok: true, value: attemptHarness(owner).attempt }),
      fetcher: successfulFetcher(
        response([{ slot: 'slot-a', outcome: 'failed', reason: 'provider_timeout' }])
      ),
      renderWinner: () => false,
    });

    await expect(
      service.create({
        navigation: navigation(),
        requestBody: '{}',
        slots: Object.freeze(['slot-a']),
        timeoutMs: 10_000,
      }).result
    ).resolves.toEqual({
      slots: [
        {
          slot: 'slot-a',
          path: 'primary',
          outcome: 'failed',
          reason: 'provider_timeout',
        },
      ],
    });
  });

  it('ends the shared deadline after parse while retaining caller cancellation during render', async () => {
    vi.useFakeTimers();
    try {
      let fetchSignal: AbortSignal | undefined;
      const settled = vi.fn();
      const service = createService({
        createAttempt: (owner) => ({ ok: true, value: attemptHarness(owner).attempt }),
        fetcher: vi.fn(async (_input, init) => {
          fetchSignal = init.signal;
          return {
            ok: true,
            json: async () =>
              response([{ slot: 'slot-a', outcome: 'winner', candidateId: candidateId(0) }]),
          };
        }),
        renderWinner: () => true,
      });
      const caller = new AbortController();
      const batch = service.create({
        navigation: navigation(),
        requestBody: '{}',
        signal: caller.signal,
        slots: Object.freeze(['slot-a']),
        timeoutMs: 100,
      });
      void batch.result.then(settled);

      await vi.advanceTimersByTimeAsync(1_000);
      expect(settled).not.toHaveBeenCalled();
      expect(fetchSignal?.aborted).toBe(false);

      caller.abort();
      await expect(batch.result).resolves.toEqual({
        slots: [
          { slot: 'slot-a', path: 'primary', outcome: 'cancelled', reason: 'caller_aborted' },
        ],
      });
      expect(fetchSignal?.aborted).toBe(false);
    } finally {
      vi.useRealTimers();
    }
  });

  it('cancels every child and the shared fetch when navigation disposes', async () => {
    const pending = abortablePendingFetcher();
    const owner = navigation();
    const service = createService({
      createAttempt: (attemptOwner) => ({
        ok: true,
        value: attemptHarness(attemptOwner).attempt,
      }),
      fetcher: pending.fetcher,
      renderWinner: () => false,
    });
    const batch = service.create({
      navigation: owner,
      requestBody: '{}',
      slots: Object.freeze(['slot-a', 'slot-b']),
      timeoutMs: 10_000,
    });

    owner.dispose();

    await expect(batch.result).resolves.toEqual({
      slots: [
        {
          slot: 'slot-a',
          path: 'primary',
          outcome: 'cancelled',
          reason: 'navigation_disposed',
        },
        {
          slot: 'slot-b',
          path: 'primary',
          outcome: 'cancelled',
          reason: 'navigation_disposed',
        },
      ],
    });
    expect(pending.signals[0]?.aborted).toBe(true);
  });

  it('aborts the old shared fetch when its only child is superseded', async () => {
    const firstFetch = abortablePendingFetcher();
    const owner = navigation();
    const fetchers = [
      firstFetch.fetcher,
      successfulFetcher(response([{ slot: 'slot-a', outcome: 'no_bid' }])),
    ] as const;
    let fetchIndex = 0;
    const service = createService({
      createAttempt: (attemptOwner) => ({
        ok: true,
        value: attemptHarness(attemptOwner).attempt,
      }),
      fetcher: (input, init) => fetchers[fetchIndex++]!(input, init),
      renderWinner: () => false,
    });
    const first = service.create({
      navigation: owner,
      requestBody: '{}',
      slots: Object.freeze(['slot-a']),
      timeoutMs: 10_000,
    });
    const second = service.create({
      navigation: owner,
      requestBody: '{}',
      slots: Object.freeze(['slot-a']),
      timeoutMs: 10_000,
    });

    await expect(first.result).resolves.toEqual({
      slots: [{ slot: 'slot-a', path: 'primary', outcome: 'cancelled', reason: 'superseded' }],
    });
    expect(firstFetch.signals[0]?.aborted).toBe(true);
    await expect(second.result).resolves.toEqual({
      slots: [{ slot: 'slot-a', path: 'primary', outcome: 'no_bid' }],
    });
  });

  it('fails closed without fetching when deadline setup settles reentrantly', async () => {
    const fetcher = successfulFetcher(response([{ slot: 'slot-a', outcome: 'no_bid' }]));
    const clear = vi.fn();
    const service = createService({
      createAttempt: (owner) => ({ ok: true, value: attemptHarness(owner).attempt }),
      fetcher,
      renderWinner: () => false,
      scheduler: {
        clear,
        set: (callback) => {
          callback();
          return Object.freeze({ handle: true });
        },
      },
    });

    await expect(
      service.create({
        navigation: navigation(),
        requestBody: '{}',
        slots: Object.freeze(['slot-a']),
        timeoutMs: 100,
      }).result
    ).resolves.toEqual({
      slots: [{ slot: 'slot-a', path: 'primary', outcome: 'failed', reason: 'auction_timeout' }],
    });
    expect(fetcher).not.toHaveBeenCalled();
    expect(clear).toHaveBeenCalled();
  });

  it('settles and skips transport when an attempt refuses settlement observation', async () => {
    const fetcher = successfulFetcher(response([{ slot: 'slot-a', outcome: 'no_bid' }]));
    const service = createService({
      createAttempt: (owner) => {
        const attempt = attemptHarness(owner).attempt;
        return {
          ok: true,
          value: {
            ...attempt,
            fail: vi.fn(() => false),
            onSettled: vi.fn(() => false),
          } as RenderAttempt,
        };
      },
      fetcher,
      renderWinner: () => false,
    });

    await expect(
      service.create({
        navigation: navigation(),
        requestBody: '{}',
        slots: Object.freeze(['slot-a']),
        timeoutMs: 100,
      }).result
    ).resolves.toEqual({
      slots: [{ slot: 'slot-a', path: 'primary', outcome: 'failed', reason: 'internal_error' }],
    });
    expect(fetcher).not.toHaveBeenCalled();
  });

  it('contains an attempt that claims cancellation without notifying its observer', async () => {
    const pending = abortablePendingFetcher();
    const service = createService({
      createAttempt: (owner) => {
        const attempt = attemptHarness(owner).attempt;
        return {
          ok: true,
          value: {
            ...attempt,
            cancel: vi.fn(() => true),
          } as RenderAttempt,
        };
      },
      fetcher: pending.fetcher,
      renderWinner: () => false,
    });
    const batch = service.create({
      navigation: navigation(),
      requestBody: '{}',
      slots: Object.freeze(['slot-a']),
      timeoutMs: 10_000,
    });

    batch.cancel();

    await expect(batch.result).resolves.toEqual({
      slots: [{ slot: 'slot-a', path: 'primary', outcome: 'cancelled', reason: 'caller_aborted' }],
    });
    expect(pending.signals[0]?.aborted).toBe(true);
  });

  it('observes a branded caller signal without consulting shadowed instance hooks', async () => {
    const pending = abortablePendingFetcher();
    const service = createService({
      createAttempt: (owner) => ({ ok: true, value: attemptHarness(owner).attempt }),
      fetcher: pending.fetcher,
      renderWinner: () => false,
    });
    const caller = new AbortController();
    const publisherHook = vi.fn(() => {
      throw new Error('publisher signal hook');
    });
    Object.defineProperties(caller.signal, {
      aborted: { configurable: true, get: publisherHook },
      addEventListener: { configurable: true, get: publisherHook },
      removeEventListener: { configurable: true, get: publisherHook },
    });

    const batch = service.create({
      navigation: navigation(),
      requestBody: '{}',
      signal: caller.signal,
      slots: Object.freeze(['slot-a']),
      timeoutMs: 10_000,
    });
    caller.abort();

    await expect(batch.result).resolves.toEqual({
      slots: [{ slot: 'slot-a', path: 'primary', outcome: 'cancelled', reason: 'caller_aborted' }],
    });
    expect(publisherHook).not.toHaveBeenCalled();
    expect(pending.signals[0]?.aborted).toBe(true);
  });
});
