import { describe, it, expect, vi, beforeEach, afterEach, afterAll } from 'vitest';

// Track every 'message' EventListener added to window across the entire test
// file.  This lets the installTsRenderBridge suite remove all accumulated
// handlers (registered by each vi.resetModules() + module re-import in the
// installTsAdInit suite) before dispatching its own events. The spy is
// restored and remaining handlers are detached in the afterAll below so the
// patch never leaks past this file.
const allMessageHandlers: EventListener[] = [];
const originalWindowAddEventListener = window.addEventListener.bind(window);
// Plain wrapper, deliberately not vi.spyOn: the render-bridge suite spies on
// window.addEventListener itself, and vi.spyOn on an already-spied method
// returns the same mock instance — its "original" would alias the inner
// implementation and recurse.
(window as { addEventListener: typeof window.addEventListener }).addEventListener = ((
  type: string,
  handler: EventListenerOrEventListenerObject,
  options?: boolean | AddEventListenerOptions
) => {
  if (type === 'message' && handler) {
    allMessageHandlers.push(handler as EventListener);
  }
  return originalWindowAddEventListener(type, handler, options);
}) as typeof window.addEventListener;

afterAll(() => {
  for (const handler of allMessageHandlers) {
    window.removeEventListener('message', handler);
  }
  allMessageHandlers.length = 0;
  (window as { addEventListener: typeof window.addEventListener }).addEventListener =
    originalWindowAddEventListener;
});

interface SlotRenderEvent {
  isEmpty: boolean;
  slot: {
    getSlotElementId(): string;
    getTargeting(key: string): string[];
  };
}

type TestWindow = Window & {
  googletag?: unknown;
  apstag?: { setDisplayBids?: () => void };
  // Typed as `any` to avoid the TypeScript intersection with the global
  // Window.tsjs declaration (TsjsApi from core/types.ts), which would require
  // every test fixture to satisfy the full TsjsApi shape.
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  tsjs?: any;
};

