import { describe, expect, it, vi } from 'vitest';

import {
  CREATIVE_ATTEMPT_WINDOW_MS,
  GptDiagnosticsStore,
  MAX_ATTRIBUTION_ISSUES,
  MAX_CALLBACK_ISSUES,
  MAX_CREATIVE_ATTEMPTS,
  MAX_DIAGNOSTIC_SLOTS,
  MAX_REQUEST_CYCLES_PER_SLOT,
  MAX_TRUSTED_SERVER_ASSOCIATIONS,
  REQUEST_PATH_ATTRIBUTION_WINDOW_MS,
  TRUSTED_SERVER_ATTRIBUTION_WINDOW_MS,
  type GptDiagnosticsSlotLike,
} from '../../../src/integrations/gpt_diagnostics/store';

function fakeSlot(elementId: string, adUnitPath = `/example/${elementId}`): GptDiagnosticsSlotLike {
  return {
    getSlotElementId: () => elementId,
    getAdUnitPath: () => adUnitPath,
  };
}

function associateSlot(
  store: GptDiagnosticsStore,
  slot: GptDiagnosticsSlotLike,
  auctionSlotId: string
): void {
  store.recordTrustedServerOpportunity(slot, auctionSlotId, 'renderable_candidate');
}

function recordCompletedAttempts(
  store: GptDiagnosticsStore,
  count: number,
  prefix: string
): number[] {
  const attemptIds: number[] = [];
  let remaining = count;

  for (let slotIndex = 0; remaining > 0; slotIndex += 1) {
    const slot = fakeSlot(`${prefix}-slot-${slotIndex}`);
    const auctionSlotId = `${prefix}-auction-${slotIndex}`;
    associateSlot(store, slot, auctionSlotId);
    const cycles = Math.min(MAX_REQUEST_CYCLES_PER_SLOT, remaining);
    for (let cycleIndex = 0; cycleIndex < cycles; cycleIndex += 1) {
      store.recordSlotRequested(slot);
      const attemptId = store.recordTrustedServerCreativeRequest(auctionSlotId);
      expect(attemptId).toEqual(expect.any(Number));
      attemptIds.push(attemptId!);
      store.recordTrustedServerCreativeResponse(attemptId!);
      remaining -= 1;
    }
  }

  return attemptIds;
}

function assertCoverageEquation(store: GptDiagnosticsStore): void {
  for (const counters of Object.values(store.snapshot().coverage)) {
    expect(counters.observed).toBe(counters.matched + counters.unmatched + counters.ambiguous);
  }
}

function last<T>(values: readonly T[]): T | undefined {
  return values[values.length - 1];
}

