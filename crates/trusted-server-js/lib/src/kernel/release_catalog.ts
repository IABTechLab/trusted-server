export type ReleasePhase = 'critical' | 'deferred';
export type ReleaseTrigger = 'first_display_or_idle';
export type ReleaseIncludePredicate =
  | 'always'
  | 'creative_guard'
  | 'gpt_diagnostics_active'
  | 'diagnostics_presentation'
  | 'prebid_and_gpt'
  | `integration:${
      | 'aps'
      | 'datadome'
      | 'didomi'
      | 'google_tag_manager'
      | 'gpt'
      | 'lockr'
      | 'osano'
      | 'permutive'
      | 'prebid'
      | 'sourcepoint'
      | 'testlight'}`;

export interface ReleaseCatalogEntry {
  readonly order: number;
  readonly id: string;
  readonly product: string;
  readonly phase: ReleasePhase;
  readonly trigger: ReleaseTrigger | null;
  readonly include: ReleaseIncludePredicate;
  /** A conditional edge uses `<capability>?<predicate>` and no other grammar. */
  readonly consumes: readonly string[];
  readonly provides: readonly string[];
  readonly obligation: string;
}

export interface ReleaseCatalogSelection {
  readonly integrations: readonly string[];
  readonly creative?: Readonly<{
    enabled: boolean;
    clickGuard: boolean;
    renderGuard: boolean;
  }>;
  readonly gptDiagnosticsActive?: boolean;
  readonly renderTraceOverlay?: boolean;
}

const row = (entry: ReleaseCatalogEntry): ReleaseCatalogEntry =>
  Object.freeze({
    ...entry,
    consumes: Object.freeze([...entry.consumes]),
    provides: Object.freeze([...entry.provides]),
  });

