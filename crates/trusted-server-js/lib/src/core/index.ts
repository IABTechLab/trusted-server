// Public tsjs core bundle: sets up the global API, queue, and default methods.
export type { AdUnit, TsjsApi } from './types';
import type { TsjsApi } from './types';
import { addAdUnits } from './registry';
import { renderAdUnit, renderAllAdUnits } from './render';
import { log } from './log';
import { setConfig, getConfig } from './config';
import { requestAds } from './request';
import { installQueue } from './queue';

const VERSION = '0.1.0';

const w: Window & { tsjs?: TsjsApi } =
  ((globalThis as unknown as { window?: Window }).window as Window & {
    tsjs?: TsjsApi;
  }) || ({} as Window & { tsjs?: TsjsApi });

// Collect existing tsjs queued fns before we overwrite
const pending: Array<() => void> = Array.isArray(w.tsjs?.que) ? [...w.tsjs.que] : [];

// Create API and attach methods
const api: TsjsApi = (w.tsjs ??= {} as TsjsApi);
api.version = VERSION;
api.addAdUnits = addAdUnits;
api.renderAdUnit = renderAdUnit;
api.renderAllAdUnits = () => renderAllAdUnits();
api.log = log;
api.setConfig = setConfig;
api.getConfig = getConfig;
// Provide core requestAds API
api.requestAds = requestAds;
// Point global tsjs
w.tsjs = api;

// Single shared queue
installQueue(api, w);

// Flush prior queued callbacks
for (const fn of pending) {
  try {
    if (typeof fn === 'function') {
      fn.call(api);
      log.debug('queue: flushed callback');
    }
  } catch {
    /* ignore queued callback error */
  }
}

log.info('tsjs initialized', {
  methods: [
    'setConfig',
    'getConfig',
    'requestAds',
    'addAdUnits',
    'renderAdUnit',
    'renderAllAdUnits',
  ],
});
