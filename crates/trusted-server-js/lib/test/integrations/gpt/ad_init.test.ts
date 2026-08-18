import { readFileSync } from 'node:fs';
import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { resolve } from 'node:path';

import { describe, it, expect, vi, beforeEach, afterEach, afterAll } from 'vitest';

import envelope from '../../fixtures/aps-renderer-v1.json';
import type { AuctionBidData, TsjsApi } from '../../../src/core/types';

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

// The `Prebid Response` payload the render bridge posts back to the Prebid
// Universal Creative over the message port.
interface PrebidResponseMessage {
  message?: string;
  adId?: string;
  ad?: string;
  width?: number;
  height?: number;
}

// `tsjs` is declared globally as the full `TsjsApi` (core/types.ts). Omitting
// it from `Window` before re-adding it as a `Partial` avoids the intersection
// that would force every fixture below to satisfy the whole `TsjsApi` shape.
type TestWindow = Omit<Window, 'tsjs'> & {
  googletag?: unknown;
  apstag?: { setDisplayBids?: () => void };
  tsjs?: Partial<TsjsApi>;
};

async function runGptBootstrapWithGoogleTag(googletag: object): Promise<void> {
  const bootstrapUrl = new URL(
    '../../../../../trusted-server-core/src/integrations/gpt_bootstrap.js',
    import.meta.url
  );
  const urlPath = decodeURIComponent(bootstrapUrl.pathname);
  let bootstrapPath: string;
  if (urlPath.startsWith('/@fs/')) {
    bootstrapPath = urlPath.slice('/@fs'.length);
  } else if (bootstrapUrl.protocol === 'file:') {
    bootstrapPath = urlPath;
  } else {
    bootstrapPath = path.resolve(process.cwd(), `.${urlPath}`);
  }
  const bootstrap = await readFile(bootstrapPath, 'utf8');
  const runBootstrap = new Function('window', 'googletag', bootstrap) as (
    window: Window,
    googletag: object
  ) => void;
  runBootstrap(window, googletag);
}

type HandoffImplementation = 'bootstrap' | 'bundle';

async function installHandoff(implementation: HandoffImplementation): Promise<void> {
  if (implementation === 'bootstrap') {
    const bootstrap = readFileSync(
      resolve(process.cwd(), '../../trusted-server-core/src/integrations/gpt_bootstrap.js'),
      'utf8'
    );
    window.eval(bootstrap);
    return;
  }

  const { installTsAdInit } = await import('../../../src/integrations/gpt/index');
  installTsAdInit();
}

interface ResponsiveSlotElementOptions {
  containerVisible?: boolean;
  containerWidth?: number;
  containerHeight?: number;
  elementHidden?: boolean;
  elementWidth?: number;
  elementHeight?: number;
  checkVisibility?: boolean;
}

function appendResponsiveSlotElement(
  id: string,
  {
    containerVisible = false,
    containerWidth = 0,
    containerHeight = 0,
    elementHidden = false,
    elementWidth = 0,
    elementHeight = 0,
    checkVisibility,
  }: ResponsiveSlotElementOptions = {}
): HTMLDivElement {
  const container = document.createElement('div');
  container.id = `${id}-container`;
  container.dataset.responsiveSlotTest = 'true';
  container.style.display = containerVisible ? 'block' : 'none';
  container.getBoundingClientRect = () =>
    ({ width: containerWidth, height: containerHeight }) as DOMRect;

  const element = document.createElement('div');
  element.id = id;
  element.style.display = elementHidden ? 'none' : 'block';
  element.getBoundingClientRect = () => ({ width: elementWidth, height: elementHeight }) as DOMRect;
  if (checkVisibility !== undefined) {
    (element as HTMLElement & { checkVisibility?: () => boolean }).checkVisibility = vi
      .fn()
      .mockReturnValue(checkVisibility);
  }
  container.appendChild(element);
  document.body.appendChild(container);
  return element;
}