describe('GptDiagnosticsStore', () => {
  it('uses the explicit creative-attempt and attribution retention bounds', () => {
    expect(CREATIVE_ATTEMPT_WINDOW_MS).toBe(30_000);
    expect(MAX_CREATIVE_ATTEMPTS).toBe(128);
    expect(MAX_ATTRIBUTION_ISSUES).toBe(128);
  });

  it('records a complete filled lifecycle with valid timings and visibility', () => {
    let now = 10;
    const store = new GptDiagnosticsStore({ now: () => now });
    const slot = fakeSlot('ad-slot-1', '/example/site/banner');

    store.recordSlotRequested(slot);
    now = 25;
    store.recordSlotResponseReceived(slot);
    now = 30;
    store.recordSlotRenderEnded(slot, {
      isEmpty: false,
      size: [728, 90],
      isBackfill: true,
      slotContentChanged: true,
    });
    now = 40;
    store.recordSlotOnload(slot);
    now = 60;
    store.recordImpressionViewable(slot);
    now = 70;
    store.recordSlotVisibilityChanged(slot, 35);
    now = 80;
    store.recordSlotVisibilityChanged(slot, 20);

    const snapshot = store.snapshot();
    const recordedSlot = snapshot.slots[0];
    const cycle = recordedSlot.requests[0];

    expect(snapshot.gptObserved).toBe(true);
    expect(recordedSlot).toMatchObject({
      runtimeSlotNumber: 1,
      slotElementId: 'ad-slot-1',
      adUnitPath: '/example/site/banner',
      currentVisibilityPercentage: 20,
      maximumVisibilityPercentage: 35,
    });
    expect(cycle).toMatchObject({
      requestNumber: 1,
      requestedAtMs: 10,
      responseAtMs: 25,
      renderAtMs: 30,
      loadAtMs: 40,
      viewableAtMs: 60,
      isEmpty: false,
      size: [728, 90],
      isBackfill: true,
      slotContentChanged: true,
      incompleteSequence: false,
      durations: {
        requestToResponseMs: 15,
        responseToRenderMs: 5,
        requestToRenderMs: 20,
        renderToLoadMs: 10,
        renderToViewableMs: 30,
      },
    });
    expect(snapshot.callbackIssues).toEqual([]);
    assertCoverageEquation(store);
  });

  it('matches the unique response-bearing load that arrives before render', () => {
    let now = 1;
    const store = new GptDiagnosticsStore({ now: () => now });
    const slot = fakeSlot('early-load');

    store.recordSlotRequested(slot);
    now = 2;
    store.recordSlotResponseReceived(slot);
    now = 3;
    store.recordSlotOnload(slot);
    now = 4;
    store.recordSlotRenderEnded(slot, { isEmpty: false });

    const cycle = store.snapshot().slots[0].requests[0];
    expect(cycle).toMatchObject({
      loadAtMs: 3,
      loadObservedBeforeRender: true,
      incompleteSequence: false,
    });
    expect(cycle.durations.renderToLoadMs).toBeUndefined();
    expect(store.snapshot().callbackIssues).not.toContainEqual(
      expect.objectContaining({ kind: 'slotOnload', reason: 'invalid_event_order' })
    );
  });

  it('keeps no-response loads unmatched and overlapping response-bearing loads ambiguous', () => {
    let now = 1;
    const store = new GptDiagnosticsStore({ now: () => now });
    const missingResponse = fakeSlot('missing-load-response');
    store.recordSlotRequested(missingResponse);
    store.recordSlotOnload(missingResponse);
    const overlapping = fakeSlot('overlapping-load-response');
    now = 2;
    store.recordSlotRequested(overlapping);
    now = 3;
    store.recordSlotResponseReceived(overlapping);
    now = 4;
    store.recordSlotRequested(overlapping);
    now = 5;
    store.recordSlotResponseReceived(overlapping);
    now = 6;
    store.recordSlotOnload(overlapping);

    expect(store.snapshot().coverage.slotOnload).toMatchObject({ unmatched: 1, ambiguous: 1 });
    assertCoverageEquation(store);
  });

  it('matches load and viewability after a render with unknown fill state', () => {
    let now = 1;
    const store = new GptDiagnosticsStore({ now: () => now });
    const unknown = fakeSlot('unknown-fill');
    const empty = fakeSlot('known-empty');

    store.recordSlotRequested(unknown);
    now = 2;
    store.recordSlotResponseReceived(unknown);
    now = 3;
    store.recordSlotRenderEnded(unknown, {});
    now = 5;
    store.recordSlotOnload(unknown);
    now = 8;
    store.recordImpressionViewable(unknown);

    store.recordSlotRequested(empty);
    store.recordSlotResponseReceived(empty);
    store.recordSlotRenderEnded(empty, { isEmpty: true });
    store.recordSlotOnload(empty);
    store.recordImpressionViewable(empty);

    const [unknownCycle, emptyCycle] = store.snapshot().slots.map((slot) => slot.requests[0]);
    expect(unknownCycle).toMatchObject({
      isEmpty: undefined,
      loadAtMs: 5,
      viewableAtMs: 8,
      durations: { renderToLoadMs: 2, renderToViewableMs: 5 },
    });
    expect(emptyCycle.loadAtMs).toBe(8);
    expect(emptyCycle.viewableAtMs).toBeUndefined();
    expect(store.snapshot().coverage.slotOnload).toMatchObject({ matched: 2, unmatched: 0 });
    expect(store.snapshot().coverage.impressionViewable).toMatchObject({
      matched: 1,
      unmatched: 1,
    });
    assertCoverageEquation(store);
  });

  it('keeps empty and pending cycles truthful without timeout-based incompleteness', () => {
    let now = 1;
    const store = new GptDiagnosticsStore({ now: () => now });
    const requesting = fakeSlot('requesting');
    const responded = fakeSlot('responded');
    const empty = fakeSlot('empty');

    store.recordSlotRequested(requesting);
    now += 1;
    store.recordSlotRequested(responded);
    now += 1;
    store.recordSlotResponseReceived(responded);
    now += 1;
    store.recordSlotRequested(empty);
    now += 1;
    store.recordSlotResponseReceived(empty);
    now += 1;
    store.recordSlotRenderEnded(empty, { isEmpty: true });

    const [requestingCycle, respondedCycle, emptyCycle] = store
      .snapshot()
      .slots.map((slot) => slot.requests[0]);

    expect(requestingCycle.incompleteSequence).toBe(false);
    expect(requestingCycle.responseAtMs).toBeUndefined();
    expect(respondedCycle.incompleteSequence).toBe(false);
    expect(respondedCycle.renderAtMs).toBeUndefined();
    expect(emptyCycle).toMatchObject({ isEmpty: true, incompleteSequence: false });
  });

  it('creates sequential request numbers and retains refresh history', () => {
    let now = 0;
    const store = new GptDiagnosticsStore({ now: () => ++now });
    const slot = fakeSlot('refresh-slot');

    for (let request = 0; request < 3; request += 1) {
      store.recordSlotRequested(slot);
      store.recordSlotResponseReceived(slot);
      store.recordSlotRenderEnded(slot, { isEmpty: request === 1 });
    }

    expect(store.snapshot().slots[0].requests.map((cycle) => cycle.requestNumber)).toEqual([
      1, 2, 3,
    ]);
    assertCoverageEquation(store);
  });

  it('keeps duplicate element IDs separated by Slot object identity', () => {
    const store = new GptDiagnosticsStore({ now: () => 1 });
    const first = fakeSlot('duplicate-id', '/example/first');
    const second = fakeSlot('duplicate-id', '/example/second');

    store.recordSlotRequested(first);
    store.recordSlotRequested(second);

    expect(store.snapshot().slots).toMatchObject([
      { runtimeSlotNumber: 1, slotElementId: 'duplicate-id', adUnitPath: '/example/first' },
      { runtimeSlotNumber: 2, slotElementId: 'duplicate-id', adUnitPath: '/example/second' },
    ]);
  });

  it('tolerates empty and throwing Slot metadata methods', () => {
    const store = new GptDiagnosticsStore({ now: () => 1 });
    const slot: GptDiagnosticsSlotLike = {
      getSlotElementId: () => '',
      getAdUnitPath: () => {
        throw new Error('publisher method failed');
      },
    };

    expect(() => store.recordSlotRequested(slot)).not.toThrow();
    expect(store.snapshot().slots[0]).toMatchObject({ runtimeSlotNumber: 1 });
    expect(store.snapshot().slots[0].slotElementId).toBeUndefined();
    expect(store.snapshot().slots[0].adUnitPath).toBeUndefined();
  });

  it('records callbacks without a request as unmatched issues', () => {
    const store = new GptDiagnosticsStore({ now: () => 12 });
    const slot = fakeSlot('unrequested');

    store.recordSlotResponseReceived(slot);
    store.recordSlotRenderEnded(slot, { isEmpty: false });
    store.recordSlotOnload(slot);
    store.recordImpressionViewable(slot);

    const snapshot = store.snapshot();
    expect(snapshot.slots[0].requests).toEqual([]);
    expect(snapshot.callbackIssues).toHaveLength(4);
    expect(snapshot.callbackIssues.every((issue) => issue.disposition === 'unmatched')).toBe(true);
    expect(
      snapshot.callbackIssues.every((issue) => issue.reason === 'no_compatible_request_cycle')
    ).toBe(true);
    assertCoverageEquation(store);
  });

  it('preserves overlapping request callbacks as ambiguous instead of guessing', () => {
    let now = 0;
    const store = new GptDiagnosticsStore({ now: () => ++now });
    const slot = fakeSlot('overlap');

    store.recordSlotRequested(slot);
    store.recordSlotRequested(slot);
    store.recordSlotResponseReceived(slot);
    store.recordSlotRenderEnded(slot, { isEmpty: false });

    const snapshot = store.snapshot();
    expect(snapshot.slots[0].requests).toHaveLength(2);
    expect(snapshot.slots[0].requests.every((cycle) => cycle.responseAtMs === undefined)).toBe(
      true
    );
    expect(snapshot.slots[0].requests.every((cycle) => cycle.renderAtMs === undefined)).toBe(true);
    expect(snapshot.callbackIssues).toMatchObject([
      {
        kind: 'slotResponseReceived',
        disposition: 'ambiguous',
        reason: 'overlapping_request_cycles',
      },
      {
        kind: 'slotRenderEnded',
        disposition: 'ambiguous',
        reason: 'overlapping_request_cycles',
      },
    ]);
    assertCoverageEquation(store);
  });

  it('keeps a uniquely matched out-of-order callback matched and suppresses invalid duration', () => {
    let now = 10;
    const store = new GptDiagnosticsStore({ now: () => now });
    const slot = fakeSlot('out-of-order');

    store.recordSlotRequested(slot);
    now = 20;
    store.recordSlotRenderEnded(slot, { isEmpty: false });
    now = 30;
    store.recordSlotResponseReceived(slot);

    const snapshot = store.snapshot();
    const cycle = snapshot.slots[0].requests[0];
    expect(cycle.incompleteSequence).toBe(true);
    expect(cycle.durations.requestToResponseMs).toBe(20);
    expect(cycle.durations.requestToRenderMs).toBe(10);
    expect(cycle.durations.responseToRenderMs).toBeUndefined();
    expect(snapshot.coverage.slotResponseReceived).toEqual({
      observed: 1,
      matched: 1,
      unmatched: 0,
      ambiguous: 0,
    });
    expect(snapshot.callbackIssues).toContainEqual(
      expect.objectContaining({
        kind: 'slotResponseReceived',
        disposition: 'matched',
        reason: 'invalid_event_order',
      })
    );
    assertCoverageEquation(store);
  });

  it('enforces slot, cycle, and callback issue retention limits', () => {
    let now = 0;
    const store = new GptDiagnosticsStore({ now: () => ++now });
    const slots = Array.from({ length: MAX_DIAGNOSTIC_SLOTS + 1 }, (_, index) =>
      fakeSlot(`slot-${index}`)
    );

    for (const slot of slots) store.recordSlotRequested(slot);

    let snapshot = store.snapshot();
    expect(snapshot.slots).toHaveLength(MAX_DIAGNOSTIC_SLOTS);
    expect(snapshot.slots[0].runtimeSlotNumber).toBe(2);
    expect(snapshot.metadata.evictedSlots).toBe(1);

    store.recordSlotResponseReceived(slots[0]);
    snapshot = store.snapshot();
    expect(snapshot.callbackIssues[snapshot.callbackIssues.length - 1]).toMatchObject({
      runtimeSlotNumber: 1,
      disposition: 'unmatched',
      reason: 'evicted_slot',
    });

    const retainedSlot = slots[slots.length - 1];
    for (let index = 0; index < MAX_REQUEST_CYCLES_PER_SLOT; index += 1) {
      store.recordSlotRequested(retainedSlot);
    }
    snapshot = store.snapshot();
    const retainedRecord = snapshot.slots[snapshot.slots.length - 1];
    expect(retainedRecord.requests).toHaveLength(MAX_REQUEST_CYCLES_PER_SLOT);
    expect(retainedRecord.requests[0].requestNumber).toBe(2);
    expect(snapshot.metadata.evictedRequestCycles).toBe(1);

    const issueSlot = fakeSlot('issues');
    for (let index = 0; index < MAX_CALLBACK_ISSUES + 1; index += 1) {
      store.recordSlotResponseReceived(issueSlot);
    }
    snapshot = store.snapshot();
    expect(snapshot.callbackIssues).toHaveLength(MAX_CALLBACK_ISSUES);
    expect(snapshot.metadata.droppedCallbacks).toBeGreaterThanOrEqual(2);
    assertCoverageEquation(store);
  });

  it('evicts least-recently-active slots and re-enters only on a new request', () => {
    let now = 0;
    const store = new GptDiagnosticsStore({ now: () => ++now });
    const slots = Array.from({ length: MAX_DIAGNOSTIC_SLOTS + 1 }, (_, index) =>
      fakeSlot(`lru-${index}`)
    );
    for (const retained of slots.slice(0, MAX_DIAGNOSTIC_SLOTS)) {
      store.recordSlotRequested(retained);
    }

    store.recordSlotVisibilityChanged(slots[0], 10);
    store.recordSlotRequested(slots[MAX_DIAGNOSTIC_SLOTS]);
    expect(store.snapshot().slots.some((slot) => slot.runtimeSlotNumber === 1)).toBe(true);
    expect(store.snapshot().slots.some((slot) => slot.runtimeSlotNumber === 2)).toBe(false);

    store.recordSlotResponseReceived(slots[1]);
    expect(last(store.snapshot().callbackIssues)).toMatchObject({
      runtimeSlotNumber: 2,
      reason: 'evicted_slot',
    });

    store.recordSlotRequested(slots[1]);
    store.recordSlotResponseReceived(slots[1]);
    const reentered = store.snapshot().slots.find((slot) => slot.slotElementId === 'lru-1');
    expect(reentered).toMatchObject({ runtimeSlotNumber: 66 });
    expect(reentered?.requests[0]).toMatchObject({ requestNumber: 2 });
    expect(reentered?.requests[0].responseAtMs).toBeDefined();
    expect(store.snapshot().slots).toHaveLength(MAX_DIAGNOSTIC_SLOTS);
    expect(store.snapshot().metadata.evictedSlots).toBe(2);
    assertCoverageEquation(store);
  });

  it('returns lightweight detached binding inputs in stable slot order', () => {
    const store = new GptDiagnosticsStore({ now: () => 1 });
    const first = fakeSlot('first');
    const second = fakeSlot('second');
    store.recordSlotRequested(first);
    store.recordSlotRequested(second);
    store.recordSlotVisibilityChanged(first, 10);

    const inputs = store.bindingInputs();
    expect(inputs).toEqual([
      { runtimeSlotNumber: 1, slotElementId: 'first' },
      { runtimeSlotNumber: 2, slotElementId: 'second' },
    ]);
    inputs[0].slotElementId = 'changed';
    expect(store.bindingInputs()[0].slotElementId).toBe('first');
  });

  it('coalesces notifications and isolates throwing subscribers', () => {
    const scheduled: Array<() => void> = [];
    const store = new GptDiagnosticsStore({
      now: () => 1,
      schedule: (callback) => scheduled.push(callback),
    });
    const goodListener = vi.fn();
    store.subscribe(() => {
      throw new Error('subscriber failed');
    });
    const unsubscribe = store.subscribe(goodListener);

    store.markGptObserved();
    store.recordSlotRequested(fakeSlot('notify'));
    expect(scheduled).toHaveLength(1);

    scheduled.shift()!();
    expect(goodListener).toHaveBeenCalledTimes(1);

    unsubscribe();
    store.recordSlotVisibilityChanged(fakeSlot('other'), 10);
    scheduled.shift()!();
    expect(goodListener).toHaveBeenCalledTimes(1);
  });

  it('retains the Ad Manager identifiers GPT reported for the delivered ad', () => {
    const store = new GptDiagnosticsStore({ now: () => 10 });
    const slot = fakeSlot('ad-slot-identity');

    store.recordSlotRequested(slot);
    store.recordSlotResponseReceived(slot);
    store.recordSlotRenderEnded(slot, {
      isEmpty: false,
      adManager: {
        lineItemId: 6543210987,
        creativeId: 1234567890,
        campaignId: 2345678901,
        advertiserId: 3456789012,
      },
    });

    const cycle = store.snapshot().slots[0].requests[0];
    expect(cycle.adManager, 'should keep every reported identifier').toEqual({
      lineItemId: 6543210987,
      creativeId: 1234567890,
      campaignId: 2345678901,
      advertiserId: 3456789012,
    });
    expect(cycle.responseClass).toBe('reservation');
  });

  it('retains an observed outer slot box separately from GPT reported size', () => {
    const store = new GptDiagnosticsStore({ now: () => 10 });
    const slot = fakeSlot('ad-slot-outer-box');

    store.recordSlotRequested(slot);
    store.recordSlotResponseReceived(slot);
    store.recordSlotRenderEnded(slot, { isEmpty: false, size: [1, 1] });
    store.recordObservedSlotSize(1, 1, [728, 90]);

    const cycle = store.snapshot().slots[0].requests[0];
    expect(cycle.size).toEqual([1, 1]);
    expect(cycle.observedSlotSize).toEqual([728, 90]);
  });

  it('rejects a stale prior-cycle outer-box measurement after a refresh', () => {
    const store = new GptDiagnosticsStore({ now: () => 10 });
    const slot = fakeSlot('ad-slot-stale-outer-box');

    store.recordSlotRequested(slot);
    store.recordSlotResponseReceived(slot);
    store.recordSlotRenderEnded(slot, { isEmpty: false });
    store.recordSlotRequested(slot);
    store.recordSlotResponseReceived(slot);
    store.recordSlotRenderEnded(slot, { isEmpty: false });
    store.recordObservedSlotSize(1, 1, [300, 250]);
    store.recordObservedSlotSize(1, 2, [970, 250]);

    const requests = store.snapshot().slots[0].requests;
    expect(requests[0].observedSlotSize).toBeUndefined();
    expect(requests[1].observedSlotSize).toEqual([970, 250]);
  });

  it('separates a fill without Ad Manager identifiers from a reservation', () => {
    const store = new GptDiagnosticsStore({ now: () => 10 });
    const slot = fakeSlot('ad-slot-default');

    store.recordSlotRequested(slot);
    store.recordSlotRenderEnded(slot, { isEmpty: false });

    const cycle = store.snapshot().slots[0].requests[0];
    expect(cycle.responseClass).toBe('unclassified_non_empty');
    expect(cycle.adManager).toBeUndefined();
  });

  it.each([
    {
      name: 'a direct renderable candidate',
      direct: 'renderable_candidate',
      prebid: false,
      publisher: false,
      expectedPath: 'trusted_server_direct',
      expectedOpportunity: 'renderable_candidate',
    },
    {
      name: 'a direct unrenderable candidate',
      direct: 'unrenderable_candidate',
      prebid: false,
      publisher: false,
      expectedPath: 'trusted_server_direct',
      expectedOpportunity: 'unrenderable_candidate',
    },
    {
      name: 'a direct request without a candidate',
      direct: 'no_candidate',
      prebid: false,
      publisher: false,
      expectedPath: 'trusted_server_direct',
      expectedOpportunity: 'no_candidate',
    },
    {
      name: 'a Prebid refresh',
      direct: undefined,
      prebid: true,
      publisher: false,
      expectedPath: 'prebid_refresh',
      expectedOpportunity: undefined,
    },
    {
      name: 'competing direct and Prebid evidence',
      direct: 'renderable_candidate',
      prebid: true,
      publisher: false,
      expectedPath: 'competing',
      expectedOpportunity: 'renderable_candidate',
    },
    {
      name: 'an unattributed request',
      direct: undefined,
      prebid: false,
      publisher: false,
      expectedPath: 'unattributed',
      expectedOpportunity: undefined,
    },
    {
      name: 'a publisher refresh',
      direct: undefined,
      prebid: false,
      publisher: true,
      expectedPath: 'publisher_refresh',
      expectedOpportunity: undefined,
    },
    {
      name: 'competing Prebid and publisher evidence',
      direct: undefined,
      prebid: true,
      publisher: true,
      expectedPath: 'competing',
      expectedOpportunity: undefined,
    },
    {
      name: 'competing all source evidence',
      direct: 'renderable_candidate',
      prebid: true,
      publisher: true,
      expectedPath: 'competing',
      expectedOpportunity: 'renderable_candidate',
    },
  ] as const)(
    'attributes $name without inferring demand ownership',
    ({ direct, prebid, publisher, expectedPath, expectedOpportunity }) => {
      const store = new GptDiagnosticsStore({ now: () => 10, defer: () => undefined });
      const slot = fakeSlot('path-slot');

      if (direct !== undefined) {
        store.recordTrustedServerOpportunity(slot, 'auction-slot', direct);
      }
      if (prebid) store.recordPrebidRefresh([slot]);
      if (publisher) store.recordPublisherRefresh([slot]);
      store.recordSlotRequested(slot);

      const cycle = store.snapshot().slots[0].requests[0];
      expect(cycle.requestPath).toBe(expectedPath);
      expect(cycle.trustedServerOpportunity).toBe(expectedOpportunity);
    }
  );

  it('consumes direct and Prebid markers exactly once', () => {
    let now = 10;
    const store = new GptDiagnosticsStore({ now: () => now, defer: () => undefined });
    const slot = fakeSlot('one-shot');

    store.recordTrustedServerOpportunity(slot, 'auction-slot', 'renderable_candidate');
    store.recordPrebidRefresh([slot]);
    store.recordSlotRequested(slot);
    now = 11;
    store.recordSlotRequested(slot);

    const cycles = store.snapshot().slots[0].requests;
    expect(cycles).toMatchObject([
      {
        requestPath: 'competing',
        trustedServerOpportunity: 'renderable_candidate',
      },
      { requestPath: 'unattributed' },
    ]);
    expect(cycles[1].trustedServerOpportunity).toBeUndefined();
  });

  it('consumes a combined request intent with independent source facts', () => {
    let now = 10;
    const deferred: Array<() => void> = [];
    const store = new GptDiagnosticsStore({
      now: () => now,
      defer: (callback) => deferred.push(callback),
    });
    const slot = fakeSlot('intent');

    store.recordTrustedServerOpportunity(
      slot,
      'auction-slot',
      'renderable_candidate',
      ' auction-123 '
    );
    now = 20;
    store.recordPrebidRefresh([slot]);
    now = 30;
    store.recordPublisherRefresh([slot]);
    now = 34;
    store.recordSlotRequested(slot);
    now = 35;
    store.recordSlotRequested(slot);

    const cycles = store.snapshot().slots[0].requests;
    expect(cycles).toMatchObject([
      {
        requestPath: 'competing',
        requestIntentId: 1,
        trustedServerOpportunity: 'renderable_candidate',
        trustedServerAuctionId: 'auction-123',
        opportunityToRequestMs: 24,
      },
      { requestPath: 'unattributed' },
    ]);
    expect(cycles[1].requestIntentId).toBeUndefined();
    expect(deferred, 'source evidence must not schedule deferred work').toHaveLength(0);
  });

  it('keeps repeated source evidence single-source and increments consumed intent IDs', () => {
    let now = 1;
    const store = new GptDiagnosticsStore({ now: () => now, defer: () => undefined });
    const first = fakeSlot('repeat-intent-first');
    const second = fakeSlot('repeat-intent-second');
    store.recordPublisherRefresh([first]);
    now = 2;
    store.recordPublisherRefresh([first]);
    store.recordSlotRequested(first);
    now = 3;
    store.recordTrustedServerOpportunity(second, 'second-auction', 'no_candidate');
    store.recordPublisherRefresh([second]);
    store.recordSlotRequested(second);

    expect(store.snapshot().slots[0].requests[0]).toMatchObject({
      requestPath: 'publisher_refresh',
      requestIntentId: 1,
    });
    expect(store.snapshot().slots[1].requests[0]).toMatchObject({
      requestPath: 'competing',
      requestIntentId: 2,
    });
  });

  it('expires repeated source evidence lazily without scheduling timer work', () => {
    let now = 0;
    const deferred: Array<{ callback: () => void; delayMs: number }> = [];
    const store = new GptDiagnosticsStore({
      now: () => now,
      defer: (callback, delayMs) => deferred.push({ callback, delayMs }),
    });
    const consumed = fakeSlot('lazy-expiry-consumed');
    const expired = fakeSlot('lazy-expiry-expired');

    for (let observation = 0; observation < 1_000; observation += 1) {
      now = observation;
      store.recordPublisherRefresh([consumed, expired]);
    }

    expect(deferred, 'a refresh burst must not queue deferred work').toHaveLength(0);

    // The window runs from the newest observation, at t = 999.
    now = 999 + REQUEST_PATH_ATTRIBUTION_WINDOW_MS - 1;
    store.recordSlotRequested(consumed);
    now += 1;
    store.recordSlotRequested(expired);

    expect(store.snapshot().slots[0].requests[0].requestPath).toBe('publisher_refresh');
    expect(store.snapshot().slots[1].requests[0].requestPath).toBe('unattributed');
    expect(deferred, 'expiry must stay free of deferred work').toHaveLength(0);
  });

  it('replaces a fully expired intent instead of reviving its intent ID', () => {
    let now = 0;
    const deferred: Array<() => void> = [];
    const store = new GptDiagnosticsStore({
      now: () => now,
      defer: (callback) => deferred.push(callback),
    });
    const slot = fakeSlot('expired-intent-replacement');

    store.recordTrustedServerOpportunity(slot, 'auction-slot', 'renderable_candidate', 'stale');
    now = REQUEST_PATH_ATTRIBUTION_WINDOW_MS;
    store.recordPublisherRefresh([slot]);
    store.recordSlotRequested(slot);

    const cycle = store.snapshot().slots[0].requests[0];
    expect(cycle).toMatchObject({ requestPath: 'publisher_refresh', requestIntentId: 2 });
    expect(
      cycle.trustedServerOpportunity,
      'expired direct evidence must not survive'
    ).toBeUndefined();
    expect(cycle.trustedServerAuctionId).toBeUndefined();
    expect(deferred).toHaveLength(0);
  });

  it('derives a replacement from the most recent earlier filled render', () => {
    let now = 1;
    const store = new GptDiagnosticsStore({ now: () => now });
    const slot = fakeSlot('replacement');

    store.recordSlotRequested(slot);
    now = 2;
    store.recordSlotResponseReceived(slot);
    now = 3;
    store.recordSlotRenderEnded(slot, { isEmpty: false, adManager: { creativeId: 101 } });
    now = 20;
    store.recordSlotRequested(slot);
    now = 21;
    store.recordSlotResponseReceived(slot);
    now = 22;
    store.recordSlotRenderEnded(slot, { isEmpty: false, adManager: { creativeId: 202 } });

    expect(store.snapshot().slots[0].requests[1]).toMatchObject({
      replacedRequestNumber: 1,
      previousRenderToRequestMs: 17,
      previousCreativeId: 101,
      creativeChanged: true,
    });
  });

  it('compares primary and source-agnostic GPT creative identities for replacements', () => {
    let now = 1;
    const store = new GptDiagnosticsStore({ now: () => now });
    const slot = fakeSlot('replacement-fallback-creative');
    store.recordSlotRequested(slot);
    now = 2;
    store.recordSlotResponseReceived(slot);
    now = 3;
    store.recordSlotRenderEnded(slot, {
      isEmpty: false,
      adManager: { sourceAgnosticCreativeId: 101 },
    });
    now = 4;
    store.recordSlotRequested(slot);
    now = 5;
    store.recordSlotResponseReceived(slot);
    now = 6;
    store.recordSlotRenderEnded(slot, { isEmpty: false, adManager: { creativeId: 101 } });

    expect(store.snapshot().slots[0].requests[1]).toMatchObject({
      previousCreativeId: 101,
      creativeChanged: false,
    });
  });

  it('uses the latest earlier filled render while ignoring empty renders', () => {
    let now = 1;
    const store = new GptDiagnosticsStore({ now: () => now });
    const slot = fakeSlot('replacement-most-recent-filled');

    store.recordSlotRequested(slot);
    now = 2;
    store.recordSlotRenderEnded(slot, { isEmpty: false, adManager: { creativeId: 101 } });
    now = 3;
    store.recordSlotRequested(slot);
    now = 4;
    store.recordSlotRenderEnded(slot, { isEmpty: false, adManager: { creativeId: 202 } });
    now = 5;
    store.recordSlotRequested(slot);
    now = 6;
    store.recordSlotRenderEnded(slot, { isEmpty: true });
    now = 7;
    store.recordSlotRequested(slot);
    now = 8;
    store.recordSlotRenderEnded(slot, { isEmpty: false, adManager: { creativeId: 303 } });

    const requests = store.snapshot().slots[0].requests;
    expect(requests[2].replacedRequestNumber).toBeUndefined();
    expect(requests[3]).toMatchObject({
      replacedRequestNumber: 2,
      previousRenderToRequestMs: 3,
      previousCreativeId: 202,
      creativeChanged: true,
    });
  });

  it('reports one-sided creative IDs without claiming a creative change', () => {
    let now = 1;
    const store = new GptDiagnosticsStore({ now: () => now });
    const previousOnly = fakeSlot('replacement-previous-id-only');
    const currentOnly = fakeSlot('replacement-current-id-only');

    store.recordSlotRequested(previousOnly);
    now = 2;
    store.recordSlotRenderEnded(previousOnly, { isEmpty: false, adManager: { creativeId: 101 } });
    now = 3;
    store.recordSlotRequested(previousOnly);
    now = 4;
    store.recordSlotRenderEnded(previousOnly, { isEmpty: false });

    store.recordSlotRequested(currentOnly);
    now = 5;
    store.recordSlotRenderEnded(currentOnly, { isEmpty: false });
    now = 6;
    store.recordSlotRequested(currentOnly);
    now = 7;
    store.recordSlotRenderEnded(currentOnly, { isEmpty: false, adManager: { creativeId: 202 } });

    const [previousOnlyCycle] = store.snapshot().slots[0].requests.slice(-1);
    const [currentOnlyCycle] = store.snapshot().slots[1].requests.slice(-1);
    expect(previousOnlyCycle).toMatchObject({ replacedRequestNumber: 1, previousCreativeId: 101 });
    expect(previousOnlyCycle.creativeChanged).toBeUndefined();
    expect(currentOnlyCycle).toMatchObject({ replacedRequestNumber: 1 });
    expect(currentOnlyCycle.previousCreativeId).toBeUndefined();
    expect(currentOnlyCycle.creativeChanged).toBeUndefined();
  });

  it('does not infer replacements once the earlier filled cycle has been evicted', () => {
    let now = 1;
    const store = new GptDiagnosticsStore({ now: () => now });
    const slot = fakeSlot('replacement-evicted');

    store.recordSlotRequested(slot);
    now += 1;
    store.recordSlotRenderEnded(slot, { isEmpty: false, adManager: { creativeId: 101 } });
    // Complete every filler cycle so the eviction pushes the only filled render
    // out of retention and the final render still matches exactly one cycle.
    for (let index = 0; index < MAX_REQUEST_CYCLES_PER_SLOT; index += 1) {
      now += 1;
      store.recordSlotRequested(slot);
      now += 1;
      store.recordSlotResponseReceived(slot);
      now += 1;
      store.recordSlotRenderEnded(slot, { isEmpty: true });
    }
    now += 1;
    store.recordSlotRequested(slot);
    now += 1;
    store.recordSlotResponseReceived(slot);
    now += 1;
    store.recordSlotRenderEnded(slot, { isEmpty: false, adManager: { creativeId: 202 } });

    const requests = store.snapshot().slots[0].requests;
    expect(
      requests.some((cycle) => cycle.adManager?.creativeId === 101),
      'the earlier filled cycle should have been evicted'
    ).toBe(false);
    const latestCycle = last(requests)!;
    expect(latestCycle.renderAtMs, 'the final render must have been matched').toBeDefined();
    expect(latestCycle.adManager?.creativeId).toBe(202);
    expect(latestCycle.replacedRequestNumber).toBeUndefined();
    expect(latestCycle.previousRenderToRequestMs).toBeUndefined();
    expect(latestCycle.previousCreativeId).toBeUndefined();
  });

  it('keeps Trusted Server and publisher source evidence separate from replacement facts', () => {
    let now = 1;
    const store = new GptDiagnosticsStore({ now: () => now, defer: () => undefined });
    const slot = fakeSlot('replacement-source-evidence');

    store.recordSlotRequested(slot);
    now = 2;
    store.recordSlotRenderEnded(slot, { isEmpty: false, adManager: { creativeId: 101 } });
    now = 3;
    store.recordTrustedServerOpportunity(slot, 'auction-slot', 'renderable_candidate');
    store.recordPublisherRefresh([slot]);
    store.recordSlotRequested(slot);
    now = 4;
    store.recordSlotRenderEnded(slot, { isEmpty: false, adManager: { creativeId: 202 } });

    expect(store.snapshot().slots[0].requests[1]).toMatchObject({
      requestPath: 'competing',
      replacedRequestNumber: 1,
      previousCreativeId: 101,
      creativeChanged: true,
    });
  });

  it('expires request-path markers at the five-second boundary without waiting for timers', () => {
    let now = 0;
    const deferred: Array<() => void> = [];
    const store = new GptDiagnosticsStore({
      now: () => now,
      defer: (callback) => deferred.push(callback),
    });
    const beforeBoundary = fakeSlot('before-boundary');
    const atBoundary = fakeSlot('at-boundary');

    for (const slot of [beforeBoundary, atBoundary]) {
      store.recordTrustedServerOpportunity(
        slot,
        `auction-${slot.getSlotElementId?.()}`,
        'no_candidate'
      );
      store.recordPrebidRefresh([slot]);
    }

    now = REQUEST_PATH_ATTRIBUTION_WINDOW_MS - 1;
    store.recordSlotRequested(beforeBoundary);
    now = REQUEST_PATH_ATTRIBUTION_WINDOW_MS;
    store.recordSlotRequested(atBoundary);

    const [before, expired] = store.snapshot().slots.map((slot) => slot.requests[0]);
    expect(before).toMatchObject({
      requestPath: 'competing',
      trustedServerOpportunity: 'no_candidate',
    });
    expect(expired).toMatchObject({ requestPath: 'unattributed' });
    expect(expired.trustedServerOpportunity).toBeUndefined();
    expect(deferred, 'the boundary must be enforced without marker timers').toHaveLength(0);
  });

  it('keeps the newest evidence when a source is re-observed inside the window', () => {
    let now = 0;
    const deferred: Array<() => void> = [];
    const store = new GptDiagnosticsStore({
      now: () => now,
      defer: (callback) => deferred.push(callback),
    });
    const slot = fakeSlot('re-observed-source');

    store.recordTrustedServerOpportunity(slot, 'auction-slot', 'renderable_candidate');
    store.recordPrebidRefresh([slot]);
    now = 100;
    store.recordTrustedServerOpportunity(slot, 'auction-slot', 'unrenderable_candidate');
    store.recordPrebidRefresh([slot]);
    store.recordSlotRequested(slot);

    expect(store.snapshot().slots[0].requests[0]).toMatchObject({
      requestPath: 'competing',
      requestIntentId: 1,
      trustedServerOpportunity: 'unrenderable_candidate',
    });
    expect(deferred).toHaveLength(0);
  });

  it('expires sources independently and replaces Trusted Server auction metadata', () => {
    let now = 0;
    const deferred: Array<() => void> = [];
    const store = new GptDiagnosticsStore({
      now: () => now,
      defer: (callback) => deferred.push(callback),
    });
    const slot = fakeSlot('independent-expiry');
    store.recordTrustedServerOpportunity(slot, 'auction-slot', 'renderable_candidate', 'old');
    now = 1;
    store.recordPrebidRefresh([slot]);
    now = 2;
    store.recordTrustedServerOpportunity(slot, 'auction-slot', 'no_candidate');
    now = REQUEST_PATH_ATTRIBUTION_WINDOW_MS + 1;
    store.recordSlotRequested(slot);

    expect(store.snapshot().slots[0].requests[0]).toMatchObject({
      requestPath: 'trusted_server_direct',
      trustedServerOpportunity: 'no_candidate',
    });
    expect(store.snapshot().slots[0].requests[0].trustedServerAuctionId).toBeUndefined();
  });

  it('uses replacement Trusted Server evidence for latency and removes an unconsumed final source', () => {
    let now = 0;
    const deferred: Array<() => void> = [];
    const store = new GptDiagnosticsStore({
      now: () => now,
      defer: (callback) => deferred.push(callback),
    });
    const repeated = fakeSlot('repeated-trusted-server-evidence');
    const unconsumed = fakeSlot('unconsumed-trusted-server-evidence');

    store.recordTrustedServerOpportunity(repeated, 'auction-slot', 'renderable_candidate');
    now = 40;
    store.recordTrustedServerOpportunity(repeated, 'auction-slot', 'no_candidate');
    now = 50;
    store.recordSlotRequested(repeated);

    expect(store.snapshot().slots[0].requests[0]).toMatchObject({
      requestPath: 'trusted_server_direct',
      trustedServerOpportunity: 'no_candidate',
      opportunityToRequestMs: 10,
    });

    now = 60;
    store.recordTrustedServerOpportunity(unconsumed, 'other-auction-slot', 'no_candidate');
    now += REQUEST_PATH_ATTRIBUTION_WINDOW_MS;
    store.recordSlotRequested(unconsumed);

    const unconsumedCycle = store.snapshot().slots[1].requests[0];
    expect(unconsumedCycle.requestPath).toBe('unattributed');
    expect(unconsumedCycle.requestIntentId).toBeUndefined();
  });

  it('retains only valid bounded auction IDs without dropping Trusted Server intent', () => {
    const valid = 'a'.repeat(256);
    const cases: Array<[unknown, string | undefined]> = [
      [valid, valid],
      ['', undefined],
      ['   ', undefined],
      [123, undefined],
      ['é'.repeat(129), undefined],
    ];
    for (const [auctionId, expected] of cases) {
      const store = new GptDiagnosticsStore({ now: () => 1, defer: () => undefined });
      const slot = fakeSlot(`auction-id-${String(auctionId).length}`);
      store.recordTrustedServerOpportunity(
        slot,
        'auction-slot',
        'renderable_candidate',
        auctionId as string
      );
      store.recordSlotRequested(slot);
      const cycle = store.snapshot().slots[0].requests[0];
      expect(cycle.requestPath).toBe('trusted_server_direct');
      expect(cycle.trustedServerAuctionId).toBe(expected);
    }
  });

  it('does not mutate an open request cycle when a later direct marker arrives', () => {
    const store = new GptDiagnosticsStore({ now: () => 10, defer: () => undefined });
    const slot = fakeSlot('open-cycle');

    store.recordSlotRequested(slot);
    store.recordTrustedServerOpportunity(slot, 'auction-slot', 'renderable_candidate');

    const openCycle = store.snapshot().slots[0].requests[0];
    expect(openCycle).toMatchObject({ requestPath: 'unattributed' });
    expect(openCycle.trustedServerOpportunity).toBeUndefined();

    store.recordSlotRequested(slot);
    expect(store.snapshot().slots[0].requests[1]).toMatchObject({
      requestPath: 'trusted_server_direct',
      trustedServerOpportunity: 'renderable_candidate',
    });
  });

  it('ignores malformed diagnostic marker inputs without throwing', () => {
    const store = new GptDiagnosticsStore({ now: () => 10, defer: () => undefined });
    const slot = fakeSlot('valid-marker');

    expect(() =>
      store.recordTrustedServerOpportunity(null as never, 'auction-slot', 'renderable_candidate')
    ).not.toThrow();
    expect(() =>
      store.recordTrustedServerOpportunity(slot, 'auction-slot', 'invalid' as never)
    ).not.toThrow();
    expect(() => store.recordPrebidRefresh(null as never)).not.toThrow();
    expect(() => store.recordPrebidRefresh([null, 1, slot] as never)).not.toThrow();

    store.recordSlotRequested(slot);
    const cycle = store.snapshot().slots[0].requests[0];
    expect(cycle).toMatchObject({ requestPath: 'prebid_refresh' });
    expect(cycle.trustedServerOpportunity).toBeUndefined();
  });

  it.each(['renderable_candidate', 'unrenderable_candidate'] as const)(
    'moves an explicit non-empty %s to unconfirmed after one deferred notification',
    (opportunity) => {
      let now = 10;
      const deferred: Array<{ callback: () => void; delayMs: number }> = [];
      const store = new GptDiagnosticsStore({
        now: () => now,
        schedule: (callback) => callback(),
        defer: (callback, delayMs) => deferred.push({ callback, delayMs }),
      });
      const listener = vi.fn();
      const slot = fakeSlot(`delivery-${opportunity}`);
      store.subscribe(listener);

      store.recordTrustedServerOpportunity(slot, 'auction-slot', opportunity);
      store.recordSlotRequested(slot);
      expect(deferred, 'recording intent must not defer work').toHaveLength(0);
      listener.mockClear();

      now = 30;
      store.recordSlotRenderEnded(slot, { isEmpty: false });
      expect(store.snapshot().slots[0].requests[0].delivery).toBe('pending');
      expect(deferred).toHaveLength(1);
      expect(deferred[0].delayMs).toBe(TRUSTED_SERVER_ATTRIBUTION_WINDOW_MS);
      expect(listener).toHaveBeenCalledTimes(1);

      now = 30 + TRUSTED_SERVER_ATTRIBUTION_WINDOW_MS;
      deferred[0].callback();
      expect(store.snapshot().slots[0].requests[0].delivery).toBe('candidate_unconfirmed');
      expect(listener).toHaveBeenCalledTimes(2);
      expect(deferred).toHaveLength(1);
    }
  );

  it.each([
    {
      name: 'an explicit no-candidate fill',
      opportunity: 'no_candidate',
      renderFacts: { isEmpty: false },
      expected: 'no_candidate',
    },
    {
      name: 'a fill without a direct opportunity',
      opportunity: undefined,
      renderFacts: { isEmpty: false },
      expected: 'unknown',
    },
    {
      name: 'a render with omitted fill state',
      opportunity: 'renderable_candidate',
      renderFacts: {},
      expected: 'unknown',
    },
    {
      name: 'an empty render',
      opportunity: 'renderable_candidate',
      renderFacts: { isEmpty: true },
      expected: 'not_applicable',
    },
    {
      name: 'a pre-render request',
      opportunity: 'renderable_candidate',
      renderFacts: undefined,
      expected: 'not_applicable',
    },
  ] as const)(
    'derives $name from observed evidence only',
    ({ opportunity, renderFacts, expected }) => {
      let now = 10;
      const deferred: Array<() => void> = [];
      const store = new GptDiagnosticsStore({
        now: () => now,
        defer: (callback) => deferred.push(callback),
      });
      const slot = fakeSlot('delivery-state');

      if (opportunity === undefined) {
        store.recordPrebidRefresh([slot]);
      } else {
        store.recordTrustedServerOpportunity(slot, 'auction-slot', opportunity);
      }
      store.recordSlotRequested(slot);
      deferred.shift()?.();
      now = 30;
      if (renderFacts !== undefined) store.recordSlotRenderEnded(slot, renderFacts);

      expect(store.snapshot().slots[0].requests[0].delivery).toBe(expected);
      expect(deferred, 'should not schedule an attribution-boundary notification').toHaveLength(0);
      now = 30 + TRUSTED_SERVER_ATTRIBUTION_WINDOW_MS;
      expect(store.snapshot().slots[0].requests[0].delivery).toBe(expected);
    }
  );

  it.each([
    { name: 'omitted fill state', facts: {}, expected: undefined },
    { name: 'an empty render', facts: { isEmpty: true }, expected: 'empty' },
    {
      name: 'an explicit backfill',
      facts: { isEmpty: false, isBackfill: true },
      expected: 'backfill',
    },
    {
      name: 'an explicit reservation',
      facts: { isEmpty: false, adManager: { lineItemId: 123 } },
      expected: 'reservation',
    },
    {
      name: 'a source-agnostic identity confirmed as non-backfill',
      facts: {
        isEmpty: false,
        isBackfill: false,
        adManager: { sourceAgnosticLineItemId: 123 },
      },
      expected: 'reservation',
    },
    {
      name: 'a source-agnostic identity without a backfill fact',
      facts: { isEmpty: false, adManager: { sourceAgnosticLineItemId: 123 } },
      expected: 'unclassified_non_empty',
    },
    {
      name: 'an otherwise unclassified non-empty render',
      facts: { isEmpty: false },
      expected: 'unclassified_non_empty',
    },
  ] as const)('classifies $name only from explicit render facts', ({ facts, expected }) => {
    const store = new GptDiagnosticsStore({ now: () => 10 });
    const slot = fakeSlot('response-class');

    store.recordSlotRequested(slot);
    store.recordSlotRenderEnded(slot, facts);

    expect(store.snapshot().slots[0].requests[0].responseClass).toBe(expected);
  });

  it('correlates a creative request and response to the selected request cycle', () => {
    let now = 10;
    const store = new GptDiagnosticsStore({ now: () => now, defer: () => undefined });
    const slot = fakeSlot('creative-selected');
    associateSlot(store, slot, 'auction-selected');
    store.recordSlotRequested(slot);

    now = 20;
    const attemptId = store.recordTrustedServerCreativeRequest('auction-selected');
    expect(attemptId).toEqual(expect.any(Number));
    expect(store.snapshot().slots[0].requests[0]).toMatchObject({
      trustedServerCreativeRequestAtMs: 20,
      delivery: 'trusted_server_selected',
    });

    now = 25;
    store.recordTrustedServerCreativeResponse(attemptId!);
    expect(store.snapshot().slots[0].requests[0]).toMatchObject({
      trustedServerCreativeRequestAtMs: 20,
      trustedServerCreativeResponseAtMs: 25,
      delivery: 'trusted_server_response_sent',
    });
  });

  it('accepts late positive creative evidence after the candidate observation timeout', () => {
    let now = 0;
    const store = new GptDiagnosticsStore({ now: () => now, defer: () => undefined });
    const slot = fakeSlot('late-positive');
    associateSlot(store, slot, 'auction-late');
    store.recordSlotRequested(slot);
    now = 1;
    store.recordSlotRenderEnded(slot, { isEmpty: false });

    now = 1 + TRUSTED_SERVER_ATTRIBUTION_WINDOW_MS;
    expect(store.snapshot().slots[0].requests[0].delivery).toBe('candidate_unconfirmed');

    const attemptId = store.recordTrustedServerCreativeRequest('auction-late');
    expect(attemptId).toEqual(expect.any(Number));
    expect(store.snapshot().slots[0].requests[0].delivery).toBe('trusted_server_selected');
    now += 1;
    store.recordTrustedServerCreativeResponse(attemptId!);
    expect(store.snapshot().slots[0].requests[0].delivery).toBe('trusted_server_response_sent');
  });

  it('keeps the first request timestamp and live ID across duplicate creative requests', () => {
    let now = 1;
    const store = new GptDiagnosticsStore({ now: () => now, defer: () => undefined });
    const slot = fakeSlot('creative-retry');
    associateSlot(store, slot, 'auction-retry');
    store.recordSlotRequested(slot);

    now = 5;
    const firstId = store.recordTrustedServerCreativeRequest('auction-retry');
    now = 9;
    const duplicateId = store.recordTrustedServerCreativeRequest('auction-retry');

    expect(duplicateId).toBe(firstId);
    expect(store.snapshot().slots[0].requests[0].trustedServerCreativeRequestAtMs).toBe(5);
  });

  it('records each safe creative failure once in first-observed order and can later succeed', () => {
    let now = 0;
    const store = new GptDiagnosticsStore({ now: () => now, defer: () => undefined });
    const slot = fakeSlot('creative-failures');
    associateSlot(store, slot, 'auction-failures');
    store.recordSlotRequested(slot);
    const attemptId = store.recordTrustedServerCreativeRequest('auction-failures')!;

    store.recordTrustedServerCreativeFailure(attemptId, 'cache_fetch_failed');
    store.recordTrustedServerCreativeFailure(attemptId, 'missing_render_source');
    store.recordTrustedServerCreativeFailure(attemptId, 'cache_fetch_failed');
    store.recordTrustedServerCreativeFailure(attemptId, 'invalid_cache_payload');
    store.recordTrustedServerCreativeFailure(attemptId, 'response_post_failed');
    store.recordTrustedServerCreativeFailure(attemptId, 'unsafe_runtime_value' as never);

    expect(store.snapshot().slots[0].requests[0].trustedServerCreativeFailures).toEqual([
      'cache_fetch_failed',
      'missing_render_source',
      'invalid_cache_payload',
      'response_post_failed',
    ]);
    expect(store.snapshot().attributionIssues).toEqual([]);

    now = 1;
    store.recordTrustedServerCreativeResponse(attemptId);
    expect(store.snapshot().slots[0].requests[0].delivery).toBe('trusted_server_response_sent');
  });

  it('keeps an asynchronous response on its originating cycle after a newer refresh', () => {
    let now = 0;
    const store = new GptDiagnosticsStore({ now: () => now, defer: () => undefined });
    const slot = fakeSlot('async-origin');
    associateSlot(store, slot, 'auction-async');
    store.recordSlotRequested(slot);
    const firstId = store.recordTrustedServerCreativeRequest('auction-async')!;
    now = 1;
    store.recordSlotRenderEnded(slot, { isEmpty: false });

    now = 2;
    store.recordSlotRequested(slot);
    now = 3;
    store.recordTrustedServerCreativeResponse(firstId);

    const [first, second] = store.snapshot().slots[0].requests;
    expect(first).toMatchObject({
      trustedServerCreativeResponseAtMs: 3,
      delivery: 'trusted_server_response_sent',
    });
    expect(second.trustedServerCreativeResponseAtMs).toBeUndefined();
  });

  it('provisionally attaches an initial pre-render creative request', () => {
    let now = 10;
    const store = new GptDiagnosticsStore({ now: () => now, defer: () => undefined });
    const slot = fakeSlot('provisional');
    associateSlot(store, slot, 'auction-provisional');
    store.recordSlotRequested(slot);

    now = 11;
    const attemptId = store.recordTrustedServerCreativeRequest('auction-provisional');
    expect(attemptId).toEqual(expect.any(Number));
    now = 12;
    store.recordSlotRenderEnded(slot, { isEmpty: false });

    expect(store.snapshot().slots[0].requests[0]).toMatchObject({
      trustedServerCreativeRequestAtMs: 11,
      isEmpty: false,
      delivery: 'trusted_server_selected',
    });
    expect(store.snapshot().attributionIssues).toEqual([]);
  });

  it('rejects an ambiguous pre-render request when an earlier non-empty cycle is retained', () => {
    let now = 0;
    const store = new GptDiagnosticsStore({ now: () => now, defer: () => undefined });
    const slot = fakeSlot('ambiguous-creative');
    associateSlot(store, slot, 'auction-ambiguous');
    store.recordSlotRequested(slot);
    now = 1;
    store.recordSlotRenderEnded(slot, { isEmpty: false });
    now = 2;
    store.recordSlotRequested(slot);

    now = 3;
    expect(store.recordTrustedServerCreativeRequest('auction-ambiguous')).toBeUndefined();
    expect(store.snapshot().slots[0].requests[1].trustedServerCreativeRequestAtMs).toBeUndefined();
    expect(store.snapshot().attributionIssues).toEqual([
      expect.objectContaining({
        reason: 'creative_request_ambiguous_cycle',
        runtimeSlotNumber: 1,
        slotElementId: 'ambiguous-creative',
      }),
    ]);
  });

  it('accepts positive creative evidence when GPT omitted isEmpty', () => {
    let now = 0;
    const store = new GptDiagnosticsStore({ now: () => now, defer: () => undefined });
    const slot = fakeSlot('unknown-fill-positive');
    associateSlot(store, slot, 'auction-unknown-fill');
    store.recordSlotRequested(slot);
    now = 1;
    store.recordSlotRenderEnded(slot, {});
    now = 2;

    expect(store.recordTrustedServerCreativeRequest('auction-unknown-fill')).toEqual(
      expect.any(Number)
    );
    expect(store.snapshot().slots[0].requests[0].delivery).toBe('trusted_server_selected');
  });

  it('rejects explicit empty cycles and never falls back to an older compatible cycle', () => {
    let now = 0;
    const store = new GptDiagnosticsStore({ now: () => now, defer: () => undefined });
    const slot = fakeSlot('empty-current');
    associateSlot(store, slot, 'auction-empty-current');
    store.recordSlotRequested(slot);
    now = 1;
    store.recordSlotRenderEnded(slot, { isEmpty: false });
    now = 2;
    store.recordSlotRequested(slot);
    now = 3;
    store.recordSlotRenderEnded(slot, { isEmpty: true });

    expect(store.recordTrustedServerCreativeRequest('auction-empty-current')).toBeUndefined();
    const [older, current] = store.snapshot().slots[0].requests;
    expect(older.trustedServerCreativeRequestAtMs).toBeUndefined();
    expect(current.trustedServerCreativeRequestAtMs).toBeUndefined();
    expect(last(store.snapshot().attributionIssues)?.reason).toBe('creative_request_without_cycle');
  });

  it('preserves provisional evidence and reports when the cycle later renders empty', () => {
    let now = 0;
    const store = new GptDiagnosticsStore({ now: () => now, defer: () => undefined });
    const slot = fakeSlot('provisional-empty');
    associateSlot(store, slot, 'auction-provisional-empty');
    store.recordSlotRequested(slot);
    const attemptId = store.recordTrustedServerCreativeRequest('auction-provisional-empty');
    now = 1;
    store.recordSlotRenderEnded(slot, { isEmpty: true });

    expect(attemptId).toEqual(expect.any(Number));
    expect(store.snapshot().slots[0].requests[0].trustedServerCreativeRequestAtMs).toBe(0);
    expect(store.snapshot().attributionIssues).toEqual([
      expect.objectContaining({
        reason: 'creative_request_on_empty_cycle',
        runtimeSlotNumber: 1,
        slotElementId: 'provisional-empty',
      }),
    ]);

    // The attempt is dead once its cycle rendered empty, so a late response
    // cannot claim a Trusted Server delivery against that empty render.
    store.recordTrustedServerCreativeResponse(attemptId!);
    const cycle = store.snapshot().slots[0].requests[0];
    expect(cycle.trustedServerCreativeResponseAtMs).toBeUndefined();
    expect(cycle.delivery, 'an empty cycle must not report a markup response').toBe(
      'trusted_server_selected'
    );
    expect(last(store.snapshot().attributionIssues)?.reason).toBe('creative_attempt_evicted');
  });

  it('preserves provisional evidence when the render omits isEmpty', () => {
    let now = 0;
    const store = new GptDiagnosticsStore({ now: () => now, defer: () => undefined });
    const slot = fakeSlot('provisional-unknown-fill');
    associateSlot(store, slot, 'auction-provisional-unknown-fill');
    store.recordSlotRequested(slot);
    const attemptId = store.recordTrustedServerCreativeRequest('auction-provisional-unknown-fill');

    now = 1;
    store.recordSlotRenderEnded(slot, {});

    expect(attemptId).toEqual(expect.any(Number));
    expect(store.snapshot().slots[0].requests[0]).toMatchObject({
      trustedServerCreativeRequestAtMs: 0,
      delivery: 'trusted_server_selected',
    });
    expect(store.snapshot().attributionIssues).toEqual([]);
  });

  it('admits a request at the cycle-age boundary and rejects only after it', () => {
    let boundaryNow = 0;
    const boundaryStore = new GptDiagnosticsStore({
      now: () => boundaryNow,
      defer: () => undefined,
    });
    const boundarySlot = fakeSlot('cycle-boundary');
    associateSlot(boundaryStore, boundarySlot, 'auction-boundary');
    boundaryStore.recordSlotRequested(boundarySlot);
    boundaryNow = CREATIVE_ATTEMPT_WINDOW_MS;
    expect(boundaryStore.recordTrustedServerCreativeRequest('auction-boundary')).toEqual(
      expect.any(Number)
    );

    let lateNow = 0;
    const lateStore = new GptDiagnosticsStore({ now: () => lateNow, defer: () => undefined });
    const lateSlot = fakeSlot('cycle-too-old');
    associateSlot(lateStore, lateSlot, 'auction-too-old');
    lateStore.recordSlotRequested(lateSlot);
    lateNow = CREATIVE_ATTEMPT_WINDOW_MS + 1;
    expect(lateStore.recordTrustedServerCreativeRequest('auction-too-old')).toBeUndefined();
    expect(last(lateStore.snapshot().attributionIssues)?.reason).toBe(
      'creative_request_without_cycle'
    );
  });

  it('distinguishes missing slot associations from known slots without a request cycle', () => {
    const store = new GptDiagnosticsStore({ now: () => 10, defer: () => undefined });
    const associated = fakeSlot('associated-no-cycle');
    associateSlot(store, associated, 'auction-no-cycle');

    expect(store.recordTrustedServerCreativeRequest('auction-unknown')).toBeUndefined();
    expect(store.recordTrustedServerCreativeRequest('')).toBeUndefined();
    expect(store.recordTrustedServerCreativeRequest('auction-no-cycle')).toBeUndefined();

    const issues = store.snapshot().attributionIssues;
    expect(issues.map((issue) => issue.reason)).toEqual([
      'creative_request_without_slot',
      'creative_request_without_slot',
      'creative_request_without_cycle',
    ]);
    expect(issues[0].runtimeSlotNumber).toBeUndefined();
    expect(issues[0].slotElementId).toBeUndefined();
    expect(issues[2].slotElementId).toBe('associated-no-cycle');
  });

  it('expires attempts at 30 seconds without replacement or late mutation', () => {
    let now = 0;
    const store = new GptDiagnosticsStore({ now: () => now, defer: () => undefined });
    const slot = fakeSlot('attempt-expiry');
    associateSlot(store, slot, 'auction-expiry');
    store.recordSlotRequested(slot);
    const attemptId = store.recordTrustedServerCreativeRequest('auction-expiry')!;

    now = CREATIVE_ATTEMPT_WINDOW_MS - 1;
    expect(store.recordTrustedServerCreativeRequest('auction-expiry')).toBe(attemptId);
    now = CREATIVE_ATTEMPT_WINDOW_MS;
    expect(store.recordTrustedServerCreativeRequest('auction-expiry')).toBeUndefined();
    store.recordTrustedServerCreativeResponse(attemptId);
    store.recordTrustedServerCreativeFailure(attemptId, 'cache_fetch_failed');

    const cycle = store.snapshot().slots[0].requests[0];
    expect(cycle).toMatchObject({ trustedServerCreativeRequestAtMs: 0 });
    expect(cycle.trustedServerCreativeResponseAtMs).toBeUndefined();
    expect(cycle.trustedServerCreativeFailures).toBeUndefined();
    expect(store.snapshot().attributionIssues.map((issue) => issue.reason)).toEqual([
      'creative_attempt_expired',
      'creative_attempt_expired',
      'creative_attempt_expired',
    ]);
  });

  it('reuses a live attempt after the cycle ages out and expires from creative-request time', () => {
    let now = 0;
    const store = new GptDiagnosticsStore({ now: () => now, defer: () => undefined });
    const slot = fakeSlot('delayed-attempt-expiry');
    associateSlot(store, slot, 'auction-delayed-attempt-expiry');
    store.recordSlotRequested(slot);

    now = 20_000;
    const attemptId = store.recordTrustedServerCreativeRequest('auction-delayed-attempt-expiry');
    expect(attemptId).toEqual(expect.any(Number));

    now = CREATIVE_ATTEMPT_WINDOW_MS + 1;
    expect(store.recordTrustedServerCreativeRequest('auction-delayed-attempt-expiry')).toBe(
      attemptId
    );
    expect(store.snapshot().slots[0].requests[0].trustedServerCreativeRequestAtMs).toBe(20_000);

    now = 20_000 + CREATIVE_ATTEMPT_WINDOW_MS - 1;
    expect(store.recordTrustedServerCreativeRequest('auction-delayed-attempt-expiry')).toBe(
      attemptId
    );
    now = 20_000 + CREATIVE_ATTEMPT_WINDOW_MS;
    expect(
      store.recordTrustedServerCreativeRequest('auction-delayed-attempt-expiry')
    ).toBeUndefined();
    expect(last(store.snapshot().attributionIssues)?.reason).toBe('creative_attempt_expired');
    expect(store.snapshot().slots[0].requests[0].trustedServerCreativeRequestAtMs).toBe(20_000);
  });

  it('reports unknown IDs and invalidates live attempts on cycle and slot eviction', () => {
    const now = 0;
    const store = new GptDiagnosticsStore({ now: () => now, defer: () => undefined });
    store.recordTrustedServerCreativeResponse(999_999);

    const shiftedSlot = fakeSlot('shifted-attempt');
    associateSlot(store, shiftedSlot, 'auction-shifted');
    store.recordSlotRequested(shiftedSlot);
    const shiftedId = store.recordTrustedServerCreativeRequest('auction-shifted')!;
    for (let index = 0; index < MAX_REQUEST_CYCLES_PER_SLOT; index += 1) {
      store.recordSlotRequested(shiftedSlot);
    }
    store.recordTrustedServerCreativeResponse(shiftedId);

    const evictedSlot = fakeSlot('lru-attempt');
    associateSlot(store, evictedSlot, 'auction-lru-attempt');
    store.recordSlotRequested(evictedSlot);
    const evictedId = store.recordTrustedServerCreativeRequest('auction-lru-attempt')!;
    for (let index = 0; index < MAX_DIAGNOSTIC_SLOTS; index += 1) {
      store.recordSlotRequested(fakeSlot(`attempt-lru-filler-${index}`));
    }
    store.recordTrustedServerCreativeFailure(evictedId, 'response_post_failed');

    expect(store.snapshot().attributionIssues.map((issue) => issue.reason)).toEqual([
      'creative_attempt_unknown',
      'creative_attempt_evicted',
      'creative_attempt_evicted',
    ]);
  });

  it('treats duplicate writers against a completed attempt as idempotent', () => {
    let now = 0;
    const store = new GptDiagnosticsStore({ now: () => now, defer: () => undefined });
    const slot = fakeSlot('completed-attempt');
    associateSlot(store, slot, 'auction-completed');
    store.recordSlotRequested(slot);
    const attemptId = store.recordTrustedServerCreativeRequest('auction-completed')!;
    now = 1;
    store.recordTrustedServerCreativeResponse(attemptId);
    const completed = store.snapshot();

    now = 2;
    expect(store.recordTrustedServerCreativeRequest('auction-completed')).toBeUndefined();
    store.recordTrustedServerCreativeResponse(attemptId);
    store.recordTrustedServerCreativeFailure(attemptId, 'cache_fetch_failed');

    expect(store.snapshot().slots[0].requests[0]).toEqual(completed.slots[0].requests[0]);
    expect(store.snapshot().attributionIssues).toEqual([]);
  });

  it('does not replace a completed current-cycle attempt after its tombstone is reclaimed', () => {
    const now = 0;
    const store = new GptDiagnosticsStore({ now: () => now, defer: () => undefined });
    const sentinelSlot = fakeSlot('completed-current-cycle');
    associateSlot(store, sentinelSlot, 'auction-completed-current-cycle');
    store.recordSlotRequested(sentinelSlot);
    const sentinelId = store.recordTrustedServerCreativeRequest('auction-completed-current-cycle')!;
    store.recordTrustedServerCreativeResponse(sentinelId);

    recordCompletedAttempts(store, MAX_CREATIVE_ATTEMPTS - 1, 'completed-current-cycle-fill');
    const replacementSlot = fakeSlot('completed-current-cycle-replacement');
    associateSlot(store, replacementSlot, 'auction-completed-current-cycle-replacement');
    store.recordSlotRequested(replacementSlot);
    expect(
      store.recordTrustedServerCreativeRequest('auction-completed-current-cycle-replacement')
    ).toEqual(expect.any(Number));

    expect(
      store.recordTrustedServerCreativeRequest('auction-completed-current-cycle')
    ).toBeUndefined();
    expect(
      store.recordTrustedServerCreativeRequest('auction-completed-current-cycle')
    ).toBeUndefined();
    expect(store.snapshot().attributionIssues).toEqual([]);
    expect(
      store.snapshot().slots.find((slot) => slot.slotElementId === 'completed-current-cycle')
        ?.requests[0]
    ).toMatchObject({
      trustedServerCreativeRequestAtMs: 0,
      trustedServerCreativeResponseAtMs: 0,
    });
  });

  it('does not replace an expired current-cycle attempt after its tombstone is reclaimed', () => {
    let now = 0;
    const store = new GptDiagnosticsStore({ now: () => now, defer: () => undefined });
    const sentinelSlot = fakeSlot('expired-current-cycle');
    associateSlot(store, sentinelSlot, 'auction-expired-current-cycle');
    store.recordSlotRequested(sentinelSlot);
    expect(store.recordTrustedServerCreativeRequest('auction-expired-current-cycle')).toEqual(
      expect.any(Number)
    );

    now = CREATIVE_ATTEMPT_WINDOW_MS;
    recordCompletedAttempts(store, MAX_CREATIVE_ATTEMPTS - 1, 'expired-current-cycle-fill');
    const replacementSlot = fakeSlot('expired-current-cycle-replacement');
    associateSlot(store, replacementSlot, 'auction-expired-current-cycle-replacement');
    store.recordSlotRequested(replacementSlot);
    expect(
      store.recordTrustedServerCreativeRequest('auction-expired-current-cycle-replacement')
    ).toEqual(expect.any(Number));

    expect(
      store.recordTrustedServerCreativeRequest('auction-expired-current-cycle')
    ).toBeUndefined();
    expect(
      store.recordTrustedServerCreativeRequest('auction-expired-current-cycle')
    ).toBeUndefined();
    expect(store.snapshot().attributionIssues.map((issue) => issue.reason)).toEqual([
      'creative_attempt_unknown',
      'creative_attempt_unknown',
    ]);
    expect(
      store.snapshot().slots.find((slot) => slot.slotElementId === 'expired-current-cycle')
        ?.requests[0]
    ).toMatchObject({ trustedServerCreativeRequestAtMs: 0 });
  });

  it('never evicts a live attempt at capacity and lets an unassigned duplicate retry', () => {
    let now = 100;
    const store = new GptDiagnosticsStore({ now: () => now, defer: () => undefined });
    const liveIds: number[] = [];
    let created = 0;

    for (let slotIndex = 0; created < MAX_CREATIVE_ATTEMPTS; slotIndex += 1) {
      const slot = fakeSlot(`live-capacity-${slotIndex}`);
      const auctionSlotId = `auction-live-capacity-${slotIndex}`;
      associateSlot(store, slot, auctionSlotId);
      const cycles = Math.min(MAX_REQUEST_CYCLES_PER_SLOT, MAX_CREATIVE_ATTEMPTS - created);
      for (let cycleIndex = 0; cycleIndex < cycles; cycleIndex += 1) {
        store.recordSlotRequested(slot);
        liveIds.push(store.recordTrustedServerCreativeRequest(auctionSlotId)!);
        created += 1;
      }
    }

    const rejectedSlot = fakeSlot('live-capacity-rejected');
    associateSlot(store, rejectedSlot, 'auction-live-capacity-rejected');
    store.recordSlotRequested(rejectedSlot);
    expect(
      store.recordTrustedServerCreativeRequest('auction-live-capacity-rejected')
    ).toBeUndefined();
    expect(last(store.snapshot().slots)?.requests[0].trustedServerCreativeRequestAtMs).toBe(100);
    expect(last(store.snapshot().attributionIssues)?.reason).toBe('creative_attempt_capacity');

    now = 250;
    store.recordTrustedServerCreativeResponse(liveIds[0]);
    const retriedId = store.recordTrustedServerCreativeRequest('auction-live-capacity-rejected');
    expect(retriedId).toEqual(expect.any(Number));
    expect(retriedId).not.toBe(liveIds[0]);
    expect(last(store.snapshot().slots)?.requests[0].trustedServerCreativeRequestAtMs).toBe(100);
    store.recordTrustedServerCreativeResponse(liveIds[1]);
    expect(last(store.snapshot().attributionIssues)?.reason).toBe('creative_attempt_capacity');
  });

  it('does not create an already-expired attempt when a capacity retry reaches its boundary', () => {
    let now = 100;
    const store = new GptDiagnosticsStore({ now: () => now, defer: () => undefined });
    let created = 0;

    for (let slotIndex = 0; created < MAX_CREATIVE_ATTEMPTS; slotIndex += 1) {
      const slot = fakeSlot(`boundary-capacity-${slotIndex}`);
      const auctionSlotId = `auction-boundary-capacity-${slotIndex}`;
      associateSlot(store, slot, auctionSlotId);
      const cycles = Math.min(MAX_REQUEST_CYCLES_PER_SLOT, MAX_CREATIVE_ATTEMPTS - created);
      for (let cycleIndex = 0; cycleIndex < cycles; cycleIndex += 1) {
        store.recordSlotRequested(slot);
        expect(store.recordTrustedServerCreativeRequest(auctionSlotId)).toEqual(expect.any(Number));
        created += 1;
      }
    }

    const rejectedSlot = fakeSlot('boundary-capacity-rejected');
    const rejectedAuctionSlotId = 'auction-boundary-capacity-rejected';
    associateSlot(store, rejectedSlot, rejectedAuctionSlotId);
    store.recordSlotRequested(rejectedSlot);
    expect(store.recordTrustedServerCreativeRequest(rejectedAuctionSlotId)).toBeUndefined();

    now += CREATIVE_ATTEMPT_WINDOW_MS;
    expect(store.recordTrustedServerCreativeRequest(rejectedAuctionSlotId)).toBeUndefined();
    const snapshot = store.snapshot();
    const rejectedCycle = snapshot.slots.find(
      (slot) => slot.slotElementId === 'boundary-capacity-rejected'
    )?.requests[0];
    expect(rejectedCycle?.trustedServerCreativeRequestAtMs).toBe(100);
    expect(snapshot.attributionIssues.map((issue) => issue.reason)).toEqual([
      'creative_attempt_capacity',
      'creative_attempt_expired',
    ]);
  });

  it('bounds attribution issues separately without changing callback coverage', () => {
    const store = new GptDiagnosticsStore({ now: () => 1, defer: () => undefined });
    const beforeCoverage = store.snapshot().coverage;

    for (let index = 0; index < MAX_ATTRIBUTION_ISSUES + 1; index += 1) {
      store.recordTrustedServerCreativeResponse(10_000 + index);
    }

    const snapshot = store.snapshot();
    expect(snapshot.attributionIssues).toHaveLength(MAX_ATTRIBUTION_ISSUES);
    expect(snapshot.metadata.droppedAttributionIssues).toBe(1);
    expect(snapshot.callbackIssues).toEqual([]);
    expect(snapshot.metadata.droppedCallbacks).toBe(0);
    expect(snapshot.coverage).toEqual(beforeCoverage);
    assertCoverageEquation(store);
  });

  it('returns detached creative evidence and never exports attempt bookkeeping', () => {
    const now = 0;
    const store = new GptDiagnosticsStore({ now: () => now, defer: () => undefined });
    const slot = fakeSlot('detached-creative');
    associateSlot(store, slot, 'auction-detached-creative');
    store.recordSlotRequested(slot);
    store.recordSlotRenderEnded(slot, {
      isEmpty: false,
      adManager: { creativeId: 123, yieldGroupIds: [11], companyIds: [22] },
    });
    const attemptId = store.recordTrustedServerCreativeRequest('auction-detached-creative')!;
    store.recordTrustedServerCreativeFailure(attemptId, 'cache_fetch_failed');
    store.recordTrustedServerCreativeResponse(999_999);

    const first = store.snapshot();
    first.slots[0].requests[0].trustedServerCreativeFailures!.push('response_post_failed');
    first.slots[0].requests[0].adManager!.yieldGroupIds!.push(33);
    first.slots[0].requests[0].adManager!.companyIds!.push(44);
    first.attributionIssues[0].reason = 'creative_attempt_capacity';

    const second = store.snapshot();
    expect(second.slots[0].requests[0].trustedServerCreativeFailures).toEqual([
      'cache_fetch_failed',
    ]);
    expect(second.slots[0].requests[0].adManager).toMatchObject({
      creativeId: 123,
      yieldGroupIds: [11],
      companyIds: [22],
    });
    expect(second.attributionIssues[0].reason).toBe('creative_attempt_unknown');
    const serializedCycle = JSON.stringify(second.slots[0].requests[0]);
    expect(serializedCycle).not.toMatch(
      /"(?:id|status|expiresAtMs|provisionalBeforeRender|auctionSlotId|attemptId|attemptStatus)"\s*:/
    );
  });

  it('returns detached snapshot data', () => {
    const store = new GptDiagnosticsStore({ now: () => 1 });
    const slot = fakeSlot('detached');
    store.recordSlotRequested(slot);

    const first = store.snapshot();
    first.slots[0].requests[0].requestNumber = 999;
    first.coverage.slotRequested.matched = 999;

    const second = store.snapshot();
    expect(second.slots[0].requests[0].requestNumber).toBe(1);
    expect(second.coverage.slotRequested.matched).toBe(1);
  });
  it('ignores malformed publisher refresh inputs without recording intent', () => {
    const store = new GptDiagnosticsStore({ now: () => 10, defer: () => undefined });
    const slot = fakeSlot('publisher-refresh-malformed');

    expect(() => store.recordPublisherRefresh(null as never)).not.toThrow();
    expect(() => store.recordPublisherRefresh('slots' as never)).not.toThrow();
    expect(() => store.recordPublisherRefresh([null, 7, undefined, slot] as never)).not.toThrow();

    store.recordSlotRequested(slot);
    expect(store.snapshot().slots[0].requests[0].requestPath).toBe('publisher_refresh');
    expect(store.snapshot().slots).toHaveLength(1);
  });

  it('trims the oldest auction-slot association beyond the retention bound', () => {
    const store = new GptDiagnosticsStore({ now: () => 10, defer: () => undefined });
    const oldest = fakeSlot('association-oldest');
    associateSlot(store, oldest, 'auction-oldest');
    for (let index = 0; index < MAX_TRUSTED_SERVER_ASSOCIATIONS; index += 1) {
      associateSlot(store, fakeSlot(`association-${index}`), `auction-${index}`);
    }
    store.recordSlotRequested(oldest);

    expect(store.recordTrustedServerCreativeRequest('auction-oldest')).toBeUndefined();
    expect(last(store.snapshot().attributionIssues)?.reason).toBe('creative_request_without_slot');
    expect(store.recordTrustedServerCreativeRequest('auction-0')).toBeUndefined();
    expect(last(store.snapshot().attributionIssues)?.reason).toBe('creative_request_without_cycle');
  });

  it.each([
    {
      name: 'a render that precedes its response',
      kind: 'slotRenderEnded',
      record: (store: GptDiagnosticsStore, slot: GptDiagnosticsSlotLike) =>
        store.recordSlotRenderEnded(slot, { isEmpty: false }),
      arrange: (store: GptDiagnosticsStore, slot: GptDiagnosticsSlotLike) =>
        store.recordSlotResponseReceived(slot),
    },
    {
      name: 'a load that precedes its render',
      kind: 'slotOnload',
      record: (store: GptDiagnosticsStore, slot: GptDiagnosticsSlotLike) =>
        store.recordSlotOnload(slot),
      arrange: (store: GptDiagnosticsStore, slot: GptDiagnosticsSlotLike) => {
        store.recordSlotResponseReceived(slot);
        store.recordSlotRenderEnded(slot, { isEmpty: false });
      },
    },
    {
      name: 'a viewable impression that precedes its render',
      kind: 'impressionViewable',
      record: (store: GptDiagnosticsStore, slot: GptDiagnosticsSlotLike) =>
        store.recordImpressionViewable(slot),
      arrange: (store: GptDiagnosticsStore, slot: GptDiagnosticsSlotLike) => {
        store.recordSlotResponseReceived(slot);
        store.recordSlotRenderEnded(slot, { isEmpty: false });
      },
    },
  ])('reports $name as an invalid event order', ({ kind, record, arrange }) => {
    let now = 100;
    const store = new GptDiagnosticsStore({ now: () => now, defer: () => undefined });
    const slot = fakeSlot(`out-of-order-${kind}`);

    store.recordSlotRequested(slot);
    arrange(store, slot);
    // A backwards clock is the only way GPT can report a later callback with an
    // earlier timestamp; diagnostics record the contradiction rather than hide it.
    now = 1;
    record(store, slot);

    expect(store.snapshot().slots[0].requests[0].incompleteSequence).toBe(true);
    expect(store.snapshot().callbackIssues).toContainEqual(
      expect.objectContaining({ kind, disposition: 'matched', reason: 'invalid_event_order' })
    );
    assertCoverageEquation(store);
  });

  it.each([Number.NaN, Number.POSITIVE_INFINITY])(
    'rejects the non-finite visibility percentage %s',
    (percentage) => {
      const store = new GptDiagnosticsStore({ now: () => 10, defer: () => undefined });
      const slot = fakeSlot('visibility-non-finite');

      store.recordSlotRequested(slot);
      store.recordSlotVisibilityChanged(slot, percentage);

      const snapshot = store.snapshot();
      expect(snapshot.slots[0].currentVisibilityPercentage).toBeUndefined();
      expect(snapshot.slots[0].maximumVisibilityPercentage).toBeUndefined();
      expect(snapshot.callbackIssues).toContainEqual(
        expect.objectContaining({
          kind: 'slotVisibilityChanged',
          disposition: 'unmatched',
          reason: 'invalid_visibility_percentage',
        })
      );
      assertCoverageEquation(store);
    }
  );

  it('reports an unknown attempt ID for a creative failure without recording one', () => {
    const store = new GptDiagnosticsStore({ now: () => 10, defer: () => undefined });
    const slot = fakeSlot('failure-unknown-attempt');
    associateSlot(store, slot, 'auction-failure-unknown');
    store.recordSlotRequested(slot);

    store.recordTrustedServerCreativeFailure(4242, 'cache_fetch_failed');

    expect(store.snapshot().slots[0].requests[0].trustedServerCreativeFailures).toBeUndefined();
    expect(last(store.snapshot().attributionIssues)?.reason).toBe('creative_attempt_unknown');
  });

  it('keeps one outstanding delivery-boundary timer across a refresh burst', () => {
    let now = 0;
    const deferred: Array<{ callback: () => void; delayMs: number }> = [];
    const store = new GptDiagnosticsStore({
      now: () => now,
      schedule: (callback) => callback(),
      defer: (callback, delayMs) => deferred.push({ callback, delayMs }),
    });
    const slots = Array.from({ length: 8 }, (_, index) => fakeSlot(`burst-${index}`));

    for (const [index, slot] of slots.entries()) {
      now = index;
      store.recordTrustedServerOpportunity(slot, `burst-auction-${index}`, 'renderable_candidate');
      store.recordSlotRequested(slot);
      store.recordSlotRenderEnded(slot, { isEmpty: false });
    }

    expect(deferred, 'a burst of candidate renders must share one timer').toMatchObject([
      { delayMs: TRUSTED_SERVER_ATTRIBUTION_WINDOW_MS },
    ]);

    // Firing at the earliest deadline re-arms once for the next one, never per render.
    now = TRUSTED_SERVER_ATTRIBUTION_WINDOW_MS;
    deferred.shift()!.callback();
    expect(deferred).toMatchObject([{ delayMs: 1 }]);

    now = 7 + TRUSTED_SERVER_ATTRIBUTION_WINDOW_MS;
    deferred.shift()!.callback();
    expect(deferred, 'no boundary remains once every candidate crossed it').toHaveLength(0);
    for (const slot of store.snapshot().slots) {
      expect(slot.requests[0].delivery).toBe('candidate_unconfirmed');
    }
  });
});
