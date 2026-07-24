import type {
  AdTraceApi,
  AdTraceConfidence,
  AdTraceCoverageCategory,
  AdTraceCoverageCounter,
  AdTraceCoverageObservation,
  AdTraceEvent,
  AdTraceEventKind,
  AdTraceExport,
  AdTraceObservation,
  AdTraceStage,
  AdTraceStageName,
  GenerationTraceDiagnostics,
  GenerationTraceSnapshot,
  RenderTraceOutcome,
  RenderTraceSnapshot,
  RenderTraceVisibility,
  SlotTraceSnapshot,
} from './types';

export const AD_TRACE_MAX_EVENTS = 256;
export const AD_TRACE_MAX_SLOTS = 64;
export const AD_TRACE_MAX_GENERATIONS = 8;
export const AD_TRACE_MAX_RENDERS = 200;
export const AD_TRACE_ACK_TTL_MS = 30_000;
const AD_TRACE_MAX_LISTENERS = 32;
const AD_TRACE_MAX_ANOMALIES = 64;

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const LABEL_RE = /^[\w.-]{1,64}$/;
const EVENT_KINDS = new Set<AdTraceEventKind>([
  'ts_auction_observed',
  'ts_winner_observed',
  'prebid_auction_init',
  'prebid_bid_response',
  'prebid_targeting_selected',
  'prebid_bid_won',
  'prebid_auction_end',
  'prebid_render_succeeded',
  'prebid_render_failed',
  'gpt_targeting_applied',
  'gpt_request_started',
  'gpt_slot_requested',
  'gpt_slot_response_received',
  'gpt_slot_render_ended',
  'gpt_slot_onload',
  'gpt_impression_viewable',
  'gpt_slot_visibility_changed',
  'aps_display_bids_set',
  'aps_renderer_ready',
  'pb_render_requested',
  'pb_render_rejected',
  'pb_render_served',
  'direct_render_rejected',
  'creative_load_acknowledged',
  'creative_ack_timed_out',
  'creative_ack_superseded',
  'creative_ack_source_mismatched',
  'creative_ack_missing_token',
  'generation_superseded',
]);
const CONFIDENCES = new Set(['definitive', 'strong', 'probable', 'none']);
const EMPTY_STAGE: AdTraceStage = { outcome: 'not_observed', confidence: 'none', reason: 'none' };

interface MutableGeneration extends GenerationTraceSnapshot {
  timestamps: {
    requestedAt?: number;
    responseAt?: number;
    renderedAt?: number;
    iframeLoadedAt?: number;
    acknowledgedAt?: number;
    viewableAt?: number;
  };
}
interface MutableSlot {
  slotId: string;
  latestGeneration: number;
  nextRequestNumber: number;
  baseStages: Record<AdTraceStageName, AdTraceStage>;
  generations: MutableGeneration[];
}

export interface AdTraceStore extends AdTraceApi {
  record(observation: AdTraceObservation): void;
  recordCoverage(observation: AdTraceCoverageObservation): void;
  nextGeneration(slotId: string): number;
  subscribe(listener: () => void): () => void;
  bindElement(slotId: string, generation: number, element: HTMLElement): void;
  getBoundElement(slotId: string, generation: number): HTMLElement | undefined;
  updateVisibility(slotId: string, generation: number, visibility: RenderTraceVisibility): void;
}

function stages(): Record<AdTraceStageName, AdTraceStage> {
  return {
    trustedServer: { ...EMPTY_STAGE },
    prebid: { ...EMPTY_STAGE },
    gam: { ...EMPTY_STAGE },
    creative: { ...EMPTY_STAGE },
  };
}

function safeLabel(value: unknown): string | undefined {
  return typeof value === 'string' && LABEL_RE.test(value) ? value : undefined;
}
function safeUuid(value: unknown): string | undefined {
  return typeof value === 'string' && UUID_RE.test(value) ? value : undefined;
}
function cloneStages(value: Record<AdTraceStageName, AdTraceStage>) {
  return Object.fromEntries(
    Object.entries(value).map(([key, stage]) => [key, { ...stage }])
  ) as Record<AdTraceStageName, AdTraceStage>;
}
function cloneFreeze<T>(value: T): T {
  const clone = JSON.parse(JSON.stringify(value)) as T;
  const freeze = (item: unknown): void => {
    if (!item || typeof item !== 'object' || Object.isFrozen(item)) return;
    Object.freeze(item);
    Object.values(item as Record<string, unknown>).forEach(freeze);
  };
  freeze(clone);
  return clone;
}
function newSlot(slotId: string): MutableSlot {
  return {
    slotId,
    latestGeneration: 0,
    nextRequestNumber: 0,
    baseStages: stages(),
    generations: [],
  };
}

