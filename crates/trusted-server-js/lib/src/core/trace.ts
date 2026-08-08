// Closure-private render diagnostics and presentation for the hard-cutover runtime.
import type { RenderTraceDiagnostics, RenderTraceRecord } from "./types";

const MAX_RENDER_LOG_ENTRIES = 200;

/** DOM id of the diagnostics-owned floating trace panel. */
export const TRACE_PANEL_ID = "ts-render-trace-panel";

/** CSS class of the diagnostics-owned per-slot badge. */
export const TRACE_BADGE_CLASS = "ts-render-badge";

/** Whether an element and its ancestor chain are visibly presented. */
export function isEffectivelyVisible(el: Element | null): boolean {
  try {
    if (!el || !(el instanceof HTMLElement) || !el.isConnected) return false;
    const rect = el.getBoundingClientRect();
    if (rect.width <= 0 || rect.height <= 0) return false;
    let node: HTMLElement | null = el;
    while (node) {
      const style = getComputedStyle(node);
      if (
        style.display === "none" ||
        style.visibility === "hidden" ||
        Number.parseFloat(style.opacity || "1") === 0
      ) {
        return false;
      }
      node = node.parentElement;
    }
    return true;
  } catch {
    return false;
  }
}

type PanelStatus = "ok" | "hidden" | "gam-only" | "empty";

function panelStatus(record: Readonly<RenderTraceRecord>): PanelStatus {
  if (!record.rendered || record.gamEmpty === true) return "empty";
  if (record.visible !== true) return "hidden";
  return record.injected === true ? "ok" : "gam-only";
}

const STATUS_STYLE: Record<PanelStatus, { color: string; mark: string; label: string }> = {
  ok: { color: "#3fb950", mark: "✓", label: "ok" },
  hidden: { color: "#d29922", mark: "⚠", label: "hidden" },
  "gam-only": { color: "#58a6ff", mark: "◐", label: "gam-only" },
  empty: { color: "#f85149", mark: "✗", label: "empty" },
};

const MAX_RENDER_TRACE_SLOTS = 256;
const MAX_RENDER_TRACE_SUBSCRIBERS = 32;
const MAX_RENDER_TRACE_NOTIFICATIONS = 200;

type RenderTraceInputV1 = Omit<RenderTraceRecord, 'at' | 'count' | 'seq'>;
type RenderTraceUpdateV1 = Partial<Omit<RenderTraceRecord, 'at' | 'count' | 'seq' | 'slotId'>>;

/** Safe GPT fact shape admitted by the closure-private diagnostics bus. */
export interface RenderTraceGptFactV1 extends Readonly<Record<string, unknown>> {
  readonly kind:
    | 'slotRequested'
    | 'slotResponseReceived'
    | 'slotRenderEnded'
    | 'slotOnload'
    | 'impressionViewable'
    | 'slotVisibilityChanged';
  readonly slot: Readonly<{ readonly token: object; readonly elementId?: string }>;
  readonly isEmpty?: boolean;
  readonly inViewPercentage?: number;
}

/** Current registered-slot identity and presentation state for one safe GPT fact. */
export interface RenderTraceGptResolutionV1 {
  readonly slotId: string;
  readonly elementId?: string;
  readonly visible?: boolean;
}

export interface RenderTraceRuntimeScheduler {
  readonly set: (callback: () => void, milliseconds: number) => unknown;
  readonly clear: (handle: unknown) => void;
}

export interface RenderTraceRuntimeOptions {
  readonly document?: Document | undefined;
  readonly exportRecord?: (record: Readonly<RenderTraceRecord>) => void;
  readonly now?: () => number;
  readonly onOverflow?: (droppedNotifications: number) => void;
  readonly onPresentationError?: (error: unknown) => void;
  readonly onSubscriberError?: (error: unknown) => void;
  readonly overlayEnabled?: boolean;
  readonly schedule?: (callback: () => void) => () => void;
  readonly scheduler?: RenderTraceRuntimeScheduler;
}

export interface RenderTraceRuntimeOwner {
  readonly api: RenderTraceDiagnostics;
  readonly diagnostics: RenderTraceDiagnostics;
  readonly record: (input: RenderTraceInputV1) => Readonly<RenderTraceRecord>;
  readonly enrich: (
    recordOrSequence: Readonly<RenderTraceRecord> | number,
    patch: RenderTraceUpdateV1
  ) => Readonly<RenderTraceRecord> | undefined;
  readonly prune: (slotId: string, sequence?: number) => boolean;
  readonly observeGptFact: (
    fact: Readonly<RenderTraceGptFactV1>,
    resolve: (elementId: string | undefined) => RenderTraceGptResolutionV1 | undefined
  ) => void;
  readonly dispose: () => void;
}

