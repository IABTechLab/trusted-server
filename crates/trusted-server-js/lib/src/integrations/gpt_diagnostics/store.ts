import type {
  GptDiagnosticsAdManagerIdentity,
  GptDiagnosticsAttributionIssue,
  GptDiagnosticsAttributionIssueReason,
  GptDiagnosticsCallbackDisposition,
  GptDiagnosticsCallbackIssue,
  GptDiagnosticsCallbackKind,
  GptDiagnosticsCoverageCounters,
  GptDiagnosticsCreativeFailure,
  GptDiagnosticsDelivery,
  GptDiagnosticsDurations,
  GptDiagnosticsRequestCycle,
  GptDiagnosticsRequestPath,
  GptDiagnosticsResponseClass,
  GptDiagnosticsSlotHandle,
  GptDiagnosticsTrustedServerOpportunity,
  Size,
} from '../../core/types';

export const MAX_DIAGNOSTIC_SLOTS = 64;
export const MAX_REQUEST_CYCLES_PER_SLOT = 10;
export const MAX_CALLBACK_ISSUES = 128;
export const MAX_TRUSTED_SERVER_ASSOCIATIONS = 64;
export const MAX_REQUESTED_SLOT_SIZES = 16;
export const CREATIVE_ATTEMPT_WINDOW_MS = 30_000;
export const MAX_CREATIVE_ATTEMPTS = 128;
export const MAX_ATTRIBUTION_ISSUES = 128;

/** How long request-path evidence remains eligible for the next GPT request. */
export const REQUEST_PATH_ATTRIBUTION_WINDOW_MS = 5_000;

/** How long an explicit non-empty render with an opportunity waits for creative evidence. */
export const TRUSTED_SERVER_ATTRIBUTION_WINDOW_MS = 5_000;

const CALLBACK_KINDS: GptDiagnosticsCallbackKind[] = [
  'slotRequested',
  'slotResponseReceived',
  'slotRenderEnded',
  'slotOnload',
  'impressionViewable',
  'slotVisibilityChanged',
];

/**
 * The GPT slot shape diagnostics reads. Aliased to the exported handle type so
 * the writer signatures and the store implementation cannot drift apart.
 */
export type GptDiagnosticsSlotLike = GptDiagnosticsSlotHandle;

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

export interface GptDiagnosticsBindingInput {
  runtimeSlotNumber: number;
  slotElementId?: string;
}

export interface GptDiagnosticsStoreSnapshot {
  gptObserved: boolean;
  slots: GptDiagnosticsStoreSlotSnapshot[];
  callbackIssues: GptDiagnosticsCallbackIssue[];
  attributionIssues: GptDiagnosticsAttributionIssue[];
  coverage: Record<GptDiagnosticsCallbackKind, GptDiagnosticsCoverageCounters>;
  metadata: {
    droppedCallbacks: number;
    droppedAttributionIssues: number;
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
  /** Deferred marker cleanup and diagnostic-window re-notification. */
  defer?: (callback: () => void, delayMs: number) => void;
}

type RequestIntentSource = 'trusted_server_direct' | 'prebid_refresh' | 'publisher_refresh';

interface PendingSourceEvidence {
  observedAtMs: number;
  trustedServerOpportunity?: GptDiagnosticsTrustedServerOpportunity;
  trustedServerAuctionId?: string;
  requestedSlotSizes?: ReadonlyArray<Size>;
}

interface PendingRequestIntent {
  intentId: number;
  sources: Map<RequestIntentSource, PendingSourceEvidence>;
}

type AttemptStatus = 'live' | 'completed' | 'expired' | 'evicted';

interface CreativeAttemptRecord {
  id: number;
  cycle?: MutableRequestCycle;
  runtimeSlotNumber?: number;
  slotElementId?: string;
  requestedAtMs: number;
  expiresAtMs: number;
  provisionalBeforeRender: boolean;
  status: AttemptStatus;
}

type AttributionIdentity = Partial<Pick<MutableSlotRecord, 'runtimeSlotNumber' | 'slotElementId'>>;

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

function isSlotObject(slot: unknown): slot is GptDiagnosticsSlotLike & object {
  return typeof slot === 'object' && slot !== null;
}

function isTrustedServerOpportunity(
  opportunity: unknown
): opportunity is GptDiagnosticsTrustedServerOpportunity {
  return (
    opportunity === 'renderable_candidate' ||
    opportunity === 'unrenderable_candidate' ||
    opportunity === 'no_candidate'
  );
}

function isCreativeFailure(reason: unknown): reason is GptDiagnosticsCreativeFailure {
  return (
    reason === 'missing_render_source' ||
    reason === 'cache_fetch_failed' ||
    reason === 'invalid_cache_payload' ||
    reason === 'response_post_failed'
  );
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
    ...(cycle.loadObservedBeforeRender
      ? {}
      : { renderToLoadMs: validDuration(cycle.renderAtMs, cycle.loadAtMs) }),
    renderToViewableMs: validDuration(cycle.renderAtMs, cycle.viewableAtMs),
  };
}