function generationDiagnostics(requestNumber: number): GenerationTraceDiagnostics {
  return {
    requestNumber,
    terminalState: 'active',
    durations: {},
  };
}

function updateStage(target: Record<AdTraceStageName, AdTraceStage>, event: AdTraceEvent): void {
  const explicit = event.outcome
    ? {
        outcome: event.outcome,
        confidence: event.confidence ?? 'none',
        reason: event.reason ?? 'observed',
      }
    : undefined;
  switch (event.kind) {
    case 'ts_winner_observed':
      target.trustedServer = {
        outcome: 'won',
        confidence: 'definitive',
        reason: 'final_server_winner',
      };
      break;
    case 'ts_auction_observed':
      target.trustedServer = explicit ?? {
        outcome: 'unresolved',
        confidence: 'none',
        reason: 'terminal_summary',
      };
      break;
    case 'prebid_targeting_selected':
      target.prebid = explicit ?? {
        outcome: event.bidTraceId ? 'won' : 'client_bid_won',
        confidence: 'definitive',
        reason: 'selected_targeting',
      };
      break;
    case 'prebid_auction_end':
      if (explicit && target.prebid.confidence !== 'definitive') target.prebid = explicit;
      break;
    case 'prebid_bid_won':
      if (
        target.prebid.outcome === 'won' ||
        target.prebid.outcome === 'client_bid_won' ||
        target.prebid.outcome === 'lost'
      ) {
        // A Prebid win corroborates selection only. It is never creative-load
        // evidence, including when the selected bid originated from Trusted Server.
        target.prebid = {
          ...target.prebid,
          reason: 'selected_targeting_with_bid_won',
        };
        if (
          (target.prebid.outcome === 'client_bid_won' || target.prebid.outcome === 'lost') &&
          target.gam.outcome === 'direct_or_unattributed'
        ) {
          target.gam = {
            outcome: 'client_prebid_candidate',
            confidence: 'probable',
            reason: 'client_bid_won_and_gpt_rendered',
          };
        }
      }
      break;
    case 'prebid_render_succeeded':
      if (target.creative.confidence !== 'definitive') {
        target.creative = {
          outcome: 'prebid_render_succeeded',
          confidence: 'strong',
          reason: event.reason ?? 'prebid_render_succeeded',
        };
      }
      break;
    case 'prebid_render_failed':
      if (target.creative.confidence !== 'definitive') {
        target.creative = {
          outcome: 'render_failed',
          confidence: 'definitive',
          reason: event.reason ?? 'prebid_render_failed',
        };
      }
      break;
    case 'gpt_slot_render_ended':
      // Cooperative acknowledgement is stronger than later GPT callbacks and
      // must never be downgraded to a probable candidate.
      if (target.gam.confidence === 'definitive') break;
      if (explicit?.outcome === 'unresolved') target.gam = explicit;
      else if (event.isEmpty)
        target.gam = { outcome: 'empty', confidence: 'definitive', reason: 'gpt_empty' };
      else if (event.isBackfill)
        target.gam = { outcome: 'backfill', confidence: 'definitive', reason: 'gpt_backfill' };
      else if (event.bidTraceId)
        target.gam = {
          outcome: 'trusted_server_candidate',
          confidence: 'probable',
          reason: 'trace_targeting_rendered',
        };
      else if (
        (target.prebid.outcome === 'client_bid_won' || target.prebid.outcome === 'lost') &&
        target.prebid.reason === 'selected_targeting_with_bid_won'
      )
        target.gam = {
          outcome: 'client_prebid_candidate',
          confidence: 'probable',
          reason: 'client_bid_won_and_gpt_rendered',
        };
      else
        target.gam = {
          outcome: 'direct_or_unattributed',
          confidence: 'probable',
          reason: 'non_empty_unattributed',
        };
      break;
    case 'aps_display_bids_set':
      // APS setting display bids is a handoff only. GAM attribution remains
      // unobserved until a correlated non-empty GPT render arrives.
      break;
    case 'aps_renderer_ready':
      if (target.creative.confidence !== 'definitive') {
        target.creative = {
          outcome: 'aps_renderer_ready',
          confidence: 'strong',
          reason: event.reason ?? 'aps_renderer_ready',
        };
      }
      break;
    case 'gpt_slot_onload':
      if (target.creative.outcome === 'not_observed')
        target.creative = {
          outcome: 'gpt_iframe_onload',
          confidence: 'probable',
          reason: 'gpt_slot_onload',
        };
      break;
    case 'pb_render_served':
      if (target.creative.confidence !== 'definitive') {
        target.creative = {
          outcome: 'renderer_served',
          confidence: 'strong',
          reason: event.reason ?? 'pb_render_response',
        };
      }
      break;
    case 'direct_render_rejected':
      if (target.creative.confidence === 'none') {
        target.creative = {
          outcome: 'rejected',
          confidence: 'none',
          reason: event.reason ?? 'direct_render_rejected',
        };
      }
      break;
    case 'creative_load_acknowledged':
      target.creative = {
        outcome: 'load_acknowledged',
        confidence: 'definitive',
        reason:
          event.reason === 'direct_iframe_load' ? 'direct_iframe_load' : 'source_validated_load',
      };
      if (event.reason !== 'direct_iframe_load') {
        target.gam = {
          outcome: 'trusted_server_won',
          confidence: 'definitive',
          reason: 'creative_load_acknowledged',
        };
      }
      break;
    case 'creative_ack_timed_out':
      if (target.creative.confidence !== 'definitive') {
        target.creative = {
          outcome: 'ack_timed_out',
          confidence: 'none',
          reason: 'ack_timed_out',
        };
      }
      break;
    case 'creative_ack_missing_token':
      if (target.creative.confidence !== 'definitive') {
        target.creative = {
          outcome: 'ack_missing_token',
          confidence: 'none',
          reason: 'ack_missing_token',
        };
      }
      break;
    case 'creative_ack_source_mismatched':
      if (target.creative.confidence !== 'definitive') {
        target.creative = {
          outcome: 'ack_source_mismatched',
          confidence: 'none',
          reason: 'ack_source_mismatched',
        };
      }
      break;
    case 'creative_ack_superseded':
      if (target.creative.confidence !== 'definitive') {
        target.creative = {
          outcome: 'ack_superseded',
          confidence: 'none',
          reason: 'ack_superseded',
        };
      }
      break;
    case 'generation_superseded':
      // Ownership cleanup is lifecycle evidence, not contradictory render
      // evidence. Preserve every previously observed stage unchanged.
      break;
    default:
      break;
  }
}

