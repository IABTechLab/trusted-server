import { beforeEach, describe, expect, it, vi } from 'vitest';

import { snapshotTsjsBootV1 } from '../../src/core/contracts/boot';
import type { TsjsApi, TsjsBootV1 } from '../../src/core/types';

const RELEASE = 'a'.repeat(64);
const RUNTIME_SRC = `/static/tsjs=tsjs-unified.min.js?v=${'c'.repeat(64)}`;

function boot() {
  return snapshotTsjsBootV1(
    {
      abi: 1,
      releaseId: RELEASE,
      manifest: {
        version: 1,
        releaseId: RELEASE,
        firstDisplay: null,
        runtimeSrc: RUNTIME_SRC,
        integrations: [{ id: 'render_runtime', phase: 'takeover' }],
      },
      auctionProjection: {
        version: 1,
        auction: { version: 1, auctionId: 'initial', results: [] },
        slots: [],
        bids: [],
      },
      integrations: { version: 1, entries: [] },
      creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
      diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
    },
    RELEASE
  )!;
}

async function loadMinimalProductionRuntime(): Promise<void> {
  await import('../../src/composition/runtime_transport');
  await import('../../src/integrations/render_runtime/index');
}

function installRuntimeScript(): void {
  const script = document.createElement('script');
  script.id = 'trustedserver-js';
  script.src = new URL(RUNTIME_SRC, window.location.origin).href;
  document.head.append(script);
  Object.defineProperty(document, 'currentScript', { configurable: true, value: script });
}

function installBootClaim(target: object, acceptedBoot: Readonly<TsjsBootV1>): void {
  const claim = (source: unknown): Readonly<TsjsBootV1> | undefined => {
    if (source !== document.currentScript) return undefined;
    Reflect.deleteProperty(target, '_claimBootSnapshot');
    return acceptedBoot;
  };
  Object.defineProperty(target, '_claimBootSnapshot', {
    configurable: true,
    enumerable: false,
    value: claim,
    writable: false,
  });
}

describe('core production bootstrap', () => {
  beforeEach(async () => {
    await vi.resetModules();
    document.head.replaceChildren();
    document.body.innerHTML = '';
    delete (window as unknown as { tsjs?: unknown }).tsjs;
    installRuntimeScript();
  });

  it('commits the exact hard-cutover API and drains the retained preload queue', async () => {
    const queued = vi.fn(function (this: TsjsApi) {
      expect(this).toBe((window as unknown as { tsjs?: unknown }).tsjs);
    });
    const preload = {
      boot: boot(),
      que: [queued],
      renderAdUnit: vi.fn(),
      bids: { legacy: true },
    };
    installBootClaim(preload, preload.boot);
    (window as unknown as { tsjs?: unknown }).tsjs = preload;

    await loadMinimalProductionRuntime();
    await vi.waitFor(() =>
      expect((window as unknown as { tsjs?: TsjsApi }).tsjs?._internal?.state).toBe('kernel')
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
    expect(preload).not.toHaveProperty('_claimBootSnapshot');
    expect(preload).not.toHaveProperty('renderAdUnit');
    expect(preload).not.toHaveProperty('bids');
    expect(preload).not.toHaveProperty('renderAllAdUnits');
    expect(preload).not.toHaveProperty('setConfig');
    expect(preload).not.toHaveProperty('getConfig');
  });

  it('starts installation in the combined bundle task without waiting for DOM readiness', async () => {
    const readyState = vi.spyOn(document, 'readyState', 'get').mockReturnValue('loading');
    const preload = { boot: boot(), que: [] };
    installBootClaim(preload, preload.boot);
    (window as unknown as { tsjs?: unknown }).tsjs = preload;

    try {
      await loadMinimalProductionRuntime();
      await vi.waitFor(() =>
        expect((window as unknown as { tsjs?: TsjsApi }).tsjs?._internal?.state).toBe('kernel')
      );
    } finally {
      readyState.mockRestore();
    }
  });

  it('publishes no terminal API when the closure boot claim is malformed', async () => {
    const preload = { boot: boot(), que: [] };
    Object.defineProperty(preload, '_claimBootSnapshot', {
      configurable: true,
      enumerable: true,
      value: () => preload.boot,
    });
    (window as unknown as { tsjs?: unknown }).tsjs = preload;

    await import('../../src/composition/runtime_transport');
    await Promise.resolve();

    expect(preload).toHaveProperty('_claimBootSnapshot');
    expect(preload).not.toHaveProperty('_internal');
    expect(preload).not.toHaveProperty('requestAds');
  });

  it('does not publish a fallback API over a non-configurable boot claim', async () => {
    const preload = { boot: boot(), que: [] };
    Object.defineProperty(preload, '_claimBootSnapshot', {
      configurable: false,
      enumerable: false,
      value: () => preload.boot,
      writable: false,
    });
    (window as unknown as { tsjs?: unknown }).tsjs = preload;

    await import('../../src/composition/runtime_transport');
    await Promise.resolve();

    expect(preload).toHaveProperty('_claimBootSnapshot');
    expect(preload).not.toHaveProperty('_internal');
    expect(preload).not.toHaveProperty('requestAds');
  });
});