function normalizedAuctionId(value: unknown): string | undefined {
  if (typeof value !== 'string') return undefined;
  const trimmed = value.trim();
  if (trimmed.length === 0) return undefined;
  return new TextEncoder().encode(trimmed).length <= 256 ? trimmed : undefined;
}

function normalizedRequestedSlotSizes(value: unknown): ReadonlyArray<Size> | undefined {
  if (!Array.isArray(value)) return undefined;

  const requestedSlotSizes: Size[] = [];
  for (const candidate of value.slice(0, MAX_REQUESTED_SLOT_SIZES)) {
    if (
      !Array.isArray(candidate) ||
      candidate.length !== 2 ||
      typeof candidate[0] !== 'number' ||
      typeof candidate[1] !== 'number' ||
      !Number.isFinite(candidate[0]) ||
      !Number.isFinite(candidate[1]) ||
      candidate[0] <= 0 ||
      candidate[1] <= 0
    ) {
      continue;
    }
    requestedSlotSizes.push(Object.freeze([candidate[0], candidate[1]] as [number, number]));
  }

  return requestedSlotSizes.length > 0 ? Object.freeze(requestedSlotSizes) : undefined;
}

function responseClass(cycle: MutableRequestCycle): GptDiagnosticsResponseClass | undefined {
  if (cycle.renderAtMs === undefined) return undefined;
  if (cycle.isEmpty === true) return 'empty';
  if (cycle.isEmpty !== false) return undefined;
  if (cycle.isBackfill === true) return 'backfill';

  const identity = cycle.adManager;
  // Reservation-specific IDs are absent on backfill, so their presence is
  // direct evidence of a reservation line item.
  if (identity?.lineItemId !== undefined || identity?.creativeId !== undefined) {
    return 'reservation';
  }
  // Source-agnostic IDs are populated for reservation and backfill alike, so
  // they prove a reservation only alongside an explicit non-backfill fact.
  if (
    cycle.isBackfill === false &&
    (identity?.sourceAgnosticLineItemId !== undefined ||
      identity?.sourceAgnosticCreativeId !== undefined)
  ) {
    return 'reservation';
  }
  return 'unclassified_non_empty';
}

/** Resolve delivery state from positive observations without guessing ownership. */
function delivery(cycle: MutableRequestCycle, nowMs: number): GptDiagnosticsDelivery {
  if (cycle.trustedServerCreativeResponseAtMs !== undefined) {
    return 'trusted_server_response_sent';
  }
  if (cycle.trustedServerCreativeRequestAtMs !== undefined) return 'trusted_server_selected';
  if (cycle.renderAtMs === undefined || cycle.isEmpty === true) return 'not_applicable';
  if (cycle.isEmpty !== false) return 'unknown';
  if (cycle.trustedServerOpportunity === 'no_candidate') return 'no_candidate';
  if (cycle.trustedServerOpportunity === undefined) return 'unknown';
  return nowMs - cycle.renderAtMs >= TRUSTED_SERVER_ATTRIBUTION_WINDOW_MS
    ? 'candidate_unconfirmed'
    : 'pending';
}

