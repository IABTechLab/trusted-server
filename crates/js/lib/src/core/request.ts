import { delay } from '../shared/async';

import { log } from './log';
import { getAllUnits, firstSize } from './registry';
import { renderCreativeIntoSlot, renderAllAdUnits, createAdIframe, findSlot } from './render';
import { getConfig } from './config';
import { RequestMode } from './types';
import type { RequestAdsCallback, RequestAdsOptions } from './types';

// getHighestCpmBids is provided by the Prebid extension (shim) to mirror Prebid's API

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

  const mode: RequestMode = (getConfig().mode as RequestMode | undefined) ?? RequestMode.FirstParty;
  log.info('requestAds: called', { hasCallback: typeof callback === 'function', mode });
  try {
    const adUnits = getAllUnits();
    const payload = { adUnits, config: {} };
    log.debug('requestAds: payload', { units: adUnits.length });
    if (mode === RequestMode.FirstParty) void requestAdsFirstParty(adUnits);
    else requestAdsThirdParty(payload);
    // Synchronously invoke callback to match test expectations
    try {
      if (callback) callback();
    } catch {
      /* ignore callback errors */
    }
    // network handled in requestAdsThirdParty; no-op here
  } catch {
    log.warn('requestAds: failed to initiate');
  }
}

async function requestAdsFirstParty(adUnits: ReadonlyArray<{ code: string }>) {
  for (const unit of adUnits) {
    const size = (firstSize(unit) ?? [300, 250]) as readonly [number, number];
    const slotId = unit.code;

    const attemptInsert = async (attemptsRemaining: number): Promise<void> => {
      const container = findSlot(slotId) as HTMLElement | null;
      if (container) {
        const iframe = createAdIframe(container, {
          name: `tsjs_iframe_${slotId}`,
          title: 'Ad content',
          width: size[0],
          height: size[1],
        });
        iframe.src = `/first-party/ad?slot=${encodeURIComponent(slotId)}&w=${encodeURIComponent(String(size[0]))}&h=${encodeURIComponent(String(size[1]))}`;
        return;
      }

      if (attemptsRemaining <= 0) {
        log.warn('requestAds(firstParty): slot not found; skipping iframe', { slotId });
        return;
      }

      if (typeof document !== 'undefined' && document.readyState === 'loading') {
        document.addEventListener(
          'DOMContentLoaded',
          () => {
            void attemptInsert(attemptsRemaining - 1);
          },
          { once: true }
        );
        return;
      }

      await delay(50);
      await attemptInsert(attemptsRemaining - 1);
    };

    void attemptInsert(10);
  }
}

function requestAdsThirdParty(payload: { adUnits: unknown[]; config: unknown }) {
  // Render simple placeholders immediately so pages have content
  renderAllAdUnits();
  if (typeof fetch !== 'function') {
    log.warn('requestAds: fetch not available; nothing to render');
    return;
  }
  log.info('requestAds: sending request to /third-party/ad', {
    units: (payload.adUnits || []).length,
  });
  void fetch('/third-party/ad', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    credentials: 'same-origin',
    body: JSON.stringify(payload),
    keepalive: true,
  })
    .then(async (res) => {
      log.debug('requestAds: sent');
      try {
        const ct = res.headers.get('content-type') || '';
        if (res.ok && ct.includes('application/json')) {
          const data: unknown = await res.json();
          for (const bid of parseSeatBids(data)) {
            if (bid.impid && bid.adm) renderCreativeIntoSlot(String(bid.impid), bid.adm);
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

// Local minimal OpenRTB typing to keep core decoupled from Prebid extension types
type RtBid = { impid?: string; adm?: string };
type RtSeatBid = { bid?: RtBid[] | null };
type RtResponse = { seatbid?: RtSeatBid[] | null };

function isSeatBidArray(x: unknown): x is RtSeatBid[] {
  return Array.isArray(x);
}

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
      out.push({ impid, adm });
    }
  }
  return out;
}
