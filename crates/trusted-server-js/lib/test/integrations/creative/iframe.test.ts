import { beforeEach, afterEach, describe, expect, it, vi } from 'vitest';

import { activateCreativeRuntime, disposeImportedCreativeModule, waitForExpect } from './helpers';

describe('creative/iframe.ts', () => {
  const ORIGINAL_FETCH = global.fetch;

  beforeEach(() => {
    disposeImportedCreativeModule();
    vi.resetModules();
    document.body.innerHTML = '';
  });

  afterEach(() => {
    disposeImportedCreativeModule();
    global.fetch = ORIGINAL_FETCH;
  });

  it('proxies iframe src via signer endpoint', async () => {
    const signed =
      '/first-party/proxy?tsurl=https%3A%2F%2Fframe.example%2Fwidget.html&tstoken=iframe&tsexp=1';
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({ href: signed }),
    });
    global.fetch = fetchMock as unknown as typeof fetch;

    await activateCreativeRuntime({ renderGuard: true });

    const iframe = document.createElement('iframe');
    iframe.src = 'https://frame.example/widget.html?cb=1';

    await waitForExpect(() => {
      expect(fetchMock).toHaveBeenCalledWith(
        expect.stringContaining('/first-party/sign'),
        expect.objectContaining({ method: 'POST' })
      );
      expect(iframe.src).toContain('/first-party/proxy?');
      expect(iframe.src).toContain('tsexp=');
    });
  });

  it('falls back to raw iframe src when signing fails', async () => {
    const fetchMock = vi.fn().mockRejectedValue(new Error('network'));
    global.fetch = fetchMock as unknown as typeof fetch;

    await activateCreativeRuntime({ renderGuard: true });

    const iframe = document.createElement('iframe');
    iframe.src = 'https://frame.example/fallback.html';

    await waitForExpect(() => {
      expect(fetchMock).toHaveBeenCalled();
      expect(iframe.src).toContain('https://frame.example/fallback.html');
    });
  });

  it('cancels queued and future iframe rewrites on disposal', async () => {
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({ href: '/first-party/proxy?tsurl=iframe&tstoken=token&tsexp=1' }),
    });
    global.fetch = fetchMock as unknown as typeof fetch;
    const { installDynamicIframeProxy } = await import('../../../src/integrations/creative/iframe');
    const handle = installDynamicIframeProxy(false);
    const iframe = document.createElement('iframe');
    iframe.setAttribute('src', 'https://frame.example/queued.html');

    handle.dispose();
    await Promise.resolve();
    iframe.setAttribute('src', 'https://frame.example/later.html');
    await Promise.resolve();

    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(iframe.src).toContain('https://frame.example/later.html');
  });
});