function copyCycle(cycle: MutableRequestCycle, nowMs: number): GptDiagnosticsRequestCycle {
  return {
    ...cycle,
    durations: derivedDurations(cycle),
    requestedSlotSizes: cycle.requestedSlotSizes?.map((size) => [...size] as Size),
    size: cycle.size ? ([...cycle.size] as Size) : undefined,
    observedSlotSize: cycle.observedSlotSize ? ([...cycle.observedSlotSize] as Size) : undefined,
    adManager: cycle.adManager
      ? {
          ...cycle.adManager,
          yieldGroupIds: cycle.adManager.yieldGroupIds
            ? [...cycle.adManager.yieldGroupIds]
            : undefined,
          companyIds: cycle.adManager.companyIds ? [...cycle.adManager.companyIds] : undefined,
        }
      : undefined,
    trustedServerCreativeFailures: cycle.trustedServerCreativeFailures
      ? [...cycle.trustedServerCreativeFailures]
      : undefined,
    responseClass: responseClass(cycle),
    delivery: delivery(cycle, nowMs),
  };
}

/** Bounded store for facts observed from GPT callbacks and diagnostics integration bridges. */
export class GptDiagnosticsStore {
  private readonly now: () => number;
  private readonly schedule: (callback: () => void) => void;
  private readonly defer: (callback: () => void, delayMs: number) => void;
  /** Auction slot ID → GPT slot, established by the Trusted Server integration. */
  private readonly trustedServerSlots = new Map<string, GptDiagnosticsSlotLike>();
  private readonly pendingRequestIntents = new WeakMap<object, PendingRequestIntent>();
  private readonly slotNumbers = new WeakMap<object, number>();
  private readonly requestNumbers = new WeakMap<object, number>();
  private readonly attemptIdsByCycle = new WeakMap<MutableRequestCycle, number>();
  private readonly creativeAttempts = new Map<number, CreativeAttemptRecord>();
  private readonly slots = new Map<number, MutableSlotRecord>();
  private readonly slotOrder: number[] = [];
  private readonly slotActivityOrder: number[] = [];
  private readonly listeners = new Set<StoreListener>();
  private readonly coverage = emptyCoverage();
  private readonly callbackIssues: GptDiagnosticsCallbackIssue[] = [];
  private readonly attributionIssues: GptDiagnosticsAttributionIssue[] = [];
  private readonly metadata = {
    droppedCallbacks: 0,
    droppedAttributionIssues: 0,
    evictedSlots: 0,
    evictedRequestCycles: 0,
  };
  private nextRuntimeSlotNumber = 1;
  private nextRequestIntentId = 1;
  private nextCreativeAttemptId = 1;
  private notificationScheduled = false;
  private deliveryBoundaryScheduled = false;
  private gptObserved = false;

  constructor(options: StoreOptions = {}) {
    this.now = options.now ?? (() => performance.now());
    this.schedule = options.schedule ?? ((callback) => queueMicrotask(callback));
    this.defer = options.defer ?? ((callback, delayMs) => setTimeout(callback, delayMs));
  }

  /** Record Trusted Server's opportunity evidence for a GPT slot's next request. */
  recordTrustedServerOpportunity(
    slot: GptDiagnosticsSlotLike,
    auctionSlotId: string,
    opportunity: GptDiagnosticsTrustedServerOpportunity,
    trustedServerAuctionId?: string,
    requestedSlotSizes?: ReadonlyArray<Size>
  ): void {
    if (
      !isSlotObject(slot) ||
      typeof auctionSlotId !== 'string' ||
      auctionSlotId.length === 0 ||
      !isTrustedServerOpportunity(opportunity)
    ) {
      return;
    }

    this.trustedServerSlots.delete(auctionSlotId);
    this.trustedServerSlots.set(auctionSlotId, slot);
    while (this.trustedServerSlots.size > MAX_TRUSTED_SERVER_ASSOCIATIONS) {
      const oldest = this.trustedServerSlots.keys().next().value;
      if (oldest === undefined) break;
      this.trustedServerSlots.delete(oldest);
    }

    this.recordRequestIntentSource(slot, 'trusted_server_direct', {
      trustedServerOpportunity: opportunity,
      trustedServerAuctionId: normalizedAuctionId(trustedServerAuctionId),
      requestedSlotSizes: normalizedRequestedSlotSizes(requestedSlotSizes),
    });
  }

