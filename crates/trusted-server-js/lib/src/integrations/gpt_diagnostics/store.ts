import type {
  GptDiagnosticsCallbackDisposition,
  GptDiagnosticsCallbackIssue,
  GptDiagnosticsCallbackKind,
  GptDiagnosticsCoverageCounters,
  GptDiagnosticsDurations,
  GptDiagnosticsRequestCycle,
  Size,
} from '../../core/types';

export const MAX_DIAGNOSTIC_SLOTS = 64;
export const MAX_REQUEST_CYCLES_PER_SLOT = 10;
export const MAX_CALLBACK_ISSUES = 128;

const CALLBACK_KINDS: GptDiagnosticsCallbackKind[] = [
  'slotRequested',
  'slotResponseReceived',
  'slotRenderEnded',
  'slotOnload',
  'impressionViewable',
  'slotVisibilityChanged',
];

export interface GptDiagnosticsSlotLike {
  getSlotElementId?: (() => string) | undefined;
  getAdUnitPath?: (() => string) | undefined;
}

export interface GptRenderFacts {
  isEmpty?: boolean | undefined;
  size?: Size | undefined;
  isBackfill?: boolean | undefined;
  slotContentChanged?: boolean | undefined;
}

export interface GptDiagnosticsStoreSlotSnapshot {
  runtimeSlotNumber: number;
  slotElementId?: string | undefined;
  adUnitPath?: string | undefined;
  currentVisibilityPercentage?: number | undefined;
  maximumVisibilityPercentage?: number | undefined;
  requests: GptDiagnosticsRequestCycle[];
}

export interface GptDiagnosticsBindingInput {
  runtimeSlotNumber: number;
  slotElementId?: string | undefined;
}

export interface GptDiagnosticsStoreSnapshot {
  gptObserved: boolean;
  slots: GptDiagnosticsStoreSlotSnapshot[];
  callbackIssues: GptDiagnosticsCallbackIssue[];
  coverage: Record<GptDiagnosticsCallbackKind, GptDiagnosticsCoverageCounters>;
  metadata: {
    droppedCallbacks: number;
    evictedSlots: number;
    evictedRequestCycles: number;
  };
}

type MutableRequestCycle = GptDiagnosticsRequestCycle;

interface MutableSlotRecord {
  runtimeSlotNumber: number;
  slotElementId?: string | undefined;
  adUnitPath?: string | undefined;
  currentVisibilityPercentage?: number | undefined;
  maximumVisibilityPercentage?: number | undefined;
  requests: MutableRequestCycle[];
}

interface StoreOptions {
  now?: (() => number) | undefined;
  schedule?: ((callback: () => void) => void) | undefined;
}

type StoreListener = () => void;

type CompatibleCycle = (cycle: MutableRequestCycle) => boolean;

function emptyCoverage(): Record<GptDiagnosticsCallbackKind, GptDiagnosticsCoverageCounters> {
  return Object.fromEntries(
    CALLBACK_KINDS.map((kind) => [kind, { observed: 0, matched: 0, unmatched: 0, ambiguous: 0 }])
  ) as Record<GptDiagnosticsCallbackKind, GptDiagnosticsCoverageCounters>;
}

function optionalNonEmptyString(read: (() => string) | undefined): string | undefined {
  if (typeof read !== 'function') return undefined;

  try {
    const value = read();
    return typeof value === 'string' && value.length > 0 ? value : undefined;
  } catch {
    return undefined;
  }
}

function validDuration(start: number | undefined, end: number | undefined): number | undefined {
  if (
    start === undefined ||
    end === undefined ||
    !Number.isFinite(start) ||
    !Number.isFinite(end)
  ) {
    return undefined;
  }

  const duration = end - start;
  return duration >= 0 ? duration : undefined;
}

function derivedDurations(cycle: MutableRequestCycle): GptDiagnosticsDurations {
  return {
    requestToResponseMs: validDuration(cycle.requestedAtMs, cycle.responseAtMs),
    responseToRenderMs: validDuration(cycle.responseAtMs, cycle.renderAtMs),
    requestToRenderMs: validDuration(cycle.requestedAtMs, cycle.renderAtMs),
    renderToLoadMs: validDuration(cycle.renderAtMs, cycle.loadAtMs),
    renderToViewableMs: validDuration(cycle.renderAtMs, cycle.viewableAtMs),
  };
}

