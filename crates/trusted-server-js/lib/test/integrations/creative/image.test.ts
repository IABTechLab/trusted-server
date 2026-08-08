import { beforeEach, afterEach, describe, expect, it, vi } from 'vitest';

import { activateCreativeRuntime, disposeImportedCreativeModule, waitForExpect } from './helpers';

const ORIGINAL_FETCH = global.fetch;

describe('creative/image.ts', () => {
  beforeEach(() => {
    disposeImportedCreativeModule();
    vi.resetModules();
    document.body.innerHTML = '';
  });

  afterEach(() => {
    disposeImportedCreativeModule();
    global.fetch = ORIGINAL_FETCH;
  });

  it('proxies image src via signer endpoint', async () => {
    const signed =
      '/first-party/proxy?tsurl=https%3A%2F%2Fimg.example%2Fpixel.gif&tstoken=new&tsexp=1';
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({ href: signed }),
    });
    global.fetch = fetchMock as unknown as typeof fetch;

    await activateCreativeRuntime({ renderGuard: true });

    const img = new Image();
    img.src = 'https://img.example/pixel.gif?cb=1';

    await waitForExpect(() => {
      expect(fetchMock).toHaveBeenCalledWith(
        expect.stringContaining('/first-party/sign'),
        expect.objectContaining({ method: 'POST' })
      );
      expect(img.src).toContain('/first-party/proxy?');
      expect(img.src).toContain('tsexp=');
    });
  });

  it('falls back to raw image src when signing fails', async () => {
    const fetchMock = vi.fn().mockRejectedValue(new Error('network'));
    global.fetch = fetchMock as unknown as typeof fetch;

    await activateCreativeRuntime({ renderGuard: true });

    const img = new Image();
    img.src = 'https://img.example/fallback.png';

    await waitForExpect(() => {
      expect(fetchMock).toHaveBeenCalled();
      expect(img.src).toContain('https://img.example/fallback.png');
    });
  });

  it('defers the baseline scan and restores only its exact hooks on disposal', async () => {
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({ href: '/first-party/proxy?tsurl=image&tstoken=token&tsexp=1' }),
    });
    global.fetch = fetchMock as unknown as typeof fetch;
    const image = document.createElement('img');
    image.setAttribute('src', 'https://img.example/preexisting.png');
    document.body.appendChild(image);
    const baselineSrc = Object.getOwnPropertyDescriptor(HTMLImageElement.prototype, 'src');
    const baselineSetAttribute = HTMLImageElement.prototype.setAttribute;
    const { installDynamicImageProxy } = await import('../../../src/integrations/creative/image');

    const handle = installDynamicImageProxy(false);
    await Promise.resolve();
    expect(fetchMock).not.toHaveBeenCalled();

    handle.scan();
    await waitForExpect(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    handle.dispose();
    handle.dispose();

    expect(Object.getOwnPropertyDescriptor(HTMLImageElement.prototype, 'src')).toEqual(baselineSrc);
    expect(HTMLImageElement.prototype.setAttribute).toBe(baselineSetAttribute);
  });
});
