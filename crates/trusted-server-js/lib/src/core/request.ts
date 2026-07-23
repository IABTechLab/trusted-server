// Request orchestration for tsjs: unified auction endpoint with iframe-based creative rendering.
import { renderApsCreative } from '../integrations/aps/render';

import { terminalSummaryStageOutcome } from './ad_trace';
import { buildAdRequest, sendAuction } from './auction';
import { collectContext } from './context';
import { log } from './log';
import { getAllUnits, firstSize } from './registry';
import { createAdIframe, findSlot, buildCreativeDocument, sanitizeCreativeHtml } from './render';
import type { AuctionTraceSummary, TrustedServerBidTrace } from './types';

export type RequestAdsCallback = () => void;
export interface RequestAdsOptions {
  bidsBackHandler?: RequestAdsCallback;
  timeout?: number;
}

const MAX_DIRECT_RENDER_OWNERS = 64;

interface DirectRenderOwner {
  token: symbol;
  slotId: string;
  generation?: number;
}

const latestDirectOwners = new Map<string, DirectRenderOwner>();

type RenderCreativeInlineOptions = {
  slotId: string;
  // Accept unknown input here because bidder JSON is untrusted at runtime.
  creativeHtml: unknown;
  creativeWidth?: number;
  creativeHeight?: number;
  seat: string;
  creativeId: string;
  owner: DirectRenderOwner;
  trace?: TrustedServerBidTrace;
};

function claimDirectOwner(slotId: string): DirectRenderOwner {
  const previous = latestDirectOwners.get(slotId);
  if (previous) recordDirectRejection(previous, 'direct_owner_replaced');
  const ts = window.tsjs;
  const generation = ts?.recordAdTrace ? ts.nextAdTraceGeneration?.(slotId) : undefined;
  const owner: DirectRenderOwner = {
    token: Symbol(slotId),
    slotId,
    ...(generation && generation > 0 ? { generation } : {}),
  };
  latestDirectOwners.delete(slotId);
  latestDirectOwners.set(slotId, owner);
  if (latestDirectOwners.size > MAX_DIRECT_RENDER_OWNERS) {
    const oldest = latestDirectOwners.keys().next().value as string | undefined;
    if (oldest) {
      const evicted = latestDirectOwners.get(oldest);
      if (evicted) recordDirectRejection(evicted, 'direct_owner_evicted');
      latestDirectOwners.delete(oldest);
    }
  }
  return owner;
}

function ownerIsCurrent(owner: DirectRenderOwner): boolean {
  return latestDirectOwners.get(owner.slotId) === owner;
}

function recordRootSummary(
  summary: AuctionTraceSummary | undefined,
  owner: DirectRenderOwner,
  hasWinner: boolean
): void {
  if (!summary || !owner.generation) return;
  window.tsjs?.recordAdTrace?.({
    kind: 'ts_auction_observed',
    slotId: owner.slotId,
    generation: owner.generation,
    auctionTraceId: summary.auctionTraceId,
    outcome: terminalSummaryStageOutcome(summary.outcome, hasWinner),
    confidence: 'definitive',
    reason: 'terminal_summary',
  });
}

function recordDirectRejection(owner: DirectRenderOwner, reason: string): void {
  if (!owner.generation) return;
  window.tsjs?.recordAdTrace?.({
    kind: 'direct_render_rejected',
    slotId: owner.slotId,
    generation: owner.generation,
    reason,
  });
}

