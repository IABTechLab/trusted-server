import type { GptDiagnosticsRequestCycle } from '../../core/types';

import type { GptDiagnosticsBindingManager } from './binding';
import type {
  GptDiagnosticsBindingInput,
  GptDiagnosticsStoreSlotSnapshot,
  GptDiagnosticsStoreSnapshot,
} from './store';

interface BadgeStore {
  snapshot(): GptDiagnosticsStoreSnapshot;
  bindingInputs(): GptDiagnosticsBindingInput[];
  subscribe(listener: () => void): () => void;
}

interface BadgeBindings {
  get: GptDiagnosticsBindingManager['get'];
  subscribe(listener: () => void): () => void;
}

type BadgeWindow = Window & {
  MutationObserver?: typeof MutationObserver;
  ResizeObserver?: typeof ResizeObserver;
};

const BADGE_MAX_WIDTH_PX = 260;
const BADGE_EDGE_GUTTER_PX = 4;

interface BadgeOptions {
  window?: BadgeWindow;
  document?: Document;
  scheduleFrame?: (callback: () => void) => void;
}

function defaultScheduleFrame(callback: () => void): void {
  if (typeof requestAnimationFrame === 'function') {
    requestAnimationFrame(() => callback());
  } else {
    queueMicrotask(callback);
  }
}