export class DiagnosticsSubscriberLimitError extends Error {
  public readonly code = 'subscriber_capacity' as const;
  public readonly surface: 'renderTrace' | 'gpt';

  public constructor(surface: 'renderTrace' | 'gpt') {
    super('subscriber_capacity');
    this.name = 'DiagnosticsSubscriberLimitError';
    this.surface = surface;
  }
}

interface RenderTraceSubscription {
  readonly id: number;
  readonly listener: (record: Readonly<RenderTraceRecord>) => void;
}

interface PendingRenderTraceNotification {
  readonly record: Readonly<RenderTraceRecord>;
  readonly subscriberIds: readonly number[];
}

function copyRenderTraceRecord(record: Readonly<RenderTraceRecord>): Readonly<RenderTraceRecord> {
  const copy: Record<string, unknown> = {
    slotId: record.slotId,
    path: record.path,
    rendered: record.rendered,
  };
  const optional = [
    'elementId',
    'auctionId',
    'bidder',
    'adId',
    'bidId',
    'creativeId',
    'admHash',
    'servedFrom',
    'gamEmpty',
    'injected',
    'visible',
  ] as const;
  for (const key of optional) {
    const value = record[key];
    if (value !== undefined) copy[key] = value;
  }
  copy.count = record.count;
  copy.seq = record.seq;
  copy.at = record.at;
  return Object.freeze(copy) as unknown as Readonly<RenderTraceRecord>;
}

function scheduleRenderTraceTask(callback: () => void): () => void {
  const handle = globalThis.setTimeout(callback, 0);
  return (): void => globalThis.clearTimeout(handle);
}

const RUNTIME_TRACE_ATTRIBUTES = [
  'data-ts-slot-id',
  'data-ts-render-path',
  'data-ts-rendered',
  'data-ts-auction-id',
  'data-ts-bidder',
  'data-ts-ad-id',
  'data-ts-bid-id',
  'data-ts-creative-id',
  'data-ts-adm-hash',
  'data-ts-served-from',
  'data-ts-gam-empty',
  'data-ts-injected',
  'data-ts-visible',
] as const;

interface PresentedTraceSlot {
  readonly element: HTMLElement;
  readonly priorInlinePosition?: string;
}

interface RenderTracePresentation {
  readonly present: (record: Readonly<RenderTraceRecord>) => void;
  readonly prune: (slotId: string) => void;
  readonly dispose: () => void;
}