// Entry point matching Prebid's requestBids signature; uses unified /auction endpoint.
export function requestAds(
  callbackOrOpts?: RequestAdsCallback | RequestAdsOptions,
  maybeOpts?: RequestAdsOptions
): void {
  let callback: RequestAdsCallback | undefined;
  let opts: RequestAdsOptions | undefined;
  if (typeof callbackOrOpts === 'function') {
    callback = callbackOrOpts as RequestAdsCallback;
    opts = maybeOpts;
  } else {
    opts = callbackOrOpts as RequestAdsOptions | undefined;
    callback = opts?.bidsBackHandler;
  }

  log.info('requestAds: called', { hasCallback: typeof callback === 'function' });
  try {
    const adUnits = getAllUnits();
    const requestedSlotIds = [
      ...new Set(
        adUnits
          .map((unit) => unit.code)
          .filter((code): code is string => typeof code === 'string' && code.length > 0)
      ),
    ];
    const owners = new Map(requestedSlotIds.map((slotId) => [slotId, claimDirectOwner(slotId)]));
    const config = collectContext();
    const payload = { ...buildAdRequest(adUnits), config };
    log.debug('requestAds: payload', { units: adUnits.length, contextKeys: Object.keys(config) });

    void sendAuction('/auction', payload).then((result) => {
      if (result.kind !== 'ok') {
        for (const owner of owners.values()) {
          if (ownerIsCurrent(owner)) {
            recordDirectRejection(owner, `${result.kind}_${result.reason}`);
          }
        }
        return;
      }

      log.info('requestAds: got bids', { count: result.bids.length });
      const bySlot = new Map<string, typeof result.bids>();
      for (const bid of result.bids) {
        if (!owners.has(bid.impid)) continue;
        const existing = bySlot.get(bid.impid) ?? [];
        existing.push(bid);
        bySlot.set(bid.impid, existing);
      }

      for (const [slotId, owner] of owners) {
        if (!ownerIsCurrent(owner)) continue;
        const slotBids = bySlot.get(slotId) ?? [];
        recordRootSummary(result.summary, owner, slotBids.length > 0);
        if (slotBids.length === 0) continue;
        if (slotBids.length !== 1) {
          recordDirectRejection(owner, 'ambiguous_winner');
          continue;
        }

        const bid = slotBids[0];
        const trace =
          bid.trace &&
          result.summary &&
          bid.trace.slotId === slotId &&
          bid.trace.auctionTraceId === result.summary.auctionTraceId
            ? bid.trace
            : undefined;
        if (trace && owner.generation) {
          window.tsjs?.recordAdTrace?.({
            kind: 'ts_winner_observed',
            slotId,
            generation: owner.generation,
            auctionTraceId: trace.auctionTraceId,
            bidTraceId: trace.bidTraceId,
            provider: trace.provider,
            bidder: trace.bidder,
          });
        }
        if (bid.renderer) {
          if (!ownerIsCurrent(owner)) continue;
          const started = renderApsCreative({
            slotId,
            renderer: bid.renderer,
            onReady: () => {
              if (!ownerIsCurrent(owner) || !owner.generation) return;
              window.tsjs?.recordAdTrace?.({
                kind: 'aps_renderer_ready',
                slotId,
                generation: owner.generation,
                auctionTraceId: trace?.auctionTraceId,
                bidTraceId: trace?.bidTraceId,
                reason: 'direct_aps_renderer_ready',
              });
            },
          });
          if (!started) {
            recordDirectRejection(owner, 'aps_render_rejected');
          } else if (owner.generation) {
            window.tsjs?.recordAdTrace?.({
              kind: 'pb_render_served',
              slotId,
              generation: owner.generation,
              auctionTraceId: trace?.auctionTraceId,
              bidTraceId: trace?.bidTraceId,
              reason: 'direct_aps_renderer',
            });
          }
          continue;
        }
        if (!bid.adm) {
          recordDirectRejection(owner, 'missing_adm');
          continue;
        }
        renderCreativeInline({
          slotId,
          creativeHtml: bid.adm,
          creativeWidth: bid.width,
          creativeHeight: bid.height,
          seat: bid.seat,
          creativeId: bid.creativeId,
          owner,
          ...(trace ? { trace } : {}),
        });
      }
      log.info('requestAds: rendered creatives from response');
    });

    // Synchronously invoke callback to match test expectations
    try {
      if (callback) callback();
    } catch {
      /* ignore callback errors */
    }
  } catch {
    log.warn('requestAds: failed to initiate');
  }
}

// Render a creative by writing sanitized, non-executable HTML into a sandboxed iframe.
function renderCreativeInline({
  slotId,
  creativeHtml,
  creativeWidth,
  creativeHeight,
  seat,
  creativeId,
  owner,
  trace,
}: RenderCreativeInlineOptions): void {
  if (!ownerIsCurrent(owner)) return;
  const container = findSlot(slotId) as HTMLElement | null;
  if (!container) {
    recordDirectRejection(owner, 'slot_missing');
    log.warn('renderCreativeInline: slot not found; skipping render', { slotId, seat, creativeId });
    return;
  }

  try {
    if (owner.generation) {
      window.tsjs?.bindAdTraceElement?.(slotId, owner.generation, container);
    }
    const sanitization = sanitizeCreativeHtml(creativeHtml);
    if (sanitization.kind === 'rejected') {
      recordDirectRejection(owner, 'creative_rejected');
      log.warn('renderCreativeInline: rejected creative', {
        slotId,
        seat,
        creativeId,
        originalLength: sanitization.originalLength,
        rejectionReason: sanitization.rejectionReason,
      });
      return;
    }

    if (!ownerIsCurrent(owner)) return;
    // Clear the slot only after sanitization succeeds so rejected creatives never blank existing content.
    container.innerHTML = '';

    // Determine size with fallback chain: creative size → ad unit size → 300x250
    let width: number;
    let height: number;

    if (creativeWidth && creativeHeight && creativeWidth > 0 && creativeHeight > 0) {
      width = creativeWidth;
      height = creativeHeight;
      log.debug('renderCreativeInline: using creative dimensions', { width, height });
    } else {
      const unit = getAllUnits().find((u) => u.code === slotId);
      const size = (unit && firstSize(unit)) || [300, 250];
      width = size[0];
      height = size[1];
      log.debug('renderCreativeInline: using ad unit dimensions', { width, height });
    }

    const iframe = createAdIframe(container, {
      name: `tsjs_iframe_${slotId}`,
      title: 'Ad content',
      width,
      height,
    });
    iframe.addEventListener(
      'load',
      () => {
        if (!ownerIsCurrent(owner) || !iframe.isConnected || iframe.parentElement !== container)
          return;
        if (owner.generation) {
          window.tsjs?.recordAdTrace?.({
            kind: 'creative_load_acknowledged',
            slotId,
            generation: owner.generation,
            auctionTraceId: trace?.auctionTraceId,
            bidTraceId: trace?.bidTraceId,
            reason: 'direct_iframe_load',
          });
        }
      },
      { once: true }
    );

    iframe.srcdoc = buildCreativeDocument(sanitization.sanitizedHtml);
    if (owner.generation) {
      window.tsjs?.recordAdTrace?.({
        kind: 'pb_render_served',
        slotId,
        generation: owner.generation,
        auctionTraceId: trace?.auctionTraceId,
        bidTraceId: trace?.bidTraceId,
        reason: 'direct_iframe_created',
      });
    }

    log.info('renderCreativeInline: rendered', {
      slotId,
      seat,
      creativeId,
      width,
      height,
      originalLength: sanitization.originalLength,
    });
  } catch (err) {
    recordDirectRejection(owner, 'render_failed');
    log.warn('renderCreativeInline: failed', { slotId, seat, creativeId, err });
  }
}
