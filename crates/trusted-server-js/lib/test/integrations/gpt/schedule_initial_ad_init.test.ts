import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';

import type { TsjsApi } from '../../../src/core/types';

type TestWindow = Window & {
  googletag?: unknown;
  tsjs?: TsjsApi;
};

const originalPushState = history.pushState.bind(history);
const originalReplaceState = history.replaceState.bind(history);

/**
 * Executable lifecycle coverage for `tsjs.scheduleInitialAdInit` — the
 * deferred initial-adInit bootstrap the server's `</body>` bids script hands
 * off to. These tests run the real scheduler (and, where noted, the real
 * `adInit()` and SPA auction hook) instead of string-matching the emitted
 * script, so post-load ordering, two-frame deferral, exactly-once invocation,
 * and stale-navigation cancellation are all exercised, not just spelled.
 */
describe('scheduleInitialAdInit', () => {
  let rafQueue: FrameRequestCallback[];
  let readyState: DocumentReadyState;
  let fetchStub: ReturnType<typeof vi.fn>;
  let popstateHandlers: EventListenerOrEventListenerObject[] = [];
  const realAddEventListener = window.addEventListener.bind(window);

  /** Run every queued animation-frame callback (one frame's worth). */
  function flushFrame(): void {
    const queued = [...rafQueue];
    rafQueue.length = 0;
    queued.forEach((cb) => cb(0));
  }

  /** Flush the microtask/timer queue so the SPA hook's awaits settle. */
  async function flushAsync(): Promise<void> {
    await new Promise((resolve) => setTimeout(resolve, 0));
  }

  async function importGptModule() {
    return import('../../../src/integrations/gpt/index');
  }

  beforeEach(() => {
    vi.resetModules();
    delete (window as TestWindow).tsjs;
    delete (window as TestWindow).googletag;
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
    // Manual animation-frame queue: the scheduler must be observed frame by
    // frame, so frames only run when a test flushes them explicitly.
    rafQueue = [];
    (
      window as { requestAnimationFrame: typeof window.requestAnimationFrame }
    ).requestAnimationFrame = ((cb: FrameRequestCallback) => {
      rafQueue.push(cb);
      return rafQueue.length;
    }) as typeof window.requestAnimationFrame;
    // Controllable document.readyState (jsdom reports 'complete' by default;
    // the scheduler branches on it).
    readyState = 'loading';
    Object.defineProperty(document, 'readyState', {
      configurable: true,
      get: () => readyState,
    });
  });

  afterEach(() => {
    history.pushState = originalPushState;
    history.replaceState = originalReplaceState;
    // Reset jsdom location back to root for the next test.
    originalReplaceState({}, '', '/');
    document.body.innerHTML = '';
    popstateHandlers.forEach((handler) => window.removeEventListener('popstate', handler));
    popstateHandlers = [];
    // Remove the instance property so the prototype getter is visible again.
    delete (document as unknown as Record<string, unknown>).readyState;
    delete (window as unknown as Record<string, unknown>).requestAnimationFrame;
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it('defers adInit until window load plus two animation frames', async () => {
    await importGptModule();
    const ts = (window as TestWindow).tsjs!;
    const adInit = vi.fn();
    ts.adInit = adInit;

    ts.scheduleInitialAdInit!();
    expect(adInit).not.toHaveBeenCalled();

    // load alone must not run it — React commits after the load-time frame.
    window.dispatchEvent(new Event('load'));
    expect(adInit).not.toHaveBeenCalled();

    // One frame is not enough: the double rAF exists so the call lands after
    // React's post-hydration commit, not inside the load-event frame.
    flushFrame();
    expect(adInit).not.toHaveBeenCalled();

    flushFrame();
    expect(adInit).toHaveBeenCalledTimes(1);
  });

  it('runs after two frames without a load event when the document is already complete', async () => {
    readyState = 'complete';
    await importGptModule();
    const ts = (window as TestWindow).tsjs!;
    const adInit = vi.fn();
    ts.adInit = adInit;

    ts.scheduleInitialAdInit!();
    // Still never synchronous — even past load, adInit waits two frames.
    expect(adInit).not.toHaveBeenCalled();

    flushFrame();
    expect(adInit).not.toHaveBeenCalled();
    flushFrame();
    expect(adInit).toHaveBeenCalledTimes(1);
  });

  it('invokes adInit exactly once even across duplicate load events and extra frames', async () => {
    await importGptModule();
    const ts = (window as TestWindow).tsjs!;
    const adInit = vi.fn();
    ts.adInit = adInit;

    ts.scheduleInitialAdInit!();
    window.dispatchEvent(new Event('load'));
    window.dispatchEvent(new Event('load'));
    flushFrame();
    flushFrame();
    flushFrame();

    expect(adInit).toHaveBeenCalledTimes(1);
  });

  it('still runs after a query-only history change before load', async () => {
    // The SPA auction hook identifies routes by pathname only, so a query-only
    // replaceState is not a navigation: it must neither trigger an auction nor
    // cancel the pending initial adInit. (A URL-equality guard would abort
    // here and leave the initial ads uninitialized.)
    await importGptModule();
    const ts = (window as TestWindow).tsjs!;
    const adInit = vi.fn();
    ts.adInit = adInit;

    ts.scheduleInitialAdInit!();
    history.replaceState({}, '', '/?utm_source=newsletter');
    await flushAsync();
    expect(fetchStub).not.toHaveBeenCalled();

    window.dispatchEvent(new Event('load'));
    flushFrame();
    flushFrame();
    expect(adInit).toHaveBeenCalledTimes(1);
  });

  it('cancels the initial run after an /a → /b → /a round trip before load', async () => {
    // Both navigations commit and return to the original URL, so a URL
    // comparison would see "unchanged" and run adInit a second time against
    // the round-tripped route's live state. The navigation generation counts
    // both commits and stands the initial callback down.
    fetchStub.mockResolvedValue({
      ok: true,
      json: async () => ({ slots: [], bids: {} }),
    });
    await importGptModule();
    const ts = (window as TestWindow).tsjs!;
    const adInit = vi.fn();
    ts.adInit = adInit;

    ts.scheduleInitialAdInit!();
    history.pushState({}, '', '/b');
    await flushAsync();
    history.pushState({}, '', '/');
    await flushAsync();
    expect(ts.navGeneration).toBe(2);

    window.dispatchEvent(new Event('load'));
    flushFrame();
    flushFrame();
    expect(adInit).not.toHaveBeenCalled();
  });

  it('applies server targeting to a publisher-displayed slot before its refresh', async () => {
    // A publisher that defined and displayed its GPT slot before window load
    // still gets the server-side targeting applied before the refresh that
    // delivers it — the deferred run must order setTargeting ahead of the ad
    // request it triggers.
    const div = document.createElement('div');
    div.id = 'div-atf-sidebar';
    document.body.appendChild(div);
    const mockSlot = {
      addService: vi.fn().mockReturnThis(),
      setTargeting: vi.fn().mockReturnThis(),
      clearTargeting: vi.fn().mockReturnThis(),
      getSlotElementId: vi.fn().mockReturnValue('div-atf-sidebar'),
      getTargeting: vi.fn().mockReturnValue([]),
    };
    const mockPubads = {
      enableSingleRequest: vi.fn(),
      getSlots: vi.fn().mockReturnValue([mockSlot]),
      addEventListener: vi.fn(),
      refresh: vi.fn(),
    };
    (window as TestWindow).googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: vi.fn(),
      pubads: vi.fn().mockReturnValue(mockPubads),
      enableServices: vi.fn(),
    };
    await importGptModule();
    const ts = (window as TestWindow).tsjs!;
    ts.adSlots = [
      {
        id: 'atf_sidebar_ad',
        gam_unit_path: '/123/atf',
        div_id: 'div-atf-sidebar',
        formats: [[300, 250]],
        targeting: { pos: 'atf' },
      },
    ];
    ts.bids = { atf_sidebar_ad: { hb_pb: '1.00', hb_bidder: 'kargo' } };

    ts.scheduleInitialAdInit!();
    window.dispatchEvent(new Event('load'));
    flushFrame();
    flushFrame();

    expect(mockSlot.setTargeting).toHaveBeenCalledWith('hb_pb', '1.00');
    expect(mockSlot.setTargeting).toHaveBeenCalledWith('ts_initial', '1');
    expect(mockPubads.refresh).toHaveBeenCalledWith([mockSlot]);
    const targetingOrder = mockSlot.setTargeting.mock.invocationCallOrder[0]!;
    const refreshOrder = mockPubads.refresh.mock.invocationCallOrder[0]!;
    expect(targetingOrder).toBeLessThan(refreshOrder);
  });
});