  /** Mark GPT slots whose next request follows a Prebid-controlled refresh. */
  recordPrebidRefresh(slots: GptDiagnosticsSlotLike[]): void {
    if (!Array.isArray(slots)) return;

    for (const slot of slots) {
      if (!isSlotObject(slot)) continue;
      this.recordRequestIntentSource(slot, 'prebid_refresh');
    }
  }

  /** Record publisher refresh observation from the private GPT diagnostics observer. */
  recordPublisherRefresh(slots: GptDiagnosticsSlotLike[]): void {
    if (!Array.isArray(slots)) return;
    for (const slot of slots) {
      if (!isSlotObject(slot)) continue;
      this.recordRequestIntentSource(slot, 'publisher_refresh');
    }
  }

  /** Record a creative markup request against the associated current GPT cycle. */
  recordTrustedServerCreativeRequest(auctionSlotId: string): number | undefined {
    const timestampMs = this.now();
    this.expireCreativeAttempts(timestampMs);

    if (typeof auctionSlotId !== 'string' || auctionSlotId.length === 0) {
      this.reportAttributionIssue('creative_request_without_slot', timestampMs);
      return undefined;
    }

    const slot = this.trustedServerSlots.get(auctionSlotId);
    if (!slot) {
      this.reportAttributionIssue('creative_request_without_slot', timestampMs);
      return undefined;
    }

    const identity = this.attributionIdentity(slot);
    const runtimeSlotNumber = this.slotNumbers.get(slot);
    const record = runtimeSlotNumber === undefined ? undefined : this.slots.get(runtimeSlotNumber);
    if (!record || record.requests.length === 0) {
      this.reportAttributionIssue('creative_request_without_cycle', timestampMs, identity);
      return undefined;
    }

    const cycle = record.requests.reduce((greatest, candidate) =>
      candidate.requestNumber > greatest.requestNumber ? candidate : greatest
    );
    const existingAttemptId = this.attemptIdsByCycle.get(cycle);
    if (existingAttemptId !== undefined) {
      if (cycle.trustedServerCreativeResponseAtMs !== undefined) return undefined;

      const existingAttempt = this.creativeAttempts.get(existingAttemptId);
      if (!existingAttempt) {
        this.reportAttributionIssue('creative_attempt_unknown', timestampMs, identity);
        return undefined;
      }
      if (existingAttempt.status === 'live') return existingAttempt.id;
      // Defensive: a completed attempt always stamped the response timestamp on
      // this same cycle, so the check above returns first. Unreachable today,
      // kept so a future lifecycle change cannot fall through to an issue.
      if (existingAttempt.status === 'completed') return undefined;

      this.reportAttributionIssue(
        existingAttempt.status === 'expired'
          ? 'creative_attempt_expired'
          : 'creative_attempt_evicted',
        timestampMs,
        existingAttempt
      );
      return undefined;
    }
    // Defensive: a response timestamp is only ever written through an attempt,
    // which leaves an entry in `attemptIdsByCycle` and so takes the branch
    // above. Unreachable today, kept so no cycle can be re-attempted after a
    // response was already recorded against it.
    if (cycle.trustedServerCreativeResponseAtMs !== undefined) return undefined;

    const retainedCreativeRequestAtMs = cycle.trustedServerCreativeRequestAtMs;
    if (
      retainedCreativeRequestAtMs !== undefined &&
      Number.isFinite(retainedCreativeRequestAtMs) &&
      timestampMs - retainedCreativeRequestAtMs >= CREATIVE_ATTEMPT_WINDOW_MS
    ) {
      this.reportAttributionIssue('creative_attempt_expired', timestampMs, identity);
      return undefined;
    }

    const requestedAtMs = cycle.requestedAtMs;
    if (
      !Number.isFinite(timestampMs) ||
      requestedAtMs === undefined ||
      !Number.isFinite(requestedAtMs) ||
      timestampMs - requestedAtMs > CREATIVE_ATTEMPT_WINDOW_MS ||
      cycle.isEmpty === true
    ) {
      this.reportAttributionIssue('creative_request_without_cycle', timestampMs, identity);
      return undefined;
    }

    const provisionalBeforeRender = cycle.renderAtMs === undefined;
    if (
      provisionalBeforeRender &&
      record.requests.some(
        (candidate) =>
          candidate !== cycle &&
          candidate.requestNumber < cycle.requestNumber &&
          candidate.isEmpty === false &&
          candidate.requestedAtMs !== undefined &&
          Number.isFinite(candidate.requestedAtMs) &&
          timestampMs - candidate.requestedAtMs <= CREATIVE_ATTEMPT_WINDOW_MS
      )
    ) {
      this.reportAttributionIssue('creative_request_ambiguous_cycle', timestampMs, identity);
      return undefined;
    }

    cycle.trustedServerCreativeRequestAtMs ??= timestampMs;
    if (!this.ensureCreativeAttemptCapacity()) {
      this.reportAttributionIssue('creative_attempt_capacity', timestampMs, identity);
      return undefined;
    }

    const id = this.nextCreativeAttemptId;
    this.nextCreativeAttemptId += 1;
    const creativeRequestedAtMs = cycle.trustedServerCreativeRequestAtMs;
    const attempt: CreativeAttemptRecord = {
      id,
      cycle,
      runtimeSlotNumber: record.runtimeSlotNumber,
      slotElementId: record.slotElementId,
      requestedAtMs: creativeRequestedAtMs,
      expiresAtMs: creativeRequestedAtMs + CREATIVE_ATTEMPT_WINDOW_MS,
      provisionalBeforeRender,
      status: 'live',
    };
    this.creativeAttempts.set(id, attempt);
    this.attemptIdsByCycle.set(cycle, id);
    this.notify();
    return id;
  }

