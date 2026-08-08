import { beforeEach, describe, expect, it, vi } from 'vitest';

import type { GptDiagnosticsBinding } from '../../../src/core/types';
import { DiagnosticsSubscriberLimitError } from '../../../src/core/trace';
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

function scheduleInto(tasks: Array<() => void>): (callback: () => void) => () => void {
  return (callback) => {
    tasks.push(callback);
    return () => {
      const index = tasks.indexOf(callback);
      if (index >= 0) tasks.splice(index, 1);
    };
  };
}

beforeEach(() => {
  vi.restoreAllMocks();
  window.history.replaceState({}, '', '/article?private=value#fragment');
});

describe('GptDiagnosticsApiController', () => {
  it('keeps evidence writers off the public read-only API', () => {
    const controller = new GptDiagnosticsApiController(fakeApiStore(), new FakeBindings(), {
      show: vi.fn(),
      hide: vi.fn(),
    });

    expect(Object.keys(controller.api).sort()).toEqual([
      'export',
      'hide',
      'show',
      'snapshot',
      'subscribe',
    ]);
    expect(Object.keys(controller.recorder).sort()).toEqual([
      'recordPrebidRefresh',
      'recordTrustedServerCreativeFailure',
      'recordTrustedServerCreativeRequest',
      'recordTrustedServerCreativeResponse',
      'recordTrustedServerOpportunity',
    ]);
  });

  it('delegates attribution writers with exact arguments and returns the attempt ID', () => {
    const store = fakeApiStore();
    const controller = new GptDiagnosticsApiController(store, new FakeBindings(), {
      show: vi.fn(),
      hide: vi.fn(),
    });
    const slot = fakeSlot();
    const slots = [slot];

    controller.recorder.recordTrustedServerOpportunity(
      slot,
      'auction-slot-example',
      'renderable_candidate'
    );
    controller.recorder.recordPrebidRefresh(slots);
    const attemptId =
      controller.recorder.recordTrustedServerCreativeRequest('auction-slot-example');
    controller.recorder.recordTrustedServerCreativeResponse(41);
    controller.recorder.recordTrustedServerCreativeFailure(41, 'cache_fetch_failed');

    expect(store.recordTrustedServerOpportunity).toHaveBeenCalledTimes(1);
    expect(store.recordTrustedServerOpportunity).toHaveBeenCalledWith(
      slot,
      'auction-slot-example',
      'renderable_candidate',
      undefined,
      undefined
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

  it('forwards an optional opaque auction ID without changing diagnostics fail-open behavior', () => {
    const store = fakeApiStore();
    const controller = new GptDiagnosticsApiController(store, new FakeBindings(), {
      show: vi.fn(),
      hide: vi.fn(),
    });
    const slot = fakeSlot();

    controller.recorder.recordTrustedServerOpportunity(
      slot,
      'auction-slot-example',
      'renderable_candidate',
      'auction-123'
    );

    expect(store.recordTrustedServerOpportunity).toHaveBeenCalledWith(
      slot,
      'auction-slot-example',
      'renderable_candidate',
      'auction-123',
      undefined
    );
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
      controller.recorder.recordTrustedServerOpportunity(
        slot,
        'auction-slot-example',
        'renderable_candidate'
      )
    ).not.toThrow();
    expect(() => controller.recorder.recordPrebidRefresh([slot])).not.toThrow();
    expect(
      controller.recorder.recordTrustedServerCreativeRequest('auction-slot-example')
    ).toBeUndefined();
    expect(() => controller.recorder.recordTrustedServerCreativeResponse(41)).not.toThrow();
    expect(() =>
      controller.recorder.recordTrustedServerCreativeFailure(41, 'response_post_failed')
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
              requestedSlotSizes: [
                [300, 250],
                [728, 90],
              ] as const,
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
    expect(cycle?.requestedSlotSizes).toEqual([
      [300, 250],
      [728, 90],
    ]);
    expect(cycle?.requestedSlotSizes).not.toBe(source.slots[0]?.requests[0]?.requestedSlotSizes);
    expect(cycle?.requestedSlotSizes?.[0]).not.toBe(
      source.slots[0]?.requests[0]?.requestedSlotSizes?.[0]
    );
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
    // Pin the exported shape rather than blocklisting known-bad names: the
    // export is built by spreading store records, so any new field must be
    // added here deliberately before it can reach a downloaded snapshot.
    expect(Object.keys(snapshot).sort()).toEqual([
      'attributionIssues',
      'callbackIssues',
      'capturedAt',
      'coverage',
      'metadata',
      'page',
      'slots',
      'version',
    ]);
    expect(Object.keys(snapshot.slots[0]!).sort()).toEqual([
      'adUnitPath',
      'binding',
      'currentVisibilityPercentage',
      'maximumVisibilityPercentage',
      'requests',
      'runtimeSlotNumber',
      'slotElementId',
    ]);
    expect(Object.keys(snapshot.slots[0]!.requests[0]!).sort()).toEqual([
      'adManager',
      'delivery',
      'durations',
      'incompleteSequence',
      'isBackfill',
      'isEmpty',
      'observedSlotSize',
      'renderAtMs',
      'requestNumber',
      'requestPath',
      'requestedAtMs',
      'requestedSlotSizes',
      'responseAtMs',
      'responseClass',
      'size',
      'slotContentChanged',
      'trustedServerCreativeFailures',
    ]);
    expect(Object.keys(snapshot.metadata).sort()).toEqual([
      'droppedAttributionIssues',
      'droppedCallbacks',
      'evictedRequestCycles',
      'evictedSlots',
    ]);

    const second = controller.api.snapshot();
    expect(second).not.toBe(snapshot);
    expect(second.slots).not.toBe(snapshot.slots);
    expect(Object.isFrozen(snapshot)).toBe(true);
    expect(Object.isFrozen(snapshot.page)).toBe(true);
    expect(Object.isFrozen(snapshot.slots)).toBe(true);
    expect(Object.isFrozen(snapshot.slots[0]?.requests)).toBe(true);
    expect(Object.isFrozen(snapshot.slots[0]?.requests[0]?.durations)).toBe(true);
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
        schedule: scheduleInto(scheduled),
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

  it('captures subscriber membership per commit and coalesces to the latest snapshot', () => {
    const scheduled: Array<() => void> = [];
    const store = new GptDiagnosticsStore({ now: () => 1, schedule: (callback) => callback() });
    const bindings = new FakeBindings();
    const controller = new GptDiagnosticsApiController(
      store,
      bindings,
      { show: vi.fn(), hide: vi.fn() },
      {
        now: () => new Date('2026-07-28T00:00:00.000Z'),
        schedule: scheduleInto(scheduled),
      }
    );
    const first = vi.fn();
    const second = vi.fn();
    const releaseFirst = controller.api.subscribe(first);
    const observedSlot = fakeSlot();

    store.recordSlotRequested(observedSlot);
    controller.api.subscribe(second);
    releaseFirst();
    expect(scheduled).toHaveLength(1);
    scheduled.shift()?.();
    expect(first).not.toHaveBeenCalled();
    expect(second).not.toHaveBeenCalled();

    store.recordSlotVisibilityChanged(observedSlot, 10);
    store.recordSlotVisibilityChanged(observedSlot, 20);
    expect(scheduled).toHaveLength(1);
    scheduled.shift()?.();
    expect(second).toHaveBeenCalledOnce();
    expect(second.mock.calls[0]?.[0]).toEqual(
      expect.objectContaining({
        slots: [expect.objectContaining({ currentVisibilityPercentage: 20 })],
      })
    );
  });

  it('validates callability before enforcing the shared 32-subscriber cap', () => {
    const controller = new GptDiagnosticsApiController(
      new GptDiagnosticsStore({ now: () => 1 }),
      new FakeBindings(),
      { show: vi.fn(), hide: vi.fn() }
    );
    const releases = Array.from({ length: 32 }, () => controller.api.subscribe(() => undefined));

    expect(() => controller.api.subscribe(null as never)).toThrow(TypeError);
    expect(() => controller.api.subscribe(() => undefined)).toThrow(
      DiagnosticsSubscriberLimitError
    );
    expect(() => controller.api.subscribe(() => undefined)).toThrow(
      expect.objectContaining({ code: 'subscriber_capacity', surface: 'gpt' })
    );
    releases[0]?.();
    releases[0]?.();
    expect(controller.api.subscribe(() => undefined)).toEqual(expect.any(Function));
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
      { schedule: scheduleInto(scheduled) }
    );
    const listener = vi.fn();
    controller.api.subscribe(listener);

    store.recordSlotRequested(fakeSlot());
    expect(scheduled).toHaveLength(1);

    controller.destroy();
    while (scheduled.length > 0) scheduled.shift()?.();
    store.recordSlotVisibilityChanged(fakeSlot(), 10);
    bindings.emit();

    expect(scheduled).toEqual([]);
    expect(listener).not.toHaveBeenCalled();
  });
});
