import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';

import installPrebidJsShim from '../../../src/integrations/ext/prebidjs';

const ORIGINAL_WINDOW = global.window;

function createWindow() {
  return {
    tsjs: undefined as any,
    pbjs: undefined as any,
  } as Window & { tsjs?: any; pbjs?: any };
}

describe('ext/prebidjs', () => {
  let testWindow: ReturnType<typeof createWindow>;

  beforeEach(() => {
    testWindow = createWindow();
    Object.assign(globalThis as any, { window: testWindow });
  });

  afterEach(() => {
    Object.assign(globalThis as any, { window: ORIGINAL_WINDOW });
    vi.restoreAllMocks();
  });

  it('installs shim, aliases pbjs to tsjs, and shares queue', () => {
    const result = installPrebidJsShim();
    expect(result).toBe(true);
    expect(testWindow.pbjs).toBe(testWindow.tsjs);
    expect(Array.isArray(testWindow.tsjs!.que)).toBe(true);
  });

  it('flushes queued pbjs callbacks', () => {
    const callback = vi.fn();
    testWindow.pbjs = { que: [callback] } as any;

    installPrebidJsShim();

    expect(callback).toHaveBeenCalled();
    expect(testWindow.pbjs).toBe(testWindow.tsjs);
  });

  it('ensures shared queue and requestBids shim delegates to requestAds', () => {
    installPrebidJsShim();

    const api = testWindow.tsjs!;
    api.requestAds = vi.fn();
    const requestBids = testWindow.pbjs!.requestBids.bind(testWindow.pbjs);

    const callback = vi.fn();
    requestBids(callback);
    expect(api.requestAds).toHaveBeenCalledWith(callback, undefined);

    requestBids({ timeout: 100 } as any);
    expect(api.requestAds).toHaveBeenCalledWith({ timeout: 100 });
  });
});
