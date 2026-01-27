// Public tsjs core bundle: sets up the global API.
export type { TsjsApi } from './types';
import type { TsjsApi } from './types';
import { log } from './log';
import { setConfig, getConfig } from './config';

const VERSION = '0.1.0';

const w: Window & { tsjs?: TsjsApi } =
  ((globalThis as unknown as { window?: Window }).window as Window & {
    tsjs?: TsjsApi;
  }) || ({} as Window & { tsjs?: TsjsApi });

// Create API and attach methods
const api: TsjsApi = (w.tsjs ??= {} as TsjsApi);
api.version = VERSION;
api.log = log;
api.setConfig = setConfig;
api.getConfig = getConfig;
w.tsjs = api;

log.info('tsjs initialized', { version: VERSION });
