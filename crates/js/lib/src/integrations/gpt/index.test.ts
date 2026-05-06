import { describe, it, expect, vi, beforeEach } from 'vitest';

describe('installTsAdInit', () => {
  beforeEach(() => {
    vi.resetModules();
    delete (window as any).__ts_ad_slots;
    delete (window as any).__ts_bids;
    delete (window as any).__tsAdInit;
    // jsdom does not implement navigator.sendBeacon; polyfill it for tests
    if (!('sendBeacon' in navigator)) {
      Object.defineProperty(navigator, 'sendBeacon', {
        value: vi.fn().mockReturnValue(true),
        writable: true,
        configurable: true,
      });
    }
  });

  it('reads window.__ts_bids synchronously and applies bid targeting before refresh', async () => {
    const mockSlot = {
      addService: vi.fn().mockReturnThis(),
      setTargeting: vi.fn().mockReturnThis(),
      getSlotElementId: vi.fn().mockReturnValue('atf'),
      getTargeting: vi.fn().mockReturnValue(['abc']),
    };
    const mockPubads = {
      enableSingleRequest: vi.fn(),
      addEventListener: vi.fn(),
      refresh: vi.fn(),
    };
    (window as any).googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: vi.fn().mockReturnValue(mockSlot),
      pubads: vi.fn().mockReturnValue(mockPubads),
      enableServices: vi.fn(),
    };
    (window as any).__ts_ad_slots = [
      {
        id: 'atf',
        gam_unit_path: '/123/atf',
        div_id: 'atf',
        formats: [[300, 250]],
        targeting: { pos: 'atf' },
      },
    ];
    (window as any).__ts_bids = {
      atf: {
        hb_pb: '1.00',
        hb_bidder: 'kargo',
        hb_adid: 'abc',
        nurl: 'https://ssp/win',
        burl: 'https://ssp/bill',
      },
    };

    const fetchSpy = vi.spyOn(global, 'fetch');

    const { installTsAdInit } = await import('./index');
    installTsAdInit();
    (window as any).__tsAdInit();

    expect(fetchSpy).not.toHaveBeenCalled();
    expect(mockSlot.setTargeting).toHaveBeenCalledWith('hb_pb', '1.00');
    expect(mockSlot.setTargeting).toHaveBeenCalledWith('hb_bidder', 'kargo');
    expect(mockSlot.setTargeting).toHaveBeenCalledWith('ts_initial', '1');
    expect(mockPubads.refresh).toHaveBeenCalled();

    fetchSpy.mockRestore();
  });

  it('fires both nurl and burl via sendBeacon on slotRenderEnded when our bid won', async () => {
    const beaconSpy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true);
    let capturedListener: ((e: any) => void) | undefined;

    const mockSlot = {
      addService: vi.fn().mockReturnThis(),
      setTargeting: vi.fn().mockReturnThis(),
      getSlotElementId: vi.fn().mockReturnValue('atf'),
      getTargeting: vi.fn().mockReturnValue(['abc']),
    };
    const mockPubads = {
      enableSingleRequest: vi.fn(),
      refresh: vi.fn(),
      addEventListener: vi.fn((event: string, fn: (e: any) => void) => {
        if (event === 'slotRenderEnded') capturedListener = fn;
      }),
    };
    (window as any).googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: vi.fn().mockReturnValue(mockSlot),
      pubads: vi.fn().mockReturnValue(mockPubads),
      enableServices: vi.fn(),
    };
    (window as any).__ts_ad_slots = [
      {
        id: 'atf',
        gam_unit_path: '/123/atf',
        div_id: 'atf',
        formats: [[300, 250]],
        targeting: {},
      },
    ];
    (window as any).__ts_bids = {
      atf: {
        hb_pb: '1.00',
        hb_bidder: 'kargo',
        hb_adid: 'abc',
        nurl: 'https://ssp/win',
        burl: 'https://ssp/bill',
      },
    };

    const { installTsAdInit } = await import('./index');
    installTsAdInit();
    (window as any).__tsAdInit();

    expect(capturedListener).toBeDefined();
    capturedListener!({ isEmpty: false, slot: mockSlot });

    expect(beaconSpy).toHaveBeenCalledWith('https://ssp/win');
    expect(beaconSpy).toHaveBeenCalledWith('https://ssp/bill');
    beaconSpy.mockRestore();
  });

  it('does not fire nurl/burl when bid did not win GAM line item', async () => {
    const beaconSpy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true);
    let capturedListener: ((e: any) => void) | undefined;

    const mockSlotNoMatch = {
      addService: vi.fn().mockReturnThis(),
      setTargeting: vi.fn().mockReturnThis(),
      getSlotElementId: vi.fn().mockReturnValue('atf'),
      getTargeting: vi.fn().mockReturnValue(['OTHER_BID_ID']),
    };
    const mockPubads = {
      enableSingleRequest: vi.fn(),
      refresh: vi.fn(),
      addEventListener: vi.fn((event: string, fn: (e: any) => void) => {
        if (event === 'slotRenderEnded') capturedListener = fn;
      }),
    };
    (window as any).googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: vi.fn().mockReturnValue(mockSlotNoMatch),
      pubads: vi.fn().mockReturnValue(mockPubads),
      enableServices: vi.fn(),
    };
    (window as any).__ts_ad_slots = [
      {
        id: 'atf',
        gam_unit_path: '/123/atf',
        div_id: 'atf',
        formats: [[300, 250]],
        targeting: {},
      },
    ];
    (window as any).__ts_bids = {
      atf: {
        hb_pb: '1.00',
        hb_bidder: 'kargo',
        hb_adid: 'abc',
        nurl: 'https://ssp/win',
        burl: 'https://ssp/bill',
      },
    };

    const { installTsAdInit } = await import('./index');
    installTsAdInit();
    (window as any).__tsAdInit();
    capturedListener!({ isEmpty: false, slot: mockSlotNoMatch });

    expect(beaconSpy).not.toHaveBeenCalled();
    beaconSpy.mockRestore();
  });

  it('calls refresh even when __ts_bids is empty (graceful fallback)', async () => {
    const mockPubads = {
      enableSingleRequest: vi.fn(),
      addEventListener: vi.fn(),
      refresh: vi.fn(),
    };
    (window as any).googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: vi.fn().mockReturnValue({
        addService: vi.fn().mockReturnThis(),
        setTargeting: vi.fn().mockReturnThis(),
      }),
      pubads: vi.fn().mockReturnValue(mockPubads),
      enableServices: vi.fn(),
    };
    (window as any).__ts_ad_slots = [];
    (window as any).__ts_bids = {};

    const { installTsAdInit } = await import('./index');
    installTsAdInit();
    (window as any).__tsAdInit();

    expect(mockPubads.refresh).toHaveBeenCalled();
  });
});
