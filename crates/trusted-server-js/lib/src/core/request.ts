// Request orchestration for tsjs: unified auction endpoint with iframe-based creative rendering.
import { renderApsCreative } from '../integrations/aps/render';

import { buildAdRequest, sendAuction } from './auction';
import { recordRender, stampCreativeTrace } from './trace';
import { collectContext } from './context';
import { log } from './log';
import { getAllUnits, firstSize } from './registry';
import { createAdIframe, findSlot, buildCreativeDocument, sanitizeCreativeHtml } from './render';

export type RequestAdsCallback = () => void;
export interface RequestAdsOptions {
  bidsBackHandler?: RequestAdsCallback;
  timeout?: number;
}

type RenderCreativeInlineOptions = {
  slotId: string;
  // Accept unknown input here because bidder JSON is untrusted at runtime.
  creativeHtml: unknown;
  creativeWidth?: number;
  creativeHeight?: number;
  seat: string;
  creativeId: string;
  auctionId?: string;
  admHash?: string;
};

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
    const config = collectContext();
    const payload = { ...buildAdRequest(adUnits), config };
    log.debug('requestAds: payload', { units: adUnits.length, contextKeys: Object.keys(config) });

    // Use unified auction endpoint
    void sendAuction('/auction', payload)
      .then((bids) => {
        log.info('requestAds: got bids', { count: bids.length });
        for (const bid of bids) {
          if (!bid.impid) continue;
          if (bid.renderer) {
            renderApsCreative({ slotId: bid.impid, renderer: bid.renderer });
            continue;
          }
          if (!bid.adm) {
            log.debug('requestAds: bid has no adm, skipping', { slotId: bid.impid });
            continue;
          }
          renderCreativeInline({
            slotId: bid.impid,
            creativeHtml: bid.adm,
            creativeWidth: bid.width,
            creativeHeight: bid.height,
            seat: bid.seat,
            creativeId: bid.creativeId,
            auctionId: bid.auctionId,
            admHash: bid.admHash,
          });
        }
        log.info('requestAds: rendered creatives from response');
      })
      .catch((err) => {
        log.warn('requestAds: auction failed', err);
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
  auctionId,
  admHash,
}: RenderCreativeInlineOptions): void {
  const trace = {
    slotId,
    path: 'auction' as const,
    auctionId,
    bidder: seat,
    creativeId,
    admHash,
    servedFrom: 'inline' as const,
  };
  const container = findSlot(slotId) as HTMLElement | null;
  if (!container) {
    log.warn('renderCreativeInline: slot not found; skipping render', { slotId, seat, creativeId });
    recordRender({ ...trace, rendered: false });
    return;
  }

  try {
    const sanitization = sanitizeCreativeHtml(creativeHtml);
    if (sanitization.kind === 'rejected') {
      log.warn('renderCreativeInline: rejected creative', {
        slotId,
        seat,
        creativeId,
        originalLength: sanitization.originalLength,
        rejectionReason: sanitization.rejectionReason,
      });
      // Stamp rendered:false so the DOM marker semantics match the SSAT path
      // (explicit false on a failed render, not just an absent attribute).
      const rejectedRecord = recordRender({
        ...trace,
        rendered: false,
        elementId: container.id || undefined,
      });
      stampCreativeTrace(container, rejectedRecord);
      return;
    }

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

    iframe.srcdoc = buildCreativeDocument(sanitization.sanitizedHtml);

    // Trace: registry entry + DOM markers joining this creative back to the
    // server-side auction (matches the `auction delivered creative:` log line).
    const record = recordRender({
      ...trace,
      rendered: true,
      elementId: container.id || undefined,
    });
    stampCreativeTrace(container, record);
    stampCreativeTrace(iframe, record);

    log.info('renderCreativeInline: rendered', {
      slotId,
      seat,
      creativeId,
      auctionId,
      admHash,
      width,
      height,
      originalLength: sanitization.originalLength,
    });
  } catch (err) {
    log.warn('renderCreativeInline: failed', { slotId, seat, creativeId, err });
  }
}