describe('installTsAdInit', () => {
  beforeEach(() => {
    vi.resetModules();
    const tw = window as TestWindow;
    delete tw.tsjs;
    // jsdom does not implement navigator.sendBeacon; polyfill it for tests
    if (!('sendBeacon' in navigator)) {
      Object.defineProperty(navigator, 'sendBeacon', {
        value: vi.fn().mockReturnValue(true),
        writable: true,
        configurable: true,
      });
    }
    // adInit now queries the DOM for div elements by id/prefix — create the
    // test div so getElementById and querySelector both resolve correctly.
    if (!document.getElementById('div-atf-sidebar')) {
      const div = document.createElement('div');
      div.id = 'div-atf-sidebar';
      document.body.appendChild(div);
    }
  });

  afterEach(() => {
    document.getElementById('div-atf-sidebar')?.remove();
    document.getElementById("ad'prefix-real")?.remove();
  });

  it('reads window.tsjs.bids synchronously and applies bid targeting before refresh', async () => {
    const mockSlot = {
      addService: vi.fn().mockReturnThis(),
      setTargeting: vi.fn().mockReturnThis(),
      getSlotElementId: vi.fn().mockReturnValue('div-atf-sidebar'),
      getTargeting: vi.fn().mockReturnValue(['abc']),
    };
    const mockPubads = {
      enableSingleRequest: vi.fn(),
      getSlots: vi.fn().mockReturnValue([mockSlot]),
      addEventListener: vi.fn(),
      refresh: vi.fn(),
    };
    (window as TestWindow).googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: vi.fn().mockReturnValue(mockSlot),
      pubads: vi.fn().mockReturnValue(mockPubads),
      enableServices: vi.fn(),
    };
    (window as TestWindow).tsjs = {
      adSlots: [
        {
          id: 'atf_sidebar_ad',
          gam_unit_path: '/123/atf',
          div_id: 'div-atf-sidebar',
          formats: [[300, 250]],
          targeting: { pos: 'atf' },
        },
      ],
      bids: {
        atf_sidebar_ad: {
          hb_pb: '1.00',
          hb_bidder: 'kargo',
          hb_adid: 'abc-uuid',
          hb_cache_host: 'cache.example.com',
          hb_cache_path: '/pbc/v1/cache',
          nurl: 'https://ssp/win',
          burl: 'https://ssp/bill',
        },
      },
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
    } as any;

    const fetchSpy = vi.spyOn(global, 'fetch');

    const { installTsAdInit } = await import('../../../src/integrations/gpt/index');
    installTsAdInit();
    (window as TestWindow).tsjs!.adInit!();

    expect(fetchSpy).not.toHaveBeenCalled();
    expect(mockSlot.setTargeting).toHaveBeenCalledWith('hb_pb', '1.00');
    expect(mockSlot.setTargeting).toHaveBeenCalledWith('hb_bidder', 'kargo');
    expect(mockSlot.setTargeting).toHaveBeenCalledWith('hb_adid', 'abc-uuid');
    expect(mockSlot.setTargeting).toHaveBeenCalledWith('hb_cache_host', 'cache.example.com');
    expect(mockSlot.setTargeting).toHaveBeenCalledWith('hb_cache_path', '/pbc/v1/cache');
    expect(mockSlot.setTargeting).toHaveBeenCalledWith('ts_initial', '1');
    expect(mockPubads.refresh).toHaveBeenCalled();

    fetchSpy.mockRestore();
  });

  it('displays TS-defined slots and does not include them in refresh', async () => {
    const mockSlot = {
      addService: vi.fn().mockReturnThis(),
      setTargeting: vi.fn().mockReturnThis(),
      getSlotElementId: vi.fn().mockReturnValue('div-atf-sidebar'),
      getTargeting: vi.fn().mockReturnValue([]),
    };
    const mockPubads = {
      enableSingleRequest: vi.fn(),
      // Publisher has not defined this slot, so TS defines (owns) it.
      getSlots: vi.fn().mockReturnValue([]),
      addEventListener: vi.fn(),
      refresh: vi.fn(),
    };
    const defineSlotMock = vi.fn().mockReturnValue(mockSlot);
    const displayMock = vi.fn();
    (window as TestWindow).googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: defineSlotMock,
      display: displayMock,
      pubads: vi.fn().mockReturnValue(mockPubads),
      enableServices: vi.fn(),
    };
    (window as TestWindow).tsjs = {
      adSlots: [
        {
          id: 'atf_sidebar_ad',
          gam_unit_path: '/123/atf',
          div_id: 'div-atf-sidebar',
          formats: [[300, 250]],
          targeting: {},
        },
      ],
      bids: {},
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
    } as any;

    const { installTsAdInit } = await import('../../../src/integrations/gpt/index');
    installTsAdInit();
    (window as TestWindow).tsjs!.adInit!();

    expect(defineSlotMock).toHaveBeenCalled();
    // GPT requires display() to register/render a freshly-defined slot.
    expect(displayMock).toHaveBeenCalledWith('div-atf-sidebar');
    // TS-owned slots are displayed, not refreshed (refresh() no-ops for a slot
    // that was never displayed).
    expect(mockPubads.refresh).not.toHaveBeenCalled();
  });

  it('refreshes TS-defined slots when the publisher disabled GPT initial load', async () => {
    // With pubads().disableInitialLoad(), display() only registers a freshly
    // defined slot — the ad request must come from refresh(). A TS-owned slot
    // must therefore be refreshed too, or it renders blank.
    const mockSlot = {
      addService: vi.fn().mockReturnThis(),
      setTargeting: vi.fn().mockReturnThis(),
      getSlotElementId: vi.fn().mockReturnValue('div-atf-sidebar'),
      getTargeting: vi.fn().mockReturnValue([]),
    };
    const mockPubads = {
      enableSingleRequest: vi.fn(),
      // Publisher has not defined this slot, so TS defines (owns) it.
      getSlots: vi.fn().mockReturnValue([]),
      addEventListener: vi.fn(),
      refresh: vi.fn(),
      disableInitialLoad: vi.fn(),
    };
    const displayMock = vi.fn();
    (window as TestWindow).googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: vi.fn().mockReturnValue(mockSlot),
      display: displayMock,
      pubads: vi.fn().mockReturnValue(mockPubads),
      enableServices: vi.fn(),
    };
    (window as TestWindow).tsjs = {
      adSlots: [
        {
          id: 'atf_sidebar_ad',
          gam_unit_path: '/123/atf',
          div_id: 'div-atf-sidebar',
          formats: [[300, 250]],
          targeting: {},
        },
      ],
      bids: {},
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
    } as any;

    const { installTsAdInit } = await import('../../../src/integrations/gpt/index');
    installTsAdInit();

    // Publisher disables initial load — goes through the wrapper the detector
    // installed, recording the state on window.tsjs.
    mockPubads.disableInitialLoad();
    expect((window as TestWindow).tsjs!.gptInitialLoadDisabled).toBe(true);

    (window as TestWindow).tsjs!.adInit!();

    // The slot is still registered via display(), and additionally refreshed so
    // it actually requests an ad under disableInitialLoad().
    expect(displayMock).toHaveBeenCalledWith('div-atf-sidebar');
    expect(mockPubads.refresh).toHaveBeenCalledWith([mockSlot]);
  });

  it('sets adInitRefreshInProgress only for the duration of the internal refresh', async () => {
    const mockSlot = {
      addService: vi.fn().mockReturnThis(),
      setTargeting: vi.fn().mockReturnThis(),
      getSlotElementId: vi.fn().mockReturnValue('div-atf-sidebar'),
      getTargeting: vi.fn().mockReturnValue([]),
    };
    let flagDuringRefresh: boolean | undefined;
    const mockPubads = {
      enableSingleRequest: vi.fn(),
      // Publisher-owned slot reused by TS, so it goes through refresh() (which
      // carries the bypass flag) rather than display().
      getSlots: vi.fn().mockReturnValue([mockSlot]),
      addEventListener: vi.fn(),
      refresh: vi.fn(() => {
        flagDuringRefresh = (window as TestWindow).tsjs!.adInitRefreshInProgress;
      }),
    };
    (window as TestWindow).googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: vi.fn().mockReturnValue(mockSlot),
      pubads: vi.fn().mockReturnValue(mockPubads),
      enableServices: vi.fn(),
    };
    (window as TestWindow).tsjs = {
      adSlots: [
        {
          id: 'atf_sidebar_ad',
          gam_unit_path: '/123/atf',
          div_id: 'div-atf-sidebar',
          formats: [[300, 250]],
          targeting: {},
        },
      ],
      bids: {},
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
    } as any;

    const { installTsAdInit } = await import('../../../src/integrations/gpt/index');
    installTsAdInit();
    (window as TestWindow).tsjs!.adInit!();

    expect(mockPubads.refresh).toHaveBeenCalled();
    expect(flagDuringRefresh).toBe(true);
    expect((window as TestWindow).tsjs!.adInitRefreshInProgress).toBe(false);
  });

  it('clears stale TS targeting from previously touched slots when the new route has no TS slots', async () => {
    const clearTargeting = vi.fn().mockReturnThis();
    const staleSlot = {
      addService: vi.fn().mockReturnThis(),
      setTargeting: vi.fn().mockReturnThis(),
      clearTargeting,
      getSlotElementId: vi.fn().mockReturnValue('div-old-route'),
      getTargeting: vi.fn().mockReturnValue([]),
    };
    const mockPubads = {
      enableSingleRequest: vi.fn(),
      getSlots: vi.fn().mockReturnValue([staleSlot]),
      addEventListener: vi.fn(),
      refresh: vi.fn(),
    };
    (window as TestWindow).googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: vi.fn(),
      pubads: vi.fn().mockReturnValue(mockPubads),
      enableServices: vi.fn(),
    };
    (window as TestWindow).tsjs = {
      // New route has no matching TS slots.
      adSlots: [],
      bids: {},
      // Previous route touched the publisher-owned slot on div-old-route.
      divToSlotId: { 'div-old-route': 'old_slot' },
      prevSlotTargetingKeys: { 'div-old-route': ['pos'] },
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
    } as any;

    const { installTsAdInit } = await import('../../../src/integrations/gpt/index');
    installTsAdInit();
    (window as TestWindow).tsjs!.adInit!();

    expect(clearTargeting).toHaveBeenCalledWith('hb_pb');
    expect(clearTargeting).toHaveBeenCalledWith('hb_bidder');
    expect(clearTargeting).toHaveBeenCalledWith('hb_adid');
    expect(clearTargeting).toHaveBeenCalledWith('hb_cache_host');
    expect(clearTargeting).toHaveBeenCalledWith('hb_cache_path');
    expect(clearTargeting).toHaveBeenCalledWith('ts_initial');
    expect(clearTargeting).toHaveBeenCalledWith('pos');
    expect(mockPubads.refresh).not.toHaveBeenCalled();
    expect((window as TestWindow).tsjs!.divToSlotId).toEqual({});
    expect((window as TestWindow).tsjs!.prevSlotTargetingKeys).toEqual({});
  });

  it('does not enable GPT services when the page-bids response has no slots', async () => {
    // A gated page-bids response returns no slots. With nothing to display or
    // refresh and services not already enabled, adInit() must not call
    // enableSingleRequest()/enableServices() and activate the publisher's GPT
    // services on a consent-denied or kill-switched navigation.
    const mockPubads = {
      enableSingleRequest: vi.fn(),
      getSlots: vi.fn().mockReturnValue([]),
      addEventListener: vi.fn(),
      refresh: vi.fn(),
    };
    const enableServices = vi.fn();
    (window as TestWindow).googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: vi.fn(),
      pubads: vi.fn().mockReturnValue(mockPubads),
      enableServices,
    };
    (window as TestWindow).tsjs = {
      adSlots: [],
      bids: {},
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
    } as any;

    const { installTsAdInit } = await import('../../../src/integrations/gpt/index');
    installTsAdInit();
    (window as TestWindow).tsjs!.adInit!();

    expect(mockPubads.enableSingleRequest).not.toHaveBeenCalled();
    expect(enableServices).not.toHaveBeenCalled();
    expect((window as TestWindow).tsjs!.servicesEnabled).toBeFalsy();
    expect(mockPubads.refresh).not.toHaveBeenCalled();
  });

  it('keeps the GAM path when debug adm is present', async () => {
    const slotEl = document.getElementById('div-atf-sidebar')!;
    const mockSlot = {
      addService: vi.fn().mockReturnThis(),
      setTargeting: vi.fn().mockReturnThis(),
      getSlotElementId: vi.fn().mockReturnValue('div-atf-sidebar'),
      getTargeting: vi.fn().mockReturnValue(['debug-uuid']),
    };
    const mockPubads = {
      enableSingleRequest: vi.fn(),
      getSlots: vi.fn().mockReturnValue([mockSlot]),
      addEventListener: vi.fn(),
      refresh: vi.fn(),
    };
    const destroySlots = vi.fn();
    (window as TestWindow).googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: vi.fn().mockReturnValue(mockSlot),
      destroySlots,
      pubads: vi.fn().mockReturnValue(mockPubads),
      enableServices: vi.fn(),
    };
    (window as TestWindow).tsjs = {
      adSlots: [
        {
          id: 'atf_sidebar_ad',
          gam_unit_path: '/123/atf',
          div_id: 'div-atf-sidebar',
          formats: [[300, 250]],
          targeting: { pos: 'atf' },
        },
      ],
      bids: {
        atf_sidebar_ad: {
          hb_pb: '0.20',
          hb_bidder: 'mocktioneer',
          hb_adid: 'debug-uuid',
          adm: '<div>Debug creative</div>',
        },
      },
    };

    const { installTsAdInit } = await import('../../../src/integrations/gpt/index');
    installTsAdInit();
    (window as TestWindow).tsjs!.adInit!();

    expect(slotEl.innerHTML).toBe('');
    expect(destroySlots).not.toHaveBeenCalledWith([mockSlot]);
    expect(mockSlot.setTargeting).toHaveBeenCalledWith('hb_pb', '0.20');
    expect(mockSlot.setTargeting).toHaveBeenCalledWith('hb_bidder', 'mocktioneer');
    expect(mockSlot.setTargeting).toHaveBeenCalledWith('hb_adid', 'debug-uuid');
    expect(mockSlot.setTargeting).toHaveBeenCalledWith('ts_initial', '1');
    expect(mockPubads.refresh).toHaveBeenCalledWith([mockSlot]);
  });

  it('does not fire win/billing beacons from slotRenderEnded targeting alone', async () => {
    const beaconSpy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true);
    let capturedListener: ((e: SlotRenderEvent) => void) | undefined;

    const mockSlot = {
      addService: vi.fn().mockReturnThis(),
      setTargeting: vi.fn().mockReturnThis(),
      getSlotElementId: vi.fn().mockReturnValue('div-atf-sidebar'),
      getTargeting: vi.fn().mockReturnValue(['abc']),
    };
    const mockPubads = {
      enableSingleRequest: vi.fn(),
      getSlots: vi.fn().mockReturnValue([mockSlot]),
      refresh: vi.fn(),
      addEventListener: vi.fn((event: string, fn: (e: SlotRenderEvent) => void) => {
        if (event === 'slotRenderEnded') capturedListener = fn;
      }),
    };
    (window as TestWindow).googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: vi.fn().mockReturnValue(mockSlot),
      pubads: vi.fn().mockReturnValue(mockPubads),
      enableServices: vi.fn(),
    };
    (window as TestWindow).tsjs = {
      adSlots: [
        {
          id: 'atf_sidebar_ad',
          gam_unit_path: '/123/atf',
          div_id: 'div-atf-sidebar',
          formats: [[300, 250]],
          targeting: {},
        },
      ],
      bids: {
        atf_sidebar_ad: {
          hb_pb: '1.00',
          hb_bidder: 'kargo',
          hb_adid: 'abc',
          nurl: 'https://ssp/win',
          burl: 'https://ssp/bill',
        },
      },
    };

    const { installTsAdInit } = await import('../../../src/integrations/gpt/index');
    installTsAdInit();
    (window as TestWindow).tsjs!.adInit!();

    expect(capturedListener).toBeDefined();
    capturedListener!({ isEmpty: false, slot: mockSlot });

    expect(beaconSpy).not.toHaveBeenCalled();

    // GPT slot targeting is request state, not proof that the TS creative
    // rendered. A repeated non-empty render must still not bill from this path.
    capturedListener!({ isEmpty: false, slot: mockSlot });
    expect(beaconSpy).not.toHaveBeenCalled();

    beaconSpy.mockRestore();
  });

  it('stamps trace markers and records the render on slotRenderEnded', async () => {
    let capturedListener: ((e: SlotRenderEvent) => void) | undefined;

    const mockSlot = {
      addService: vi.fn().mockReturnThis(),
      setTargeting: vi.fn().mockReturnThis(),
      getSlotElementId: vi.fn().mockReturnValue('div-atf-sidebar'),
      getTargeting: vi.fn().mockReturnValue([]),
    };
    const mockPubads = {
      enableSingleRequest: vi.fn(),
      getSlots: vi.fn().mockReturnValue([mockSlot]),
      refresh: vi.fn(),
      addEventListener: vi.fn((event: string, fn: (e: SlotRenderEvent) => void) => {
        if (event === 'slotRenderEnded') capturedListener = fn;
      }),
    };
    (window as TestWindow).googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: vi.fn().mockReturnValue(mockSlot),
      pubads: vi.fn().mockReturnValue(mockPubads),
      enableServices: vi.fn(),
    };
    (window as TestWindow).tsjs = {
      adSlots: [
        {
          id: 'atf_sidebar_ad',
          gam_unit_path: '/123/atf',
          div_id: 'div-atf-sidebar',
          formats: [[300, 250]],
          targeting: {},
        },
      ],
      bids: {
        atf_sidebar_ad: {
          hb_pb: '1.00',
          hb_bidder: 'kargo',
          hb_adid: 'cache-uuid-9',
          hb_bid_id: 'bid-uuid-9',
          hb_auction_id: 'ts-req-trace9',
          hb_crid: 'cr-98765',
          hb_adm_hash: 'a1b2c3d4e5f60718',
        },
      },
    };

    const { installTsAdInit } = await import('../../../src/integrations/gpt/index');
    installTsAdInit();
    (window as TestWindow).tsjs!.adInit!();

    expect(capturedListener).toBeDefined();
    capturedListener!({ isEmpty: false, slot: mockSlot });

    const el = document.getElementById('div-atf-sidebar')!;
    expect(el.getAttribute('data-ts-slot-id')).toBe('atf_sidebar_ad');
    expect(el.getAttribute('data-ts-render-path')).toBe('ssat');
    expect(el.getAttribute('data-ts-rendered')).toBe('true');
    expect(el.getAttribute('data-ts-auction-id')).toBe('ts-req-trace9');
    expect(el.getAttribute('data-ts-bidder')).toBe('kargo');
    expect(el.getAttribute('data-ts-ad-id')).toBe('cache-uuid-9');
    expect(el.getAttribute('data-ts-bid-id')).toBe('bid-uuid-9');
    expect(el.getAttribute('data-ts-creative-id')).toBe('cr-98765');
    expect(el.getAttribute('data-ts-adm-hash')).toBe('a1b2c3d4e5f60718');

    const record = (window as TestWindow).tsjs!.renders?.['atf_sidebar_ad'];
    expect(record).toEqual(
      expect.objectContaining({
        slotId: 'atf_sidebar_ad',
        path: 'ssat',
        rendered: true,
        elementId: 'div-atf-sidebar',
        auctionId: 'ts-req-trace9',
        bidder: 'kargo',
        adId: 'cache-uuid-9',
        bidId: 'bid-uuid-9',
        creativeId: 'cr-98765',
        admHash: 'a1b2c3d4e5f60718',
        servedFrom: 'gam',
      })
    );

    // An empty render must record rendered:false and bump the count.
    capturedListener!({ isEmpty: true, slot: mockSlot });
    const second = (window as TestWindow).tsjs!.renders?.['atf_sidebar_ad'];
    expect(second?.rendered).toBe(false);
    expect(second?.count).toBe(2);
    expect(el.getAttribute('data-ts-rendered')).toBe('false');
  });

  it('drops a late render from a reused publisher slot without consuming the new route', async () => {
    let capturedListener: ((e: SlotRenderEvent) => void) | undefined;
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
      refresh: vi.fn(),
      addEventListener: vi.fn((event: string, fn: (e: SlotRenderEvent) => void) => {
        if (event === 'slotRenderEnded') capturedListener = fn;
      }),
    };
    (window as TestWindow).googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: vi.fn().mockReturnValue(mockSlot),
      pubads: vi.fn().mockReturnValue(mockPubads),
      enableServices: vi.fn(),
    };
    (window as TestWindow).tsjs = {
      adSlots: [
        {
          id: 'atf_sidebar_ad',
          gam_unit_path: '/123/atf',
          div_id: 'div-atf-sidebar',
          formats: [[300, 250]],
          targeting: {},
        },
      ],
      bids: {
        atf_sidebar_ad: {
          hb_bidder: 'route-a',
          hb_adid: 'ad-a',
          hb_bid_id: 'bid-a',
          hb_auction_id: 'auction-a',
        },
      },
    };

    const { installTsAdInit } = await import('../../../src/integrations/gpt/index');
    installTsAdInit();
    const ts = (window as TestWindow).tsjs!;
    ts.adInit!();

    ts.bids = {
      atf_sidebar_ad: {
        hb_bidder: 'route-b',
        hb_adid: 'ad-b',
        hb_bid_id: 'bid-b',
        hb_auction_id: 'auction-b',
      },
    };
    ts.adInit!();

    // Route A's event arrives only after route B has reused and refreshed the
    // same publisher-owned GPT slot object. It must be rejected, not stamped
    // with route B's tuple or allowed to consume route B's pending request.
    capturedListener!({ isEmpty: false, slot: mockSlot });
    expect(ts.renders?.['atf_sidebar_ad']).toBeUndefined();

    capturedListener!({ isEmpty: false, slot: mockSlot });
    expect(ts.renders?.['atf_sidebar_ad']).toEqual(
      expect.objectContaining({
        path: 'ssat',
        auctionId: 'auction-b',
        bidId: 'bid-b',
        bidder: 'route-b',
      })
    );
  });

  it('does not confirm a deferred ADM placement after navigation supersedes it', async () => {
    let capturedListener: ((e: SlotRenderEvent) => void) | undefined;
    let runDeferredPlacement: FrameRequestCallback | undefined;
    vi.stubGlobal(
      'requestAnimationFrame',
      vi.fn((callback: FrameRequestCallback) => {
        runDeferredPlacement = callback;
        return 1;
      })
    );

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
      refresh: vi.fn(),
      addEventListener: vi.fn((event: string, fn: (e: SlotRenderEvent) => void) => {
        if (event === 'slotRenderEnded') capturedListener = fn;
      }),
    };
    (window as TestWindow).googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: vi.fn().mockReturnValue(mockSlot),
      pubads: vi.fn().mockReturnValue(mockPubads),
      enableServices: vi.fn(),
    };
    (window as TestWindow).tsjs = {
      adSlots: [
        {
          id: 'atf_sidebar_ad',
          gam_unit_path: '/123/atf',
          div_id: 'div-atf-sidebar',
          formats: [[300, 250]],
          targeting: {},
        },
      ],
      bids: {
        atf_sidebar_ad: {
          hb_bidder: 'mocktioneer',
          hb_adid: 'deferred-ad',
          hb_auction_id: 'auction-a',
          adm: '<iframe src="https://creative.example.com/render"></iframe>',
        },
      },
    };

    try {
      const { installTsAdInit } = await import('../../../src/integrations/gpt/index');
      installTsAdInit();
      const ts = (window as TestWindow).tsjs!;
      ts.adInit!();
      capturedListener!({ isEmpty: false, slot: mockSlot });

      const oldRecord = ts.renders?.['atf_sidebar_ad'];
      expect(oldRecord?.injected).toBe(false);
      expect(runDeferredPlacement).toBeDefined();

      // A newer route/adInit starts before the animation-frame retry runs while
      // retaining the same publisher-owned slot element. The old callback must
      // not mutate that shared element before its trace guard runs.
      ts.adInit!();
      const reusedSlot = document.getElementById('div-atf-sidebar')!;
      runDeferredPlacement!(0);

      expect(oldRecord?.injected).toBe(false);
      expect(reusedSlot.querySelector('iframe')).toBeNull();
      expect(reusedSlot.getAttribute('data-ts-injected')).toBe('false');
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it('confirms a successful deferred ADM placement on the original record', async () => {
    let capturedListener: ((e: SlotRenderEvent) => void) | undefined;
    let runDeferredPlacement: FrameRequestCallback | undefined;
    vi.stubGlobal(
      'requestAnimationFrame',
      vi.fn((callback: FrameRequestCallback) => {
        runDeferredPlacement = callback;
        return 1;
      })
    );
    const mockSlot = {
      addService: vi.fn().mockReturnThis(),
      setTargeting: vi.fn().mockReturnThis(),
      getSlotElementId: vi.fn().mockReturnValue('div-atf-sidebar'),
      getTargeting: vi.fn().mockReturnValue([]),
    };
    const mockPubads = {
      enableSingleRequest: vi.fn(),
      getSlots: vi.fn().mockReturnValue([mockSlot]),
      refresh: vi.fn(),
      addEventListener: vi.fn((event: string, fn: (e: SlotRenderEvent) => void) => {
        if (event === 'slotRenderEnded') capturedListener = fn;
      }),
    };
    (window as TestWindow).googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: vi.fn().mockReturnValue(mockSlot),
      pubads: vi.fn().mockReturnValue(mockPubads),
      enableServices: vi.fn(),
    };
    (window as TestWindow).tsjs = {
      adSlots: [
        {
          id: 'atf_sidebar_ad',
          gam_unit_path: '/123/atf',
          div_id: 'div-atf-sidebar',
          formats: [[300, 250]],
          targeting: {},
        },
      ],
      bids: {
        atf_sidebar_ad: {
          hb_adid: 'deferred-ad',
          adm: '<iframe src="https://creative.example.com/render"></iframe>',
        },
      },
    };

    try {
      const { installTsAdInit } = await import('../../../src/integrations/gpt/index');
      installTsAdInit();
      const ts = (window as TestWindow).tsjs!;
      ts.adInit!();
      capturedListener!({ isEmpty: false, slot: mockSlot });
      const record = ts.renders?.atf_sidebar_ad;
      const originalBookkeeping = {
        seq: record?.seq,
        count: record?.count,
        at: record?.at,
        historyLength: ts.renderLog?.length,
      };

      runDeferredPlacement!(0);

      expect(ts.renders?.atf_sidebar_ad).toBe(record);
      expect(record?.injected).toBe(true);
      expect({
        seq: record?.seq,
        count: record?.count,
        at: record?.at,
        historyLength: ts.renderLog?.length,
      }).toEqual(originalBookkeeping);
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it('does not attribute a later GAM refresh to the finished server-side auction', async () => {
    let capturedListener: ((e: SlotRenderEvent) => void) | undefined;

    const mockSlot = {
      addService: vi.fn().mockReturnThis(),
      setTargeting: vi.fn().mockReturnThis(),
      getSlotElementId: vi.fn().mockReturnValue('div-atf-sidebar'),
      getTargeting: vi.fn().mockReturnValue([]),
    };
    const mockPubads = {
      enableSingleRequest: vi.fn(),
      getSlots: vi.fn().mockReturnValue([mockSlot]),
      refresh: vi.fn(),
      addEventListener: vi.fn((event: string, fn: (e: SlotRenderEvent) => void) => {
        if (event === 'slotRenderEnded') capturedListener = fn;
      }),
    };
    (window as TestWindow).googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: vi.fn().mockReturnValue(mockSlot),
      pubads: vi.fn().mockReturnValue(mockPubads),
      enableServices: vi.fn(),
    };
    (window as TestWindow).tsjs = {
      adSlots: [
        {
          id: 'atf_sidebar_ad',
          gam_unit_path: '/123/atf',
          div_id: 'div-atf-sidebar',
          formats: [[300, 250]],
          targeting: {},
        },
      ],
      bids: {
        atf_sidebar_ad: {
          hb_bidder: 'kargo',
          hb_adid: 'cache-uuid-9',
          hb_auction_id: 'ts-req-trace9',
          hb_adm_hash: 'a1b2c3d4e5f60718',
        },
      },
    };

    const { installTsAdInit } = await import('../../../src/integrations/gpt/index');
    installTsAdInit();
    (window as TestWindow).tsjs!.adInit!();

    // First render consumes the targeting adInit applied → attributable.
    capturedListener!({ isEmpty: false, slot: mockSlot });
    expect((window as TestWindow).tsjs!.renders?.['atf_sidebar_ad']?.path).toBe('ssat');

    // A publisher-driven refresh fills the slot again, but the server-side
    // auction ran once and is long finished. ts.bids still holds its data —
    // re-stamping it would claim a render that auction never produced.
    capturedListener!({ isEmpty: false, slot: mockSlot });

    const refreshed = (window as TestWindow).tsjs!.renders?.['atf_sidebar_ad'];
    expect(refreshed?.path).toBe('gam-refresh');
    expect(refreshed?.rendered).toBe(true);
    expect(refreshed?.count).toBe(2);
    expect(refreshed?.auctionId).toBeUndefined();
    expect(refreshed?.bidder).toBeUndefined();
    expect(refreshed?.admHash).toBeUndefined();

    // Stale attribution must not survive on the DOM either.
    const el = document.getElementById('div-atf-sidebar')!;
    expect(el.getAttribute('data-ts-render-path')).toBe('gam-refresh');
    expect(el.hasAttribute('data-ts-auction-id')).toBe(false);
    expect(el.hasAttribute('data-ts-adm-hash')).toBe(false);
  });

  it('does not fire beacons for an APS-style bid that carries no hb_adid', async () => {
    const beaconSpy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true);
    let capturedListener: ((e: SlotRenderEvent) => void) | undefined;

    const mockSlot = {
      addService: vi.fn().mockReturnThis(),
      setTargeting: vi.fn().mockReturnThis(),
      getSlotElementId: vi.fn().mockReturnValue('div-atf-sidebar'),
      getTargeting: vi.fn().mockReturnValue([]),
    };
    const mockPubads = {
      enableSingleRequest: vi.fn(),
      getSlots: vi.fn().mockReturnValue([mockSlot]),
      refresh: vi.fn(),
      addEventListener: vi.fn((event: string, fn: (e: SlotRenderEvent) => void) => {
        if (event === 'slotRenderEnded') capturedListener = fn;
      }),
    };
    (window as TestWindow).googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: vi.fn().mockReturnValue(mockSlot),
      pubads: vi.fn().mockReturnValue(mockPubads),
      enableServices: vi.fn(),
    };
    (window as TestWindow).tsjs = {
      adSlots: [
        {
          id: 'atf_sidebar_ad',
          gam_unit_path: '/123/atf',
          div_id: 'div-atf-sidebar',
          formats: [[300, 250]],
          targeting: {},
        },
      ],
      bids: {
        atf_sidebar_ad: {
          hb_pb: '1.50',
          hb_bidder: 'aps',
          nurl: 'https://aps/win',
          burl: 'https://aps/bill',
        },
      },
    };

    const { installTsAdInit } = await import('../../../src/integrations/gpt/index');
    installTsAdInit();
    (window as TestWindow).tsjs!.adInit!();

    expect(capturedListener).toBeDefined();

    // Without an hb_adid to confirm the rendered creative is ours, a non-empty
    // render is not proof of a TS win: the slot could have been filled by other
    // GAM demand. The beacon must not fire, so we never over-report billing.
    capturedListener!({ isEmpty: false, slot: mockSlot });
    expect(beaconSpy).not.toHaveBeenCalled();

    beaconSpy.mockRestore();
  });

  it('does not fire nurl/burl when bid did not win GAM line item', async () => {
    const beaconSpy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true);
    let capturedListener: ((e: SlotRenderEvent) => void) | undefined;

    const mockSlotNoMatch = {
      addService: vi.fn().mockReturnThis(),
      setTargeting: vi.fn().mockReturnThis(),
      getSlotElementId: vi.fn().mockReturnValue('div-atf-sidebar'),
      getTargeting: vi.fn().mockReturnValue(['OTHER_BID_ID']),
    };
    const mockPubads = {
      enableSingleRequest: vi.fn(),
      getSlots: vi.fn().mockReturnValue([mockSlotNoMatch]),
      refresh: vi.fn(),
      addEventListener: vi.fn((event: string, fn: (e: SlotRenderEvent) => void) => {
        if (event === 'slotRenderEnded') capturedListener = fn;
      }),
    };
    (window as TestWindow).googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: vi.fn().mockReturnValue(mockSlotNoMatch),
      pubads: vi.fn().mockReturnValue(mockPubads),
      enableServices: vi.fn(),
    };
    (window as TestWindow).tsjs = {
      adSlots: [
        {
          id: 'atf_sidebar_ad',
          gam_unit_path: '/123/atf',
          div_id: 'div-atf-sidebar',
          formats: [[300, 250]],
          targeting: {},
        },
      ],
      bids: {
        atf_sidebar_ad: {
          hb_pb: '1.00',
          hb_bidder: 'kargo',
          hb_adid: 'abc',
          nurl: 'https://ssp/win',
          burl: 'https://ssp/bill',
        },
      },
    };

    const { installTsAdInit } = await import('../../../src/integrations/gpt/index');
    installTsAdInit();
    (window as TestWindow).tsjs!.adInit!();
    capturedListener!({ isEmpty: false, slot: mockSlotNoMatch });

    expect(beaconSpy).not.toHaveBeenCalled();
    beaconSpy.mockRestore();
  });

  it('does not fire beacons for slotRenderEnded on slots not owned by TS', async () => {
    const beaconSpy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true);
    let capturedListener: ((e: SlotRenderEvent) => void) | undefined;

    const mockSlot = {
      addService: vi.fn().mockReturnThis(),
      setTargeting: vi.fn().mockReturnThis(),
      getSlotElementId: vi.fn().mockReturnValue('div-atf-sidebar'),
      getTargeting: vi.fn().mockReturnValue(['abc']),
    };
    const arenaSlot = {
      getSlotElementId: () => 'arena-owned-div',
      getTargeting: () => [],
    };
    const mockPubads = {
      enableSingleRequest: vi.fn(),
      getSlots: vi.fn().mockReturnValue([mockSlot]),
      refresh: vi.fn(),
      addEventListener: vi.fn((event: string, fn: (e: SlotRenderEvent) => void) => {
        if (event === 'slotRenderEnded') capturedListener = fn;
      }),
    };
    (window as TestWindow).googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: vi.fn().mockReturnValue(mockSlot),
      pubads: vi.fn().mockReturnValue(mockPubads),
      enableServices: vi.fn(),
    };
    (window as TestWindow).tsjs = {
      adSlots: [
        {
          id: 'atf_sidebar_ad',
          gam_unit_path: '/123/atf',
          div_id: 'div-atf-sidebar',
          formats: [[300, 250]],
          targeting: {},
        },
      ],
      bids: {
        atf_sidebar_ad: { hb_pb: '1.00', hb_bidder: 'kargo', hb_adid: 'abc' },
      },
    };

    const { installTsAdInit } = await import('../../../src/integrations/gpt/index');
    installTsAdInit();
    (window as TestWindow).tsjs!.adInit!();

    capturedListener!({ isEmpty: false, slot: arenaSlot });

    expect(beaconSpy).not.toHaveBeenCalled();
    beaconSpy.mockRestore();
  });

  it('calls apstag.setDisplayBids when hb_bidder is aps', async () => {
    const setDisplayBidsSpy = vi.fn();
    (window as TestWindow).apstag = { setDisplayBids: setDisplayBidsSpy };

    const mockSlot = {
      addService: vi.fn().mockReturnThis(),
      setTargeting: vi.fn().mockReturnThis(),
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
      defineSlot: vi.fn().mockReturnValue(mockSlot),
      pubads: vi.fn().mockReturnValue(mockPubads),
      enableServices: vi.fn(),
    };
    (window as TestWindow).tsjs = {
      adSlots: [
        {
          id: 'atf_sidebar_ad',
          gam_unit_path: '/123/atf',
          div_id: 'div-atf-sidebar',
          formats: [[300, 250]],
          targeting: {},
        },
      ],
      bids: {
        atf_sidebar_ad: { hb_pb: '1.50', hb_bidder: 'aps', nurl: '', burl: '' },
      },
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
    } as any;

    const { installTsAdInit } = await import('../../../src/integrations/gpt/index');
    installTsAdInit();
    (window as TestWindow).tsjs!.adInit!();

    expect(setDisplayBidsSpy).toHaveBeenCalled();

    delete (window as TestWindow).apstag;
  });

  it('does not call apstag.setDisplayBids when hb_bidder is not aps', async () => {
    const setDisplayBidsSpy = vi.fn();
    (window as TestWindow).apstag = { setDisplayBids: setDisplayBidsSpy };

    const mockSlot = {
      addService: vi.fn().mockReturnThis(),
      setTargeting: vi.fn().mockReturnThis(),
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
      defineSlot: vi.fn().mockReturnValue(mockSlot),
      pubads: vi.fn().mockReturnValue(mockPubads),
      enableServices: vi.fn(),
    };
    (window as TestWindow).tsjs = {
      adSlots: [
        {
          id: 'atf_sidebar_ad',
          gam_unit_path: '/123/atf',
          div_id: 'div-atf-sidebar',
          formats: [[300, 250]],
          targeting: {},
        },
      ],
      bids: {
        atf_sidebar_ad: { hb_pb: '1.00', hb_bidder: 'kargo' },
      },
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
    } as any;

    const { installTsAdInit } = await import('../../../src/integrations/gpt/index');
    installTsAdInit();
    (window as TestWindow).tsjs!.adInit!();

    expect(setDisplayBidsSpy).not.toHaveBeenCalled();

    delete (window as TestWindow).apstag;
  });

  it('calls refresh even when tsjs.bids is empty (graceful fallback)', async () => {
    const emptyTestSlot = {
      addService: vi.fn().mockReturnThis(),
      setTargeting: vi.fn().mockReturnThis(),
      getSlotElementId: vi.fn().mockReturnValue('div-atf-sidebar'),
      getTargeting: vi.fn().mockReturnValue([]),
    };
    const mockPubads = {
      enableSingleRequest: vi.fn(),
      getSlots: vi.fn().mockReturnValue([emptyTestSlot]),
      addEventListener: vi.fn(),
      refresh: vi.fn(),
    };
    (window as TestWindow).googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: vi.fn().mockReturnValue({
        addService: vi.fn().mockReturnThis(),
        setTargeting: vi.fn().mockReturnThis(),
      }),
      pubads: vi.fn().mockReturnValue(mockPubads),
      enableServices: vi.fn(),
    };
    (window as TestWindow).tsjs = {
      adSlots: [
        {
          id: 'atf_sidebar_ad',
          gam_unit_path: '/123/atf',
          div_id: 'div-atf-sidebar',
          formats: [[300, 250]],
          targeting: {},
        },
      ],
      bids: {},
    };

    const { installTsAdInit } = await import('../../../src/integrations/gpt/index');
    installTsAdInit();
    (window as TestWindow).tsjs!.adInit!();

    expect(mockPubads.refresh).toHaveBeenCalled();
  });

  it('resolves dynamic div prefixes without interpolating div_id into a CSS selector', async () => {
    const dynamicDiv = document.createElement('div');
    dynamicDiv.id = "ad'prefix-real";
    document.body.appendChild(dynamicDiv);

    const dynamicSlot = {
      addService: vi.fn().mockReturnThis(),
      setTargeting: vi.fn().mockReturnThis(),
      getSlotElementId: vi.fn().mockReturnValue("ad'prefix-real"),
      getTargeting: vi.fn().mockReturnValue([]),
    };
    const mockPubads = {
      enableSingleRequest: vi.fn(),
      getSlots: vi.fn().mockReturnValue([dynamicSlot]),
      addEventListener: vi.fn(),
      refresh: vi.fn(),
    };
    (window as TestWindow).googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: vi.fn().mockReturnValue(dynamicSlot),
      pubads: vi.fn().mockReturnValue(mockPubads),
      enableServices: vi.fn(),
    };
    (window as TestWindow).tsjs = {
      adSlots: [
        {
          id: 'dynamic_slot',
          gam_unit_path: '/123/dynamic',
          div_id: "ad'prefix-",
          formats: [[300, 250]],
          targeting: {},
        },
      ],
      bids: {},
    };

    const { installTsAdInit } = await import('../../../src/integrations/gpt/index');
    installTsAdInit();

    expect(() => (window as TestWindow).tsjs!.adInit!()).not.toThrow();
    expect(mockPubads.refresh).toHaveBeenCalledWith([dynamicSlot]);
  });
});