/** The only production TSJS integration-module catalog and injection order. */
export const RELEASE_CATALOG: readonly ReleaseCatalogEntry[] = Object.freeze([
  row({
    order: 1,
    id: 'render_runtime',
    product: 'runtime',
    phase: 'critical',
    trigger: null,
    include: 'always',
    provides: [
      'slots.v1',
      'auction.v1',
      'render.v1',
      'messages.v1',
      'trace.v1',
      'trace.presentation.v1',
      'direct.v1',
    ],
    consumes: ['runtime.v1'],
    obligation: 'Public direct first display, projection, one lifecycle/dispatcher/trace owner',
  }),
  row({
    order: 2,
    id: 'aps',
    product: 'APS',
    phase: 'critical',
    trigger: null,
    include: 'integration:aps',
    provides: ['aps.v1'],
    consumes: ['runtime.v1', 'slots.v1', 'render.v1', 'messages.v1', 'trace.v1'],
    obligation: 'Any initial APS winner and PUC claim must render',
  }),
  row({
    order: 3,
    id: 'creative',
    product: 'creative',
    phase: 'critical',
    trigger: null,
    include: 'creative_guard',
    provides: [],
    consumes: ['runtime.v1'],
    obligation: 'Guards must observe parser-time DOM/constructor activity',
  }),
  row({
    order: 4,
    id: 'datadome',
    product: 'DataDome',
    phase: 'critical',
    trigger: null,
    include: 'integration:datadome',
    provides: [],
    consumes: ['runtime.v1'],
    obligation: 'Script/preload rewriting must precede publisher SDK insertion',
  }),
  row({
    order: 5,
    id: 'didomi',
    product: 'Didomi',
    phase: 'critical',
    trigger: null,
    include: 'integration:didomi',
    provides: [],
    consumes: ['runtime.v1'],
    obligation: 'didomiConfig.sdkPath must exist before SDK evaluation',
  }),
  row({
    order: 6,
    id: 'google_tag_manager',
    product: 'GTM/GA',
    phase: 'critical',
    trigger: null,
    include: 'integration:google_tag_manager',
    provides: [],
    consumes: ['runtime.v1'],
    obligation: 'Script/preload/beacon/fetch guards must precede matching traffic',
  }),
  row({
    order: 7,
    id: 'gpt',
    product: 'GPT',
    phase: 'critical',
    trigger: null,
    include: 'integration:gpt',
    provides: ['gpt.v1', 'gpt.events.v1', 'pbs_cache.baseline.v1'],
    consumes: ['runtime.v1', 'slots.v1', 'auction.v1', 'render.v1', 'messages.v1', 'trace.v1'],
    obligation:
      'Sole GPT adapter/listeners plus every initial handoff/hydration/reconciliation path',
  }),
  row({
    order: 8,
    id: 'gpt_diagnostics',
    product: 'diagnostics',
    phase: 'critical',
    trigger: null,
    include: 'gpt_diagnostics_active',
    provides: ['gpt_diag.v1'],
    consumes: ['runtime.v1', 'gpt.events.v1'],
    obligation:
      'Consume the GPT-owned bounded fact stream and commit the final data-only public API',
  }),
  row({
    order: 9,
    id: 'lockr',
    product: 'Lockr',
    phase: 'critical',
    trigger: null,
    include: 'integration:lockr',
    provides: [],
    consumes: ['runtime.v1'],
    obligation: 'Script guard/readiness/API-host rewrite may precede first display',
  }),
  row({
    order: 10,
    id: 'osano_consent',
    product: 'Osano',
    phase: 'critical',
    trigger: null,
    include: 'integration:osano',
    provides: ['osano_consent.v1'],
    consumes: ['runtime.v1'],
    obligation: 'Initial USP/GPP/TCF mirror must precede consent-dependent auction work',
  }),
  row({
    order: 11,
    id: 'permutive_context',
    product: 'Permutive',
    phase: 'critical',
    trigger: null,
    include: 'integration:permutive',
    provides: ['permutive_context.v1'],
    consumes: ['runtime.v1'],
    obligation: 'Guard/readiness and initial normalized segments feed first auction context',
  }),
  row({
    order: 12,
    id: 'sourcepoint_consent',
    product: 'Sourcepoint',
    phase: 'critical',
    trigger: null,
    include: 'integration:sourcepoint',
    provides: ['sourcepoint_consent.v1'],
    consumes: ['runtime.v1'],
    obligation: 'Initial GPP/localStorage mirror and optional SDK guard precede consent use',
  }),
  row({
    order: 13,
    id: 'prebid',
    product: 'Prebid',
    phase: 'critical',
    trigger: null,
    include: 'integration:prebid',
    provides: ['prebid.v1'],
    consumes: ['runtime.v1', 'slots.v1', 'render.v1', 'messages.v1', 'aps.v1?aps'],
    obligation:
      'External-artifact readiness, bidder aliases, user-ID/EIDs, publisher queue, initial auction, and TS bidder/PUC admission',
  }),
  row({
    order: 14,
    id: 'testlight',
    product: 'Testlight',
    phase: 'critical',
    trigger: null,
    include: 'integration:testlight',
    provides: [],
    consumes: ['runtime.v1'],
    obligation: 'Preexisting callbacks must bridge before publisher code can replace/drain them',
  }),
  row({
    order: 15,
    id: 'diagnostics_presentation',
    product: 'diagnostics',
    phase: 'deferred',
    trigger: 'first_display_or_idle',
    include: 'diagnostics_presentation',
    provides: [],
    consumes: ['runtime.v1', 'trace.presentation.v1', 'gpt_diag.v1?gpt_diagnostics_active'],
    obligation: 'DOM overlay, badges, formatting, clipboard/download interaction',
  }),
  row({
    order: 16,
    id: 'gpt_later',
    product: 'GPT',
    phase: 'deferred',
    trigger: 'first_display_or_idle',
    include: 'integration:gpt',
    provides: [],
    consumes: ['runtime.v1', 'slots.v1', 'auction.v1', 'render.v1', 'gpt.v1', 'trace.v1'],
    obligation: 'Post-first-display refresh, SPA navigation, and later reconciliation only',
  }),
  row({
    order: 17,
    id: 'osano_lifecycle',
    product: 'Osano',
    phase: 'deferred',
    trigger: 'first_display_or_idle',
    include: 'integration:osano',
    provides: [],
    consumes: ['runtime.v1', 'osano_consent.v1'],
    obligation: 'Later retry/event/focus/visibility/clear maintenance',
  }),
  row({
    order: 18,
    id: 'permutive_lifecycle',
    product: 'Permutive',
    phase: 'deferred',
    trigger: 'first_display_or_idle',
    include: 'integration:permutive',
    provides: [],
    consumes: ['runtime.v1', 'permutive_context.v1'],
    obligation: 'Later SDK/segment refresh maintenance',
  }),
  row({
    order: 19,
    id: 'prebid_later',
    product: 'Prebid',
    phase: 'deferred',
    trigger: 'first_display_or_idle',
    include: 'prebid_and_gpt',
    provides: [],
    consumes: ['runtime.v1', 'slots.v1', 'gpt.v1', 'prebid.v1'],
    obligation: 'Synthetic refresh and GAM-path exclusion; never initial admission',
  }),
  row({
    order: 20,
    id: 'sourcepoint_lifecycle',
    product: 'Sourcepoint',
    phase: 'deferred',
    trigger: 'first_display_or_idle',
    include: 'integration:sourcepoint',
    provides: [],
    consumes: ['runtime.v1', 'sourcepoint_consent.v1'],
    obligation: 'Later retry/visibility/focus/update/safe-clear maintenance',
  }),
]);

