import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { describe, expect, it } from 'vitest';

import type { IntegrationRegistration } from '../../src/kernel/integration_registry';
import {
  MAX_TAKEOVER_MODULES,
  MAX_MANIFEST_MODULES,
  RELEASE_CATALOG,
  selectReleaseCatalog,
} from '../../src/kernel/release_catalog';

const RELEASE_ID = 'a'.repeat(64);
const packageRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');

const EXPECTED_CATALOG = Object.freeze([
  [
    'render_runtime',
    'takeover',
    'always',
    ['runtime.v1'],
    [
      'slots.v1',
      'auction.v1',
      'render.v1',
      'messages.v1',
      'trace.v1',
      'trace.presentation.v1',
      'direct.v1',
    ],
  ],
  [
    'aps',
    'takeover',
    'integration:aps',
    ['runtime.v1', 'slots.v1', 'render.v1', 'messages.v1', 'trace.v1'],
    ['aps.v1'],
  ],
  ['creative', 'takeover', 'creative_guard', ['runtime.v1'], []],
  ['datadome', 'takeover', 'integration:datadome', ['runtime.v1'], []],
  ['didomi', 'takeover', 'integration:didomi', ['runtime.v1'], []],
  ['google_tag_manager', 'takeover', 'integration:google_tag_manager', ['runtime.v1'], []],
  [
    'gpt',
    'takeover',
    'integration:gpt',
    ['runtime.v1', 'slots.v1', 'auction.v1', 'render.v1', 'messages.v1', 'trace.v1', 'aps.v1?aps'],
    ['gpt.v1', 'gpt.events.v1', 'pbs_cache.baseline.v1'],
  ],
  [
    'gpt_diagnostics',
    'takeover',
    'gpt_diagnostics_active',
    ['runtime.v1', 'gpt.events.v1'],
    ['gpt_diag.v1'],
  ],
  ['lockr', 'takeover', 'integration:lockr', ['runtime.v1'], []],
  ['osano_consent', 'takeover', 'integration:osano', ['runtime.v1'], ['osano_consent.v1']],
  [
    'permutive_context',
    'takeover',
    'integration:permutive',
    ['runtime.v1'],
    ['permutive_context.v1'],
  ],
  [
    'sourcepoint_consent',
    'takeover',
    'integration:sourcepoint',
    ['runtime.v1'],
    ['sourcepoint_consent.v1'],
  ],
  [
    'prebid',
    'takeover',
    'integration:prebid',
    ['runtime.v1', 'slots.v1', 'render.v1', 'messages.v1', 'aps.v1?aps'],
    ['prebid.v1'],
  ],
  ['testlight', 'takeover', 'integration:testlight', ['runtime.v1'], []],
  [
    'diagnostics_presentation',
    'deferred',
    'diagnostics_presentation',
    ['runtime.v1', 'trace.presentation.v1', 'gpt_diag.v1?gpt_diagnostics_active'],
    [],
  ],
  [
    'gpt_later',
    'deferred',
    'integration:gpt',
    ['runtime.v1', 'slots.v1', 'auction.v1', 'render.v1', 'gpt.v1', 'trace.v1'],
    [],
  ],
  ['osano_lifecycle', 'deferred', 'integration:osano', ['runtime.v1', 'osano_consent.v1'], []],
  [
    'permutive_lifecycle',
    'deferred',
    'integration:permutive',
    ['runtime.v1', 'permutive_context.v1'],
    [],
  ],
  [
    'prebid_later',
    'deferred',
    'prebid_and_gpt',
    ['runtime.v1', 'slots.v1', 'gpt.v1', 'prebid.v1'],
    [],
  ],
  [
    'sourcepoint_lifecycle',
    'deferred',
    'integration:sourcepoint',
    ['runtime.v1', 'sourcepoint_consent.v1'],
    [],
  ],
] as const);

const DEFERRED_FACTORIES = Object.freeze([
  [
    'diagnostics_presentation',
    '../../src/integrations/gpt_diagnostics/presentation',
    'createDiagnosticsPresentationIntegrationRegistration',
  ],
  ['gpt_later', '../../src/integrations/gpt/later', 'createGptLaterIntegrationRegistration'],
  [
    'osano_lifecycle',
    '../../src/integrations/osano/lifecycle',
    'createOsanoLifecycleIntegrationRegistration',
  ],
  [
    'permutive_lifecycle',
    '../../src/integrations/permutive/lifecycle',
    'createPermutiveLifecycleIntegrationRegistration',
  ],
  [
    'prebid_later',
    '../../src/integrations/prebid/later',
    'createPrebidLaterIntegrationRegistration',
  ],
  [
    'sourcepoint_lifecycle',
    '../../src/integrations/sourcepoint/lifecycle',
    'createSourcepointLifecycleIntegrationRegistration',
  ],
] as const);

function selectedIds(selection: Parameters<typeof selectReleaseCatalog>[0]): readonly string[] {
  return selectReleaseCatalog(selection).map(({ id }) => id);
}

