import { describe, expect, it } from 'vitest';

import { buildKernelBoot } from '../../src/kernel/fallback';

const RELEASE_ID = 'a'.repeat(64);

function manifest(ids: readonly string[]) {
  return {
    version: 1 as const,
    releaseId: RELEASE_ID,
    integrations: ids.map((id) => ({ id, required: true as const })),
  };
}

function boot(creative: unknown) {
  return {
    auctionProjection: {
      version: 1,
      auction: { version: 1, auctionId: 'initial', results: [] },
      bids: [],
    },
    creative,
    diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
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
