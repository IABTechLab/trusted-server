import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

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

/** Allow a MutationObserver-scheduled slot check to run. */
async function flushAnimationFrame(): Promise<void> {
  await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
  await Promise.resolve();
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

  it('increments navGeneration only when a pathname navigation is accepted', async () => {
    // The deferred initial-adInit bootstrap keys off this counter, so it must
    // move in lockstep with the hook's own navigation identity: bumped
    // synchronously for each accepted pathname change, untouched by the
    // query-only and same-path history calls the hook ignores.
    fetchStub.mockResolvedValue({
      ok: true,
      json: async () => ({ slots: [], bids: {} }),
    });
    const { installSpaAuctionHook } = await importGptModule();
    installSpaAuctionHook();
    const ts = (window as TestWindow).tsjs!;
    expect(ts.navGeneration).toBe(0);

    history.pushState({}, '', '/next-page');
    expect(ts.navGeneration).toBe(1);

    history.replaceState({}, '', '/next-page?utm_source=x');
    expect(ts.navGeneration).toBe(1);

    history.pushState({}, '', '/next-page');
    expect(ts.navGeneration).toBe(1);
    await flushAsync();
  });

  it.each(['bootstrap', 'bundle'] as const)(
    'invalidates unclaimed GPT handoffs before an SPA fetch (%s)',
    async (implementation) => {
      let resolveFetch: ((response: unknown) => void) | undefined;
      fetchStub.mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            resolveFetch = resolve;
          })
      );

      const routeADiv = document.createElement('div');
      routeADiv.id = 'div-atf-sidebar';
      document.body.appendChild(routeADiv);
      const routeASlot = {
        getSlotElementId: vi.fn().mockReturnValue(routeADiv.id),
      };
      const routeBSlot = {
        getSlotElementId: vi.fn().mockReturnValue('div-atf-sidebar-2'),
      };
      const nativeDefineSlot = vi.fn().mockReturnValue(routeBSlot);
      const nativeDisplay = vi.fn();
      const pubads = {
        getSlots: vi.fn().mockReturnValue([routeASlot]),
        refresh: vi.fn(),
      };
      const googletag = {
        cmd: { push: vi.fn((fn: () => void) => fn()) },
        defineSlot: nativeDefineSlot,
        display: nativeDisplay,
        pubads: vi.fn().mockReturnValue(pubads),
      };
      const staleHandoff = {
        gamUnitPath: '/123/atf',
        formats: [[300, 250]],
        divIdPrefix: 'div-atf-sidebar',
        slotElementId: routeADiv.id,
        publisherClaimed: false,
        suppressPublisherDisplay: false,
        suppressPublisherRefresh: false,
      };
      const claimedHandoff = {
        ...staleHandoff,
        slotElementId: 'div-claimed',
        publisherClaimed: true,
      };
      (window as TestWindow).googletag = googletag;
      (window as TestWindow).tsjs = {
        gptSlotHandoffs: {
          [staleHandoff.slotElementId]: staleHandoff,
          'div-atf-sidebar-hydrated': staleHandoff,
          [claimedHandoff.slotElementId]: claimedHandoff,
        },
      };

      if (implementation === 'bootstrap') {
        const bootstrap = readFileSync(
          resolve(process.cwd(), '../../trusted-server-core/src/integrations/gpt_bootstrap.js'),
          'utf8'
        );
        window.eval(bootstrap);
      }
      await importGptModule();

      routeADiv.remove();
      const routeBDiv = document.createElement('div');
      routeBDiv.id = 'div-atf-sidebar-2';
      document.body.appendChild(routeBDiv);
      history.pushState({}, '', '/route-b');

      const publisherSlot = (
        googletag.defineSlot as unknown as (
          adUnitPath: string,
          formats: number[][],
          elementId: string
        ) => typeof routeBSlot
      )('/123/atf', [[300, 250]], routeBDiv.id);
      googletag.display(routeBDiv.id);

      expect(publisherSlot).toBe(routeBSlot);
      expect(nativeDefineSlot).toHaveBeenCalledWith('/123/atf', [[300, 250]], routeBDiv.id);
      expect(nativeDisplay).toHaveBeenCalledWith(routeBDiv.id);
      expect((window as TestWindow).tsjs!.gptSlotHandoffs).toEqual({
        [claimedHandoff.slotElementId]: claimedHandoff,
      });
      expect(resolveFetch).toBeDefined();

      resolveFetch!({ ok: true, json: async () => ({ slots: [], bids: {} }) });
      await flushAsync();
    }
  );

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

    history.pushState({}, '', '/next-page?edition=fictional#section');
    await flushAsync();

    expect(fetchStub).toHaveBeenCalledWith(
      '/_ts/page-bids?path=%2Fnext-page',
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
    // A gated page-bids response (template switch, auction gate, or consent
    // denial) returns no slots. With no prior TS state to sweep, the hook must
    // not call adInit() so a gated navigation cannot activate publisher GPT.
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
    await flushAnimationFrame();

    expect(ts.adSlots).toEqual([{ id: 'late', div_id: 'div-late' }]);
    expect(ts.bids).toEqual({ late: { hb_pb: '2.00' } });
    expect(adInit).toHaveBeenCalledTimes(1);
  });

  it('applies bids immediately when a prefix-configured placement exists but is hidden', async () => {
    // A breakpoint-hidden placement (mobile-only config while on desktop) has
    // rendered its div but the tiered resolver returns no element for it. The
    // slot wait must count it as present — otherwise every navigation to the
    // route stalls for the full SPA_SLOT_WAIT_MS before applying bids to the
    // visible slots, and adInit skips the hidden slot anyway.
    document.body.innerHTML =
      '<div id="div-visible"></div>' + '<div id="ad-hidden-r1x" style="display:none"></div>';
    fetchStub.mockResolvedValue({
      ok: true,
      json: async () => ({
        slots: [
          { id: 'visible', div_id: 'div-visible' },
          { id: 'hidden', div_id: 'ad-hidden-' },
        ],
        bids: { visible: { hb_pb: '2.00' } },
      }),
    });
    const { installSpaAuctionHook } = await importGptModule();
    installSpaAuctionHook();
    const ts = (window as TestWindow).tsjs!;
    const adInit = vi.fn();
    ts.adInit = adInit;

    history.pushState({}, '', '/mixed-route');
    await flushAsync();

    // Bids apply without waiting out the slot timeout.
    expect(ts.adSlots).toEqual([
      { id: 'visible', div_id: 'div-visible' },
      { id: 'hidden', div_id: 'ad-hidden-' },
    ]);
    expect(ts.bids).toEqual({ visible: { hb_pb: '2.00' } });
    expect(adInit).toHaveBeenCalledTimes(1);
  });

  it('checks for route containers directly in a hidden document', async () => {
    vi.spyOn(document, 'visibilityState', 'get').mockReturnValue('hidden');
    vi.stubGlobal('requestAnimationFrame', undefined);
    fetchStub.mockResolvedValue({
      ok: true,
      json: async () => ({
        slots: [{ id: 'hidden', div_id: 'div-hidden' }],
        bids: { hidden: { hb_pb: '3.00' } },
      }),
    });
    const { installSpaAuctionHook } = await importGptModule();
    installSpaAuctionHook();
    const ts = (window as TestWindow).tsjs!;
    const adInit = vi.fn();
    ts.adInit = adInit;

    history.pushState({}, '', '/hidden-route');
    await flushAsync();
    expect(adInit).not.toHaveBeenCalled();

    document.body.innerHTML = '<div id="div-hidden"></div>';
    await flushAsync();

    expect(ts.adSlots).toEqual([{ id: 'hidden', div_id: 'div-hidden' }]);
    expect(ts.bids).toEqual({ hidden: { hb_pb: '3.00' } });
    expect(adInit).toHaveBeenCalledTimes(1);
  });

  it('cancels a pending visible-tab frame when the document becomes hidden', async () => {
    let visibility: DocumentVisibilityState = 'visible';
    vi.spyOn(document, 'visibilityState', 'get').mockImplementation(() => visibility);
    const requestAnimationFrameMock = vi.fn().mockReturnValue(17);
    const cancelAnimationFrameMock = vi.fn();
    vi.stubGlobal('requestAnimationFrame', requestAnimationFrameMock);
    vi.stubGlobal('cancelAnimationFrame', cancelAnimationFrameMock);
    fetchStub.mockResolvedValue({
      ok: true,
      json: async () => ({
        slots: [{ id: 'hidden-late', div_id: 'div-hidden-late' }],
        bids: { 'hidden-late': { hb_pb: '3.50' } },
      }),
    });
    const { installSpaAuctionHook } = await importGptModule();
    installSpaAuctionHook();
    const ts = (window as TestWindow).tsjs!;
    const adInit = vi.fn();
    ts.adInit = adInit;

    history.pushState({}, '', '/hidden-late-route');
    await flushAsync();

    // A mutation while visible schedules a frame that never runs.
    document.body.appendChild(document.createElement('span'));
    await flushAsync();
    expect(requestAnimationFrameMock).toHaveBeenCalledTimes(1);

    // The next mutation happens after the document is hidden. It must cancel
    // the stale frame and perform the presence check immediately.
    visibility = 'hidden';
    document.body.innerHTML = '<div id="div-hidden-late"></div>';
    await flushAsync();

    expect(cancelAnimationFrameMock).toHaveBeenCalledWith(17);
    expect(adInit).toHaveBeenCalledTimes(1);
    expect(ts.adSlots).toEqual([{ id: 'hidden-late', div_id: 'div-hidden-late' }]);
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
    await flushAnimationFrame();

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
      '/_ts/page-bids?path=%2Freplaced',
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
      '/_ts/page-bids?path=%2Fpopped',
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

  it('falls back to the deprecated alias when the canonical path is behind Basic Auth', async () => {
    // An operator `[[handlers]]` regex broad enough to cover `/_ts` answers the
    // canonical path with 401 that no anonymous browser fetch can satisfy.
    // Without the fallback, every SPA navigation on that deployment loses ads.
    document.body.innerHTML = '<div id="div-s1"></div>';
    fetchStub.mockResolvedValueOnce({ ok: false, status: 401 }).mockResolvedValueOnce({
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

    history.pushState({}, '', '/auth-gated');
    await flushAsync();

    expect(fetchStub).toHaveBeenNthCalledWith(
      1,
      '/_ts/page-bids?path=%2Fauth-gated',
      expect.anything()
    );
    // The fallback marks itself so the server can separate a current bundle
    // that could not use the canonical path (a deployment to fix) from a
    // pre-rename bundle (which ages out on its own).
    expect(fetchStub).toHaveBeenNthCalledWith(
      2,
      '/__ts/page-bids?path=%2Fauth-gated',
      expect.objectContaining({ headers: { 'X-TSJS-Page-Bids': 'fallback' } })
    );
    expect(ts.adSlots).toEqual([{ id: 's1', div_id: 'div-s1' }]);
    expect(adInit).toHaveBeenCalledTimes(1);
  });

  it('falls back to the deprecated alias when the canonical path returns a non-JSON body', async () => {
    // A server rolled back to before the rename does not register the canonical
    // path, so it falls through to the publisher-origin proxy and answers 200
    // HTML. That is the wrong endpoint, not a transient failure.
    document.body.innerHTML = '<div id="div-s1"></div>';
    fetchStub
      .mockResolvedValueOnce({
        ok: true,
        json: async () => {
          throw new SyntaxError('Unexpected token <');
        },
      })
      .mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          slots: [{ id: 's1', div_id: 'div-s1' }],
          bids: { s1: { hb_pb: '1.00' } },
        }),
      });
    const { installSpaAuctionHook } = await importGptModule();
    installSpaAuctionHook();
    const ts = (window as TestWindow).tsjs!;

    history.pushState({}, '', '/rolled-back');
    await flushAsync();

    expect(fetchStub).toHaveBeenNthCalledWith(
      2,
      '/__ts/page-bids?path=%2Frolled-back',
      expect.anything()
    );
    expect(ts.adSlots).toEqual([{ id: 's1', div_id: 'div-s1' }]);
  });

  it('stays on the alias for the rest of the session once the fallback works', async () => {
    // Re-probing the canonical path on every navigation would double the
    // request count for the whole session on an affected deployment.
    document.body.innerHTML = '<div id="div-s1"></div>';
    fetchStub.mockResolvedValueOnce({ ok: false, status: 401 }).mockResolvedValue({
      ok: true,
      json: async () => ({
        slots: [{ id: 's1', div_id: 'div-s1' }],
        bids: { s1: { hb_pb: '1.00' } },
      }),
    });
    const { installSpaAuctionHook } = await importGptModule();
    installSpaAuctionHook();

    history.pushState({}, '', '/first');
    await flushAsync();
    history.pushState({}, '', '/second');
    await flushAsync();

    expect(fetchStub).toHaveBeenCalledTimes(3);
    expect(fetchStub).toHaveBeenNthCalledWith(
      3,
      '/__ts/page-bids?path=%2Fsecond',
      expect.anything()
    );
  });

  it('does not retry the alias when the endpoint denies the request', async () => {
    // 403 is the cross-site gate, which applies to both registered paths — the
    // alias would deny it identically, so retrying only burns a request.
    fetchStub.mockResolvedValue({ ok: false, status: 403 });
    const { installSpaAuctionHook } = await importGptModule();
    installSpaAuctionHook();
    const ts = (window as TestWindow).tsjs!;
    const adInit = vi.fn();
    ts.adInit = adInit;

    history.pushState({}, '', '/denied');
    await flushAsync();

    expect(fetchStub).toHaveBeenCalledTimes(1);
    expect(ts.adSlots).toBeUndefined();
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
