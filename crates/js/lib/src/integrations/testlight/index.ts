import type { TsjsApi } from '../../core/types';
import { installQueue } from '../../core/queue';
import { log } from '../../core/log';
import { resolvePrebidWindow, PrebidWindow } from '../../shared/globals';

type TestlightCallback = () => void;

type TestlightGlobal = {
  que?: TestlightCallback[];
};

type TestlightWindow = PrebidWindow & {
  testlight?: TestlightGlobal;
};

function ensureTsjsApi(win: TestlightWindow): TsjsApi {
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

function installTestlightQueue(api: TsjsApi, win: TestlightWindow): void {
  if (!Array.isArray(api.que)) {
    installQueue(api, win);
  }
}

function flushCallbacks(queue: TestlightCallback[], api: TsjsApi): void {
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
      log.debug('testlight shim: flushed callback');
    } catch (err) {
      log.debug('testlight shim: queued callback threw', err);
    }
  }
}

export function installTestlightShim(): boolean {
  const win = resolvePrebidWindow() as TestlightWindow;
  const api = ensureTsjsApi(win);
  installTestlightQueue(api, win);

  const testlight = (win.testlight = win.testlight ?? {});
  const pending: TestlightCallback[] = Array.isArray(testlight.que) ? [...testlight.que] : [];
  const queue: TestlightCallback[] = [];
  testlight.que = queue;

  const originalPush = queue.push.bind(queue);
  queue.push = function (...callbacks: TestlightCallback[]): number {
    const len = originalPush(...callbacks);
    flushCallbacks(queue, api);
    return len;
  };

  if (pending.length > 0) {
    queue.push(...pending);
  }

  log.info('testlight shim installed', { queuedCallbacks: queue.length });
  return true;
}

if (typeof window !== 'undefined') {
  installTestlightShim();
}
