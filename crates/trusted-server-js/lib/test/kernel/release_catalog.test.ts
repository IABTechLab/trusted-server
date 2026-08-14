import { describe, expect, it } from 'vitest';

import { FIRST_DISPLAY_CONTRACT_IDS } from '../../src/first_display/contracts';
import * as releaseCatalog from '../../src/kernel/release_catalog';
import {
  FIRST_DISPLAY_CATALOG,
  MAX_FIRST_DISPLAY_SLICES,
  MAX_CRITICAL_MODULES,
  MAX_MANIFEST_MODULES,
  MINIMAL_CRITICAL_IDS,
  REFERENCE_CRITICAL_IDS,
  RELEASE_CATALOG,
  selectFirstDisplayCatalog,
  selectReleaseCatalog,
  validateReleaseCatalog,
  type ReleaseCatalogEntry,
} from '../../src/kernel/release_catalog';

const EXPECTED = [
  ['render_runtime', 'runtime', 'critical', null, 'always'],
  ['aps', 'APS', 'critical', null, 'integration:aps'],
  ['creative', 'creative', 'critical', null, 'creative_guard'],
  ['datadome', 'DataDome', 'critical', null, 'integration:datadome'],
  ['didomi', 'Didomi', 'critical', null, 'integration:didomi'],
  ['google_tag_manager', 'GTM/GA', 'critical', null, 'integration:google_tag_manager'],
  ['gpt', 'GPT', 'critical', null, 'integration:gpt'],
  ['gpt_diagnostics', 'diagnostics', 'critical', null, 'gpt_diagnostics_active'],
  ['lockr', 'Lockr', 'critical', null, 'integration:lockr'],
  ['osano_consent', 'Osano', 'critical', null, 'integration:osano'],
  ['permutive_context', 'Permutive', 'critical', null, 'integration:permutive'],
  ['sourcepoint_consent', 'Sourcepoint', 'critical', null, 'integration:sourcepoint'],
  ['prebid', 'Prebid', 'critical', null, 'integration:prebid'],
  ['testlight', 'Testlight', 'critical', null, 'integration:testlight'],
  [
    'diagnostics_presentation',
    'diagnostics',
    'deferred',
    'first_display_or_idle',
    'diagnostics_presentation',
  ],
  ['gpt_later', 'GPT', 'deferred', 'first_display_or_idle', 'integration:gpt'],
  ['osano_lifecycle', 'Osano', 'deferred', 'first_display_or_idle', 'integration:osano'],
  [
    'permutive_lifecycle',
    'Permutive',
    'deferred',
    'first_display_or_idle',
    'integration:permutive',
  ],
  ['prebid_later', 'Prebid', 'deferred', 'first_display_or_idle', 'prebid_and_gpt'],
  [
    'sourcepoint_lifecycle',
    'Sourcepoint',
    'deferred',
    'first_display_or_idle',
    'integration:sourcepoint',
  ],
] as const;

