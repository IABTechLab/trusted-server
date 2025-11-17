import type { TsjsApi } from '../core/types';
import { installQueue } from '../core/queue';
import { log } from '../core/log';
import { resolvePrebidWindow, PrebidWindow } from '../shared/globals';

type StarlightCallback = () => void;

type StarlightGlobal = {
  que?: StarlightCallback[];
};

type StarlightWindow = PrebidWindow & {
  starlight?: StarlightGlobal;
};

function ensureTsjsApi(win: StarlightWindow): TsjsApi {
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

function installStarlightQueue(api: TsjsApi, win: StarlightWindow): void {
  if (!Array.isArray(api.que)) {
    installQueue(api, win);
  }
}

function flushCallbacks(queue: StarlightCallback[], api: TsjsApi): void {
  while (queue.length > 0) {
    const fn = queue.shift();
    if (typeof fn !== 'function') {
      continue;
    }
    try {
      if (Array.isArray(api.que)) {
        api.que.push(fn);
      } else {
        fn.call(api);
      }
      log.debug('starlight shim: flushed callback');
    } catch (err) {
      log.debug('starlight shim: queued callback threw', err);
    }
  }
}

export function installStarlightShim(): boolean {
  const win = resolvePrebidWindow() as StarlightWindow;
  const api = ensureTsjsApi(win);
  installStarlightQueue(api, win);

  const starlight = (win.starlight = win.starlight ?? {});
  const pending: StarlightCallback[] = Array.isArray(starlight.que) ? [...starlight.que] : [];
  const queue: StarlightCallback[] = [];
  starlight.que = queue;

  const originalPush = queue.push.bind(queue);
  queue.push = function (...callbacks: StarlightCallback[]): number {
    const len = originalPush(...callbacks);
    flushCallbacks(queue, api);
    return len;
  };

  if (pending.length > 0) {
    queue.push(...pending);
  }

  log.info('starlight shim installed', { queuedCallbacks: queue.length });
  return true;
}

if (typeof window !== 'undefined') {
  installStarlightShim();
}
