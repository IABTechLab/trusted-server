import { beforeEach, describe, expect, it, vi } from 'vitest';

import type { GptDiagnosticsBinding } from '../../../src/core/types';
import { GptDiagnosticsApiController } from '../../../src/integrations/gpt_diagnostics/api';
import { GptDiagnosticsStore } from '../../../src/integrations/gpt_diagnostics/store';

class FakeBindings {
  private readonly listeners = new Set<() => void>();

  exportBinding(_runtimeSlotNumber: number): GptDiagnosticsBinding {
    return { status: 'bound' };
  }

  subscribe(listener: () => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  emit(): void {
    for (const listener of this.listeners) listener();
  }
}

function fakeSlot() {
  return {
    getSlotElementId: () => 'ad-slot-example',
    getAdUnitPath: () => '/example/site/banner',
  };
}

function readBlob(blob: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.addEventListener('load', () => resolve(String(reader.result)));
    reader.addEventListener('error', () => reject(reader.error));
    reader.readAsText(blob);
  });
}

function emptyCoverage() {
  return {
    slotRequested: { observed: 0, matched: 0, unmatched: 0, ambiguous: 0 },
    slotResponseReceived: { observed: 0, matched: 0, unmatched: 0, ambiguous: 0 },
    slotRenderEnded: { observed: 0, matched: 0, unmatched: 0, ambiguous: 0 },
    slotOnload: { observed: 0, matched: 0, unmatched: 0, ambiguous: 0 },
    impressionViewable: { observed: 0, matched: 0, unmatched: 0, ambiguous: 0 },
    slotVisibilityChanged: { observed: 0, matched: 0, unmatched: 0, ambiguous: 0 },
  };
}

function fakeApiStore() {
  return {
    snapshot: vi.fn(() => ({
      gptObserved: false,
      slots: [],
      callbackIssues: [],
      attributionIssues: [],
      coverage: emptyCoverage(),
      metadata: {
        droppedCallbacks: 0,
        droppedAttributionIssues: 0,
        evictedSlots: 0,
        evictedRequestCycles: 0,
      },
    })),
    subscribe: vi.fn(() => () => undefined),
    recordTrustedServerOpportunity: vi.fn(),
    recordPrebidRefresh: vi.fn(),
    recordTrustedServerCreativeRequest: vi.fn((_auctionSlotId: string) => 41),
    recordTrustedServerCreativeResponse: vi.fn(),
    recordTrustedServerCreativeFailure: vi.fn(),
  };
}

beforeEach(() => {
  vi.restoreAllMocks();
  window.history.replaceState({}, '', '/article?private=value#fragment');
});

