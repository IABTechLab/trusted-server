import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

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
        sanitizedLength: creativeHtml.length,
      })
    );
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

  it('rejects creatives with stripped executable content without logging raw HTML', async () => {
    const creativeHtml = '<img src="/track.png" onerror="alert(1)"><script>alert(2)</script>';
    (globalThis as any).fetch = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      headers: { get: () => 'application/json' },
      json: async () => ({
        seatbid: [
          {
            seat: 'appnexus',
            bid: [{ impid: 'slot1', adm: creativeHtml, crid: 'creative-danger' }],
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
    expect(document.querySelector('#slot1')?.innerHTML).toBe('');

    const rejectionCall = warnSpy.mock.calls.find(
      ([message]) => message === 'renderCreativeInline: rejected creative'
    );
    expect(rejectionCall?.[1]).toEqual(
      expect.objectContaining({
        slotId: 'slot1',
        seat: 'appnexus',
        creativeId: 'creative-danger',
        rejectionReason: 'removed-dangerous-content',
      })
    );
    expect(JSON.stringify(rejectionCall)).not.toContain('<script>');
    expect(JSON.stringify(rejectionCall)).not.toContain('onerror');
  });

  it('rejects creatives with dangerous URI attributes without logging raw HTML', async () => {
    const creativeHtml = '<a href="javascript:alert(1)">danger</a>';
    (globalThis as any).fetch = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      headers: { get: () => 'application/json' },
      json: async () => ({
        seatbid: [
          {
            seat: 'appnexus',
            bid: [{ impid: 'slot1', adm: creativeHtml, crid: 'creative-uri-danger' }],
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
    expect(document.querySelector('#slot1')?.innerHTML).toBe('');

    const rejectionCall = warnSpy.mock.calls.find(
      ([message]) => message === 'renderCreativeInline: rejected creative'
    );
    expect(rejectionCall?.[1]).toEqual(
      expect.objectContaining({
        slotId: 'slot1',
        seat: 'appnexus',
        creativeId: 'creative-uri-danger',
        rejectionReason: 'removed-dangerous-content',
      })
    );
    expect(JSON.stringify(rejectionCall)).not.toContain('javascript:alert(1)');
  });

  it('rejects creatives with dangerous inline styles that survive sanitization', async () => {
    const creativeHtml = '<div style="background-image:url(javascript:alert(1))">danger</div>';
    (globalThis as any).fetch = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      headers: { get: () => 'application/json' },
      json: async () => ({
        seatbid: [
          {
            seat: 'appnexus',
            bid: [{ impid: 'slot1', adm: creativeHtml, crid: 'creative-style-danger' }],
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
    expect(document.querySelector('#slot1')?.innerHTML).toBe('');

    const rejectionCall = warnSpy.mock.calls.find(
      ([message]) => message === 'renderCreativeInline: rejected creative'
    );
    expect(rejectionCall?.[1]).toEqual(
      expect.objectContaining({
        slotId: 'slot1',
        seat: 'appnexus',
        creativeId: 'creative-style-danger',
        rejectionReason: 'removed-dangerous-content',
      })
    );
    expect(JSON.stringify(rejectionCall)).not.toContain('background-image');
    expect(JSON.stringify(rejectionCall)).not.toContain('javascript:alert(1)');
  });

  it('rejects malformed non-string creative HTML', async () => {
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
    expect(document.querySelector('#slot1')?.innerHTML).toBe('');

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
