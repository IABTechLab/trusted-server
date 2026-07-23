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
    delete (window as TestWindow).googletag;
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

  it('skips adInit on an empty page-bids response with no prior TS state', async () => {
    // A gated page-bids response (auction kill switch or consent denial) returns
    // no slots. With no prior TS state to sweep, the hook must not call adInit()
    // so a consent-denied navigation cannot activate the publisher's GPT setup.
    fetchStub.mockResolvedValue({
      ok: true,
      json: async () => ({ slots: [], bids: {} }),
    });
    const { installSpaAuctionHook } = await importGptModule();
    installSpaAuctionHook();
    const ts = (window as TestWindow).tsjs!;
    const adInit = vi.fn();
    ts.adInit = adInit;

    history.pushState({}, '', '/gated-route');
    await flushAsync();

    expect(ts.adSlots).toEqual([]);
    expect(ts.bids).toEqual({});
    expect(adInit).not.toHaveBeenCalled();
  });

  it('runs adInit on an empty page-bids response when prior TS state exists', async () => {
    // When TS touched slots on a previous navigation, an empty response still
    // needs adInit() to sweep the stale TS targeting from those slots.
    fetchStub.mockResolvedValue({
      ok: true,
      json: async () => ({ slots: [], bids: {} }),
    });
    const { installSpaAuctionHook } = await importGptModule();
    installSpaAuctionHook();
    const ts = (window as TestWindow).tsjs!;
    ts.prevSlotTargetingKeys = { 'div-prev': ['hb_pb'] };
    const adInit = vi.fn();
    ts.adInit = adInit;

    history.pushState({}, '', '/cleanup-route');
    await flushAsync();

    expect(ts.adSlots).toEqual([]);
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

  it('stops orphan recovery before a fast route DOM swap can replay old bids', async () => {
    document.body.innerHTML = '<div id="ad-header-0-_R_old_"></div>';
    const definedDivs: string[] = [];
    const mockPubads = {
      enableSingleRequest: vi.fn(),
      getSlots: vi.fn().mockReturnValue([]),
      refresh: vi.fn(),
      addEventListener: vi.fn(),
    };
    (window as TestWindow).googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: vi.fn((_path: string, _sizes: unknown, divId: string) => {
        definedDivs.push(divId);
        return {
          addService: vi.fn().mockReturnThis(),
          setTargeting: vi.fn().mockReturnThis(),
          clearTargeting: vi.fn().mockReturnThis(),
          getSlotElementId: vi.fn().mockReturnValue(divId),
          getTargeting: vi.fn().mockReturnValue([]),
        };
      }),
      pubads: vi.fn().mockReturnValue(mockPubads),
      enableServices: vi.fn(),
      display: vi.fn(),
      destroySlots: vi.fn(),
    };
    // Keep page-bids slower than the orphan observer's 250 ms debounce.
    fetchStub.mockReturnValue(new Promise(() => {}));

    await importGptModule();
    const ts = (window as TestWindow).tsjs!;
    ts.adSlots = [
      {
        id: 'ad-header-0',
        gam_unit_path: '/123/header',
        div_id: 'ad-header-0',
        formats: [[728, 90]],
        targeting: {},
      },
    ];
    ts.bids = { 'ad-header-0': { hb_adid: 'old-route-ad' } };
    ts.adInit!();
    expect(definedDivs).toEqual(['ad-header-0-_R_old_']);

    history.pushState({}, '', '/new-route');
    document.body.innerHTML = '<div id="ad-header-0-_R_new_"></div>';
    await new Promise((resolve) => setTimeout(resolve, 350));

    // The pending old-route watcher was disconnected synchronously when
    // navigation began, so it never rebound or re-requested the old auction.
    expect(definedDivs).toEqual(['ad-header-0-_R_old_']);
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

  it('retries the same path after a failed page-bids fetch (currentPath rollback)', async () => {
    // A failed load must roll `currentPath` back so re-navigating to the SAME
    // path retries instead of being swallowed by the no-op guard at the top of
    // onNavigate. Without the rollback, currentPath would already equal the
    // failed path and the second navigation would return early.
    document.body.innerHTML = '<div id="div-s1"></div>';
    fetchStub.mockResolvedValueOnce({ ok: false, status: 500 }).mockResolvedValueOnce({
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

    // First navigation to the path fails; nothing is applied.
    history.pushState({}, '', '/retry-page');
    await flushAsync();
    expect(ts.adSlots).toBeUndefined();

    // Re-navigate to the same path — the retry must re-fetch and apply.
    history.pushState({}, '', '/retry-page');
    await flushAsync();

    expect(fetchStub).toHaveBeenCalledTimes(2);
    expect(ts.adSlots).toEqual([{ id: 's1', div_id: 'div-s1' }]);
    expect(adInit).toHaveBeenCalledTimes(1);
  });

  it('does not strand a path that was aborted mid-flight then failed on the next nav', async () => {
    // Rapid A→B where A is aborted mid-flight and B then fails must roll
    // `currentPath` back to the last *applied* path (here the initial route),
    // not to A. Rolling back to A — which never loaded — would leave it behind
    // the no-op guard so a later real navigation to A never re-fetches.
    document.body.innerHTML = '<div id="div-a"></div>';
    let resolveA: ((value: unknown) => void) | undefined;
    fetchStub
      // A: still in flight when B starts (aborted, never settles on its own).
      .mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            resolveA = resolve;
          })
      )
      // B: fails.
      .mockResolvedValueOnce({ ok: false, status: 500 })
      // A retried: succeeds.
      .mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          slots: [{ id: 'a', div_id: 'div-a' }],
          bids: { a: { hb_pb: '1.00' } },
        }),
      });
    const { installSpaAuctionHook } = await importGptModule();
    installSpaAuctionHook();
    const ts = (window as TestWindow).tsjs!;
    const adInit = vi.fn();
    ts.adInit = adInit;

    // A starts (left in flight), then B aborts A and fails.
    history.pushState({}, '', '/a');
    history.pushState({}, '', '/b');
    await flushAsync();
    expect(ts.adSlots).toBeUndefined();

    // Navigate back to /a. With the rollback keyed to the last applied path
    // (the initial route) instead of B's previous path (/a), this is NOT
    // swallowed by the no-op guard and re-fetches.
    history.pushState({}, '', '/a');
    await flushAsync();

    expect(fetchStub).toHaveBeenCalledTimes(3);
    expect(ts.adSlots).toEqual([{ id: 'a', div_id: 'div-a' }]);
    expect(adInit).toHaveBeenCalledTimes(1);

    // The original aborted A fetch resolving late must not clobber the retry.
    resolveA?.({ ok: true, json: async () => ({ slots: [{ id: 'stale' }], bids: {} }) });
    await flushAsync();
    expect(ts.adSlots).toEqual([{ id: 'a', div_id: 'div-a' }]);
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
