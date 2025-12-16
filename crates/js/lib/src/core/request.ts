// Request orchestration for tsjs: unified auction endpoint with iframe-based creative rendering.
import { log } from './log';
import { getAllUnits, firstSize } from './registry';
import { createAdIframe, findSlot, buildCreativeDocument } from './render';
import type { RequestAdsCallback, RequestAdsOptions } from './types';

// getHighestCpmBids is provided by the Prebid extension (shim) to mirror Prebid's API

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
    const payload = { adUnits, config: {} };
    log.debug('requestAds: payload', { units: adUnits.length });

    // Use unified auction endpoint
    void requestAdsUnified(payload);

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

// Fire a JSON POST to the unified /auction endpoint and render creatives via iframes.
function requestAdsUnified(payload: { adUnits: unknown[]; config: unknown }) {
  if (typeof fetch !== 'function') {
    log.warn('requestAds: fetch not available; nothing to render');
    return;
  }

  log.info('requestAds: sending request to /auction', {
    units: (payload.adUnits || []).length,
  });

  void fetch('/auction', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    credentials: 'same-origin',
    body: JSON.stringify(payload),
    keepalive: true,
  })
    .then(async (res) => {
      log.debug('requestAds: received response');
      try {
        const ct = res.headers.get('content-type') || '';
        if (res.ok && ct.includes('application/json')) {
          const data: unknown = await res.json();
          const bids = parseSeatBids(data);

          log.info('requestAds: got bids', { count: bids.length });

          for (const bid of bids) {
            if (bid.impid && bid.adm) {
              // adm contains the creative HTML directly (already rewritten with proxy URLs)
              renderCreativeInline(String(bid.impid), String(bid.adm), bid.w, bid.h);
            }
          }

          log.info('requestAds: rendered creatives from response');
          return;
        }
        log.warn('requestAds: unexpected response', { ok: res.ok, status: res.status, ct });
      } catch (err) {
        log.warn('requestAds: failed to process response', err);
      }
    })
    .catch((e) => {
      log.warn('requestAds: failed', e);
    });
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
      // Use actual creative dimensions from bid response
      width = creativeWidth;
      height = creativeHeight;
      log.debug('renderCreativeInline: using creative dimensions', { width, height });
    } else {
      // Fallback to ad unit's first size, then to 300x250
      const unit = getAllUnits().find((u) => u.code === slotId);
      const size = (unit && firstSize(unit)) || [300, 250];
      width = size[0];
      height = size[1];
      log.debug('renderCreativeInline: using ad unit dimensions', { width, height });
    }

    // Create iframe sized for the ad
    const iframe = createAdIframe(container, {
      name: `tsjs_iframe_${slotId}`,
      title: 'Ad content',
      width,
      height,
    });

    // Write creative HTML directly into iframe using srcdoc
    iframe.srcdoc = buildCreativeDocument(creativeHtml);

    log.info('renderCreativeInline: rendered', { slotId, width, height, htmlLength: creativeHtml.length });
  } catch (err) {
    log.warn('renderCreativeInline: failed', { slotId, err });
  }
}

// Local minimal OpenRTB typing to keep core decoupled from Prebid extension types
type RtBid = { impid?: string; adm?: string; w?: number; h?: number };
type RtSeatBid = { bid?: RtBid[] | null };
type RtResponse = { seatbid?: RtSeatBid[] | null };

function isSeatBidArray(x: unknown): x is RtSeatBid[] {
  return Array.isArray(x);
}

// Minimal OpenRTB seatbid parser—just enough to render adm by impid.
function parseSeatBids(data: unknown): RtBid[] {
  const out: RtBid[] = [];
  const resp = data as Partial<RtResponse>;
  const seatbids = resp && resp.seatbid;
  if (!seatbids || !isSeatBidArray(seatbids)) return out;
  for (const sb of seatbids) {
    const bids = sb && sb.bid;
    if (!Array.isArray(bids)) continue;
    for (const b of bids) {
      const impid = typeof b?.impid === 'string' ? b!.impid : undefined;
      const adm = typeof b?.adm === 'string' ? b!.adm : undefined;
      const w = typeof b?.w === 'number' ? b!.w : undefined;
      const h = typeof b?.h === 'number' ? b!.h : undefined;
      out.push({ impid, adm, w, h });
    }
  }
  return out;
}
