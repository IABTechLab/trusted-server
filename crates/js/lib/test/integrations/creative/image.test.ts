import { beforeEach, afterEach, describe, expect, it, vi } from 'vitest';

import { importCreativeModule, waitForExpect } from './helpers';

const ORIGINAL_FETCH = global.fetch;

describe('creative/image.ts', () => {
  beforeEach(() => {
    vi.resetModules();
    document.body.innerHTML = '';
  });

  afterEach(() => {
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

    await importCreativeModule({ renderGuard: true });

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

    await importCreativeModule({ renderGuard: true });

    const img = new Image();
    img.src = 'https://img.example/fallback.png';

    await waitForExpect(() => {
      expect(fetchMock).toHaveBeenCalled();
      expect(img.src).toContain('https://img.example/fallback.png');
    });
  });
});
