import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';

// Track every 'message' EventListener added to window across the entire test
// file.  This lets the installTsRenderBridge suite remove all accumulated
// handlers (registered by each vi.resetModules() + import('./index') in the
// installTsAdInit suite) before dispatching its own events.
const allMessageHandlers: EventListener[] = [];
const _origWindowAddEventListener = window.addEventListener.bind(window);
// eslint-disable-next-line @typescript-eslint/no-explicit-any
(window as any).addEventListener = function (
  type: string,
  handler: EventListenerOrEventListenerObject,
  options?: unknown
) {
  if (type === 'message') {
    allMessageHandlers.push(handler as EventListener);
  }
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return _origWindowAddEventListener(type, handler as EventListener, options as any);
};

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

    const { installTsAdInit } = await import('./index');
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

    const { installTsAdInit } = await import('./index');
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

  it('fires both nurl and burl via sendBeacon on slotRenderEnded when our bid won', async () => {
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

    const { installTsAdInit } = await import('./index');
    installTsAdInit();
    (window as TestWindow).tsjs!.adInit!();

    expect(capturedListener).toBeDefined();
    capturedListener!({ isEmpty: false, slot: mockSlot });

    expect(beaconSpy).toHaveBeenCalledWith('https://ssp/win');
    expect(beaconSpy).toHaveBeenCalledWith('https://ssp/bill');
    beaconSpy.mockRestore();
  });

  it('does not fire beacons when a rendered bid has no hb_adid confirmation', async () => {
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

    const { installTsAdInit } = await import('./index');
    installTsAdInit();
    (window as TestWindow).tsjs!.adInit!();

    expect(capturedListener).toBeDefined();
    capturedListener!({ isEmpty: false, slot: mockSlot });

    expect(beaconSpy).not.toHaveBeenCalled();

    capturedListener!({ isEmpty: true, slot: mockSlot });
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

    const { installTsAdInit } = await import('./index');
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

    const { installTsAdInit } = await import('./index');
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

    const { installTsAdInit } = await import('./index');
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

    const { installTsAdInit } = await import('./index');
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

    const { installTsAdInit } = await import('./index');
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

    const { installTsAdInit } = await import('./index');
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

    (window as TestWindow).tsjs = {
      bids: {
        homepage_header: {
          hb_adid: 'test-cache-uuid',
          hb_bidder: 'kargo',
          hb_pb: '1.50',
          hb_cache_host: 'openads.example.com',
          hb_cache_path: '/cache',
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

  it('calls stopImmediatePropagation and fetches PBS Cache for a TS bid', async () => {
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
    await import('./index');
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
  });

  it('responds with adm without fetching PBS Cache when debug adm is available', async () => {
    const debugAdm = '<div>Debug Creative</div>';
    (window as TestWindow).tsjs = {
      bids: {
        homepage_header: {
          hb_adid: 'debug-adid',
          hb_bidder: 'mocktioneer',
          hb_pb: '0.20',
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
    await import('./index');
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
  });

  it('ignores message when adId does not match any TS bid', async () => {
    await import('./index');
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
    await import('./index');
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

  it('ignores non-Prebid messages', async () => {
    await import('./index');
    window.dispatchEvent(
      new MessageEvent('message', { data: JSON.stringify({ message: 'Other' }) })
    );
    await new Promise<void>((r) => setTimeout(r, 50));
    expect(fetchStub).not.toHaveBeenCalled();
  });
});
