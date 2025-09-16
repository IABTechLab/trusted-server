import type { TsjsApi, HighestCpmBid, RequestAdsCallback, RequestAdsOptions } from '../core/types';
import { log } from '../core/log';
import { installQueue } from '../core/queue';
import { getAllCodes, getAllUnits, firstSize } from '../core/registry';
import { resolvePrebidWindow, PrebidWindow } from '../shared/globals';
type RequestBidsFunction = (
  callbackOrOpts?: RequestAdsCallback | RequestAdsOptions,
  opts?: RequestAdsOptions
) => void;

/**
 * Shim implementation for pbjs.getHighestCpmBids that returns synthetic
 * placeholder bids derived from the registered core ad units.
 */
function getHighestCpmBidsShim(adUnitCodes?: string | string[]): ReadonlyArray<HighestCpmBid> {
  const codes: string[] =
    typeof adUnitCodes === 'string' ? [adUnitCodes] : (adUnitCodes ?? getAllCodes());
  const results: HighestCpmBid[] = [];
  for (const code of codes) {
    const unit = getAllUnits().find((u) => u.code === code);
    if (!unit) continue;
    const size = (firstSize(unit) ?? [300, 250]) as readonly [number, number];
    results.push({
      adUnitCode: code,
      width: size[0],
      height: size[1],
      cpm: 0,
      currency: 'USD',
      bidderCode: 'tsjs',
      creativeId: 'tsjs-placeholder',
      adserverTargeting: {},
    });
  }
  return results;
}

/**
 * Shim implementation for pbjs.requestBids that forwards to core requestAds.
 */
function requestBidsShim(api: TsjsApi): RequestBidsFunction {
  return (callbackOrOpts?: RequestAdsCallback | RequestAdsOptions, opts?: RequestAdsOptions) => {
    const requestAds = api.requestAds as
      | ((options?: RequestAdsOptions) => void)
      | ((callback: RequestAdsCallback, options?: RequestAdsOptions) => void)
      | undefined;
    if (!requestAds) return;
    if (typeof callbackOrOpts === 'function') {
      requestAds(callbackOrOpts, opts);
    } else {
      requestAds(callbackOrOpts);
    }
  };
}

function ensureTsjsApi(win: PrebidWindow): TsjsApi {
  if (win.tsjs) return win.tsjs;
  const stub: TsjsApi = {
    version: '0.0.0',
    que: [],
    addAdUnits: () => undefined,
    renderAdUnit: () => undefined,
    renderAllAdUnits: () => undefined,
  };
  win.tsjs = stub;
  return stub;
}

export function installPrebidJsShim(): boolean {
  const w = resolvePrebidWindow();

  // Ensure core exists
  const api = ensureTsjsApi(w);

  // Capture any queued pbjs callbacks before aliasing
  const pending: Array<() => void> = Array.isArray(w.pbjs?.que) ? [...(w.pbjs?.que ?? [])] : [];

  // Core provides requestAds/getHighestCpmBids; extension aliases pbjs and shims requestBids → requestAds

  // Alias pbjs to tsjs and ensure a single shared queue
  w.pbjs = api;
  if (!Array.isArray(api.que)) {
    installQueue(api, w);
  }
  const pbjsApi = w.pbjs as TsjsApi & { requestBids?: RequestBidsFunction };
  // Make sure both globals share the same queue
  if (Array.isArray(api.que)) {
    pbjsApi.que = api.que;
  }
  // Shim Prebid-style API surface
  pbjsApi.requestBids = requestBidsShim(api);
  pbjsApi.getHighestCpmBids = getHighestCpmBidsShim;

  // Flush previously queued pbjs callbacks
  for (const fn of pending) {
    try {
      if (typeof fn === 'function') {
        fn.call(api);
        log.debug('prebidjs extension: flushed callback');
      }
    } catch (err) {
      log.debug('prebidjs extension: queued callback failed', err);
    }
  }

  log.info('prebidjs extension installed', {
    hasRequestBids: typeof pbjsApi.requestBids === 'function',
    hasGetHighestCpmBids: typeof pbjsApi.getHighestCpmBids === 'function',
  });

  return true;
}

export default installPrebidJsShim;
