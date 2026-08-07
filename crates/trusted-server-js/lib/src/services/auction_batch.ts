import type { NavigationSession, RenderAttemptScope, WinnerContext } from '../kernel/sessions';

import type {
  RenderAttempt,
  RenderAttemptCreationResult,
  RenderCancellationReason,
  RenderFailureReason,
  RenderOutcome,
} from './render';

const DEFAULT_AUCTION_ENDPOINT = '/auction';

export type AuctionBatchFetcher = (input: string, init: RequestInit) => Promise<unknown>;

export interface AuctionBatchBid {
  readonly candidateId: string;
  readonly rendererReservationId: string;
  readonly impid: string;
  readonly provider: string;
  readonly price: number;
  readonly width: number;
  readonly height: number;
  readonly renderSource: unknown;
  readonly adm?: string | undefined;
}

export type AuctionBatchDecision =
  | Readonly<{ slot: string; outcome: 'winner'; candidateId: string }>
  | Readonly<{ slot: string; outcome: 'no_bid' }>
  | Readonly<{ slot: string; outcome: 'failed'; reason: RenderFailureReason }>;

export interface ParsedAuctionBatchResponse {
  readonly auction: Readonly<{
    readonly results: readonly AuctionBatchDecision[];
  }>;
  readonly bids: readonly AuctionBatchBid[];
}

export interface AuctionBatchScheduler {
  readonly clear: (handle: unknown) => void;
  readonly set: (callback: () => void, milliseconds: number) => unknown;
}

export type AuctionBatchSlotResult = Readonly<{ slot: string; path: 'primary' } & RenderOutcome>;

export interface AuctionBatchResult {
  readonly slots: readonly AuctionBatchSlotResult[];
}

export interface AuctionBatch {
  readonly result: Promise<Readonly<AuctionBatchResult>>;
  readonly cancel: () => void;
}

export interface AuctionBatchInput {
  readonly navigation: NavigationSession;
  readonly requestBody: string;
  readonly signal?: AbortSignal;
  readonly slots: readonly string[];
  readonly timeoutMs: number;
}

export interface AuctionBatchServiceOptions {
  readonly cachePolicy?: unknown;
  readonly createAttempt: (owner: RenderAttemptScope) => RenderAttemptCreationResult;
  readonly endpoint?: string;
  readonly fetcher: AuctionBatchFetcher;
  readonly parseResponse: (
    value: unknown,
    cachePolicy?: unknown
  ) => ParsedAuctionBatchResponse | undefined;
  readonly renderWinner: (attempt: RenderAttempt, bid: AuctionBatchBid) => boolean;
  readonly scheduler?: AuctionBatchScheduler;
}

export interface AuctionBatchService {
  readonly create: (input: AuctionBatchInput) => AuctionBatch;
  readonly dispose: () => void;
}

interface ActiveChild {
  readonly attempt: RenderAttempt;
  readonly navigationGeneration: object;
  terminal: boolean;
}

interface BatchChild extends ActiveChild {
  readonly index: number;
  readonly slot: string;
}

function frozen<Value extends object>(value: Value): Readonly<Value> {
  return Object.freeze(value);
}

function defaultScheduler(): AuctionBatchScheduler {
  return frozen({
    clear: (handle: unknown): void =>
      globalThis.clearTimeout(handle as ReturnType<typeof globalThis.setTimeout>),
    set: (callback: () => void, milliseconds: number): unknown =>
      globalThis.setTimeout(callback, milliseconds),
  });
}

function terminalResult(slot: string, outcome: RenderOutcome): AuctionBatchSlotResult {
  return frozen({ slot, path: 'primary' as const, ...outcome });
}

function failedResult(slot: string, reason: RenderFailureReason): AuctionBatchSlotResult {
  return terminalResult(slot, frozen({ outcome: 'failed' as const, reason }));
}

function cancelledResult(slot: string, reason: RenderCancellationReason): AuctionBatchSlotResult {
  return terminalResult(slot, frozen({ outcome: 'cancelled' as const, reason }));
}