function createRenderTracePresentation(
  options: RenderTraceRuntimeOptions,
  history: () => readonly Readonly<RenderTraceRecord>[]
): RenderTracePresentation {
  const targetDocument =
    options.document ?? (typeof document === 'undefined' ? undefined : document);
  const overlayEnabled = options.overlayEnabled === true;
  const presented = new Map<string, PresentedTraceSlot>();
  const panelRecords = new Map<number, Readonly<RenderTraceRecord>>();
  const panelRows = new Map<number, HTMLButtonElement>();
  let panel: HTMLElement | undefined;
  let panelHeading: HTMLElement | undefined;
  let panelRowsHost: HTMLElement | undefined;

  const report = (error: unknown): void => {
    try {
      options.onPresentationError?.(error);
    } catch {
      // Presentation reporting is diagnostics-only.
    }
  };

  const removeBadge = (element: HTMLElement): void => {
    for (const badge of element.querySelectorAll(`:scope > .${TRACE_BADGE_CLASS}`)) badge.remove();
  };

  const clearElement = (presentedSlot: PresentedTraceSlot): void => {
    const { element, priorInlinePosition } = presentedSlot;
    for (const attribute of RUNTIME_TRACE_ATTRIBUTES) element.removeAttribute(attribute);
    removeBadge(element);
    if (priorInlinePosition !== undefined && element.style.position === 'relative') {
      element.style.position = priorInlinePosition;
    }
  };

  const createBadge = (
    element: HTMLElement,
    record: Readonly<RenderTraceRecord>
  ): PresentedTraceSlot => {
    let priorInlinePosition: string | undefined;
    try {
      const position = targetDocument?.defaultView?.getComputedStyle(element).position;
      if (position === 'static' || position === '') {
        priorInlinePosition = element.style.position;
        element.style.position = 'relative';
      }
    } catch {
      // A badge remains noninteractive even if its containing block is publisher-owned.
    }
    const status = panelStatus(record);
    const style = STATUS_STYLE[status];
    const badge = targetDocument?.createElement('div');
    if (!badge) {
      return {
        element,
        ...(priorInlinePosition === undefined ? {} : { priorInlinePosition }),
      };
    }
    badge.className = TRACE_BADGE_CLASS;
    badge.textContent =
      `TS ${style.mark} #${record.seq}` +
      `${record.bidder ? ` · ${record.bidder}` : ''}` +
      `${style.label === 'ok' ? '' : ` · ${style.label}`}`;
    badge.style.setProperty('position', 'absolute');
    badge.style.setProperty('top', '4px');
    badge.style.setProperty('left', '4px');
    badge.style.setProperty('z-index', '2147483646');
    badge.style.setProperty('pointer-events', 'none');
    badge.style.setProperty('font', '10px/1.5 ui-monospace, Menlo, Consolas, monospace');
    badge.style.setProperty('padding', '1px 5px');
    badge.style.setProperty('color', '#fff');
    badge.style.setProperty('background', style.color);
    badge.style.setProperty('border-radius', '3px');
    element.appendChild(badge);
    return { element, ...(priorInlinePosition === undefined ? {} : { priorInlinePosition }) };
  };

  const exportRow = (record: Readonly<RenderTraceRecord>): void => {
    const copied = copyRenderTraceRecord(record);
    try {
      if (options.exportRecord) {
        options.exportRecord(copied);
        return;
      }
      const clipboard = targetDocument?.defaultView?.navigator.clipboard;
      const write = clipboard?.writeText;
      if (typeof write !== 'function') return;
      const pending = Reflect.apply(write, clipboard, [JSON.stringify(copied, null, 2)]) as
        Promise<void> | undefined;
      void pending?.catch(report);
    } catch (error) {
      report(error);
    }
  };

  const renderPanel = (record?: Readonly<RenderTraceRecord>): void => {
    if (!overlayEnabled || !targetDocument?.body) return;
    if (!panel) {
      const collision = targetDocument.getElementById(TRACE_PANEL_ID);
      if (collision) return;
      panel = targetDocument.createElement('div');
      panel.id = TRACE_PANEL_ID;
      panel.setAttribute('data-ts-render-trace-owner', '1');
      panel.style.setProperty('position', 'fixed');
      panel.style.setProperty('bottom', '12px');
      panel.style.setProperty('right', '12px');
      panel.style.setProperty('z-index', '2147483647');
      panel.style.setProperty('max-width', '360px');
      panel.style.setProperty('max-height', '45vh');
      panel.style.setProperty('overflow', 'auto');
      panel.style.setProperty('background', 'rgba(17,17,17,0.94)');
      panel.style.setProperty('color', '#eee');
      panel.style.setProperty('font', '11px/1.5 ui-monospace, Menlo, Consolas, monospace');
      panel.style.setProperty('border', '1px solid #333');
      panel.style.setProperty('border-radius', '6px');
      panel.style.setProperty('box-shadow', '0 4px 16px rgba(0,0,0,0.4)');
      panelHeading = targetDocument.createElement('div');
      panelHeading.style.setProperty('padding', '6px 10px');
      panelHeading.style.setProperty('font-weight', '700');
      panelRowsHost = targetDocument.createElement('div');
      panel.append(panelHeading, panelRowsHost);
      targetDocument.body.appendChild(panel);
    }
    const retained = history();
    panelHeading!.textContent = `TS Render Trace · ${retained.length} renders`;
    const retainedSequences = new Set(retained.map(({ seq }) => seq));
    for (const [sequence, row] of panelRows) {
      if (retainedSequences.has(sequence)) continue;
      row.remove();
      panelRows.delete(sequence);
      panelRecords.delete(sequence);
    }
    if (record && retainedSequences.has(record.seq)) {
      panelRecords.set(record.seq, record);
      let row = panelRows.get(record.seq);
      if (!row) {
        row = targetDocument.createElement('button');
        row.type = 'button';
        row.setAttribute('data-ts-trace-seq', String(record.seq));
        row.style.setProperty('display', 'block');
        row.style.setProperty('width', '100%');
        row.style.setProperty('padding', '6px 10px');
        row.style.setProperty('border', '0');
        row.style.setProperty('border-top', '1px solid #2a2a2a');
        row.style.setProperty('background', 'transparent');
        row.style.setProperty('font', 'inherit');
        row.style.setProperty('text-align', 'left');
        row.style.setProperty('cursor', 'pointer');
        row.addEventListener('click', () => {
          const exported = panelRecords.get(record.seq);
          if (exported) exportRow(exported);
        });
        panelRows.set(record.seq, row);
        panelRowsHost!.prepend(row);
      }
      const status = panelStatus(record);
      const style = STATUS_STYLE[status];
      row.textContent = `#${record.seq} ${style.mark} ${record.slotId} · ${style.label} · ${record.path}`;
      row.style.setProperty('border-left', `3px solid ${style.color}`);
      row.style.setProperty('color', style.color);
    }
  };

  const present = (record: Readonly<RenderTraceRecord>): void => {
    try {
      const prior = presented.get(record.slotId);
      const elementId = record.elementId ?? record.slotId;
      const candidate = targetDocument?.getElementById(elementId);
      const element = candidate && candidate instanceof HTMLElement ? candidate : undefined;
      if (prior && prior.element !== element) {
        clearElement(prior);
        presented.delete(record.slotId);
      }
      if (element) {
        const retainedPosition = prior?.element === element ? prior.priorInlinePosition : undefined;
        removeBadge(element);
        const values: Readonly<
          Record<(typeof RUNTIME_TRACE_ATTRIBUTES)[number], string | undefined>
        > = {
          'data-ts-slot-id': record.slotId,
          'data-ts-render-path': record.path,
          'data-ts-rendered': String(record.rendered),
          'data-ts-auction-id': record.auctionId,
          'data-ts-bidder': record.bidder,
          'data-ts-ad-id': record.adId,
          'data-ts-bid-id': record.bidId,
          'data-ts-creative-id': record.creativeId,
          'data-ts-adm-hash': record.admHash,
          'data-ts-served-from': record.servedFrom,
          'data-ts-gam-empty': record.gamEmpty === undefined ? undefined : String(record.gamEmpty),
          'data-ts-injected': record.injected === undefined ? undefined : String(record.injected),
          'data-ts-visible': record.visible === undefined ? undefined : String(record.visible),
        };
        for (const attribute of RUNTIME_TRACE_ATTRIBUTES) {
          const value = values[attribute];
          if (value === undefined || value === '') element.removeAttribute(attribute);
          else element.setAttribute(attribute, value);
        }
        const status = panelStatus(record);
        if (
          overlayEnabled &&
          element.tagName !== 'IFRAME' &&
          (status === 'ok' || status === 'gam-only')
        ) {
          const next = createBadge(element, record);
          presented.set(record.slotId, {
            element,
            ...(retainedPosition === undefined
              ? next.priorInlinePosition === undefined
                ? {}
                : { priorInlinePosition: next.priorInlinePosition }
              : { priorInlinePosition: retainedPosition }),
          });
        } else {
          if (retainedPosition !== undefined && element.style.position === 'relative') {
            element.style.position = retainedPosition;
          }
          presented.set(record.slotId, { element });
        }
      }
      renderPanel(record);
    } catch (error) {
      report(error);
    }
  };

  const prune = (slotId: string): void => {
    try {
      const existing = presented.get(slotId);
      if (existing) clearElement(existing);
      presented.delete(slotId);
      renderPanel();
    } catch (error) {
      report(error);
    }
  };

  const dispose = (): void => {
    for (const slotId of [...presented.keys()]) prune(slotId);
    try {
      panel?.remove();
    } catch (error) {
      report(error);
    }
    panel = undefined;
    panelHeading = undefined;
    panelRowsHost = undefined;
    panelRecords.clear();
    panelRows.clear();
  };

  return Object.freeze({ present, prune, dispose });
}

