import type {
  AdTraceApi,
  AdTraceConfidence,
  AdTraceEvent,
  AdTraceEventKind,
  AdTraceExport,
  AdTraceObservation,
  AdTraceStage,
  AdTraceStageName,
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
  'aps_display_bids_set',
  'pb_render_requested',
  'pb_render_rejected',
  'pb_render_served',
  'direct_render_rejected',
  'creative_load_acknowledged',
  'generation_superseded',
]);
const CONFIDENCES = new Set(['definitive', 'strong', 'probable', 'none']);
const EMPTY_STAGE: AdTraceStage = { outcome: 'not_observed', confidence: 'none', reason: 'none' };

type MutableGeneration = GenerationTraceSnapshot;
interface MutableSlot {
  slotId: string;
  latestGeneration: number;
  baseStages: Record<AdTraceStageName, AdTraceStage>;
  generations: MutableGeneration[];
}

export interface AdTraceStore extends AdTraceApi {
  record(observation: AdTraceObservation): void;
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
  return { slotId, latestGeneration: 0, baseStages: stages(), generations: [] };
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
      if (target.prebid.outcome === 'client_bid_won' || target.prebid.outcome === 'lost') {
        target.prebid = {
          ...target.prebid,
          reason: 'selected_targeting_with_bid_won',
        };
        if (target.gam.outcome === 'direct_or_unattributed') {
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
        reason: 'source_validated_load',
      };
      if (event.reason !== 'direct_iframe_load') {
        target.gam = {
          outcome: 'trusted_server_won',
          confidence: 'definitive',
          reason: 'creative_load_acknowledged',
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
  const latest = slot.generations.at(-1);
  return {
    slotId: slot.slotId,
    latestGeneration: slot.latestGeneration,
    generations: slot.generations.map((item) => ({
      generation: item.generation,
      stages: cloneStages(item.stages),
    })),
    stages: cloneStages(latest?.stages ?? slot.baseStages),
  };
}

function isRenderEvent(kind: AdTraceEventKind): boolean {
  return (
    kind === 'gpt_request_started' ||
    kind === 'gpt_slot_render_ended' ||
    kind === 'prebid_render_succeeded' ||
    kind === 'prebid_render_failed' ||
    kind === 'pb_render_requested' ||
    kind === 'pb_render_rejected' ||
    kind === 'pb_render_served' ||
    kind === 'direct_render_rejected' ||
    kind === 'creative_load_acknowledged' ||
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
  if (current.outcome === 'served') return { outcome: 'served', confidence: 'strong' };
  if (event.kind === 'pb_render_served') return { outcome: 'served', confidence: 'strong' };
  if (event.kind === 'gpt_slot_render_ended')
    return event.isEmpty
      ? { outcome: 'empty', confidence: 'definitive' }
      : { outcome: 'gam_only', confidence: 'probable' };
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
    const timestamp = now();
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
      };
      events.push(event);
      if (events.length > AD_TRACE_MAX_EVENTS) {
        events.shift();
        droppedEvents += 1;
      }
      if (slotId) {
        const slot = ensureSlot(slotId);
        const exact = generation
          ? slot.generations.find((item) => item.generation === generation)
          : undefined;
        if (exact) updateStage(exact.stages, event);
        else if (
          !generation &&
          (event.kind === 'ts_winner_observed' || event.kind === 'ts_auction_observed')
        ) {
          // Generationless server evidence seeds only the next request. Updating
          // the latest retained generation would rewrite prior-navigation history.
          updateStage(slot.baseStages, event);
        }
        if (generation) updateRender(event, slotId, generation);
      }
      notify();
    },
    nextGeneration(slotId) {
      const safeSlotId = safeLabel(slotId);
      if (!safeSlotId) return 0;
      const slot = ensureSlot(safeSlotId);
      slot.latestGeneration = ++generationSequence;
      slot.generations.push({
        generation: slot.latestGeneration,
        stages: cloneStages(slot.baseStages),
      });
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
        metadata: { droppedEvents, evictedSlots },
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

export function isCanonicalTraceUuid(value: unknown): value is string {
  return safeUuid(value) !== undefined;
}
export function isBoundedTraceLabel(value: unknown): value is string {
  return safeLabel(value) !== undefined;
}