function responseMembershipIsExact(
  parsed: ParsedAuctionBatchResponse,
  slots: readonly string[]
): boolean {
  const decisions = parsed.auction.results;
  if (decisions.length !== slots.length) return false;
  const membership = new Set(slots);
  if (membership.size !== slots.length) return false;
  const observed = new Set<string>();
  for (let index = 0; index < decisions.length; index += 1) {
    const slot = decisions[index]?.slot;
    if (!slot || !membership.has(slot) || observed.has(slot)) return false;
    observed.add(slot);
  }
  return observed.size === membership.size;
}

/** Runtime-owned coordinator for navigation-scoped one-fetch auction batches. */
export function createAuctionBatchService(
  options: AuctionBatchServiceOptions
): AuctionBatchService {
  const endpoint = options.endpoint ?? DEFAULT_AUCTION_ENDPOINT;
  const parseResponse = options.parseResponse;
  const scheduler = options.scheduler ?? defaultScheduler();
  const activeByNavigation = new Map<object, Map<string, ActiveChild>>();
  const batches = new Set<Readonly<{ cancel: (reason: RenderCancellationReason) => void }>>();
  let nextBatchOrdinal = 0;
  let disposed = false;

  const activeSlots = (generation: object): Map<string, ActiveChild> => {
    const existing = activeByNavigation.get(generation);
    if (existing) return existing;
    const created = new Map<string, ActiveChild>();
    activeByNavigation.set(generation, created);
    return created;
  };

  const create = (input: AuctionBatchInput): AuctionBatch => {
    const slots = frozen(Array.from(input.slots));
    const results: Array<AuctionBatchSlotResult | undefined> = new Array(slots.length);
    let resolveResult: (value: Readonly<AuctionBatchResult>) => void = () => undefined;
    const result = new Promise<Readonly<AuctionBatchResult>>((resolve) => {
      resolveResult = resolve;
    });
    const immediate = (reason: RenderCancellationReason): AuctionBatch => {
      const terminal = frozen({
        slots: frozen(slots.map((slot) => cancelledResult(slot, reason))),
      });
      resolveResult(terminal);
      return frozen({ result, cancel: () => undefined });
    };

    if (disposed || !input.navigation.isCurrent()) return immediate('navigation_disposed');
    nextBatchOrdinal += 1;
    const owner = input.navigation.createAuctionBatch(`auction-batch-${nextBatchOrdinal}`);
    if (!owner) return immediate('navigation_disposed');

    const children: Array<BatchChild | undefined> = new Array(slots.length);
    const navigationGeneration = input.navigation.generation;
    const navigationSlots = activeSlots(navigationGeneration);
    const controller = new AbortController();
    let callerListener: (() => void) | undefined;
    let deadlineHandle: unknown;
    let deadlineArmed = false;
    let fetchPending = false;
    let finished = false;
    let building = true;
    let remaining = slots.length;

    const clearDeadline = (): void => {
      if (!deadlineArmed) return;
      deadlineArmed = false;
      const handle = deadlineHandle;
      deadlineHandle = undefined;
      try {
        scheduler.clear(handle);
      } catch {
        // The logical deadline is already inert.
      }
    };

    const abortFetch = (): void => {
      if (!fetchPending) return;
      fetchPending = false;
      try {
        controller.abort();
      } catch {
        // Child outcomes remain authoritative if host abort throws.
      }
    };

    const cleanupSignal = (): void => {
      if (!callerListener || !input.signal) return;
      try {
        input.signal.removeEventListener('abort', callerListener);
      } catch {
        // A hostile signal cannot retain batch authority.
      }
      callerListener = undefined;
    };

    const finishIfComplete = (): void => {
      if (finished || building || remaining !== 0) return;
      finished = true;
      clearDeadline();
      abortFetch();
      cleanupSignal();
      const membership = activeByNavigation.get(navigationGeneration);
      if (membership?.size === 0) activeByNavigation.delete(navigationGeneration);
      batches.delete(batchControl);
      try {
        owner.dispose();
      } catch {
        // All public children are already terminal.
      }
      resolveResult(
        frozen({
          slots: frozen(
            results.map(
              (entry, index) => entry ?? failedResult(slots[index] ?? '', 'internal_error')
            )
          ),
        })
      );
    };

    const settleIndex = (index: number, terminal: AuctionBatchSlotResult): void => {
      if (results[index]) return;
      results[index] = terminal;
      remaining -= 1;
      const child = children[index];
      if (child) {
        child.terminal = true;
        if (navigationSlots.get(child.slot) === child) navigationSlots.delete(child.slot);
      }
      finishIfComplete();
    };

    const cancelLive = (reason: RenderCancellationReason): void => {
      for (let index = 0; index < children.length; index += 1) {
        const child = children[index];
        if (!child || child.terminal) continue;
        let cancelled: boolean;
        try {
          cancelled = child.attempt.cancel(reason) === true;
        } catch {
          cancelled = false;
        }
        if (!cancelled && !child.terminal) {
          settleIndex(index, cancelledResult(child.slot, reason));
        }
      }
      finishIfComplete();
    };

    const batchControl = frozen({ cancel: cancelLive });
    batches.add(batchControl);

    for (let index = 0; index < slots.length; index += 1) {
      const slot = slots[index];
      if (!slot) {
        settleIndex(index, failedResult('', 'internal_error'));
        continue;
      }
      const previous = navigationSlots.get(slot);
      if (previous && !previous.terminal) {
        try {
          previous.attempt.cancel('superseded');
        } catch {
          // Exact removal below decides whether the new child may proceed.
        }
      }
      if (navigationSlots.get(slot) === previous && previous && !previous.terminal) {
        settleIndex(index, failedResult(slot, 'internal_error'));
        continue;
      }

      const issued = owner.createRenderAttempt(slot);
      if (!issued.ok) {
        settleIndex(
          index,
          issued.reason === 'identity_generation_failed'
            ? failedResult(slot, 'identity_generation_failed')
            : issued.reason === 'stale_owner'
              ? cancelledResult(slot, 'navigation_disposed')
              : failedResult(slot, 'internal_error')
        );
        continue;
      }
      let created: RenderAttemptCreationResult;
      try {
        created = options.createAttempt(issued.value);
      } catch {
        created = frozen({ ok: false, reason: 'invalid_attempt' as const });
      }
      if (!created.ok) {
        try {
          issued.value.dispose();
        } catch {
          // The failed construction owns no public result authority.
        }
        settleIndex(
          index,
          created.reason === 'identity_generation_failed'
            ? failedResult(slot, 'identity_generation_failed')
            : created.reason === 'stale_owner'
              ? cancelledResult(slot, 'navigation_disposed')
              : failedResult(slot, 'internal_error')
        );
        continue;
      }
      const child: BatchChild = {
        attempt: created.value,
        index,
        navigationGeneration,
        slot,
        terminal: false,
      };
      children[index] = child;
      navigationSlots.set(slot, child);
      let observing: boolean;
      try {
        observing =
          created.value.onSettled((outcome) =>
            settleIndex(index, terminalResult(slot, outcome))
          ) === true;
      } catch {
        observing = false;
      }
      if (!observing && !child.terminal) {
        try {
          created.value.fail('internal_error');
        } catch {
          settleIndex(index, failedResult(slot, 'internal_error'));
        }
      }
    }
    building = false;

    const publicBatch = frozen({
      result,
      cancel: (): void => cancelLive('caller_aborted'),
    });

    if (remaining === 0) {
      finishIfComplete();
      return publicBatch;
    }
    if (input.signal?.aborted === true) {
      cancelLive('caller_aborted');
      return publicBatch;
    }
    if (input.signal) {
      callerListener = (): void => cancelLive('caller_aborted');
      try {
        input.signal.addEventListener('abort', callerListener, { once: true });
      } catch {
        cancelLive('caller_aborted');
        return publicBatch;
      }
      if (Reflect.get(input.signal, 'aborted') === true) {
        cancelLive('caller_aborted');
        return publicBatch;
      }
    }

    const failLive = (reason: RenderFailureReason): void => {
      for (let index = 0; index < children.length; index += 1) {
        const child = children[index];
        if (!child || child.terminal) continue;
        try {
          if (child.attempt.fail(reason) !== true && !child.terminal) {
            settleIndex(index, failedResult(child.slot, reason));
          }
        } catch {
          settleIndex(index, failedResult(child.slot, reason));
        }
      }
      finishIfComplete();
    };

    const completeTransport = (): void => {
      fetchPending = false;
      clearDeadline();
    };

    const applyResponse = (parsed: ParsedAuctionBatchResponse): void => {
      const bids = new Map(parsed.bids.map((bid) => [bid.candidateId, bid]));
      const decisions = new Map(
        parsed.auction.results.map((decision) => [decision.slot, decision])
      );
      for (let index = 0; index < children.length; index += 1) {
        const child = children[index];
        if (!child || child.terminal) continue;
        const decision = decisions.get(child.slot);
        if (!decision) {
          child.attempt.fail('invalid_response');
          continue;
        }
        if (decision.outcome === 'no_bid') {
          child.attempt.noBid();
          continue;
        }
        if (decision.outcome === 'failed') {
          child.attempt.fail(decision.reason);
          continue;
        }
        const bid = bids.get(decision.candidateId);
        const context: WinnerContext | undefined = bid
          ? frozen({ selectedCpm: bid.price })
          : undefined;
        let admitted: boolean;
        try {
          admitted =
            !!bid &&
            !!context &&
            child.attempt.admitDirectWinner(bid.renderSource, context) === true;
        } catch {
          admitted = false;
        }
        if (!admitted || !bid) {
          if (!child.terminal) child.attempt.fail('winner_not_renderable');
          continue;
        }
        let rendering: boolean;
        try {
          rendering = options.renderWinner(child.attempt, bid) === true;
        } catch {
          rendering = false;
        }
        if (!rendering && !child.terminal) child.attempt.fail('winner_not_renderable');
      }
    };

    const processFetch = async (fetchResult: Promise<unknown>): Promise<void> => {
      let response: unknown;
      try {
        response = await fetchResult;
      } catch {
        if (finished) return;
        completeTransport();
        failLive('network_error');
        return;
      }
      if (finished) return;
      let ok: unknown;
      let json: unknown;
      try {
        ok = Reflect.get(response as object, 'ok');
        json = Reflect.get(response as object, 'json');
      } catch {
        completeTransport();
        failLive('invalid_response');
        return;
      }
      if (ok !== true) {
        completeTransport();
        failLive('http_error');
        return;
      }
      if (typeof json !== 'function') {
        completeTransport();
        failLive('invalid_response');
        return;
      }
      let body: unknown;
      try {
        body = await Reflect.apply(json, response, []);
      } catch {
        if (finished) return;
        completeTransport();
        failLive('invalid_response');
        return;
      }
      if (finished) return;
      let parsed: ParsedAuctionBatchResponse | undefined;
      try {
        parsed = parseResponse(body, options.cachePolicy);
      } catch {
        parsed = undefined;
      }
      completeTransport();
      if (!parsed || !responseMembershipIsExact(parsed, slots)) {
        failLive('invalid_response');
        return;
      }
      applyResponse(parsed);
    };

    fetchPending = true;
    try {
      deadlineArmed = true;
      const handle = scheduler.set(() => {
        if (finished || !fetchPending) return;
        abortFetch();
        clearDeadline();
        failLive('auction_timeout');
      }, input.timeoutMs);
      if (deadlineArmed && !finished) deadlineHandle = handle;
      else {
        try {
          scheduler.clear(handle);
        } catch {
          // A synchronously terminal batch cannot regain deadline authority.
        }
      }
    } catch {
      deadlineArmed = false;
      fetchPending = false;
      failLive('internal_error');
      return publicBatch;
    }
    if (finished) return publicBatch;

    let fetchResult: Promise<unknown>;
    try {
      fetchResult = options.fetcher(
        endpoint,
        frozen({
          body: input.requestBody,
          headers: frozen({ 'content-type': 'application/json' }),
          method: 'POST',
          signal: controller.signal,
        })
      );
    } catch {
      completeTransport();
      failLive('network_error');
      return publicBatch;
    }
    void processFetch(fetchResult);
    return publicBatch;
  };

  return frozen({
    create,
    dispose: (): void => {
      if (disposed) return;
      disposed = true;
      const active = Array.from(batches);
      batches.clear();
      for (let index = 0; index < active.length; index += 1) {
        active[index]?.cancel('navigation_disposed');
      }
      activeByNavigation.clear();
    },
  });
}
