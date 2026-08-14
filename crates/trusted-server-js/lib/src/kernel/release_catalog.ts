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

export type FirstDisplaySliceId =
  | 'first_display'
  | 'aps_initial'
  | 'creative_initial'
  | 'datadome_initial'
  | 'didomi_initial'
  | 'google_tag_manager_initial'
  | 'gpt_initial'
  | 'lockr_initial'
  | 'osano_initial'
  | 'permutive_initial'
  | 'sourcepoint_initial'
  | 'prebid_initial'
  | 'testlight_initial';

export type FirstDisplayIncludePredicate =
  | 'eligible_batch'
  | 'aps_participates'
  | 'creative_guard'
  | 'gpt_initial'
  | 'prebid_participates'
  | `integration:${
      | 'datadome'
      | 'didomi'
      | 'google_tag_manager'
      | 'lockr'
      | 'osano'
      | 'permutive'
      | 'sourcepoint'
      | 'testlight'}`;

export interface FirstDisplayCatalogEntry {
  readonly order: number;
  readonly id: FirstDisplaySliceId;
  readonly include: FirstDisplayIncludePredicate;
  readonly allowedImports: readonly string[];
  readonly inputs: readonly string[];
  readonly outputs: readonly string[];
  readonly obligation: string;
}

export interface FirstDisplayCatalogSelection {
  readonly eligibleBatch: boolean;
  readonly integrations: readonly string[];
  readonly apsParticipates?: boolean;
  readonly prebidParticipates?: boolean;
  readonly creative?: Readonly<{
    enabled: boolean;
    clickGuard: boolean;
    renderGuard: boolean;
  }>;
}

const firstDisplayRow = (entry: FirstDisplayCatalogEntry): FirstDisplayCatalogEntry =>
  Object.freeze({
    ...entry,
    allowedImports: Object.freeze([...entry.allowedImports]),
    inputs: Object.freeze([...entry.inputs]),
    outputs: Object.freeze([...entry.outputs]),
  });