describe('installTsRenderBridge', () => {
  let fetchStub: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    vi.resetModules();
    // Remove ALL accumulated 'message' handlers from previous test module imports
    // to prevent stale bridge listeners from intercepting our test event.
    for (const handler of allMessageHandlers) {
      window.removeEventListener('message', handler);
    }
    allMessageHandlers.length = 0;

    fetchStub = vi.fn();
    vi.stubGlobal('fetch', fetchStub);
    if (typeof navigator.sendBeacon !== 'function') {
      Object.defineProperty(navigator, 'sendBeacon', {
        value: vi.fn().mockReturnValue(true),
        writable: true,
        configurable: true,
      });
    }

    (window as TestWindow).tsjs = {
      bids: {
        homepage_header: {
          hb_adid: 'test-cache-uuid',
          hb_bidder: 'kargo',
          hb_pb: '1.50',
          hb_cache_host: 'openads.example.com',
          hb_cache_path: '/cache',
          nurl: 'https://ssp.example/win',
          burl: 'https://ssp.example/bill',
        },
      },
      adSlots: [
        {
          id: 'homepage_header',
          formats: [[728, 90]] as [number, number][],
          gam_unit_path: '/a/b/c',
          div_id: 'div-header',
          targeting: {},
        },
      ],
    };
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    document.getElementById('div-header')?.remove();
    delete (window as TestWindow).tsjs;
  });

  function createTrustedSlotIframe(): Window {
    const slot = document.createElement('div');
    slot.id = 'div-header';
    const iframe = document.createElement('iframe');
    slot.appendChild(iframe);
    document.body.appendChild(slot);
    return iframe.contentWindow!;
  }

  async function captureBridgeListener(): Promise<(e: MessageEvent) => unknown> {
    let bridgeListener: ((e: MessageEvent) => unknown) | undefined;
    const origAdd = window.addEventListener.bind(window);
    const addSpy = vi
      .spyOn(window, 'addEventListener')
      .mockImplementation(
        (type: string, handler: EventListenerOrEventListenerObject, opts?: unknown) => {
          if (type === 'message') bridgeListener = handler as (e: MessageEvent) => unknown;
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          origAdd(type, handler as EventListener, opts as any);
        }
      );
    await import('../../../src/integrations/gpt/index');
    addSpy.mockRestore();

    expect(bridgeListener, 'bridge listener should be registered').toBeDefined();
    return bridgeListener!;
  }

  it('calls stopImmediatePropagation and fetches PBS Cache for a TS bid', async () => {
    const beaconSpy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true);
    const mockAd = '<div>Test Creative</div>';
    fetchStub.mockResolvedValue({
      ok: true,
      text: () => Promise.resolve(mockAd),
    } as Response);

    // Capture the bridge's 'message' listener at module-init time.
    let bridgeListener: ((e: MessageEvent) => unknown) | undefined;
    const origAdd = window.addEventListener.bind(window);
    const addSpy = vi
      .spyOn(window, 'addEventListener')
      .mockImplementation(
        (type: string, handler: EventListenerOrEventListenerObject, opts?: unknown) => {
          if (type === 'message') bridgeListener = handler as (e: MessageEvent) => unknown;
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          origAdd(type, handler as EventListener, opts as any);
        }
      );
    await import('../../../src/integrations/gpt/index');
    addSpy.mockRestore(); // Restore only addEventListener — fetchStub must stay stubbed

    expect(bridgeListener, 'bridge listener should be registered').toBeDefined();

    const stopSpy = vi.fn();
    const portMessages: string[] = [];
    const fakePort = { postMessage: (s: string) => portMessages.push(s) };
    const source = createTrustedSlotIframe();

    // Dispatch the fake event — bridge listener fires synchronously, then runs
    // fire-and-forget fetch().then() chains asynchronously.
    bridgeListener!(
      Object.assign(new Event('message'), {
        data: JSON.stringify({ message: 'Prebid Request', adId: 'test-cache-uuid' }),
        ports: [fakePort],
        source,
        stopImmediatePropagation: stopSpy,
      }) as unknown as MessageEvent
    );

    // Flush microtasks so the fetch mock resolves and .then chains fire.
    await new Promise<void>((resolve) => setTimeout(resolve, 50));

    expect(fetchStub).toHaveBeenCalledWith(
      'https://openads.example.com/cache?uuid=test-cache-uuid',
      // Carries an abort signal so a navigation can cancel a render belonging
      // to the route it is leaving.
      { mode: 'cors', signal: expect.any(AbortSignal) }
    );
    expect(stopSpy).toHaveBeenCalled();
    expect(portMessages).toHaveLength(1);
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const parsed = JSON.parse(portMessages[0]) as Record<string, any>;
    expect(parsed.message).toBe('Prebid Response');
    expect(parsed.adId).toBe('test-cache-uuid');
    expect(parsed.ad).toBe(mockAd);
    expect(beaconSpy).toHaveBeenCalledWith('https://ssp.example/win');
    expect(beaconSpy).toHaveBeenCalledWith('https://ssp.example/bill');
    expect(beaconSpy).toHaveBeenCalledTimes(2);

    // The PBS Cache branch must trace the same as the debug-adm branch —
    // regression coverage for the two recordBridgeRender call sites staying
    // in sync (this branch's stamp only lands after the async fetch settles).
    const record = (window as TestWindow).tsjs!.renders?.['homepage_header'];
    expect(record).toEqual(
      expect.objectContaining({
        slotId: 'homepage_header',
        path: 'ssat',
        rendered: true,
        injected: true,
        servedFrom: 'pbs-cache',
      })
    );

    bridgeListener!(
      Object.assign(new Event('message'), {
        data: JSON.stringify({ message: 'Prebid Request', adId: 'test-cache-uuid' }),
        ports: [fakePort],
        source,
        stopImmediatePropagation: stopSpy,
      }) as unknown as MessageEvent
    );
    await new Promise<void>((resolve) => setTimeout(resolve, 50));
    expect(beaconSpy).toHaveBeenCalledTimes(2);
    beaconSpy.mockRestore();
  });

  it('keeps GAM and bridge signals for both arrival orders on one record per impression', async () => {
    const source = createTrustedSlotIframe();
    let slotRenderListener: ((event: SlotRenderEvent) => void) | undefined;
    const gptSlot = {
      addService: vi.fn().mockReturnThis(),
      setTargeting: vi.fn().mockReturnThis(),
      clearTargeting: vi.fn().mockReturnThis(),
      getSlotElementId: vi.fn().mockReturnValue('div-header'),
      getTargeting: vi.fn().mockReturnValue([]),
    };
    const mockPubads = {
      enableSingleRequest: vi.fn(),
      getSlots: vi.fn().mockReturnValue([gptSlot]),
      refresh: vi.fn(),
      addEventListener: vi.fn((event: string, listener: (event: SlotRenderEvent) => void) => {
        if (event === 'slotRenderEnded') slotRenderListener = listener;
      }),
    };
    (window as TestWindow).googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: vi.fn().mockReturnValue(gptSlot),
      pubads: vi.fn().mockReturnValue(mockPubads),
      enableServices: vi.fn(),
    };
    const ts = (window as TestWindow).tsjs!;
    ts.bids.homepage_header = {
      hb_adid: 'debug-first',
      hb_bidder: 'mocktioneer',
    };
    const bridgeListener = await captureBridgeListener();
    ts.adInit!();

    const sendBridgeRequest = (adId: string): void => {
      bridgeListener(
        Object.assign(new Event('message'), {
          data: JSON.stringify({ message: 'Prebid Request', adId }),
          ports: [{ postMessage: vi.fn() }],
          source,
          stopImmediatePropagation: vi.fn(),
        }) as unknown as MessageEvent
      );
    };

    // GAM first, bridge second.
    slotRenderListener!({ isEmpty: false, slot: gptSlot });
    const firstRecord = ts.renders?.homepage_header;
    ts.bids.homepage_header = {
      ...ts.bids.homepage_header,
      adm: '<div>First creative</div>',
    };
    sendBridgeRequest('debug-first');
    expect(ts.renders?.homepage_header).toBe(firstRecord);
    expect(firstRecord).toEqual(
      expect.objectContaining({ count: 1, injected: true, servedFrom: 'debug-adm' })
    );
    expect(ts.renderLog).toHaveLength(1);

    // Bridge first, GAM second for the next impression.
    ts.bids.homepage_header = {
      hb_adid: 'debug-second',
      hb_bidder: 'mocktioneer',
      adm: '<iframe src="https://creative.example.com/second"></iframe>',
    };
    ts.adInit!();
    sendBridgeRequest('debug-second');
    const secondRecord = ts.renders?.homepage_header;
    slotRenderListener!({ isEmpty: false, slot: gptSlot });
    expect(ts.renders?.homepage_header).toBe(secondRecord);
    expect(secondRecord).toEqual(
      expect.objectContaining({ count: 2, injected: true, gamEmpty: false })
    );
    expect(ts.renderLog).toHaveLength(2);
  });

  it('fetches PBS Cache once when two same-adId messages race before the fetch resolves', async () => {
    // Concurrent render double-fire guard: two 'Prebid Request' messages for the
    // same adId can arrive before the first cache fetch settles. The in-flight
    // `renderingAdIds` gate must collapse them to a single fetch — the persistent
    // firedBeacons dedup only engages after a fetch resolves, so it cannot stop
    // the second fetch on its own. Deferring the fetch keeps both messages in the
    // window where only the in-flight gate can prevent the duplicate.
    const beaconSpy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true);
    const mockAd = '<div>Test Creative</div>';
    let resolveFetch: (value: Response) => void = () => {};
    fetchStub.mockReturnValue(
      new Promise<Response>((resolve) => {
        resolveFetch = resolve;
      })
    );

    const bridgeListener = await captureBridgeListener();

    const stopSpy = vi.fn();
    const portMessages: string[] = [];
    const fakePort = { postMessage: (s: string) => portMessages.push(s) };
    const source = createTrustedSlotIframe();

    const dispatch = (): unknown =>
      bridgeListener(
        Object.assign(new Event('message'), {
          data: JSON.stringify({ message: 'Prebid Request', adId: 'test-cache-uuid' }),
          ports: [fakePort],
          source,
          stopImmediatePropagation: stopSpy,
        }) as unknown as MessageEvent
      );

    // Both messages dispatched before the deferred fetch resolves.
    dispatch();
    dispatch();

    // The second message hit the in-flight gate — only one fetch launched.
    expect(fetchStub).toHaveBeenCalledTimes(1);

    // Resolve the single fetch and flush its .then chain.
    resolveFetch({ ok: true, text: () => Promise.resolve(mockAd) } as Response);
    await new Promise<void>((resolve) => setTimeout(resolve, 50));

    expect(fetchStub).toHaveBeenCalledTimes(1);
    expect(portMessages).toHaveLength(1);
    // A single render still fires both win and billing beacons exactly once.
    expect(beaconSpy).toHaveBeenCalledWith('https://ssp.example/win');
    expect(beaconSpy).toHaveBeenCalledWith('https://ssp.example/bill');
    expect(beaconSpy).toHaveBeenCalledTimes(2);
    beaconSpy.mockRestore();
  });

  it('drops a PBS Cache result when the live bid changed before fetch completion', async () => {
    let resolveFetch: (value: Response) => void = () => {};
    fetchStub.mockReturnValue(
      new Promise<Response>((resolve) => {
        resolveFetch = resolve;
      })
    );
    const ts = (window as TestWindow).tsjs!;
    ts.bids.homepage_header = {
      ...ts.bids.homepage_header,
      hb_auction_id: 'auction-1',
      hb_bid_id: 'bid-1',
    };
    const bridgeListener = await captureBridgeListener();
    const portMessages: string[] = [];
    const source = createTrustedSlotIframe();

    bridgeListener(
      Object.assign(new Event('message'), {
        data: JSON.stringify({ message: 'Prebid Request', adId: 'test-cache-uuid' }),
        ports: [{ postMessage: (message: string) => portMessages.push(message) }],
        source,
        stopImmediatePropagation: vi.fn(),
      }) as unknown as MessageEvent
    );

    // Same bridge ad ID and auction ID, but a different bid object/trace ID.
    // Comparing only hb_adid + hb_auction_id would incorrectly accept the old
    // creative and stamp it with the captured route's data.
    ts.bids.homepage_header = {
      ...ts.bids.homepage_header,
      hb_bid_id: 'bid-2',
      hb_adm_hash: 'new-creative-hash',
    };
    resolveFetch({ ok: true, text: () => Promise.resolve('<div>Old creative</div>') } as Response);
    await new Promise<void>((resolve) => setTimeout(resolve, 0));

    expect(portMessages).toHaveLength(0);
    expect(ts.renders?.homepage_header).toBeUndefined();
  });

  it('responds with adm without fetching PBS Cache when debug adm is available', async () => {
    const beaconSpy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true);
    const debugAdm = '<div>Debug Creative</div>';
    (window as TestWindow).tsjs = {
      bids: {
        homepage_header: {
          hb_adid: 'debug-adid',
          hb_bidder: 'mocktioneer',
          hb_pb: '0.20',
          nurl: 'https://debug.example/win',
          burl: 'https://debug.example/bill',
          adm: debugAdm,
        },
      },
      adSlots: [
        {
          id: 'homepage_header',
          formats: [[728, 90]] as [number, number][],
          gam_unit_path: '/a/b/c',
          div_id: 'div-header',
          targeting: {},
        },
      ],
    };

    let bridgeListener: ((e: MessageEvent) => unknown) | undefined;
    const origAdd = window.addEventListener.bind(window);
    const addSpy = vi
      .spyOn(window, 'addEventListener')
      .mockImplementation(
        (type: string, handler: EventListenerOrEventListenerObject, opts?: unknown) => {
          if (type === 'message') bridgeListener = handler as (e: MessageEvent) => unknown;
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          origAdd(type, handler as EventListener, opts as any);
        }
      );
    await import('../../../src/integrations/gpt/index');
    addSpy.mockRestore();

    expect(bridgeListener, 'bridge listener should be registered').toBeDefined();

    const stopSpy = vi.fn();
    const portMessages: string[] = [];
    const fakePort = { postMessage: (s: string) => portMessages.push(s) };
    const source = createTrustedSlotIframe();

    bridgeListener!(
      Object.assign(new Event('message'), {
        data: JSON.stringify({ message: 'Prebid Request', adId: 'debug-adid' }),
        ports: [fakePort],
        source,
        stopImmediatePropagation: stopSpy,
      }) as unknown as MessageEvent
    );

    await new Promise<void>((resolve) => setTimeout(resolve, 50));

    expect(fetchStub).not.toHaveBeenCalled();
    expect(stopSpy).toHaveBeenCalled();
    expect(portMessages).toHaveLength(1);
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const parsed = JSON.parse(portMessages[0]) as Record<string, any>;
    expect(parsed.message).toBe('Prebid Response');
    expect(parsed.adId).toBe('debug-adid');
    expect(parsed.ad).toBe(debugAdm);
    expect(parsed.width).toBe(728);
    expect(parsed.height).toBe(90);
    expect(beaconSpy).toHaveBeenCalledWith('https://debug.example/win');
    expect(beaconSpy).toHaveBeenCalledWith('https://debug.example/bill');
    expect(beaconSpy).toHaveBeenCalledTimes(2);
    beaconSpy.mockRestore();

    // Serving debug adm through the Universal Creative bridge is a confirmed
    // TS placement — same as the PBS Cache branch — and must not be silently
    // untraced just because no cache fetch was involved.
    const record = (window as TestWindow).tsjs!.renders?.['homepage_header'];
    expect(record).toEqual(
      expect.objectContaining({
        slotId: 'homepage_header',
        path: 'ssat',
        rendered: true,
        injected: true,
        bidder: 'mocktioneer',
        servedFrom: 'debug-adm',
      })
    );
  });

  it('falls back to keepalive fetch when sendBeacon is unavailable', async () => {
    const originalSendBeacon = navigator.sendBeacon;
    Object.defineProperty(navigator, 'sendBeacon', {
      value: undefined,
      writable: true,
      configurable: true,
    });

    try {
      (window as TestWindow).tsjs.bids.homepage_header = {
        hb_adid: 'debug-no-beacon',
        hb_bidder: 'mocktioneer',
        hb_pb: '0.20',
        nurl: 'https://debug.example/win',
        burl: 'https://debug.example/bill',
        adm: '<div>Debug Creative</div>',
      };

      const bridgeListener = await captureBridgeListener();
      const portMessages: string[] = [];
      const fakePort = { postMessage: (s: string) => portMessages.push(s) };
      const source = createTrustedSlotIframe();

      expect(() =>
        bridgeListener(
          Object.assign(new Event('message'), {
            data: JSON.stringify({ message: 'Prebid Request', adId: 'debug-no-beacon' }),
            ports: [fakePort],
            source,
            stopImmediatePropagation: vi.fn(),
          }) as unknown as MessageEvent
        )
      ).not.toThrow();

      expect(fetchStub).toHaveBeenCalledWith('https://debug.example/win', {
        method: 'POST',
        keepalive: true,
        mode: 'no-cors',
      });
      expect(fetchStub).toHaveBeenCalledWith('https://debug.example/bill', {
        method: 'POST',
        keepalive: true,
        mode: 'no-cors',
      });
    } finally {
      Object.defineProperty(navigator, 'sendBeacon', {
        value: originalSendBeacon,
        writable: true,
        configurable: true,
      });
    }
  });

  it('falls back to keepalive fetch when sendBeacon rejects the payload', async () => {
    const beaconSpy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(false);
    (window as TestWindow).tsjs.bids.homepage_header = {
      hb_adid: 'debug-rejected-beacon',
      hb_bidder: 'mocktioneer',
      hb_pb: '0.20',
      nurl: 'https://debug.example/win',
      burl: 'https://debug.example/bill',
      adm: '<div>Debug Creative</div>',
    };

    const bridgeListener = await captureBridgeListener();
    const portMessages: string[] = [];
    const fakePort = { postMessage: (s: string) => portMessages.push(s) };
    const source = createTrustedSlotIframe();
    const event = Object.assign(new Event('message'), {
      data: JSON.stringify({ message: 'Prebid Request', adId: 'debug-rejected-beacon' }),
      ports: [fakePort],
      source,
      stopImmediatePropagation: vi.fn(),
    }) as unknown as MessageEvent;

    bridgeListener(event);

    expect(beaconSpy).toHaveBeenCalledWith('https://debug.example/win');
    expect(beaconSpy).toHaveBeenCalledWith('https://debug.example/bill');
    expect(fetchStub).toHaveBeenCalledWith('https://debug.example/win', {
      method: 'POST',
      keepalive: true,
      mode: 'no-cors',
    });
    expect(fetchStub).toHaveBeenCalledWith('https://debug.example/bill', {
      method: 'POST',
      keepalive: true,
      mode: 'no-cors',
    });

    bridgeListener(event);
    expect(fetchStub).toHaveBeenCalledTimes(2);
    beaconSpy.mockRestore();
  });

  it('ignores message when adId does not match any TS bid', async () => {
    await import('../../../src/integrations/gpt/index');
    fetchStub.mockResolvedValue({ ok: true, text: () => Promise.resolve('') } as Response);

    window.dispatchEvent(
      new MessageEvent('message', {
        data: JSON.stringify({ message: 'Prebid Request', adId: 'unknown-id' }),
        ports: [],
      })
    );

    await new Promise<void>((r) => setTimeout(r, 100));
    expect(fetchStub).not.toHaveBeenCalled();
  });

  it('ignores matching adId messages from outside configured slot iframes', async () => {
    await import('../../../src/integrations/gpt/index');
    fetchStub.mockResolvedValue({ ok: true, text: () => Promise.resolve('') } as Response);

    const foreignIframe = document.createElement('iframe');
    document.body.appendChild(foreignIframe);
    const portMessages: string[] = [];
    const fakePort = { postMessage: (s: string) => portMessages.push(s) };
    const stopSpy = vi.fn();

    window.dispatchEvent(
      new MessageEvent('message', {
        data: JSON.stringify({ message: 'Prebid Request', adId: 'test-cache-uuid' }),
        ports: [fakePort as MessagePort],
        source: foreignIframe.contentWindow,
      })
    );

    await new Promise<void>((r) => setTimeout(r, 50));
    expect(fetchStub).not.toHaveBeenCalled();
    expect(stopSpy).not.toHaveBeenCalled();
    expect(portMessages).toHaveLength(0);
    foreignIframe.remove();
  });

  it('ignores a request whose source slot does not own the resolved adId', async () => {
    // Two configured slots; slot A's iframe requests slot B's hb_adid. The
    // bridge must not return slot B's creative or fire slot B's beacons.
    (window as TestWindow).tsjs.bids.homepage_footer = {
      hb_adid: 'footer-uuid',
      hb_bidder: 'kargo',
      hb_pb: '2.00',
      hb_cache_host: 'openads.example.com',
      hb_cache_path: '/cache',
      nurl: 'https://ssp.example/footer-win',
      burl: 'https://ssp.example/footer-bill',
    };
    (window as TestWindow).tsjs.adSlots.push({
      id: 'homepage_footer',
      formats: [[300, 250]] as [number, number][],
      gam_unit_path: '/a/b/footer',
      div_id: 'div-footer',
      targeting: {},
    });

    const beaconSpy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true);
    await import('../../../src/integrations/gpt/index');
    fetchStub.mockResolvedValue({ ok: true, text: () => Promise.resolve('') } as Response);

    // Source iframe lives under slot A (div-header).
    const source = createTrustedSlotIframe();
    const portMessages: string[] = [];
    const fakePort = { postMessage: (s: string) => portMessages.push(s) };

    window.dispatchEvent(
      new MessageEvent('message', {
        // adId belongs to slot B (homepage_footer), not slot A's iframe.
        data: JSON.stringify({ message: 'Prebid Request', adId: 'footer-uuid' }),
        ports: [fakePort as MessagePort],
        source,
      })
    );

    await new Promise<void>((r) => setTimeout(r, 50));
    expect(fetchStub).not.toHaveBeenCalled();
    expect(portMessages).toHaveLength(0);
    expect(beaconSpy).not.toHaveBeenCalled();
    document.getElementById('div-footer')?.remove();
  });

  it('ignores non-Prebid messages', async () => {
    await import('../../../src/integrations/gpt/index');
    window.dispatchEvent(
      new MessageEvent('message', { data: JSON.stringify({ message: 'Other' }) })
    );
    await new Promise<void>((r) => setTimeout(r, 50));
    expect(fetchStub).not.toHaveBeenCalled();
  });
});