function copyCycle(cycle: MutableRequestCycle): GptDiagnosticsRequestCycle {
  return {
    ...cycle,
    durations: derivedDurations(cycle),
    size: cycle.size ? ([...cycle.size] as Size) : undefined,
  };
}

/** Bounded store for facts observed directly from GPT callbacks. */
export class GptDiagnosticsStore {
  private readonly now: () => number;
  private readonly schedule: (callback: () => void) => void;
  private readonly slotNumbers = new WeakMap<object, number>();
  private readonly requestNumbers = new WeakMap<object, number>();
  private readonly slots = new Map<number, MutableSlotRecord>();
  private readonly slotOrder: number[] = [];
  private readonly slotActivityOrder: number[] = [];
  private readonly listeners = new Set<StoreListener>();
  private readonly coverage = emptyCoverage();
  private readonly callbackIssues: GptDiagnosticsCallbackIssue[] = [];
  private readonly metadata = {
    droppedCallbacks: 0,
    evictedSlots: 0,
    evictedRequestCycles: 0,
  };
  private nextRuntimeSlotNumber = 1;
  private notificationScheduled = false;
  private gptObserved = false;

  constructor(options: StoreOptions = {}) {
    this.now = options.now ?? (() => performance.now());
    this.schedule = options.schedule ?? ((callback) => queueMicrotask(callback));
  }

  markGptObserved(): void {
    if (this.gptObserved) return;
    this.gptObserved = true;
    this.notify();
  }

  subscribe(listener: StoreListener): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  recordSlotRequested(slot: GptDiagnosticsSlotLike): void {
    const timestampMs = this.timestamp();
    const record = this.prepareCallback('slotRequested', slot, timestampMs);
    if (!record) return;

    if (record.requests.length >= MAX_REQUEST_CYCLES_PER_SLOT) {
      record.requests.shift();
      this.metadata.evictedRequestCycles += 1;
    }

    const requestNumber = (this.requestNumbers.get(slot) ?? 0) + 1;
    this.requestNumbers.set(slot, requestNumber);
    record.requests.push({
      requestNumber,
      requestedAtMs: timestampMs,
      durations: {},
      incompleteSequence: false,
    });
    this.incrementDisposition('slotRequested', 'matched');
    this.notify();
  }

  recordSlotResponseReceived(slot: GptDiagnosticsSlotLike): void {
    const timestampMs = this.timestamp();
    this.matchCycle(
      'slotResponseReceived',
      slot,
      timestampMs,
      (cycle) => cycle.requestedAtMs !== undefined && cycle.responseAtMs === undefined,
      (record, cycle) => {
        cycle.responseAtMs = timestampMs;
        if (
          validDuration(cycle.requestedAtMs, timestampMs) === undefined ||
          (cycle.renderAtMs !== undefined && timestampMs > cycle.renderAtMs)
        ) {
          cycle.incompleteSequence = true;
          this.addIssue(
            'slotResponseReceived',
            record,
            timestampMs,
            'matched',
            'invalid_event_order'
          );
        }
      }
    );
  }

  recordSlotRenderEnded(slot: GptDiagnosticsSlotLike, facts: GptRenderFacts): void {
    const timestampMs = this.timestamp();
    this.matchCycle(
      'slotRenderEnded',
      slot,
      timestampMs,
      (cycle) => cycle.renderAtMs === undefined,
      (record, cycle) => {
        cycle.renderAtMs = timestampMs;
        cycle.isEmpty = facts.isEmpty;
        cycle.size = facts.size ? ([...facts.size] as Size) : undefined;
        cycle.isBackfill = facts.isBackfill;
        cycle.slotContentChanged = facts.slotContentChanged;

        if (cycle.responseAtMs === undefined) {
          cycle.incompleteSequence = true;
          this.addIssue(
            'slotRenderEnded',
            record,
            timestampMs,
            'matched',
            'missing_response_before_render'
          );
        } else if (validDuration(cycle.responseAtMs, timestampMs) === undefined) {
          cycle.incompleteSequence = true;
          this.addIssue('slotRenderEnded', record, timestampMs, 'matched', 'invalid_event_order');
        }
      }
    );
  }

