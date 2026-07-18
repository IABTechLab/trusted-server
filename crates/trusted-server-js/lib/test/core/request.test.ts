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

  it('does not blank the slot when a later bid for the same slot is rejected', async () => {
    // Regression: multi-bid scenario where a rejected bid must not erase an earlier
    // successful render into the same slot.
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

    document.body.innerHTML = '<div id="slot1"></div>';
    addAdUnits({ code: 'slot1', mediaTypes: { banner: { sizes: [[300, 250]] } } } as any);

    requestAds();
    await flushRequestAds();

    // The good creative should have rendered; the bad one should not have blanked it.
    const iframe = document.querySelector('#slot1 iframe') as HTMLIFrameElement | null;
    expect(iframe).toBeTruthy();
    expect(iframe!.srcdoc).toContain(goodCreative);
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

  it('stamps trace markers and records the render in window.tsjs.renders', async () => {
    const creativeHtml = '<div>Traced Creative</div>';
    (globalThis as any).fetch = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      headers: { get: () => 'application/json' },
      json: async () => ({
        id: 'auction-trace-1',
        seatbid: [
          {
            seat: 'kargo',
            bid: [
              {
                impid: 'slot1',
                adm: creativeHtml,
                crid: 'cr-777',
                ext: { ts: { auction_id: 'auction-trace-1', adm_hash: 'a1b2c3d4e5f60718' } },
              },
            ],
          },
        ],
      }),
    });

    const { addAdUnits } = await import('../../src/core/registry');
    const { requestAds } = await import('../../src/core/request');
    const { RENDER_EVENT_NAME } = await import('../../src/core/trace');
    const eventListener = vi.fn();
    window.addEventListener(RENDER_EVENT_NAME, eventListener);

    document.body.innerHTML = '<div id="slot1"></div>';
    addAdUnits({ code: 'slot1', mediaTypes: { banner: { sizes: [[300, 250]] } } } as any);

    requestAds();
    await flushRequestAds();

    const container = document.querySelector('#slot1') as HTMLElement;
    expect(container.getAttribute('data-ts-slot-id')).toBe('slot1');
    expect(container.getAttribute('data-ts-render-path')).toBe('auction');
    expect(container.getAttribute('data-ts-rendered')).toBe('true');
    expect(container.getAttribute('data-ts-auction-id')).toBe('auction-trace-1');
    expect(container.getAttribute('data-ts-bidder')).toBe('kargo');
    expect(container.getAttribute('data-ts-creative-id')).toBe('cr-777');
    expect(container.getAttribute('data-ts-adm-hash')).toBe('a1b2c3d4e5f60718');

    const iframe = container.querySelector('iframe') as HTMLIFrameElement;
    expect(iframe.getAttribute('data-ts-slot-id')).toBe('slot1');
    expect(iframe.getAttribute('data-ts-adm-hash')).toBe('a1b2c3d4e5f60718');

    const record = (window as any).tsjs?.renders?.['slot1'];
    expect(record).toEqual(
      expect.objectContaining({
        slotId: 'slot1',
        path: 'auction',
        rendered: true,
        elementId: 'slot1',
        auctionId: 'auction-trace-1',
        bidder: 'kargo',
        creativeId: 'cr-777',
        admHash: 'a1b2c3d4e5f60718',
        servedFrom: 'inline',
      })
    );
    expect(eventListener).toHaveBeenCalledTimes(1);

    window.removeEventListener(RENDER_EVENT_NAME, eventListener);
  });

  it('records a rendered:false trace entry when the creative is rejected', async () => {
    (globalThis as any).fetch = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      headers: { get: () => 'application/json' },
      json: async () => ({
        id: 'auction-trace-2',
        seatbid: [
          {
            seat: 'appnexus',
            bid: [{ impid: 'slot1', adm: '   ', crid: 'creative-empty' }],
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

    const record = (window as any).tsjs?.renders?.['slot1'];
    expect(record).toEqual(
      expect.objectContaining({
        slotId: 'slot1',
        path: 'auction',
        rendered: false,
        auctionId: 'auction-trace-2',
      })
    );
    // Rejected creative must stamp an explicit rendered:false marker,
    // matching the SSAT path's empty-render semantics.
    expect(document.querySelector('#slot1')?.getAttribute('data-ts-rendered')).toBe('false');
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
