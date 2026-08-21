import type { GptDiagnosticsRequestCycle } from '../../core/types';
import { realmOwnedElement, realmOwnedHtmlElement } from '../../shared/realm';

import type { GptDiagnosticsBindingManager } from './binding';
import { unhandledCase } from './exhaustive';
import { formatSizes, scheduleFrame } from './presentation_helpers';
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
  MutationObserver?: typeof MutationObserver | undefined;
  ResizeObserver?: typeof ResizeObserver | undefined;
};

const BADGE_MAX_WIDTH_PX = 260;
const BADGE_EDGE_GUTTER_PX = 4;
const MAX_BADGE_REQUESTED_SLOT_SIZES = 3;

interface BadgeOptions {
  window?: BadgeWindow | undefined;
  document?: Document | undefined;
  scheduleFrame?: ((callback: () => void) => () => void) | undefined;
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

function nodeIntersectsSlotIds(
  node: Node,
  slotElementIds: Set<string>,
  targetWindow: BadgeWindow
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
  targetWindow: BadgeWindow
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

function formatMilliseconds(value: number | undefined): string | undefined {
  if (value === undefined || !Number.isFinite(value) || value < 0) return undefined;
  if (value >= 1000) return `${Math.round(value / 100) / 10} s`;
  return `${Math.round(value)} ms`;
}

/** Use the store's evidence ladder verbatim; presentation never re-derives ownership. */
function deliveryLabel(cycle: GptDiagnosticsRequestCycle): string | undefined {
  switch (cycle.delivery) {
    case 'trusted_server_response_sent':
      return 'TS response sent';
    case 'trusted_server_selected':
      return 'TS selected';
    case 'pending':
      return 'TS candidate (pending)';
    case 'candidate_unconfirmed':
      return 'TS unconfirmed';
    case 'no_candidate':
      return 'No TS candidate';
    case 'unknown':
      return 'Delivery unknown';
    case 'not_applicable':
    case undefined:
      return undefined;
    default:
      return unhandledCase(cycle.delivery);
  }
}

/** Format one observed GPT request cycle for both badge and accessible text surfaces. */
export function formatGptDiagnosticsBadgeText(cycle: GptDiagnosticsRequestCycle): string {
  const firstLine: string[] = [];
  if (cycle.isEmpty === true) firstLine.push('Empty');
  else if (cycle.isEmpty === false) firstLine.push('Filled');
  else if (cycle.renderAtMs !== undefined) firstLine.push('Rendered (fill unknown)');
  else firstLine.push('Pending');
  const delivery = deliveryLabel(cycle);
  if (delivery) firstLine.push(delivery);
  if (cycle.requestPath === 'competing') firstLine.push('Competing paths');
  if (cycle.requestedSlotSizes) {
    const displayedSizes = cycle.requestedSlotSizes.slice(0, MAX_BADGE_REQUESTED_SLOT_SIZES);
    const remainingSizeCount = cycle.requestedSlotSizes.length - displayedSizes.length;
    firstLine.push(
      `Requested ${formatSizes(displayedSizes)}${
        remainingSizeCount > 0 ? ` +${remainingSizeCount}` : ''
      }`
    );
  }
  if (cycle.size) firstLine.push(`GPT fill ${cycle.size[0]}×${cycle.size[1]}`);
  if (cycle.observedSlotSize) {
    firstLine.push(`Outer box ${cycle.observedSlotSize[0]}×${cycle.observedSlotSize[1]}`);
  }

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
  private readonly scheduleFrame: (callback: () => void) => () => void;
  private readonly unsubscribeStore: () => void;
  private readonly unsubscribeBindings: () => void;
  private readonly slotElementIds = new Set<string>();
  private slots: GptDiagnosticsStoreSlotSnapshot[] = [];
  private layer: HTMLElement | undefined;
  private mutationObserver?: MutationObserver;
  private resizeObserver?: ResizeObserver;
  private cancelScheduledUpdate: (() => void) | undefined;
  private scheduled = false;
  private destroyed = false;

  constructor(store: BadgeStore, bindings: BadgeBindings, options: BadgeOptions = {}) {
    this.store = store;
    this.bindings = bindings;
    this.document = options.document ?? document;
    this.window =
      options.window ??
      (this.document.defaultView as unknown as BadgeWindow | null) ??
      (window as unknown as BadgeWindow);
    this.scheduleFrame =
      options.scheduleFrame ?? ((callback) => scheduleFrame(this.window, callback));
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
    this.layer = realmOwnedHtmlElement(layer, this.window);
    if (this.layer?.isConnected) this.refreshSlots();
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
      const element = realmOwnedHtmlElement(binding.element, this.window);
      if (binding.binding.status !== 'bound' || !element?.isConnected) continue;

      const rectangle = element.getBoundingClientRect();
      if (!intersectsViewport(rectangle, this.window)) continue;
      observedElements.push(element);

      const badge = this.document.createElement('div');
      badge.className = 'tsgd-badge';
      badge.dataset.runtimeSlot = String(slot.runtimeSlotNumber);
      badge.textContent = formatGptDiagnosticsBadgeText(cycle);
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
    const cancelUpdate = this.cancelScheduledUpdate;
    this.cancelScheduledUpdate = undefined;
    this.scheduled = false;
    try {
      cancelUpdate?.();
    } catch {
      // Continue releasing every independently owned badge resource.
    }
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
    let active = true;
    let cancelFrame: (() => void) | undefined;
    const run = (): void => {
      if (!active) return;
      active = false;
      this.cancelScheduledUpdate = undefined;
      try {
        cancelFrame?.();
      } catch {
        // A completed frame remains authoritative when scheduler cleanup fails.
      }
      this.scheduled = false;
      this.update();
    };
    try {
      cancelFrame = this.scheduleFrame(run);
      if (typeof cancelFrame !== 'function') {
        throw new TypeError('Invalid badge frame scheduler');
      }
      if (active) {
        this.cancelScheduledUpdate = (): void => {
          if (!active) return;
          active = false;
          this.scheduled = false;
          cancelFrame?.();
        };
      } else {
        cancelFrame();
      }
    } catch {
      active = false;
      this.cancelScheduledUpdate = undefined;
      this.scheduled = false;
    }
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
      if (
        records.some((record) =>
          mutationIntersectsSlotIds(record, this.slotElementIds, this.window)
        )
      ) {
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
