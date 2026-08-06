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

  /** A slot config pointing at `divId`, shaped like the SSR-injected payload. */
  function slotFor(divId: string) {
    return {
      id: 'in_content_ad',
      gam_unit_path: '/123/in_content',
      div_id: divId,
      formats: [[300, 250]] as [number, number][],
      targeting: {},
    };
  }

  /** Append an ad-slot element the way the publisher's SSR markup would. */
  function appendSlotElement(divId: string): HTMLElement {
    const el = document.createElement('div');
    el.id = divId;
    document.body.appendChild(el);
    return el;
  }

  /**
   * Mark an element hydrated the way React DOM does: an own property whose name
   * carries the `__reactFiber$` prefix plus a per-instance random suffix.
   */
  function markReactHydrated(el: Element): void {
    Object.defineProperty(el, '__reactFiber$r4nd0m', { value: {}, configurable: true });
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
    // Remove the instance properties so the prototype getters are visible again.
    delete (document as unknown as Record<string, unknown>).readyState;
    delete (document as unknown as Record<string, unknown>).hidden;
    delete (window as unknown as Record<string, unknown>).requestAnimationFrame;
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it('applies the SSR payload and defers adInit until window load plus two animation frames', async () => {
    await importGptModule();
    const ts = (window as TestWindow).tsjs!;
    const adInit = vi.fn();
    ts.adInit = adInit;

    ts.scheduleInitialAdInit!({ atf: { hb_pb: '1.00' } });
    // On the initial document (generation 0) the SSR bids are adopted
    // immediately — the deferral applies to the GPT work, not the payload.
    expect(ts.bids).toEqual({ atf: { hb_pb: '1.00' } });
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

  it('fires once the slot containers are React-hydrated, before window load', async () => {
    // Fake only the poll's interval — leave requestAnimationFrame to the
    // manual rafQueue override installed in beforeEach.
    vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval'] });
    try {
      await importGptModule();
      const ts = (window as TestWindow).tsjs!;
      const adInit = vi.fn();
      ts.adInit = adInit;
      const el = appendSlotElement('ad-in_content-0');
      ts.adSlots = [slotFor('ad-in_content-0')];

      ts.scheduleInitialAdInit!();
      // The container is present but React has not claimed it yet. Mutating now
      // is what trips #418, so the poll must hold however long that takes.
      vi.advanceTimersByTime(500);
      flushFrame();
      flushFrame();
      expect(adInit).not.toHaveBeenCalled();

      // React hydrates the container the adInit pass will mutate.
      markReactHydrated(el);
      // No load event dispatched — the container poll must trigger it.
      vi.advanceTimersByTime(50);
      // The trigger fired, but the double rAF still gates the actual call.
      flushFrame();
      expect(adInit).not.toHaveBeenCalled();
      flushFrame();
      expect(adInit).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it('holds while any matched container is unhydrated, then fires on window load', async () => {
    // One container hydrates and the other never does. adInit issues a single
    // batched GPT request for the whole initial pass, so it must wait for the
    // laggard rather than mutate a container React has not reached.
    vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval'] });
    try {
      await importGptModule();
      const ts = (window as TestWindow).tsjs!;
      const adInit = vi.fn();
      ts.adInit = adInit;
      const first = appendSlotElement('ad-in_content-0');
      appendSlotElement('ad-in_content-1');
      ts.adSlots = [slotFor('ad-in_content-0'), slotFor('ad-in_content-1')];

      ts.scheduleInitialAdInit!();
      markReactHydrated(first);
      vi.advanceTimersByTime(500);
      flushFrame();
      flushFrame();
      expect(adInit).not.toHaveBeenCalled();

      // window.load remains the can't-hang fallback.
      window.dispatchEvent(new Event('load'));
      flushFrame();
      flushFrame();
      expect(adInit).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it('does not fire from the container poll on non-React pages, falls back to window load', async () => {
    // Fake only the poll's interval — leave requestAnimationFrame to the
    // manual rafQueue override installed in beforeEach.
    vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval'] });
    try {
      await importGptModule();
      const ts = (window as TestWindow).tsjs!;
      const adInit = vi.fn();
      ts.adInit = adInit;

      // No React (so containers never report hydrated): the poll must never
      // fire adInit on its own.
      ts.scheduleInitialAdInit!();
      vi.advanceTimersByTime(500);
      flushFrame();
      flushFrame();
      expect(adInit).not.toHaveBeenCalled();

      // window.load remains the fallback trigger.
      window.dispatchEvent(new Event('load'));
      flushFrame();
      flushFrame();
      expect(adInit).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it('fires exactly once when hydration and a later load event both occur', async () => {
    // Fake only the poll's interval — leave requestAnimationFrame to the
    // manual rafQueue override installed in beforeEach.
    vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval'] });
    try {
      await importGptModule();
      const ts = (window as TestWindow).tsjs!;
      const adInit = vi.fn();
      ts.adInit = adInit;
      const el = appendSlotElement('ad-in_content-0');
      ts.adSlots = [slotFor('ad-in_content-0')];

      ts.scheduleInitialAdInit!();
      markReactHydrated(el);
      vi.advanceTimersByTime(50);
      flushFrame();
      flushFrame();
      expect(adInit).toHaveBeenCalledTimes(1);

      // A later load event must not re-run adInit (poll cleared, load fires once).
      window.dispatchEvent(new Event('load'));
      flushFrame();
      flushFrame();
      expect(adInit).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it('fires exactly once when a load event precedes container hydration', async () => {
    // The reverse signal order: `load` wins the race on a page whose containers
    // hydrate later. The poll installed at schedule time must have been
    // cleared, so the late hydration cannot re-run adInit.
    vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval'] });
    try {
      await importGptModule();
      const ts = (window as TestWindow).tsjs!;
      const adInit = vi.fn();
      ts.adInit = adInit;
      const el = appendSlotElement('ad-in_content-0');
      ts.adSlots = [slotFor('ad-in_content-0')];

      ts.scheduleInitialAdInit!();
      window.dispatchEvent(new Event('load'));
      flushFrame();
      flushFrame();
      expect(adInit).toHaveBeenCalledTimes(1);

      markReactHydrated(el);
      vi.advanceTimersByTime(500);
      flushFrame();
      flushFrame();
      expect(adInit).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it('fires without waiting a poll tick when the containers are already hydrated', async () => {
    // On a streamed App Router page React can reach the ad containers before
    // the `</body>` bids script runs, so the synchronous pre-check must catch
    // it — no timer is ever advanced here.
    vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval'] });
    try {
      await importGptModule();
      const ts = (window as TestWindow).tsjs!;
      const adInit = vi.fn();
      ts.adInit = adInit;
      const el = appendSlotElement('ad-in_content-0');
      markReactHydrated(el);
      ts.adSlots = [slotFor('ad-in_content-0')];

      ts.scheduleInitialAdInit!();
      expect(vi.getTimerCount()).toBe(0);
      flushFrame();
      expect(adInit).not.toHaveBeenCalled();
      flushFrame();
      expect(adInit).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it('drops the SSR payload when a navigation committed before scheduling', async () => {
    // The SPA hook is installed by the synchronous head bundle, so a
    // navigation can commit while the document is still streaming — before
    // the </body> script calls the scheduler. The SSR payload then belongs
    // to a document the page has already left: it must not overwrite the
    // live route's bids, and the initial adInit must never fire.
    fetchStub.mockResolvedValue({
      ok: true,
      json: async () => ({ slots: [], bids: {} }),
    });
    await importGptModule();
    const ts = (window as TestWindow).tsjs!;
    const adInit = vi.fn();
    ts.adInit = adInit;

    history.pushState({}, '', '/b');
    await flushAsync();
    expect(ts.navGeneration).toBe(1);
    ts.bids = { live_slot: { hb_pb: '2.50' } };

    ts.scheduleInitialAdInit!({ ssr_slot: { hb_pb: '1.00' } });
    expect(ts.bids).toEqual({ live_slot: { hb_pb: '2.50' } });

    window.dispatchEvent(new Event('load'));
    flushFrame();
    flushFrame();
    expect(adInit).not.toHaveBeenCalled();
  });

  it('preserves a page-bids response applied before scheduling', async () => {
    // Same race, with the SPA navigation's page-bids response fully applied
    // (slots + bids + its own adInit) before the scheduler is called: the
    // stale SSR payload must not corrupt the applied state, and the route's
    // adInit count must stay at the SPA hook's single call.
    document.body.innerHTML = '<div id="div-s1"></div>';
    fetchStub.mockResolvedValue({
      ok: true,
      json: async () => ({
        slots: [{ id: 's1', div_id: 'div-s1' }],
        bids: { s1: { hb_pb: '3.00' } },
      }),
    });
    await importGptModule();
    const ts = (window as TestWindow).tsjs!;
    const adInit = vi.fn();
    ts.adInit = adInit;

    history.pushState({}, '', '/b');
    await flushAsync();
    expect(ts.bids).toEqual({ s1: { hb_pb: '3.00' } });
    expect(adInit).toHaveBeenCalledTimes(1);

    ts.scheduleInitialAdInit!({ ssr_slot: { hb_pb: '1.00' } });
    window.dispatchEvent(new Event('load'));
    flushFrame();
    flushFrame();

    expect(ts.bids).toEqual({ s1: { hb_pb: '3.00' } });
    expect(adInit).toHaveBeenCalledTimes(1);
  });

  it('cancels queued GPT work when a navigation commits before the command queue drains', async () => {
    // adInit() only queues its slot work on googletag.cmd, which drains when
    // GPT itself loads — possibly long after the generation check that
    // guarded the adInit() call. A navigation in that gap must cancel the
    // queued mutation, not let it run against the new route's DOM.
    const commandQueue: Array<() => void> = [];
    const mockPubads = {
      enableSingleRequest: vi.fn(),
      getSlots: vi.fn().mockReturnValue([]),
      addEventListener: vi.fn(),
      refresh: vi.fn(),
    };
    const defineSlot = vi.fn();
    const destroySlots = vi.fn();
    (window as TestWindow).googletag = {
      cmd: commandQueue,
      defineSlot,
      destroySlots,
      pubads: vi.fn().mockReturnValue(mockPubads),
      enableServices: vi.fn(),
    };
    fetchStub.mockResolvedValue({
      ok: true,
      json: async () => ({ slots: [], bids: {} }),
    });
    document.body.innerHTML = '<div id="div-atf-sidebar"></div>';
    await importGptModule();
    const ts = (window as TestWindow).tsjs!;
    ts.adSlots = [
      {
        id: 'atf_sidebar_ad',
        gam_unit_path: '/123/atf',
        div_id: 'div-atf-sidebar',
        formats: [[300, 250]],
      },
    ];
    ts.bids = { atf_sidebar_ad: { hb_pb: '1.00' } };

    // GPT not loaded yet: the queued work sits in the command array.
    ts.adInit!();
    expect(commandQueue.length).toBeGreaterThan(0);

    // A navigation commits before GPT drains the queue.
    history.pushState({}, '', '/b');
    await flushAsync();
    expect(ts.navGeneration).toBe(1);

    // GPT loads and drains the queue: the stale callback must stand down.
    commandQueue.splice(0).forEach((fn) => fn());
    expect(defineSlot).not.toHaveBeenCalled();
    expect(destroySlots).not.toHaveBeenCalled();
    expect(mockPubads.refresh).not.toHaveBeenCalled();
    expect(mockPubads.enableSingleRequest).not.toHaveBeenCalled();
  });

  it('rides animation frames in a hidden document, holding adInit until first view', async () => {
    // Browsers do not service rAF while the document is hidden, so a
    // background-tab load queues the frames but does not run them until the
    // tab is first viewed. This is intended (see installScheduleInitialAdInit):
    // the initial request spends its impression on a viewed tab. The scheduler
    // must keep riding rAF — not switch to a timer — while hidden.
    Object.defineProperty(document, 'hidden', {
      configurable: true,
      get: () => true,
    });
    await importGptModule();
    const ts = (window as TestWindow).tsjs!;
    const adInit = vi.fn();
    ts.adInit = adInit;

    ts.scheduleInitialAdInit!({ atf: { hb_pb: '1.00' } });
    window.dispatchEvent(new Event('load'));

    // Hidden tab: the frame chain is queued but unserviced — adInit waits.
    expect(rafQueue.length).toBeGreaterThan(0);
    expect(adInit).not.toHaveBeenCalled();

    // First view: the browser services the pending frames.
    flushFrame();
    flushFrame();
    expect(adInit).toHaveBeenCalledTimes(1);
  });
});