/** Closed build catalog for the one provisional first-display artifact. */
export const FIRST_DISPLAY_CATALOG: readonly FirstDisplayCatalogEntry[] = Object.freeze([
  firstDisplayRow({
    order: 1,
    id: 'first_display',
    include: 'eligible_batch',
    allowedImports: [
      'first_display/contracts',
      'first_display/driver',
      'first_display/leaf/projection',
      'first_display/render_bridge',
      'first_display/registration',
      'first_display/transaction',
      'kernel/contracts/puc_dynamic_owner',
    ],
    inputs: ['boot.v1', 'projection.v1'],
    outputs: ['first_display.control.v1'],
    obligation:
      'Validate the immutable batch and own provisional lifetime, timing, ingress, and transfer',
  }),
  firstDisplayRow({
    order: 2,
    id: 'aps_initial',
    include: 'aps_participates',
    allowedImports: [
      'first_display/contracts',
      'first_display/registration',
      'first_display/slices/definition',
      'first_display/leaf/aps_protocol',
    ],
    inputs: ['first_display.control.v1'],
    outputs: ['aps.initial.v1'],
    obligation: 'Own initial reservation, PUC, and APS document protocols',
  }),
  firstDisplayRow({
    order: 3,
    id: 'creative_initial',
    include: 'creative_guard',
    allowedImports: [
      'first_display/contracts',
      'first_display/registration',
      'first_display/slices/definition',
      'first_display/leaf/creative_guard',
    ],
    inputs: ['first_display.control.v1'],
    outputs: ['creative.initial.v1'],
    obligation: 'Install current parser-time creative guards and record initial observations only',
  }),
  firstDisplayRow({
    order: 4,
    id: 'datadome_initial',
    include: 'integration:datadome',
    allowedImports: [
      'first_display/contracts',
      'first_display/registration',
      'first_display/slices/definition',
      'first_display/leaf/route_guard',
    ],
    inputs: ['first_display.control.v1'],
    outputs: ['datadome.initial.v1'],
    obligation: 'Install the initial DataDome script and preload route guard',
  }),
  firstDisplayRow({
    order: 5,
    id: 'didomi_initial',
    include: 'integration:didomi',
    allowedImports: [
      'first_display/contracts',
      'first_display/registration',
      'first_display/slices/definition',
      'first_display/leaf/config_guard',
    ],
    inputs: ['first_display.control.v1'],
    outputs: ['didomi.initial.v1'],
    obligation: 'Install the configured Didomi SDK path before SDK evaluation',
  }),
  firstDisplayRow({
    order: 6,
    id: 'google_tag_manager_initial',
    include: 'integration:google_tag_manager',
    allowedImports: [
      'first_display/contracts',
      'first_display/registration',
      'first_display/slices/definition',
      'first_display/leaf/route_guard',
    ],
    inputs: ['first_display.control.v1'],
    outputs: ['google_tag_manager.initial.v1'],
    obligation: 'Install initial GTM script, preload, beacon, and fetch guards',
  }),
  firstDisplayRow({
    order: 7,
    id: 'gpt_initial',
    include: 'gpt_initial',
    allowedImports: [
      'first_display/contracts',
      'first_display/registration',
      'first_display/slices/definition',
      'first_display/adapters/googletag',
      'first_display/leaf/gpt_protocol',
    ],
    inputs: ['first_display.control.v1'],
    outputs: ['gpt.initial.v1'],
    obligation: 'Sole provisional GPT adapter, listeners, targeting, request, and handoff capture',
  }),
  firstDisplayRow({
    order: 8,
    id: 'lockr_initial',
    include: 'integration:lockr',
    allowedImports: [
      'first_display/contracts',
      'first_display/registration',
      'first_display/slices/definition',
      'first_display/leaf/route_guard',
    ],
    inputs: ['first_display.control.v1'],
    outputs: ['lockr.initial.v1'],
    obligation: 'Install the initial Lockr script guard and bounded readiness observation',
  }),
  firstDisplayRow({
    order: 9,
    id: 'osano_initial',
    include: 'integration:osano',
    allowedImports: [
      'first_display/contracts',
      'first_display/registration',
      'first_display/slices/definition',
      'first_display/leaf/consent_snapshot',
    ],
    inputs: ['first_display.control.v1'],
    outputs: ['osano.initial.v1'],
    obligation: 'Capture the initial consent mirrors required by the protected batch',
  }),
  firstDisplayRow({
    order: 10,
    id: 'permutive_initial',
    include: 'integration:permutive',
    allowedImports: [
      'first_display/contracts',
      'first_display/registration',
      'first_display/slices/definition',
      'first_display/leaf/context_snapshot',
    ],
    inputs: ['first_display.control.v1'],
    outputs: ['permutive.initial.v1'],
    obligation: 'Install initial guard/readiness and capture normalized segments',
  }),
  firstDisplayRow({
    order: 11,
    id: 'sourcepoint_initial',
    include: 'integration:sourcepoint',
    allowedImports: [
      'first_display/contracts',
      'first_display/registration',
      'first_display/slices/definition',
      'first_display/leaf/consent_snapshot',
    ],
    inputs: ['first_display.control.v1'],
    outputs: ['sourcepoint.initial.v1'],
    obligation: 'Install the initial SDK guard and capture the GPP mirror',
  }),
  firstDisplayRow({
    order: 12,
    id: 'prebid_initial',
    include: 'prebid_participates',
    allowedImports: [
      'first_display/contracts',
      'first_display/registration',
      'first_display/slices/definition',
      'first_display/leaf/prebid_protocol',
    ],
    inputs: ['first_display.control.v1', 'gpt.initial.v1'],
    outputs: ['prebid.initial.v1'],
    obligation:
      'Own initial artifact admission, queue, bidder, identity, EID, TS bid, and PUC setup',
  }),
  firstDisplayRow({
    order: 13,
    id: 'testlight_initial',
    include: 'integration:testlight',
    allowedImports: [
      'first_display/contracts',
      'first_display/registration',
      'first_display/slices/definition',
      'first_display/leaf/callback_capture',
    ],
    inputs: ['first_display.control.v1'],
    outputs: ['testlight.initial.v1'],
    obligation: 'Capture preexisting callbacks before publisher replacement or drain',
  }),
]);

const FIRST_DISPLAY_KNOWN_INTEGRATIONS = new Set([
  'aps',
  'creative',
  'datadome',
  'didomi',
  'google_tag_manager',
  'gpt',
  'lockr',
  'osano',
  'permutive',
  'prebid',
  'sourcepoint',
  'testlight',
]);

/** Select the exact ordered provisional slices from trusted server-owned facts. */
export function selectFirstDisplayCatalog(
  selection: FirstDisplayCatalogSelection
): readonly FirstDisplayCatalogEntry[] {
  const integrations = new Set<string>();
  for (const id of selection.integrations) {
    if (!FIRST_DISPLAY_KNOWN_INTEGRATIONS.has(id)) {
      throw new TypeError(`Unknown first-display integration: ${id}`);
    }
    integrations.add(id);
  }
  if (!selection.eligibleBatch) return Object.freeze([]);

  const include = (predicate: FirstDisplayIncludePredicate): boolean => {
    if (predicate === 'eligible_batch') return true;
    if (predicate === 'aps_participates') {
      return selection.apsParticipates === true && integrations.has('aps');
    }
    if (predicate === 'prebid_participates') {
      return selection.prebidParticipates === true && integrations.has('prebid');
    }
    if (predicate === 'gpt_initial') return integrations.has('gpt');
    if (predicate === 'creative_guard') {
      const creative = selection.creative;
      return Boolean(creative?.enabled && (creative.clickGuard || creative.renderGuard));
    }
    return integrations.has(predicate.slice('integration:'.length));
  };

  const selected = FIRST_DISPLAY_CATALOG.filter((entry) => include(entry.include));
  if (!selected.some(({ id }) => id === 'gpt_initial')) return Object.freeze([]);
  return Object.freeze(selected);
}

export const MAX_FIRST_DISPLAY_SLICES = FIRST_DISPLAY_CATALOG.length;

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
