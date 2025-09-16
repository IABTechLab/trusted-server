import { beforeEach, afterEach, describe, expect, it, vi } from 'vitest';

const ORIGINAL_FETCH = global.fetch;

const FIRST_PARTY_CLICK =
  '/first-party/click?tsurl=https%3A%2F%2Fexample.com%2Flanding&foo=1&tstoken=token123';
const MUTATED_CLICK = 'https://example.com/landing?bar=2';
const PROXY_RESPONSE =
  '/first-party/click?tsurl=https%3A%2F%2Fexample.com%2Flanding&bar=2&tstoken=newtoken';

async function importCreativeModule() {
  delete (globalThis as any).__ts_creative_installed;
  await import('../src/creative/index');
}

describe('tsjs creative guard', () => {
  beforeEach(() => {
    vi.resetModules();
    document.body.innerHTML = '';
  });

  afterEach(() => {
    global.fetch = ORIGINAL_FETCH;
    vi.useRealTimers();
  });

  it('repairs anchor href using proxy rebuild fallback when fetch is unavailable', async () => {
    vi.useFakeTimers();
    global.fetch = undefined as any;

    const anchor = document.createElement('a');
    anchor.setAttribute('data-tsclick', FIRST_PARTY_CLICK);
    anchor.setAttribute('href', FIRST_PARTY_CLICK);
    document.body.appendChild(anchor);

    await importCreativeModule();

    anchor.setAttribute('href', MUTATED_CLICK);

    await Promise.resolve();
    await vi.runAllTimersAsync();

    const finalHref = anchor.getAttribute('href') ?? '';
    expect(finalHref.startsWith('/first-party/proxy-rebuild?')).toBe(true);
    expect(finalHref).toContain('add=%7B%22bar%22%3A%222%22%7D');
    expect(finalHref).toContain('del=%5B%22foo%22%5D');
  });

  it('updates href and data-tsclick using proxy rebuild response', async () => {
    vi.useFakeTimers();

    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({ href: PROXY_RESPONSE }),
    });
    global.fetch = fetchMock as unknown as typeof fetch;

    const anchor = document.createElement('a');
    anchor.setAttribute('data-tsclick', FIRST_PARTY_CLICK);
    anchor.setAttribute('href', FIRST_PARTY_CLICK);
    document.body.appendChild(anchor);

    await importCreativeModule();

    anchor.setAttribute('href', MUTATED_CLICK);

    await Promise.resolve();
    await vi.runAllTimersAsync();

    expect(fetchMock).toHaveBeenCalled();
    const call = fetchMock.mock.calls[0];
    expect(call[0]).toBe('/first-party/proxy-rebuild');
    const payload = JSON.parse(call[1]?.body as string);
    expect(payload).toEqual({
      tsclick: FIRST_PARTY_CLICK,
      add: { bar: '2' },
      del: ['foo'],
    });

    expect(anchor.getAttribute('href')).toBe(PROXY_RESPONSE);
    expect(anchor.getAttribute('data-tsclick')).toBe(PROXY_RESPONSE);
  });
});
