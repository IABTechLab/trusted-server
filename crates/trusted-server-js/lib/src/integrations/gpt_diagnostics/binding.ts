import type { GptDiagnosticsBinding, GptDiagnosticsSlotExport } from '../../core/types';

import { scheduleFrame } from './presentation_helpers';
import type { GptDiagnosticsBindingInput } from './store';

interface BindingStore {
  bindingInputs(): GptDiagnosticsBindingInput[];
  subscribe(listener: () => void): () => void;
}

type BindingWindow = Window & {
  CSS?: typeof CSS;
  HTMLElement: typeof HTMLElement;
  MutationObserver?: typeof MutationObserver;
};

interface BindingOptions {
  document?: Document;
  window?: BindingWindow;
  scheduleFrame?: (callback: () => void) => void;
}

export interface GptDiagnosticsBindingView {
  binding: GptDiagnosticsBinding;
  element?: HTMLElement;
  visible: boolean;
}

type BindingListener = () => void;

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

function nodeIntersectsSlotIds(node: Node, slotElementIds: Set<string>): boolean {
  if (!(node instanceof Element)) return false;
  if (slotElementIds.has(node.id)) return true;
  return Array.from(node.querySelectorAll('[id]')).some((element) =>
    slotElementIds.has(element.id)
  );
}

function mutationIntersectsSlotIds(record: MutationRecord, slotElementIds: Set<string>): boolean {
  if (record.type === 'attributes') {
    if (!(record.target instanceof Element)) return false;
    return slotElementIds.has(record.target.id) || slotElementIds.has(record.oldValue ?? '');
  }

  if (record.target instanceof Element && slotElementIds.has(record.target.id)) return true;
  return [...record.addedNodes, ...record.removedNodes].some((node) =>
    nodeIntersectsSlotIds(node, slotElementIds)
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
  private readonly scheduleFrame: (callback: () => void) => void;
  private readonly bindings = new Map<number, GptDiagnosticsBindingView>();
  private readonly listeners = new Set<BindingListener>();
  private readonly slotElementIds = new Set<string>();
  private readonly unsubscribeStore: () => void;
  private mutationObserver?: MutationObserver;
  private refreshScheduled = false;
  private destroyed = false;

  constructor(store: BindingStore, options: BindingOptions = {}) {
    this.store = store;
    this.document = options.document ?? document;
    this.window = options.window ?? (window as unknown as BindingWindow);
    this.scheduleFrame =
      options.scheduleFrame ?? ((callback) => scheduleFrame(this.window, callback));
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
    this.scheduleFrame(() => {
      this.refreshScheduled = false;
      this.refresh();
    });
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

    const candidate = this.document.getElementById(slotElementId);
    if (!candidate || !(candidate instanceof this.window.HTMLElement) || !candidate.isConnected) {
      return {
        binding: { status: 'unbound', reason: 'missing_element' },
        visible: false,
      };
    }

    const element = candidate as HTMLElement;
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
        records.some((record) => mutationIntersectsSlotIds(record, this.slotElementIds))
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
