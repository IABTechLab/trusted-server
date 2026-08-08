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
