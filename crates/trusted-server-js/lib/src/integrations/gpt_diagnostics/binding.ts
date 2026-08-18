import type { GptDiagnosticsBinding, GptDiagnosticsSlotExport } from '../../core/types';
import { realmOwnedElement, realmOwnedHtmlElement } from '../../shared/realm';

import type { GptDiagnosticsBindingInput } from './store';

interface BindingStore {
  bindingInputs(): GptDiagnosticsBindingInput[];
  subscribe(listener: () => void): () => void;
}

type BindingWindow = Window & {
  CSS?: typeof CSS | undefined;
  HTMLElement: typeof HTMLElement;
  MutationObserver?: typeof MutationObserver | undefined;
};

interface BindingOptions {
  document?: Document | undefined;
  window?: BindingWindow | undefined;
  scheduleFrame?: ((callback: () => void) => () => void) | undefined;
}

export interface GptDiagnosticsBindingView {
  binding: GptDiagnosticsBinding;
  element?: HTMLElement | undefined;
  visible: boolean;
}

type BindingListener = () => void;

function defaultScheduleFrame(callback: () => void): () => void {
  if (typeof requestAnimationFrame === 'function' && typeof cancelAnimationFrame === 'function') {
    const frame = requestAnimationFrame(() => callback());
    return () => cancelAnimationFrame(frame);
  }
  let active = true;
  queueMicrotask(() => {
    if (active) callback();
  });
  return () => {
    active = false;
  };
}

function isVisibleInViewport(element: HTMLElement, window: BindingWindow): boolean {
  const rectangle = element.getBoundingClientRect();
  if (rectangle.width <= 0 || rectangle.height <= 0) return false;

  return (
    rectangle.bottom > 0 &&
    rectangle.right > 0 &&
    rectangle.top < window.innerHeight &&
    rectangle.left < window.innerWidth
  );
}

function nodeIntersectsSlotIds(
  node: Node,
  slotElementIds: Set<string>,
  targetWindow: BindingWindow
): boolean {
  const element = realmOwnedElement(node, targetWindow);
  if (!element) return false;
  if (slotElementIds.has(element.id)) return true;
  return Array.from(element.querySelectorAll('[id]')).some((descendant) =>
    slotElementIds.has(descendant.id)
  );
}

function mutationIntersectsSlotIds(
  record: MutationRecord,
  slotElementIds: Set<string>,
  targetWindow: BindingWindow
): boolean {
  const target = realmOwnedElement(record.target, targetWindow);
  if (record.type === 'attributes') {
    if (!target) return false;
    return slotElementIds.has(target.id) || slotElementIds.has(record.oldValue ?? '');
  }

  if (target && slotElementIds.has(target.id)) return true;
  return [...record.addedNodes, ...record.removedNodes].some((node) =>
    nodeIntersectsSlotIds(node, slotElementIds, targetWindow)
  );
}

function bindingEquals(
  previous: GptDiagnosticsBindingView | undefined,
  next: GptDiagnosticsBindingView
): boolean {
  return (
    previous?.binding.status === next.binding.status &&
    previous.binding.reason === next.binding.reason &&
    previous.element === next.element &&
    previous.visible === next.visible
  );
}

/** Tracks conservative exact DOM bindings for retained GPT slot records. */
export class GptDiagnosticsBindingManager {
  private readonly store: BindingStore;
  private readonly document: Document;
  private readonly window: BindingWindow;
  private readonly scheduleFrame: (callback: () => void) => () => void;
  private readonly bindings = new Map<number, GptDiagnosticsBindingView>();
  private readonly listeners = new Set<BindingListener>();
  private readonly slotElementIds = new Set<string>();
  private readonly unsubscribeStore: () => void;
  private mutationObserver?: MutationObserver;
  private cancelScheduledRefresh: (() => void) | undefined;
  private refreshScheduled = false;
  private destroyed = false;

  constructor(store: BindingStore, options: BindingOptions = {}) {
    this.store = store;
    this.document = options.document ?? document;
    this.window =
      options.window ??
      (this.document.defaultView as unknown as BindingWindow | null) ??
      (window as unknown as BindingWindow);
    this.scheduleFrame = options.scheduleFrame ?? defaultScheduleFrame;
    this.unsubscribeStore = this.store.subscribe(() => this.scheduleRefresh());

    this.window.addEventListener('scroll', this.scheduleRefresh, { passive: true });
    this.window.addEventListener('resize', this.scheduleRefresh, { passive: true });
    this.installMutationObserver();
    this.refresh();
  }

  subscribe(listener: BindingListener): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  get(runtimeSlotNumber: number): GptDiagnosticsBindingView {
    return (
      this.bindings.get(runtimeSlotNumber) ?? {
        binding: { status: 'unbound', reason: 'missing_element' },
        visible: false,
      }
    );
  }

  exportBinding(runtimeSlotNumber: number): GptDiagnosticsSlotExport['binding'] {
    return { ...this.get(runtimeSlotNumber).binding };
  }

