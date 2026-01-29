import { describe, it, expect, beforeEach, vi } from 'vitest';

// Ensure mocks referenced inside vi.mock factory are hoisted
const { renderMock } = vi.hoisted(() => ({ renderMock: vi.fn() }));

describe('request.requestAds', () => {
  beforeEach(async () => {
    await vi.resetModules();
    document.body.innerHTML = '';
    renderMock.mockReset();
  });

  it('sends fetch and renders creatives via iframe from response', async () => {
    // mock fetch - returns creative HTML inline in adm field
    const creativeHtml = '<div>Test Creative</div>';
    (globalThis as any).fetch = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      headers: { get: () => 'application/json' },
      json: async () => ({
        seatbid: [{ bid: [{ impid: 'slot1', adm: creativeHtml }] }],
      }),
    });

    const { addAdUnits } = await import('../../src/core/registry');
    const { requestAds } = await import('../../src/core/request');

    document.body.innerHTML = '<div id="slot1"></div>';
    addAdUnits({ code: 'slot1', mediaTypes: { banner: { sizes: [[300, 250]] } } } as any);

    requestAds();
    // wait microtasks
    await Promise.resolve();
    await Promise.resolve();

    expect((globalThis as any).fetch).toHaveBeenCalled();

    // Verify iframe was created with creative HTML in srcdoc
    const iframe = document.querySelector('#slot1 iframe') as HTMLIFrameElement | null;
    expect(iframe).toBeTruthy();
    expect(iframe!.srcdoc).toContain(creativeHtml);
  });

  it('handles unexpected third-party response without rendering', async () => {
    vi.mock('../../src/core/render', async () => {
      const actual = await vi.importActual<any>('../../src/core/render');
      return {
        ...actual,
        renderCreativeIntoSlot: (slotId: string, html: string) => renderMock(slotId, html),
      };
    });

    (globalThis as any).fetch = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      headers: { get: () => 'text/plain' },
      json: async () => ({}),
    });

    const { addAdUnits } = await import('../../src/core/registry');
    const { setConfig } = await import('../../src/core/config');
    const { requestAds } = await import('../../src/core/request');

    addAdUnits({ code: 'slot1', mediaTypes: { banner: { sizes: [[300, 250]] } } } as any);
    setConfig({ mode: 'thirdParty' } as any);

    requestAds();
    await Promise.resolve();
    await Promise.resolve();

    expect((globalThis as any).fetch).toHaveBeenCalled();
    expect(renderMock).not.toHaveBeenCalled();
  });

  it('ignores fetch rejection gracefully', async () => {
    vi.mock('../../src/core/render', async () => {
      const actual = await vi.importActual<any>('../../src/core/render');
      return {
        ...actual,
        renderCreativeIntoSlot: (slotId: string, html: string) => renderMock(slotId, html),
      };
    });

    (globalThis as any).fetch = vi.fn().mockRejectedValue(new Error('network-error'));

    const { addAdUnits } = await import('../../src/core/registry');
    const { setConfig } = await import('../../src/core/config');
    const { requestAds } = await import('../../src/core/request');

    addAdUnits({ code: 'slot1', mediaTypes: { banner: { sizes: [[300, 250]] } } } as any);
    setConfig({ mode: 'thirdParty' } as any);

    requestAds();
    await Promise.resolve();
    await Promise.resolve();

    expect((globalThis as any).fetch).toHaveBeenCalled();
    expect(renderMock).not.toHaveBeenCalled();
  });

  it('inserts an iframe with creative HTML from unified auction', async () => {
    // mock fetch for unified auction endpoint - returns inline HTML
    const creativeHtml = '<img src="/first-party/proxy?tsurl=...">Ad</img>';
    (globalThis as any).fetch = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      headers: { get: () => 'application/json' },
      json: async () => ({
        seatbid: [{ bid: [{ impid: 'slot1', adm: creativeHtml }] }],
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

    await Promise.resolve();
    await Promise.resolve();

    // Verify iframe was inserted with creative HTML in srcdoc
    const iframe = document.querySelector('#slot1 iframe') as HTMLIFrameElement | null;
    expect(iframe).toBeTruthy();
    expect(iframe!.srcdoc).toContain(creativeHtml);
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

    await Promise.resolve();
    await Promise.resolve();

    // No iframe should be inserted because the slot isn't present in DOM
    const iframe = document.querySelector('iframe');
    expect(iframe).toBeNull();
  });
});
