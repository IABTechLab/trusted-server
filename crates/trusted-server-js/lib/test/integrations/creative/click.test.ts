import { beforeEach, afterEach, describe, expect, it, vi } from 'vitest';

import {
  FIRST_PARTY_CLICK,
  MUTATED_CLICK,
  PROXY_RESPONSE,
  disposeImportedCreativeModule,
  importCreativeModule,
} from './helpers';

const ORIGINAL_FETCH = global.fetch;

// The guard persists validated, absolutized URLs (resolved against the pinned
// trusted base), so expectations compare against the absolute forms.
const absolute = (url: string): string => new URL(url, location.href).toString();
const REBUILD_PREFIX = absolute('/first-party/proxy-rebuild?');

describe('creative/click.ts', () => {
  beforeEach(() => {
    disposeImportedCreativeModule();
    vi.resetModules();
    document.body.innerHTML = '';
  });

  afterEach(() => {
    disposeImportedCreativeModule();
    global.fetch = ORIGINAL_FETCH;
    vi.useRealTimers();
  });

  it('owns click listeners and defers the baseline scan until requested', async () => {
    vi.useFakeTimers();
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({ href: PROXY_RESPONSE }),
    });
    global.fetch = fetchMock as unknown as typeof fetch;
    const anchor = document.createElement('a');
    anchor.setAttribute('data-tsclick', FIRST_PARTY_CLICK);
    anchor.setAttribute('href', MUTATED_CLICK);
    document.body.appendChild(anchor);
    const removeEventListener = vi.spyOn(document, 'removeEventListener');
    const { installClickGuard } = await import('../../../src/integrations/creative/click');

    const handle = installClickGuard(false);
    await Promise.resolve();
    await vi.runAllTimersAsync();
    expect(fetchMock).not.toHaveBeenCalled();

    handle.scan();
    await Promise.resolve();
    await vi.runAllTimersAsync();
    expect(fetchMock).toHaveBeenCalledTimes(1);

    handle.dispose();
    handle.dispose();
    expect(removeEventListener.mock.calls.filter(([type]) => type === 'click')).toHaveLength(1);
    expect(removeEventListener.mock.calls.filter(([type]) => type === 'auxclick')).toHaveLength(1);
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
    // data-tsclick keeps the server's root-relative shape: it is echoed back as
    // the rebuild payload's `tsclick`, which the server parses as a click path.
    expect(anchor.getAttribute('data-tsclick')).toBe(PROXY_RESPONSE);
  });

  it('sends a root-relative tsclick on a second rebuild after a successful one', async () => {
    // Regression: persisting an absolute canonical click made the next rebuild
    // POST a value the server rejects as an invalid click path, so the second
    // mutation was silently lost on non-opaque consumers.
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

    expect(anchor.getAttribute('data-tsclick')).toBe(PROXY_RESPONSE);

    // A second mutation now diffs against the rebuilt canonical click.
    anchor.setAttribute('href', 'https://example.com/landing?baz=3');
    await Promise.resolve();
    await vi.runAllTimersAsync();

    expect(fetchMock.mock.calls.length).toBeGreaterThan(1);
    const payloads = fetchMock.mock.calls.map(
      (call) => JSON.parse(call[1]?.body as string) as { tsclick: string }
    );
    // Every payload must carry the server's root-relative click form: an
    // absolute one is rejected as an invalid click path.
    for (const payload of payloads) {
      expect(payload.tsclick.startsWith('/first-party/click?')).toBe(true);
    }
    expect(payloads.some((payload) => payload.tsclick === PROXY_RESPONSE)).toBe(true);
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

  it('navigates an over-long rebuild through a form POST instead of a GET URL', async () => {
    // Fastly rejects request URLs over 8192 bytes before the handler runs, and
    // a signed click with many tracking params exceeds that once nested in
    // another query string. A form body has no such bound, and a submission is
    // a navigation — so it is not blocked by CORS from the opaque origin.
    vi.useFakeTimers();

    const originDescriptor = Object.getOwnPropertyDescriptor(window, 'origin');
    Object.defineProperty(window, 'origin', { value: 'null', configurable: true });
    global.fetch = undefined as unknown as typeof fetch;

    const submits: HTMLFormElement[] = [];
    const originalSubmit = HTMLFormElement.prototype.submit;
    HTMLFormElement.prototype.submit = function patched(this: HTMLFormElement) {
      submits.push(this);
    };

    try {
      // A signed click long enough that the nested rebuild URL crosses the cap.
      const filler = 'a'.repeat(6800);
      const longClick = `/first-party/click?tsurl=https%3A%2F%2Fexample.com%2Flanding&foo=1&pad=${filler}&tstoken=token123`;
      const anchor = document.createElement('a');
      anchor.setAttribute('data-tsclick', longClick);
      anchor.setAttribute('href', longClick);
      document.body.appendChild(anchor);

      await importCreativeModule();

      anchor.setAttribute('href', `https://example.com/landing?pad=${filler}&bar=2`);
      await Promise.resolve();
      await vi.runAllTimersAsync();

      anchor.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }));
      await Promise.resolve();
      await vi.runAllTimersAsync();

      expect(submits.length).toBeGreaterThan(0);
      const form = submits[0];
      expect(form.method.toLowerCase()).toBe('post');
      expect(form.action).toContain('/first-party/proxy-rebuild');
      const tsclick = form.querySelector('input[name="tsclick"]') as HTMLInputElement | null;
      expect(tsclick?.value).toBe(longClick);
      const add = form.querySelector('input[name="add"]') as HTMLInputElement | null;
      expect(add?.value).toContain('bar');
    } finally {
      HTMLFormElement.prototype.submit = originalSubmit;
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