  refresh(): void {
    if (this.destroyed) return;

    const slots = this.store.bindingInputs();
    this.slotElementIds.clear();
    const claimCounts = new Map<string, number>();
    for (const slot of slots) {
      if (!slot.slotElementId) continue;
      this.slotElementIds.add(slot.slotElementId);
      claimCounts.set(slot.slotElementId, (claimCounts.get(slot.slotElementId) ?? 0) + 1);
    }

    const nextBindings = new Map<number, GptDiagnosticsBindingView>();
    let changed = this.bindings.size !== slots.length;
    for (const slot of slots) {
      const next = this.resolveBinding(slot.slotElementId, claimCounts);
      nextBindings.set(slot.runtimeSlotNumber, next);
      changed ||= !bindingEquals(this.bindings.get(slot.runtimeSlotNumber), next);
    }

    this.bindings.clear();
    for (const [runtimeSlotNumber, binding] of nextBindings) {
      this.bindings.set(runtimeSlotNumber, binding);
    }

    if (changed) this.notify();
  }

  destroy(): void {
    if (this.destroyed) return;
    this.destroyed = true;
    const cancelRefresh = this.cancelScheduledRefresh;
    this.cancelScheduledRefresh = undefined;
    this.refreshScheduled = false;
    try {
      cancelRefresh?.();
    } catch {
      // Continue releasing every independently owned binding resource.
    }
    this.unsubscribeStore();
    this.mutationObserver?.disconnect();
    this.window.removeEventListener('scroll', this.scheduleRefresh);
    this.window.removeEventListener('resize', this.scheduleRefresh);
    this.listeners.clear();
    this.bindings.clear();
  }

  private readonly scheduleRefresh = (): void => {
    if (this.destroyed || this.refreshScheduled) return;
    this.refreshScheduled = true;
    let active = true;
    let cancelFrame: (() => void) | undefined;
    const run = (): void => {
      if (!active) return;
      active = false;
      this.cancelScheduledRefresh = undefined;
      try {
        cancelFrame?.();
      } catch {
        // A completed frame remains authoritative when scheduler cleanup fails.
      }
      this.refreshScheduled = false;
      this.refresh();
    };
    try {
      cancelFrame = this.scheduleFrame(run);
      if (typeof cancelFrame !== 'function') {
        throw new TypeError('Invalid binding frame scheduler');
      }
      if (active) {
        this.cancelScheduledRefresh = (): void => {
          if (!active) return;
          active = false;
          this.refreshScheduled = false;
          cancelFrame?.();
        };
      } else {
        cancelFrame();
      }
    } catch {
      active = false;
      this.cancelScheduledRefresh = undefined;
      this.refreshScheduled = false;
    }
  };

  private resolveBinding(
    slotElementId: string | undefined,
    claimCounts: Map<string, number>
  ): GptDiagnosticsBindingView {
    if (!slotElementId) {
      return {
        binding: { status: 'unbound', reason: 'missing_slot_element_id' },
        visible: false,
      };
    }

    if ((claimCounts.get(slotElementId) ?? 0) > 1) {
      return {
        binding: { status: 'ambiguous', reason: 'duplicate_gpt_slot_id' },
        visible: false,
      };
    }

    const element = realmOwnedHtmlElement(this.document.getElementById(slotElementId), this.window);
    if (!element || !element.isConnected) {
      return {
        binding: { status: 'unbound', reason: 'missing_element' },
        visible: false,
      };
    }

    const escape = this.window.CSS?.escape;
    if (typeof escape !== 'function') {
      return {
        binding: { status: 'ambiguous', reason: 'dom_uniqueness_unverifiable' },
        visible: false,
      };
    }

    try {
      const matches = this.document.querySelectorAll(`#${escape(slotElementId)}`);
      if (matches.length !== 1 || matches[0] !== element) {
        return {
          binding: { status: 'ambiguous', reason: 'duplicate_dom_id' },
          visible: false,
        };
      }
    } catch {
      return {
        binding: { status: 'ambiguous', reason: 'dom_uniqueness_unverifiable' },
        visible: false,
      };
    }

    return {
      binding: { status: 'bound' },
      element,
      visible: isVisibleInViewport(element, this.window),
    };
  }

  private installMutationObserver(): void {
    const Observer = this.window.MutationObserver;
    if (typeof Observer !== 'function' || !this.document.documentElement) return;

    const observer = new Observer((records) => {
      if (
        this.slotElementIds.size > 0 &&
        records.some((record) =>
          mutationIntersectsSlotIds(record, this.slotElementIds, this.window)
        )
      ) {
        this.scheduleRefresh();
      }
    });
    this.mutationObserver = observer;
    observer.observe(this.document.documentElement, {
      attributes: true,
      attributeFilter: ['id'],
      attributeOldValue: true,
      childList: true,
      subtree: true,
    });
  }

  private notify(): void {
    for (const listener of this.listeners) {
      try {
        listener();
      } catch {
        // One diagnostics consumer must not block binding updates.
      }
    }
  }
}