/** Create one document-runtime render trace without exposing its mutation authority. */
export function createRenderTraceDiagnostics(
  options: RenderTraceRuntimeOptions = {}
): RenderTraceRuntimeOwner {
  const current = new Map<string, Readonly<RenderTraceRecord>>();
  const counts = new Map<string, number>();
  const history: Array<Readonly<RenderTraceRecord>> = [];
  const recordsBySequence = new Map<number, Readonly<RenderTraceRecord>>();
  const gptImpressions = new Map<
    object,
    {
      readonly baselineSequence: number | undefined;
      reconciled?: boolean;
      renderEnded?: boolean;
      sequence?: number;
      readonly slotId: string;
    }
  >();
  const subscribers = new Map<number, RenderTraceSubscription>();
  const pendingOrder: number[] = [];
  const pendingBySequence = new Map<number, PendingRenderTraceNotification>();
  let sequence = 0;
  let subscriberSequence = 0;
  let droppedNotifications = 0;
  let reportedDroppedNotifications = 0;
  let cancelScheduled: (() => void) | undefined;
  let disposed = false;
  const presentation = createRenderTracePresentation(options, () => history);

  const schedule = (callback: () => void): (() => void) => {
    if (options.schedule) return options.schedule(callback);
    if (options.scheduler) {
      const handle = options.scheduler.set(callback, 0);
      return (): void => options.scheduler?.clear(handle);
    }
    return scheduleRenderTraceTask(callback);
  };

  const reportSubscriberError = (error: unknown): void => {
    try {
      options.onSubscriberError?.(error);
    } catch {
      // Diagnostics error reporting cannot affect correctness work.
    }
  };

  const drain = (): void => {
    cancelScheduled = undefined;
    if (droppedNotifications !== reportedDroppedNotifications) {
      reportedDroppedNotifications = droppedNotifications;
      try {
        options.onOverflow?.(droppedNotifications);
      } catch {
        // Diagnostics-only overflow reporting stays inside the diagnostics task.
      }
    }
    while (!disposed && pendingOrder.length > 0) {
      const next = pendingOrder.shift();
      if (next === undefined) continue;
      const pending = pendingBySequence.get(next);
      pendingBySequence.delete(next);
      if (!pending) continue;
      for (const id of pending.subscriberIds) {
        const subscription = subscribers.get(id);
        if (!subscription) continue;
        try {
          subscription.listener(pending.record);
        } catch (error) {
          reportSubscriberError(error);
        }
      }
    }
  };

  const ensureDrain = (): boolean => {
    if (cancelScheduled) return true;
    try {
      const cancel = schedule(drain);
      if (typeof cancel !== 'function') throw new TypeError('invalid diagnostics scheduler');
      if (!disposed && pendingOrder.length > 0) cancelScheduled = cancel;
      return true;
    } catch {
      pendingOrder.length = 0;
      pendingBySequence.clear();
      cancelScheduled = undefined;
      return false;
    }
  };

  const enqueue = (record: Readonly<RenderTraceRecord>): void => {
    if (disposed || subscribers.size === 0) return;
    const pending = Object.freeze({
      record: copyRenderTraceRecord(record),
      subscriberIds: Object.freeze([...subscribers.keys()]),
    });
    if (pendingBySequence.has(record.seq)) {
      pendingBySequence.set(record.seq, pending);
      return;
    }
    if (pendingOrder.length >= MAX_RENDER_TRACE_NOTIFICATIONS) {
      const dropped = pendingOrder.shift();
      if (dropped !== undefined) pendingBySequence.delete(dropped);
      droppedNotifications += 1;
    }
    pendingOrder.push(record.seq);
    pendingBySequence.set(record.seq, pending);
    ensureDrain();
  };

  const retained = (record: Readonly<RenderTraceRecord>): boolean =>
    current.get(record.slotId)?.seq === record.seq ||
    history.some((candidate) => candidate.seq === record.seq);

  const record = (input: RenderTraceInputV1): Readonly<RenderTraceRecord> => {
    if (!disposed && input.path !== 'gam-refresh') {
      for (const impression of gptImpressions.values()) {
        if (
          impression.slotId !== input.slotId ||
          impression.renderEnded !== true ||
          impression.reconciled === true ||
          impression.sequence === undefined ||
          current.get(input.slotId)?.seq !== impression.sequence
        ) {
          continue;
        }
        const reconciled = enrich(impression.sequence, input);
        if (reconciled) {
          impression.reconciled = true;
          return reconciled;
        }
      }
    }
    const previous = current.get(input.slotId);
    const evictedCurrentSlot =
      !previous && current.size >= MAX_RENDER_TRACE_SLOTS
        ? (current.keys().next().value as string | undefined)
        : undefined;
    let at: number;
    try {
      at = (options.now ?? Date.now)();
    } catch {
      at = Date.now();
    }
    const previousCount = counts.get(input.slotId) ?? 0;
    if (!counts.has(input.slotId) && counts.size >= MAX_RENDER_TRACE_SLOTS) {
      let evictedCounter: string | undefined;
      for (const candidate of counts.keys()) {
        if (!current.has(candidate)) {
          evictedCounter = candidate;
          break;
        }
      }
      evictedCounter ??= evictedCurrentSlot;
      if (evictedCounter !== undefined) counts.delete(evictedCounter);
    }
    counts.set(input.slotId, previousCount + 1);
    const committed = copyRenderTraceRecord({
      ...input,
      count: previousCount + 1,
      seq: (sequence += 1),
      at,
    });
    if (disposed) return committed;
    if (evictedCurrentSlot !== undefined) {
      current.delete(evictedCurrentSlot);
      presentation.prune(evictedCurrentSlot);
    }
    current.set(committed.slotId, committed);
    recordsBySequence.set(committed.seq, committed);
    history.push(committed);
    if (history.length > MAX_RENDER_LOG_ENTRIES) {
      const evicted = history.shift();
      if (evicted && !retained(evicted)) recordsBySequence.delete(evicted.seq);
    }
    if (previous && !retained(previous)) recordsBySequence.delete(previous.seq);
    enqueue(committed);
    presentation.present(committed);
    return committed;
  };

  const enrich = (
    recordOrSequence: Readonly<RenderTraceRecord> | number,
    patch: RenderTraceUpdateV1
  ): Readonly<RenderTraceRecord> | undefined => {
    if (disposed) return undefined;
    const targetSequence =
      typeof recordOrSequence === 'number' ? recordOrSequence : recordOrSequence?.seq;
    if (!Number.isSafeInteger(targetSequence) || targetSequence <= 0) return undefined;
    const existing = recordsBySequence.get(targetSequence);
    if (!existing) return undefined;
    const injected =
      existing.injected === true || patch.injected === true
        ? { injected: true as const }
        : existing.injected === false || patch.injected === false
          ? { injected: false as const }
          : {};
    const merged = {
      ...existing,
      ...patch,
      rendered:
        existing.rendered === true && patch.rendered === false
          ? true
          : (patch.rendered ?? existing.rendered),
      ...injected,
      slotId: existing.slotId,
      count: existing.count,
      seq: existing.seq,
      at: existing.at,
    } as RenderTraceRecord;
    const committed = copyRenderTraceRecord(merged);
    recordsBySequence.set(targetSequence, committed);
    if (current.get(existing.slotId)?.seq === targetSequence) {
      current.set(existing.slotId, committed);
    }
    const historyIndex = history.findIndex(({ seq }) => seq === targetSequence);
    if (historyIndex >= 0) history[historyIndex] = committed;
    enqueue(committed);
    presentation.present(committed);
    return committed;
  };

  const prune = (slotId: string, expectedSequence?: number): boolean => {
    if (disposed || typeof slotId !== 'string') return false;
    const existing = current.get(slotId);
    if (!existing || (expectedSequence !== undefined && existing.seq !== expectedSequence)) {
      return false;
    }
    current.delete(slotId);
    if (!retained(existing)) recordsBySequence.delete(existing.seq);
    presentation.prune(slotId);
    return true;
  };

  const observeGptFact = (
    fact: Readonly<RenderTraceGptFactV1>,
    resolve: (elementId: string | undefined) => RenderTraceGptResolutionV1 | undefined
  ): void => {
    if (disposed || typeof resolve !== 'function') return;
    try {
      const token = fact.slot.token;
      if (typeof token !== 'object' || token === null || !Object.isFrozen(token)) return;
      const resolution = resolve(fact.slot.elementId);
      if (!resolution || typeof resolution.slotId !== 'string' || resolution.slotId === '') return;

      if (fact.kind === 'slotRequested') {
        for (const [candidateToken, impression] of gptImpressions) {
          if (impression.slotId === resolution.slotId) gptImpressions.delete(candidateToken);
        }
        if (gptImpressions.size >= MAX_RENDER_TRACE_SLOTS) {
          const oldestToken = gptImpressions.keys().next().value as object | undefined;
          if (oldestToken) gptImpressions.delete(oldestToken);
        }
        gptImpressions.set(token, {
          baselineSequence: current.get(resolution.slotId)?.seq,
          slotId: resolution.slotId,
        });
        return;
      }

      const impression = gptImpressions.get(token);
      if (!impression || impression.slotId !== resolution.slotId) return;
      if (fact.kind === 'slotResponseReceived') return;
      if (fact.kind === 'slotRenderEnded') {
        if (typeof fact.isEmpty !== 'boolean') return;
        if (impression.renderEnded) return;
        impression.renderEnded = true;
        const latest = current.get(impression.slotId);
        const target =
          latest && latest.seq !== impression.baselineSequence
            ? latest
            : record({
                slotId: impression.slotId,
                path: 'gam-refresh',
                rendered: !fact.isEmpty,
                gamEmpty: fact.isEmpty,
                injected: false,
                ...(resolution.elementId === undefined ? {} : { elementId: resolution.elementId }),
                ...(resolution.visible === undefined
                  ? {}
                  : { visible: !fact.isEmpty && resolution.visible }),
                servedFrom: 'gam',
              });
        const enriched = enrich(target, {
          rendered: !fact.isEmpty,
          gamEmpty: fact.isEmpty,
          injected: false,
          ...(target.servedFrom === undefined ? { servedFrom: 'gam' as const } : {}),
          ...(resolution.elementId === undefined ? {} : { elementId: resolution.elementId }),
          ...(resolution.visible === undefined
            ? {}
            : { visible: !fact.isEmpty && resolution.visible }),
        });
        impression.sequence = enriched?.seq ?? target.seq;
        return;
      }

      const targetSequence = impression.sequence;
      if (targetSequence === undefined || current.get(impression.slotId)?.seq !== targetSequence) {
        return;
      }
      if (fact.kind === 'impressionViewable') {
        enrich(targetSequence, { visible: true });
      } else if (
        fact.kind === 'slotVisibilityChanged' &&
        typeof fact.inViewPercentage === 'number' &&
        Number.isFinite(fact.inViewPercentage)
      ) {
        enrich(targetSequence, { visible: fact.inViewPercentage > 0 });
      } else if (fact.kind === 'slotOnload' && resolution.visible !== undefined) {
        enrich(targetSequence, { visible: resolution.visible });
      }
    } catch {
      // GPT diagnostics cannot affect the committed render or adapter callback.
    }
  };

  const api: RenderTraceDiagnostics = Object.freeze({
    current: (): Readonly<Record<string, Readonly<RenderTraceRecord>>> => {
      const snapshot = Object.create(null) as Record<string, Readonly<RenderTraceRecord>>;
      for (const [slotId, traceRecord] of current) {
        Object.defineProperty(snapshot, slotId, {
          configurable: false,
          enumerable: true,
          value: copyRenderTraceRecord(traceRecord),
          writable: false,
        });
      }
      return Object.freeze(snapshot);
    },
    history: (): readonly Readonly<RenderTraceRecord>[] =>
      Object.freeze(history.map((traceRecord) => copyRenderTraceRecord(traceRecord))),
    subscribe: (listener: (record: Readonly<RenderTraceRecord>) => void): (() => void) => {
      if (typeof listener !== 'function')
        throw new TypeError('diagnostics listener must be callable');
      if (disposed) return () => undefined;
      if (subscribers.size >= MAX_RENDER_TRACE_SUBSCRIBERS) {
        throw new DiagnosticsSubscriberLimitError('renderTrace');
      }
      const id = (subscriberSequence += 1);
      const subscription = Object.freeze({ id, listener });
      subscribers.set(id, subscription);
      let active = true;
      return (): void => {
        if (!active) return;
        active = false;
        if (subscribers.get(id) === subscription) subscribers.delete(id);
      };
    },
  });

  const dispose = (): void => {
    if (disposed) return;
    disposed = true;
    try {
      cancelScheduled?.();
    } catch {
      // The disposed latch suppresses a hostile late callback.
    }
    cancelScheduled = undefined;
    subscribers.clear();
    pendingOrder.length = 0;
    pendingBySequence.clear();
    current.clear();
    history.length = 0;
    recordsBySequence.clear();
    counts.clear();
    gptImpressions.clear();
    presentation.dispose();
  };

  return Object.freeze({
    api,
    diagnostics: api,
    record,
    enrich,
    prune,
    observeGptFact,
    dispose,
  });
}

/** Short name used by the browser composition owner. */
export const createRenderTrace = createRenderTraceDiagnostics;