  recordSlotOnload(slot: GptDiagnosticsSlotLike): void {
    const timestampMs = this.timestamp();
    this.matchCycle(
      'slotOnload',
      slot,
      timestampMs,
      (cycle) =>
        cycle.renderAtMs !== undefined && cycle.isEmpty !== true && cycle.loadAtMs === undefined,
      (record, cycle) => {
        cycle.loadAtMs = timestampMs;
        if (validDuration(cycle.renderAtMs, timestampMs) === undefined) {
          cycle.incompleteSequence = true;
          this.addIssue('slotOnload', record, timestampMs, 'matched', 'invalid_event_order');
        }
      }
    );
  }

  recordImpressionViewable(slot: GptDiagnosticsSlotLike): void {
    const timestampMs = this.timestamp();
    this.matchCycle(
      'impressionViewable',
      slot,
      timestampMs,
      (cycle) =>
        cycle.renderAtMs !== undefined &&
        cycle.isEmpty !== true &&
        cycle.viewableAtMs === undefined,
      (record, cycle) => {
        cycle.viewableAtMs = timestampMs;
        if (validDuration(cycle.renderAtMs, timestampMs) === undefined) {
          cycle.incompleteSequence = true;
          this.addIssue(
            'impressionViewable',
            record,
            timestampMs,
            'matched',
            'invalid_event_order'
          );
        }
      }
    );
  }

  recordSlotVisibilityChanged(slot: GptDiagnosticsSlotLike, percentage: number): void {
    const timestampMs = this.timestamp();
    const record = this.prepareCallback('slotVisibilityChanged', slot, timestampMs);
    if (!record) return;

    if (!Number.isFinite(percentage)) {
      this.incrementDisposition('slotVisibilityChanged', 'unmatched');
      this.addIssue(
        'slotVisibilityChanged',
        record,
        timestampMs,
        'unmatched',
        'invalid_visibility_percentage'
      );
      this.notify();
      return;
    }

    record.currentVisibilityPercentage = percentage;
    record.maximumVisibilityPercentage = Math.max(
      record.maximumVisibilityPercentage ?? percentage,
      percentage
    );
    this.incrementDisposition('slotVisibilityChanged', 'matched');
    this.notify();
  }

  bindingInputs(): GptDiagnosticsBindingInput[] {
    return this.slotOrder.flatMap((runtimeSlotNumber) => {
      const record = this.slots.get(runtimeSlotNumber);
      if (!record) return [];
      return [
        {
          runtimeSlotNumber: record.runtimeSlotNumber,
          slotElementId: record.slotElementId,
        },
      ];
    });
  }

  snapshot(): GptDiagnosticsStoreSnapshot {
    return {
      gptObserved: this.gptObserved,
      slots: this.slotOrder.flatMap((runtimeSlotNumber) => {
        const record = this.slots.get(runtimeSlotNumber);
        if (!record) return [];

        return [
          {
            runtimeSlotNumber: record.runtimeSlotNumber,
            slotElementId: record.slotElementId,
            adUnitPath: record.adUnitPath,
            currentVisibilityPercentage: record.currentVisibilityPercentage,
            maximumVisibilityPercentage: record.maximumVisibilityPercentage,
            requests: record.requests.map(copyCycle),
          },
        ];
      }),
      callbackIssues: this.callbackIssues.map((issue) => ({ ...issue })),
      coverage: Object.fromEntries(
        CALLBACK_KINDS.map((kind) => [kind, { ...this.coverage[kind] }])
      ) as Record<GptDiagnosticsCallbackKind, GptDiagnosticsCoverageCounters>,
      metadata: { ...this.metadata },
    };
  }

  private timestamp(): number {
    this.gptObserved = true;
    return this.now();
  }

