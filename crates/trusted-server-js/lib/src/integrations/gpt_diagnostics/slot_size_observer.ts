import type { Size } from '../../core/types';

import type { GptDiagnosticsBindingManager } from './binding';
import { scheduleFrame } from './presentation_helpers';
import type { GptDiagnosticsStoreSnapshot } from './store';

interface SlotSizeStore {
  snapshot(): GptDiagnosticsStoreSnapshot;
  recordObservedSlotSize(runtimeSlotNumber: number, requestNumber: number, size: Size): void;
  subscribe(listener: () => void): () => void;
}

interface SlotSizeBindings {
  get: GptDiagnosticsBindingManager['get'];
  subscribe(listener: () => void): () => void;
}

type SlotSizeWindow = Window & {
  HTMLElement: typeof HTMLElement;
  ResizeObserver?: typeof ResizeObserver;
};

interface SlotSizeObserverOptions {
  window?: SlotSizeWindow;
  scheduleFrame?: (callback: () => void) => void;
}

interface ObservedCycle {
  runtimeSlotNumber: number;
  requestNumber: number;
}

function latestFilledCycle(
  slot: GptDiagnosticsStoreSnapshot['slots'][number]
): ObservedCycle | undefined {
  const cycle = slot.requests[slot.requests.length - 1];
  if (!cycle || cycle.isEmpty !== false || cycle.renderAtMs === undefined) return undefined;
  return { runtimeSlotNumber: slot.runtimeSlotNumber, requestNumber: cycle.requestNumber };
}

/**
 * Observes the outer CSS boxes of uniquely bound elements after filled GPT renders.
 *
 * Measurements remain separately labelled from GPT's reported creative size and
 * are conditionally written with the runtime-slot and request-cycle identity that
 * was current when the measurement was scheduled.
 */
export class GptDiagnosticsSlotSizeObserver {
  private readonly store: SlotSizeStore;
  private readonly bindings: SlotSizeBindings;
  private readonly window: SlotSizeWindow;
  private readonly scheduleFrame: (callback: () => void) => void;
  private readonly unsubscribeStore: () => void;
  private readonly unsubscribeBindings: () => void;
  private resizeObserver?: ResizeObserver;
  private refreshScheduled = false;
  private destroyed = false;

  constructor(
    store: SlotSizeStore,
    bindings: SlotSizeBindings,
    options: SlotSizeObserverOptions = {}
  ) {
    this.store = store;
    this.bindings = bindings;
    this.window = options.window ?? (window as unknown as SlotSizeWindow);
    this.scheduleFrame =
      options.scheduleFrame ?? ((callback) => scheduleFrame(this.window, callback));
    this.unsubscribeStore = this.store.subscribe(this.scheduleRefresh);
    this.unsubscribeBindings = this.bindings.subscribe(this.scheduleRefresh);
    this.refresh();
  }

  destroy(): void {
    if (this.destroyed) return;
    this.destroyed = true;
    this.unsubscribeStore();
    this.unsubscribeBindings();
    this.resizeObserver?.disconnect();
  }

  private readonly scheduleRefresh = (): void => {
    if (this.destroyed || this.refreshScheduled) return;
    this.refreshScheduled = true;
    this.scheduleFrame(() => {
      this.refreshScheduled = false;
      this.refresh();
    });
  };

  private refresh(): void {
    if (this.destroyed) return;
    this.resizeObserver?.disconnect();
    const observations = new Map<HTMLElement, ObservedCycle>();
    const ResizeObserverConstructor = this.window.ResizeObserver;
    if (typeof ResizeObserverConstructor === 'function') {
      this.resizeObserver = new ResizeObserverConstructor((entries) => {
        for (const entry of entries) {
          const element = entry.target;
          if (!(element instanceof this.window.HTMLElement)) continue;
          const cycle = observations.get(element);
          if (cycle) this.scheduleMeasure(element, cycle);
        }
      });
    }

    for (const slot of this.store.snapshot().slots) {
      const cycle = latestFilledCycle(slot);
      const binding = this.bindings.get(slot.runtimeSlotNumber);
      if (!cycle || binding.binding.status !== 'bound' || !binding.element?.isConnected) continue;
      observations.set(binding.element, cycle);
      this.resizeObserver?.observe(binding.element);
      this.scheduleMeasure(binding.element, cycle);
    }
  }

  private scheduleMeasure(element: HTMLElement, cycle: ObservedCycle): void {
    this.scheduleFrame(() => this.measure(element, cycle));
  }

  private measure(element: HTMLElement, cycle: ObservedCycle): void {
    if (this.destroyed) return;

    const binding = this.bindings.get(cycle.runtimeSlotNumber);
    if (binding.binding.status !== 'bound' || binding.element !== element || !element.isConnected) {
      return;
    }

    const rectangle = element.getBoundingClientRect();
    if (
      !Number.isFinite(rectangle.width) ||
      !Number.isFinite(rectangle.height) ||
      rectangle.width < 0 ||
      rectangle.height < 0
    ) {
      return;
    }
    this.store.recordObservedSlotSize(cycle.runtimeSlotNumber, cycle.requestNumber, [
      Math.round(rectangle.width),
      Math.round(rectangle.height),
    ]);
  }
}
