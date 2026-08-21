import { beforeEach, describe, expect, it, vi } from 'vitest';

import { snapshotTsjsBootV1 } from '../../src/core/contracts/boot';
import {
  canonicalIntegrationConfigDigestV1,
  sha256HexUtf8V1,
} from '../../src/core/contracts/integration_configs';
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

function bootWithHostileConfigSurface() {
  return snapshotTsjsBootV1(
    {
      ...boot(),
      manifest: {
        ...boot().manifest,
        integrations: [
          { id: 'render_runtime', phase: 'takeover' },
          { id: 'aps', phase: 'takeover' },
        ],
      },
      integrations: {
        version: 1,
        entries: [
          {
            id: 'aps',
            config: {
              accessorValue: true,
              custom: { value: 1 },
              symbolTarget: { value: 1 },
              sparse: [1, null, 3],
              aliasLeft: { value: 1 },
              aliasRight: { value: 1 },
              mutable: { value: 1 },
            },
          },
        ],
      },
    },
    RELEASE
  )!;
}

function freezeDataGraph(value: unknown, skipped?: object, seen = new Set<object>()): void {
  if (typeof value !== 'object' || value === null || value === skipped || seen.has(value)) return;
  seen.add(value);
  for (const key of Reflect.ownKeys(value)) {
    const descriptor = Object.getOwnPropertyDescriptor(value, key);
    if (descriptor && 'value' in descriptor) freezeDataGraph(descriptor.value, skipped, seen);
  }
  Object.freeze(value);
}

type HostileBootKind =
  | 'accessor'
  | 'custom_prototype'
  | 'symbol'
  | 'sparse_array'
  | 'repeated_alias'
  | 'mutable_config'
  | 'mutable_manifest'
  | 'mutable_projection'
  | 'mutable_diagnostics';