function intersectsViewport(rectangle: DOMRect, window: Window): boolean {
  return (
    rectangle.width > 0 &&
    rectangle.height > 0 &&
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

function formatMilliseconds(value: number | undefined): string | undefined {
  if (value === undefined || !Number.isFinite(value) || value < 0) return undefined;
  if (value >= 1000) return `${Math.round(value / 100) / 10} s`;
  return `${Math.round(value)} ms`;
}

function badgeText(cycle: GptDiagnosticsRequestCycle): string {
  const firstLine: string[] = [];
  if (cycle.isEmpty === true) firstLine.push('Empty');
  else if (cycle.isEmpty === false) firstLine.push('Filled');
  else if (cycle.renderAtMs !== undefined) firstLine.push('Rendered (fill unknown)');
  else firstLine.push('Pending');
  if (cycle.size) firstLine.push(`${cycle.size[0]}×${cycle.size[1]}`);

  const timingLine: string[] = [];
  const response = formatMilliseconds(cycle.durations.requestToResponseMs);
  const render = formatMilliseconds(cycle.durations.responseToRenderMs);
  if (response) timingLine.push(`Response ${response}`);
  if (render) timingLine.push(`Render ${render}`);

  const lines = [firstLine.join(' · ')];
  if (timingLine.length > 0) lines.push(timingLine.join(' · '));
  const viewable = formatMilliseconds(cycle.durations.renderToViewableMs);
  if (viewable) lines.push(`Viewable after ${viewable}`);
  if (cycle.incompleteSequence) lines.push('Incomplete sequence');
  return lines.join('\n');
}

function latestCycle(
  slot: GptDiagnosticsStoreSlotSnapshot
): GptDiagnosticsRequestCycle | undefined {
  return slot.requests[slot.requests.length - 1];
}

/** Renders viewport-positioned badges inside the diagnostics shadow layer. */
export class GptDiagnosticsBadgeManager {
  private readonly store: BadgeStore;
  private readonly bindings: BadgeBindings;
  private readonly window: BadgeWindow;
  private readonly document: Document;
  private readonly scheduleFrame: (callback: () => void) => void;
  private readonly unsubscribeStore: () => void;
  private readonly unsubscribeBindings: () => void;
  private readonly slotElementIds = new Set<string>();
  private slots: GptDiagnosticsStoreSlotSnapshot[] = [];
  private layer?: HTMLElement;
  private mutationObserver?: MutationObserver;
  private resizeObserver?: ResizeObserver;
  private scheduled = false;
  private destroyed = false;

  constructor(store: BadgeStore, bindings: BadgeBindings, options: BadgeOptions = {}) {
    this.store = store;
    this.bindings = bindings;
    this.window = options.window ?? (window as unknown as BadgeWindow);
    this.document = options.document ?? document;
    this.scheduleFrame = options.scheduleFrame ?? defaultScheduleFrame;
    this.refreshSlotElementIds();
    this.unsubscribeStore = this.store.subscribe(() => {
      this.refreshSlotElementIds();
      if (this.layer?.isConnected) this.refreshSlots();
      this.scheduleUpdate();
    });
    this.unsubscribeBindings = this.bindings.subscribe(this.scheduleUpdate);
    this.window.addEventListener('scroll', this.scheduleUpdate, { passive: true });
    this.window.addEventListener('resize', this.scheduleUpdate, { passive: true });
    this.installMutationObserver();
    this.installResizeObserver();
  }

  setLayer(layer: HTMLElement | undefined): void {
    if (this.destroyed) return;
    this.layer = layer;
    if (layer?.isConnected) this.refreshSlots();
    this.scheduleUpdate();
  }

  update(): void {
    if (this.destroyed || !this.layer?.isConnected) return;
    const observedElements: HTMLElement[] = [];
    const badges: HTMLElement[] = [];

    for (const slot of this.slots) {
      const cycle = latestCycle(slot);
      if (!cycle) continue;
      const binding = this.bindings.get(slot.runtimeSlotNumber);
      const element = binding.element;
      if (binding.binding.status !== 'bound' || !element?.isConnected) continue;

      const rectangle = element.getBoundingClientRect();
      if (!intersectsViewport(rectangle, this.window)) continue;
      observedElements.push(element);

      const badge = this.document.createElement('div');
      badge.className = 'tsgd-badge';
      badge.dataset.runtimeSlot = String(slot.runtimeSlotNumber);
      badge.textContent = badgeText(cycle);
      badge.style.maxWidth = `${BADGE_MAX_WIDTH_PX}px`;
      badge.style.left = `${Math.max(
        BADGE_EDGE_GUTTER_PX,
        Math.min(rectangle.left, this.window.innerWidth - BADGE_MAX_WIDTH_PX - BADGE_EDGE_GUTTER_PX)
      )}px`;
      if (rectangle.top >= 56) {
        badge.style.top = `${rectangle.top - 8}px`;
        badge.style.transform = 'translateY(-100%)';
      } else {
        badge.style.top = `${Math.max(
          BADGE_EDGE_GUTTER_PX,
          rectangle.top + BADGE_EDGE_GUTTER_PX
        )}px`;
      }
      badges.push(badge);
    }

    this.layer.replaceChildren(...badges);
    this.resizeObserver?.disconnect();
    for (const element of observedElements) this.resizeObserver?.observe(element);
  }

  destroy(): void {
    if (this.destroyed) return;
    this.destroyed = true;
    this.unsubscribeStore();
    this.unsubscribeBindings();
    this.window.removeEventListener('scroll', this.scheduleUpdate);
    this.window.removeEventListener('resize', this.scheduleUpdate);
    this.mutationObserver?.disconnect();
    this.resizeObserver?.disconnect();
    this.layer?.replaceChildren();
    this.layer = undefined;
  }

  private readonly scheduleUpdate = (): void => {
    if (this.destroyed || this.scheduled) return;
    this.scheduled = true;
    this.scheduleFrame(() => {
      this.scheduled = false;
      this.update();
    });
  };

  private refreshSlots(): void {
    this.slots = this.store.snapshot().slots;
  }

  private refreshSlotElementIds(): void {
    this.slotElementIds.clear();
    for (const slot of this.store.bindingInputs()) {
      if (slot.slotElementId) this.slotElementIds.add(slot.slotElementId);
    }
  }

  private installMutationObserver(): void {
    const Observer = this.window.MutationObserver;
    if (typeof Observer !== 'function' || !this.document.documentElement) return;
    this.mutationObserver = new Observer((records) => {
      if (!this.layer?.isConnected || this.slotElementIds.size === 0) return;
      if (records.some((record) => mutationIntersectsSlotIds(record, this.slotElementIds))) {
        this.scheduleUpdate();
      }
    });
    this.mutationObserver.observe(this.document.documentElement, {
      attributes: true,
      attributeFilter: ['id', 'style', 'class'],
      attributeOldValue: true,
      childList: true,
      subtree: true,
    });
  }

  private installResizeObserver(): void {
    const Observer = this.window.ResizeObserver;
    if (typeof Observer !== 'function') return;
    this.resizeObserver = new Observer(this.scheduleUpdate);
  }
}

export const gptDiagnosticsBadgeTextForTest = badgeText;