function snapshot(slot: MutableSlot): SlotTraceSnapshot {
  const latest = slot.generations[slot.generations.length - 1];
  return {
    slotId: slot.slotId,
    latestGeneration: slot.latestGeneration,
    generations: slot.generations.map((item) => ({
      generation: item.generation,
      stages: cloneStages(item.stages),
      diagnostics: cloneFreeze(item.diagnostics),
    })),
    stages: cloneStages(latest?.stages ?? slot.baseStages),
  };
}

const COVERAGE_CATEGORIES: readonly AdTraceCoverageCategory[] = [
  'gpt_requests',
  'gpt_responses',
  'gpt_renders',
  'gpt_loads',
  'gpt_viewability',
  'gpt_visibility',
  'prebid_render_succeeded',
  'prebid_render_failed',
];
const RESPONSE_CLASSES = new Set(['empty', 'backfill', 'reservation', 'unclassified_non_empty']);

function emptyCoverage(): Record<AdTraceCoverageCategory, AdTraceCoverageCounter> {
  return Object.fromEntries(
    COVERAGE_CATEGORIES.map((category) => [
      category,
      { observed: 0, correlated: 0, ambiguous: 0, unmatched: 0, ignored: 0 },
    ])
  ) as Record<AdTraceCoverageCategory, AdTraceCoverageCounter>;
}

function boundedInteger(value: unknown, minimum: number, maximum: number): number | undefined {
  return typeof value === 'number' &&
    Number.isInteger(value) &&
    value >= minimum &&
    value <= maximum
    ? value
    : undefined;
}

function setDuration(
  diagnostics: GenerationTraceDiagnostics,
  name: keyof GenerationTraceDiagnostics['durations'],
  start: number | undefined,
  end: number,
  recordAnomaly: (reason: string) => void
): void {
  if (start === undefined) return;
  const duration = Math.round(end - start);
  if (duration < 0) {
    recordAnomaly('out_of_order_timing');
    return;
  }
  diagnostics.durations[name] = duration;
}