const unconditionalCapability = (edge: string): string => edge.split('?', 1)[0]!;

/** Validate ordering and capability invariants for a catalog or selected prefix. */
export function validateReleaseCatalog(entries: readonly ReleaseCatalogEntry[]): void {
  if (entries.length > 20) throw new TypeError('Release catalog exceeds manifest capacity');
  const ids = new Set<string>();
  const providers = new Map<string, number>([['runtime.v1', 0]]);
  let sawDeferred = false;
  let criticalCount = 0;

  for (const [index, entry] of entries.entries()) {
    if (entry.order !== index + 1) throw new TypeError('Release catalog order is invalid');
    const canonical = RELEASE_CATALOG[index];
    if (
      !canonical ||
      entry.id !== canonical.id ||
      entry.phase !== canonical.phase ||
      entry.trigger !== canonical.trigger ||
      entry.include !== canonical.include
    ) {
      throw new TypeError('Release catalog id, phase override, trigger, or predicate is invalid');
    }
    if (!/^[a-z0-9][a-z0-9_-]{0,63}$/.test(entry.id) || ids.has(entry.id)) {
      throw new TypeError('Release catalog id is invalid or duplicated');
    }
    ids.add(entry.id);
    if (entry.phase === 'critical') {
      criticalCount += 1;
      if (criticalCount > 14) throw new TypeError('Release catalog exceeds critical capacity');
      if (sawDeferred || entry.trigger !== null)
        throw new TypeError('Critical phase order is invalid');
    } else {
      sawDeferred = true;
      if (entry.trigger !== 'first_display_or_idle') {
        throw new TypeError('Deferred phase trigger is invalid');
      }
      if (entry.provides.length !== 0) throw new TypeError('Deferred provider is forbidden');
    }
    for (const capability of entry.provides) {
      if (providers.has(capability)) throw new TypeError('Capability has multiple providers');
      providers.set(capability, entry.order);
    }
  }

  for (const entry of entries) {
    for (const edge of entry.consumes) {
      const conditionalParts = edge.split('?');
      if (
        conditionalParts.length > 2 ||
        (conditionalParts.length === 2 &&
          !['aps.v1?aps', 'gpt_diag.v1?gpt_diagnostics_active'].includes(edge))
      ) {
        throw new TypeError(`Invalid conditional capability edge: ${edge}`);
      }
      const capability = unconditionalCapability(edge);
      const providerOrder = providers.get(capability);
      if (providerOrder === undefined)
        throw new TypeError(`Unknown capability edge: ${capability}`);
      if (providerOrder >= entry.order) {
        throw new TypeError('Capability provider order creates an order violation or cycle');
      }
    }
  }
}

const KNOWN_INTEGRATIONS = new Set(
  RELEASE_CATALOG.flatMap(({ include }) =>
    include.startsWith('integration:') ? [include.slice('integration:'.length)] : []
  )
);

/** Select immutable manifest rows from trusted server-owned page configuration. */
export function selectReleaseCatalog(
  selection: ReleaseCatalogSelection
): readonly ReleaseCatalogEntry[] {
  const integrations = new Set<string>();
  for (const id of selection.integrations) {
    if (!KNOWN_INTEGRATIONS.has(id)) throw new TypeError(`Unknown integration: ${id}`);
    integrations.add(id);
  }
  const include = (predicate: ReleaseIncludePredicate): boolean => {
    if (predicate === 'always') return true;
    if (predicate === 'creative_guard') {
      const creative = selection.creative;
      return Boolean(creative?.enabled && (creative.clickGuard || creative.renderGuard));
    }
    if (predicate === 'gpt_diagnostics_active') return selection.gptDiagnosticsActive === true;
    if (predicate === 'diagnostics_presentation') {
      return selection.renderTraceOverlay === true || selection.gptDiagnosticsActive === true;
    }
    if (predicate === 'prebid_and_gpt') {
      return integrations.has('prebid') && integrations.has('gpt');
    }
    return integrations.has(predicate.slice('integration:'.length));
  };

  return Object.freeze(RELEASE_CATALOG.filter((entry) => include(entry.include)));
}

validateReleaseCatalog(RELEASE_CATALOG);

export const MAX_CRITICAL_MODULES = RELEASE_CATALOG.filter(
  ({ phase }) => phase === 'critical'
).length;
export const MAX_MANIFEST_MODULES = RELEASE_CATALOG.length;
export const MINIMAL_CRITICAL_IDS = Object.freeze(['core', 'render_runtime'] as const);
export const REFERENCE_CRITICAL_IDS = Object.freeze([
  'core',
  'render_runtime',
  'creative',
  'gpt',
  'prebid',
  'datadome',
] as const);
