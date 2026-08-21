import { beforeEach, describe, expect, it, vi } from 'vitest';

import { snapshotTsjsBootV1 } from '../../src/core/contracts/boot';
import {
  canonicalIntegrationConfigDigestV1,
  sha256HexUtf8V1,
} from '../../src/core/contracts/integration_configs';
import type { TsjsApi, TsjsBootV1 } from '../../src/core/types';
import stringifyCorpus from '../fixtures/contracts/ecmascript-json-stringify-v1.json';

const RELEASE = 'a'.repeat(64);
const RUNTIME_SRC = `/static/tsjs=tsjs-unified.min.js?v=${'c'.repeat(64)}`;

describe('server JSON.stringify canonicalization corpus', () => {
  it.each(stringifyCorpus)('$name', ({ input, expected }) => {
    const canonical = JSON.stringify(JSON.parse(input));
    expect(canonical).toBe(expected);
    expect(sha256HexUtf8V1(canonical)).toMatch(/^[0-9a-f]{64}$/);
  });
});

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
  Object.defineProperty(target, '_claimBootSnapshot', {
    configurable: true,
    enumerable: false,
    value: (source: unknown) => {
      if (source !== document.currentScript) return undefined;
      Reflect.deleteProperty(target, '_claimBootSnapshot');
      return Object.freeze({
        boot: acceptedBoot,
        complete: () => undefined,
        integrity: Object.freeze({
          version: 1,
          projectionDigest: sha256HexUtf8V1(JSON.stringify(acceptedBoot.auctionProjection)),
          integrationConfigDigest: canonicalIntegrationConfigDigestV1(acceptedBoot.integrations),
        }),
      });
    },
    writable: false,
  });
}

describe('hard-cutover requestAds API', () => {
  beforeEach(async () => {
    await vi.resetModules();
    document.head.replaceChildren();
    delete (window as unknown as { tsjs?: unknown }).tsjs;
    installRuntimeScript();
  });

  it('replaces a callback-era request function with the exact Promise result surface', async () => {
    const legacyCallback = vi.fn();
    const preload = {
      boot: boot(),
      que: [],
      requestAds: legacyCallback,
    };
    installBootClaim(preload, preload.boot);
    (window as unknown as { tsjs?: unknown }).tsjs = preload;

    await loadMinimalProductionRuntime();
    await vi.waitFor(() =>
      expect((window as unknown as { tsjs?: TsjsApi }).tsjs?._internal?.state).toBe('kernel')
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
