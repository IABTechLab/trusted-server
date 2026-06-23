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
  // popstate listeners registered by each module import. In production the hook
  // installs once (guarded by `ts.spaHookInstalled`), but tests wipe
  // `window.tsjs` and re-import per test, so without explicit removal the
  // listeners accumulate on the shared window and all fire on every dispatch.
  let popstateHandlers: EventListenerOrEventListenerObject[] = [];
  const realAddEventListener = window.addEventListener.bind(window);

  beforeEach(() => {
    vi.resetModules();
    delete (window as TestWindow).tsjs;
    // Restore unwrapped history methods so each module import wraps exactly
    // once — without this, wrappers from prior imports accumulate.
    history.pushState = originalPushState;
    history.replaceState = originalReplaceState;
    fetchStub = vi.fn();
    vi.stubGlobal('fetch', fetchStub);
    popstateHandlers = [];
    vi.spyOn(window, 'addEventListener').mockImplementation((type, listener, options) => {
      if (type === 'popstate' && listener) popstateHandlers.push(listener);
      return realAddEventListener(type, listener, options);
    });
  });

  afterEach(() => {
    history.pushState = originalPushState;
    history.replaceState = originalReplaceState;
    // Reset jsdom location back to root for the next test.
    originalReplaceState({}, '', '/');
    // Drop any ad containers inserted by a test so DOM state does not leak.
    document.body.innerHTML = '';
    // Remove this test's popstate listener(s) so they do not fire in later tests.
    popstateHandlers.forEach((handler) => window.removeEventListener('popstate', handler));
    popstateHandlers = [];
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it('fetches page-bids on pushState and applies slots/bids via adInit', async () => {
    // The route's ad container already exists, so bids apply immediately.
    document.body.innerHTML = '<div id="div-s1"></div>';
    fetchStub.mockResolvedValue({
      ok: true,
      json: async () => ({
        slots: [{ id: 's1', div_id: 'div-s1' }],
        bids: { s1: { hb_pb: '1.00' } },
      }),
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
    expect(ts.adSlots).toEqual([{ id: 's1', div_id: 'div-s1' }]);
    expect(ts.bids).toEqual({ s1: { hb_pb: '1.00' } });
    expect(adInit).toHaveBeenCalledTimes(1);
  });

  it('defers applying bids until the route ad container is inserted', async () => {
    fetchStub.mockResolvedValue({
      ok: true,
      json: async () => ({
        slots: [{ id: 'late', div_id: 'div-late' }],
        bids: { late: { hb_pb: '2.00' } },
      }),
    });
    const { installSpaAuctionHook } = await importGptModule();
    installSpaAuctionHook();
    const ts = (window as TestWindow).tsjs!;
    const adInit = vi.fn();
    ts.adInit = adInit;

    // Navigate before the new route's container has rendered.
    history.pushState({}, '', '/late-route');
    await flushAsync();
    expect(adInit).not.toHaveBeenCalled();
    expect(ts.adSlots).toBeUndefined();

    // Container commits — the hook should now apply bids exactly once.
    document.body.innerHTML = '<div id="div-late"></div>';
    await flushAsync();

    expect(ts.adSlots).toEqual([{ id: 'late', div_id: 'div-late' }]);
    expect(ts.bids).toEqual({ late: { hb_pb: '2.00' } });
    expect(adInit).toHaveBeenCalledTimes(1);
  });

  it('waits for every configured route ad container before applying bids', async () => {
    document.body.innerHTML = '<div id="div-first"></div>';
    fetchStub.mockResolvedValue({
      ok: true,
      json: async () => ({
        slots: [
          { id: 'first', div_id: 'div-first' },
          { id: 'second', div_id: 'div-second' },
        ],
        bids: {
          first: { hb_pb: '1.00' },
          second: { hb_pb: '2.00' },
        },
      }),
    });
    const { installSpaAuctionHook } = await importGptModule();
    installSpaAuctionHook();
    const ts = (window as TestWindow).tsjs!;
    const adInit = vi.fn();
    ts.adInit = adInit;

    history.pushState({}, '', '/multi-slot-route');
    await flushAsync();

    expect(adInit).not.toHaveBeenCalled();
    expect(ts.adSlots).toBeUndefined();

    const second = document.createElement('div');
    second.id = 'div-second';
    document.body.appendChild(second);
    await flushAsync();

    expect(ts.adSlots).toEqual([
      { id: 'first', div_id: 'div-first' },
      { id: 'second', div_id: 'div-second' },
    ]);
    expect(ts.bids).toEqual({
      first: { hb_pb: '1.00' },
      second: { hb_pb: '2.00' },
    });
    expect(adInit).toHaveBeenCalledTimes(1);
  });

  it('does not fetch when pushState targets the current path', async () => {
    await importGptModule();

    history.pushState({}, '', '/');
    await flushAsync();

    expect(fetchStub).not.toHaveBeenCalled();
  });

  it('fetches on replaceState navigation', async () => {
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
  });

  it('fetches on popstate navigation to a new path', async () => {
    fetchStub.mockResolvedValue({
      ok: true,
      json: async () => ({ slots: [], bids: {} }),
    });
    await importGptModule();

    // Browsers change the URL out-of-band on back/forward, then fire popstate.
    // Use the unwrapped history method so the patched handler is not invoked.
    originalReplaceState({}, '', '/popped');
    window.dispatchEvent(new PopStateEvent('popstate'));
    await flushAsync();
    expect(fetchStub).toHaveBeenCalledWith(
      '/__ts/page-bids?path=%2Fpopped',
      expect.objectContaining({ credentials: 'include' })
    );
  });

  it('does not re-fetch on popstate to the same path', async () => {
    fetchStub.mockResolvedValue({
      ok: true,
      json: async () => ({ slots: [], bids: {} }),
    });
    await importGptModule();

    history.replaceState({}, '', '/replaced');
    await flushAsync();
    expect(fetchStub).toHaveBeenCalledTimes(1);

    // popstate on the same path (hash-only change or scroll-restoration
    // back/forward) must not re-request impressions.
    window.dispatchEvent(new PopStateEvent('popstate'));
    await flushAsync();
    expect(fetchStub).toHaveBeenCalledTimes(1);
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
        json: async () => ({ slots: [{ id: 'newer', div_id: 'div-newer' }], bids: {} }),
      });
    // Container for the newer route exists so its bids apply without waiting.
    document.body.innerHTML = '<div id="div-newer"></div>';
    const { installSpaAuctionHook } = await importGptModule();
    installSpaAuctionHook();
    const ts = (window as TestWindow).tsjs!;
    const adInit = vi.fn();
    ts.adInit = adInit;

    history.pushState({}, '', '/first');
    history.pushState({}, '', '/second');
    await flushAsync();

    expect(ts.adSlots).toEqual([{ id: 'newer', div_id: 'div-newer' }]);
    expect(adInit).toHaveBeenCalledTimes(1);

    // First navigation's response arrives late — it must not overwrite the
    // newer route's slots or trigger another adInit.
    resolveFirst!({
      ok: true,
      json: async () => ({ slots: [{ id: 'stale' }], bids: {} }),
    });
    await flushAsync();

    expect(ts.adSlots).toEqual([{ id: 'newer', div_id: 'div-newer' }]);
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