function updateGenerationDiagnostics(
  generation: MutableGeneration,
  event: AdTraceEvent,
  recordAnomaly: (reason: string) => void
): void {
  const { diagnostics, timestamps } = generation;
  switch (event.kind) {
    case 'gpt_request_started':
    case 'gpt_slot_requested':
      timestamps.requestedAt ??= event.timestamp;
      break;
    case 'gpt_slot_response_received':
      timestamps.responseAt ??= event.timestamp;
      setDuration(
        diagnostics,
        'requestToResponseMs',
        timestamps.requestedAt,
        event.timestamp,
        recordAnomaly
      );
      break;
    case 'gpt_slot_render_ended':
      timestamps.renderedAt ??= event.timestamp;
      diagnostics.terminalState = event.isEmpty ? 'empty' : 'rendered';
      if (event.responseClass) diagnostics.responseClass = event.responseClass;
      if (event.renderedWidth !== undefined && event.renderedHeight !== undefined) {
        diagnostics.renderedSize = [event.renderedWidth, event.renderedHeight];
      }
      if (event.slotContentChanged !== undefined) {
        diagnostics.slotContentChanged = event.slotContentChanged;
      }
      if (event.sizeMatchesConfigured !== undefined) {
        diagnostics.sizeMatchesConfigured = event.sizeMatchesConfigured;
      }
      setDuration(
        diagnostics,
        'responseToRenderMs',
        timestamps.responseAt,
        event.timestamp,
        recordAnomaly
      );
      break;
    case 'gpt_slot_onload':
      timestamps.iframeLoadedAt ??= event.timestamp;
      setDuration(
        diagnostics,
        'renderToIframeLoadMs',
        timestamps.renderedAt,
        event.timestamp,
        recordAnomaly
      );
      break;
    case 'gpt_impression_viewable':
      timestamps.viewableAt ??= event.timestamp;
      setDuration(
        diagnostics,
        'renderToViewableMs',
        timestamps.renderedAt,
        event.timestamp,
        recordAnomaly
      );
      break;
    case 'gpt_slot_visibility_changed':
      if (event.inViewPercentage !== undefined) {
        diagnostics.currentVisibilityPercentage = event.inViewPercentage;
        diagnostics.maximumVisibilityPercentage = Math.max(
          diagnostics.maximumVisibilityPercentage ?? 0,
          event.inViewPercentage
        );
      }
      break;
    case 'prebid_render_succeeded':
      diagnostics.prebidRender = 'succeeded';
      break;
    case 'prebid_render_failed':
      diagnostics.prebidRender = 'failed';
      break;
    case 'creative_load_acknowledged':
      timestamps.acknowledgedAt ??= event.timestamp;
      diagnostics.acknowledgement = 'confirmed';
      setDuration(
        diagnostics,
        'renderToCreativeAcknowledgementMs',
        timestamps.renderedAt,
        event.timestamp,
        recordAnomaly
      );
      break;
    case 'creative_ack_timed_out':
      if (diagnostics.acknowledgement !== 'confirmed') diagnostics.acknowledgement = 'timed_out';
      break;
    case 'creative_ack_superseded':
      if (diagnostics.acknowledgement !== 'confirmed') diagnostics.acknowledgement = 'superseded';
      break;
    case 'creative_ack_source_mismatched':
      if (diagnostics.acknowledgement !== 'confirmed') {
        diagnostics.acknowledgement = 'source_mismatched';
      }
      break;
    case 'creative_ack_missing_token':
      if (diagnostics.acknowledgement !== 'confirmed') {
        diagnostics.acknowledgement = 'missing_token';
      }
      break;
    case 'generation_superseded':
      diagnostics.terminalState = 'superseded';
      break;
    default:
      break;
  }
  if (event.prebidAuctionDurationMs !== undefined) {
    diagnostics.durations.prebidAuctionMs = event.prebidAuctionDurationMs;
  }
}