  /** Record that a creative attempt successfully posted markup. */
  recordTrustedServerCreativeResponse(attemptId: number): void {
    const timestampMs = this.now();
    this.expireCreativeAttempts(timestampMs);
    const attempt = this.creativeAttempts.get(attemptId);
    if (!attempt) {
      this.reportAttributionIssue('creative_attempt_unknown', timestampMs);
      return;
    }
    if (attempt.status === 'completed') return;
    if (attempt.status !== 'live') {
      this.reportAttributionIssue(
        attempt.status === 'expired' ? 'creative_attempt_expired' : 'creative_attempt_evicted',
        timestampMs,
        attempt
      );
      return;
    }
    if (!attempt.cycle) {
      this.reportAttributionIssue('creative_attempt_unknown', timestampMs, attempt);
      return;
    }

    attempt.cycle.trustedServerCreativeResponseAtMs ??= timestampMs;
    attempt.status = 'completed';
    attempt.cycle = undefined;
    this.notify();
  }

  /** Record one safe, non-terminal creative failure category. */
  recordTrustedServerCreativeFailure(
    attemptId: number,
    reason: GptDiagnosticsCreativeFailure
  ): void {
    if (!isCreativeFailure(reason)) return;

    const timestampMs = this.now();
    this.expireCreativeAttempts(timestampMs);
    const attempt = this.creativeAttempts.get(attemptId);
    if (!attempt) {
      this.reportAttributionIssue('creative_attempt_unknown', timestampMs);
      return;
    }
    if (attempt.status === 'completed') return;
    if (attempt.status !== 'live') {
      this.reportAttributionIssue(
        attempt.status === 'expired' ? 'creative_attempt_expired' : 'creative_attempt_evicted',
        timestampMs,
        attempt
      );
      return;
    }
    if (!attempt.cycle) {
      this.reportAttributionIssue('creative_attempt_unknown', timestampMs, attempt);
      return;
    }

    const failures = (attempt.cycle.trustedServerCreativeFailures ??= []);
    if (failures.includes(reason)) return;
    failures.push(reason);
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
      const evictedCycle = record.requests[0];
      if (evictedCycle) this.evictCreativeAttempt(evictedCycle);
      record.requests.shift();
      this.metadata.evictedRequestCycles += 1;
    }

