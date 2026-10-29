import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { FIRST_PARTY_CLICK, MUTATED_CLICK, PROXY_RESPONSE, importCreativeModule } from './helpers';

const originalFetch = global.fetch;
const stampDescriptor = Object.getOwnPropertyDescriptor(window, '__tsCreativeOrigin');
const originDescriptor = Object.getOwnPropertyDescriptor(window, 'origin');
const ENDPOINT_ORIGIN = 'https://ads.example.com:8443';
const STAMP_ORIGIN = 'https://publisher.example';

function setCurrentScript(): void {
  const script = Object.assign(document.createElement('script'), {
    src: `${ENDPOINT_ORIGIN}/static/tsjs=tsjs-unified.min.js?v=hash`,
  });
  Object.defineProperty(document, 'currentScript', { configurable: true, value: script });
}

beforeEach(() => {
  vi.resetModules();
  vi.stubGlobal('document', document.implementation.createHTMLDocument('creative'));
  vi.stubGlobal('location', {
    href: 'https://foreign.example/creative',
    origin: 'https://foreign.example',
  });
  Object.defineProperty(window, '__tsCreativeOrigin', { configurable: true, value: STAMP_ORIGIN });
  vi.useFakeTimers();
});

afterEach(() => {
  global.fetch = originalFetch;
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  if (stampDescriptor) Object.defineProperty(window, '__tsCreativeOrigin', stampDescriptor);
  else Reflect.deleteProperty(window, '__tsCreativeOrigin');
  if (originDescriptor) Object.defineProperty(window, 'origin', originDescriptor);
  else Reflect.deleteProperty(window, 'origin');
});

