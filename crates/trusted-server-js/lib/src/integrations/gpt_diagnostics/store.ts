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
  GptDiagnosticsTrustedServerOpportunity,
  Size,
} from '../../core/types';

export const MAX_DIAGNOSTIC_SLOTS = 64;
export const MAX_REQUEST_CYCLES_PER_SLOT = 10;
export const MAX_CALLBACK_ISSUES = 128;
export const MAX_TRUSTED_SERVER_ASSOCIATIONS = 64;
export const CREATIVE_ATTEMPT_WINDOW_MS = 30_000;
export const MAX_CREATIVE_ATTEMPTS = 128;
export const MAX_ATTRIBUTION_ISSUES = 128;

/** Maximum UTF-8 byte length retained for an optional Trusted Server auction ID. */
export const MAX_TRUSTED_SERVER_AUCTION_ID_UTF8_BYTES = 256;

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

/** A request-triggering source observable at an installed integration boundary. */
type RequestIntentSource = 'trusted_server_direct' | 'prebid_refresh' | 'publisher_refresh';

/** One source's independently timestamped evidence for a pending per-slot request intent. */
interface PendingSourceEvidence {
  generation: number;
  observedAtMs: number;
  trustedServerOpportunity?: GptDiagnosticsTrustedServerOpportunity;
  trustedServerAuctionId?: string;
}

/** Accumulated, not-yet-consumed request-triggering evidence for a GPT slot's next request. */
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

/** Accept only a string whose trimmed value is non-empty and within the UTF-8 byte boundary. */
function normalizeTrustedServerAuctionId(candidate: unknown): string | undefined {
  if (typeof candidate !== 'string') return undefined;

  const trimmed = candidate.trim();
  if (trimmed.length === 0) return undefined;
  if (new TextEncoder().encode(trimmed).length > MAX_TRUSTED_SERVER_AUCTION_ID_UTF8_BYTES) {
    return undefined;
  }

  return trimmed;
}

/** Classify a consumed request intent by how many independent sources observed it. */
function classifyRequestIntent(
  intent: PendingRequestIntent | undefined
): GptDiagnosticsRequestPath {
  if (intent === undefined || intent.sources.size === 0) return 'unattributed';
  if (intent.sources.size > 1) return 'competing';

  const [source] = intent.sources.keys();
  return source;
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
    renderToLoadMs: validDuration(cycle.renderAtMs, cycle.loadAtMs),
    renderToViewableMs: validDuration(cycle.renderAtMs, cycle.viewableAtMs),
  };
}