function isRenderEvent(kind: AdTraceEventKind): boolean {
  return (
    kind === 'gpt_request_started' ||
    kind === 'gpt_slot_render_ended' ||
    kind === 'gpt_impression_viewable' ||
    kind === 'aps_renderer_ready' ||
    kind === 'prebid_render_succeeded' ||
    kind === 'prebid_render_failed' ||
    kind === 'pb_render_requested' ||
    kind === 'pb_render_rejected' ||
    kind === 'pb_render_served' ||
    kind === 'direct_render_rejected' ||
    kind === 'creative_load_acknowledged' ||
    kind === 'creative_ack_timed_out' ||
    kind === 'creative_ack_superseded' ||
    kind === 'creative_ack_source_mismatched' ||
    kind === 'creative_ack_missing_token' ||
    kind === 'generation_superseded'
  );
}

function renderSource(event: AdTraceEvent): RenderTraceSnapshot['source'] {
  if (event.reason?.startsWith('direct_')) return 'direct_auction';
  if (event.kind.startsWith('pb_render_') || event.kind === 'creative_load_acknowledged')
    return 'pb_render';
  return 'gpt';
}

function renderOutcome(
  current: Pick<RenderTraceSnapshot, 'outcome' | 'confidence'>,
  event: AdTraceEvent
): { outcome: RenderTraceOutcome; confidence: AdTraceConfidence } {
  if (current.outcome === 'confirmed') return { outcome: 'confirmed', confidence: 'definitive' };
  if (current.outcome === 'empty' && current.confidence === 'definitive') {
    return { outcome: 'empty', confidence: 'definitive' };
  }
  if (event.kind === 'creative_load_acknowledged')
    return { outcome: 'confirmed', confidence: 'definitive' };
  if (event.kind === 'creative_ack_timed_out') return { outcome: 'timed_out', confidence: 'none' };
  if (current.outcome === 'timed_out') return { outcome: 'timed_out', confidence: 'none' };
  if (current.outcome === 'served') return { outcome: 'served', confidence: 'strong' };
  if (event.kind === 'pb_render_served') return { outcome: 'served', confidence: 'strong' };
  if (event.kind === 'gpt_slot_render_ended') {
    if (event.outcome === 'unresolved') return { outcome: 'unresolved', confidence: 'none' };
    return event.isEmpty
      ? { outcome: 'empty', confidence: 'definitive' }
      : { outcome: 'gam_only', confidence: 'probable' };
  }
  if (current.outcome === 'gam_only') return { outcome: 'gam_only', confidence: 'probable' };
  return { outcome: 'unresolved', confidence: 'none' };
}

