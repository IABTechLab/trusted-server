import { afterEach, describe, expect, it, vi } from 'vitest';

import type { GptDiagnosticsRequestCycle } from '../../../src/core/types';
import { GptDiagnosticsSlotSizeObserver } from '../../../src/integrations/gpt_diagnostics/slot_size_observer';
import type { GptDiagnosticsStoreSnapshot } from '../../../src/integrations/gpt_diagnostics/store';

class ResizeObserverMock {
  static instances: ResizeObserverMock[] = [];
  readonly observe = vi.fn();
  readonly disconnect = vi.fn();

  constructor(readonly callback: ResizeObserverCallback) {
    ResizeObserverMock.instances.push(this);
  }

  emit(element: Element): void {
    this.callback([{ target: element } as ResizeObserverEntry], this as unknown as ResizeObserver);
  }
}

function cycle(requestNumber: number, isEmpty: boolean | undefined): GptDiagnosticsRequestCycle {
  return {
    requestNumber,
    isEmpty,
    renderAtMs: 1,
    durations: {},
    incompleteSequence: false,
  };
}

function snapshot(requests: GptDiagnosticsRequestCycle[]): GptDiagnosticsStoreSnapshot {
  return {
    gptObserved: true,
    slots: [
      {
        runtimeSlotNumber: 1,
        slotElementId: 'ad-slot-example',
        requests,
      },
    ],
    callbackIssues: [],
    attributionIssues: [],
    coverage: {
      slotRequested: { observed: 0, matched: 0, unmatched: 0, ambiguous: 0 },
      slotResponseReceived: { observed: 0, matched: 0, unmatched: 0, ambiguous: 0 },
      slotRenderEnded: { observed: 0, matched: 0, unmatched: 0, ambiguous: 0 },
      slotOnload: { observed: 0, matched: 0, unmatched: 0, ambiguous: 0 },
      impressionViewable: { observed: 0, matched: 0, unmatched: 0, ambiguous: 0 },
      slotVisibilityChanged: { observed: 0, matched: 0, unmatched: 0, ambiguous: 0 },
    },
    metadata: {
      droppedCallbacks: 0,
      droppedAttributionIssues: 0,
      evictedSlots: 0,
      evictedRequestCycles: 0,
    },
  };
}

describe('GptDiagnosticsSlotSizeObserver', () => {
  afterEach(() => {
    ResizeObserverMock.instances = [];
    document.body.replaceChildren();
  });

  it('keeps GPT 1×1 distinct from the observed outer box and updates it on resize', () => {
    const element = document.createElement('div');
    document.body.append(element);
    const getBoundingClientRect = vi.spyOn(element, 'getBoundingClientRect');
    getBoundingClientRect.mockReturnValue({ width: 728, height: 90 } as DOMRect);
    const requests = [cycle(1, false)];
    requests[0]!.size = [1, 1];
    const store = {
      snapshot: () => snapshot(requests),
      recordObservedSlotSize: vi.fn(),
      subscribe: () => () => undefined,
    };
    const bindings = {
      get: () => ({ binding: { status: 'bound' as const }, element, visible: true }),
      subscribe: () => () => undefined,
    };

    const observer = new GptDiagnosticsSlotSizeObserver(store, bindings, {
      window: { HTMLElement, ResizeObserver: ResizeObserverMock } as unknown as Window,
      scheduleFrame: (callback) => callback(),
    });

    expect(store.recordObservedSlotSize).toHaveBeenCalledWith(1, 1, [728, 90]);
    expect(requests[0]!.size).toEqual([1, 1]);

    getBoundingClientRect.mockReturnValue({ width: 970, height: 250 } as DOMRect);
    ResizeObserverMock.instances[ResizeObserverMock.instances.length - 1]!.emit(element);
    expect(store.recordObservedSlotSize).toHaveBeenLastCalledWith(1, 1, [970, 250]);
    observer.destroy();
  });

  it.each(['unbound', 'ambiguous'] as const)('does not observe %s slots', (status) => {
    const element = document.createElement('div');
    document.body.append(element);
    const store = {
      snapshot: () => snapshot([cycle(1, false)]),
      recordObservedSlotSize: vi.fn(),
      subscribe: () => () => undefined,
    };
    const bindings = {
      get: () => ({ binding: { status }, element, visible: false }),
      subscribe: () => () => undefined,
    };

    const observer = new GptDiagnosticsSlotSizeObserver(store, bindings, {
      window: { HTMLElement, ResizeObserver: ResizeObserverMock } as unknown as Window,
      scheduleFrame: (callback) => callback(),
    });

    expect(store.recordObservedSlotSize).not.toHaveBeenCalled();
    expect(
      ResizeObserverMock.instances[ResizeObserverMock.instances.length - 1]!.observe
    ).not.toHaveBeenCalled();
    observer.destroy();
  });

  it('cannot apply a delayed prior-cycle measurement to a later refresh', () => {
    const element = document.createElement('div');
    document.body.append(element);
    vi.spyOn(element, 'getBoundingClientRect').mockReturnValue({
      width: 300,
      height: 250,
    } as DOMRect);
    const requests = [cycle(1, false)];
    const listeners: Array<() => void> = [];
    const store = {
      snapshot: () => snapshot(requests),
      recordObservedSlotSize: vi.fn(),
      subscribe: (listener: () => void) => {
        listeners.push(listener);
        return () => undefined;
      },
    };
    const bindings = {
      get: () => ({ binding: { status: 'bound' as const }, element, visible: true }),
      subscribe: () => () => undefined,
    };
    const frames: Array<() => void> = [];
    const observer = new GptDiagnosticsSlotSizeObserver(store, bindings, {
      window: { HTMLElement, ResizeObserver: ResizeObserverMock } as unknown as Window,
      scheduleFrame: (callback) => frames.push(callback),
    });
    const firstObserver = ResizeObserverMock.instances[0]!;

    requests.push(cycle(2, false));
    listeners[0]!();
    frames.shift()!();
    firstObserver.emit(element);
    while (frames.length > 0) frames.shift()!();

    expect(store.recordObservedSlotSize).toHaveBeenCalledWith(1, 1, [300, 250]);
    expect(store.recordObservedSlotSize).toHaveBeenCalledWith(1, 2, [300, 250]);
    observer.destroy();
  });
});
