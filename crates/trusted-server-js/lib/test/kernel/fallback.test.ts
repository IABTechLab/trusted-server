import { describe, expect, it } from 'vitest';

import {
  buildFallbackBoot,
  buildKernelBoot,
  trustedCriticalOrigin,
} from '../../src/kernel/fallback';

const RELEASE_ID = 'a'.repeat(64);
const TRUSTED_CRITICAL_SRC = `/static/tsjs=tsjs-unified.min.js?v=${'d'.repeat(64)}`;

function opaqueDocument(stamp: PropertyDescriptor | undefined): Document {
  const view = { location: { origin: 'null' } } as Record<string, unknown>;
  if (stamp) Object.defineProperty(view, '__tsCreativeOrigin', stamp);
  return { defaultView: view } as unknown as Document;
}

describe('critical artifact origin', () => {
  it('accepts only the immutable own-data creative stamp for an opaque document', () => {
    expect(
      trustedCriticalOrigin(
        opaqueDocument({
          configurable: false,
          enumerable: false,
          value: 'https://publisher.example',
          writable: false,
        })
      )
    ).toBe('https://publisher.example');
  });

  it.each([
    undefined,
    { configurable: true, enumerable: false, value: 'https://publisher.example', writable: false },
    { configurable: false, enumerable: false, value: 'https://publisher.example', writable: true },
    { configurable: false, enumerable: true, value: 'https://publisher.example', writable: false },
    { configurable: false, enumerable: false, get: () => 'https://publisher.example' },
    {
      configurable: false,
      enumerable: false,
      value: 'https://attacker.example/path',
      writable: false,
    },
  ])('rejects an absent, mutable, accessor-backed, or non-origin creative stamp', (stamp) => {
    expect(trustedCriticalOrigin(opaqueDocument(stamp))).toBeUndefined();
  });
});

function manifest(ids: readonly string[]) {
  return {
    version: 1 as const,
    releaseId: RELEASE_ID,
    criticalSrc: `/static/tsjs=tsjs-unified.min.js?v=${'c'.repeat(64)}`,
    integrations: ids.map((id) =>
      id === 'diagnostics_presentation'
        ? {
            id,
            phase: 'deferred' as const,
            trigger: 'first_display_or_idle' as const,
            src: `/static/tsjs=integrations/${id}.min.js?v=${'d'.repeat(64)}`,
          }
        : { id, phase: 'critical' as const }
    ),
  };
}

function boot(
  creative: unknown,
  diagnostics: Readonly<{
    renderTraceOverlay: boolean;
    gptActive: boolean;
  }> = { renderTraceOverlay: false, gptActive: false }
) {
  return {
    auctionProjection: {
      version: 1,
      auction: { version: 1, auctionId: 'initial', results: [] },
      slots: [],
      bids: [],
    },
    creative,
    diagnostics: {
      version: 1,
      renderTraceOverlay: diagnostics.renderTraceOverlay,
      gpt: { active: diagnostics.gptActive },
    },
  };
}

describe('kernel boot creative ABI', () => {
  it.each([
    { version: 1, enabled: false, clickGuard: true, renderGuard: false },
    { version: 1, enabled: false, clickGuard: false, renderGuard: true },
  ])('rejects disabled creative with an enabled guard bit', (creative) => {
    expect(buildKernelBoot(RELEASE_ID, manifest([]), boot(creative))).toBeUndefined();
  });

  it('rejects a null-prototype creative record', () => {
    const creative = Object.assign(Object.create(null) as object, {
      version: 1,
      enabled: false,
      clickGuard: false,
      renderGuard: false,
    });

    expect(buildKernelBoot(RELEASE_ID, manifest([]), boot(creative))).toBeUndefined();
  });

  it.each([
    ['enabled creative without a manifest member', true, []],
    ['enabled creative with duplicate manifest members', true, ['creative', 'creative']],
    ['disabled creative with a manifest member', false, ['creative']],
  ] as const)('rejects %s', (_caseName, enabled, ids) => {
    expect(
      buildKernelBoot(
        RELEASE_ID,
        manifest(ids),
        boot({ version: 1, enabled, clickGuard: false, renderGuard: false })
      )
    ).toBeUndefined();
  });

  it('accepts enabled creative with both guards false only with one manifest member', () => {
    const accepted = buildKernelBoot(
      RELEASE_ID,
      manifest(['creative']),
      boot({ version: 1, enabled: true, clickGuard: false, renderGuard: false })
    ) as { readonly creative?: unknown } | undefined;

    expect(accepted?.creative).toEqual({
      version: 1,
      enabled: true,
      clickGuard: false,
      renderGuard: false,
    });
    expect(Object.isFrozen(accepted?.creative)).toBe(true);
  });
});

