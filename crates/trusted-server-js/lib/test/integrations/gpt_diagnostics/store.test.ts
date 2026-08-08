import { describe, expect, it, vi } from 'vitest';

import {
  GptDiagnosticsStore,
  MAX_CALLBACK_ISSUES,
  MAX_DIAGNOSTIC_SLOTS,
  MAX_REQUEST_CYCLES_PER_SLOT,
  type GptDiagnosticsSlotLike,
} from '../../../src/integrations/gpt_diagnostics/store';

function fakeSlot(elementId: string, adUnitPath = `/example/${elementId}`): GptDiagnosticsSlotLike {
  return {
    getSlotElementId: () => elementId,
    getAdUnitPath: () => adUnitPath,
  };
}

function assertCoverageEquation(store: GptDiagnosticsStore): void {
  for (const counters of Object.values(store.snapshot().coverage)) {
    expect(counters.observed).toBe(counters.matched + counters.unmatched + counters.ambiguous);
  }
}

describe('GptDiagnosticsStore', () => {
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
    const recordedSlot = snapshot.slots[0]!;
    const cycle = recordedSlot.requests[0]!;

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

  it('uses adapter callback times even when buffered delivery occurs much later', () => {
    const store = new GptDiagnosticsStore({ now: () => 9_999 });
    const slot = fakeSlot('buffered-slot');

    store.recordSlotRequested(slot, 10);
    store.recordSlotResponseReceived(slot, 25);
    store.recordSlotRenderEnded(slot, { isEmpty: false }, 30);

    expect(store.snapshot().slots[0]?.requests[0]).toMatchObject({
      requestedAtMs: 10,
      responseAtMs: 25,
      renderAtMs: 30,
      durations: { requestToResponseMs: 15, responseToRenderMs: 5, requestToRenderMs: 20 },
    });
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
    expect(emptyCycle!.loadAtMs).toBeUndefined();
    expect(emptyCycle!.viewableAtMs).toBeUndefined();
    expect(store.snapshot().coverage.slotOnload).toMatchObject({ matched: 1, unmatched: 1 });
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

    expect(requestingCycle!.incompleteSequence).toBe(false);
    expect(requestingCycle!.responseAtMs).toBeUndefined();
    expect(respondedCycle!.incompleteSequence).toBe(false);
    expect(respondedCycle!.renderAtMs).toBeUndefined();
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

    expect(store.snapshot().slots[0]!.requests.map((cycle) => cycle.requestNumber)).toEqual([
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
    expect(store.snapshot().slots[0]!.slotElementId).toBeUndefined();
    expect(store.snapshot().slots[0]!.adUnitPath).toBeUndefined();
  });

  it('records callbacks without a request as unmatched issues', () => {
    const store = new GptDiagnosticsStore({ now: () => 12 });
    const slot = fakeSlot('unrequested');

    store.recordSlotResponseReceived(slot);
    store.recordSlotRenderEnded(slot, { isEmpty: false });
    store.recordSlotOnload(slot);
    store.recordImpressionViewable(slot);

    const snapshot = store.snapshot();
    expect(snapshot.slots[0]!.requests).toEqual([]);
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
    expect(snapshot.slots[0]!.requests).toHaveLength(2);
    expect(snapshot.slots[0]!.requests.every((cycle) => cycle.responseAtMs === undefined)).toBe(
      true
    );
    expect(snapshot.slots[0]!.requests.every((cycle) => cycle.renderAtMs === undefined)).toBe(true);
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
    const cycle = snapshot.slots[0]!.requests[0]!;
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
    expect(snapshot.slots[0]!.runtimeSlotNumber).toBe(2);
    expect(snapshot.metadata.evictedSlots).toBe(1);

    store.recordSlotResponseReceived(slots[0]!);
    snapshot = store.snapshot();
    expect(snapshot.callbackIssues[snapshot.callbackIssues.length - 1]).toMatchObject({
      runtimeSlotNumber: 1,
      disposition: 'unmatched',
      reason: 'evicted_slot',
    });

    const retainedSlot = slots[slots.length - 1]!;
    for (let index = 0; index < MAX_REQUEST_CYCLES_PER_SLOT; index += 1) {
      store.recordSlotRequested(retainedSlot);
    }
    snapshot = store.snapshot();
    const retainedRecord = snapshot.slots[snapshot.slots.length - 1]!;
    expect(retainedRecord.requests).toHaveLength(MAX_REQUEST_CYCLES_PER_SLOT);
    expect(retainedRecord.requests[0]!.requestNumber).toBe(2);
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

    store.recordSlotVisibilityChanged(slots[0]!, 10);
    store.recordSlotRequested(slots[MAX_DIAGNOSTIC_SLOTS]!);
    expect(store.snapshot().slots.some((slot) => slot.runtimeSlotNumber === 1)).toBe(true);
    expect(store.snapshot().slots.some((slot) => slot.runtimeSlotNumber === 2)).toBe(false);

    store.recordSlotResponseReceived(slots[1]!);
    expect(store.snapshot().callbackIssues.slice(-1)[0]).toMatchObject({
      runtimeSlotNumber: 2,
      reason: 'evicted_slot',
    });

    store.recordSlotRequested(slots[1]!);
    store.recordSlotResponseReceived(slots[1]!);
    const reentered = store.snapshot().slots.find((slot) => slot.slotElementId === 'lru-1');
    expect(reentered).toMatchObject({ runtimeSlotNumber: 66 });
    expect(reentered?.requests[0]).toMatchObject({ requestNumber: 2 });
    expect(reentered?.requests[0]!.responseAtMs).toBeDefined();
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
    inputs[0]!.slotElementId = 'changed';
    expect(store.bindingInputs()[0]!.slotElementId).toBe('first');
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

  it('announces correctness commits synchronously while coalescing presentation work', () => {
    const scheduled: Array<() => void> = [];
    const store = new GptDiagnosticsStore({
      now: () => 1,
      schedule: (callback) => scheduled.push(callback),
    });
    const commitListener = vi.fn();
    const presentationListener = vi.fn();
    store.subscribeCommits(commitListener);
    store.subscribe(presentationListener);

    store.markGptObserved();
    store.recordSlotRequested(fakeSlot('commit-membership'));

    expect(commitListener).toHaveBeenCalledTimes(2);
    expect(presentationListener).not.toHaveBeenCalled();
    expect(scheduled).toHaveLength(1);
    scheduled.shift()?.();
    expect(presentationListener).toHaveBeenCalledOnce();
  });

  it('returns detached snapshot data', () => {
    const store = new GptDiagnosticsStore({ now: () => 1 });
    const slot = fakeSlot('detached');
    store.recordSlotRequested(slot);

    const first = store.snapshot();
    first.slots[0]!.requests[0]!.requestNumber = 999;
    first.coverage.slotRequested.matched = 999;

    const second = store.snapshot();
    expect(second.slots[0]!.requests[0]!.requestNumber).toBe(1);
    expect(second.coverage.slotRequested.matched).toBe(1);
  });
});
