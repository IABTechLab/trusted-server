import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const OLD_TOKEN = '550e8400-e29b-41d4-a716-446655440000';
const NEW_TOKEN = '650e8400-e29b-41d4-a716-446655440000';

function slotWithTargeting(values: Record<string, string>) {
  return {
    getSlotElementId: () => 'div-header',
    getTargeting: (key: string) => (values[key] ? [values[key]] : []),
  };
}

function trustedSource(): Window {
  const root = document.createElement('div');
  root.id = 'div-header';
  const iframe = document.createElement('iframe');
  root.appendChild(iframe);
  document.body.appendChild(root);
  return iframe.contentWindow!;
}

describe('GPT immutable ad trace render attribution', () => {
  let bridge: (event: MessageEvent) => void;
  let module: typeof import('../../../src/integrations/gpt/index');
  let record: ReturnType<typeof vi.fn>;

  beforeEach(async () => {
    vi.resetModules();
    record = vi.fn();
    Object.defineProperty(navigator, 'sendBeacon', {
      value: vi.fn(),
      configurable: true,
      writable: true,
    });
    let generation = 0;
    window.tsjs = {
      recordAdTrace: record,
      nextAdTraceGeneration: () => ++generation,
      divToSlotId: { 'div-header': 'slot-a' },
      adSlots: [
        {
          id: 'slot-a',
          div_id: 'div-header',
          gam_unit_path: '/123/example',
          formats: [[300, 250]],
        },
      ],
      bids: {
        'slot-a': {
          hb_adid: 'old-ad-id',
          adm: '<div>Old creative</div>',
          nurl: 'https://billing.example/win',
          burl: 'https://billing.example/bill',
          trace: {
            version: 1,
            auctionTraceId: '750e8400-e29b-41d4-a716-446655440000',
            bidTraceId: OLD_TOKEN,
            source: 'initial_navigation',
            slotId: 'slot-a',
            provider: 'prebid',
            bidder: 'example-bidder',
          },
        },
      },
    } as any;
    const originalAdd = window.addEventListener.bind(window);
    const spy = vi
      .spyOn(window, 'addEventListener')
      .mockImplementation((type, listener, options) => {
        if (type === 'message') bridge = listener as (event: MessageEvent) => void;
        originalAdd(type, listener, options);
      });
    module = await import('../../../src/integrations/gpt/index');
    spy.mockRestore();
  });

  afterEach(() => {
    document.getElementById('div-header')?.remove();
    delete window.tsjs;
    vi.restoreAllMocks();
  });

  it('preserves authoritative missing values in a queued boundary snapshot', () => {
    const source = trustedSource();
    const port = { postMessage: vi.fn() };
    const stop = vi.fn();
    const beacon = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true);
    module.captureAdTraceRequest(slotWithTargeting({ hb_adid: 'old-ad-id' }) as any, 'bootstrap', {
      slotId: 'slot-a',
      bidder: undefined,
      adId: undefined,
      traceToken: undefined,
      bid: undefined,
    });

    bridge(
      Object.assign(new Event('message'), {
        data: JSON.stringify({ message: 'Prebid Request', adId: 'old-ad-id' }),
        ports: [port],
        source,
        stopImmediatePropagation: stop,
      }) as unknown as MessageEvent
    );

    expect(stop).toHaveBeenCalledOnce();
    expect(port.postMessage).not.toHaveBeenCalled();
    expect(beacon).not.toHaveBeenCalled();
  });

  it('never pairs a new client or refreshed TS adId with the stale live bid payload', () => {
    const source = trustedSource();
    const port = { postMessage: vi.fn() };
    const beacon = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true);

    for (const targeting of [
      { hb_adid: 'client-ad-id', hb_bidder: 'client-bidder' },
      { hb_adid: 'new-ts-ad-id', hb_bidder: 'example-bidder', ts_trace: NEW_TOKEN },
    ]) {
      window.tsjs!.prebidCorrelation = [
        {
          auctionId: 'auction-2',
          slotId: 'slot-a',
          requestId: 'request-2',
          adId: targeting.hb_adid,
          bidder: targeting.hb_bidder,
          ...(targeting.ts_trace ? { traceToken: targeting.ts_trace } : {}),
          ...(!targeting.ts_trace ? { events: ['prebid_bid_won' as const] } : {}),
        },
      ];
      module.captureAdTraceRequest(slotWithTargeting(targeting) as any, 'prebid_refresh');
      bridge(
        Object.assign(new Event('message'), {
          data: JSON.stringify({ message: 'Prebid Request', adId: targeting.hb_adid }),
          ports: [port],
          source,
          stopImmediatePropagation: vi.fn(),
        }) as unknown as MessageEvent
      );
    }

    expect(port.postMessage).not.toHaveBeenCalled();
    expect(beacon).not.toHaveBeenCalled();
    expect(record).toHaveBeenCalledWith(
      expect.objectContaining({ kind: 'prebid_targeting_selected', outcome: 'client_bid_won' })
    );
    expect(record).toHaveBeenCalledWith(
      expect.objectContaining({ kind: 'prebid_bid_won', generation: 1 })
    );
    expect(record).toHaveBeenCalledWith(
      expect.objectContaining({
        kind: 'prebid_targeting_selected',
        outcome: 'won',
        bidTraceId: NEW_TOKEN,
      })
    );

    window.tsjs!.prebidCorrelation = [
      {
        auctionId: 'auction-3',
        slotId: 'slot-a',
        requestId: 'client-request',
        adId: 'winning-client-ad',
        bidder: 'client-bidder',
      },
      {
        auctionId: 'auction-3',
        slotId: 'slot-a',
        requestId: 'ts-request',
        adId: 'losing-ts-ad',
        bidder: 'trustedServer',
        traceToken: NEW_TOKEN,
      },
    ];
    module.captureAdTraceRequest(
      slotWithTargeting({ hb_adid: 'winning-client-ad', hb_bidder: 'client-bidder' }) as any,
      'prebid_refresh'
    );
    expect(record).toHaveBeenCalledWith(
      expect.objectContaining({ kind: 'prebid_targeting_selected', outcome: 'lost' })
    );
  });

  it('serves, bills once, and acknowledges only the exact immutable generation/source/token', () => {
    const source = trustedSource();
    const foreignSource = window;
    const port = { postMessage: vi.fn() };
    const beacon = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true);
    module.captureAdTraceRequest(
      slotWithTargeting({
        hb_adid: 'old-ad-id',
        hb_bidder: 'example-bidder',
        ts_trace: OLD_TOKEN,
      }) as any,
      'display'
    );

    bridge(
      Object.assign(new Event('message'), {
        data: JSON.stringify({ message: 'Prebid Request', adId: 'old-ad-id' }),
        ports: [port],
        source,
        stopImmediatePropagation: vi.fn(),
      }) as unknown as MessageEvent
    );
    const response = JSON.parse(port.postMessage.mock.calls[0][0]);
    expect(response.traceToken).toBe(OLD_TOKEN);
    expect(response.ad).toBe('<div>Old creative</div>');
    expect(beacon).toHaveBeenCalledTimes(2);

    // A newer generation does not steal or invalidate the retained exact ack.
    const nextSlot = slotWithTargeting({ hb_adid: 'client-next', hb_bidder: 'client-bidder' });
    module.captureAdTraceRequest(nextSlot as any, 'prebid_refresh');

    bridge(
      Object.assign(new Event('message'), {
        data: { type: 'ts-creative-load', version: 1, traceToken: OLD_TOKEN },
        source: foreignSource,
      }) as unknown as MessageEvent
    );
    expect(record).not.toHaveBeenCalledWith(
      expect.objectContaining({ kind: 'creative_load_acknowledged' })
    );
    bridge(
      Object.assign(new Event('message'), {
        data: { type: 'ts-creative-load', version: 1, traceToken: NEW_TOKEN },
        source,
      }) as unknown as MessageEvent
    );
    expect(record).not.toHaveBeenCalledWith(
      expect.objectContaining({ kind: 'creative_load_acknowledged' })
    );

    bridge(
      Object.assign(new Event('message'), {
        data: { type: 'ts-creative-load', version: 1, traceToken: OLD_TOKEN },
        source,
      }) as unknown as MessageEvent
    );
    expect(record).toHaveBeenCalledWith(
      expect.objectContaining({
        kind: 'creative_load_acknowledged',
        generation: 1,
        bidTraceId: OLD_TOKEN,
      })
    );
    expect(beacon).toHaveBeenCalledTimes(2);

    module.supersedeAdTraceSlot(nextSlot as any, 'slot_destroyed');
    expect(record).toHaveBeenCalledWith(
      expect.objectContaining({
        kind: 'generation_superseded',
        generation: 2,
        reason: 'slot_destroyed',
      })
    );
  });

  it('rejects acknowledgements after the exact slot generation is superseded', () => {
    const source = trustedSource();
    const slot = slotWithTargeting({
      hb_adid: 'old-ad-id',
      hb_bidder: 'example-bidder',
      ts_trace: OLD_TOKEN,
    });
    const port = { postMessage: vi.fn() };
    vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true);
    module.captureAdTraceRequest(slot as any, 'display');
    bridge(
      Object.assign(new Event('message'), {
        data: JSON.stringify({ message: 'Prebid Request', adId: 'old-ad-id' }),
        ports: [port],
        source,
        stopImmediatePropagation: vi.fn(),
      }) as unknown as MessageEvent
    );
    module.supersedeAdTraceSlot(slot as any, 'slot_destroyed');
    record.mockClear();
    bridge(
      Object.assign(new Event('message'), {
        data: { type: 'ts-creative-load', version: 1, traceToken: OLD_TOKEN },
        source,
      }) as unknown as MessageEvent
    );
    expect(record).not.toHaveBeenCalledWith(
      expect.objectContaining({ kind: 'creative_load_acknowledged' })
    );
    expect(record).toHaveBeenCalledWith(
      expect.objectContaining({ kind: 'pb_render_rejected', reason: 'invalid_acknowledgement' })
    );
  });

  it('expires pending acknowledgements after thirty seconds', () => {
    let now = 0;
    vi.spyOn(performance, 'now').mockImplementation(() => now);
    const source = trustedSource();
    const slot = slotWithTargeting({
      hb_adid: 'old-ad-id',
      hb_bidder: 'example-bidder',
      ts_trace: OLD_TOKEN,
    });
    const port = { postMessage: vi.fn() };
    vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true);
    module.captureAdTraceRequest(slot as any, 'display');
    bridge(
      Object.assign(new Event('message'), {
        data: JSON.stringify({ message: 'Prebid Request', adId: 'old-ad-id' }),
        ports: [port],
        source,
        stopImmediatePropagation: vi.fn(),
      }) as unknown as MessageEvent
    );
    now = 30_001;
    record.mockClear();
    bridge(
      Object.assign(new Event('message'), {
        data: { type: 'ts-creative-load', version: 1, traceToken: OLD_TOKEN },
        source,
      }) as unknown as MessageEvent
    );
    expect(record).not.toHaveBeenCalledWith(
      expect.objectContaining({ kind: 'creative_load_acknowledged' })
    );
    expect(record).toHaveBeenCalledWith(
      expect.objectContaining({ kind: 'generation_superseded', reason: 'ack_expired' })
    );
  });
});