describe('terminal fallback boot manifest', () => {
  it('uses the independently trusted critical source when the manifest field is missing', () => {
    const fallback = buildFallbackBoot(
      RELEASE_ID,
      {
        ...boot({ version: 1, enabled: false, clickGuard: false, renderGuard: false }),
        manifest: {
          version: 1,
          releaseId: RELEASE_ID,
          integrations: [],
        },
      },
      TRUSTED_CRITICAL_SRC
    ) as { readonly manifest: unknown };

    expect(fallback.manifest).toEqual({
      version: 1,
      releaseId: RELEASE_ID,
      criticalSrc: TRUSTED_CRITICAL_SRC,
      integrations: [],
    });
  });

  it('uses the independently trusted critical source when the manifest field is malformed', () => {
    const fallback = buildFallbackBoot(
      RELEASE_ID,
      {
        ...boot({ version: 1, enabled: false, clickGuard: false, renderGuard: false }),
        manifest: {
          version: 1,
          releaseId: RELEASE_ID,
          criticalSrc: `/static/tsjs=tsjs-unified.min.js?v=${'e'.repeat(64)}&publisher=1`,
          integrations: [],
        },
      },
      TRUSTED_CRITICAL_SRC
    ) as { readonly manifest: unknown };

    expect(fallback.manifest).toEqual({
      version: 1,
      releaseId: RELEASE_ID,
      criticalSrc: TRUSTED_CRITICAL_SRC,
      integrations: [],
    });
  });

  it('refuses to construct a fallback boot without an independently trusted critical source', () => {
    expect(
      buildFallbackBoot(
        RELEASE_ID,
        boot({ version: 1, enabled: false, clickGuard: false, renderGuard: false }),
        undefined as never
      )
    ).toBeUndefined();
  });

  it('publishes the exact phase-aware fallback manifest with the accepted critical source', () => {
    const acceptedManifest = manifest(['render_runtime', 'diagnostics_presentation']);
    const fallback = buildFallbackBoot(
      RELEASE_ID,
      boot({ version: 1, enabled: false, clickGuard: false, renderGuard: false }),
      acceptedManifest.criticalSrc
    ) as { readonly manifest: unknown };

    expect(fallback.manifest).toEqual({
      version: 1,
      releaseId: RELEASE_ID,
      criticalSrc: acceptedManifest.criticalSrc,
      integrations: [],
    });
    expect(Reflect.ownKeys(fallback.manifest as object).sort()).toEqual([
      'criticalSrc',
      'integrations',
      'releaseId',
      'version',
    ]);
    expect(Object.isFrozen(fallback.manifest)).toBe(true);
  });
});

describe('kernel boot diagnostics presentation membership', () => {
  const disabledCreative = Object.freeze({
    version: 1,
    enabled: false,
    clickGuard: false,
    renderGuard: false,
  });

  it.each([
    { renderTraceOverlay: false, gptActive: false, presentation: false },
    { renderTraceOverlay: true, gptActive: false, presentation: true },
    { renderTraceOverlay: false, gptActive: true, presentation: true },
    { renderTraceOverlay: true, gptActive: true, presentation: true },
  ])(
    'accepts diagnostics_presentation iff overlay=$renderTraceOverlay or GPT=$gptActive',
    ({ renderTraceOverlay, gptActive, presentation }) => {
      const ids = [
        ...(gptActive ? ['gpt_diagnostics'] : []),
        ...(presentation ? ['diagnostics_presentation'] : []),
      ];

      expect(
        buildKernelBoot(
          RELEASE_ID,
          manifest(ids),
          boot(disabledCreative, { renderTraceOverlay, gptActive })
        )
      ).toBeDefined();
    }
  );

  it.each([
    { renderTraceOverlay: false, gptActive: false, presentation: true },
    { renderTraceOverlay: true, gptActive: false, presentation: false },
    { renderTraceOverlay: false, gptActive: true, presentation: false },
    { renderTraceOverlay: true, gptActive: true, presentation: false },
  ])(
    'rejects the inverse diagnostics_presentation membership for overlay=$renderTraceOverlay and GPT=$gptActive',
    ({ renderTraceOverlay, gptActive, presentation }) => {
      const ids = [
        ...(gptActive ? ['gpt_diagnostics'] : []),
        ...(presentation ? ['diagnostics_presentation'] : []),
      ];

      expect(
        buildKernelBoot(
          RELEASE_ID,
          manifest(ids),
          boot(disabledCreative, { renderTraceOverlay, gptActive })
        )
      ).toBeUndefined();
    }
  );

  it('accepts the complete server-shaped phase-aware boot manifest', () => {
    const expectedManifest = manifest(['render_runtime']);
    const candidate = {
      abi: 1,
      releaseId: RELEASE_ID,
      manifest: expectedManifest,
      ...boot(disabledCreative),
    };

    expect(buildKernelBoot(RELEASE_ID, expectedManifest, candidate)).toBeDefined();
  });

  it.each([
    [19, true],
    [20, true],
    [21, false],
  ] as const)('accepts at most %i complete server manifest integrations', (count, accepted) => {
    const expectedManifest = manifest(
      Array.from({ length: count }, (_, index) => `integration_${index + 1}`)
    );
    const candidate = {
      abi: 1,
      releaseId: RELEASE_ID,
      manifest: expectedManifest,
      ...boot(disabledCreative),
    };

    expect(buildKernelBoot(RELEASE_ID, expectedManifest, candidate) !== undefined).toBe(accepted);
  });

  it('rejects a complete boot whose phase-aware manifest differs from the accepted manifest', () => {
    const expectedManifest = manifest(['render_runtime']);
    const candidateManifest = {
      ...expectedManifest,
      criticalSrc: `/static/tsjs=tsjs-unified.min.js?v=${'e'.repeat(64)}`,
    };

    expect(
      buildKernelBoot(RELEASE_ID, expectedManifest, {
        abi: 1,
        releaseId: RELEASE_ID,
        manifest: candidateManifest,
        ...boot(disabledCreative),
      })
    ).toBeUndefined();
  });

  it.each(['diagnostics root', 'diagnostics GPT child'] as const)(
    'rejects a null-prototype %s',
    (target) => {
      const candidate = boot(disabledCreative);
      const diagnostics =
        target === 'diagnostics root'
          ? Object.assign(Object.create(null) as object, candidate.diagnostics)
          : {
              ...candidate.diagnostics,
              gpt: Object.assign(Object.create(null) as object, candidate.diagnostics.gpt),
            };

      expect(
        buildKernelBoot(RELEASE_ID, manifest([]), {
          ...candidate,
          diagnostics,
        })
      ).toBeUndefined();
    }
  );
});