function transitiveSources(entry: string): ReadonlySet<string> {
  const visited = new Set<string>();
  const visit = (relative: string): void => {
    const normalized = relative.split('\\').join('/');
    if (visited.has(normalized)) return;
    visited.add(normalized);
    const source = fs.readFileSync(path.join(packageRoot, normalized), 'utf8');
    const expression = /(?:import|export)\s+(?:type\s+)?(?:[^'";]*?\s+from\s+)?['"]([^'"]+)['"]/g;
    for (const match of source.matchAll(expression)) {
      const request = match[1];
      if (!request?.startsWith('.')) continue;
      const base = path.posix.normalize(path.posix.join(path.posix.dirname(normalized), request));
      const candidates = [`${base}.ts`, `${base}.tsx`, path.posix.join(base, 'index.ts')];
      const next = candidates.find((candidate) => fs.existsSync(path.join(packageRoot, candidate)));
      if (next) visit(next);
    }
  };
  visit(entry);
  return visited;
}

describe('canonical takeover and deferred product slices', () => {
  it('maps every spec catalog row exactly once with exact phase, predicate, and capabilities', () => {
    expect(RELEASE_CATALOG).toHaveLength(MAX_MANIFEST_MODULES);
    expect(MAX_MANIFEST_MODULES).toBe(20);
    expect(MAX_TAKEOVER_MODULES).toBe(14);
    expect(new Set(RELEASE_CATALOG.map(({ id }) => id))).toHaveLength(20);
    expect(
      RELEASE_CATALOG.map(({ id, phase, include, consumes, provides }) => [
        id,
        phase,
        include,
        [...consumes],
        [...provides],
      ])
    ).toEqual(EXPECTED_CATALOG);
    expect(RELEASE_CATALOG.every(({ obligation }) => obligation.trim().length > 0)).toBe(true);
    expect(RELEASE_CATALOG.slice(0, 14).every(({ trigger }) => trigger === null)).toBe(true);
    expect(
      RELEASE_CATALOG.slice(14).every(
        ({ trigger, provides }) => trigger === 'first_display_or_idle' && provides.length === 0
      )
    ).toBe(true);
  });

  it('selects every server-owned inclusion predicate without phase overrides', () => {
    expect(selectedIds({ integrations: [] })).toEqual(['render_runtime']);
    expect(
      selectedIds({
        integrations: ['aps', 'gpt', 'prebid', 'osano', 'permutive', 'sourcepoint'],
        creative: { enabled: true, clickGuard: false, renderGuard: true },
        gptDiagnosticsActive: true,
      })
    ).toEqual([
      'render_runtime',
      'aps',
      'creative',
      'gpt',
      'gpt_diagnostics',
      'osano_consent',
      'permutive_context',
      'sourcepoint_consent',
      'prebid',
      'diagnostics_presentation',
      'gpt_later',
      'osano_lifecycle',
      'permutive_lifecycle',
      'prebid_later',
      'sourcepoint_lifecycle',
    ]);
    expect(
      selectedIds({
        integrations: ['prebid'],
        creative: { enabled: true, clickGuard: false, renderGuard: false },
        renderTraceOverlay: true,
      })
    ).toEqual(['render_runtime', 'prebid', 'diagnostics_presentation']);
    expect(() => selectReleaseCatalog({ integrations: ['unknown'] })).toThrow(
      'Unknown integration: unknown'
    );
  });

  it('grants presentation authority to the one deferred presentation slice only', () => {
    const presentationConsumers = RELEASE_CATALOG.filter(({ consumes }) =>
      consumes.some((edge) => edge.startsWith('trace.presentation.v1'))
    );
    expect(presentationConsumers.map(({ id }) => id)).toEqual(['diagnostics_presentation']);
    for (const id of ['aps', 'gpt', 'gpt_later']) {
      expect(RELEASE_CATALOG.find((entry) => entry.id === id)?.consumes).not.toContain(
        'trace.presentation.v1'
      );
    }
  });

  it.each(DEFERRED_FACTORIES)(
    '%s exports its real release-bound deferred registration',
    async (id, request, exportName) => {
      const module = (await import(request)) as Record<string, unknown>;
      const factory = module[exportName];
      expect(factory).toEqual(expect.any(Function));
      const registration = Reflect.apply(
        factory as (releaseId: string) => IntegrationRegistration,
        undefined,
        [RELEASE_ID]
      );
      expect(registration).toMatchObject({ abi: 1, id, phase: 'deferred', releaseId: RELEASE_ID });
      expect(Reflect.ownKeys(registration).sort()).toEqual([
        'abi',
        'id',
        'phase',
        'prepare',
        'releaseId',
      ]);
      expect(Object.isFrozen(registration)).toBe(true);
    }
  );

  it('keeps production core and deferred entry graphs free of test seams and owner duplication', () => {
    const coreSources = transitiveSources('src/composition/runtime_transport.ts');
    expect(
      [...coreSources].some((source) => /(?:browser_test|\/test\/|ForTest)/.test(source))
    ).toBe(false);
    expect(
      [...coreSources].filter((source) => source.startsWith('src/integrations/')).sort()
    ).toEqual(['src/integrations/render_runtime/module.ts']);

    for (const [, request] of DEFERRED_FACTORIES) {
      const entry = `${request.replace('../../', 'src/').replace(/^src\/src\//, 'src/')}.ts`;
      const sources = transitiveSources(entry);
      expect([...sources].some((source) => source.startsWith('src/adapters/'))).toBe(false);
      expect(
        [...sources].some((source) => /composition\/browser(?:_test)?\.ts$/.test(source))
      ).toBe(false);
      expect([...sources].some((source) => source.endsWith('kernel/runtime.ts'))).toBe(false);
    }
  });
});