describe('GptDiagnosticsApiController', () => {
  it('delegates attribution writers with exact arguments and returns the attempt ID', () => {
    const store = fakeApiStore();
    const controller = new GptDiagnosticsApiController(store, new FakeBindings(), {
      show: vi.fn(),
      hide: vi.fn(),
    });
    const slot = fakeSlot();
    const slots = [slot];

    controller.api.recordTrustedServerOpportunity(
      slot,
      'auction-slot-example',
      'renderable_candidate'
    );
    controller.api.recordPrebidRefresh(slots);
    const attemptId = controller.api.recordTrustedServerCreativeRequest('auction-slot-example');
    controller.api.recordTrustedServerCreativeResponse(41);
    controller.api.recordTrustedServerCreativeFailure(41, 'cache_fetch_failed');

    expect(store.recordTrustedServerOpportunity).toHaveBeenCalledTimes(1);
    expect(store.recordTrustedServerOpportunity).toHaveBeenCalledWith(
      slot,
      'auction-slot-example',
      'renderable_candidate'
    );
    expect(store.recordPrebidRefresh).toHaveBeenCalledTimes(1);
    expect(store.recordPrebidRefresh).toHaveBeenCalledWith(slots);
    expect(store.recordTrustedServerCreativeRequest).toHaveBeenCalledTimes(1);
    expect(store.recordTrustedServerCreativeRequest).toHaveBeenCalledWith('auction-slot-example');
    expect(attemptId).toBe(41);
    expect(store.recordTrustedServerCreativeResponse).toHaveBeenCalledTimes(1);
    expect(store.recordTrustedServerCreativeResponse).toHaveBeenCalledWith(41);
    expect(store.recordTrustedServerCreativeFailure).toHaveBeenCalledTimes(1);
    expect(store.recordTrustedServerCreativeFailure).toHaveBeenCalledWith(41, 'cache_fetch_failed');
  });

  it('swallows every attribution writer failure without changing ad delivery', () => {
    const failure = new Error('diagnostics failed');
    const store = {
      ...fakeApiStore(),
      recordTrustedServerOpportunity: vi.fn(() => {
        throw failure;
      }),
      recordPrebidRefresh: vi.fn(() => {
        throw failure;
      }),
      recordTrustedServerCreativeRequest: vi.fn((_auctionSlotId: string): number | undefined => {
        throw failure;
      }),
      recordTrustedServerCreativeResponse: vi.fn(() => {
        throw failure;
      }),
      recordTrustedServerCreativeFailure: vi.fn(() => {
        throw failure;
      }),
    };
    const controller = new GptDiagnosticsApiController(store, new FakeBindings(), {
      show: vi.fn(),
      hide: vi.fn(),
    });
    const slot = fakeSlot();

    expect(() =>
      controller.api.recordTrustedServerOpportunity(
        slot,
        'auction-slot-example',
        'renderable_candidate'
      )
    ).not.toThrow();
    expect(() => controller.api.recordPrebidRefresh([slot])).not.toThrow();
    expect(
      controller.api.recordTrustedServerCreativeRequest('auction-slot-example')
    ).toBeUndefined();
    expect(() => controller.api.recordTrustedServerCreativeResponse(41)).not.toThrow();
    expect(() =>
      controller.api.recordTrustedServerCreativeFailure(41, 'response_post_failed')
    ).not.toThrow();
  });

  it('detaches nested attribution evidence from the store snapshot', () => {
    const source = {
      gptObserved: true,
      slots: [
        {
          runtimeSlotNumber: 1,
          requests: [
            {
              requestNumber: 1,
              durations: {},
              incompleteSequence: false,
              adManager: {
                yieldGroupIds: [10],
                companyIds: [20],
              },
              trustedServerCreativeFailures: ['cache_fetch_failed' as const],
            },
          ],
        },
      ],
      callbackIssues: [],
      attributionIssues: [
        {
          reason: 'creative_attempt_expired' as const,
          timestampMs: 30,
        },
      ],
      coverage: emptyCoverage(),
      metadata: {
        droppedCallbacks: 0,
        droppedAttributionIssues: 2,
        evictedSlots: 0,
        evictedRequestCycles: 0,
      },
    };
    const store = {
      ...fakeApiStore(),
      snapshot: vi.fn(() => source),
    };
    const controller = new GptDiagnosticsApiController(store, new FakeBindings(), {
      show: vi.fn(),
      hide: vi.fn(),
    });

    const snapshot = controller.api.snapshot();
    const cycle = snapshot.slots[0]?.requests[0];

    expect(snapshot.attributionIssues).toEqual(source.attributionIssues);
    expect(snapshot.attributionIssues).not.toBe(source.attributionIssues);
    expect(snapshot.attributionIssues?.[0]).not.toBe(source.attributionIssues[0]);
    expect(cycle?.trustedServerCreativeFailures).toEqual(['cache_fetch_failed']);
    expect(cycle?.trustedServerCreativeFailures).not.toBe(
      source.slots[0]?.requests[0]?.trustedServerCreativeFailures
    );
    expect(cycle?.adManager?.yieldGroupIds).toEqual([10]);
    expect(cycle?.adManager?.yieldGroupIds).not.toBe(
      source.slots[0]?.requests[0]?.adManager.yieldGroupIds
    );
    expect(cycle?.adManager?.companyIds).toEqual([20]);
    expect(cycle?.adManager?.companyIds).not.toBe(
      source.slots[0]?.requests[0]?.adManager.companyIds
    );
    expect(snapshot.metadata).not.toBe(source.metadata);
    expect(snapshot.metadata.droppedAttributionIssues).toBe(2);
  });

  it('creates a fresh V1 allowlist snapshot with current binding facts', () => {
    let monotonicNow = 10;
    const store = new GptDiagnosticsStore({
      now: () => monotonicNow,
      schedule: (callback) => callback(),
    });
    const slot = fakeSlot();
    store.recordSlotRequested(slot);
    monotonicNow = 20;
    store.recordSlotResponseReceived(slot);
    monotonicNow = 25;
    store.recordSlotRenderEnded(slot, { isEmpty: false, size: [300, 250] });
    const bindings = new FakeBindings();
    const presentation = { show: vi.fn(), hide: vi.fn() };
    const controller = new GptDiagnosticsApiController(store, bindings, presentation, {
      now: () => new Date('2026-07-28T12:34:56.000Z'),
    });

    const snapshot = controller.api.snapshot();

    expect(snapshot).toMatchObject({
      version: 1,
      capturedAt: '2026-07-28T12:34:56.000Z',
      page: {
        origin: window.location.origin,
        pathname: '/article',
      },
      slots: [
        {
          runtimeSlotNumber: 1,
          slotElementId: 'ad-slot-example',
          adUnitPath: '/example/site/banner',
          binding: { status: 'bound' },
          requests: [
            {
              requestNumber: 1,
              isEmpty: false,
              size: [300, 250],
              durations: {
                requestToResponseMs: 10,
                responseToRenderMs: 5,
                requestToRenderMs: 15,
              },
            },
          ],
        },
      ],
    });
    expect(JSON.stringify(snapshot.page)).not.toContain('private');
    expect(JSON.stringify(snapshot.page)).not.toContain('fragment');
    expect(JSON.stringify(snapshot)).not.toMatch(/bidder|targeting|creativeMarkup|cookie|userId/i);

    const second = controller.api.snapshot();
    expect(second).not.toBe(snapshot);
    expect(second.slots).not.toBe(snapshot.slots);
  });

  it('coalesces store and binding updates and isolates subscribers', () => {
    const scheduled: Array<() => void> = [];
    const store = new GptDiagnosticsStore({
      now: () => 1,
      schedule: (callback) => callback(),
    });
    const bindings = new FakeBindings();
    const controller = new GptDiagnosticsApiController(
      store,
      bindings,
      { show: vi.fn(), hide: vi.fn() },
      {
        now: () => new Date('2026-07-28T00:00:00.000Z'),
        schedule: (callback) => scheduled.push(callback),
      }
    );
    controller.api.subscribe(() => {
      throw new Error('subscriber failed');
    });
    const listener = vi.fn();
    const unsubscribe = controller.api.subscribe(listener);

    const slot = fakeSlot();
    store.recordSlotRequested(slot);
    bindings.emit();
    store.recordSlotVisibilityChanged(slot, 20);

    expect(scheduled).toHaveLength(1);
    scheduled.shift()!();
    expect(listener).toHaveBeenCalledTimes(1);
    expect(listener).toHaveBeenCalledWith(expect.objectContaining({ version: 1 }));

    unsubscribe();
    store.recordSlotVisibilityChanged(slot, 30);
    scheduled.shift()!();
    expect(listener).toHaveBeenCalledTimes(1);
  });

  it('delegates show and hide without mutating diagnostics data', () => {
    const store = new GptDiagnosticsStore({ now: () => 1 });
    const presentation = { show: vi.fn(), hide: vi.fn() };
    const controller = new GptDiagnosticsApiController(store, new FakeBindings(), presentation);

    controller.api.show();
    controller.api.hide();

    expect(presentation.show).toHaveBeenCalledTimes(1);
    expect(presentation.hide).toHaveBeenCalledTimes(1);
    expect(store.snapshot().slots).toEqual([]);
  });

  it('downloads the V1 snapshot locally and revokes the object URL', async () => {
    const store = new GptDiagnosticsStore({ now: () => 1 });
    store.recordSlotRequested(fakeSlot());
    const bindings = new FakeBindings();
    let capturedBlob: Blob | undefined;
    const createObjectURL = vi.fn((blob: Blob) => {
      capturedBlob = blob;
      return 'blob:gpt-diagnostics';
    });
    const revokeObjectURL = vi.fn();
    Object.defineProperty(window.URL, 'createObjectURL', {
      configurable: true,
      value: createObjectURL,
    });
    Object.defineProperty(window.URL, 'revokeObjectURL', {
      configurable: true,
      value: revokeObjectURL,
    });
    const click = vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(() => {});
    const fetchReference = window.fetch;
    const controller = new GptDiagnosticsApiController(
      store,
      bindings,
      { show: vi.fn(), hide: vi.fn() },
      { now: () => new Date('2026-07-28T12:34:56.000Z') }
    );

    controller.api.export();

    expect(createObjectURL).toHaveBeenCalledTimes(1);
    expect(click).toHaveBeenCalledTimes(1);
    expect(revokeObjectURL).toHaveBeenCalledWith('blob:gpt-diagnostics');
    expect(document.querySelector('a[download]')).toBeNull();
    expect(window.fetch).toBe(fetchReference);

    const exported = JSON.parse(await readBlob(capturedBlob!));
    expect(exported).toMatchObject({
      version: 1,
      page: { pathname: '/article' },
    });
    expect(exported.page).not.toHaveProperty('search');
    expect(exported.page).not.toHaveProperty('hash');
  });

  it('stops source notifications after destruction', () => {
    const scheduled: Array<() => void> = [];
    const store = new GptDiagnosticsStore({
      now: () => 1,
      schedule: (callback) => callback(),
    });
    const bindings = new FakeBindings();
    const controller = new GptDiagnosticsApiController(
      store,
      bindings,
      { show: vi.fn(), hide: vi.fn() },
      { schedule: (callback) => scheduled.push(callback) }
    );
    const listener = vi.fn();
    controller.api.subscribe(listener);

    controller.destroy();
    store.recordSlotRequested(fakeSlot());
    bindings.emit();

    expect(scheduled).toEqual([]);
    expect(listener).not.toHaveBeenCalled();
  });
});
