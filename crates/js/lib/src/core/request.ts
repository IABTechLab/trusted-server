// Request orchestration for tsjs: unified auction endpoint with iframe-based creative rendering.
import { log } from './log';
import { getAllUnits, firstSize } from './registry';
import { createAdIframe, findSlot, buildCreativeDocument } from './render';
import { buildAdRequest, sendAuction } from './auction';

export type RequestAdsCallback = () => void;
export interface RequestAdsOptions {
  bidsBackHandler?: RequestAdsCallback;
  timeout?: number;
}

/**
 * Read Permutive segment IDs from localStorage.
 *
 * Permutive stores event data in the `permutive-app` key. Each event entry in
 * `eventPublication.eventUpload` is a tuple of [eventKey, eventObject]. We
 * iterate in reverse (most recent first) looking for any event whose
 * `properties.segments` is a non-empty array.
 *
 * Returns an array of segment ID numbers, or an empty array if unavailable.
 */
function getPermutiveSegments(): number[] {
  try {
    const raw = localStorage.getItem('permutive-app');
    if (!raw) return [];

    const data = JSON.parse(raw);
    const uploads: unknown[] = data?.eventPublication?.eventUpload;
    if (!Array.isArray(uploads) || uploads.length === 0) return [];

    // Iterate most-recent-first to get the freshest segments
    for (let i = uploads.length - 1; i >= 0; i--) {
      const entry = uploads[i];
      if (!Array.isArray(entry) || entry.length < 2) continue;

      const segments = entry[1]?.event?.properties?.segments;
      if (Array.isArray(segments) && segments.length > 0) {
        log.debug('getPermutiveSegments: found segments', { count: segments.length });
        return segments.filter((s: unknown) => typeof s === 'number') as number[];
      }
    }
  } catch {
    log.debug('getPermutiveSegments: failed to read from localStorage');
  }
  return [];
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
    const permutiveSegments = getPermutiveSegments();
    const config: Record<string, unknown> = {};
    if (permutiveSegments.length > 0) {
      config.permutive_segments = permutiveSegments;
    }
    const payload = { ...buildAdRequest(adUnits), config };
    log.debug('requestAds: payload', { units: adUnits.length, permutiveSegments: permutiveSegments.length });

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