  private prepareCallback(
    kind: GptDiagnosticsCallbackKind,
    slot: GptDiagnosticsSlotLike,
    timestampMs: number
  ): MutableSlotRecord | undefined {
    this.coverage[kind].observed += 1;
    const existingNumber = this.slotNumbers.get(slot);
    if (existingNumber !== undefined) {
      const existingRecord = this.slots.get(existingNumber);
      if (existingRecord) {
        this.refreshSlotMetadata(existingRecord, slot);
        this.markRecentlyActive(existingNumber);
        return existingRecord;
      }

      if (kind !== 'slotRequested') {
        this.incrementDisposition(kind, 'unmatched');
        this.addIssue(
          kind,
          { runtimeSlotNumber: existingNumber },
          timestampMs,
          'unmatched',
          'evicted_slot'
        );
        this.notify();
        return undefined;
      }
    }

    if (this.slots.size >= MAX_DIAGNOSTIC_SLOTS) {
      const evictedNumber = this.slotActivityOrder.shift();
      if (evictedNumber !== undefined) {
        this.slots.delete(evictedNumber);
        const slotIndex = this.slotOrder.indexOf(evictedNumber);
        if (slotIndex >= 0) this.slotOrder.splice(slotIndex, 1);
        this.metadata.evictedSlots += 1;
      }
    }

    const runtimeSlotNumber = this.nextRuntimeSlotNumber;
    this.nextRuntimeSlotNumber += 1;
    const record: MutableSlotRecord = {
      runtimeSlotNumber,
      requests: [],
    };
    this.slotNumbers.set(slot, runtimeSlotNumber);
    this.refreshSlotMetadata(record, slot);
    this.slots.set(runtimeSlotNumber, record);
    this.slotOrder.push(runtimeSlotNumber);
    this.slotActivityOrder.push(runtimeSlotNumber);
    return record;
  }

  private refreshSlotMetadata(record: MutableSlotRecord, slot: GptDiagnosticsSlotLike): void {
    record.slotElementId ??= optionalNonEmptyString(
      typeof slot.getSlotElementId === 'function' ? slot.getSlotElementId.bind(slot) : undefined
    );
    record.adUnitPath ??= optionalNonEmptyString(
      typeof slot.getAdUnitPath === 'function' ? slot.getAdUnitPath.bind(slot) : undefined
    );
  }

  private markRecentlyActive(runtimeSlotNumber: number): void {
    const previousIndex = this.slotActivityOrder.indexOf(runtimeSlotNumber);
    if (previousIndex >= 0) this.slotActivityOrder.splice(previousIndex, 1);
    this.slotActivityOrder.push(runtimeSlotNumber);
  }

  private matchCycle(
    kind: GptDiagnosticsCallbackKind,
    slot: GptDiagnosticsSlotLike,
    timestampMs: number,
    compatible: CompatibleCycle,
    attach: (record: MutableSlotRecord, cycle: MutableRequestCycle) => void
  ): void {
    const record = this.prepareCallback(kind, slot, timestampMs);
    if (!record) return;

    const candidates = record.requests.filter(compatible);
    if (candidates.length === 0) {
      this.incrementDisposition(kind, 'unmatched');
      this.addIssue(kind, record, timestampMs, 'unmatched', 'no_compatible_request_cycle');
      this.notify();
      return;
    }
    if (candidates.length > 1) {
      this.incrementDisposition(kind, 'ambiguous');
      this.addIssue(kind, record, timestampMs, 'ambiguous', 'overlapping_request_cycles');
      this.notify();
      return;
    }

    attach(record, candidates[0]!);
    this.incrementDisposition(kind, 'matched');
    this.notify();
  }

  private incrementDisposition(
    kind: GptDiagnosticsCallbackKind,
    disposition: GptDiagnosticsCallbackDisposition
  ): void {
    this.coverage[kind][disposition] += 1;
  }

  private addIssue(
    kind: GptDiagnosticsCallbackKind,
    record: Pick<MutableSlotRecord, 'runtimeSlotNumber' | 'slotElementId'>,
    timestampMs: number,
    disposition: GptDiagnosticsCallbackDisposition,
    reason: string
  ): void {
    if (this.callbackIssues.length >= MAX_CALLBACK_ISSUES) {
      this.callbackIssues.shift();
      this.metadata.droppedCallbacks += 1;
    }

    this.callbackIssues.push({
      kind,
      runtimeSlotNumber: record.runtimeSlotNumber,
      slotElementId: record.slotElementId,
      timestampMs,
      disposition,
      reason,
    });
  }

  private notify(): void {
    if (this.notificationScheduled) return;
    this.notificationScheduled = true;
    this.schedule(() => {
      this.notificationScheduled = false;
      for (const listener of this.listeners) {
        try {
          listener();
        } catch {
          // One diagnostics subscriber must not block the rest.
        }
      }
    });
  }
}