describe('creative/click.ts captured first-party fallback', () => {
  it('keeps rebuilt authority through repeated repairs and later navigation', async () => {
    setCurrentScript();
    const rebuilt = `${ENDPOINT_ORIGIN}${PROXY_RESPONSE}`;
    const rebuiltAgain = rebuilt.replace('bar=2', 'baz=3');
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce({ ok: true, json: async () => ({ href: rebuilt }) })
      .mockResolvedValue({ ok: true, json: async () => ({ href: rebuiltAgain }) });
    global.fetch = fetchMock as unknown as typeof fetch;
    const openMock = vi.spyOn(window, 'open').mockImplementation(() => null);
    const anchor = document.createElement('a');
    anchor.setAttribute('data-tsclick', `${ENDPOINT_ORIGIN}${FIRST_PARTY_CLICK}`);
    anchor.setAttribute('href', `${ENDPOINT_ORIGIN}${FIRST_PARTY_CLICK}`);
    anchor.setAttribute('target', '_blank');
    document.body.appendChild(anchor);
    await importCreativeModule();
    await vi.runAllTimersAsync();

    anchor.setAttribute('href', MUTATED_CLICK);
    await vi.runAllTimersAsync();

    expect(anchor.getAttribute('href')).toBe(rebuilt);
    expect(anchor.getAttribute('data-tsclick')).toBe(rebuilt);
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(fetchMock.mock.calls[0][0]).toBe(`${ENDPOINT_ORIGIN}/first-party/proxy-rebuild`);
    anchor.click();
    await vi.runAllTimersAsync();
    expect(openMock).toHaveBeenLastCalledWith(rebuilt, '_blank', 'noopener,noreferrer');
    expect(fetchMock).toHaveBeenCalledTimes(1);

    anchor.setAttribute('href', 'https://example.com/landing?baz=3');
    await vi.runAllTimersAsync();
    expect(anchor.getAttribute('data-tsclick')).toBe(rebuiltAgain);
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(JSON.parse(fetchMock.mock.calls[1][1].body)).toEqual({
      tsclick: rebuilt,
      add: { baz: '3' },
      del: ['bar'],
    });
    anchor.click();
    await vi.runAllTimersAsync();
    expect(openMock).toHaveBeenLastCalledWith(rebuiltAgain, '_blank', 'noopener,noreferrer');
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it.each([true, false])(
    'preserves stamped legacy relative navigation with script=%s',
    async (script) => {
      if (script) setCurrentScript();
      const openMock = vi.spyOn(window, 'open').mockImplementation(() => null);
      const anchor = document.createElement('a');
      anchor.setAttribute('data-tsclick', FIRST_PARTY_CLICK);
      anchor.setAttribute('href', FIRST_PARTY_CLICK);
      anchor.target = '_blank';
      document.body.appendChild(anchor);
      await importCreativeModule();
      await vi.runAllTimersAsync();

      anchor.click();
      await vi.runAllTimersAsync();
      expect(openMock).toHaveBeenLastCalledWith(
        `${STAMP_ORIGIN}${FIRST_PARTY_CLICK}`,
        '_blank',
        'noopener,noreferrer'
      );
      expect(anchor.getAttribute('data-tsclick')).toBe(FIRST_PARTY_CLICK);
    }
  );

  it.each([
    { script: true, opaque: false },
    { script: true, opaque: true },
    { script: false, opaque: true },
  ])(
    'keeps repeated fallback repairs and navigation on the endpoint: %j',
    async ({ script, opaque }) => {
      if (script) setCurrentScript();
      const origin = script ? ENDPOINT_ORIGIN : STAMP_ORIGIN;
      const fetchMock = vi.fn();
      global.fetch = opaque
        ? (fetchMock as unknown as typeof fetch)
        : (undefined as unknown as typeof fetch);
      if (opaque) {
        vi.stubGlobal('location', { href: 'about:srcdoc', origin: 'null' });
        Object.defineProperty(window, 'origin', { configurable: true, value: 'null' });
      }
      const openMock = vi.spyOn(window, 'open').mockImplementation(() => null);
      const anchor = document.createElement('a');
      anchor.setAttribute('data-tsclick', FIRST_PARTY_CLICK);
      anchor.setAttribute('href', FIRST_PARTY_CLICK);
      anchor.target = '_blank';
      document.body.appendChild(anchor);
      await importCreativeModule();
      await vi.runAllTimersAsync();

      for (const [mutation, param] of [
        [MUTATED_CLICK, 'bar'],
        ['https://example.com/landing?baz=3', 'baz'],
      ]) {
        anchor.setAttribute('href', mutation);
        await vi.runAllTimersAsync();
        const fallback = new URL(anchor.getAttribute('href') ?? '');
        expect(fallback.origin).toBe(origin);
        expect(fallback.pathname).toBe('/first-party/proxy-rebuild');
        expect(JSON.parse(fallback.searchParams.get('add') ?? '{}')).toEqual({
          [param]: param === 'bar' ? '2' : '3',
        });
        expect(anchor.getAttribute('data-tsclick')).toBe(FIRST_PARTY_CLICK);
        anchor.click();
        await vi.runAllTimersAsync();
        expect(openMock).toHaveBeenLastCalledWith(
          fallback.toString(),
          '_blank',
          'noopener,noreferrer'
        );
      }
      expect(fetchMock).not.toHaveBeenCalled();
    }
  );

  it('keeps the captured authority for an opaque long-URL form fallback', async () => {
    setCurrentScript();
    vi.stubGlobal('location', { href: 'about:srcdoc', origin: 'null' });
    Object.defineProperty(window, 'origin', { configurable: true, value: 'null' });
    const fetchMock = vi.fn();
    global.fetch = fetchMock as unknown as typeof fetch;
    const submitMock = vi.spyOn(HTMLFormElement.prototype, 'submit').mockImplementation(() => {});
    const filler = 'a'.repeat(6800);
    const signed = `${ENDPOINT_ORIGIN}${FIRST_PARTY_CLICK}&pad=${filler}`;
    const anchor = document.createElement('a');
    anchor.setAttribute('data-tsclick', signed);
    anchor.setAttribute('href', signed);
    anchor.target = '_blank';
    document.body.appendChild(anchor);
    await importCreativeModule();
    await vi.runAllTimersAsync();

    anchor.setAttribute('href', `https://example.com/landing?bar=2&pad=${filler}`);
    await vi.runAllTimersAsync();
    anchor.click();
    await vi.runAllTimersAsync();

    expect(fetchMock).not.toHaveBeenCalled();
    expect(submitMock).toHaveBeenCalledTimes(1);
    const form = document.querySelector('form');
    expect(form?.action).toBe(`${ENDPOINT_ORIGIN}/first-party/proxy-rebuild`);
    expect(form?.method).toBe('post');
    expect(form?.target).toBe('_blank');
    expect(form?.querySelector<HTMLInputElement>('input[name="tsclick"]')?.value).toBe(signed);
    expect(form?.querySelector<HTMLInputElement>('input[name="add"]')?.value).toBe('{"bar":"2"}');
    expect(anchor.getAttribute('data-tsclick')).toBe(signed);
  });
});
