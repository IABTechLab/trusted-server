import { beforeEach, afterEach, describe, expect, it, vi } from 'vitest';

import { FIRST_PARTY_CLICK, MUTATED_CLICK, PROXY_RESPONSE, importCreativeModule } from './helpers';

const ORIGINAL_FETCH = global.fetch;

// The guard persists validated, absolutized URLs (resolved against the pinned
// trusted base), so expectations compare against the absolute forms.
const absolute = (url: string): string => new URL(url, location.href).toString();
const REBUILD_PREFIX = absolute('/first-party/proxy-rebuild?');

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
    global.fetch = undefined as unknown as typeof fetch;

    const anchor = document.createElement('a');
    anchor.setAttribute('data-tsclick', FIRST_PARTY_CLICK);
    anchor.setAttribute('href', FIRST_PARTY_CLICK);
    document.body.appendChild(anchor);

    await importCreativeModule();

    anchor.setAttribute('href', MUTATED_CLICK);

    await Promise.resolve();
    await vi.runAllTimersAsync();

    const finalHref = anchor.getAttribute('href') ?? '';
    expect(finalHref.startsWith(REBUILD_PREFIX)).toBe(true);
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
    const call = fetchMock.mock.calls[0]!;
    expect(call[0]).toBe('/first-party/proxy-rebuild');
    const payload = JSON.parse(call[1]?.body as string);
    expect(payload).toEqual({
      tsclick: FIRST_PARTY_CLICK,
      add: { bar: '2' },
      del: ['foo'],
    });

    expect(anchor.getAttribute('href')).toBe(absolute(PROXY_RESPONSE));
    expect(anchor.getAttribute('data-tsclick')).toBe(absolute(PROXY_RESPONSE));
  });

  it('skips the doomed POST and uses the GET fallback in an opaque origin', async () => {
    // A sandboxed srcdoc creative without `allow-same-origin` has origin
    // "null": its JSON POST preflights and fails, so the guard must go
    // straight to the GET navigation fallback instead of fetching.
    vi.useFakeTimers();

    const originDescriptor = Object.getOwnPropertyDescriptor(window, 'origin');
    Object.defineProperty(window, 'origin', { value: 'null', configurable: true });

    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({ href: PROXY_RESPONSE }),
    });
    global.fetch = fetchMock as unknown as typeof fetch;

    try {
      const anchor = document.createElement('a');
      anchor.setAttribute('data-tsclick', FIRST_PARTY_CLICK);
      anchor.setAttribute('href', FIRST_PARTY_CLICK);
      document.body.appendChild(anchor);

      await importCreativeModule();

      anchor.setAttribute('href', MUTATED_CLICK);

      await Promise.resolve();
      await vi.runAllTimersAsync();

      expect(fetchMock).not.toHaveBeenCalled();
      const finalHref = anchor.getAttribute('href') ?? '';
      expect(finalHref.startsWith(REBUILD_PREFIX)).toBe(true);
      expect(finalHref).toContain('add=%7B%22bar%22%3A%222%22%7D');
      expect(finalHref).toContain('del=%5B%22foo%22%5D');
      // The fallback must never become the canonical click: data-tsclick is
      // what future mutation diffs compare against.
      expect(anchor.getAttribute('data-tsclick')).toBe(FIRST_PARTY_CLICK);
    } finally {
      if (originDescriptor) {
        Object.defineProperty(window, 'origin', originDescriptor);
      } else {
        delete (window as { origin?: string }).origin;
      }
    }
  });

  it('rebuilds a second mutation wave after an opaque-origin fallback', async () => {
    // Wave 1 replaces href with the GET fallback; the canonical signed click in
    // data-tsclick must survive so wave 2's mutation is still diffed and
    // rebuilt instead of being lost.
    vi.useFakeTimers();

    const originDescriptor = Object.getOwnPropertyDescriptor(window, 'origin');
    Object.defineProperty(window, 'origin', { value: 'null', configurable: true });
    global.fetch = undefined as unknown as typeof fetch;

    try {
      const anchor = document.createElement('a');
      anchor.setAttribute('data-tsclick', FIRST_PARTY_CLICK);
      anchor.setAttribute('href', FIRST_PARTY_CLICK);
      document.body.appendChild(anchor);

      await importCreativeModule();

      anchor.setAttribute('href', MUTATED_CLICK);
      await Promise.resolve();
      await vi.runAllTimersAsync();

      expect(anchor.getAttribute('data-tsclick')).toBe(FIRST_PARTY_CLICK);

      anchor.setAttribute('href', 'https://example.com/landing?baz=3');
      await Promise.resolve();
      await vi.runAllTimersAsync();

      const finalHref = anchor.getAttribute('href') ?? '';
      expect(finalHref.startsWith(REBUILD_PREFIX)).toBe(true);
      expect(finalHref).toContain('baz');
      expect(anchor.getAttribute('data-tsclick')).toBe(FIRST_PARTY_CLICK);
    } finally {
      if (originDescriptor) {
        Object.defineProperty(window, 'origin', originDescriptor);
      } else {
        delete (window as { origin?: string }).origin;
      }
    }
  });

  it('navigates the observer-repaired fallback, not the pre-mutation click', async () => {
    // Mutations made before any user interaction are repaired by the mutation
    // observer, which writes the GET fallback to href while keeping the
    // canonical signed click in data-tsclick. A later click must navigate the
    // repaired URL — canonicalizing the fallback against the canonical click
    // would fail the base comparison and silently navigate the original.
    vi.useFakeTimers();

    const originDescriptor = Object.getOwnPropertyDescriptor(window, 'origin');
    Object.defineProperty(window, 'origin', { value: 'null', configurable: true });
    global.fetch = undefined as unknown as typeof fetch;
    const openMock = vi.fn();
    const originalOpen = window.open;
    window.open = openMock as unknown as typeof window.open;

    try {
      const anchor = document.createElement('a');
      anchor.setAttribute('data-tsclick', FIRST_PARTY_CLICK);
      anchor.setAttribute('href', FIRST_PARTY_CLICK);
      // Force the middle-click branch so navigation lands in window.open,
      // which jsdom can observe.
      anchor.setAttribute('target', '_blank');
      document.body.appendChild(anchor);

      await importCreativeModule();

      // Wave 1: creative mutates the link, observer repairs it.
      anchor.setAttribute('href', MUTATED_CLICK);
      await Promise.resolve();
      await vi.runAllTimersAsync();

      const repaired = anchor.getAttribute('href') ?? '';
      expect(repaired.startsWith(REBUILD_PREFIX)).toBe(true);

      // Now the user clicks.
      anchor.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }));
      await Promise.resolve();
      await vi.runAllTimersAsync();

      expect(openMock).toHaveBeenCalled();
      const navigated = String(openMock.mock.calls[0]![0]);
      expect(navigated.startsWith(REBUILD_PREFIX)).toBe(true);
      expect(navigated).toContain('add=%7B%22bar%22%3A%222%22%7D');
      expect(navigated).not.toBe(absolute(FIRST_PARTY_CLICK));
    } finally {
      window.open = originalOpen;
      if (originDescriptor) {
        Object.defineProperty(window, 'origin', originDescriptor);
      } else {
        delete (window as { origin?: string }).origin;
      }
    }
  });

  it('refuses to navigate to or persist non-http(s) URLs', async () => {
    // The guard reads creative-controlled attributes; a javascript: value must
    // never reach location.href or an href write.
    vi.useFakeTimers();
    global.fetch = undefined as unknown as typeof fetch;

    const anchor = document.createElement('a');
    anchor.setAttribute('data-tsclick', 'javascript:evil()');
    anchor.setAttribute('href', 'javascript:evil()');
    document.body.appendChild(anchor);

    await importCreativeModule();

    anchor.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }));
    await Promise.resolve();
    await vi.runAllTimersAsync();

    expect(anchor.getAttribute('href')).toBe('javascript:evil()');
    // jsdom throws on real navigation, so reaching this point without an
    // unhandled navigation error is the assertion that location.href was
    // never assigned the javascript: URL.
  });
});