function runGptBootstrap(): void {
  const bootstrap = readFileSync(
    resolve(process.cwd(), '../../trusted-server-core/src/integrations/gpt_bootstrap.js'),
    'utf8'
  );
  window.eval(bootstrap);
}

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
    document.getElementById('div-new-slot')?.remove();
    document.getElementById('div-atf-sidebar-2')?.remove();
    document.getElementById('div-size-hydrated')?.remove();
    document.getElementById('ad-header-0-_r_1_')?.remove();
    document.getElementById("ad'prefix-real")?.remove();
    document.querySelectorAll('[data-responsive-slot-test]').forEach((element) => element.remove());
  });

  function configureOpportunityDiagnostics(
    bid: AuctionBidData | undefined,
    recordTrustedServerOpportunity: ReturnType<typeof vi.fn>,
    formats: Array<[number, number]> = [[300, 250]]
  ) {
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
          formats,
          targeting: {},
        },
      ],
      bids: bid ? { atf_sidebar_ad: bid } : {},
      gptDiagnosticsRecorder: {
        recordTrustedServerOpportunity,
      } as unknown as TsjsApi['gptDiagnosticsRecorder'],
    };

    return { mockPubads, mockSlot };
  }

  it.each([
    [
      'inline markup',
      { hb_pb: '1.00', hb_adid: 'abc-uuid', adm: '<div>Creative</div>' },
      'renderable_candidate',
    ],
    [
      'complete cache coordinates',
      {
        hb_bidder: 'example-bidder',
        hb_adid: 'abc-uuid',
        hb_cache_host: 'cache.example.com',
        hb_cache_path: '/cache',
      },
      'renderable_candidate',
    ],
    [
      'an ad ID without a render source',
      { hb_pb: '1.00', hb_adid: 'abc-uuid' },
      'unrenderable_candidate',
    ],
    [
      'a render source without an ad ID',
      { hb_pb: '1.00', adm: '<div>Creative</div>' },
      'unrenderable_candidate',
    ],
    [
      'no non-empty Trusted Server bid targeting',
      { hb_pb: '', hb_bidder: '', hb_adid: '', adm: '<div>Creative</div>' },
      'no_candidate',
    ],
  ] as const)(
    'records exactly one %s opportunity for every resolved GPT slot',
    async (_description, bid, expectedOpportunity) => {
      const recordTrustedServerOpportunity = vi.fn();
      const { mockSlot } = configureOpportunityDiagnostics(
        bid as AuctionBidData,
        recordTrustedServerOpportunity
      );

      const { installTsAdInit } = await import('../../../src/integrations/gpt/index');
      installTsAdInit();
      (window as TestWindow).tsjs!.adInit!();

      expect(recordTrustedServerOpportunity).toHaveBeenCalledTimes(1);
      expect(recordTrustedServerOpportunity).toHaveBeenCalledWith(
        mockSlot,
        'atf_sidebar_ad',
        expectedOpportunity,
        undefined,
        [[300, 250]]
      );
    }
  );

  it('forwards winning bid auction metadata to diagnostics only when present', async () => {
    const recordTrustedServerOpportunity = vi.fn();
    const { mockSlot } = configureOpportunityDiagnostics(
      {
        hb_pb: '1.00',
        hb_bidder: 'example',
        hb_adid: 'creative-1',
        hb_auction_id: 'auction-123',
      },
      recordTrustedServerOpportunity
    );

    const { installTsAdInit } = await import('../../../src/integrations/gpt/index');
    installTsAdInit();
    (window as TestWindow).tsjs!.adInit!();

    expect(recordTrustedServerOpportunity).toHaveBeenCalledWith(
      mockSlot,
      'atf_sidebar_ad',
      'unrenderable_candidate',
      'auction-123',
      [[300, 250]]
    );
  });

  it('captures every configured Trusted Server format when associating a GPT slot', async () => {
    const recordTrustedServerOpportunity = vi.fn();
    const formats: Array<[number, number]> = [
      [300, 250],
      [728, 90],
      [320, 50],
    ];
    const { mockSlot } = configureOpportunityDiagnostics(
      undefined,
      recordTrustedServerOpportunity,
      formats
    );

    const { installTsAdInit } = await import('../../../src/integrations/gpt/index');
    installTsAdInit();
    (window as TestWindow).tsjs!.adInit!();

    expect(recordTrustedServerOpportunity).toHaveBeenCalledWith(
      mockSlot,
      'atf_sidebar_ad',
      'no_candidate',
      undefined,
      formats
    );
  });

  it('records no_candidate when the resolved slot has no bid', async () => {
    const recordTrustedServerOpportunity = vi.fn();
    const { mockSlot } = configureOpportunityDiagnostics(undefined, recordTrustedServerOpportunity);

    const { installTsAdInit } = await import('../../../src/integrations/gpt/index');
    installTsAdInit();
    (window as TestWindow).tsjs!.adInit!();

    expect(recordTrustedServerOpportunity).toHaveBeenCalledTimes(1);
    expect(recordTrustedServerOpportunity).toHaveBeenCalledWith(
      mockSlot,
      'atf_sidebar_ad',
      'no_candidate',
      undefined,
      [[300, 250]]
    );
  });

  it('keeps targeting, display, and refresh running when opportunity diagnostics throws', async () => {
    const existingSlot = {
      addService: vi.fn().mockReturnThis(),
      setTargeting: vi.fn().mockReturnThis(),
      getSlotElementId: vi.fn().mockReturnValue('div-atf-sidebar'),
      getTargeting: vi.fn().mockReturnValue([]),
    };
    const definedSlot = {
      addService: vi.fn().mockReturnThis(),
      setTargeting: vi.fn().mockReturnThis(),
      getSlotElementId: vi.fn().mockReturnValue('div-new-slot'),
      getTargeting: vi.fn().mockReturnValue([]),
    };
    const refresh = vi.fn();
    const mockPubads = {
      enableSingleRequest: vi.fn(),
      getSlots: vi.fn().mockReturnValue([existingSlot]),
      addEventListener: vi.fn(),
      refresh,
    };
    const display = vi.fn();
    (window as TestWindow).googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: vi.fn().mockReturnValue(definedSlot),
      pubads: vi.fn().mockReturnValue(mockPubads),
      enableServices: vi.fn(),
      display,
    };
    const newSlotDiv = document.createElement('div');
    newSlotDiv.id = 'div-new-slot';
    document.body.appendChild(newSlotDiv);
    const recordTrustedServerOpportunity = vi.fn(() => {
      throw new Error('diagnostics unavailable');
    });
    (window as TestWindow).tsjs = {
      adSlots: [
        {
          id: 'atf_sidebar_ad',
          gam_unit_path: '/123/atf',
          div_id: 'div-atf-sidebar',
          formats: [[300, 250]],
          targeting: {},
        },
        {
          id: 'new_slot_ad',
          gam_unit_path: '/123/new',
          div_id: 'div-new-slot',
          formats: [[728, 90]],
          targeting: {},
        },
      ],
      bids: {
        atf_sidebar_ad: {
          hb_pb: '1.00',
          hb_adid: 'existing-id',
          adm: '<div>Existing</div>',
        },
        new_slot_ad: {
          hb_pb: '2.00',
          hb_adid: 'new-id',
          adm: '<div>New</div>',
        },
      },
      gptDiagnosticsRecorder: {
        recordTrustedServerOpportunity,
      } as unknown as TsjsApi['gptDiagnosticsRecorder'],
    };

    const { installTsAdInit } = await import('../../../src/integrations/gpt/index');
    installTsAdInit();
    expect(() => (window as TestWindow).tsjs!.adInit!()).not.toThrow();

    expect(recordTrustedServerOpportunity).toHaveBeenCalledTimes(2);
    expect(existingSlot.setTargeting).toHaveBeenCalledWith('hb_pb', '1.00');
    expect(definedSlot.setTargeting).toHaveBeenCalledWith('hb_pb', '2.00');
    expect(display).toHaveBeenCalledWith('div-new-slot');
    expect(refresh).toHaveBeenCalledWith([existingSlot]);
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
    };

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
    expect(mockPubads.enableSingleRequest).toHaveBeenCalledOnce();
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
    const nativeRefresh = vi.fn();
    const mockPubads = {
      enableSingleRequest: vi.fn(),
      // Publisher has not defined this slot, so TS defines (owns) it.
      getSlots: vi.fn().mockReturnValue([]),
      addEventListener: vi.fn(),
      refresh: nativeRefresh,
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
    };

    const { installTsAdInit } = await import('../../../src/integrations/gpt/index');
    installTsAdInit();
    (window as TestWindow).tsjs!.adInit!();

    expect(defineSlotMock).toHaveBeenCalled();
    // GPT requires display() to register/render a freshly-defined slot.
    expect(displayMock).toHaveBeenCalledWith('div-atf-sidebar');
    // TS-owned slots are displayed, not refreshed (refresh() no-ops for a slot
    // that was never displayed).
    expect(nativeRefresh).not.toHaveBeenCalled();
  });

  it('hands a late publisher definition the TS inner-div slot without a second request', async () => {
    type FakeSlot = {
      addService(service: unknown): FakeSlot;
      setTargeting(key: string, value: string | string[]): FakeSlot;
      getSlotElementId(): string;
      getTargeting(key?: string): string[];
    };
    const slots = new Map<string, FakeSlot>();
    const requests: string[] = [];
    const makeSlot = (elementId: string): FakeSlot => ({
      addService: vi.fn().mockReturnThis(),
      setTargeting: vi.fn().mockReturnThis(),
      getSlotElementId: vi.fn().mockReturnValue(elementId),
      getTargeting: vi.fn().mockReturnValue([]),
    });
    const pubads = {
      enableSingleRequest: vi.fn(),
      getSlots: vi.fn(() => Array.from(slots.values())),
      addEventListener: vi.fn(),
      refresh: vi.fn((requestedSlots?: FakeSlot[]) => {
        (requestedSlots ?? Array.from(slots.values())).forEach((slot) =>
          requests.push(slot.getSlotElementId())
        );
      }),
    };
    const nativeDefineSlot = vi.fn(
      (_adUnitPath: string, _formats: number[][], elementId: string) => {
        if (slots.has(elementId)) return null;
        const slot = makeSlot(elementId);
        slots.set(elementId, slot);
        return slot;
      }
    );
    const nativeDisplay = vi.fn((elementId: string) => requests.push(elementId));
    const destroySlots = vi.fn();
    const googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: nativeDefineSlot,
      display: nativeDisplay,
      pubads: vi.fn().mockReturnValue(pubads),
      destroySlots,
      enableServices: vi.fn(),
    };
    (window as TestWindow).googletag = googletag;
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

    const publisherDefineSlot = googletag.defineSlot as unknown as (
      adUnitPath: string,
      formats: number[][],
      elementId: string
    ) => FakeSlot;
    const publisherDisplay = googletag.display as unknown as (elementId: string) => void;
    const publisherSlot = publisherDefineSlot('/123/atf', [[300, 250]], 'div-atf-sidebar');
    publisherSlot.addService(pubads);
    publisherDisplay('div-atf-sidebar');

    expect(nativeDefineSlot).toHaveBeenCalledTimes(1);
    expect(nativeDisplay).toHaveBeenCalledTimes(1);
    expect(requests).toEqual(['div-atf-sidebar']);
    expect((window as TestWindow).tsjs!.prevGptSlots).toEqual([]);

    const duplicatePublisherSlot = publisherDefineSlot('/123/atf', [[300, 250]], 'div-atf-sidebar');
    expect(duplicatePublisherSlot).toBeNull();
    expect(nativeDefineSlot).toHaveBeenCalledTimes(2);

    (window as TestWindow).tsjs!.adSlots = [];
    (window as TestWindow).tsjs!.adInit!();
    expect(destroySlots).not.toHaveBeenCalled();
  });

  it.each(['slot', 'element'] as const)(
    'hands a hydrated publisher ID off when it displays by %s',
    async (displayMode) => {
      type FakeSlot = {
        addService(service: unknown): FakeSlot;
        setTargeting(key: string, value: string | string[]): FakeSlot;
        getSlotElementId(): string;
        getTargeting(key?: string): string[];
      };
      const ssrDiv = document.getElementById('div-atf-sidebar')!;
      ssrDiv.id = 'ad-header-0-_R_0_';
      const hydratedId = 'ad-header-0-_r_1_';
      const slots = new Map<string, FakeSlot>();
      const requests: string[] = [];
      const makeSlot = (elementId: string): FakeSlot => ({
        addService: vi.fn().mockReturnThis(),
        setTargeting: vi.fn().mockReturnThis(),
        getSlotElementId: vi.fn().mockReturnValue(elementId),
        getTargeting: vi.fn().mockReturnValue([]),
      });
      const pubads = {
        enableSingleRequest: vi.fn(),
        getSlots: vi.fn(() => Array.from(slots.values())),
        addEventListener: vi.fn(),
        refresh: vi.fn(),
      };
      const nativeDefineSlot = vi.fn(
        (_adUnitPath: string, _formats: number[][], elementId: string) => {
          const slot = makeSlot(elementId);
          slots.set(elementId, slot);
          return slot;
        }
      );
      const nativeDisplay = vi.fn((target: string | Element | FakeSlot) => {
        if (typeof target === 'string') {
          requests.push(target);
        } else if ('getSlotElementId' in target) {
          requests.push(target.getSlotElementId());
        } else {
          requests.push(target.id);
        }
      });
      const googletag = {
        cmd: { push: vi.fn((fn: () => void) => fn()) },
        defineSlot: nativeDefineSlot,
        display: nativeDisplay,
        pubads: vi.fn().mockReturnValue(pubads),
        enableServices: vi.fn(),
      };
      (window as TestWindow).googletag = googletag;
      (window as TestWindow).tsjs = {
        adSlots: [
          {
            id: 'header_ad',
            gam_unit_path: '/123/header',
            div_id: 'ad-header-0-',
            formats: [[970, 250]],
            targeting: {},
          },
        ],
        bids: {},
      };

      const { installTsAdInit } = await import('../../../src/integrations/gpt/index');
      installTsAdInit();
      (window as TestWindow).tsjs!.adInit!();
      ssrDiv.id = hydratedId;

      const publisherSlot = (
        googletag.defineSlot as unknown as (
          adUnitPath: string,
          formats: number[][],
          elementId: string
        ) => FakeSlot
      )('/123/header', [[970, 250]], hydratedId);
      publisherSlot.addService(pubads);
      const publisherDisplay = googletag.display as unknown as (
        target: string | Element | FakeSlot
      ) => void;
      publisherDisplay(displayMode === 'slot' ? publisherSlot : ssrDiv);

      expect(nativeDefineSlot).toHaveBeenCalledTimes(1);
      expect(requests).toEqual(['ad-header-0-_R_0_']);
      expect((window as TestWindow).tsjs!.gptSlotHandoffs[hydratedId]).toBe(
        (window as TestWindow).tsjs!.gptSlotHandoffs['ad-header-0-_R_0_']
      );
    }
  );

  it('does not transfer an ambiguous hydrated publisher definition', async () => {
    type FakeSlot = {
      addService(service: unknown): FakeSlot;
      setTargeting(key: string, value: string | string[]): FakeSlot;
      getSlotElementId(): string;
      getTargeting(key?: string): string[];
    };
    const makeSlot = (elementId: string): FakeSlot => ({
      addService: vi.fn().mockReturnThis(),
      setTargeting: vi.fn().mockReturnThis(),
      getSlotElementId: vi.fn().mockReturnValue(elementId),
      getTargeting: vi.fn().mockReturnValue([]),
    });
    const firstSlot = makeSlot('ad-header-0-_R_0_');
    const secondSlot = makeSlot('ad-header-0-_R_1_');
    const nativeDefineSlot = vi.fn((_adUnitPath: string, _formats: number[][], elementId: string) =>
      makeSlot(elementId)
    );
    const pubads = {
      enableSingleRequest: vi.fn(),
      getSlots: vi.fn(() => [firstSlot, secondSlot]),
      addEventListener: vi.fn(),
      refresh: vi.fn(),
    };
    (window as TestWindow).googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: nativeDefineSlot,
      display: vi.fn(),
      pubads: vi.fn().mockReturnValue(pubads),
      enableServices: vi.fn(),
    };
    const firstHandoff = {
      gamUnitPath: '/123/header',
      formats: [[970, 250]],
      divIdPrefix: 'ad-header-0-',
      slotElementId: 'ad-header-0-_R_0_',
      publisherClaimed: false,
      suppressPublisherDisplay: false,
      suppressPublisherRefresh: false,
    };
    const secondHandoff = { ...firstHandoff, slotElementId: 'ad-header-0-_R_1_' };
    (window as TestWindow).tsjs = {
      gptSlotHandoffs: {
        'ad-header-0-_R_0_': firstHandoff,
        'ad-header-0-_R_1_': secondHandoff,
      },
    };

    const { installTsAdInit } = await import('../../../src/integrations/gpt/index');
    installTsAdInit();
    const defined = (
      (window as TestWindow).googletag as {
        defineSlot(adUnitPath: string, formats: number[][], elementId: string): FakeSlot;
      }
    ).defineSlot('/123/header', [[970, 250]], 'ad-header-0-_r_1_');

    expect(nativeDefineSlot).toHaveBeenCalledOnce();
    expect(defined).not.toBe(firstSlot);
    expect(defined).not.toBe(secondSlot);
    expect(firstHandoff.publisherClaimed).toBe(false);
    expect(secondHandoff.publisherClaimed).toBe(false);
  });

  it('delegates a div-less publisher definition with an unclaimed bundle handoff', async () => {
    const fallbackSlot = {
      getSlotElementId: vi.fn().mockReturnValue('div-ts-fallback'),
    };
    const nativeDefineSlot = vi.fn().mockReturnValue(null);
    const pubads = {
      getSlots: vi.fn().mockReturnValue([fallbackSlot]),
      refresh: vi.fn(),
    };
    const googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: nativeDefineSlot,
      display: vi.fn(),
      pubads: vi.fn().mockReturnValue(pubads),
    };
    const handoff = {
      gamUnitPath: '/123/fallback',
      formats: [[300, 250]],
      divIdPrefix: 'div-ts-',
      slotElementId: 'div-ts-fallback',
      publisherClaimed: false,
      suppressPublisherDisplay: false,
      suppressPublisherRefresh: false,
    };
    (window as TestWindow).googletag = googletag;
    (window as TestWindow).tsjs = {
      gptSlotHandoffs: { 'div-ts-fallback': handoff },
    };

    const { installTsAdInit } = await import('../../../src/integrations/gpt/index');
    installTsAdInit();

    expect(() =>
      (
        googletag.defineSlot as unknown as (
          adUnitPath: string,
          formats: number[][],
          elementId?: string
        ) => unknown
      )('/123/unrelated', [[728, 90]])
    ).not.toThrow();
    expect(nativeDefineSlot).toHaveBeenCalledWith('/123/unrelated', [[728, 90]]);
    expect(handoff.publisherClaimed).toBe(false);
  });

  it('prunes destroyed TS-owned handoffs and their aliases on SPA navigation', async () => {
    const slots = new Map<
      string,
      {
        addService(service: unknown): unknown;
        getSlotElementId(): string;
        getTargeting(key?: string): string[];
        setTargeting(key: string, value: string | string[]): unknown;
      }
    >();
    const makeSlot = (elementId: string) => ({
      addService: vi.fn().mockReturnThis(),
      getSlotElementId: vi.fn().mockReturnValue(elementId),
      getTargeting: vi.fn().mockReturnValue([]),
      setTargeting: vi.fn().mockReturnThis(),
    });
    const destroySlots = vi.fn();
    const pubads = {
      addEventListener: vi.fn(),
      enableSingleRequest: vi.fn(),
      getSlots: vi.fn(() => Array.from(slots.values())),
      refresh: vi.fn(),
    };
    const googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: vi.fn((_adUnitPath: string, _formats: number[][], elementId: string) => {
        const slot = makeSlot(elementId);
        slots.set(elementId, slot);
        return slot;
      }),
      destroySlots,
      display: vi.fn(),
      enableServices: vi.fn(),
      pubads: vi.fn().mockReturnValue(pubads),
    };
    (window as TestWindow).googletag = googletag;
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

    const handoff = (window as TestWindow).tsjs!.gptSlotHandoffs['div-atf-sidebar'];
    (window as TestWindow).tsjs!.gptSlotHandoffs['div-atf-sidebar-hydrated'] = handoff;
    (window as TestWindow).tsjs!.gptSlotHandoffs.unrelated = {
      ...handoff,
      slotElementId: 'div-unrelated',
    };
    const ownedSlot = slots.get('div-atf-sidebar')!;

    (window as TestWindow).tsjs!.adSlots = [];
    (window as TestWindow).tsjs!.adInit!();

    expect(destroySlots).toHaveBeenCalledWith([ownedSlot]);
    expect((window as TestWindow).tsjs!.gptSlotHandoffs).toEqual({
      unrelated: expect.objectContaining({ slotElementId: 'div-unrelated' }),
    });
  });

  it('suppresses a cross-realm element display without throwing', async () => {
    const nativeDisplay = vi.fn();
    const pubads = {
      getSlots: vi.fn().mockReturnValue([]),
      refresh: vi.fn(),
    };
    const googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: vi.fn(),
      display: nativeDisplay,
      pubads: vi.fn().mockReturnValue(pubads),
    };
    const iframe = document.createElement('iframe');
    document.body.appendChild(iframe);
    const crossRealmElement = iframe.contentDocument!.createElement('div');
    crossRealmElement.id = 'div-cross-realm';
    (window as TestWindow).googletag = googletag;
    (window as TestWindow).tsjs = {
      gptSlotHandoffs: {
        'div-cross-realm': {
          gamUnitPath: '/123/cross-realm',
          formats: [[300, 250]],
          divIdPrefix: 'div-cross-realm',
          slotElementId: 'div-cross-realm',
          publisherClaimed: true,
          suppressPublisherDisplay: true,
          suppressPublisherRefresh: false,
        },
      },
    };

    const { installTsAdInit } = await import('../../../src/integrations/gpt/index');
    installTsAdInit();

    expect(() =>
      (googletag.display as unknown as (target: Element) => void)(crossRealmElement)
    ).not.toThrow();
    expect(nativeDisplay).not.toHaveBeenCalled();
    iframe.remove();
  });

  it('runs the embedded bootstrap handoff for a hydrated publisher ID', async () => {
    type FakeSlot = {
      addService(service: unknown): FakeSlot;
      setTargeting(key: string, value: string | string[]): FakeSlot;
      getSlotElementId(): string;
    };
    const ssrDiv = document.getElementById('div-atf-sidebar')!;
    const hydratedId = 'ad-header-0-_r_1_';
    ssrDiv.id = 'ad-header-0-_R_0_';
    const slots = new Map<string, FakeSlot>();
    const requests: string[] = [];
    const makeSlot = (elementId: string): FakeSlot => ({
      addService: vi.fn().mockReturnThis(),
      setTargeting: vi.fn().mockReturnThis(),
      getSlotElementId: vi.fn().mockReturnValue(elementId),
    });
    const pubads = {
      enableSingleRequest: vi.fn(),
      getSlots: vi.fn(() => Array.from(slots.values())),
      refresh: vi.fn(),
    };
    const nativeDefineSlot = vi.fn(
      (_adUnitPath: string, _formats: number[][], elementId: string) => {
        if (slots.has(elementId) || elementId === hydratedId) return null;
        const slot = makeSlot(elementId);
        slots.set(elementId, slot);
        return slot;
      }
    );
    const nativeDisplay = vi.fn((target: string | FakeSlot) => {
      requests.push(typeof target === 'string' ? target : target.getSlotElementId());
    });
    (window as TestWindow).googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: nativeDefineSlot,
      display: nativeDisplay,
      pubads: vi.fn().mockReturnValue(pubads),
      enableServices: vi.fn(),
    };
    (window as TestWindow).tsjs = {
      adSlots: [
        {
          id: 'header_ad',
          gam_unit_path: '/123/header',
          div_id: 'ad-header-0-',
          formats: [[970, 250]],
          targeting: {},
        },
      ],
      bids: {},
    };

    runGptBootstrap();
    (window as TestWindow).tsjs!.adInit!();
    const { installTsAdInit } = await import('../../../src/integrations/gpt/index');
    installTsAdInit();
    ssrDiv.id = hydratedId;

    const googletag = (window as TestWindow).googletag as {
      defineSlot(adUnitPath: string, formats: number[][], elementId: string): FakeSlot | null;
      display(target: FakeSlot): void;
    };
    const publisherSlot = googletag.defineSlot('/123/header', [[970, 250]], ssrDiv.id);
    expect(publisherSlot).not.toBeNull();
    googletag.display(publisherSlot!);

    expect(nativeDefineSlot).toHaveBeenCalledTimes(1);
    expect(requests).toEqual(['ad-header-0-_R_0_']);

    const duplicatePublisherSlot = googletag.defineSlot('/123/header', [[970, 250]], ssrDiv.id);
    expect(duplicatePublisherSlot).toBeNull();
    expect(nativeDefineSlot).toHaveBeenCalledTimes(2);
  });

  it('delegates a div-less publisher definition with an unclaimed bootstrap handoff', () => {
    const fallbackSlot = {
      getSlotElementId: vi.fn().mockReturnValue('div-ts-fallback'),
    };
    const nativeDefineSlot = vi.fn().mockReturnValue(null);
    const pubads = {
      getSlots: vi.fn().mockReturnValue([fallbackSlot]),
      refresh: vi.fn(),
    };
    const googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: nativeDefineSlot,
      display: vi.fn(),
      pubads: vi.fn().mockReturnValue(pubads),
    };
    const handoff = {
      gamUnitPath: '/123/fallback',
      formats: [[300, 250]],
      divIdPrefix: 'div-ts-',
      slotElementId: 'div-ts-fallback',
      publisherClaimed: false,
      suppressPublisherDisplay: false,
      suppressPublisherRefresh: false,
    };
    (window as TestWindow).googletag = googletag;
    (window as TestWindow).tsjs = {
      gptSlotHandoffs: { 'div-ts-fallback': handoff },
    };

    const bootstrap = readFileSync(
      resolve(process.cwd(), '../../trusted-server-core/src/integrations/gpt_bootstrap.js'),
      'utf8'
    );
    window.eval(bootstrap);

    expect(() =>
      (
        googletag.defineSlot as unknown as (
          adUnitPath: string,
          formats: number[][],
          elementId?: string
        ) => unknown
      )('/123/unrelated', [[728, 90]])
    ).not.toThrow();
    expect(nativeDefineSlot).toHaveBeenCalledWith('/123/unrelated', [[728, 90]]);
    expect(handoff.publisherClaimed).toBe(false);
  });

  it.each(['bootstrap', 'bundle'] as const)(
    'does not hand a sibling slot to a TS fallback through the %s prefix path',
    async (implementation) => {
      const fallbackSlot = {
        getSlotElementId: vi.fn().mockReturnValue('div-atf-sidebar'),
      };
      const siblingSlot = {
        getSlotElementId: vi.fn().mockReturnValue('div-atf-sidebar-2'),
      };
      const nativeDefineSlot = vi.fn().mockReturnValue(siblingSlot);
      const nativeDisplay = vi.fn();
      const pubads = {
        getSlots: vi.fn().mockReturnValue([fallbackSlot]),
        refresh: vi.fn(),
      };
      const googletag = {
        cmd: { push: vi.fn((fn: () => void) => fn()) },
        defineSlot: nativeDefineSlot,
        display: nativeDisplay,
        pubads: vi.fn().mockReturnValue(pubads),
      };
      const handoff = {
        gamUnitPath: '/123/mpu',
        formats: [[300, 250]],
        divIdPrefix: 'div-atf-sidebar',
        slotElementId: 'div-atf-sidebar',
        publisherClaimed: false,
        suppressPublisherDisplay: false,
        suppressPublisherRefresh: false,
      };
      const siblingElement = document.createElement('div');
      siblingElement.id = 'div-atf-sidebar-2';
      document.body.appendChild(siblingElement);
      (window as TestWindow).googletag = googletag;
      (window as TestWindow).tsjs = {
        gptSlotHandoffs: { 'div-atf-sidebar': handoff },
      };

      await installHandoff(implementation);

      const publisherSlot = (
        googletag.defineSlot as unknown as (
          adUnitPath: string,
          formats: number[][],
          elementId: string
        ) => typeof siblingSlot
      )('/123/mpu', [[300, 250]], siblingElement.id);
      (googletag.display as unknown as (target: string) => void)(siblingElement.id);

      expect(publisherSlot).toBe(siblingSlot);
      expect(nativeDefineSlot).toHaveBeenCalledOnce();
      expect(nativeDisplay).toHaveBeenCalledWith(siblingElement.id);
      expect(handoff.publisherClaimed).toBe(false);
      expect(handoff.suppressPublisherDisplay).toBe(false);
    }
  );

  it.each(['bootstrap', 'bundle'] as const)(
    'hands a publisher shorthand size to the TS fallback through the %s prefix path',
    async (implementation) => {
      const fallbackSlot = {
        getSlotElementId: vi.fn().mockReturnValue('div-size-original'),
      };
      const nativeDefineSlot = vi.fn();
      const nativeDisplay = vi.fn();
      const pubads = {
        getSlots: vi.fn().mockReturnValue([fallbackSlot]),
        refresh: vi.fn(),
      };
      const googletag = {
        cmd: { push: vi.fn((fn: () => void) => fn()) },
        defineSlot: nativeDefineSlot,
        display: nativeDisplay,
        pubads: vi.fn().mockReturnValue(pubads),
      };
      const handoff = {
        gamUnitPath: '/123/size',
        formats: [[300, 250]],
        divIdPrefix: 'div-size-',
        slotElementId: 'div-size-original',
        publisherClaimed: false,
        suppressPublisherDisplay: false,
        suppressPublisherRefresh: false,
      };
      const hydratedElement = document.createElement('div');
      hydratedElement.id = 'div-size-hydrated';
      document.body.appendChild(hydratedElement);
      (window as TestWindow).googletag = googletag;
      (window as TestWindow).tsjs = {
        gptSlotHandoffs: { 'div-size-original': handoff },
      };

      await installHandoff(implementation);

      const publisherSlot = (
        googletag.defineSlot as unknown as (
          adUnitPath: string,
          formats: number[],
          elementId: string
        ) => typeof fallbackSlot
      )('/123/size', [300, 250], hydratedElement.id);
      (googletag.display as unknown as (target: string) => void)(hydratedElement.id);

      expect(publisherSlot).toBe(fallbackSlot);
      expect(nativeDefineSlot).not.toHaveBeenCalled();
      expect(nativeDisplay).not.toHaveBeenCalled();
      expect(handoff.publisherClaimed).toBe(true);
      expect(handoff.suppressPublisherDisplay).toBe(false);
    }
  );

  it('filters only the claimed slot from the first bootstrap global refresh', () => {
    const claimedSlot = {
      getSlotElementId: vi.fn().mockReturnValue('div-claimed'),
    };
    const unrelatedSlot = {
      getSlotElementId: vi.fn().mockReturnValue('div-unrelated'),
    };
    const nativeRefresh = vi.fn();
    const pubads = {
      getSlots: vi.fn().mockReturnValue([claimedSlot, unrelatedSlot]),
      refresh: nativeRefresh,
    };
    (window as TestWindow).googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: vi.fn(),
      display: vi.fn(),
      pubads: vi.fn().mockReturnValue(pubads),
    };
    (window as TestWindow).tsjs = {
      gptSlotHandoffs: {
        'div-claimed': {
          gamUnitPath: '/123/claimed',
          formats: [[300, 250]],
          divIdPrefix: 'div-claimed',
          slotElementId: 'div-claimed',
          publisherClaimed: true,
          suppressPublisherDisplay: false,
          suppressPublisherRefresh: true,
        },
      },
    };

    return installHandoff('bootstrap').then(() => {
      (pubads.refresh as () => void)();

      expect(nativeRefresh).toHaveBeenCalledWith([unrelatedSlot]);
      expect((window as TestWindow).tsjs!.gptSlotHandoffs['div-claimed']).toEqual(
        expect.objectContaining({ suppressPublisherRefresh: false })
      );
    });
  });

  it('preserves refresh options while filtering a claimed bootstrap slot', () => {
    const claimedSlot = {
      getSlotElementId: vi.fn().mockReturnValue('div-claimed'),
    };
    const unrelatedSlot = {
      getSlotElementId: vi.fn().mockReturnValue('div-unrelated'),
    };
    const nativeRefresh = vi.fn();
    const pubads = {
      getSlots: vi.fn().mockReturnValue([claimedSlot, unrelatedSlot]),
      refresh: nativeRefresh,
    };
    (window as TestWindow).googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: vi.fn(),
      display: vi.fn(),
      pubads: vi.fn().mockReturnValue(pubads),
    };
    (window as TestWindow).tsjs = {
      gptSlotHandoffs: {
        'div-claimed': {
          gamUnitPath: '/123/claimed',
          formats: [[300, 250]],
          divIdPrefix: 'div-claimed',
          slotElementId: 'div-claimed',
          publisherClaimed: true,
          suppressPublisherDisplay: false,
          suppressPublisherRefresh: true,
        },
      },
    };
    const refreshOptions = { changeCorrelator: false };

    return installHandoff('bootstrap').then(() => {
      (pubads.refresh as (slots: (typeof claimedSlot)[], options: typeof refreshOptions) => void)(
        [claimedSlot, unrelatedSlot],
        refreshOptions
      );

      expect(nativeRefresh).toHaveBeenCalledWith([unrelatedSlot], refreshOptions);
    });
  });

  it('does not transfer an ambiguous hydrated publisher definition through bootstrap', () => {
    const firstSlot = {
      getSlotElementId: vi.fn().mockReturnValue('div-prefix-original-a'),
    };
    const secondSlot = {
      getSlotElementId: vi.fn().mockReturnValue('div-prefix-original-b'),
    };
    const nativeDefineSlot = vi.fn().mockReturnValue(null);
    const pubads = {
      getSlots: vi.fn().mockReturnValue([firstSlot, secondSlot]),
      refresh: vi.fn(),
    };
    const firstHandoff = {
      gamUnitPath: '/123/prefix',
      formats: [[300, 250]],
      divIdPrefix: 'div-prefix-',
      slotElementId: 'div-prefix-original-a',
      publisherClaimed: false,
      suppressPublisherDisplay: false,
      suppressPublisherRefresh: false,
    };
    const secondHandoff = {
      ...firstHandoff,
      slotElementId: 'div-prefix-original-b',
    };
    const googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: nativeDefineSlot,
      display: vi.fn(),
      pubads: vi.fn().mockReturnValue(pubads),
    };
    (window as TestWindow).googletag = googletag;
    (window as TestWindow).tsjs = {
      gptSlotHandoffs: {
        'div-prefix-original-a': firstHandoff,
        'div-prefix-original-b': secondHandoff,
      },
    };

    return installHandoff('bootstrap').then(() => {
      const defined = (
        googletag.defineSlot as unknown as (
          adUnitPath: string,
          formats: number[][],
          elementId: string
        ) => null
      )('/123/prefix', [[300, 250]], 'div-prefix-hydrated');

      expect(defined).toBeNull();
      expect(nativeDefineSlot).toHaveBeenCalledOnce();
      expect(firstHandoff.publisherClaimed).toBe(false);
      expect(secondHandoff.publisherClaimed).toBe(false);
    });
  });

  it('preserves refresh options while filtering a claimed disabled-load slot', async () => {
    type FakeSlot = {
      addService(service: unknown): FakeSlot;
      setTargeting(key: string, value: string | string[]): FakeSlot;
      getSlotElementId(): string;
      getTargeting(key?: string): string[];
    };
    const slots = new Map<string, FakeSlot>();
    const makeSlot = (elementId: string): FakeSlot => ({
      addService: vi.fn().mockReturnThis(),
      setTargeting: vi.fn().mockReturnThis(),
      getSlotElementId: vi.fn().mockReturnValue(elementId),
      getTargeting: vi.fn().mockReturnValue([]),
    });
    const nativeRefresh = vi.fn();
    const pubads = {
      enableSingleRequest: vi.fn(),
      getSlots: vi.fn(() => Array.from(slots.values())),
      addEventListener: vi.fn(),
      refresh: nativeRefresh,
      disableInitialLoad: vi.fn(),
    };
    const nativeDefineSlot = vi.fn(
      (_adUnitPath: string, _formats: number[][], elementId: string) => {
        const slot = makeSlot(elementId);
        slots.set(elementId, slot);
        return slot;
      }
    );
    const googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: nativeDefineSlot,
      display: vi.fn(),
      pubads: vi.fn().mockReturnValue(pubads),
      enableServices: vi.fn(),
    };
    (window as TestWindow).googletag = googletag;
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
    pubads.disableInitialLoad();
    (window as TestWindow).tsjs!.adInit!();

    const publisherSlot = (
      googletag.defineSlot as unknown as (
        adUnitPath: string,
        formats: number[][],
        elementId: string
      ) => FakeSlot
    )('/123/atf', [[300, 250]], 'div-atf-sidebar');
    const unrelatedSlot = makeSlot('div-unrelated');
    const refreshOptions = { changeCorrelator: false };
    (
      pubads.refresh as unknown as (
        requestedSlots: FakeSlot[],
        options: { changeCorrelator: boolean }
      ) => void
    )([publisherSlot, unrelatedSlot], refreshOptions);

    expect(nativeRefresh).toHaveBeenLastCalledWith([unrelatedSlot], refreshOptions);
  });

  it('suppresses only the claimed slot from the first disabled-load publisher refresh', async () => {
    type FakeSlot = {
      addService(service: unknown): FakeSlot;
      setTargeting(key: string, value: string | string[]): FakeSlot;
      getSlotElementId(): string;
      getTargeting(key?: string): string[];
    };
    const slots = new Map<string, FakeSlot>();
    const requests: string[] = [];
    let initialLoadDisabled = false;
    const makeSlot = (elementId: string): FakeSlot => ({
      addService: vi.fn().mockReturnThis(),
      setTargeting: vi.fn().mockReturnThis(),
      getSlotElementId: vi.fn().mockReturnValue(elementId),
      getTargeting: vi.fn().mockReturnValue([]),
    });
    const pubads = {
      enableSingleRequest: vi.fn(),
      getSlots: vi.fn(() => Array.from(slots.values())),
      addEventListener: vi.fn(),
      refresh: vi.fn((requestedSlots?: FakeSlot[]) => {
        (requestedSlots ?? Array.from(slots.values())).forEach((slot) =>
          requests.push(slot.getSlotElementId())
        );
      }),
      disableInitialLoad: vi.fn(() => {
        initialLoadDisabled = true;
      }),
    };
    const nativeDefineSlot = vi.fn(
      (_adUnitPath: string, _formats: number[][], elementId: string) => {
        const slot = makeSlot(elementId);
        slots.set(elementId, slot);
        return slot;
      }
    );
    const nativeDisplay = vi.fn((elementId: string) => {
      if (!initialLoadDisabled) requests.push(elementId);
    });
    const googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: nativeDefineSlot,
      display: nativeDisplay,
      pubads: vi.fn().mockReturnValue(pubads),
      enableServices: vi.fn(),
    };
    (window as TestWindow).googletag = googletag;
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
    pubads.disableInitialLoad();
    (window as TestWindow).tsjs!.adInit!();

    const publisherDefineSlot = googletag.defineSlot as unknown as (
      adUnitPath: string,
      formats: number[][],
      elementId: string
    ) => FakeSlot;
    const publisherDisplay = googletag.display as unknown as (elementId: string) => void;
    const publisherRefresh = pubads.refresh as unknown as () => void;
    const publisherSlot = publisherDefineSlot('/123/atf', [[300, 250]], 'div-atf-sidebar');
    publisherSlot.addService(pubads);
    publisherDisplay('div-atf-sidebar');
    slots.set('div-unrelated', makeSlot('div-unrelated'));
    publisherRefresh();

    expect(nativeDefineSlot).toHaveBeenCalledTimes(1);
    expect(nativeDisplay).toHaveBeenCalledTimes(1);
    expect(requests.filter((elementId) => elementId === 'div-atf-sidebar')).toHaveLength(1);
    expect(requests).toContain('div-unrelated');
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
    const nativeRefresh = vi.fn();
    const mockPubads = {
      enableSingleRequest: vi.fn(),
      // Publisher has not defined this slot, so TS defines (owns) it.
      getSlots: vi.fn().mockReturnValue([]),
      addEventListener: vi.fn(),
      refresh: nativeRefresh,
      disableInitialLoad: vi.fn(),
    };
    const getConfigMock = vi.fn().mockReturnValue(undefined);
    const displayMock = vi.fn();
    (window as TestWindow).googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: vi.fn().mockReturnValue(mockSlot),
      display: displayMock,
      pubads: vi.fn().mockReturnValue(mockPubads),
      enableServices: vi.fn(),
      // Exercise the wrapper fallback used when the getter has no value.
      getConfig: getConfigMock,
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

    // Publisher disables initial load — goes through the wrapper the detector
    // installed, recording the state on window.tsjs.
    mockPubads.disableInitialLoad();
    expect((window as TestWindow).tsjs!.gptInitialLoadDisabled).toBe(true);

    (window as TestWindow).tsjs!.adInit!();

    // The slot is still registered via display(), and additionally refreshed so
    // it actually requests an ad under disableInitialLoad().
    expect(displayMock).toHaveBeenCalledWith('div-atf-sidebar');
    expect(nativeRefresh).toHaveBeenCalledWith([mockSlot]);
  });

  it('preserves legacy state in the edge bootstrap when getConfig does not report it', async () => {
    const mockSlot = {
      addService: vi.fn().mockReturnThis(),
      setTargeting: vi.fn().mockReturnThis(),
      getSlotElementId: vi.fn().mockReturnValue('div-atf-sidebar'),
    };
    const disableInitialLoadMock = vi.fn();
    const nativeRefresh = vi.fn();
    const mockPubads = {
      enableSingleRequest: vi.fn(),
      getSlots: vi.fn().mockReturnValue([]),
      refresh: nativeRefresh,
      disableInitialLoad: disableInitialLoadMock,
    };
    const displayMock = vi.fn();
    const getConfigMock = vi.fn().mockReturnValue(undefined);
    const googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: vi.fn().mockReturnValue(mockSlot),
      display: displayMock,
      pubads: vi.fn().mockReturnValue(mockPubads),
      enableServices: vi.fn(),
      getConfig: getConfigMock,
    };
    (window as TestWindow).googletag = googletag;
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

    await runGptBootstrapWithGoogleTag(googletag);

    mockPubads.disableInitialLoad();
    expect(disableInitialLoadMock).toHaveBeenCalledOnce();
    expect((window as TestWindow).tsjs!.gptInitialLoadDisabled).toBe(true);

    (window as TestWindow).tsjs!.adInit!();

    expect(displayMock).toHaveBeenCalledWith('div-atf-sidebar');
    expect(nativeRefresh).toHaveBeenCalledWith([mockSlot]);
  });

  it('tracks setConfig state and re-enabling in the edge bootstrap', async () => {
    const mockSlot = {
      addService: vi.fn().mockReturnThis(),
      setTargeting: vi.fn().mockReturnThis(),
      getSlotElementId: vi.fn().mockReturnValue('div-atf-sidebar'),
    };
    type InitialLoadConfig = {
      disableInitialLoad?: boolean | null;
    };
    let effectiveConfig: { disableInitialLoad?: boolean } = {};
    const setConfigMock = vi.fn((config: InitialLoadConfig) => {
      if ('disableInitialLoad' in config) {
        effectiveConfig = { disableInitialLoad: config.disableInitialLoad === true };
      }
    });
    const nativeRefresh = vi.fn();
    const mockPubads = {
      enableSingleRequest: vi.fn(),
      getSlots: vi.fn().mockReturnValue([]),
      refresh: nativeRefresh,
    };
    const displayMock = vi.fn();
    const googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: vi.fn().mockReturnValue(mockSlot),
      display: displayMock,
      pubads: vi.fn().mockReturnValue(mockPubads),
      enableServices: vi.fn(),
      getConfig: undefined as undefined | (() => { disableInitialLoad?: boolean }),
      setConfig: setConfigMock,
    };
    (window as TestWindow).googletag = googletag;
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

    await runGptBootstrapWithGoogleTag(googletag);

    // Older GPT runtimes may expose setConfig without getConfig. In that case,
    // the wrapper tracks explicit initial-load updates directly.
    googletag.setConfig({ disableInitialLoad: true });
    expect((window as TestWindow).tsjs!.gptInitialLoadDisabled).toBe(true);
    googletag.setConfig({ disableInitialLoad: false });
    expect((window as TestWindow).tsjs!.gptInitialLoadDisabled).toBe(false);

    googletag.getConfig = vi.fn(() => effectiveConfig);
    setConfigMock.mockClear();
    googletag.setConfig({ disableInitialLoad: true });
    expect(setConfigMock).toHaveBeenCalledOnce();
    expect((window as TestWindow).tsjs!.gptInitialLoadDisabled).toBe(true);

    (window as TestWindow).tsjs!.adInit!();

    expect(displayMock).toHaveBeenCalledWith('div-atf-sidebar');
    expect(nativeRefresh).toHaveBeenCalledWith([mockSlot]);

    nativeRefresh.mockClear();
    googletag.setConfig({ disableInitialLoad: false });
    expect((window as TestWindow).tsjs!.gptInitialLoadDisabled).toBe(false);

    (window as TestWindow).tsjs!.adInit!();

    expect(nativeRefresh).not.toHaveBeenCalled();

    googletag.setConfig({ disableInitialLoad: true });
    googletag.setConfig({ disableInitialLoad: null });
    expect((window as TestWindow).tsjs!.gptInitialLoadDisabled).toBe(false);

    (window as TestWindow).tsjs!.adInit!();

    expect(nativeRefresh).not.toHaveBeenCalled();
  });

  it('tracks the effective initial-load state from setConfig', async () => {
    // Modern GPT configuration uses googletag.setConfig() rather than the
    // legacy pubads().disableInitialLoad() method. TS must detect both forms.
    const mockSlot = {
      addService: vi.fn().mockReturnThis(),
      setTargeting: vi.fn().mockReturnThis(),
      getSlotElementId: vi.fn().mockReturnValue('div-atf-sidebar'),
      getTargeting: vi.fn().mockReturnValue([]),
    };
    type InitialLoadConfig = {
      disableInitialLoad?: boolean | null;
      singleRequest?: boolean;
    };
    let effectiveConfig: { disableInitialLoad?: boolean } = {};
    const setConfigMock = vi.fn((config: InitialLoadConfig) => {
      if ('disableInitialLoad' in config) {
        effectiveConfig = { disableInitialLoad: config.disableInitialLoad === true };
      }
    });
    const disableInitialLoadMock = vi.fn(() => {
      effectiveConfig = { disableInitialLoad: true };
    });
    const nativeRefresh = vi.fn();
    const mockPubads = {
      enableSingleRequest: vi.fn(),
      // Publisher has not defined this slot, so TS defines (owns) it.
      getSlots: vi.fn().mockReturnValue([]),
      addEventListener: vi.fn(),
      refresh: nativeRefresh,
      disableInitialLoad: disableInitialLoadMock,
    };
    const displayMock = vi.fn();
    const getConfigMock = vi.fn(() => effectiveConfig);
    const googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: vi.fn().mockReturnValue(mockSlot),
      display: displayMock,
      pubads: vi.fn().mockReturnValue(mockPubads),
      enableServices: vi.fn(),
      getConfig: undefined as undefined | typeof getConfigMock,
      setConfig: setConfigMock,
    };
    (window as TestWindow).googletag = googletag;
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
    installTsAdInit();

    const gpt = (window as TestWindow).googletag as {
      setConfig(config: InitialLoadConfig): void;
    };
    gpt.setConfig({ singleRequest: true });
    expect(setConfigMock).toHaveBeenCalledOnce();
    expect((window as TestWindow).tsjs!.gptInitialLoadDisabled).not.toBe(true);

    (window as TestWindow).tsjs!.adInit!();

    expect(displayMock).toHaveBeenCalledWith('div-atf-sidebar');
    expect(nativeRefresh).not.toHaveBeenCalled();

    // Fall back to the explicit setConfig value when getConfig is unavailable.
    gpt.setConfig({ disableInitialLoad: true });
    expect((window as TestWindow).tsjs!.gptInitialLoadDisabled).toBe(true);
    gpt.setConfig({ disableInitialLoad: false });
    expect((window as TestWindow).tsjs!.gptInitialLoadDisabled).toBe(false);

    googletag.getConfig = getConfigMock;
    setConfigMock.mockClear();
    const config = { disableInitialLoad: true, singleRequest: true };
    gpt.setConfig(config);
    expect(setConfigMock).toHaveBeenCalledOnce();
    expect(setConfigMock).toHaveBeenLastCalledWith(config);
    expect(getConfigMock).toHaveBeenCalledWith('disableInitialLoad');
    expect((window as TestWindow).tsjs!.gptInitialLoadDisabled).toBe(true);

    (window as TestWindow).tsjs!.adInit!();

    expect(nativeRefresh).toHaveBeenCalledWith([mockSlot]);

    nativeRefresh.mockClear();
    gpt.setConfig({ disableInitialLoad: false });
    expect((window as TestWindow).tsjs!.gptInitialLoadDisabled).toBe(false);
    gpt.setConfig({ disableInitialLoad: null });
    expect((window as TestWindow).tsjs!.gptInitialLoadDisabled).toBe(false);

    (window as TestWindow).tsjs!.adInit!();

    expect(nativeRefresh).not.toHaveBeenCalled();

    // GPT exposes one effective setting across the modern and legacy APIs.
    // A legacy call made after setConfig(false) disables initial load.
    mockPubads.disableInitialLoad();
    expect((window as TestWindow).tsjs!.gptInitialLoadDisabled).toBe(true);

    (window as TestWindow).tsjs!.adInit!();

    expect(nativeRefresh).toHaveBeenCalledWith([mockSlot]);

    // A later modern call can re-enable initial load after the legacy API.
    nativeRefresh.mockClear();
    gpt.setConfig({ disableInitialLoad: false });
    expect((window as TestWindow).tsjs!.gptInitialLoadDisabled).toBe(false);

    (window as TestWindow).tsjs!.adInit!();

    expect(nativeRefresh).not.toHaveBeenCalled();

    // Resetting the setting to its default has the same effective result.
    mockPubads.disableInitialLoad();
    gpt.setConfig({ disableInitialLoad: null });
    expect((window as TestWindow).tsjs!.gptInitialLoadDisabled).toBe(false);

    (window as TestWindow).tsjs!.adInit!();

    expect(nativeRefresh).not.toHaveBeenCalled();
  });

  it('reads initial-load configuration effective before detector installation', async () => {
    const mockSlot = {
      addService: vi.fn().mockReturnThis(),
      setTargeting: vi.fn().mockReturnThis(),
      getSlotElementId: vi.fn().mockReturnValue('div-atf-sidebar'),
      getTargeting: vi.fn().mockReturnValue([]),
    };
    const nativeRefresh = vi.fn();
    const mockPubads = {
      enableSingleRequest: vi.fn(),
      getSlots: vi.fn().mockReturnValue([]),
      addEventListener: vi.fn(),
      refresh: nativeRefresh,
    };
    const displayMock = vi.fn();
    const getConfigMock = vi.fn().mockReturnValue({ disableInitialLoad: true });
    (window as TestWindow).googletag = {
      cmd: { push: vi.fn((fn: () => void) => fn()) },
      defineSlot: vi.fn().mockReturnValue(mockSlot),
      display: displayMock,
      pubads: vi.fn().mockReturnValue(mockPubads),
      enableServices: vi.fn(),
      getConfig: getConfigMock,
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

    expect(getConfigMock).toHaveBeenCalledWith('disableInitialLoad');
    expect((window as TestWindow).tsjs!.gptInitialLoadDisabled).toBe(true);

    (window as TestWindow).tsjs!.adInit!();

    expect(displayMock).toHaveBeenCalledWith('div-atf-sidebar');
    expect(nativeRefresh).toHaveBeenCalledWith([mockSlot]);
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
      getTargeting: vi.fn((key: string) => (key === 'ts' ? ['publisher-value'] : [])),
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
    };

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
    expect(clearTargeting).not.toHaveBeenCalledWith('ts');
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
    };

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
    };

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
    };

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

  it.each([
    { implementation: 'runtime', activeIndexes: [2], publisherOwned: true, selectedIndex: 2 },
    { implementation: 'runtime', activeIndexes: [], selectedIndex: null },
    {
      implementation: 'runtime',
      candidateIndexes: [2],
      activeIndexes: [],
      selectedIndex: null,
    },
    {
      implementation: 'runtime',
      activeIndexes: [],
      elementLayoutIndexes: [1],
      visibleContainerIndexes: [1],
      selectedIndex: 1,
    },
    { implementation: 'runtime', activeIndexes: [0, 2], selectedIndex: null },
    {
      implementation: 'runtime',
      activeIndexes: [2, 3],
      hiddenElementIndexes: [2],
      selectedIndex: 3,
    },
    {
      implementation: 'runtime',
      activeIndexes: [],
      hiddenElementIndexes: [0, 1, 3],
      visibleContainerIndexes: [2],
      selectedIndex: 2,
    },
    {
      implementation: 'runtime',
      activeIndexes: [],
      hiddenElementIndexes: [0, 1, 3],
      visibleContainerIndexes: [1, 2],
      containerWidthIndexes: [2],
      selectedIndex: 2,
    },
    { implementation: 'runtime', activeIndexes: [2], divId: '', selectedIndex: null },
    { implementation: 'bootstrap', activeIndexes: [2], publisherOwned: true, selectedIndex: 2 },
    { implementation: 'bootstrap', activeIndexes: [], selectedIndex: null },
    {
      implementation: 'bootstrap',
      candidateIndexes: [2],
      activeIndexes: [],
      selectedIndex: null,
    },
    {
      implementation: 'bootstrap',
      activeIndexes: [],
      elementLayoutIndexes: [1],
      visibleContainerIndexes: [1],
      selectedIndex: 1,
    },
    { implementation: 'bootstrap', activeIndexes: [0, 2], selectedIndex: null },
    {
      implementation: 'bootstrap',
      activeIndexes: [2, 3],
      hiddenElementIndexes: [2],
      selectedIndex: 3,
    },
    {
      implementation: 'bootstrap',
      activeIndexes: [],
      hiddenElementIndexes: [0, 1, 3],
      visibleContainerIndexes: [2],
      selectedIndex: 2,
    },
    {
      implementation: 'bootstrap',
      activeIndexes: [],
      hiddenElementIndexes: [0, 1, 3],
      visibleContainerIndexes: [1, 2],
      containerWidthIndexes: [2],
      selectedIndex: 2,
    },
    { implementation: 'bootstrap', activeIndexes: [2], divId: '', selectedIndex: null },
  ] as const)(
    '$implementation resolves responsive matches $activeIndexes to $selectedIndex',
    async (testCase) => {
      const { implementation, activeIndexes, selectedIndex } = testCase;
      const hiddenElementIndexes =
        'hiddenElementIndexes' in testCase ? testCase.hiddenElementIndexes : [];
      const elementLayoutIndexes =
        'elementLayoutIndexes' in testCase ? testCase.elementLayoutIndexes : [];
      const visibleContainerIndexes =
        'visibleContainerIndexes' in testCase ? testCase.visibleContainerIndexes : activeIndexes;
      const containerWidthIndexes =
        'containerWidthIndexes' in testCase ? testCase.containerWidthIndexes : activeIndexes;
      const containerHeightIndexes =
        'containerHeightIndexes' in testCase ? testCase.containerHeightIndexes : activeIndexes;
      const elementWidthIndexes =
        'elementWidthIndexes' in testCase ? testCase.elementWidthIndexes : elementLayoutIndexes;
      const elementHeightIndexes =
        'elementHeightIndexes' in testCase ? testCase.elementHeightIndexes : elementLayoutIndexes;
      const candidateIndexes =
        'candidateIndexes' in testCase ? testCase.candidateIndexes : [0, 1, 2, 3];
      const divId = 'divId' in testCase ? testCase.divId : 'ad-responsive-';
      const publisherOwned = 'publisherOwned' in testCase && testCase.publisherOwned;
      const elements = ['a', 'b', 'c', 'd'].map((suffix, index) =>
        appendResponsiveSlotElement(
          (candidateIndexes as readonly number[]).includes(index)
            ? `ad-responsive-${suffix}`
            : `unrelated-responsive-${suffix}`,
          {
            containerVisible: (visibleContainerIndexes as readonly number[]).includes(index),
            containerWidth: (containerWidthIndexes as readonly number[]).includes(index) ? 320 : 0,
            containerHeight: (containerHeightIndexes as readonly number[]).includes(index)
              ? 100
              : 0,
            elementHidden: (hiddenElementIndexes as readonly number[]).includes(index),
            elementWidth: (elementWidthIndexes as readonly number[]).includes(index) ? 300 : 0,
            elementHeight: (elementHeightIndexes as readonly number[]).includes(index) ? 250 : 0,
          }
        )
      );
      const selectedElement = selectedIndex === null ? undefined : elements[selectedIndex];
      const mockSlot = {
        addService: vi.fn().mockReturnThis(),
        setTargeting: vi.fn().mockReturnThis(),
        getSlotElementId: vi.fn().mockReturnValue(selectedElement?.id ?? elements[0]!.id),
        getTargeting: vi.fn().mockReturnValue([]),
      };
      const nativeRefresh = vi.fn();
      const mockPubads = {
        enableSingleRequest: vi.fn(),
        getSlots: vi.fn().mockReturnValue(publisherOwned ? [mockSlot] : []),
        addEventListener: vi.fn(),
        refresh: nativeRefresh,
      };
      const defineSlot = vi.fn().mockReturnValue(mockSlot);
      const nativeDisplay = vi.fn();
      (window as TestWindow).googletag = {
        cmd: { push: vi.fn((fn: () => void) => fn()) },
        defineSlot,
        display: nativeDisplay,
        pubads: vi.fn().mockReturnValue(mockPubads),
        enableServices: vi.fn(),
      };
      (window as TestWindow).tsjs = {
        adSlots: [
          {
            id: 'responsive_slot',
            gam_unit_path: '/123/responsive',
            div_id: divId,
            formats: [[300, 250]],
            targeting: {},
          },
        ],
        bids: {},
      };

      if (implementation === 'runtime') {
        const { installTsAdInit } = await import('../../../src/integrations/gpt/index');
        installTsAdInit();
      } else {
        runGptBootstrap();
      }
      (window as TestWindow).tsjs!.adInit!();

      if (selectedElement) {
        if (publisherOwned) {
          expect(defineSlot).not.toHaveBeenCalled();
          expect(nativeRefresh).toHaveBeenCalledWith([mockSlot]);
        } else {
          expect(defineSlot).toHaveBeenCalledWith(
            '/123/responsive',
            [[300, 250]],
            selectedElement.id
          );
          expect(nativeDisplay).toHaveBeenCalledWith(selectedElement.id);
        }
        expect((window as TestWindow).tsjs!.divToSlotId).toEqual({
          [selectedElement.id]: 'responsive_slot',
        });
      } else {
        expect(defineSlot).not.toHaveBeenCalled();
        expect((window as TestWindow).tsjs!.divToSlotId).toEqual({});
      }
    }
  );

  it.each(['runtime', 'bootstrap'] as const)(
    '$implementation reports an ambiguous prefix once during adInit',
    async (implementation) => {
      const elements = ['a', 'b', 'c', 'd'].map((suffix) =>
        appendResponsiveSlotElement(`ad-warning-${suffix}`, { containerVisible: true })
      );
      const mockSlot = {
        addService: vi.fn().mockReturnThis(),
        setTargeting: vi.fn().mockReturnThis(),
        getSlotElementId: vi.fn().mockReturnValue(elements[0]!.id),
        getTargeting: vi.fn().mockReturnValue([]),
      };
      const mockPubads = {
        enableSingleRequest: vi.fn(),
        getSlots: vi.fn().mockReturnValue([]),
        addEventListener: vi.fn(),
        refresh: vi.fn(),
      };
      (window as TestWindow).googletag = {
        cmd: { push: vi.fn((fn: () => void) => fn()) },
        defineSlot: vi.fn().mockReturnValue(mockSlot),
        display: vi.fn(),
        pubads: vi.fn().mockReturnValue(mockPubads),
        enableServices: vi.fn(),
      };
      const bootstrapWarn = vi.fn();
      (window as TestWindow).tsjs = {
        adSlots: [
          {
            id: 'warning_slot',
            gam_unit_path: '/123/warning',
            div_id: 'ad-warning-',
            formats: [[300, 250]],
            targeting: {},
          },
          {
            id: 'warning_slot_duplicate',
            gam_unit_path: '/123/warning',
            div_id: 'ad-warning-',
            formats: [[300, 250]],
            targeting: {},
          },
        ],
        bids: {},
        ...(implementation === 'bootstrap' ? { log: { warn: bootstrapWarn } } : {}),
      };

      const runtimeWarn = vi.spyOn(console, 'warn');
      if (implementation === 'runtime') {
        const { installTsAdInit } = await import('../../../src/integrations/gpt/index');
        installTsAdInit();
      } else {
        runGptBootstrap();
      }
      (window as TestWindow).tsjs!.adInit!();

      if (implementation === 'runtime') {
        const warningCall = runtimeWarn.mock.calls.find((call) =>
          call.includes('GPT slot prefix did not resolve to one active element')
        );
        expect(runtimeWarn).toHaveBeenCalledTimes(1);
        expect(warningCall).toEqual(
          expect.arrayContaining([
            expect.objectContaining({
              divId: 'ad-warning-',
              prefixMatchCount: 4,
              activeMatchCount: 0,
            }),
          ])
        );
      } else {
        expect(bootstrapWarn).toHaveBeenCalledTimes(1);
        expect(bootstrapWarn).toHaveBeenCalledWith(
          'GPT slot prefix did not resolve to one active element',
          {
            divId: 'ad-warning-',
            prefixMatchCount: 4,
            activeMatchCount: 0,
          }
        );
      }
      runtimeWarn.mockRestore();
    }
  );

  it.each(['runtime', 'bootstrap'] as const)(
    '$implementation trusts checkVisibility when resolving a visible slot',
    async (implementation) => {
      const element = appendResponsiveSlotElement('ad-native-slot', {
        checkVisibility: true,
      });
      const mockSlot = {
        addService: vi.fn().mockReturnThis(),
        setTargeting: vi.fn().mockReturnThis(),
        getSlotElementId: vi.fn().mockReturnValue(element.id),
        getTargeting: vi.fn().mockReturnValue([]),
      };
      const defineSlot = vi.fn().mockReturnValue(mockSlot);
      const mockPubads = {
        enableSingleRequest: vi.fn(),
        getSlots: vi.fn().mockReturnValue([]),
        addEventListener: vi.fn(),
        refresh: vi.fn(),
      };
      (window as TestWindow).googletag = {
        cmd: { push: vi.fn((fn: () => void) => fn()) },
        defineSlot,
        display: vi.fn(),
        pubads: vi.fn().mockReturnValue(mockPubads),
        enableServices: vi.fn(),
      };
      (window as TestWindow).tsjs = {
        adSlots: [
          {
            id: 'native_visibility_slot',
            gam_unit_path: '/123/native-visibility',
            div_id: 'ad-native-',
            formats: [[300, 250]],
            targeting: {},
          },
        ],
        bids: {},
      };

      if (implementation === 'runtime') {
        const { installTsAdInit } = await import('../../../src/integrations/gpt/index');
        installTsAdInit();
      } else {
        runGptBootstrap();
      }
      (window as TestWindow).tsjs!.adInit!();

      expect(defineSlot).toHaveBeenCalledWith('/123/native-visibility', [[300, 250]], element.id);
    }
  );

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

