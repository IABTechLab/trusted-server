import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { FIRST_PARTY_CLICK, MUTATED_CLICK, waitForExpect } from './helpers';

const ORIGINAL_FETCH = global.fetch;

describe('creative guard ownership', () => {
  beforeEach(() => {
    vi.resetModules();
    document.body.innerHTML = '';
  });

  afterEach(() => {
    global.fetch = ORIGINAL_FETCH;
    vi.useRealTimers();
  });

  it('defers the click scan and releases its observer and capture listeners', async () => {
    vi.useFakeTimers();
    global.fetch = undefined as unknown as typeof fetch;
    const anchor = document.createElement('a');
    anchor.setAttribute('data-tsclick', FIRST_PARTY_CLICK);
    anchor.setAttribute('href', MUTATED_CLICK);
    document.body.appendChild(anchor);
    const { installClickGuard } = await import('../../../src/integrations/creative/click');

    const guard = installClickGuard(false);
    await Promise.resolve();
    await vi.runAllTimersAsync();
    expect(anchor.getAttribute('href')).toBe(MUTATED_CLICK);

    guard.scan();
    await Promise.resolve();
    await vi.runAllTimersAsync();
    expect(anchor.getAttribute('href')).toContain('/first-party/proxy-rebuild?');

    guard.dispose();
    guard.dispose();
    anchor.setAttribute('href', MUTATED_CLICK);
    const click = new MouseEvent('click', { bubbles: true, cancelable: true });
    anchor.dispatchEvent(click);
    await Promise.resolve();
    await vi.runAllTimersAsync();

    expect(click.defaultPrevented).toBe(false);
    expect(anchor.getAttribute('href')).toBe(MUTATED_CLICK);
  });

  it('defers image scans, cancels late signing, and compare-restores owned hooks', async () => {
    let resolveSigning: ((value: unknown) => void) | undefined;
    const fetchMock = vi.fn(
      () =>
        new Promise((resolve) => {
          resolveSigning = resolve;
        })
    );
    global.fetch = fetchMock as unknown as typeof fetch;
    const image = document.createElement('img');
    image.setAttribute('src', 'https://img.example/existing.gif');
    document.body.appendChild(image);
    const descriptorBefore = Object.getOwnPropertyDescriptor(HTMLImageElement.prototype, 'src');
    const { installDynamicImageProxy } = await import('../../../src/integrations/creative/image');

    const guard = installDynamicImageProxy(false);
    expect(fetchMock).not.toHaveBeenCalled();
    expect(Object.getOwnPropertyDescriptor(HTMLImageElement.prototype, 'src')).not.toEqual(
      descriptorBefore
    );

    guard.scan();
    await waitForExpect(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    guard.dispose();
    resolveSigning?.({
      ok: true,
      json: async () => ({ href: '/first-party/proxy?late=1' }),
    });
    await Promise.resolve();
    await Promise.resolve();

    expect(image.getAttribute('src')).toBe('https://img.example/existing.gif');
    expect(Object.getOwnPropertyDescriptor(HTMLImageElement.prototype, 'src')).toEqual(
      descriptorBefore
    );

    const replacement = installDynamicImageProxy(false);
    expect(replacement).not.toBe(guard);
    expect(Object.getOwnPropertyDescriptor(HTMLImageElement.prototype, 'src')).not.toEqual(
      descriptorBefore
    );
    replacement.dispose();
    expect(Object.getOwnPropertyDescriptor(HTMLImageElement.prototype, 'src')).toEqual(
      descriptorBefore
    );
  });

  it('does not overwrite a foreign iframe hook installed after activation', async () => {
    const { installDynamicIframeProxy } = await import('../../../src/integrations/creative/iframe');
    const guard = installDynamicIframeProxy(false);
    const owned = Object.getOwnPropertyDescriptor(HTMLIFrameElement.prototype, 'src');
    expect(owned).toBeDefined();
    const foreignGet = function (this: HTMLIFrameElement): string {
      return this.getAttribute('src') ?? '';
    };
    const foreignSet = function (this: HTMLIFrameElement, value: string): void {
      this.setAttribute('src', value);
    };
    Object.defineProperty(HTMLIFrameElement.prototype, 'src', {
      configurable: true,
      enumerable: owned?.enumerable ?? true,
      get: foreignGet,
      set: foreignSet,
    });

    guard.dispose();

    const current = Object.getOwnPropertyDescriptor(HTMLIFrameElement.prototype, 'src');
    expect(current?.get).toBe(foreignGet);
    expect(current?.set).toBe(foreignSet);
  });
});
