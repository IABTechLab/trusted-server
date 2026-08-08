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

describe('core production bootstrap', () => {
  beforeEach(async () => {
    await vi.resetModules();
    document.body.innerHTML = '';
    delete (window as unknown as { tsjs?: unknown }).tsjs;
  });

  it('commits the exact hard-cutover API and drains the retained preload queue', async () => {
    const queued = vi.fn(function (this: TsjsApi) {
      expect(this).toBe((window as unknown as { tsjs?: unknown }).tsjs);
    });
    const preload = {
      boot: boot(),
      que: [queued],
      _integrationConfig: {},
      renderAdUnit: vi.fn(),
      bids: { legacy: true },
    };
    (window as unknown as { tsjs?: unknown }).tsjs = preload;

    await import('../../src/composition/index');
    await vi.waitFor(() =>
      expect((window as unknown as { tsjs?: TsjsApi }).tsjs?._internal.state).toBe('kernel')
    );

    const api = (window as unknown as { tsjs: TsjsApi }).tsjs;
    expect(api).toBe(preload);
    expect(api.version).toBe('1.0.0');
    expect(api.releaseId).toBe(RELEASE);
    expect(api.boot.releaseId).toBe(RELEASE);
    expect(api.boot.manifest.releaseId).toBe(RELEASE);
    expect(Object.isFrozen(api.boot)).toBe(true);
    expect(Object.isFrozen(api.que)).toBe(true);
    expect(typeof api.addAdUnits).toBe('function');
    expect(typeof api.requestAds).toBe('function');
    expect(api._registerIntegration({})).toBe(false);
    expect(queued).toHaveBeenCalledOnce();
    expect(preload).not.toHaveProperty('_integrationConfig');
    expect(preload).not.toHaveProperty('renderAdUnit');
    expect(preload).not.toHaveProperty('bids');
    expect(preload).not.toHaveProperty('renderAllAdUnits');
    expect(preload).not.toHaveProperty('setConfig');
    expect(preload).not.toHaveProperty('getConfig');
  });

  it('starts installation in the combined bundle task without waiting for DOM readiness', async () => {
    const readyState = vi.spyOn(document, 'readyState', 'get').mockReturnValue('loading');
    const preload = { boot: boot(), que: [], _integrationConfig: {} };
    (window as unknown as { tsjs?: unknown }).tsjs = preload;

    try {
      await import('../../src/composition/index');
      await vi.waitFor(() =>
        expect((window as unknown as { tsjs?: TsjsApi }).tsjs?._internal.state).toBe('kernel')
      );
    } finally {
      readyState.mockRestore();
    }
  });

  it('fails closed when the transient integration-config transport is not plain data', async () => {
    const preload = {
      boot: boot(),
      que: [],
      _integrationConfig: new (class Config {})(),
    };
    (window as unknown as { tsjs?: unknown }).tsjs = preload;

    await import('../../src/composition/index');
    await vi.waitFor(() =>
      expect((window as unknown as { tsjs?: TsjsApi }).tsjs?._internal.state).toBe('fallback')
    );

    const api = (window as unknown as { tsjs: TsjsApi }).tsjs;
    expect(api._internal).toMatchObject({ state: 'fallback', reason: 'abi_mismatch' });
    expect(api).not.toHaveProperty('diagnostics');
  });
});
