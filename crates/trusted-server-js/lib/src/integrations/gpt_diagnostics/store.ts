import type {
  GptDiagnosticsAdManagerIdentity,
  GptDiagnosticsCallbackDisposition,
  GptDiagnosticsCallbackIssue,
  GptDiagnosticsCallbackKind,
  GptDiagnosticsCoverageCounters,
  GptDiagnosticsDelivery,
  GptDiagnosticsDurations,
  GptDiagnosticsRequestCycle,
  GptDiagnosticsResponseClass,
  Size,
} from '../../core/types';

export const MAX_DIAGNOSTIC_SLOTS = 64;
export const MAX_REQUEST_CYCLES_PER_SLOT = 10;
export const MAX_CALLBACK_ISSUES = 128;
export const MAX_TRUSTED_SERVER_CANDIDATE_SLOTS = 64;

/**
 * How long a filled Trusted Server candidate waits for its creative to request
 * markup before the cycle is reported as other Ad Manager demand.
 *
 * The Prebid Universal Creative requests markup as soon as Ad Manager writes it
 * into the slot. A later claim still upgrades the cycle.
 */
export const TRUSTED_SERVER_ATTRIBUTION_WINDOW_MS = 5_000;

const CALLBACK_KINDS: GptDiagnosticsCallbackKind[] = [
  'slotRequested',
  'slotResponseReceived',
  'slotRenderEnded',
  'slotOnload',
  'impressionViewable',
  'slotVisibilityChanged',
];

export interface GptDiagnosticsSlotLike {
  getSlotElementId?(): string;
  getAdUnitPath?(): string;
}

export interface GptRenderFacts {
  isEmpty?: boolean;
  size?: Size;
  isBackfill?: boolean;
  slotContentChanged?: boolean;
  adManager?: GptDiagnosticsAdManagerIdentity;
}

export interface GptDiagnosticsStoreSlotSnapshot {
  runtimeSlotNumber: number;
  slotElementId?: string;
  adUnitPath?: string;
  currentVisibilityPercentage?: number;
  maximumVisibilityPercentage?: number;
  requests: GptDiagnosticsRequestCycle[];
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
  slotElementId?: string;
  adUnitPath?: string;
  currentVisibilityPercentage?: number;
  maximumVisibilityPercentage?: number;
  requests: MutableRequestCycle[];
}

interface StoreOptions {
  now?: () => number;
  schedule?: (callback: () => void) => void;
  /** Deferred re-notification, used only to close an attribution window. */
  defer?: (callback: () => void, delayMs: number) => void;
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

function responseClass(cycle: MutableRequestCycle): GptDiagnosticsResponseClass | undefined {
  if (cycle.renderAtMs === undefined) return undefined;
  if (cycle.isEmpty === true) return 'empty';
  if (cycle.isBackfill === true) return 'backfill';
  return cycle.adManager?.lineItemId !== undefined ||
    cycle.adManager?.creativeId !== undefined ||
    cycle.adManager?.sourceAgnosticLineItemId !== undefined
    ? 'reservation'
    : 'unclassified_non_empty';
}

/**
 * Resolve who delivered the ad from observed evidence only.
 *
 * A claim proves Ad Manager selected the Trusted Server creative, because no
 * other creative asks Trusted Server for markup. Absence of a claim proves the
 * opposite only once the attribution window has elapsed, and only for a slot
 * Trusted Server actually had a candidate on.
 */
function delivery(cycle: MutableRequestCycle, nowMs: number): GptDiagnosticsDelivery {
  if (cycle.trustedServerClaimAtMs !== undefined) return 'trusted_server';
  if (cycle.renderAtMs === undefined || cycle.isEmpty !== false) return 'not_applicable';
  if (!cycle.trustedServerCandidate) return 'no_candidate';
  return nowMs - cycle.renderAtMs >= TRUSTED_SERVER_ATTRIBUTION_WINDOW_MS
    ? 'other_demand'
    : 'pending';
}

function copyCycle(cycle: MutableRequestCycle, nowMs: number): GptDiagnosticsRequestCycle {
  return {
    ...cycle,
    durations: derivedDurations(cycle),
    size: cycle.size ? ([...cycle.size] as Size) : undefined,
    adManager: cycle.adManager ? { ...cycle.adManager } : undefined,
    responseClass: responseClass(cycle),
    delivery: delivery(cycle, nowMs),
  };
}

/** Bounded store for facts observed directly from GPT callbacks. */
export class GptDiagnosticsStore {
  private readonly now: () => number;
  private readonly schedule: (callback: () => void) => void;
  private readonly defer: (callback: () => void, delayMs: number) => void;
  /** Auction slot ID → GPT slot, established by the Trusted Server integration. */
  private readonly trustedServerSlots = new Map<string, GptDiagnosticsSlotLike>();
  /** Slots carrying Trusted Server targeting that has not yet been requested. */
  private readonly pendingCandidates = new WeakSet<object>();
  private readonly slotNumbers = new WeakMap<object, number>();
  private readonly slots = new Map<number, MutableSlotRecord>();
  private readonly slotOrder: number[] = [];
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
    this.defer = options.defer ?? ((callback, delayMs) => setTimeout(callback, delayMs));
  }

