// Request orchestration for tsjs: unified auction endpoint with iframe-based creative rendering.
import { log } from './log';
import { collectContext } from './context';
import { getAllUnits, firstSize } from './registry';
import { createAdIframe, findSlot, buildCreativeDocument } from './render';
import { buildAdRequest, sendAuction } from './auction';

export type RequestAdsCallback = () => void;
export interface RequestAdsOptions {
  bidsBackHandler?: RequestAdsCallback;
  timeout?: number;
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
    const config = collectContext();
    const payload = { ...buildAdRequest(adUnits), config };
    log.debug('requestAds: payload', { units: adUnits.length, contextKeys: Object.keys(config) });

    // Use unified auction endpoint
    void sendAuction('/auction', payload)
      .then((bids) => {
        log.info('requestAds: got bids', { count: bids.length });
        for (const bid of bids) {
          if (bid.impid && bid.adm) {
            renderCreativeInline(bid.impid, bid.adm, bid.width, bid.height);
          }
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

// Render a creative by writing HTML directly into a sandboxed iframe.
function renderCreativeInline(
  slotId: string,
  creativeHtml: string,
  creativeWidth?: number,
  creativeHeight?: number
): void {
  const container = findSlot(slotId) as HTMLElement | null;
  if (!container) {
    log.warn('renderCreativeInline: slot not found; skipping render', { slotId });
    return;
  }

  try {
    // Clear previous content
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

    iframe.srcdoc = buildCreativeDocument(creativeHtml);

    log.info('renderCreativeInline: rendered', {
      slotId,
      width,
      height,
      htmlLength: creativeHtml.length,
    });
  } catch (err) {
    log.warn('renderCreativeInline: failed', { slotId, err });
  }
}
