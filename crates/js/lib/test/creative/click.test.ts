import { beforeEach, afterEach, describe, expect, it, vi } from 'vitest';

import {
  FIRST_PARTY_CLICK,
  MUTATED_CLICK,
  PROXY_RESPONSE,
  importCreativeModule,
} from './helpers';

const ORIGINAL_FETCH = global.fetch;

describe('creative/click.ts', () => {
  beforeEach(() => {
    vi.resetModules();
    document.body.innerHTML = '';
  });

  afterEach(() => {
    global.fetch = ORIGINAL_FETCH;
    vi.useRealTimers();
  });

  it('repairs anchors via proxy rebuild fallback when fetch is unavailable', async () => {
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

  it('updates anchors using proxy rebuild response payload', async () => {
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
