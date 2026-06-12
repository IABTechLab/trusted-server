import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';

import type { TsjsApi } from '../../../src/core/types';

type TestWindow = Window & {
  googletag?: unknown;
  tsjs?: TsjsApi;
};

const originalPushState = history.pushState.bind(history);
const originalReplaceState = history.replaceState.bind(history);

async function importGptModule() {
  return import('../../../src/integrations/gpt/index');
}

/** Flush the microtask/timer queue so onNavigate's awaits settle. */
async function flushAsync(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 0));
}

describe('installSpaAuctionHook', () => {
  let fetchStub: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    vi.resetModules();
    delete (window as TestWindow).tsjs;
    // Restore unwrapped history methods so each module import wraps exactly
    // once — without this, wrappers from prior imports accumulate.
    history.pushState = originalPushState;
    history.replaceState = originalReplaceState;
    fetchStub = vi.fn();
    vi.stubGlobal('fetch', fetchStub);
  });

  afterEach(() => {
    history.pushState = originalPushState;
    history.replaceState = originalReplaceState;
    // Reset jsdom location back to root for the next test.
    originalReplaceState({}, '', '/');
    vi.unstubAllGlobals();
  });

  it('fetches page-bids on pushState and applies slots/bids via adInit', async () => {
    fetchStub.mockResolvedValue({
      ok: true,
      json: async () => ({ slots: [{ id: 's1' }], bids: { s1: { hb_pb: '1.00' } } }),
    });
    const { installSpaAuctionHook } = await importGptModule();
    installSpaAuctionHook();
    const ts = (window as TestWindow).tsjs!;
    const adInit = vi.fn();
    ts.adInit = adInit;

    history.pushState({}, '', '/next-page');
    await flushAsync();

    expect(fetchStub).toHaveBeenCalledWith(
      '/__ts/page-bids?path=%2Fnext-page',
      expect.objectContaining({
        credentials: 'include',
        headers: { 'X-TSJS-Page-Bids': '1' },
      })
    );
    expect(ts.adSlots).toEqual([{ id: 's1' }]);
    expect(ts.bids).toEqual({ s1: { hb_pb: '1.00' } });
    expect(adInit).toHaveBeenCalledTimes(1);
  });

  it('does not fetch when pushState targets the current path', async () => {
    await importGptModule();

    history.pushState({}, '', '/');
    await flushAsync();

    expect(fetchStub).not.toHaveBeenCalled();
  });

  it('fetches on replaceState and popstate navigation', async () => {
    fetchStub.mockResolvedValue({
      ok: true,
      json: async () => ({ slots: [], bids: {} }),
    });
    await importGptModule();

    history.replaceState({}, '', '/replaced');
    await flushAsync();
    expect(fetchStub).toHaveBeenCalledWith(
      '/__ts/page-bids?path=%2Freplaced',
      expect.objectContaining({ credentials: 'include' })
    );

    window.dispatchEvent(new PopStateEvent('popstate'));
    await flushAsync();
    expect(fetchStub).toHaveBeenLastCalledWith(
      '/__ts/page-bids?path=%2Freplaced',
      expect.objectContaining({ credentials: 'include' })
    );
  });

  it('drops a stale response that resolves after a newer navigation started', async () => {
    let resolveFirst: ((value: unknown) => void) | undefined;
    fetchStub
      .mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            resolveFirst = resolve;
          })
      )
      .mockResolvedValueOnce({
        ok: true,
        json: async () => ({ slots: [{ id: 'newer' }], bids: {} }),
      });
    const { installSpaAuctionHook } = await importGptModule();
    installSpaAuctionHook();
    const ts = (window as TestWindow).tsjs!;
    const adInit = vi.fn();
    ts.adInit = adInit;

    history.pushState({}, '', '/first');
    history.pushState({}, '', '/second');
    await flushAsync();

    expect(ts.adSlots).toEqual([{ id: 'newer' }]);
    expect(adInit).toHaveBeenCalledTimes(1);

    // First navigation's response arrives late — it must not overwrite the
    // newer route's slots or trigger another adInit.
    resolveFirst!({
      ok: true,
      json: async () => ({ slots: [{ id: 'stale' }], bids: {} }),
    });
    await flushAsync();

    expect(ts.adSlots).toEqual([{ id: 'newer' }]);
    expect(adInit).toHaveBeenCalledTimes(1);
  });

  it('leaves slots and bids untouched on a non-OK response', async () => {
    fetchStub.mockResolvedValue({ ok: false, status: 500 });
    const { installSpaAuctionHook } = await importGptModule();
    installSpaAuctionHook();
    const ts = (window as TestWindow).tsjs!;
    ts.adSlots = [{ id: 'existing' } as never];
    const adInit = vi.fn();
    ts.adInit = adInit;

    history.pushState({}, '', '/error-page');
    await flushAsync();

    expect(ts.adSlots).toEqual([{ id: 'existing' }]);
    expect(adInit).not.toHaveBeenCalled();
  });

  it('is idempotent — repeated install calls do not double-fetch a navigation', async () => {
    fetchStub.mockResolvedValue({
      ok: true,
      json: async () => ({ slots: [], bids: {} }),
    });
    const { installSpaAuctionHook } = await importGptModule();
    // Module init already installed the hook; both calls must be no-ops.
    installSpaAuctionHook();
    installSpaAuctionHook();

    history.pushState({}, '', '/once');
    await flushAsync();

    expect(fetchStub).toHaveBeenCalledTimes(1);
  });
});