    const requestNumber = (this.requestNumbers.get(slot) ?? 0) + 1;
    this.requestNumbers.set(slot, requestNumber);
    const intent = this.consumeRequestIntent(slot, timestampMs);
    const trustedServerEvidence = intent?.sources.get('trusted_server_direct');
    const requestPath = this.requestPath(intent);
    record.requests.push({
      requestNumber,
      requestedAtMs: timestampMs,
      durations: {},
      incompleteSequence: false,
      requestPath,
      ...(intent ? { requestIntentId: intent.intentId } : {}),
      ...(trustedServerEvidence?.trustedServerOpportunity !== undefined
        ? { trustedServerOpportunity: trustedServerEvidence.trustedServerOpportunity }
        : {}),
      ...(trustedServerEvidence?.trustedServerAuctionId !== undefined
        ? { trustedServerAuctionId: trustedServerEvidence.trustedServerAuctionId }
        : {}),
      ...(trustedServerEvidence?.requestedSlotSizes !== undefined
        ? { requestedSlotSizes: trustedServerEvidence.requestedSlotSizes }
        : {}),
      ...(trustedServerEvidence
        ? {
            opportunityToRequestMs: validDuration(trustedServerEvidence.observedAtMs, timestampMs),
          }
        : {}),
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
        const provisionalCreativeRequest =
          cycle.renderAtMs === undefined && cycle.trustedServerCreativeRequestAtMs !== undefined;
        cycle.renderAtMs = timestampMs;
        cycle.isEmpty = facts.isEmpty;
        cycle.size = facts.size ? ([...facts.size] as Size) : undefined;
        cycle.isBackfill = facts.isBackfill;
        cycle.slotContentChanged = facts.slotContentChanged;
        cycle.adManager = facts.adManager ? { ...facts.adManager } : undefined;

        if (facts.isEmpty === false) this.recordReplacement(record, cycle);

        if (facts.isEmpty === true && provisionalCreativeRequest) {
          this.addAttributionIssue('creative_request_on_empty_cycle', timestampMs, record);
          // The attempt was matched to a cycle GPT then reported empty, so it
          // must not complete here and report a creative response against an
          // empty render. The attribution issue above records why it stopped.
          this.evictCreativeAttempt(cycle);
        }

        if (
          facts.isEmpty === false &&
          (cycle.trustedServerOpportunity === 'renderable_candidate' ||
            cycle.trustedServerOpportunity === 'unrenderable_candidate')
        ) {
          this.scheduleDeliveryBoundaryNotification();
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

  /**
   * Retain an outer CSS box only when this exact slot and request cycle still
   * identify a filled render. Async DOM measurements use this guard so a prior
   * render cannot alter a later refresh cycle.
   */
  recordObservedSlotSize(runtimeSlotNumber: number, requestNumber: number, size: Size): void {
    if (
      !Number.isSafeInteger(requestNumber) ||
      requestNumber <= 0 ||
      !Number.isFinite(size[0]) ||
      !Number.isFinite(size[1]) ||
      size[0] < 0 ||
      size[1] < 0
    ) {
      return;
    }

    const record = this.slots.get(runtimeSlotNumber);
    if (!record) return;

    const cycle = record.requests.find((candidate) => candidate.requestNumber === requestNumber);
    if (
      !cycle ||
      record.requests[record.requests.length - 1] !== cycle ||
      cycle.isEmpty !== false ||
      cycle.renderAtMs === undefined
    ) {
      return;
    }

    const observedSlotSize: Size = [size[0], size[1]];
    if (
      cycle.observedSlotSize?.[0] === observedSlotSize[0] &&
      cycle.observedSlotSize[1] === observedSlotSize[1]
    ) {
      return;
    }
    cycle.observedSlotSize = observedSlotSize;
    this.notify();
  }

  recordSlotOnload(slot: GptDiagnosticsSlotLike): void {
    const timestampMs = this.timestamp();
    this.matchCycle(
      'slotOnload',
      slot,
      timestampMs,
      (cycle) => cycle.responseAtMs !== undefined && cycle.loadAtMs === undefined,
      (record, cycle) => {
        cycle.loadAtMs = timestampMs;
        if (cycle.renderAtMs === undefined) {
          cycle.loadObservedBeforeRender = true;
        } else if (validDuration(cycle.renderAtMs, timestampMs) === undefined) {
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
      attributionIssues: this.attributionIssues.map((issue) => ({ ...issue })),
      coverage: Object.fromEntries(
        CALLBACK_KINDS.map((kind) => [kind, { ...this.coverage[kind] }])
      ) as Record<GptDiagnosticsCallbackKind, GptDiagnosticsCoverageCounters>,
      metadata: { ...this.metadata },
    };
  }

  private attributionIdentity(slot: GptDiagnosticsSlotLike & object): AttributionIdentity {
    const runtimeSlotNumber = this.slotNumbers.get(slot);
    const record = runtimeSlotNumber === undefined ? undefined : this.slots.get(runtimeSlotNumber);
    const slotElementId =
      record?.slotElementId ??
      optionalNonEmptyString(
        typeof slot.getSlotElementId === 'function' ? slot.getSlotElementId.bind(slot) : undefined
      );
    return {
      ...(runtimeSlotNumber !== undefined ? { runtimeSlotNumber } : {}),
      ...(slotElementId !== undefined ? { slotElementId } : {}),
    };
  }

  private addAttributionIssue(
    reason: GptDiagnosticsAttributionIssueReason,
    timestampMs: number,
    identity: AttributionIdentity = {}
  ): void {
    if (this.attributionIssues.length >= MAX_ATTRIBUTION_ISSUES) {
      this.attributionIssues.shift();
      this.metadata.droppedAttributionIssues += 1;
    }

    this.attributionIssues.push({
      reason,
      timestampMs,
      ...(identity.runtimeSlotNumber !== undefined && identity.runtimeSlotNumber > 0
        ? { runtimeSlotNumber: identity.runtimeSlotNumber }
        : {}),
      ...(identity.slotElementId !== undefined ? { slotElementId: identity.slotElementId } : {}),
    });
  }

  private reportAttributionIssue(
    reason: GptDiagnosticsAttributionIssueReason,
    timestampMs: number,
    identity: AttributionIdentity = {}
  ): void {
    this.addAttributionIssue(reason, timestampMs, identity);
    this.notify();
  }

  private expireCreativeAttempts(timestampMs: number): void {
    for (const attempt of this.creativeAttempts.values()) {
      if (attempt.status !== 'live' || timestampMs < attempt.expiresAtMs) continue;
      attempt.status = 'expired';
      attempt.cycle = undefined;
    }
  }

  private evictCreativeAttempt(cycle: MutableRequestCycle): void {
    const attemptId = this.attemptIdsByCycle.get(cycle);
    if (attemptId === undefined) return;
    const attempt = this.creativeAttempts.get(attemptId);
    if (!attempt || attempt.status !== 'live') return;
    attempt.status = 'evicted';
    attempt.cycle = undefined;
  }

  private ensureCreativeAttemptCapacity(): boolean {
    if (this.creativeAttempts.size < MAX_CREATIVE_ATTEMPTS) return true;

    for (const [attemptId, attempt] of this.creativeAttempts) {
      if (attempt.status === 'live') continue;
      this.creativeAttempts.delete(attemptId);
      return true;
    }
    return false;
  }

  /**
   * Record one source's evidence for the slot's next GPT request.
   *
   * Evidence expires lazily — on the next recording for the same slot and on
   * consumption — so retained intents never own a timer. Pending intents are
   * keyed weakly by GPT slot object, which bounds them by the slots the page
   * itself still holds.
   */
  private recordRequestIntentSource(
    slot: object,
    source: RequestIntentSource,
    facts: Pick<
      PendingSourceEvidence,
      'trustedServerOpportunity' | 'trustedServerAuctionId' | 'requestedSlotSizes'
    > = {}
  ): void {
    const observedAtMs = this.now();
    let intent = this.pendingRequestIntents.get(slot);
    if (intent) {
      this.expireRequestIntentSources(intent, observedAtMs);
      // A fully expired intent is replaced rather than revived, so its intent
      // ID cannot outlive the observation window it was issued for.
      if (intent.sources.size === 0) intent = undefined;
    }
    if (!intent) {
      intent = { intentId: this.nextRequestIntentId, sources: new Map() };
      this.nextRequestIntentId += 1;
      this.pendingRequestIntents.set(slot, intent);
    }
    intent.sources.set(source, { observedAtMs, ...facts });
  }

  private expireRequestIntentSources(intent: PendingRequestIntent, timestampMs: number): void {
    for (const [source, evidence] of intent.sources) {
      if (timestampMs - evidence.observedAtMs >= REQUEST_PATH_ATTRIBUTION_WINDOW_MS) {
        intent.sources.delete(source);
      }
    }
  }

  private consumeRequestIntent(
    slot: object,
    timestampMs: number
  ): PendingRequestIntent | undefined {
    const intent = this.pendingRequestIntents.get(slot);
    this.pendingRequestIntents.delete(slot);
    if (!intent) return undefined;
    this.expireRequestIntentSources(intent, timestampMs);
    return intent.sources.size > 0 ? intent : undefined;
  }

  /**
   * Keep at most one outstanding timer for the delivery-evidence boundary.
   *
   * Each candidate render flips from `pending` to `candidate_unconfirmed` once
   * its window elapses, and subscribers need one notification per boundary. A
   * single timer that re-arms from retained cycles bounds the timer queue by
   * retained state instead of by refresh rate.
   */
  private scheduleDeliveryBoundaryNotification(
    delayMs: number = TRUSTED_SERVER_ATTRIBUTION_WINDOW_MS
  ): void {
    if (this.deliveryBoundaryScheduled) return;
    this.deliveryBoundaryScheduled = true;
    this.defer(() => {
      this.deliveryBoundaryScheduled = false;
      this.notify();
      const nextDelayMs = this.nextDeliveryBoundaryDelayMs();
      if (nextDelayMs !== undefined) this.scheduleDeliveryBoundaryNotification(nextDelayMs);
    }, delayMs);
  }

  private nextDeliveryBoundaryDelayMs(): number | undefined {
    const nowMs = this.now();
    let earliestDeadlineMs: number | undefined;
    for (const record of this.slots.values()) {
      for (const cycle of record.requests) {
        if (
          cycle.renderAtMs === undefined ||
          cycle.isEmpty !== false ||
          (cycle.trustedServerOpportunity !== 'renderable_candidate' &&
            cycle.trustedServerOpportunity !== 'unrenderable_candidate')
        ) {
          continue;
        }
        const deadlineMs = cycle.renderAtMs + TRUSTED_SERVER_ATTRIBUTION_WINDOW_MS;
        if (deadlineMs <= nowMs) continue;
        if (earliestDeadlineMs === undefined || deadlineMs < earliestDeadlineMs) {
          earliestDeadlineMs = deadlineMs;
        }
      }
    }
    return earliestDeadlineMs === undefined ? undefined : earliestDeadlineMs - nowMs;
  }

  private requestPath(intent: PendingRequestIntent | undefined): GptDiagnosticsRequestPath {
    if (!intent) return 'unattributed';
    if (intent.sources.size > 1) return 'competing';
    const source = intent.sources.keys().next().value as RequestIntentSource | undefined;
    return source ?? 'unattributed';
  }

  private recordReplacement(record: MutableSlotRecord, cycle: MutableRequestCycle): void {
    const currentIndex = record.requests.indexOf(cycle);
    if (currentIndex <= 0) return;
    const previous = record.requests
      .slice(0, currentIndex)
      .reverse()
      .find((candidate) => candidate.isEmpty === false && candidate.renderAtMs !== undefined);
    if (!previous) return;
    cycle.replacedRequestNumber = previous.requestNumber;
    cycle.previousRenderToRequestMs = validDuration(previous.renderAtMs, cycle.requestedAtMs);
    const previousCreativeId =
      previous.adManager?.creativeId ?? previous.adManager?.sourceAgnosticCreativeId;
    const currentCreativeId =
      cycle.adManager?.creativeId ?? cycle.adManager?.sourceAgnosticCreativeId;
    if (previousCreativeId !== undefined) cycle.previousCreativeId = previousCreativeId;
    if (previousCreativeId !== undefined && currentCreativeId !== undefined) {
      cycle.creativeChanged = previousCreativeId !== currentCreativeId;
    }
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
        const evictedRecord = this.slots.get(evictedNumber);
        if (evictedRecord) {
          for (const cycle of evictedRecord.requests) this.evictCreativeAttempt(cycle);
        }
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
