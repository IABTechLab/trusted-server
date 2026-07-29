import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import envelope from '../fixtures/aps-renderer-v1.json';

async function flushRequestAds(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 0));
}

describe('request.requestAds', () => {
  let originalFetch: typeof globalThis.fetch;

  beforeEach(async () => {
    await vi.resetModules();
    document.body.innerHTML = '';
    delete window.tsjs;
    originalFetch = globalThis.fetch;
  });

  afterEach(() => {
    globalThis.fetch = originalFetch;
    vi.restoreAllMocks();
  });

  it('sends fetch and renders creatives via iframe from response', async () => {
    // mock fetch - returns creative HTML inline in adm field
    const creativeHtml = '<div>Test Creative</div>';
    (globalThis as any).fetch = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      headers: { get: () => 'application/json' },
      json: async () => ({
        seatbid: [
          {
            seat: 'trusted-server',
            bid: [{ impid: 'slot1', adm: creativeHtml, crid: 'creative-1' }],
          },
        ],
      }),
    });

    const { addAdUnits } = await import('../../src/core/registry');
    const { log } = await import('../../src/core/log');
    const { requestAds } = await import('../../src/core/request');
    const infoSpy = vi.spyOn(log, 'info').mockImplementation(() => undefined);

    document.body.innerHTML = '<div id="slot1"></div>';
    addAdUnits({ code: 'slot1', mediaTypes: { banner: { sizes: [[300, 250]] } } } as any);

    requestAds();
    await flushRequestAds();

    expect((globalThis as any).fetch).toHaveBeenCalled();

    // Verify iframe was created with creative HTML in srcdoc
    const iframe = document.querySelector('#slot1 iframe') as HTMLIFrameElement | null;
    expect(iframe).toBeTruthy();
    expect(iframe!.srcdoc).toContain(creativeHtml);

    const renderCall = infoSpy.mock.calls.find(
      ([message]) => message === 'renderCreativeInline: rendered'
    );
    expect(renderCall?.[1]).toEqual(
      expect.objectContaining({
        slotId: 'slot1',
        seat: 'trusted-server',
        creativeId: 'creative-1',
        originalLength: creativeHtml.length,
      })
    );
  });

  it('dispatches a valid APS descriptor to the opaque static renderer route', async () => {
    const apsBid = envelope.seatbid[0].bid[0];
    const renderer = {
      type: 'aps',
      version: 1,
      accountId: 'example-account-id',
      bidId: apsBid.id,
      tagType: apsBid.ext.tagtype,
      creativeUrl: apsBid.ext.creativeurl,
      aaxResponse: btoa(JSON.stringify(envelope)),
      width: apsBid.w,
      height: apsBid.h,
    };
    (globalThis as any).fetch = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      headers: { get: () => 'application/json' },
      json: async () => ({
        seatbid: [
          {
            seat: 'aps',
            bid: [
              {
                impid: 'slot1',
                price: 1.23,
                w: 300,
                h: 250,
                ext: { trusted_server: { renderer } },
              },
            ],
          },
        ],
      }),
    });

    const { addAdUnits } = await import('../../src/core/registry');
    const { requestAds } = await import('../../src/core/request');
    document.body.innerHTML = '<div id="slot1"><span>existing</span></div>';
    addAdUnits({ code: 'slot1', mediaTypes: { banner: { sizes: [[300, 250]] } } } as any);

    requestAds();
    await flushRequestAds();

    const iframe = document.querySelector('#slot1 iframe') as HTMLIFrameElement | null;
    expect(iframe).not.toBeNull();
    expect(iframe!.src).toContain('/integrations/aps/renderer#tsaps=');
    expect(iframe!.srcdoc).toBe('');
    expect(iframe!.getAttribute('sandbox')).not.toContain('allow-same-origin');
    expect(document.querySelector('#slot1 span')).not.toBeNull();

    const postMessage = vi.spyOn(iframe!.contentWindow!, 'postMessage');
    iframe!.dispatchEvent(new Event('load'));
    expect(document.querySelector('#slot1 span')).not.toBeNull();
    expect(postMessage).toHaveBeenCalledWith(expect.objectContaining({ renderer }), '*');

    const message = postMessage.mock.calls[0][0] as { nonce: string };
    window.dispatchEvent(
      new MessageEvent('message', {
        data: { message: 'trusted-server/aps/renderer-ready', nonce: message.nonce },
        source: iframe!.contentWindow,
      })
    );
    expect(document.querySelector('#slot1 span')).toBeNull();
  });

  it('does not mutate the slot for an invalid APS descriptor', async () => {
    (globalThis as any).fetch = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      headers: { get: () => 'application/json' },
      json: async () => ({
        seatbid: [
          {
            seat: 'aps',
            bid: [
              {
                impid: 'slot1',
                ext: {
                  trusted_server: {
                    renderer: { type: 'aps', version: 1, aaxResponse: 'invalid' },
                  },
                },
              },
            ],
          },
        ],
      }),
    });

    const { addAdUnits } = await import('../../src/core/registry');
    const { requestAds } = await import('../../src/core/request');
    document.body.innerHTML = '<div id="slot1"><span>existing</span></div>';
    addAdUnits({ code: 'slot1', mediaTypes: { banner: { sizes: [[300, 250]] } } } as any);

    requestAds();
    await flushRequestAds();

    expect(document.querySelector('#slot1 iframe')).toBeNull();
    expect(document.querySelector('#slot1 span')).not.toBeNull();
  });

  it('does not render on non-JSON response', async () => {
    (globalThis as any).fetch = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      headers: { get: () => 'text/plain' },
      json: async () => ({}),
    });

    const { addAdUnits } = await import('../../src/core/registry');
    const { requestAds } = await import('../../src/core/request');

    document.body.innerHTML = '<div id="slot1"></div>';
    addAdUnits({ code: 'slot1', mediaTypes: { banner: { sizes: [[300, 250]] } } } as any);

    requestAds();
    await flushRequestAds();

    expect((globalThis as any).fetch).toHaveBeenCalled();
    expect(document.querySelector('iframe')).toBeNull();
  });

  it('ignores fetch rejection gracefully', async () => {
    (globalThis as any).fetch = vi.fn().mockRejectedValue(new Error('network-error'));

    const { addAdUnits } = await import('../../src/core/registry');
    const { requestAds } = await import('../../src/core/request');

    document.body.innerHTML = '<div id="slot1"></div>';
    addAdUnits({ code: 'slot1', mediaTypes: { banner: { sizes: [[300, 250]] } } } as any);

    requestAds();
    await flushRequestAds();

    expect((globalThis as any).fetch).toHaveBeenCalled();
    expect(document.querySelector('iframe')).toBeNull();
  });

  it('inserts an iframe with creative HTML from unified auction', async () => {
    // mock fetch for unified auction endpoint - returns inline HTML
    const creativeHtml = '<img src="/first-party/proxy?tsurl=...">Ad</img>';
    (globalThis as any).fetch = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      headers: { get: () => 'application/json' },
      json: async () => ({
        seatbid: [
          {
            seat: 'trusted-server',
            bid: [{ impid: 'slot1', adm: creativeHtml, crid: 'creative-2' }],
          },
        ],
      }),
    });

    const { addAdUnits } = await import('../../src/core/registry');
    const { requestAds } = await import('../../src/core/request');

    // Prepare slot in DOM
    const div = document.createElement('div');
    div.id = 'slot1';
    document.body.appendChild(div);

    // Add an ad unit and request
    addAdUnits({ code: 'slot1', mediaTypes: { banner: { sizes: [[300, 250]] } } } as any);
    requestAds();

    await flushRequestAds();

    // Verify iframe was inserted with creative HTML in srcdoc
    const iframe = document.querySelector('#slot1 iframe') as HTMLIFrameElement | null;
    expect(iframe).toBeTruthy();
    expect(iframe!.srcdoc).toContain('<img src="/first-party/proxy?tsurl=...">');
    expect(iframe!.srcdoc).toContain('Ad');
  });

  it('renders creatives with safe URI markup', async () => {
    const creativeHtml =
      '<a href="mailto:test@example.com">Contact</a><img src="https://example.com/ad.png" alt="ad">';
    (globalThis as any).fetch = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      headers: { get: () => 'application/json' },
      json: async () => ({
        seatbid: [
          {
            seat: 'trusted-server',
            bid: [{ impid: 'slot1', adm: creativeHtml, crid: 'creative-safe-uri' }],
          },
        ],
      }),
    });

    const { addAdUnits } = await import('../../src/core/registry');
    const { requestAds } = await import('../../src/core/request');

    document.body.innerHTML = '<div id="slot1"></div>';
    addAdUnits({ code: 'slot1', mediaTypes: { banner: { sizes: [[300, 250]] } } } as any);

    requestAds();
    await flushRequestAds();

    const iframe = document.querySelector('#slot1 iframe') as HTMLIFrameElement | null;
    expect(iframe).toBeTruthy();
    expect(iframe!.srcdoc).toContain('mailto:test@example.com');
    expect(iframe!.srcdoc).toContain('https://example.com/ad.png');
  });

  it('rejects malformed non-string creative HTML without blanking the slot', async () => {
    (globalThis as any).fetch = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      headers: { get: () => 'application/json' },
      json: async () => ({
        seatbid: [
          {
            seat: 'appnexus',
            bid: [{ impid: 'slot1', adm: { html: '<div>bad</div>' }, crid: 'creative-invalid' }],
          },
        ],
      }),
    });

    const { addAdUnits } = await import('../../src/core/registry');
    const { log } = await import('../../src/core/log');
    const { requestAds } = await import('../../src/core/request');
    const warnSpy = vi.spyOn(log, 'warn').mockImplementation(() => undefined);

    document.body.innerHTML = '<div id="slot1"><span>existing</span></div>';
    addAdUnits({ code: 'slot1', mediaTypes: { banner: { sizes: [[300, 250]] } } } as any);

    requestAds();
    await flushRequestAds();

    expect(document.querySelector('#slot1 iframe')).toBeNull();
    // Invalid-type rejection must not blank existing slot content.
    expect(document.querySelector('#slot1')?.innerHTML).toBe('<span>existing</span>');

    const rejectionCall = warnSpy.mock.calls.find(
      ([message]) => message === 'renderCreativeInline: rejected creative'
    );
    expect(rejectionCall?.[1]).toEqual(
      expect.objectContaining({
        slotId: 'slot1',
        seat: 'appnexus',
        creativeId: 'creative-invalid',
        rejectionReason: 'invalid-creative-html',
      })
    );
    expect(JSON.stringify(rejectionCall)).not.toContain('[object Object]');
  });

  it('rejects an ambiguous multi-winner response without blanking the slot', async () => {
    // A final auction response must contain at most one winner per requested slot.
    const goodCreative = '<div>Safe Ad</div>';
    (globalThis as any).fetch = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      headers: { get: () => 'application/json' },
      json: async () => ({
        seatbid: [
          {
            seat: 'seat-a',
            bid: [{ impid: 'slot1', adm: goodCreative, crid: 'creative-good' }],
          },
          {
            // Non-string adm is rejected client-side as invalid-creative-html.
            seat: 'seat-b',
            bid: [{ impid: 'slot1', adm: { html: '<div>bad</div>' }, crid: 'creative-bad' }],
          },
        ],
      }),
    });

    const { addAdUnits } = await import('../../src/core/registry');
    const { requestAds } = await import('../../src/core/request');

    document.body.innerHTML = '<div id="slot1"><span>existing</span></div>';
    addAdUnits({ code: 'slot1', mediaTypes: { banner: { sizes: [[300, 250]] } } } as any);

    requestAds();
    await flushRequestAds();

    expect(document.querySelector('#slot1 iframe')).toBeNull();
    expect(document.querySelector('#slot1')?.textContent).toContain('existing');
  });

  it('rejects creatives that sanitize to empty markup', async () => {
    (globalThis as any).fetch = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      headers: { get: () => 'application/json' },
      json: async () => ({
        seatbid: [
          {
            seat: 'appnexus',
            bid: [{ impid: 'slot1', adm: '   ', crid: 'creative-empty' }],
          },
        ],
      }),
    });

    const { addAdUnits } = await import('../../src/core/registry');
    const { log } = await import('../../src/core/log');
    const { requestAds } = await import('../../src/core/request');
    const warnSpy = vi.spyOn(log, 'warn').mockImplementation(() => undefined);

    document.body.innerHTML = '<div id="slot1"></div>';
    addAdUnits({ code: 'slot1', mediaTypes: { banner: { sizes: [[300, 250]] } } } as any);

    requestAds();
    await flushRequestAds();

    expect(document.querySelector('#slot1 iframe')).toBeNull();

    const rejectionCall = warnSpy.mock.calls.find(
      ([message]) => message === 'renderCreativeInline: rejected creative'
    );
    expect(rejectionCall?.[1]).toEqual(
      expect.objectContaining({
        slotId: 'slot1',
        seat: 'appnexus',
        creativeId: 'creative-empty',
        rejectionReason: 'empty-after-sanitize',
      })
    );
  });

  it('keeps the latest direct owner when overlapping responses resolve out of order', async () => {
    const resolves: Array<(response: Response) => void> = [];
    (globalThis as any).fetch = vi.fn().mockImplementation(
      () =>
        new Promise<Response>((resolve) => {
          resolves.push(resolve);
        })
    );
    const recordAdTrace = vi.fn();
    window.tsjs = {
      recordAdTrace,
      nextAdTraceGeneration: vi.fn().mockReturnValueOnce(1).mockReturnValueOnce(2),
    } as any;
    const { addAdUnits } = await import('../../src/core/registry');
    const { requestAds } = await import('../../src/core/request');
    document.body.innerHTML = '<div id="slot1"><span>existing</span></div>';
    addAdUnits({ code: 'slot1', mediaTypes: { banner: { sizes: [[300, 250]] } } } as any);

    requestAds();
    requestAds();
    expect(resolves).toHaveLength(2);
    const response = (creative: string) =>
      ({
        ok: true,
        status: 200,
        headers: { get: () => 'application/json' },
        json: async () => ({
          seatbid: [{ seat: 'trusted-server', bid: [{ impid: 'slot1', adm: creative }] }],
        }),
      }) as Response;

    resolves[1](response('<div>new owner</div>'));
    await flushRequestAds();
    resolves[0](response('<div>stale owner</div>'));
    await flushRequestAds();

    const iframe = document.querySelector('#slot1 iframe') as HTMLIFrameElement;
    expect(iframe.srcdoc).toContain('new owner');
    expect(iframe.srcdoc).not.toContain('stale owner');
    expect(recordAdTrace).toHaveBeenCalledWith(
      expect.objectContaining({
        kind: 'direct_render_rejected',
        generation: 1,
        reason: 'direct_owner_replaced',
      })
    );
  });

  it('records an exact direct auction winner, placement, and iframe load', async () => {
    const auctionTraceId = '550e8400-e29b-41d4-a716-446655440000';
    const bidTraceId = '123e4567-e89b-42d3-a456-426614174000';
    const recordAdTrace = vi.fn();
    window.tsjs = {
      recordAdTrace,
      nextAdTraceGeneration: vi.fn().mockReturnValue(1),
    } as any;
    (globalThis as any).fetch = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      headers: { get: () => 'application/json' },
      json: async () => ({
        ext: {
          trusted_server: {
            trace: {
              version: 1,
              auction_trace_id: auctionTraceId,
              source: 'auction_api',
              outcome: 'completed',
            },
          },
        },
        seatbid: [
          {
            seat: 'trusted-server',
            bid: [
              {
                impid: 'slot1',
                adm: '<div>direct</div>',
                ext: {
                  trusted_server: {
                    trace: {
                      version: 1,
                      auction_trace_id: auctionTraceId,
                      bid_trace_id: bidTraceId,
                      source: 'auction_api',
                      slot_id: 'slot1',
                      provider: 'prebid',
                      bidder: 'example',
                    },
                  },
                },
              },
            ],
          },
        ],
      }),
    });

    const { addAdUnits } = await import('../../src/core/registry');
    const { requestAds } = await import('../../src/core/request');
    document.body.innerHTML = '<div id="slot1"></div>';
    addAdUnits({ code: 'slot1', mediaTypes: { banner: { sizes: [[300, 250]] } } } as any);

    requestAds();
    await flushRequestAds();
    expect(recordAdTrace).toHaveBeenCalledWith(
      expect.objectContaining({
        kind: 'ts_winner_observed',
        generation: 1,
        auctionTraceId,
        bidTraceId,
      })
    );
    expect(recordAdTrace).toHaveBeenCalledWith(
      expect.objectContaining({ kind: 'pb_render_served', reason: 'direct_iframe_created' })
    );

    const iframe = document.querySelector('#slot1 iframe') as HTMLIFrameElement;
    iframe.dispatchEvent(new Event('load'));
    expect(recordAdTrace).toHaveBeenCalledWith(
      expect.objectContaining({
        kind: 'creative_load_acknowledged',
        generation: 1,
        reason: 'direct_iframe_load',
      })
    );
  });

  it('skips iframe insertion when slot is missing', async () => {
    // mock fetch for unified auction endpoint - returns inline HTML
    (globalThis as any).fetch = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      headers: { get: () => 'application/json' },
      json: async () => ({
        seatbid: [
          {
            bid: [{ impid: 'missing-slot', adm: '<div>Creative for missing slot</div>' }],
          },
        ],
      }),
    });

    const { addAdUnits } = await import('../../src/core/registry');
    const { requestAds } = await import('../../src/core/request');

    addAdUnits({ code: 'missing-slot', mediaTypes: { banner: { sizes: [[300, 250]] } } } as any);
    requestAds();

    await flushRequestAds();

    // No iframe should be inserted because the slot isn't present in DOM
    const iframe = document.querySelector('iframe');
    expect(iframe).toBeNull();
  });
});
