import { createAdTraceStore, isBoundedTraceLabel, isCanonicalTraceUuid } from '../../core/ad_trace';
import type { AdTraceApi, AuctionBidData, AuctionTraceSummary, TsjsApi } from '../../core/types';

import { installAdTraceOverlay } from './overlay';

const TRACE_SOURCES = new Set(['initial_navigation', 'spa_navigation', 'auction_api']);
const TRACE_OUTCOMES = new Set(['completed', 'no_bid', 'skipped', 'failed', 'abandoned']);

function validSummary(value: AuctionTraceSummary | undefined): value is AuctionTraceSummary {
  return (
    value?.version === 1 &&
    isCanonicalTraceUuid(value.auctionTraceId) &&
    TRACE_SOURCES.has(value.source) &&
    TRACE_OUTCOMES.has(value.outcome)
  );
}

function validBid(value: AuctionBidData | undefined, slotId: string): boolean {
  const trace = value?.trace;
  return !!(
    trace?.version === 1 &&
    trace.slotId === slotId &&
    isCanonicalTraceUuid(trace.auctionTraceId) &&
    isCanonicalTraceUuid(trace.bidTraceId) &&
    isBoundedTraceLabel(trace.provider) &&
    isBoundedTraceLabel(trace.bidder)
  );
}

function consumeActiveBootstrap(): boolean {
  if (window.__tsjs_adTraceActive !== true) return false;
  delete window.__tsjs_adTraceActive;
  return true;
}

/** Install the session-scoped recorder, immutable API, and overlay once. */
export function installAdTrace(): boolean {
  if (typeof window === 'undefined') return false;
  if (window.tsjs?.adTrace) return true;
  if (!consumeActiveBootstrap()) return false;
  const ts = (window.tsjs ??= {} as TsjsApi);

  const store = createAdTraceStore();
  const api: AdTraceApi = Object.freeze({
    getSlot: store.getSlot,
    getEvents: store.getEvents,
    getRenderTimeline: store.getRenderTimeline,
    export: store.export,
  });
  ts.adTrace = api;
  ts.recordAdTrace = store.record;
  ts.nextAdTraceGeneration = store.nextGeneration;
  ts.subscribeAdTrace = store.subscribe;
  ts.bindAdTraceElement = store.bindElement;
  ts.getAdTraceElement = store.getBoundElement;
  ts.updateAdTraceVisibility = store.updateVisibility;
  if (!ts.captureAdTraceRequest) {
    ts.captureAdTraceRequest = (slot, trigger, snapshot) => {
      const pending = (ts.pendingAdTraceRequests ??= []);
      if (pending.length < 64) pending.push({ slot, trigger, snapshot });
      return 0;
    };
  }

  const summary = validSummary(ts.auctionTrace) ? ts.auctionTrace : undefined;
  for (const slot of ts.adSlots ?? []) {
    const bid = ts.bids?.[slot.id];
    if (validBid(bid, slot.id) && bid?.trace) {
      store.record({
        kind: 'ts_winner_observed',
        slotId: slot.id,
        auctionTraceId: bid.trace.auctionTraceId,
        bidTraceId: bid.trace.bidTraceId,
        provider: bid.trace.provider,
        bidder: bid.trace.bidder,
      });
    } else if (summary) {
      store.record({
        kind: 'ts_auction_observed',
        slotId: slot.id,
        auctionTraceId: summary.auctionTraceId,
        outcome:
          summary.outcome === 'completed' || summary.outcome === 'no_bid'
            ? 'no_bid'
            : summary.outcome === 'skipped'
              ? 'skipped'
              : 'unresolved',
        confidence: 'definitive',
        reason: 'terminal_summary',
      });
    }
  }

  installAdTraceOverlay(api, store.subscribe);
  return true;
}

if (typeof window !== 'undefined') installAdTrace();