export function createAdTraceStore(
  now: () => number = () => (typeof performance === 'undefined' ? Date.now() : performance.now())
): AdTraceStore {
  const slots = new Map<string, MutableSlot>();
  const events: AdTraceEvent[] = [];
  const renders: RenderTraceSnapshot[] = [];
  const renderByGeneration = new Map<string, RenderTraceSnapshot>();
  const elementByGeneration = new Map<string, HTMLElement>();
  const listeners = new Set<() => void>();
  let sequence = 0;
  let generationSequence = 0;
  let renderSequence = 0;
  let droppedEvents = 0;
  let evictedSlots = 0;
  const coverage = emptyCoverage();
  const anomalies = new Map<string, number>();
  const recordAnomaly = (reason: string): void => {
    const safeReason = safeLabel(reason);
    if (!safeReason) return;
    if (!anomalies.has(safeReason) && anomalies.size >= AD_TRACE_MAX_ANOMALIES) {
      anomalies.set('other', (anomalies.get('other') ?? 0) + 1);
      return;
    }
    anomalies.set(safeReason, (anomalies.get(safeReason) ?? 0) + 1);
  };
  const ensureSlot = (slotId: string): MutableSlot => {
    let slot = slots.get(slotId);
    if (slot) return slot;
    if (slots.size >= AD_TRACE_MAX_SLOTS) {
      const oldest = slots.keys().next().value as string | undefined;
      if (oldest) {
        slots.delete(oldest);
        evictedSlots += 1;
      }
    }
    slot = newSlot(slotId);
    slots.set(slotId, slot);
    return slot;
  };
  const notify = (): void => listeners.forEach((listener) => listener());
  const emitRender = (render: RenderTraceSnapshot): void => {
    if (typeof window === 'undefined' || typeof CustomEvent === 'undefined') return;
    window.dispatchEvent(new CustomEvent('tsjs:adRendered', { detail: cloneFreeze(render) }));
  };
  const updateRender = (event: AdTraceEvent, slotId: string, generation: number): void => {
    if (!isRenderEvent(event.kind)) return;
    const key = `${slotId}:${generation}`;
    let render = renderByGeneration.get(key);
    const timestamp = event.timestamp;
    if (!render) {
      render = {
        sequence: ++renderSequence,
        slotId,
        generation,
        source: renderSource(event),
        outcome: 'unresolved',
        confidence: 'none',
        visibility: 'unknown',
        createdAt: timestamp,
        updatedAt: timestamp,
      };
      renderByGeneration.set(key, render);
      renders.push(render);
      if (renders.length > AD_TRACE_MAX_RENDERS) {
        const evicted = renders.shift();
        if (evicted) {
          const evictedKey = `${evicted.slotId}:${evicted.generation}`;
          renderByGeneration.delete(evictedKey);
          elementByGeneration.delete(evictedKey);
        }
      }
    }
    const next = renderOutcome(render, event);
    render.outcome = next.outcome;
    render.confidence = next.confidence;
    if (event.reason?.startsWith('direct_')) render.source = 'direct_auction';
    else if (event.kind.startsWith('pb_render_') || event.kind === 'creative_load_acknowledged')
      render.source = render.source === 'direct_auction' ? render.source : 'pb_render';
    if (event.auctionTraceId) render.auctionTraceId = event.auctionTraceId;
    if (event.bidTraceId) render.bidTraceId = event.bidTraceId;
    if (event.reason) render.reason = event.reason;
    if (event.kind === 'gpt_impression_viewable') render.viewability = 'viewable';
    render.updatedAt = timestamp;
    emitRender(render);
  };

  return {
    record(observation) {
      if (!EVENT_KINDS.has(observation.kind)) return;
      if (observation.confidence && !CONFIDENCES.has(observation.confidence)) return;
      const slotId = safeLabel(observation.slotId);
      const generation =
        Number.isInteger(observation.generation) && (observation.generation ?? 0) > 0
          ? observation.generation
          : undefined;
      const event: AdTraceEvent = {
        sequence: ++sequence,
        timestamp: now(),
        kind: observation.kind,
        ...(slotId ? { slotId } : {}),
        ...(generation ? { generation } : {}),
        ...(safeUuid(observation.auctionTraceId)
          ? { auctionTraceId: observation.auctionTraceId }
          : {}),
        ...(safeUuid(observation.bidTraceId) ? { bidTraceId: observation.bidTraceId } : {}),
        ...(safeLabel(observation.provider) ? { provider: observation.provider } : {}),
        ...(safeLabel(observation.bidder) ? { bidder: observation.bidder } : {}),
        ...(safeLabel(observation.outcome) ? { outcome: observation.outcome } : {}),
        ...(observation.confidence ? { confidence: observation.confidence } : {}),
        ...(safeLabel(observation.reason) ? { reason: observation.reason } : {}),
        ...(typeof observation.isEmpty === 'boolean' ? { isEmpty: observation.isEmpty } : {}),
        ...(typeof observation.isBackfill === 'boolean'
          ? { isBackfill: observation.isBackfill }
          : {}),
        ...(RESPONSE_CLASSES.has(observation.responseClass ?? '')
          ? { responseClass: observation.responseClass }
          : {}),
        ...(boundedInteger(observation.renderedWidth, 1, 10_000) !== undefined
          ? { renderedWidth: observation.renderedWidth }
          : {}),
        ...(boundedInteger(observation.renderedHeight, 1, 10_000) !== undefined
          ? { renderedHeight: observation.renderedHeight }
          : {}),
        ...(typeof observation.slotContentChanged === 'boolean'
          ? { slotContentChanged: observation.slotContentChanged }
          : {}),
        ...(typeof observation.sizeMatchesConfigured === 'boolean'
          ? { sizeMatchesConfigured: observation.sizeMatchesConfigured }
          : {}),
        ...(boundedInteger(observation.inViewPercentage, 0, 100) !== undefined
          ? { inViewPercentage: observation.inViewPercentage }
          : {}),
        ...(boundedInteger(observation.prebidAuctionDurationMs, 0, 300_000) !== undefined
          ? { prebidAuctionDurationMs: observation.prebidAuctionDurationMs }
          : {}),
      };
      if (event.kind !== 'gpt_slot_visibility_changed') {
        events.push(event);
        if (events.length > AD_TRACE_MAX_EVENTS) {
          events.shift();
          droppedEvents += 1;
        }
      }
      if (slotId) {
        const slot = ensureSlot(slotId);
        const exact = generation
          ? slot.generations.find((item) => item.generation === generation)
          : undefined;
        if (exact) {
          updateStage(exact.stages, event);
          updateGenerationDiagnostics(exact, event, recordAnomaly);
        } else if (
          !generation &&
          (event.kind === 'ts_winner_observed' || event.kind === 'ts_auction_observed')
        ) {
          // Generationless server evidence seeds only the next request. Updating
          // the latest retained generation would rewrite prior-navigation history.
          updateStage(slot.baseStages, event);
        }
        if (generation) {
          if (!exact) recordAnomaly('missing_retained_generation');
          updateRender(event, slotId, generation);
        }
      }
      notify();
    },
    recordCoverage(observation) {
      const counter = coverage[observation.category];
      if (!counter) return;
      counter.observed += 1;
      counter[observation.resolution] += 1;
      if (observation.resolution === 'ambiguous' || observation.resolution === 'unmatched') {
        recordAnomaly(observation.reason ?? `${observation.category}_${observation.resolution}`);
      }
      notify();
    },
    nextGeneration(slotId) {
      const safeSlotId = safeLabel(slotId);
      if (!safeSlotId) return 0;
      const slot = ensureSlot(safeSlotId);
      slot.latestGeneration = ++generationSequence;
      slot.nextRequestNumber += 1;
      slot.generations.push({
        generation: slot.latestGeneration,
        stages: cloneStages(slot.baseStages),
        diagnostics: generationDiagnostics(slot.nextRequestNumber),
        timestamps: {},
      });
      // Generationless server evidence belongs to one future request only.
      slot.baseStages = stages();
      if (slot.generations.length > AD_TRACE_MAX_GENERATIONS) slot.generations.shift();
      notify();
      return slot.latestGeneration;
    },
    getSlot(slotId) {
      const slot = slots.get(slotId);
      return slot ? cloneFreeze(snapshot(slot)) : undefined;
    },
    getEvents() {
      return cloneFreeze(events);
    },
    getRenderTimeline() {
      return cloneFreeze(renders);
    },
    export() {
      const value: AdTraceExport = {
        version: 1,
        slots: [...slots.values()].map(snapshot),
        events,
        renders,
        metadata: {
          droppedEvents,
          evictedSlots,
          coverage,
          anomalies: Object.fromEntries(anomalies),
        },
      };
      return cloneFreeze(value);
    },
    subscribe(listener) {
      if (listeners.size >= AD_TRACE_MAX_LISTENERS) return () => {};
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    bindElement(slotId, generation, element) {
      const safeSlotId = safeLabel(slotId);
      if (!safeSlotId || !Number.isInteger(generation) || generation <= 0) return;
      const key = `${safeSlotId}:${generation}`;
      if (!elementByGeneration.has(key) && elementByGeneration.size >= AD_TRACE_MAX_RENDERS) {
        const oldest = elementByGeneration.keys().next().value as string | undefined;
        if (oldest) elementByGeneration.delete(oldest);
      }
      elementByGeneration.set(key, element);
    },
    getBoundElement(slotId, generation) {
      const safeSlotId = safeLabel(slotId);
      if (!safeSlotId || !Number.isInteger(generation) || generation <= 0) return undefined;
      return elementByGeneration.get(`${safeSlotId}:${generation}`);
    },
    updateVisibility(slotId, generation, visibility) {
      const safeSlotId = safeLabel(slotId);
      if (!safeSlotId || !Number.isInteger(generation) || generation <= 0) return;
      const render = renderByGeneration.get(`${safeSlotId}:${generation}`);
      if (!render || render.visibility === visibility) return;
      render.visibility = visibility;
      render.updatedAt = now();
      emitRender(render);
      notify();
    },
  };
}

/**
 * Map a public terminal auction outcome to an internal stage outcome.
 *
 * A completed auction without a final slot winner is a no-bid result. A
 * completed auction with a winner is immediately followed by winner evidence,
 * but remains distinct here so callers never erase failed or abandoned results.
 */
export function terminalSummaryStageOutcome(outcome: string, hasWinner = false): string {
  if (outcome === 'completed') return hasWinner ? 'completed' : 'no_bid';
  return outcome;
}

export function isCanonicalTraceUuid(value: unknown): value is string {
  return safeUuid(value) !== undefined;
}
export function isBoundedTraceLabel(value: unknown): value is string {
  return safeLabel(value) !== undefined;
}
