import type { TsjsApi } from '../core/types';
import { log } from '../core/log';
import { installQueue } from '../core/queue';
import { getAllCodes, getAllUnits, firstSize } from '../core/registry';
import type { HighestCpmBid } from '../core/types';

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
function requestBidsShim(api: TsjsApi) {
  return (...args: any[]) => (api as any).requestAds?.(...args);
}

export function installPrebidJsShim(): boolean {
  const w: Window & { tsjs?: TsjsApi; pbjs?: TsjsApi } =
    ((globalThis as unknown as { window?: Window }).window as Window & {
      tsjs?: TsjsApi;
      pbjs?: TsjsApi;
    }) || ({} as Window & { tsjs?: TsjsApi; pbjs?: TsjsApi });

  // Ensure core exists
  const api: TsjsApi = (w.tsjs ??= { version: '0.0.0', que: [] } as TsjsApi);

  // Capture any queued pbjs callbacks before aliasing
  const pending: Array<() => void> = Array.isArray(w.pbjs?.que) ? [...(w.pbjs as TsjsApi).que] : [];

  // Core provides requestAds/getHighestCpmBids; extension aliases pbjs and shims requestBids → requestAds

  // Alias pbjs to tsjs and ensure a single shared queue
  w.pbjs = api;
  if (!Array.isArray(api.que)) {
    installQueue(api, w);
  }
  // Make sure both globals share the same queue
  if (Array.isArray(api.que)) {
    (w.pbjs as TsjsApi).que = api.que;
  }
  // Shim Prebid-style API surface
  try {
    (w.pbjs as any).requestBids = requestBidsShim(api);
    (w.pbjs as any).getHighestCpmBids = getHighestCpmBidsShim;
  } catch {}

  // Flush previously queued pbjs callbacks
  for (const fn of pending) {
    try {
      if (typeof fn === 'function') {
        fn.call(api);
        log.debug('prebidjs extension: flushed callback');
      }
    } catch {
      /* ignore queued callback error */
    }
  }

  log.info('prebidjs extension installed', {
    hasRequestBids: typeof (w.pbjs as any).requestBids === 'function',
    hasGetHighestCpmBids: typeof (w.pbjs as any).getHighestCpmBids === 'function',
  });

  return true;
}

export default installPrebidJsShim;
