import { describe, it, expect, vi, beforeEach } from 'vitest';

interface SlotRenderEvent {
  isEmpty: boolean;
  slot: {
    getSlotElementId(): string;
    getTargeting(key: string): string[];
  };
}

type TsNamespace = {
  adSlots?: unknown;
  bids?: unknown;
  adInit?: () => void;
  prevGptSlots?: unknown;
  servicesEnabled?: boolean;
  spaHookInstalled?: boolean;
  divToSlotId?: Record<string, string>;
};

type TestWindow = Window & {
  googletag?: unknown;
  _ts?: TsNamespace;
};

describe('installTsAdInit', () => {
  beforeEach(() => {
    vi.resetModules();
    const tw = window as TestWindow;
    delete tw._ts;
    // jsdom does not implement navigator.sendBeacon; polyfill it for tests
    if (!('sendBeacon' in navigator)) {
      Object.defineProperty(navigator, 'sendBeacon', {
        value: vi.fn().mockReturnValue(true),
        writable: true,
        configurable: true,
      });
    }
  });

  it('reads window._ts.bids synchronously and applies bid targeting before refresh', async () => {
    const mockSlot = {
      addService: vi.fn().mockReturnThis(),
      setTargeting: vi.fn().mockReturnThis(),
      getSlotElementId: vi.fn().mockReturnValue('div-atf-sidebar'),
      getTargeting: vi.fn().mockReturnValue(['abc']),
    };
    const mockPubads = {
      enableSingleRequest: vi.fn(),
      addEventListener: vi.fn(),
      refresh: vi.fn(),
    };
    (window as TestWindow).googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: vi.fn().mockReturnValue(mockSlot),
      pubads: vi.fn().mockReturnValue(mockPubads),
      enableServices: vi.fn(),
    };
    (window as TestWindow)._ts = {
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
          hb_adid: 'abc',
          nurl: 'https://ssp/win',
          burl: 'https://ssp/bill',
        },
      },
    };

    const fetchSpy = vi.spyOn(global, 'fetch');

    const { installTsAdInit } = await import('./index');
    installTsAdInit();
    (window as TestWindow)._ts!.adInit!();

    expect(fetchSpy).not.toHaveBeenCalled();
    expect(mockSlot.setTargeting).toHaveBeenCalledWith('hb_pb', '1.00');
    expect(mockSlot.setTargeting).toHaveBeenCalledWith('hb_bidder', 'kargo');
    expect(mockSlot.setTargeting).toHaveBeenCalledWith('ts_initial', '1');
    expect(mockPubads.refresh).toHaveBeenCalled();

    fetchSpy.mockRestore();
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
    (window as TestWindow)._ts = {
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
    (window as TestWindow)._ts!.adInit!();

    expect(capturedListener).toBeDefined();
    capturedListener!({ isEmpty: false, slot: mockSlot });

    expect(beaconSpy).toHaveBeenCalledWith('https://ssp/win');
    expect(beaconSpy).toHaveBeenCalledWith('https://ssp/bill');
    beaconSpy.mockRestore();
  });

  it('fires beacons for APS bid (no hb_adid) when ad renders in our slot', async () => {
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
    (window as TestWindow)._ts = {
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
    (window as TestWindow)._ts!.adInit!();

    expect(capturedListener).toBeDefined();
    capturedListener!({ isEmpty: false, slot: mockSlot });

    expect(beaconSpy).toHaveBeenCalledWith('https://aps/win');
    expect(beaconSpy).toHaveBeenCalledWith('https://aps/bill');

    beaconSpy.mockClear();
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
    (window as TestWindow)._ts = {
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
    (window as TestWindow)._ts!.adInit!();
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
    (window as TestWindow)._ts = {
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
    (window as TestWindow)._ts!.adInit!();

    capturedListener!({ isEmpty: false, slot: arenaSlot });

    expect(beaconSpy).not.toHaveBeenCalled();
    beaconSpy.mockRestore();
  });

  it('calls refresh even when _ts.bids is empty (graceful fallback)', async () => {
    const mockPubads = {
      enableSingleRequest: vi.fn(),
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
    (window as TestWindow)._ts = {
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
    (window as TestWindow)._ts!.adInit!();

    expect(mockPubads.refresh).toHaveBeenCalled();
  });
});
