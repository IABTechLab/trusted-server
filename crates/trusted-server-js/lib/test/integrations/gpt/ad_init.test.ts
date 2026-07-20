import { describe, it, expect, vi, beforeEach, afterEach, afterAll } from 'vitest';
import envelope from '../../fixtures/aps-renderer-v1.json';

function apsRenderer() {
  const bid = envelope.seatbid[0].bid[0];
  return {
    type: 'aps' as const,
    version: 1 as const,
    accountId: 'example-account-id',
    bidId: bid.id,
    creativeId: 'fictional-creative-id',
    tagType: 'iframe' as const,
    creativeUrl: bid.ext.creativeurl,
    aaxResponse: btoa(JSON.stringify(envelope)),
    width: bid.w,
    height: bid.h,
  };
}

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

  it('keeps the GAM path when a bid carries inline adm (adInit does not inject)', async () => {
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
          adm: '<div>Inline creative</div>',
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

  // Helper: full adInit setup for a single slot whose bid carries an iframe adm.
  // `debugBid` toggles the per-bid `debug_bid` field that gates the testing bypass.
  async function fireSlotRenderWithAdm(debugBid: boolean): Promise<HTMLIFrameElement> {
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
          adm: '<iframe src="https://cdn.example/creative.html"></iframe>',
          ...(debugBid ? { debug_bid: { slot_id: 'atf_sidebar_ad' } } : {}),
        },
      },
    };

    const { installTsAdInit } = await import('../../../src/integrations/gpt/index');
    installTsAdInit();
    (window as TestWindow).tsjs!.adInit!();

    // A pre-existing GAM iframe; the bypass, if it runs, rewrites its src.
    const slotEl = document.getElementById('div-atf-sidebar')!;
    const gamIframe = document.createElement('iframe');
    gamIframe.src = 'about:blank';
    slotEl.appendChild(gamIframe);

    expect(capturedListener).toBeDefined();
    capturedListener!({ isEmpty: false, slot: mockSlot });
    return gamIframe;
  }

  it('does not run the GAM-replace bypass without debug_bid (production)', async () => {
    const gamIframe = await fireSlotRenderWithAdm(false);
    // No debug_bid ⇒ testing bypass is off; the render bridge handles the creative
    // and GAM stays in the loop, so the GAM iframe src is untouched.
    expect(gamIframe.src).toBe('about:blank');
  });

  it('runs the GAM-replace bypass when debug_bid is present (testing)', async () => {
    const gamIframe = await fireSlotRenderWithAdm(true);
    // debug_bid present ⇒ inject_adm_for_testing on ⇒ direct GAM replace fires,
    // rewriting the iframe to the creative URL from the adm.
    expect(gamIframe.src).toBe('https://cdn.example/creative.html');
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

  it('does not call native apstag for a Trusted Server APS renderer winner', async () => {
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
        atf_sidebar_ad: {
          hb_pb: '1.50',
          hb_bidder: 'aps',
          hb_adid: envelope.seatbid[0].bid[0].id,
          renderer: apsRenderer(),
        },
      },
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
    } as any;

    const { installTsAdInit } = await import('../../../src/integrations/gpt/index');
    installTsAdInit();
    (window as TestWindow).tsjs!.adInit!();

    expect(setDisplayBidsSpy).not.toHaveBeenCalled();
    expect((window as TestWindow).apstag).toEqual({ setDisplayBids: setDisplayBidsSpy });

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

  it('serves one exact APS dynamic-renderer response without cache fetches or beacons', async () => {
    const renderer = apsRenderer();
    (window as TestWindow).tsjs.bids.homepage_header = {
      hb_adid: renderer.bidId,
      hb_bidder: 'aps',
      hb_pb: '1.23',
      renderer,
      // These must not be used even if unexpected legacy fields coexist.
      nurl: 'https://notify.example/win',
      burl: 'https://notify.example/bill',
      hb_cache_host: 'cache.example.com',
      hb_cache_path: '/cache',
    };

    const beaconSpy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true);
    const bridgeListener = await captureBridgeListener();
    const source = createTrustedSlotIframe();
    const stopSpy = vi.fn();
    const portMessages: string[] = [];
    const fakePort = { postMessage: (message: string) => portMessages.push(message) };
    const event = Object.assign(new Event('message'), {
      data: JSON.stringify({ message: 'Prebid Request', adId: renderer.bidId }),
      ports: [fakePort],
      source,
      stopImmediatePropagation: stopSpy,
    }) as unknown as MessageEvent;

    bridgeListener(event);
    bridgeListener(event);

    expect(stopSpy).toHaveBeenCalledTimes(2);
    expect(fetchStub).not.toHaveBeenCalled();
    expect(beaconSpy).not.toHaveBeenCalled();
    expect(portMessages).toHaveLength(1);
    const response = JSON.parse(portMessages[0]) as Record<string, unknown>;
    expect(Object.keys(response).sort()).toEqual(
      [
        'adId',
        'apsRenderer',
        'height',
        'message',
        'renderer',
        'rendererUrl',
        'rendererVersion',
        'width',
      ].sort()
    );
    expect(response).toEqual({
      message: 'Prebid Response',
      adId: renderer.bidId,
      renderer: expect.stringContaining('window.render=function'),
      rendererVersion: 4,
      rendererUrl: new URL('/integrations/aps/renderer', window.location.origin).href,
      apsRenderer: renderer,
      width: 300,
      height: 250,
    });
    expect(String(response.renderer)).not.toContain(renderer.accountId);
    expect(String(response.renderer)).not.toContain(renderer.aaxResponse);

    // Universal Creative's dynamic-renderer path evaluates the returned static
    // source and calls window.render(response, helper, targetWindow). Consume
    // the exact bridge response through that deployed protocol shape.
    const dynamicWindow = window as unknown as {
      render?: (data: Record<string, unknown>, helper: unknown, target: Window) => Promise<void>;
    };
    window.eval(String(response.renderer));
    try {
      const rendered = dynamicWindow.render!(response, undefined, window);
      const outerFrame = document.querySelector<HTMLIFrameElement>(
        'iframe[src*="/integrations/aps/renderer#tsaps="]'
      )!;
      expect(outerFrame).not.toBeNull();
      expect(outerFrame.getAttribute('sandbox')).not.toContain('allow-same-origin');

      const rendererPost = vi.spyOn(outerFrame.contentWindow!, 'postMessage');
      outerFrame.dispatchEvent(new Event('load'));
      const sent = rendererPost.mock.calls[0][0] as { nonce: string };
      window.dispatchEvent(
        new MessageEvent('message', {
          data: { message: 'trusted-server/aps/renderer-ready', nonce: sent.nonce },
          source: outerFrame.contentWindow,
        })
      );
      await expect(rendered).resolves.toBeUndefined();
      outerFrame.remove();
    } finally {
      delete dynamicWindow.render;
    }
    beaconSpy.mockRestore();
  });

  it('serves a registered Prebid APS renderer when its generated ad ID differs from the APS bid ID', async () => {
    const renderer = apsRenderer();
    const prebidAdId = 'prebid-generated-ad-id';
    const markWinner = vi.fn();
    const markRendered = vi.fn();
    (window as TestWindow).tsjs.apsPrebidRenderers = {
      [prebidAdId]: {
        adUnitCode: 'div-header',
        renderer,
        registeredAt: Date.now(),
        expiresAt: Date.now() + 60_000,
        markWinner,
        markRendered,
      },
    };

    const bridgeListener = await captureBridgeListener();
    const source = createTrustedSlotIframe();
    const stopSpy = vi.fn();
    const portMessages: string[] = [];
    const event = Object.assign(new Event('message'), {
      data: JSON.stringify({ message: 'Prebid Request', adId: prebidAdId }),
      ports: [{ postMessage: (message: string) => portMessages.push(message) }],
      source,
      stopImmediatePropagation: stopSpy,
    }) as unknown as MessageEvent;

    bridgeListener(event);
    bridgeListener(event);

    expect(stopSpy).toHaveBeenCalledTimes(2);
    expect(portMessages).toHaveLength(1);
    expect(markWinner).toHaveBeenCalledTimes(1);
    expect(markRendered).toHaveBeenCalledTimes(1);
    expect(JSON.parse(portMessages[0])).toEqual(
      expect.objectContaining({
        message: 'Prebid Response',
        adId: prebidAdId,
        apsRenderer: renderer,
        width: renderer.width,
        height: renderer.height,
      })
    );
    expect(renderer.bidId).not.toBe(prebidAdId);
    expect((window as TestWindow).tsjs.apsPrebidRenderers[prebidAdId]).toBeUndefined();
    expect(fetchStub).not.toHaveBeenCalled();
  });

  it('does not expose a registered Prebid APS renderer to another slot iframe', async () => {
    const renderer = apsRenderer();
    const prebidAdId = 'prebid-generated-ad-id';
    (window as TestWindow).tsjs.apsPrebidRenderers = {
      [prebidAdId]: {
        adUnitCode: 'div-header',
        renderer,
        registeredAt: Date.now(),
        expiresAt: Date.now() + 60_000,
        markWinner: vi.fn(),
        markRendered: vi.fn(),
      },
    };

    const footer = document.createElement('div');
    footer.id = 'div-footer';
    const foreignIframe = document.createElement('iframe');
    footer.appendChild(foreignIframe);
    document.body.appendChild(footer);

    const bridgeListener = await captureBridgeListener();
    const stopSpy = vi.fn();
    const portMessages: string[] = [];
    bridgeListener(
      Object.assign(new Event('message'), {
        data: JSON.stringify({ message: 'Prebid Request', adId: prebidAdId }),
        ports: [{ postMessage: (message: string) => portMessages.push(message) }],
        source: foreignIframe.contentWindow,
        stopImmediatePropagation: stopSpy,
      }) as unknown as MessageEvent
    );

    expect(stopSpy).not.toHaveBeenCalled();
    expect(portMessages).toEqual([]);
    expect((window as TestWindow).tsjs.apsPrebidRenderers[prebidAdId]).toBeDefined();
    footer.remove();
  });

  it('drops an expired Prebid APS renderer without claiming the creative request', async () => {
    const prebidAdId = 'expired-prebid-ad-id';
    (window as TestWindow).tsjs.apsPrebidRenderers = {
      [prebidAdId]: {
        adUnitCode: 'div-header',
        renderer: apsRenderer(),
        registeredAt: Date.now() - 61_000,
        expiresAt: Date.now() - 1_000,
        markWinner: vi.fn(),
        markRendered: vi.fn(),
      },
    };

    const bridgeListener = await captureBridgeListener();
    const source = createTrustedSlotIframe();
    const stopSpy = vi.fn();
    const portMessages: string[] = [];
    bridgeListener(
      Object.assign(new Event('message'), {
        data: JSON.stringify({ message: 'Prebid Request', adId: prebidAdId }),
        ports: [{ postMessage: (message: string) => portMessages.push(message) }],
        source,
        stopImmediatePropagation: stopSpy,
      }) as unknown as MessageEvent
    );

    expect(stopSpy).not.toHaveBeenCalled();
    expect(portMessages).toEqual([]);
    expect((window as TestWindow).tsjs.apsPrebidRenderers[prebidAdId]).toBeUndefined();
  });

  it('validates APS data before claiming the Prebid request', async () => {
    const renderer = { ...apsRenderer(), aaxResponse: 'invalid' };
    (window as TestWindow).tsjs.bids.homepage_header = {
      hb_adid: renderer.bidId,
      hb_bidder: 'aps',
      renderer,
    };

    const bridgeListener = await captureBridgeListener();
    const source = createTrustedSlotIframe();
    const stopSpy = vi.fn();
    const portMessages: string[] = [];
    bridgeListener(
      Object.assign(new Event('message'), {
        data: JSON.stringify({ message: 'Prebid Request', adId: renderer.bidId }),
        ports: [{ postMessage: (message: string) => portMessages.push(message) }],
        source,
        stopImmediatePropagation: stopSpy,
      }) as unknown as MessageEvent
    );

    expect(stopSpy).not.toHaveBeenCalled();
    expect(portMessages).toEqual([]);
    expect(fetchStub).not.toHaveBeenCalled();
  });

  it('ignores an APS ad ID requested by another configured slot', async () => {
    const renderer = apsRenderer();
    (window as TestWindow).tsjs.bids.homepage_header = {
      hb_adid: renderer.bidId,
      hb_bidder: 'aps',
      renderer,
    };
    (window as TestWindow).tsjs.adSlots.push({
      id: 'homepage_footer',
      formats: [[300, 250]],
      gam_unit_path: '/a/b/footer',
      div_id: 'div-footer',
      targeting: {},
    });
    const footer = document.createElement('div');
    footer.id = 'div-footer';
    const foreignIframe = document.createElement('iframe');
    footer.appendChild(foreignIframe);
    document.body.appendChild(footer);

    const bridgeListener = await captureBridgeListener();
    const stopSpy = vi.fn();
    const portMessages: string[] = [];
    bridgeListener(
      Object.assign(new Event('message'), {
        data: JSON.stringify({ message: 'Prebid Request', adId: renderer.bidId }),
        ports: [{ postMessage: (message: string) => portMessages.push(message) }],
        source: foreignIframe.contentWindow,
        stopImmediatePropagation: stopSpy,
      }) as unknown as MessageEvent
    );

    expect(stopSpy).not.toHaveBeenCalled();
    expect(portMessages).toEqual([]);
    expect(fetchStub).not.toHaveBeenCalled();
    footer.remove();
  });

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
      { mode: 'cors' }
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

  it('serves inline adm without fetching PBS Cache even when cache coords are present', async () => {
    const beaconSpy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true);
    const inlineAdm = '<div>Inline Creative</div>';
    (window as TestWindow).tsjs = {
      bids: {
        homepage_header: {
          hb_adid: 'debug-adid',
          hb_bidder: 'mocktioneer',
          hb_pb: '0.20',
          // Production shape: cache coordinates ARE present, but the bridge must
          // prefer the local inline adm and skip the PBS Cache fetch.
          hb_cache_host: 'cache.example.com',
          hb_cache_path: '/pbc/v1/cache',
          nurl: 'https://debug.example/win',
          burl: 'https://debug.example/bill',
          adm: inlineAdm,
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
    expect(parsed.ad).toBe(inlineAdm);
    expect(parsed.width).toBe(728);
    expect(parsed.height).toBe(90);
    expect(beaconSpy).toHaveBeenCalledWith('https://debug.example/win');
    expect(beaconSpy).toHaveBeenCalledWith('https://debug.example/bill');
    expect(beaconSpy).toHaveBeenCalledTimes(2);
    beaconSpy.mockRestore();
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
        ports: [fakePort as unknown as MessagePort],
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
        ports: [fakePort as unknown as MessagePort],
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
