import { beforeEach, describe, expect, it, vi } from 'vitest';

import type { TsjsApi } from '../../src/core/types';

const RELEASE = 'a'.repeat(64);

function boot() {
  return {
    abi: 1,
    releaseId: RELEASE,
    manifest: { version: 1, releaseId: RELEASE, integrations: [] },
    auctionProjection: {
      version: 1,
      auction: { version: 1, auctionId: 'initial', results: [] },
      slots: [],
      bids: [],
    },
    creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
    diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
  };
}

describe('hard-cutover requestAds API', () => {
  beforeEach(async () => {
    await vi.resetModules();
    delete (window as unknown as { tsjs?: unknown }).tsjs;
  });

  it('replaces a callback-era request function with the exact Promise result surface', async () => {
    const legacyCallback = vi.fn();
    (window as unknown as { tsjs?: unknown }).tsjs = {
      boot: boot(),
      que: [],
      _integrationConfig: {},
      requestAds: legacyCallback,
    };

    await import('../../src/composition/index');
    await vi.waitFor(() =>
      expect((window as unknown as { tsjs?: TsjsApi }).tsjs?._internal.state).toBe('kernel')
    );

    const api = (window as unknown as { tsjs: TsjsApi }).tsjs;
    const result = api.requestAds();
    expect(result).toBeInstanceOf(Promise);
    await expect(result).resolves.toEqual({ slots: [] });
    expect(legacyCallback).not.toHaveBeenCalled();
    expect(api).not.toHaveProperty('renderAdUnit');
    expect(api).not.toHaveProperty('renderAllAdUnits');
  });
});