describe('parseCachedBid', () => {
  async function parseCachedBid(body: string) {
    const mod = await import('../../../src/integrations/gpt/index');
    return mod.parseCachedBid(body);
  }

  it('decodes adm, dimensions, and price from a PBS Cache bid object', async () => {
    const bid = await parseCachedBid(
      JSON.stringify({ adm: '<div>cached</div>', w: 300, h: 250, price: 1.23 })
    );
    expect(bid).toEqual({ adm: '<div>cached</div>', width: 300, height: 250, price: 1.23 });
  });

  it('accepts width/height as an alternate dimension spelling', async () => {
    const bid = await parseCachedBid(
      JSON.stringify({ adm: '<div>cached</div>', width: 728, height: 90 })
    );
    expect(bid?.width).toBe(728);
    expect(bid?.height).toBe(90);
  });

  it('treats zero dimensions as absent so the caller falls back', async () => {
    const bid = await parseCachedBid(JSON.stringify({ adm: '<div>cached</div>', w: 0, h: 0 }));
    expect(bid?.width).toBeUndefined();
    expect(bid?.height).toBeUndefined();
  });

  it('treats a non-JSON body as raw creative markup with no metadata', async () => {
    const bid = await parseCachedBid('<div>raw</div>');
    expect(bid).toEqual({ adm: '<div>raw</div>' });
  });

  it('returns undefined when the JSON payload carries no usable adm', async () => {
    expect(await parseCachedBid(JSON.stringify({ w: 300, h: 250 }))).toBeUndefined();
    expect(await parseCachedBid('   ')).toBeUndefined();
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
      divToSlotId: { 'div-header': 'homepage_header' },
    };
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    document.getElementById('div-header')?.remove();
    delete (window as TestWindow).tsjs;
  });

  function createTrustedSlotIframe(divId = 'div-header'): Window {
    const slot = document.createElement('div');
    slot.id = divId;
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
          origAdd(
            type,
            handler as EventListener,
            opts as boolean | AddEventListenerOptions | undefined
          );
        }
      );
    await import('../../../src/integrations/gpt/index');
    addSpy.mockRestore();

    expect(bridgeListener, 'bridge listener should be registered').toBeDefined();
    return bridgeListener!;
  }

  it('records an inline creative request and response with the same opaque attempt ID', async () => {
    const recordTrustedServerCreativeRequest = vi.fn().mockReturnValue(41);
    const recordTrustedServerCreativeResponse = vi.fn();
    const recordTrustedServerCreativeFailure = vi.fn();
    const tsjs = (window as TestWindow).tsjs!;
    tsjs.gptDiagnosticsRecorder = {
      recordTrustedServerCreativeRequest,
      recordTrustedServerCreativeResponse,
      recordTrustedServerCreativeFailure,
    } as unknown as TsjsApi['gptDiagnosticsRecorder'];
    tsjs.bids.homepage_header.adm = '<div>Creative</div>';
    delete tsjs.bids.homepage_header.nurl;
    delete tsjs.bids.homepage_header.burl;

    const bridgeListener = await captureBridgeListener();
    const source = createTrustedSlotIframe();
    const postMessage = vi.fn();
    bridgeListener(
      Object.assign(new Event('message'), {
        data: JSON.stringify({ message: 'Prebid Request', adId: 'test-cache-uuid' }),
        ports: [{ postMessage }],
        source,
        stopImmediatePropagation: vi.fn(),
      }) as unknown as MessageEvent
    );

    expect(recordTrustedServerCreativeRequest).toHaveBeenCalledWith('homepage_header');
    expect(postMessage).toHaveBeenCalledTimes(1);
    expect(recordTrustedServerCreativeResponse).toHaveBeenCalledWith(41);
    expect(postMessage.mock.invocationCallOrder[0]).toBeLessThan(
      recordTrustedServerCreativeResponse.mock.invocationCallOrder[0]
    );
    expect(recordTrustedServerCreativeFailure).not.toHaveBeenCalled();
  });

  it('records no creative evidence for an ad ID the requesting slot does not own', async () => {
    const recordTrustedServerCreativeRequest = vi.fn().mockReturnValue(42);
    const recordTrustedServerCreativeResponse = vi.fn();
    const recordTrustedServerCreativeFailure = vi.fn();
    (window as TestWindow).tsjs!.gptDiagnosticsRecorder = {
      recordTrustedServerCreativeRequest,
      recordTrustedServerCreativeResponse,
      recordTrustedServerCreativeFailure,
    } as unknown as TsjsApi['gptDiagnosticsRecorder'];

    const bridgeListener = await captureBridgeListener();
    const source = createTrustedSlotIframe();
    bridgeListener(
      Object.assign(new Event('message'), {
        data: JSON.stringify({ message: 'Prebid Request', adId: 'someone-elses-ad-id' }),
        ports: [{ postMessage: vi.fn() }],
        source,
        stopImmediatePropagation: vi.fn(),
      }) as unknown as MessageEvent
    );

    expect(recordTrustedServerCreativeRequest).not.toHaveBeenCalled();
    expect(recordTrustedServerCreativeResponse).not.toHaveBeenCalled();
    expect(recordTrustedServerCreativeFailure).not.toHaveBeenCalled();
  });

  it.each([
    ['missing cache coordinates', {}],
    ['incomplete cache coordinates', { hb_cache_host: 'cache.example.com' }],
  ] as const)(
    'records missing_render_source for an exact-owned request with %s',
    async (_description, cacheFields) => {
      const recordTrustedServerCreativeRequest = vi.fn().mockReturnValue(45);
      const recordTrustedServerCreativeResponse = vi.fn();
      const recordTrustedServerCreativeFailure = vi.fn();
      const tsjs = (window as TestWindow).tsjs!;
      tsjs.gptDiagnosticsRecorder = {
        recordTrustedServerCreativeRequest,
        recordTrustedServerCreativeResponse,
        recordTrustedServerCreativeFailure,
      } as unknown as TsjsApi['gptDiagnosticsRecorder'];
      delete tsjs.bids.homepage_header.hb_cache_host;
      delete tsjs.bids.homepage_header.hb_cache_path;
      Object.assign(tsjs.bids.homepage_header, cacheFields);

      const bridgeListener = await captureBridgeListener();
      const source = createTrustedSlotIframe();
      const stopImmediatePropagation = vi.fn();
      bridgeListener(
        Object.assign(new Event('message'), {
          data: JSON.stringify({ message: 'Prebid Request', adId: 'test-cache-uuid' }),
          ports: [{ postMessage: vi.fn() }],
          source,
          stopImmediatePropagation,
        }) as unknown as MessageEvent
      );

      expect(recordTrustedServerCreativeRequest).toHaveBeenCalledWith('homepage_header');
      expect(recordTrustedServerCreativeFailure).toHaveBeenCalledTimes(1);
      expect(recordTrustedServerCreativeFailure).toHaveBeenCalledWith(45, 'missing_render_source');
      expect(recordTrustedServerCreativeResponse).not.toHaveBeenCalled();
      expect(stopImmediatePropagation).not.toHaveBeenCalled();
      expect(fetchStub).not.toHaveBeenCalled();
    }
  );

  it('records no failure when diagnostics declined to open a creative attempt', async () => {
    const recordTrustedServerCreativeRequest = vi.fn().mockReturnValue(undefined);
    const recordTrustedServerCreativeResponse = vi.fn();
    const recordTrustedServerCreativeFailure = vi.fn();
    const tsjs = (window as TestWindow).tsjs!;
    tsjs.gptDiagnosticsRecorder = {
      recordTrustedServerCreativeRequest,
      recordTrustedServerCreativeResponse,
      recordTrustedServerCreativeFailure,
    } as unknown as TsjsApi['gptDiagnosticsRecorder'];
    delete tsjs.bids.homepage_header.hb_cache_host;
    delete tsjs.bids.homepage_header.hb_cache_path;

    const bridgeListener = await captureBridgeListener();
    const source = createTrustedSlotIframe();
    const stopImmediatePropagation = vi.fn();
    bridgeListener(
      Object.assign(new Event('message'), {
        data: JSON.stringify({ message: 'Prebid Request', adId: 'test-cache-uuid' }),
        ports: [{ postMessage: vi.fn() }],
        source,
        stopImmediatePropagation,
      }) as unknown as MessageEvent
    );

    // Without an attempt ID there is nothing to attribute the failure to, and
    // the missing-source fallback must still run untouched.
    expect(recordTrustedServerCreativeRequest).toHaveBeenCalledWith('homepage_header');
    expect(recordTrustedServerCreativeFailure).not.toHaveBeenCalled();
    expect(recordTrustedServerCreativeResponse).not.toHaveBeenCalled();
    expect(stopImmediatePropagation).not.toHaveBeenCalled();
    expect(fetchStub).not.toHaveBeenCalled();
  });

  it('records response_post_failed when posting inline markup throws', async () => {
    const beaconSpy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true);
    const recordTrustedServerCreativeRequest = vi.fn().mockReturnValue(46);
    const recordTrustedServerCreativeResponse = vi.fn();
    const recordTrustedServerCreativeFailure = vi.fn();
    const tsjs = (window as TestWindow).tsjs!;
    tsjs.gptDiagnosticsRecorder = {
      recordTrustedServerCreativeRequest,
      recordTrustedServerCreativeResponse,
      recordTrustedServerCreativeFailure,
    } as unknown as TsjsApi['gptDiagnosticsRecorder'];
    tsjs.bids.homepage_header.adm = '<div>Creative</div>';

    const bridgeListener = await captureBridgeListener();
    const source = createTrustedSlotIframe();
    const stopImmediatePropagation = vi.fn();
    expect(() =>
      bridgeListener(
        Object.assign(new Event('message'), {
          data: JSON.stringify({ message: 'Prebid Request', adId: 'test-cache-uuid' }),
          ports: [
            {
              postMessage: vi.fn(() => {
                throw new Error('port closed');
              }),
            },
          ],
          source,
          stopImmediatePropagation,
        }) as unknown as MessageEvent
      )
    ).not.toThrow();

    expect(stopImmediatePropagation).toHaveBeenCalledTimes(1);
    expect(recordTrustedServerCreativeFailure).toHaveBeenCalledTimes(1);
    expect(recordTrustedServerCreativeFailure).toHaveBeenCalledWith(46, 'response_post_failed');
    expect(recordTrustedServerCreativeResponse).not.toHaveBeenCalled();
    expect(beaconSpy).not.toHaveBeenCalled();
    beaconSpy.mockRestore();
  });

  it('serves a server APS renderer once and rejects a repeated request', async () => {
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
    // Server-rendered APS capabilities are one-shot per slot and ad ID. A
    // repeated Universal Creative request is claimed but receives no payload.
    expect(portMessages).toHaveLength(1);
    const response = JSON.parse(portMessages[0]) as Record<string, unknown>;
    expect(Object.keys(response).sort()).toEqual(
      [
        'adId',
        'apsMountId',
        'apsRenderer',
        'height',
        'message',
        'publisherOrigin',
        'renderer',
        'rendererVersion',
        'width',
      ].sort()
    );
    expect(response).toEqual({
      message: 'Prebid Response',
      adId: renderer.bidId,
      renderer: expect.stringContaining('window.render=function'),
      rendererVersion: 6,
      apsMountId: expect.stringMatching(/^[A-Za-z0-9_-]{22}$/),
      publisherOrigin: window.location.origin,
      apsRenderer: renderer,
      width: 300,
      height: 250,
    });
    expect(String(response.renderer)).not.toContain(renderer.accountId);
    expect(String(response.renderer)).not.toContain(renderer.aaxResponse);

    expect(String(response.renderer)).toContain('d&&d.apsMountId');
    expect(String(response.renderer)).not.toContain(renderer.creativeUrl);
    expect(String(response.renderer)).not.toContain(renderer.aaxResponse);
    beaconSpy.mockRestore();
  });

  it('serves a registered Prebid APS renderer when its generated ad ID differs from the APS bid ID', async () => {
    const renderer = apsRenderer();
    const prebidAdId = 'prebid-generated-ad-id';
    const markUsed = vi.fn();
    (window as TestWindow).tsjs.apsPrebidRenderers = {
      [prebidAdId]: {
        adUnitCode: 'div-header',
        renderer,
        registeredAt: Date.now(),
        expiresAt: Date.now() + 60_000,
        markUsed,
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
    const foreignIframe = document.createElement('iframe');
    document.body.appendChild(foreignIframe);
    bridgeListener(
      Object.assign(new Event('message'), {
        data: JSON.stringify({ message: 'Prebid Request', adId: prebidAdId }),
        ports: [{ postMessage: (message: string) => portMessages.push(message) }],
        source: foreignIframe.contentWindow,
        stopImmediatePropagation: stopSpy,
      }) as unknown as MessageEvent
    );

    expect(stopSpy).toHaveBeenCalledTimes(2);
    expect(portMessages).toHaveLength(1);
    expect(markUsed).toHaveBeenCalledTimes(1);
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
    foreignIframe.remove();
  });

  it('still serves the APS renderer when markUsed throws', async () => {
    const renderer = apsRenderer();
    const prebidAdId = 'throwing-mark-used-ad-id';
    const markUsed = vi.fn(() => {
      throw new Error('fictional markUsed failure');
    });
    (window as TestWindow).tsjs.apsPrebidRenderers = {
      [prebidAdId]: {
        adUnitCode: 'div-header',
        renderer,
        registeredAt: Date.now(),
        expiresAt: Date.now() + 60_000,
        markUsed,
      },
    };

    const bridgeListener = await captureBridgeListener();
    const source = createTrustedSlotIframe();
    const portMessages: string[] = [];

    expect(() =>
      bridgeListener(
        Object.assign(new Event('message'), {
          data: JSON.stringify({ message: 'Prebid Request', adId: prebidAdId }),
          ports: [{ postMessage: (message: string) => portMessages.push(message) }],
          source,
          stopImmediatePropagation: vi.fn(),
        }) as unknown as MessageEvent
      )
    ).not.toThrow();

    expect(portMessages).toHaveLength(1);
    expect(JSON.parse(portMessages[0])).toEqual(
      expect.objectContaining({
        message: 'Prebid Response',
        adId: prebidAdId,
        apsRenderer: renderer,
      })
    );
    expect(markUsed).toHaveBeenCalledTimes(1);
  });

  it('prunes expired consumed APS renderer IDs', async () => {
    vi.useFakeTimers();
    try {
      const renderer = apsRenderer();
      const prebidAdId = 'expiring-consumed-ad-id';
      const start = Date.now();
      const firstMarkUsed = vi.fn();
      const secondMarkUsed = vi.fn();
      (window as TestWindow).tsjs.apsPrebidRenderers = {
        [prebidAdId]: {
          adUnitCode: 'div-header',
          renderer,
          registeredAt: start,
          expiresAt: start + 60_000,
          markUsed: firstMarkUsed,
        },
      };

      const bridgeListener = await captureBridgeListener();
      const source = createTrustedSlotIframe();
      const stopImmediatePropagation = vi.fn();
      const portMessages: string[] = [];
      const sendRequest = (): void => {
        bridgeListener(
          Object.assign(new Event('message'), {
            data: JSON.stringify({ message: 'Prebid Request', adId: prebidAdId }),
            ports: [{ postMessage: (message: string) => portMessages.push(message) }],
            source,
            stopImmediatePropagation,
          }) as unknown as MessageEvent
        );
      };

      sendRequest();
      vi.advanceTimersByTime(60_001);
      (window as TestWindow).tsjs.apsPrebidRenderers[prebidAdId] = {
        adUnitCode: 'div-header',
        renderer,
        registeredAt: Date.now(),
        expiresAt: Date.now() + 60_000,
        markUsed: secondMarkUsed,
      };
      sendRequest();

      expect(portMessages).toHaveLength(2);
      expect(stopImmediatePropagation).toHaveBeenCalledTimes(2);
      expect(firstMarkUsed).toHaveBeenCalledTimes(1);
      expect(secondMarkUsed).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it('fails closed when consumed APS renderer tombstones reach capacity', async () => {
    const renderer = apsRenderer();
    const capacity = 256;
    const callbacks = Array.from({ length: capacity + 1 }, () => ({
      markUsed: vi.fn(),
    }));
    const entries = Object.fromEntries(
      callbacks.map((lifecycle, index) => [
        `capacity-ad-${index}`,
        {
          adUnitCode: 'div-header',
          renderer,
          registeredAt: Date.now(),
          expiresAt: Date.now() + 60_000,
          ...lifecycle,
        },
      ])
    );
    (window as TestWindow).tsjs.apsPrebidRenderers = entries;

    const bridgeListener = await captureBridgeListener();
    const source = createTrustedSlotIframe();
    const stopImmediatePropagation = vi.fn();
    const portMessages: string[] = [];
    const sendRequest = (adId: string): void => {
      bridgeListener(
        Object.assign(new Event('message'), {
          data: JSON.stringify({ message: 'Prebid Request', adId }),
          ports: [{ postMessage: (message: string) => portMessages.push(message) }],
          source,
          stopImmediatePropagation,
        }) as unknown as MessageEvent
      );
    };

    for (let index = 0; index < capacity; index += 1) {
      sendRequest(`capacity-ad-${index}`);
    }
    sendRequest(`capacity-ad-${capacity}`);
    sendRequest('capacity-ad-0');

    expect(portMessages).toHaveLength(capacity);
    expect(callbacks[capacity].markUsed).not.toHaveBeenCalled();
    expect(entries[`capacity-ad-${capacity}`]).toBeDefined();
    expect(callbacks[0].markUsed).toHaveBeenCalledTimes(1);
    expect(stopImmediatePropagation).toHaveBeenCalledTimes(capacity + 2);
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
        markUsed: vi.fn(),
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

    expect(stopSpy).toHaveBeenCalledTimes(1);
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
        markUsed: vi.fn(),
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

  it('claims a TS-owned request before rejecting invalid APS data', async () => {
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

    expect(stopSpy).toHaveBeenCalledOnce();
    expect(portMessages).toEqual([]);
    expect(fetchStub).not.toHaveBeenCalled();
  });

  it('accepts an APS request from a dynamic slot root resolved from its configured prefix', async () => {
    const renderer = apsRenderer();
    (window as TestWindow).tsjs.bids.homepage_header = {
      hb_adid: renderer.bidId,
      hb_bidder: 'aps',
      renderer,
    };
    (window as TestWindow).tsjs.adSlots[0].div_id = 'div-header-';

    const bridgeListener = await captureBridgeListener();
    const source = createTrustedSlotIframe('div-header-dynamic');
    const portMessages: string[] = [];
    bridgeListener(
      Object.assign(new Event('message'), {
        data: JSON.stringify({ message: 'Prebid Request', adId: renderer.bidId }),
        ports: [{ postMessage: (message: string) => portMessages.push(message) }],
        source,
        stopImmediatePropagation: vi.fn(),
      }) as unknown as MessageEvent
    );

    expect(portMessages).toHaveLength(1);
    document.getElementById('div-header-dynamic')?.remove();
  });

  it('does not let an overlapping slot prefix claim another slot iframe', async () => {
    const renderer = apsRenderer();
    (window as TestWindow).tsjs.bids.homepage_header = {
      hb_adid: renderer.bidId,
      hb_bidder: 'aps',
      renderer,
    };
    (window as TestWindow).tsjs.adSlots.push({
      id: 'homepage_header_mobile',
      formats: [[320, 50]],
      gam_unit_path: '/a/b/mobile',
      div_id: 'div-header-mobile',
      targeting: {},
    });

    const bridgeListener = await captureBridgeListener();
    const source = createTrustedSlotIframe('div-header-mobile');
    const portMessages: string[] = [];
    const stopSpy = vi.fn();
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
    document.getElementById('div-header-mobile')?.remove();
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
    const recordTrustedServerCreativeRequest = vi.fn().mockReturnValue(43);
    const recordTrustedServerCreativeResponse = vi.fn();
    const recordTrustedServerCreativeFailure = vi.fn();
    (window as TestWindow).tsjs!.gptDiagnosticsRecorder = {
      recordTrustedServerCreativeRequest,
      recordTrustedServerCreativeResponse,
      recordTrustedServerCreativeFailure,
    } as unknown as TsjsApi['gptDiagnosticsRecorder'];
    const mockAd = '<div>Test Creative</div>';
    // PBS Cache (returnCreative=false) returns the cached bid as a JSON object;
    // the creative lives under `adm`, not as the raw response body. The bridge
    // must parse it and forward `adm`, mirroring the Prebid Universal Creative.
    fetchStub.mockResolvedValue({
      ok: true,
      text: () => Promise.resolve(JSON.stringify({ adm: mockAd, width: 728, height: 90 })),
    } as Response);

    // Capture the bridge's 'message' listener at module-init time.
    let bridgeListener: ((e: MessageEvent) => unknown) | undefined;
    const origAdd = window.addEventListener.bind(window);
    const addSpy = vi
      .spyOn(window, 'addEventListener')
      .mockImplementation(
        (type: string, handler: EventListenerOrEventListenerObject, opts?: unknown) => {
          if (type === 'message') bridgeListener = handler as (e: MessageEvent) => unknown;
          origAdd(
            type,
            handler as EventListener,
            opts as boolean | AddEventListenerOptions | undefined
          );
        }
      );
    await import('../../../src/integrations/gpt/index');
    addSpy.mockRestore(); // Restore only addEventListener — fetchStub must stay stubbed

    expect(bridgeListener, 'bridge listener should be registered').toBeDefined();

    const stopSpy = vi.fn();
    const portMessages: string[] = [];
    const postMessage = vi.fn((message: string) => portMessages.push(message));
    const fakePort = { postMessage };
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

    const parsed = JSON.parse(portMessages[0]) as PrebidResponseMessage;
    expect(parsed.message).toBe('Prebid Response');
    expect(parsed.adId).toBe('test-cache-uuid');
    expect(parsed.ad).toBe(mockAd);
    expect(recordTrustedServerCreativeRequest).toHaveBeenCalledWith('homepage_header');
    expect(recordTrustedServerCreativeResponse).toHaveBeenCalledWith(43);
    expect(recordTrustedServerCreativeFailure).not.toHaveBeenCalled();
    expect(postMessage.mock.invocationCallOrder[0]).toBeLessThan(
      recordTrustedServerCreativeResponse.mock.invocationCallOrder[0]
    );
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

  it('does not classify a downstream cache-processing throw as cache_fetch_failed', async () => {
    const beaconSpy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true);
    const recordTrustedServerCreativeRequest = vi.fn().mockReturnValue(54);
    const recordTrustedServerCreativeResponse = vi.fn();
    const recordTrustedServerCreativeFailure = vi.fn();
    (window as TestWindow).tsjs!.gptDiagnosticsRecorder = {
      recordTrustedServerCreativeRequest,
      recordTrustedServerCreativeResponse,
      recordTrustedServerCreativeFailure,
    } as unknown as TsjsApi['gptDiagnosticsRecorder'];
    fetchStub.mockResolvedValue({
      ok: true,
      text: () => Promise.resolve(JSON.stringify({ adm: '<div>Creative</div>' })),
    } as Response);
    const { log } = await import('../../../src/core/log');
    const debugSpy = vi.spyOn(log, 'debug').mockImplementation(() => {
      throw new Error('success logging unavailable');
    });

    try {
      const bridgeListener = await captureBridgeListener();
      const source = createTrustedSlotIframe();
      const postMessage = vi.fn();
      const dispatch = () =>
        bridgeListener(
          Object.assign(new Event('message'), {
            data: JSON.stringify({ message: 'Prebid Request', adId: 'test-cache-uuid' }),
            ports: [{ postMessage }],
            source,
            stopImmediatePropagation: vi.fn(),
          }) as unknown as MessageEvent
        );

      dispatch();
      await new Promise<void>((resolve) => setTimeout(resolve, 50));

      expect(postMessage).toHaveBeenCalledTimes(1);
      expect(recordTrustedServerCreativeResponse).toHaveBeenCalledWith(54);
      expect(recordTrustedServerCreativeFailure).not.toHaveBeenCalled();

      // A second request must run after the first downstream failure, proving
      // the in-flight key was still cleared by the promise's finally handler.
      dispatch();
      await new Promise<void>((resolve) => setTimeout(resolve, 50));
      expect(fetchStub).toHaveBeenCalledTimes(2);
      expect(postMessage).toHaveBeenCalledTimes(2);
      expect(recordTrustedServerCreativeFailure).not.toHaveBeenCalled();
    } finally {
      debugSpy.mockRestore();
      beaconSpy.mockRestore();
    }
  });

  it.each([
    [
      'an HTTP non-ok response',
      (stub: ReturnType<typeof vi.fn>) =>
        stub.mockResolvedValue({ ok: false, status: 503 } as Response),
    ],
    [
      'a response body read rejection',
      (stub: ReturnType<typeof vi.fn>) =>
        stub.mockResolvedValue({
          ok: true,
          text: () => Promise.reject(new Error('body unavailable')),
        } as Response),
    ],
    [
      'a network rejection',
      (stub: ReturnType<typeof vi.fn>) => stub.mockRejectedValue(new Error('network unavailable')),
    ],
  ] as const)('records cache_fetch_failed once for %s', async (_description, arrangeFetch) => {
    const beaconSpy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true);
    const recordTrustedServerCreativeRequest = vi.fn().mockReturnValue(47);
    const recordTrustedServerCreativeResponse = vi.fn();
    const recordTrustedServerCreativeFailure = vi.fn();
    (window as TestWindow).tsjs!.gptDiagnosticsRecorder = {
      recordTrustedServerCreativeRequest,
      recordTrustedServerCreativeResponse,
      recordTrustedServerCreativeFailure,
    } as unknown as TsjsApi['gptDiagnosticsRecorder'];
    arrangeFetch(fetchStub);

    const bridgeListener = await captureBridgeListener();
    const source = createTrustedSlotIframe();
    bridgeListener(
      Object.assign(new Event('message'), {
        data: JSON.stringify({ message: 'Prebid Request', adId: 'test-cache-uuid' }),
        ports: [{ postMessage: vi.fn() }],
        source,
        stopImmediatePropagation: vi.fn(),
      }) as unknown as MessageEvent
    );
    await new Promise<void>((resolve) => setTimeout(resolve, 50));

    expect(recordTrustedServerCreativeRequest).toHaveBeenCalledWith('homepage_header');
    expect(recordTrustedServerCreativeFailure).toHaveBeenCalledTimes(1);
    expect(recordTrustedServerCreativeFailure).toHaveBeenCalledWith(47, 'cache_fetch_failed');
    expect(recordTrustedServerCreativeResponse).not.toHaveBeenCalled();
    expect(beaconSpy).not.toHaveBeenCalled();
    beaconSpy.mockRestore();
  });

  it('records only response_post_failed when posting cached markup throws', async () => {
    const beaconSpy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true);
    const recordTrustedServerCreativeRequest = vi.fn().mockReturnValue(48);
    const recordTrustedServerCreativeResponse = vi.fn();
    const recordTrustedServerCreativeFailure = vi.fn();
    (window as TestWindow).tsjs!.gptDiagnosticsRecorder = {
      recordTrustedServerCreativeRequest,
      recordTrustedServerCreativeResponse,
      recordTrustedServerCreativeFailure,
    } as unknown as TsjsApi['gptDiagnosticsRecorder'];
    fetchStub.mockResolvedValue({
      ok: true,
      text: () => Promise.resolve(JSON.stringify({ adm: '<div>Creative</div>' })),
    } as Response);

    const bridgeListener = await captureBridgeListener();
    const source = createTrustedSlotIframe();
    bridgeListener(
      Object.assign(new Event('message'), {
        data: JSON.stringify({ message: 'Prebid Request', adId: 'test-cache-uuid' }),
        ports: [
          {
            postMessage: vi.fn(() => {
              throw new Error('port closed');
            }),
          },
        ],
        source,
        stopImmediatePropagation: vi.fn(),
      }) as unknown as MessageEvent
    );
    await new Promise<void>((resolve) => setTimeout(resolve, 50));

    expect(recordTrustedServerCreativeFailure).toHaveBeenCalledTimes(1);
    expect(recordTrustedServerCreativeFailure).toHaveBeenCalledWith(48, 'response_post_failed');
    expect(recordTrustedServerCreativeResponse).not.toHaveBeenCalled();
    expect(beaconSpy).not.toHaveBeenCalled();
    beaconSpy.mockRestore();
  });

  it.each(['request', 'response'] as const)(
    'keeps inline delivery and beacons unchanged when the diagnostics %s writer throws',
    async (throwingWriter) => {
      const beaconSpy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true);
      const recordTrustedServerCreativeRequest = vi.fn(() => {
        if (throwingWriter === 'request') throw new Error('diagnostics request failed');
        return 49;
      });
      const recordTrustedServerCreativeResponse = vi.fn(() => {
        if (throwingWriter === 'response') throw new Error('diagnostics response failed');
      });
      const recordTrustedServerCreativeFailure = vi.fn();
      const tsjs = (window as TestWindow).tsjs!;
      tsjs.gptDiagnosticsRecorder = {
        recordTrustedServerCreativeRequest,
        recordTrustedServerCreativeResponse,
        recordTrustedServerCreativeFailure,
      } as unknown as TsjsApi['gptDiagnosticsRecorder'];
      tsjs.bids.homepage_header.adm = '<div>Creative</div>';

      const bridgeListener = await captureBridgeListener();
      const source = createTrustedSlotIframe();
      const stopImmediatePropagation = vi.fn();
      const postMessage = vi.fn();
      expect(() =>
        bridgeListener(
          Object.assign(new Event('message'), {
            data: JSON.stringify({ message: 'Prebid Request', adId: 'test-cache-uuid' }),
            ports: [{ postMessage }],
            source,
            stopImmediatePropagation,
          }) as unknown as MessageEvent
        )
      ).not.toThrow();

      expect(stopImmediatePropagation).toHaveBeenCalledTimes(1);
      expect(postMessage).toHaveBeenCalledTimes(1);
      expect(beaconSpy).toHaveBeenCalledTimes(2);
      expect(recordTrustedServerCreativeFailure).not.toHaveBeenCalled();
      if (throwingWriter === 'response') {
        expect(recordTrustedServerCreativeResponse).toHaveBeenCalledWith(49);
      } else {
        expect(recordTrustedServerCreativeResponse).not.toHaveBeenCalled();
      }
      beaconSpy.mockRestore();
    }
  );

  it('does not turn a throwing cache response diagnostic into a cache failure', async () => {
    const beaconSpy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true);
    const recordTrustedServerCreativeRequest = vi.fn().mockReturnValue(50);
    const recordTrustedServerCreativeResponse = vi.fn(() => {
      throw new Error('diagnostics response failed');
    });
    const recordTrustedServerCreativeFailure = vi.fn();
    (window as TestWindow).tsjs!.gptDiagnosticsRecorder = {
      recordTrustedServerCreativeRequest,
      recordTrustedServerCreativeResponse,
      recordTrustedServerCreativeFailure,
    } as unknown as TsjsApi['gptDiagnosticsRecorder'];
    fetchStub.mockResolvedValue({
      ok: true,
      text: () => Promise.resolve(JSON.stringify({ adm: '<div>Creative</div>' })),
    } as Response);

    const bridgeListener = await captureBridgeListener();
    const source = createTrustedSlotIframe();
    const postMessage = vi.fn();
    bridgeListener(
      Object.assign(new Event('message'), {
        data: JSON.stringify({ message: 'Prebid Request', adId: 'test-cache-uuid' }),
        ports: [{ postMessage }],
        source,
        stopImmediatePropagation: vi.fn(),
      }) as unknown as MessageEvent
    );
    await new Promise<void>((resolve) => setTimeout(resolve, 50));

    expect(postMessage).toHaveBeenCalledTimes(1);
    expect(recordTrustedServerCreativeResponse).toHaveBeenCalledWith(50);
    expect(recordTrustedServerCreativeFailure).not.toHaveBeenCalled();
    expect(beaconSpy).toHaveBeenCalledTimes(2);
    beaconSpy.mockRestore();
  });

  it('preserves missing-source fallback when the failure diagnostic throws', async () => {
    const recordTrustedServerCreativeRequest = vi.fn().mockReturnValue(51);
    const recordTrustedServerCreativeFailure = vi.fn(() => {
      throw new Error('diagnostics failure writer failed');
    });
    const tsjs = (window as TestWindow).tsjs!;
    tsjs.gptDiagnosticsRecorder = {
      recordTrustedServerCreativeRequest,
      recordTrustedServerCreativeResponse: vi.fn(),
      recordTrustedServerCreativeFailure,
    } as unknown as TsjsApi['gptDiagnosticsRecorder'];
    delete tsjs.bids.homepage_header.hb_cache_host;
    delete tsjs.bids.homepage_header.hb_cache_path;

    const bridgeListener = await captureBridgeListener();
    const source = createTrustedSlotIframe();
    const stopImmediatePropagation = vi.fn();
    expect(() =>
      bridgeListener(
        Object.assign(new Event('message'), {
          data: JSON.stringify({ message: 'Prebid Request', adId: 'test-cache-uuid' }),
          ports: [{ postMessage: vi.fn() }],
          source,
          stopImmediatePropagation,
        }) as unknown as MessageEvent
      )
    ).not.toThrow();

    expect(recordTrustedServerCreativeFailure).toHaveBeenCalledWith(51, 'missing_render_source');
    expect(stopImmediatePropagation).not.toHaveBeenCalled();
    expect(fetchStub).not.toHaveBeenCalled();
  });

  it('uses the adInit-resolved div when a responsive prefix becomes ambiguous', async () => {
    const beaconSpy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true);
    fetchStub.mockResolvedValue({
      ok: true,
      text: () => Promise.resolve('<div>Responsive Creative</div>'),
    } as Response);

    const resolvedSlot = document.createElement('div');
    resolvedSlot.id = 'div-responsive-a';
    const iframe = document.createElement('iframe');
    resolvedSlot.appendChild(iframe);
    document.body.appendChild(resolvedSlot);
    const laterSibling = document.createElement('div');
    laterSibling.id = 'div-responsive-b';
    document.body.appendChild(laterSibling);

    (window as TestWindow).tsjs!.adSlots = [
      {
        id: 'homepage_header',
        formats: [[728, 90]] as [number, number][],
        gam_unit_path: '/a/b/c',
        div_id: 'div-responsive-',
        targeting: {},
      },
    ];
    (window as TestWindow).tsjs!.divToSlotId = {
      'div-responsive-a': 'homepage_header',
    };

    const bridgeListener = await captureBridgeListener();
    const portMessages: string[] = [];
    const fakePort = { postMessage: (s: string) => portMessages.push(s) };
    const stopSpy = vi.fn();

    bridgeListener(
      Object.assign(new Event('message'), {
        data: JSON.stringify({ message: 'Prebid Request', adId: 'test-cache-uuid' }),
        ports: [fakePort],
        source: iframe.contentWindow,
        stopImmediatePropagation: stopSpy,
      }) as unknown as MessageEvent
    );
    await new Promise<void>((resolve) => setTimeout(resolve, 50));

    expect(fetchStub).toHaveBeenCalledWith(
      'https://openads.example.com/cache?uuid=test-cache-uuid',
      { mode: 'cors' }
    );
    expect(portMessages).toHaveLength(1);
    expect(stopSpy).toHaveBeenCalled();
    expect(beaconSpy).toHaveBeenCalledWith('https://ssp.example/win');
    expect(beaconSpy).toHaveBeenCalledWith('https://ssp.example/bill');
    beaconSpy.mockRestore();
  });

  it('declines to render when the PBS Cache response carries no adm', async () => {
    const beaconSpy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true);
    const recordTrustedServerCreativeRequest = vi.fn().mockReturnValue(44);
    const recordTrustedServerCreativeResponse = vi.fn();
    const recordTrustedServerCreativeFailure = vi.fn();
    (window as TestWindow).tsjs!.gptDiagnosticsRecorder = {
      recordTrustedServerCreativeRequest,
      recordTrustedServerCreativeResponse,
      recordTrustedServerCreativeFailure,
    } as unknown as TsjsApi['gptDiagnosticsRecorder'];
    // A returnCreative=false JSON entry with no `adm` (VAST-only, or malformed).
    // The bridge must NOT forward the serialized bid document to PUC.
    fetchStub.mockResolvedValue({
      ok: true,
      text: () => Promise.resolve(JSON.stringify({ width: 728, height: 90 })),
    } as Response);

    const bridgeListener = await captureBridgeListener();
    const stopSpy = vi.fn();
    const portMessages: string[] = [];
    const fakePort = { postMessage: (s: string) => portMessages.push(s) };
    const source = createTrustedSlotIframe();

    bridgeListener(
      Object.assign(new Event('message'), {
        data: JSON.stringify({ message: 'Prebid Request', adId: 'test-cache-uuid' }),
        ports: [fakePort],
        source,
        stopImmediatePropagation: stopSpy,
      }) as unknown as MessageEvent
    );
    await new Promise<void>((resolve) => setTimeout(resolve, 50));

    // TS owns the adId so Prebid is still stopped, but with nothing renderable
    // the bridge sends no Prebid Response and fires no win/billing beacons.
    expect(fetchStub).toHaveBeenCalled();
    expect(stopSpy).toHaveBeenCalled();
    expect(portMessages).toHaveLength(0);
    expect(recordTrustedServerCreativeRequest).toHaveBeenCalledWith('homepage_header');
    expect(recordTrustedServerCreativeFailure).toHaveBeenCalledTimes(1);
    expect(recordTrustedServerCreativeFailure).toHaveBeenCalledWith(44, 'invalid_cache_payload');
    expect(recordTrustedServerCreativeResponse).not.toHaveBeenCalled();
    expect(beaconSpy).not.toHaveBeenCalled();
    beaconSpy.mockRestore();
  });

  it('renders a non-JSON PBS Cache body as raw creative markup', async () => {
    const beaconSpy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true);
    const rawAd = '<div>Raw Cached Creative</div>';
    // Backward compatibility: a cache that returns the creative markup directly
    // (not a JSON bid object) is still rendered as-is.
    fetchStub.mockResolvedValue({
      ok: true,
      text: () => Promise.resolve(rawAd),
    } as Response);

    const bridgeListener = await captureBridgeListener();
    const stopSpy = vi.fn();
    const portMessages: string[] = [];
    const fakePort = { postMessage: (s: string) => portMessages.push(s) };
    const source = createTrustedSlotIframe();

    bridgeListener(
      Object.assign(new Event('message'), {
        data: JSON.stringify({ message: 'Prebid Request', adId: 'test-cache-uuid' }),
        ports: [fakePort],
        source,
        stopImmediatePropagation: stopSpy,
      }) as unknown as MessageEvent
    );
    await new Promise<void>((resolve) => setTimeout(resolve, 50));

    expect(portMessages).toHaveLength(1);

    const parsed = JSON.parse(portMessages[0]) as PrebidResponseMessage;
    expect(parsed.ad).toBe(rawAd);
    expect(beaconSpy).toHaveBeenCalledTimes(2);
    beaconSpy.mockRestore();
  });

  it('sizes a PBS Cache render from the cached bid dimensions', async () => {
    const beaconSpy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true);
    // Cached bid is 300x250 while the slot's first format is 728x90 (from the
    // default setup). The response must use the cached dimensions.
    fetchStub.mockResolvedValue({
      ok: true,
      text: () => Promise.resolve(JSON.stringify({ adm: '<div>cached</div>', w: 300, h: 250 })),
    } as Response);

    const bridgeListener = await captureBridgeListener();
    const portMessages: string[] = [];
    const fakePort = { postMessage: (s: string) => portMessages.push(s) };
    const source = createTrustedSlotIframe();

    bridgeListener(
      Object.assign(new Event('message'), {
        data: JSON.stringify({ message: 'Prebid Request', adId: 'test-cache-uuid' }),
        ports: [fakePort],
        source,
        stopImmediatePropagation: vi.fn(),
      }) as unknown as MessageEvent
    );
    await new Promise<void>((resolve) => setTimeout(resolve, 50));

    expect(portMessages).toHaveLength(1);

    const parsed = JSON.parse(portMessages[0]) as PrebidResponseMessage;
    expect(parsed.width).toBe(300);
    expect(parsed.height).toBe(250);
    beaconSpy.mockRestore();
  });

  it('expands ${AUCTION_PRICE} from the cached bid price before responding', async () => {
    const beaconSpy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true);
    fetchStub.mockResolvedValue({
      ok: true,
      text: () =>
        Promise.resolve(
          JSON.stringify({
            adm: '<a href="https://t.example/win?p=${AUCTION_PRICE}">go</a>',
            price: 2.5,
          })
        ),
    } as Response);

    const bridgeListener = await captureBridgeListener();
    const portMessages: string[] = [];
    const fakePort = { postMessage: (s: string) => portMessages.push(s) };
    const source = createTrustedSlotIframe();

    bridgeListener(
      Object.assign(new Event('message'), {
        data: JSON.stringify({ message: 'Prebid Request', adId: 'test-cache-uuid' }),
        ports: [fakePort],
        source,
        stopImmediatePropagation: vi.fn(),
      }) as unknown as MessageEvent
    );
    await new Promise<void>((resolve) => setTimeout(resolve, 50));

    expect(portMessages).toHaveLength(1);

    const parsed = JSON.parse(portMessages[0]) as PrebidResponseMessage;
    expect(parsed.ad).toContain('p=2.5');
    expect(parsed.ad).not.toContain('${AUCTION_PRICE}');
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

  it('does not let one slot block a PBS Cache render for another slot sharing an adId', async () => {
    // The in-flight guard must be scoped to the requesting slot, not the shared
    // adId: two distinct slots sharing one hb_adid must each fetch and render.
    const beaconSpy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true);
    // Deferred fetch that stays pending, so both messages are in flight when we
    // assert the launched-fetch count.
    fetchStub.mockReturnValue(new Promise<Response>(() => {}));
    (window as TestWindow).tsjs = {
      bids: {
        slot_a: {
          hb_adid: 'shared-uuid',
          hb_bidder: 'ix',
          hb_pb: '1.00',
          hb_cache_host: 'cache.example.com',
          hb_cache_path: '/cache',
        },
        slot_b: {
          hb_adid: 'shared-uuid',
          hb_bidder: 'ix',
          hb_pb: '1.00',
          hb_cache_host: 'cache.example.com',
          hb_cache_path: '/cache',
        },
      },
      adSlots: [
        {
          id: 'slot_a',
          formats: [[728, 90]] as [number, number][],
          gam_unit_path: '/a',
          div_id: 'div-a',
          targeting: {},
        },
        {
          id: 'slot_b',
          formats: [[300, 250]] as [number, number][],
          gam_unit_path: '/a',
          div_id: 'div-b',
          targeting: {},
        },
      ],
      divToSlotId: { 'div-a': 'slot_a', 'div-b': 'slot_b' },
    };

    const bridgeListener = await captureBridgeListener();

    const mkIframe = (divId: string): Window => {
      const slot = document.createElement('div');
      slot.id = divId;
      const iframe = document.createElement('iframe');
      slot.appendChild(iframe);
      document.body.appendChild(slot);
      return iframe.contentWindow!;
    };
    const sourceA = mkIframe('div-a');
    const sourceB = mkIframe('div-b');

    try {
      for (const source of [sourceA, sourceB]) {
        bridgeListener(
          Object.assign(new Event('message'), {
            data: JSON.stringify({ message: 'Prebid Request', adId: 'shared-uuid' }),
            ports: [{ postMessage: () => {} }],
            source,
            stopImmediatePropagation: vi.fn(),
          }) as unknown as MessageEvent
        );
      }

      // Each slot launches its own fetch — the shared adId does not cross-block.
      expect(fetchStub).toHaveBeenCalledTimes(2);
    } finally {
      document.getElementById('div-a')?.remove();
      document.getElementById('div-b')?.remove();
      beaconSpy.mockRestore();
    }
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
      divToSlotId: { 'div-header': 'homepage_header' },
    };

    let bridgeListener: ((e: MessageEvent) => unknown) | undefined;
    const origAdd = window.addEventListener.bind(window);
    const addSpy = vi
      .spyOn(window, 'addEventListener')
      .mockImplementation(
        (type: string, handler: EventListenerOrEventListenerObject, opts?: unknown) => {
          if (type === 'message') bridgeListener = handler as (e: MessageEvent) => unknown;
          origAdd(
            type,
            handler as EventListener,
            opts as boolean | AddEventListenerOptions | undefined
          );
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

    const parsed = JSON.parse(portMessages[0]) as PrebidResponseMessage;
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

  it('sizes the inline response from the winning bid, not the first slot format', async () => {
    // Multi-size slot whose winner is the SECOND configured format. Sizing from
    // slot.formats[0] would render the 300x250 winner in a 728x90 box.
    const beaconSpy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true);
    const winnerAdm = '<div>Winner 300x250</div>';
    (window as TestWindow).tsjs = {
      bids: {
        homepage_header: {
          hb_adid: 'winner-adid',
          hb_bidder: 'ix',
          hb_pb: '2.00',
          w: 300,
          h: 250,
          adm: winnerAdm,
        },
      },
      adSlots: [
        {
          id: 'homepage_header',
          formats: [
            [728, 90],
            [300, 250],
          ] as [number, number][],
          gam_unit_path: '/a/b/c',
          div_id: 'div-header',
          targeting: {},
        },
      ],
      divToSlotId: { 'div-header': 'homepage_header' },
    };

    const bridgeListener = await captureBridgeListener();
    const stopSpy = vi.fn();
    const portMessages: string[] = [];
    const fakePort = { postMessage: (s: string) => portMessages.push(s) };
    const source = createTrustedSlotIframe();

    try {
      bridgeListener(
        Object.assign(new Event('message'), {
          data: JSON.stringify({ message: 'Prebid Request', adId: 'winner-adid' }),
          ports: [fakePort],
          source,
          stopImmediatePropagation: stopSpy,
        }) as unknown as MessageEvent
      );
      await new Promise<void>((resolve) => setTimeout(resolve, 50));

      expect(portMessages).toHaveLength(1);

      const parsed = JSON.parse(portMessages[0]) as PrebidResponseMessage;
      expect(parsed.width).toBe(300);
      expect(parsed.height).toBe(250);
    } finally {
      beaconSpy.mockRestore();
    }
  });

  it('resolves the requesting slot bid when two slots share one hb_adid', async () => {
    // Duplicate hb_adid across slots: PBS Cache is absent, so hb_adid falls back
    // to a creative id that a bidder reuses across slots. The bridge must resolve
    // the bid by the requesting slot, not the first bid whose hb_adid matches —
    // otherwise every slot but the first renders blank.
    const beaconSpy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true);
    const headerAdm = '<div>Header Creative</div>';
    const inContentAdm = '<div>In-Content Creative</div>';
    (window as TestWindow).tsjs = {
      bids: {
        homepage_header: {
          hb_adid: 'shared-creative-id',
          hb_bidder: 'ix',
          hb_pb: '0.53',
          adm: headerAdm,
        },
        homepage_in_content: {
          hb_adid: 'shared-creative-id',
          hb_bidder: 'ix',
          hb_pb: '0.40',
          adm: inContentAdm,
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
        {
          id: 'homepage_in_content',
          formats: [[300, 250]] as [number, number][],
          gam_unit_path: '/a/b/c',
          div_id: 'div-in-content',
          targeting: {},
        },
      ],
      divToSlotId: { 'div-header': 'homepage_header', 'div-in-content': 'homepage_in_content' },
    };

    const bridgeListener = await captureBridgeListener();
    const stopSpy = vi.fn();
    const portMessages: string[] = [];
    const fakePort = { postMessage: (s: string) => portMessages.push(s) };

    // Iframe belongs to the SECOND slot, whose bid is not the first hb_adid match.
    const slot = document.createElement('div');
    slot.id = 'div-in-content';
    const iframe = document.createElement('iframe');
    slot.appendChild(iframe);
    document.body.appendChild(slot);
    const source = iframe.contentWindow!;

    try {
      bridgeListener(
        Object.assign(new Event('message'), {
          data: JSON.stringify({ message: 'Prebid Request', adId: 'shared-creative-id' }),
          ports: [fakePort],
          source,
          stopImmediatePropagation: stopSpy,
        }) as unknown as MessageEvent
      );
      await new Promise<void>((resolve) => setTimeout(resolve, 50));

      expect(portMessages).toHaveLength(1);

      const parsed = JSON.parse(portMessages[0]) as PrebidResponseMessage;
      // The requesting slot's own creative and dimensions, not the first match's.
      expect(parsed.ad).toBe(inContentAdm);
      expect(parsed.width).toBe(300);
      expect(parsed.height).toBe(250);
    } finally {
      slot.remove();
      beaconSpy.mockRestore();
    }
  });

  it('falls back to keepalive fetch when sendBeacon is unavailable', async () => {
    const originalSendBeacon = navigator.sendBeacon;
    Object.defineProperty(navigator, 'sendBeacon', {
      value: undefined,
      writable: true,
      configurable: true,
    });

    try {
      (window as TestWindow).tsjs!.bids!.homepage_header = {
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
    (window as TestWindow).tsjs!.bids!.homepage_header = {
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
    const recordTrustedServerCreativeRequest = vi.fn().mockReturnValue(52);
    const recordTrustedServerCreativeResponse = vi.fn();
    const recordTrustedServerCreativeFailure = vi.fn();
    (window as TestWindow).tsjs!.gptDiagnosticsRecorder = {
      recordTrustedServerCreativeRequest,
      recordTrustedServerCreativeResponse,
      recordTrustedServerCreativeFailure,
    } as unknown as TsjsApi['gptDiagnosticsRecorder'];
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
    expect(recordTrustedServerCreativeRequest).not.toHaveBeenCalled();
    expect(recordTrustedServerCreativeResponse).not.toHaveBeenCalled();
    expect(recordTrustedServerCreativeFailure).not.toHaveBeenCalled();
    foreignIframe.remove();
  });

  it('ignores a request whose source slot does not own the resolved adId', async () => {
    // Two configured slots; slot A's iframe requests slot B's hb_adid. The
    // bridge must not return slot B's creative or fire slot B's beacons.
    (window as TestWindow).tsjs!.bids!.homepage_footer = {
      hb_adid: 'footer-uuid',
      hb_bidder: 'kargo',
      hb_pb: '2.00',
      hb_cache_host: 'openads.example.com',
      hb_cache_path: '/cache',
      nurl: 'https://ssp.example/footer-win',
      burl: 'https://ssp.example/footer-bill',
    };
    (window as TestWindow).tsjs!.adSlots!.push({
      id: 'homepage_footer',
      formats: [[300, 250]] as [number, number][],
      gam_unit_path: '/a/b/footer',
      div_id: 'div-footer',
      targeting: {},
    });
    const recordTrustedServerCreativeRequest = vi.fn().mockReturnValue(53);
    const recordTrustedServerCreativeResponse = vi.fn();
    const recordTrustedServerCreativeFailure = vi.fn();
    (window as TestWindow).tsjs!.gptDiagnosticsRecorder = {
      recordTrustedServerCreativeRequest,
      recordTrustedServerCreativeResponse,
      recordTrustedServerCreativeFailure,
    } as unknown as TsjsApi['gptDiagnosticsRecorder'];

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
    expect(recordTrustedServerCreativeRequest).not.toHaveBeenCalled();
    expect(recordTrustedServerCreativeResponse).not.toHaveBeenCalled();
    expect(recordTrustedServerCreativeFailure).not.toHaveBeenCalled();
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