  /**
   * Associate a GPT slot with the auction slot whose targeting Trusted Server
   * applied to it.
   *
   * The association is keyed by GPT slot object identity, the same key every
   * other correlation in this store uses, so no element ID is guessed.
   */
  recordTrustedServerCandidate(slot: GptDiagnosticsSlotLike, auctionSlotId: string): void {
    if (!slot || typeof auctionSlotId !== 'string' || auctionSlotId.length === 0) return;

    this.trustedServerSlots.delete(auctionSlotId);
    this.trustedServerSlots.set(auctionSlotId, slot);
    while (this.trustedServerSlots.size > MAX_TRUSTED_SERVER_CANDIDATE_SLOTS) {
      const oldest = this.trustedServerSlots.keys().next().value;
      if (oldest === undefined) break;
      this.trustedServerSlots.delete(oldest);
    }

    const record = this.slots.get(this.slotNumbers.get(slot) ?? -1);
    const pending = record?.requests[record.requests.length - 1];
    // A candidate declared before the request cycle exists is carried by the
    // association above and applied when the cycle opens.
    if (pending && pending.renderAtMs === undefined) {
      pending.trustedServerCandidate = true;
      this.notify();
    }
    this.pendingCandidates.add(slot);
  }

  /**
   * Record that the rendered creative requested its markup from Trusted Server.
   *
   * Attributed to the auction slot's most recent rendered cycle. An unknown
   * slot or a cycle that never rendered is preserved as an issue rather than
   * attached to a guess.
   */
  recordTrustedServerClaim(auctionSlotId: string): void {
    const timestampMs = this.now();
    const slot = this.trustedServerSlots.get(auctionSlotId);
    const record = slot ? this.slots.get(this.slotNumbers.get(slot) ?? -1) : undefined;
    if (!record) {
      this.addIssue(
        'slotRenderEnded',
        { runtimeSlotNumber: 0 },
        timestampMs,
        'unmatched',
        'trusted_server_claim_without_slot'
      );
      this.notify();
      return;
    }

    const candidates = record.requests.filter(
      (cycle) => cycle.isEmpty === false && cycle.trustedServerClaimAtMs === undefined
    );
    if (candidates.length !== 1) {
      this.addIssue(
        'slotRenderEnded',
        record,
        timestampMs,
        candidates.length === 0 ? 'unmatched' : 'ambiguous',
        candidates.length === 0
          ? 'trusted_server_claim_without_render'
          : 'trusted_server_claim_ambiguous_cycle'
      );
      this.notify();
      return;
    }

    candidates[0].trustedServerClaimAtMs = timestampMs;
    candidates[0].trustedServerCandidate = true;
    this.notify();
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

    const trustedServerCandidate = this.pendingCandidates.has(slot);
    this.pendingCandidates.delete(slot);
    record.requests.push({
      requestNumber: this.nextRequestNumber(record),
      requestedAtMs: timestampMs,
      durations: {},
      incompleteSequence: false,
      ...(trustedServerCandidate ? { trustedServerCandidate: true } : {}),
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
        cycle.adManager = facts.adManager ? { ...facts.adManager } : undefined;

        // A filled candidate resolves to other demand once its window closes,
        // so the panel needs one notification at that boundary.
        if (facts.isEmpty === false && cycle.trustedServerCandidate) {
          this.defer(() => this.notify(), TRUSTED_SERVER_ATTRIBUTION_WINDOW_MS);
        }

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
        cycle.renderAtMs !== undefined && cycle.isEmpty === false && cycle.loadAtMs === undefined,
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
        cycle.isEmpty === false &&
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

  snapshot(): GptDiagnosticsStoreSnapshot {
    const nowMs = this.now();
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
            requests: record.requests.map((cycle) => copyCycle(cycle, nowMs)),
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
        return existingRecord;
      }

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

    if (this.slots.size >= MAX_DIAGNOSTIC_SLOTS) {
      const evictedNumber = this.slotOrder.shift();
      if (evictedNumber !== undefined) {
        this.slots.delete(evictedNumber);
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

  private nextRequestNumber(record: MutableSlotRecord): number {
    const latest = record.requests[record.requests.length - 1];
    return (latest?.requestNumber ?? 0) + 1;
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

    attach(record, candidates[0]);
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
