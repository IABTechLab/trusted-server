import { beforeEach, afterEach, describe, expect, it, vi } from 'vitest';

import { importCreativeModule, waitForExpect } from './helpers';

describe('creative/iframe.ts', () => {
  const ORIGINAL_FETCH = global.fetch;

  beforeEach(() => {
    vi.resetModules();
    document.body.innerHTML = '';
  });

  afterEach(() => {
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

    await importCreativeModule();

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

    await importCreativeModule();

    const iframe = document.createElement('iframe');
    iframe.src = 'https://frame.example/fallback.html';

    await waitForExpect(() => {
      expect(fetchMock).toHaveBeenCalled();
      expect(iframe.src).toContain('https://frame.example/fallback.html');
    });
  });
});