function hostileClaimedBoot(kind: HostileBootKind): {
  readonly candidate: Readonly<TsjsBootV1>;
  readonly integrity: ReturnType<typeof bootIntegrity>;
  readonly assertUnobserved: () => void;
} {
  const accepted = bootWithHostileConfigSurface();
  const candidate = JSON.parse(JSON.stringify(accepted)) as Record<string, unknown>;
  const integrations = candidate.integrations as {
    entries: Array<{ config: Record<string, unknown> }>;
  };
  const config = integrations.entries[0]!.config;
  let accessorReads = 0;
  let skipped: object | undefined;
  if (kind === 'accessor') {
    Object.defineProperty(config, 'accessorValue', {
      configurable: true,
      enumerable: true,
      get: () => {
        accessorReads += 1;
        return true;
      },
    });
  } else if (kind === 'custom_prototype') {
    Object.setPrototypeOf(config.custom as object, { inherited: true });
  } else if (kind === 'symbol') {
    Object.defineProperty(config.symbolTarget as object, Symbol('hostile'), {
      enumerable: true,
      value: true,
    });
  } else if (kind === 'sparse_array') {
    const sparse = [1, null, 3];
    Reflect.deleteProperty(sparse, '1');
    config.sparse = sparse;
  } else if (kind === 'repeated_alias') {
    config.aliasRight = config.aliasLeft;
  } else if (kind === 'mutable_config') {
    skipped = config.mutable as object;
  } else if (kind === 'mutable_manifest') {
    skipped = (candidate.manifest as { integrations: object }).integrations;
  } else if (kind === 'mutable_projection') {
    skipped = (candidate.auctionProjection as { slots: object }).slots;
  } else {
    skipped = (candidate.diagnostics as { gpt: object }).gpt;
  }
  freezeDataGraph(candidate, skipped);
  return {
    candidate: candidate as unknown as Readonly<TsjsBootV1>,
    integrity: bootIntegrity(accepted),
    assertUnobserved: () => expect(accessorReads).toBe(0),
  };
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

function bootIntegrity(acceptedBoot: Readonly<TsjsBootV1>) {
  return Object.freeze({
    version: 1 as const,
    projectionDigest: sha256HexUtf8V1(JSON.stringify(acceptedBoot.auctionProjection)),
    integrationConfigDigest: canonicalIntegrationConfigDigestV1(acceptedBoot.integrations),
  });
}

function installBootClaim(
  target: object,
  acceptedBoot: Readonly<TsjsBootV1>,
  integrity = bootIntegrity(acceptedBoot)
): ReturnType<typeof vi.fn> {
  const completed = vi.fn();
  const claim = (source: unknown) => {
    if (source !== document.currentScript) return undefined;
    Reflect.deleteProperty(target, '_claimBootSnapshot');
    return Object.freeze({ boot: acceptedBoot, integrity, complete: completed });
  };
  Object.defineProperty(target, '_claimBootSnapshot', {
    configurable: true,
    enumerable: false,
    value: claim,
    writable: false,
  });
  return completed;
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

  it.each(['projectionDigest', 'integrationConfigDigest'] as const)(
    'rejects a direct boot whose %s does not match before runtime publication',
    async (field) => {
      const preload = { boot: boot(), que: [] };
      const rejected = installBootClaim(
        preload,
        preload.boot,
        Object.freeze({ ...bootIntegrity(preload.boot), [field]: 'f'.repeat(64) })
      );
      (window as unknown as { tsjs?: unknown }).tsjs = preload;

      await loadMinimalProductionRuntime();
      await Promise.resolve();

      expect(preload).not.toHaveProperty('_internal');
      expect(preload).not.toHaveProperty('_registerIntegration');
      expect(preload).not.toHaveProperty('requestAds');
      expect(rejected).toHaveBeenCalledOnce();
      expect(rejected).toHaveBeenCalledWith('abi_mismatch');
    }
  );

  it('rejects mismatched integrity before consuming takeover or preparing runtime owners', async () => {
    const script = document.currentScript as HTMLScriptElement;
    script.id = 'trustedserver-js-runtime';
    const coordinate = vi.fn();
    const preload = { boot: boot(), que: [] };
    Object.defineProperty(preload, '_firstDisplayTakeover', {
      configurable: true,
      enumerable: false,
      value: coordinate,
      writable: false,
    });
    const rejected = installBootClaim(
      preload,
      preload.boot,
      Object.freeze({ ...bootIntegrity(preload.boot), projectionDigest: 'f'.repeat(64) })
    );
    (window as unknown as { tsjs?: unknown }).tsjs = preload;

    await loadMinimalProductionRuntime();
    await Promise.resolve();

    expect(coordinate).not.toHaveBeenCalled();
    expect(preload).toHaveProperty('_firstDisplayTakeover');
    expect(preload).not.toHaveProperty('_registerIntegration');
    expect(preload).not.toHaveProperty('_internal');
    expect(rejected).toHaveBeenCalledOnce();
    expect(rejected).toHaveBeenCalledWith('abi_mismatch');
  });

  it.each(
    (['direct', 'takeover'] as const).flatMap((mode) =>
      (
        [
          'accessor',
          'custom_prototype',
          'symbol',
          'sparse_array',
          'repeated_alias',
          'mutable_config',
          'mutable_manifest',
          'mutable_projection',
          'mutable_diagnostics',
        ] as const
      ).map((kind) => [mode, kind] as const)
    )
  )('rejects a %s claimed boot with hostile %s data before effects', async (mode, kind) => {
    const { candidate, integrity, assertUnobserved } = hostileClaimedBoot(kind);
    const preload = { boot: candidate, que: [] };
    const coordinate = vi.fn();
    if (mode === 'takeover') {
      Object.defineProperty(preload, '_firstDisplayTakeover', {
        configurable: true,
        enumerable: false,
        value: coordinate,
        writable: false,
      });
    }
    const rejected = installBootClaim(preload, candidate, integrity);
    (window as unknown as { tsjs?: unknown }).tsjs = preload;
    const { startProductionRuntime } = await import('../../src/core/index');
    const createComposition = vi.fn(() => ({
      runtime: { start: vi.fn(() => true) },
    })) as unknown as Parameters<typeof startProductionRuntime>[0];

    startProductionRuntime(createComposition);

    assertUnobserved();
    expect(createComposition).not.toHaveBeenCalled();
    expect(coordinate).not.toHaveBeenCalled();
    expect(preload).not.toHaveProperty('_internal');
    expect(rejected).toHaveBeenCalledOnce();
    expect(rejected).toHaveBeenCalledWith('abi_mismatch');
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