describe('canonical release catalog', () => {
  it('pins the exact thirteen first-display rows and closed server-owned selection', () => {
    expect(FIRST_DISPLAY_CONTRACT_IDS).toEqual(FIRST_DISPLAY_CATALOG.map(({ id }) => id));
    expect(FIRST_DISPLAY_CATALOG.map(({ order, id }) => [order, id])).toEqual([
      [1, 'first_display'],
      [2, 'aps_initial'],
      [3, 'creative_initial'],
      [4, 'datadome_initial'],
      [5, 'didomi_initial'],
      [6, 'google_tag_manager_initial'],
      [7, 'gpt_initial'],
      [8, 'lockr_initial'],
      [9, 'osano_initial'],
      [10, 'permutive_initial'],
      [11, 'sourcepoint_initial'],
      [12, 'prebid_initial'],
      [13, 'testlight_initial'],
    ]);
    expect(MAX_FIRST_DISPLAY_SLICES).toBe(13);
    expect(
      selectFirstDisplayCatalog({
        eligibleBatch: true,
        integrations: ['aps', 'gpt', 'prebid'],
        apsParticipates: true,
        prebidParticipates: true,
      }).map(({ id }) => id)
    ).toEqual(['first_display', 'aps_initial', 'gpt_initial', 'prebid_initial']);
    expect(selectFirstDisplayCatalog({ eligibleBatch: false, integrations: [] })).toEqual([]);
    expect(() =>
      selectFirstDisplayCatalog({ eligibleBatch: true, integrations: ['unknown'] })
    ).toThrow(/unknown/i);
    expect(
      FIRST_DISPLAY_CATALOG.every(
        ({ allowedImports, inputs, outputs, obligation }) =>
          allowedImports.length > 0 &&
          inputs.length > 0 &&
          outputs.length > 0 &&
          obligation.length > 0
      )
    ).toBe(true);
  });
  it('pins the exact twenty rows, phases, triggers, products, predicates, and order', () => {
    expect(
      RELEASE_CATALOG.map(({ id, product, phase, trigger, include }) => [
        id,
        product,
        phase,
        trigger,
        include,
      ])
    ).toEqual(EXPECTED);
    expect(RELEASE_CATALOG.map(({ order }) => order)).toEqual(
      Array.from({ length: 20 }, (_, index) => index + 1)
    );
  });

  it('pins the exact capability graph and named scopes', () => {
    expect(RELEASE_CATALOG.map(({ id, provides, consumes }) => [id, provides, consumes])).toEqual([
      [
        'render_runtime',
        [
          'slots.v1',
          'auction.v1',
          'render.v1',
          'messages.v1',
          'trace.v1',
          'trace.presentation.v1',
          'direct.v1',
        ],
        ['runtime.v1'],
      ],
      ['aps', ['aps.v1'], ['runtime.v1', 'slots.v1', 'render.v1', 'messages.v1', 'trace.v1']],
      ['creative', [], ['runtime.v1']],
      ['datadome', [], ['runtime.v1']],
      ['didomi', [], ['runtime.v1']],
      ['google_tag_manager', [], ['runtime.v1']],
      [
        'gpt',
        ['gpt.v1', 'gpt.events.v1', 'pbs_cache.baseline.v1'],
        ['runtime.v1', 'slots.v1', 'auction.v1', 'render.v1', 'messages.v1', 'trace.v1'],
      ],
      ['gpt_diagnostics', ['gpt_diag.v1'], ['runtime.v1', 'gpt.events.v1']],
      ['lockr', [], ['runtime.v1']],
      ['osano_consent', ['osano_consent.v1'], ['runtime.v1']],
      ['permutive_context', ['permutive_context.v1'], ['runtime.v1']],
      ['sourcepoint_consent', ['sourcepoint_consent.v1'], ['runtime.v1']],
      [
        'prebid',
        ['prebid.v1'],
        ['runtime.v1', 'slots.v1', 'render.v1', 'messages.v1', 'aps.v1?aps'],
      ],
      ['testlight', [], ['runtime.v1']],
      [
        'diagnostics_presentation',
        [],
        ['runtime.v1', 'trace.presentation.v1', 'gpt_diag.v1?gpt_diagnostics_active'],
      ],
      [
        'gpt_later',
        [],
        ['runtime.v1', 'slots.v1', 'auction.v1', 'render.v1', 'gpt.v1', 'trace.v1'],
      ],
      ['osano_lifecycle', [], ['runtime.v1', 'osano_consent.v1']],
      ['permutive_lifecycle', [], ['runtime.v1', 'permutive_context.v1']],
      ['prebid_later', [], ['runtime.v1', 'slots.v1', 'gpt.v1', 'prebid.v1']],
      ['sourcepoint_lifecycle', [], ['runtime.v1', 'sourcepoint_consent.v1']],
    ]);
    expect(RELEASE_CATALOG.every(({ obligation }) => obligation.length > 0)).toBe(true);
  });

  it('derives capacity and budget vectors without an internal diagnostics subscriber cap', () => {
    expect(MAX_CRITICAL_MODULES).toBe(14);
    expect(MAX_MANIFEST_MODULES).toBe(20);
    expect('MAX_INTERNAL_DIAGNOSTICS_SUBSCRIPTIONS' in releaseCatalog).toBe(false);
    expect(MINIMAL_CRITICAL_IDS).toEqual(['core', 'render_runtime']);
    expect(REFERENCE_CRITICAL_IDS).toEqual([
      'core',
      'render_runtime',
      'creative',
      'gpt',
      'prebid',
      'datadome',
    ]);
    expect(() => validateReleaseCatalog(RELEASE_CATALOG.slice(0, 13))).not.toThrow();
    expect(() => validateReleaseCatalog(RELEASE_CATALOG.slice(0, 14))).not.toThrow();
    expect(() => validateReleaseCatalog(RELEASE_CATALOG.slice(0, 15))).not.toThrow();
    expect(() => validateReleaseCatalog(RELEASE_CATALOG.slice(0, 19))).not.toThrow();
    expect(() => validateReleaseCatalog(RELEASE_CATALOG.slice(0, 20))).not.toThrow();
    const fifteenCritical = [
      ...RELEASE_CATALOG.slice(0, 14),
      {
        ...RELEASE_CATALOG[14]!,
        phase: 'critical' as const,
        trigger: null,
      },
    ];
    expect(() => validateReleaseCatalog(fifteenCritical)).toThrow(
      /critical capacity|phase override/i
    );
    expect(() => validateReleaseCatalog([...RELEASE_CATALOG, RELEASE_CATALOG[0]!])).toThrow();
  });

  it('selects rows only through deny-unknown server-owned predicates', () => {
    expect(selectReleaseCatalog({ integrations: [] }).map(({ id }) => id)).toEqual([
      'render_runtime',
    ]);
    expect(
      selectReleaseCatalog({
        integrations: ['aps', 'gpt', 'prebid'],
        creative: { enabled: true, clickGuard: false, renderGuard: true },
        gptDiagnosticsActive: true,
        renderTraceOverlay: true,
      }).map(({ id }) => id)
    ).toEqual([
      'render_runtime',
      'aps',
      'creative',
      'gpt',
      'gpt_diagnostics',
      'prebid',
      'diagnostics_presentation',
      'gpt_later',
      'prebid_later',
    ]);
    expect(
      selectReleaseCatalog({
        integrations: [],
        gptDiagnosticsActive: true,
        renderTraceOverlay: false,
      }).map(({ id }) => id)
    ).toEqual(['render_runtime', 'gpt_diagnostics', 'diagnostics_presentation']);
    expect(
      selectReleaseCatalog({
        integrations: [],
        gptDiagnosticsActive: false,
        renderTraceOverlay: true,
      }).map(({ id }) => id)
    ).toEqual(['render_runtime', 'diagnostics_presentation']);
    expect(
      selectReleaseCatalog({
        integrations: [],
        gptDiagnosticsActive: false,
        renderTraceOverlay: false,
      }).map(({ id }) => id)
    ).toEqual(['render_runtime']);
    expect(() => selectReleaseCatalog({ integrations: ['unknown'] })).toThrow(/unknown/i);
  });

  it('rejects duplicate providers, undeclared edges, cycles, deferred providers, and bad order', () => {
    const clone = (): ReleaseCatalogEntry[] => RELEASE_CATALOG.map((entry) => ({ ...entry }));

    const duplicateProvider = clone();
    duplicateProvider[2] = { ...duplicateProvider[2]!, provides: ['aps.v1'] };
    expect(() => validateReleaseCatalog(duplicateProvider)).toThrow(/provider/i);

    const unknownEdge = clone();
    unknownEdge[2] = { ...unknownEdge[2]!, consumes: ['missing.v1'] };
    expect(() => validateReleaseCatalog(unknownEdge)).toThrow(/capability/i);

    const deferredProvider = clone();
    deferredProvider[14] = { ...deferredProvider[14]!, provides: ['later.v1'] };
    expect(() => validateReleaseCatalog(deferredProvider)).toThrow(/deferred provider/i);

    const cycle = clone();
    cycle[0] = { ...cycle[0]!, consumes: ['aps.v1'] };
    expect(() => validateReleaseCatalog(cycle)).toThrow(/order|cycle/i);

    const wrongOrder = clone();
    [wrongOrder[0], wrongOrder[1]] = [wrongOrder[1]!, wrongOrder[0]!];
    expect(() => validateReleaseCatalog(wrongOrder)).toThrow(/order/i);

    const phaseOverride = clone();
    phaseOverride[13] = {
      ...phaseOverride[13]!,
      phase: 'deferred',
      trigger: 'first_display_or_idle',
    };
    expect(() => validateReleaseCatalog(phaseOverride)).toThrow(/phase override/i);

    const invalidConditionalEdge = clone();
    invalidConditionalEdge[12] = {
      ...invalidConditionalEdge[12]!,
      consumes: ['runtime.v1', 'aps.v1?publisher_choice'],
    };
    expect(() => validateReleaseCatalog(invalidConditionalEdge)).toThrow(/conditional/i);
  });
});