describe('orphaned TS slot recovery', () => {
  type TestWin = Window & {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    tsjs?: any;
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    googletag?: any;
  };

  beforeEach(() => {
    vi.resetModules();
    const tw = window as TestWin;
    delete tw.tsjs;
    delete tw.googletag;
    document.body.innerHTML = '';
  });

  afterEach(() => {
    document.body.innerHTML = '';
  });

  function slotStub(elementId: string) {
    return {
      addService: vi.fn().mockReturnThis(),
      setTargeting: vi.fn().mockReturnThis(),
      clearTargeting: vi.fn().mockReturnThis(),
      getSlotElementId: vi.fn().mockReturnValue(elementId),
      getTargeting: vi.fn().mockReturnValue([]),
    };
  }

  it('reports slots whose bound element left the document', async () => {
    const { orphanedTsSlots } = await import('../../../src/integrations/gpt/index');
    document.body.innerHTML = '<div id="live-div"></div>';

    const live = slotStub('live-div');
    const orphan = slotStub('ad-header-0-_R_ssr_');
    const ts = { prevGptSlots: [live, orphan] };

    const orphans = orphanedTsSlots(ts as never);
    expect(orphans).toHaveLength(1);
    expect(orphans[0].getSlotElementId()).toBe('ad-header-0-_R_ssr_');
  });

  it('returns nothing when every TS slot still has its element', async () => {
    const { orphanedTsSlots } = await import('../../../src/integrations/gpt/index');
    document.body.innerHTML = '<div id="a"></div><div id="b"></div>';
    const ts = { prevGptSlots: [slotStub('a'), slotStub('b')] };
    expect(orphanedTsSlots(ts as never)).toHaveLength(0);
  });

  it('re-runs adInit after a re-render swaps the ad div', async () => {
    // SSR div that hydration will replace.
    document.body.innerHTML = '<div id="ad-header-0-_R_ssr_"></div>';

    const definedSlots: string[] = [];
    const mockPubads = {
      enableSingleRequest: vi.fn(),
      getSlots: vi.fn().mockReturnValue([]),
      refresh: vi.fn(),
      addEventListener: vi.fn(),
    };
    const tw = window as TestWin;
    tw.googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: vi.fn((_path: string, _sizes: unknown, divId: string) => {
        definedSlots.push(divId);
        return slotStub(divId);
      }),
      pubads: vi.fn().mockReturnValue(mockPubads),
      enableServices: vi.fn(),
      display: vi.fn(),
      destroySlots: vi.fn(),
    };
    tw.tsjs = {
      adSlots: [
        {
          id: 'ad-header-0',
          gam_unit_path: '/123/header',
          div_id: 'ad-header-0',
          formats: [[728, 90]],
          targeting: {},
        },
      ],
      bids: {},
    };

    const { installTsAdInit } = await import('../../../src/integrations/gpt/index');
    installTsAdInit();
    tw.tsjs.adInit();

    expect(definedSlots).toEqual(['ad-header-0-_R_ssr_']);

    // Hydration: React replaces the SSR div with a client-id div.
    document.body.innerHTML = '<div id="ad-header-0-_r_1_"></div>';

    // The MutationObserver is debounced; give it room to fire.
    await new Promise<void>((r) => setTimeout(r, 600));

    // adInit re-ran and bound to the live div instead of the dead one.
    expect(definedSlots).toContain('ad-header-0-_r_1_');
  });
});