function responseClass(cycle: MutableRequestCycle): GptDiagnosticsResponseClass | undefined {
  if (cycle.renderAtMs === undefined) return undefined;
  if (cycle.isEmpty === true) return 'empty';
  if (cycle.isEmpty !== false) return undefined;
  if (cycle.isBackfill === true) return 'backfill';
  return cycle.adManager?.lineItemId !== undefined ||
    cycle.adManager?.creativeId !== undefined ||
    cycle.adManager?.sourceAgnosticLineItemId !== undefined
    ? 'reservation'
    : 'unclassified_non_empty';
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
    size: cycle.size ? ([...cycle.size] as Size) : undefined,
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
  /** Per-slot accumulated, not-yet-consumed request-intent evidence. */
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
  private nextMarkerGeneration = 1;
  private nextRequestIntentId = 1;
  private nextCreativeAttemptId = 1;
  private notificationScheduled = false;
  private gptObserved = false;

  constructor(options: StoreOptions = {}) {
    this.now = options.now ?? (() => performance.now());
    this.schedule = options.schedule ?? ((callback) => queueMicrotask(callback));
    this.defer = options.defer ?? ((callback, delayMs) => setTimeout(callback, delayMs));
  }

  /**
   * Record Trusted Server's opportunity evidence for a GPT slot's next request.
   * `trustedServerAuctionId` is accepted only when its trimmed value is
   * non-empty and no more than {@link MAX_TRUSTED_SERVER_AUCTION_ID_UTF8_BYTES}
   * UTF-8 bytes; an invalid or absent value clears any prior auction ID
   * without dropping the opportunity evidence.
   */
  recordTrustedServerOpportunity(
    slot: GptDiagnosticsSlotLike,
    auctionSlotId: string,
    opportunity: GptDiagnosticsTrustedServerOpportunity,
    trustedServerAuctionId?: string
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

    this.recordIntentSource(slot, 'trusted_server_direct', {
      trustedServerOpportunity: opportunity,
      trustedServerAuctionId: normalizeTrustedServerAuctionId(trustedServerAuctionId),
    });
  }

  /** Mark GPT slots whose next request follows a Prebid-controlled refresh. */
  recordPrebidRefresh(slots: GptDiagnosticsSlotLike[]): void {
    if (!Array.isArray(slots)) return;

    for (const slot of slots) {
      if (!isSlotObject(slot)) continue;
      this.recordIntentSource(slot, 'prebid_refresh');
    }
  }

  /** Mark GPT slots whose next request follows the installed publisher refresh boundary. */
  recordPublisherRefresh(slots: GptDiagnosticsSlotLike[]): void {
    if (!Array.isArray(slots)) return;

    for (const slot of slots) {
      if (!isSlotObject(slot)) continue;
      this.recordIntentSource(slot, 'publisher_refresh');
    }
  }

  /** Record a creative request against the associated current GPT cycle. */
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

  /** Record a successfully posted creative response for an attempt. */
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
    const opportunityToRequestMs = validDuration(trustedServerEvidence?.observedAtMs, timestampMs);
    record.requests.push({
      requestNumber,
      requestedAtMs: timestampMs,
      durations: {},
      incompleteSequence: false,
      requestPath: classifyRequestIntent(intent),
      ...(intent !== undefined ? { requestIntentId: intent.intentId } : {}),
      ...(trustedServerEvidence?.trustedServerOpportunity !== undefined
        ? { trustedServerOpportunity: trustedServerEvidence.trustedServerOpportunity }
        : {}),
      ...(trustedServerEvidence?.trustedServerAuctionId !== undefined
        ? { trustedServerAuctionId: trustedServerEvidence.trustedServerAuctionId }
        : {}),
      ...(opportunityToRequestMs !== undefined ? { opportunityToRequestMs } : {}),
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

        // --- Rendered replacement derivation --------------------------------
        // Must run before `cycle.renderAtMs` is set below: the search relies
        // on the natural `renderAtMs !== undefined` filter to exclude this
        // same cycle. Reads only cycles already retained in
        // `record.requests` — no new retained state, no raised retention
        // limit, and an evicted earlier render is simply absent from that
        // array, so no replacement relationship is invented for it.
        if (facts.isEmpty === false) {
          this.deriveRenderedReplacement(record, cycle, facts);
        }
        // ---------------------------------------------------------------------

        cycle.renderAtMs = timestampMs;
        cycle.isEmpty = facts.isEmpty;
        cycle.size = facts.size ? ([...facts.size] as Size) : undefined;
        cycle.isBackfill = facts.isBackfill;
        cycle.slotContentChanged = facts.slotContentChanged;
        cycle.adManager = facts.adManager ? { ...facts.adManager } : undefined;

        if (facts.isEmpty === true && provisionalCreativeRequest) {
          this.addAttributionIssue('creative_request_on_empty_cycle', timestampMs, record);
        }

        if (
          facts.isEmpty === false &&
          (cycle.trustedServerOpportunity === 'renderable_candidate' ||
            cycle.trustedServerOpportunity === 'unrenderable_candidate')
        ) {
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

  /** Prune every age-expired source synchronously, removing the intent if none remain active. */
  private pruneExpiredIntentSources(slot: object, nowMs: number): void {
    const intent = this.pendingRequestIntents.get(slot);
    if (!intent) return;

    for (const [source, evidence] of intent.sources) {
      if (nowMs - evidence.observedAtMs >= REQUEST_PATH_ATTRIBUTION_WINDOW_MS) {
        intent.sources.delete(source);
      }
    }

    if (intent.sources.size === 0) this.pendingRequestIntents.delete(slot);
  }

  /**
   * Record one source's evidence for a slot's pending request intent.
   * Prunes fully-stale evidence first, so a new intent ID is minted only
   * when no active intent remains. Replaces and restarts expiry only for the
   * recorded source, leaving sibling source evidence and its expiry intact.
   */
  private recordIntentSource(
    slot: object,
    source: RequestIntentSource,
    evidence: Pick<
      PendingSourceEvidence,
      'trustedServerOpportunity' | 'trustedServerAuctionId'
    > = {}
  ): void {
    const nowMs = this.now();
    this.pruneExpiredIntentSources(slot, nowMs);

    let intent = this.pendingRequestIntents.get(slot);
    if (!intent) {
      intent = { intentId: this.nextRequestIntentId, sources: new Map() };
      this.nextRequestIntentId += 1;
      this.pendingRequestIntents.set(slot, intent);
    }

    const generation = this.nextMarkerGeneration;
    this.nextMarkerGeneration += 1;
    intent.sources.set(source, { generation, observedAtMs: nowMs, ...evidence });

    this.defer(
      () => this.expireIntentSource(slot, source, generation),
      REQUEST_PATH_ATTRIBUTION_WINDOW_MS
    );
  }

  /** Remove one source's evidence only if this callback's generation is still current. */
  private expireIntentSource(slot: object, source: RequestIntentSource, generation: number): void {
    const intent = this.pendingRequestIntents.get(slot);
    if (!intent) return;

    const evidence = intent.sources.get(source);
    if (!evidence || evidence.generation !== generation) return;

    intent.sources.delete(source);
    if (intent.sources.size === 0) this.pendingRequestIntents.delete(slot);
  }

  /** Consume the slot's pending intent once, filtering any source that aged out before this request. */
  private consumeRequestIntent(slot: object, nowMs: number): PendingRequestIntent | undefined {
    const intent = this.pendingRequestIntents.get(slot);
    this.pendingRequestIntents.delete(slot);
    if (!intent) return undefined;

    const freshSources = new Map<RequestIntentSource, PendingSourceEvidence>();
    for (const [source, evidence] of intent.sources) {
      if (nowMs - evidence.observedAtMs < REQUEST_PATH_ATTRIBUTION_WINDOW_MS) {
        freshSources.set(source, evidence);
      }
    }

    return freshSources.size > 0 ? { intentId: intent.intentId, sources: freshSources } : undefined;
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

  /**
   * Find the most recent earlier retained non-empty render for this slot
   * and, when found, record the rendered-replacement relationship on
   * `cycle`. Only called for a current non-empty render. Leaves `cycle`
   * untouched when no qualifying earlier render is retained.
   */
  private deriveRenderedReplacement(
    record: MutableSlotRecord,
    cycle: MutableRequestCycle,
    facts: GptRenderFacts
  ): void {
    const previous = record.requests.reduce<MutableRequestCycle | undefined>(
      (latest, candidate) => {
        // The identity check is deliberately redundant with the `renderAtMs`
        // filter: today the caller derives before assigning `cycle.renderAtMs`,
        // so an unset render time already excludes this cycle. That ordering is
        // an invariant of one call site, not of this method. Keep the identity
        // check so a cycle can never report replacing itself even if the
        // assignment is ever moved earlier.
        if (
          candidate === cycle ||
          candidate.renderAtMs === undefined ||
          candidate.isEmpty !== false
        ) {
          return latest;
        }
        return latest === undefined || candidate.requestNumber > latest.requestNumber
          ? candidate
          : latest;
      },
      undefined
    );
    if (!previous) return;

    cycle.replacedRequestNumber = previous.requestNumber;

    const previousRenderToRequestMs = validDuration(previous.renderAtMs, cycle.requestedAtMs);
    if (previousRenderToRequestMs !== undefined) {
      cycle.previousRenderToRequestMs = previousRenderToRequestMs;
    }

    const previousCreativeId =
      previous.adManager?.creativeId ?? previous.adManager?.sourceAgnosticCreativeId;
    if (previousCreativeId !== undefined) {
      cycle.previousCreativeId = previousCreativeId;
    }

    const currentCreativeId =
      facts.adManager?.creativeId ?? facts.adManager?.sourceAgnosticCreativeId;
    if (previousCreativeId !== undefined && currentCreativeId !== undefined) {
      cycle.creativeChanged = previousCreativeId !== currentCreativeId;
    }
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
