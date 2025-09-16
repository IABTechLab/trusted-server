import { describe, it, expect, beforeEach, vi } from 'vitest';

// Ensure mocks referenced inside vi.mock factory are hoisted
const { renderMock } = vi.hoisted(() => ({ renderMock: vi.fn() }));

describe('request.requestAds', () => {
  beforeEach(async () => {
    await vi.resetModules();
    document.body.innerHTML = '';
    renderMock.mockReset();
  });

  it('sends fetch and renders creatives from response', async () => {
    // mock render module to capture calls
    vi.mock('../../src/core/render', async () => {
      const actual = await vi.importActual<any>('../../src/core/render');
      return {
        ...actual,
        renderCreativeIntoSlot: (slotId: string, html: string) => renderMock(slotId, html),
      };
    });

    // mock fetch
    (globalThis as any).fetch = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      headers: { get: () => 'application/json' },
      json: async () => ({ seatbid: [{ bid: [{ impid: 'slot1', adm: '<div>ad</div>' }] }] }),
    });

    const { addAdUnits } = await import('../../src/core/registry');
    const { setConfig } = await import('../../src/core/config');
    const { requestAds } = await import('../../src/core/request');

    document.body.innerHTML = '<div id="slot1"></div>';
    addAdUnits({ code: 'slot1', mediaTypes: { banner: { sizes: [[300, 250]] } } } as any);
    setConfig({ mode: 'thirdParty' } as any);

    requestAds();
    // wait microtasks
    await Promise.resolve();
    await Promise.resolve();

    expect((globalThis as any).fetch).toHaveBeenCalled();
    expect(renderMock).toHaveBeenCalledWith('slot1', '<div>ad</div>');
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

  it('inserts an iframe per ad unit with correct src (firstParty)', async () => {
    const { addAdUnits } = await import('../../src/core/registry');
    const { setConfig } = await import('../../src/core/config');
    const { requestAds } = await import('../../src/core/request');

    // Prepare slot in DOM
    const div = document.createElement('div');
    div.id = 'slot1';
    document.body.appendChild(div);

    // Configure first-party mode explicitly
    setConfig({ mode: 'firstParty' } as any);

    // Add an ad unit and request
    addAdUnits({ code: 'slot1', mediaTypes: { banner: { sizes: [[300, 250]] } } } as any);
    requestAds();

    // Verify iframe was inserted with expected src
    const iframe = document.querySelector('#slot1 iframe') as HTMLIFrameElement | null;
    expect(iframe).toBeTruthy();
    expect(iframe!.getAttribute('src')).toContain('/first-party/ad?');
    expect(iframe!.getAttribute('src')).toContain('slot=slot1');
    expect(iframe!.getAttribute('src')).toContain('w=300');
    expect(iframe!.getAttribute('src')).toContain('h=250');
  });

  it('skips iframe insertion when slot is missing (firstParty)', async () => {
    const { addAdUnits } = await import('../../src/core/registry');
    const { setConfig } = await import('../../src/core/config');
    const { requestAds } = await import('../../src/core/request');

    setConfig({ mode: 'firstParty' } as any);
    addAdUnits({ code: 'missing-slot', mediaTypes: { banner: { sizes: [[300, 250]] } } } as any);
    requestAds();

    // No iframe should be inserted because the slot isn't present
    const iframe = document.querySelector('iframe');
    expect(iframe).toBeNull();
  });
});
