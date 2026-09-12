import {
  snapshotTakeoverOutlineV1,
  type TakeoverOutlineV1,
} from '../../shared/first_display_contracts';
import type {
  BootManifestIntegrationV1,
  BootManifestV1,
  IntegrationConfigIdV1,
  TsjsBootV1,
} from '../types';
import { INTEGRATION_CONFIG_IDS_V1 } from '../types';

import { parseBrowserAuctionProjectionV1 } from './auction_projection';
import {
  canonicalIntegrationConfigDigestV1,
  snapshotIntegrationConfigsV1,
} from './integration_configs';

const FIRST_DISPLAY_SRC =
  /^\/static\/tsjs=tsjs-first-display\.min\.js\?m=([0-9a-f]{4})&v=[0-9a-f]{64}$/;
const RUNTIME_SRC = /^\/static\/tsjs=tsjs-unified\.min\.js\?v=[0-9a-f]{64}$/;
const DEFERRED_SRC = /^\/static\/tsjs=tsjs-[a-z0-9][a-z0-9_-]{0,63}\.min\.js\?v=[0-9a-f]{64}$/;
const INTEGRATION_ID = /^[a-z0-9][a-z0-9_-]{0,63}$/;
const RELEASE_ID = /^[0-9a-f]{64}$/;
const FIRST_DISPLAY_IDS = Object.freeze([
  'first_display',
  'render_owner_initial',
  'aps_initial',
  'creative_initial',
  'datadome_initial',
  'didomi_initial',
  'google_tag_manager_initial',
  'gpt_initial',
  'lockr_initial',
  'osano_initial',
  'permutive_initial',
  'sourcepoint_initial',
  'prebid_initial',
  'testlight_initial',
] as const);

export interface BootstrapInputSnapshotV1 {
  readonly target: object & { boot?: unknown; que?: unknown };
  readonly boot: Readonly<TsjsBootV1>;
  readonly outline: TakeoverOutlineV1 | null;
}

function ownPlainDataRecord(value: unknown): Record<string, unknown> | undefined {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) return undefined;
  if (Object.getPrototypeOf(value) !== Object.prototype) return undefined;
  const result: Record<string, unknown> = {};
  for (const key of Reflect.ownKeys(value)) {
    if (typeof key !== 'string') return undefined;
    const descriptor = Object.getOwnPropertyDescriptor(value, key);
    if (!descriptor?.enumerable || !('value' in descriptor)) return undefined;
    Object.defineProperty(result, key, {
      configurable: true,
      enumerable: true,
      value: descriptor.value,
      writable: true,
    });
  }
  return result;
}

function exactKeys(value: Record<string, unknown>, keys: readonly string[]): boolean {
  const actual = Object.keys(value);
  return actual.length === keys.length && actual.every((key) => keys.includes(key));
}

function snapshotArray(value: unknown, maximum: number): readonly unknown[] | undefined {
  if (!Array.isArray(value) || Object.getPrototypeOf(value) !== Array.prototype) return undefined;
  const length = Object.getOwnPropertyDescriptor(value, 'length');
  if (!length || !('value' in length) || !Number.isSafeInteger(length.value)) return undefined;
  if (length.value < 0 || length.value > maximum) return undefined;
  const keys = Reflect.ownKeys(value);
  if (keys.length !== length.value + 1) return undefined;
  const result: unknown[] = [];
  for (let index = 0; index < length.value; index += 1) {
    const descriptor = Object.getOwnPropertyDescriptor(value, String(index));
    if (!descriptor?.enumerable || !('value' in descriptor)) return undefined;
    result.push(descriptor.value);
  }
  return result;
}

function snapshotManifest(candidate: unknown, releaseId: string): BootManifestV1 | undefined {
  const root = ownPlainDataRecord(candidate);
  if (
    !root ||
    !exactKeys(root, ['version', 'releaseId', 'firstDisplay', 'runtimeSrc', 'integrations']) ||
    root.version !== 1 ||
    root.releaseId !== releaseId ||
    typeof root.runtimeSrc !== 'string' ||
    !RUNTIME_SRC.test(root.runtimeSrc)
  ) {
    return undefined;
  }

  const rawIntegrations = snapshotArray(root.integrations, 20);
  if (!rawIntegrations) return undefined;
  const integrations: BootManifestIntegrationV1[] = [];
  const ids = new Set<string>();
  let sawDeferred = false;
  for (const candidateEntry of rawIntegrations) {
    const entry = ownPlainDataRecord(candidateEntry);
    if (
      !entry ||
      typeof entry.id !== 'string' ||
      !INTEGRATION_ID.test(entry.id) ||
      ids.has(entry.id)
    ) {
      return undefined;
    }
    ids.add(entry.id);
    if (entry.phase === 'takeover') {
      if (sawDeferred || !exactKeys(entry, ['id', 'phase'])) return undefined;
      integrations.push(Object.freeze({ id: entry.id, phase: 'takeover' }));
      continue;
    }
    if (
      entry.phase !== 'deferred' ||
      !exactKeys(entry, ['id', 'phase', 'trigger', 'src']) ||
      entry.trigger !== 'first_display_or_idle' ||
      typeof entry.src !== 'string' ||
      !DEFERRED_SRC.test(entry.src)
    ) {
      return undefined;
    }
    sawDeferred = true;
    integrations.push(
      Object.freeze({
        id: entry.id,
        phase: 'deferred',
        trigger: 'first_display_or_idle',
        src: entry.src,
      })
    );
  }

  let firstDisplay: BootManifestV1['firstDisplay'];
  if (root.firstDisplay === null) {
    firstDisplay = null;
  } else {
    const fields = ownPlainDataRecord(root.firstDisplay);
    const slices = snapshotArray(fields?.slices, FIRST_DISPLAY_IDS.length);
    if (
      !fields ||
      !exactKeys(fields, ['src', 'slices']) ||
      typeof fields.src !== 'string' ||
      !slices ||
      slices.length === 0 ||
      slices.some((id) => typeof id !== 'string')
    ) {
      return undefined;
    }
    const match = FIRST_DISPLAY_SRC.exec(fields.src);
    if (!match) return undefined;
    const mask = Number.parseInt(match[1]!, 16);
    const selected = FIRST_DISPLAY_IDS.filter((_id, index) => (mask & (1 << index)) !== 0);
    if (
      (mask & 1) === 0 ||
      mask >>> FIRST_DISPLAY_IDS.length !== 0 ||
      ((mask & 4) !== 0 && (mask & 2) === 0) ||
      ((mask & 2) !== 0 && (mask & 128) === 0) ||
      selected.length !== slices.length ||
      selected.some((id, index) => id !== slices[index])
    ) {
      return undefined;
    }
    firstDisplay = Object.freeze({
      src: fields.src,
      slices: Object.freeze([...slices] as string[]),
    });
  }

  return Object.freeze({
    version: 1,
    releaseId,
    firstDisplay,
    runtimeSrc: root.runtimeSrc,
    integrations: Object.freeze(integrations),
  });
}

function productForModule(id: string): IntegrationConfigIdV1 | undefined {
  if (id === 'gpt' || id === 'gpt_later') return 'gpt';
  if (id === 'osano_consent' || id === 'osano_lifecycle') return 'osano';
  if (id === 'permutive_context' || id === 'permutive_lifecycle') return 'permutive';
  if (id === 'prebid' || id === 'prebid_later') return 'prebid';
  if (id === 'sourcepoint_consent' || id === 'sourcepoint_lifecycle') return 'sourcepoint';
  return INTEGRATION_CONFIG_IDS_V1.includes(id as IntegrationConfigIdV1)
    ? (id as IntegrationConfigIdV1)
    : undefined;
}

function configMatchesManifest(boot: TsjsBootV1): boolean {
  const selected = new Set(
    boot.manifest.integrations.flatMap(({ id }) => {
      const product = productForModule(id);
      return product ? [product] : [];
    })
  );
  const expected = INTEGRATION_CONFIG_IDS_V1.filter((id) => selected.has(id));
  return (
    expected.length === boot.integrations.entries.length &&
    expected.every((id, index) => boot.integrations.entries[index]?.id === id)
  );
}

function recursivelyFreeze(value: unknown): void {
  if (typeof value !== 'object' || value === null || Object.isFrozen(value)) return;
  for (const key of Reflect.ownKeys(value)) {
    const descriptor = Object.getOwnPropertyDescriptor(value, key);
    if (descriptor && 'value' in descriptor) recursivelyFreeze(descriptor.value);
  }
  Object.freeze(value);
}

function recursivelyFrozenPlainData(value: unknown, seen = new Set<object>()): boolean {
  if (typeof value !== 'object' || value === null) return true;
  if (seen.has(value) || !Object.isFrozen(value)) return false;
  seen.add(value);
  const isArray = Array.isArray(value);
  if (Object.getPrototypeOf(value) !== (isArray ? Array.prototype : Object.prototype)) return false;
  for (const key of Reflect.ownKeys(value)) {
    if (typeof key !== 'string') return false;
    const descriptor = Object.getOwnPropertyDescriptor(value, key);
    if (
      !descriptor ||
      ((!isArray || key !== 'length') && (!descriptor.enumerable || !('value' in descriptor)))
    ) {
      return false;
    }
    if ('value' in descriptor && !recursivelyFrozenPlainData(descriptor.value, seen)) return false;
  }
  return true;
}

/** Validate and copy the complete server-authored boot contract before browser effects. */
export function snapshotTsjsBootV1(candidate: unknown, releaseId: string): TsjsBootV1 | undefined {
  try {
    if (!RELEASE_ID.test(releaseId)) return undefined;
    const root = ownPlainDataRecord(candidate);
    if (
      !root ||
      !exactKeys(root, [
        'abi',
        'releaseId',
        'manifest',
        'auctionProjection',
        'integrations',
        'creative',
        'diagnostics',
      ]) ||
      root.abi !== 1 ||
      root.releaseId !== releaseId
    ) {
      return undefined;
    }
    const manifest = snapshotManifest(root.manifest, releaseId);
    const auctionProjection = parseBrowserAuctionProjectionV1(root.auctionProjection);
    const integrations = snapshotIntegrationConfigsV1(root.integrations);
    const creative = ownPlainDataRecord(root.creative);
    const diagnostics = ownPlainDataRecord(root.diagnostics);
    const gpt = ownPlainDataRecord(diagnostics?.gpt);
    if (
      !manifest ||
      !auctionProjection ||
      !integrations ||
      !creative ||
      !exactKeys(creative, ['version', 'enabled', 'clickGuard', 'renderGuard']) ||
      creative.version !== 1 ||
      typeof creative.enabled !== 'boolean' ||
      typeof creative.clickGuard !== 'boolean' ||
      typeof creative.renderGuard !== 'boolean' ||
      (!creative.enabled && (creative.clickGuard || creative.renderGuard)) ||
      !diagnostics ||
      !exactKeys(diagnostics, ['version', 'renderTraceOverlay', 'gpt']) ||
      diagnostics.version !== 1 ||
      typeof diagnostics.renderTraceOverlay !== 'boolean' ||
      !gpt ||
      !exactKeys(gpt, ['active']) ||
      typeof gpt.active !== 'boolean'
    ) {
      return undefined;
    }
    recursivelyFreeze(auctionProjection);
    const boot: TsjsBootV1 = Object.freeze({
      abi: 1,
      releaseId,
      manifest,
      auctionProjection,
      integrations,
      creative: Object.freeze({
        version: 1,
        enabled: creative.enabled,
        clickGuard: creative.clickGuard,
        renderGuard: creative.renderGuard,
      }),
      diagnostics: Object.freeze({
        version: 1,
        renderTraceOverlay: diagnostics.renderTraceOverlay,
        gpt: Object.freeze({ active: gpt.active }),
      }),
    });
    const has = (id: string): boolean => manifest.integrations.some((entry) => entry.id === id);
    if (
      !configMatchesManifest(boot) ||
      has('creative') !==
        (boot.creative.enabled && (boot.creative.clickGuard || boot.creative.renderGuard)) ||
      has('gpt_diagnostics') !== boot.diagnostics.gpt.active ||
      has('diagnostics_presentation') !==
        (boot.diagnostics.renderTraceOverlay || boot.diagnostics.gpt.active)
    ) {
      return undefined;
    }
    return boot;
  } catch {
    return undefined;
  }
}

/** Revalidate one recursively frozen boot and return the independent accepted snapshot. */
export function snapshotFrozenTsjsBootV1(
  candidate: unknown,
  releaseId: string
): TsjsBootV1 | undefined {
  try {
    const accepted = snapshotTsjsBootV1(candidate, releaseId);
    return accepted &&
      recursivelyFrozenPlainData(candidate) &&
      JSON.stringify(candidate) === JSON.stringify(accepted)
      ? accepted
      : undefined;
  } catch {
    return undefined;
  }
}

/** Revalidate a bootstrap snapshot while retaining its exact object identity. */
export function retainTsjsBootSnapshotV1(
  candidate: unknown,
  releaseId: string
): Readonly<TsjsBootV1> | undefined {
  return snapshotFrozenTsjsBootV1(candidate, releaseId)
    ? (candidate as Readonly<TsjsBootV1>)
    : undefined;
}

/** Capture the exact server lexical, including the one retained immutable boot copy. */
export function snapshotBootstrapInputV1(
  candidate: unknown,
  releaseId: string
): BootstrapInputSnapshotV1 | undefined {
  try {
    const root = ownPlainDataRecord(candidate);
    if (!root || !exactKeys(root, ['target', 'boot', 'outline'])) return undefined;
    if (
      (typeof root.target !== 'object' && typeof root.target !== 'function') ||
      root.target === null
    ) {
      return undefined;
    }
    const boot = snapshotTsjsBootV1(root.boot, releaseId);
    if (!boot) return undefined;
    let outline: TakeoverOutlineV1 | null = null;
    if (root.outline !== null) {
      const acceptedOutline = snapshotTakeoverOutlineV1(root.outline);
      if (!acceptedOutline) return undefined;
      outline = acceptedOutline;
    }
    const firstDisplay = boot.manifest.firstDisplay;
    if (
      (firstDisplay === null && outline !== null) ||
      (firstDisplay !== null &&
        (!outline ||
          outline.releaseId !== releaseId ||
          outline.integrationConfigDigest !==
            canonicalIntegrationConfigDigestV1(boot.integrations) ||
          outline.slotCount !== boot.auctionProjection.slots.length ||
          outline.outcomeCount !== boot.auctionProjection.auction.results.length ||
          outline.slices.length !== firstDisplay.slices.length ||
          outline.slices.some((id, index) => id !== firstDisplay.slices[index])))
    ) {
      return undefined;
    }
    return Object.freeze({
      target: root.target as BootstrapInputSnapshotV1['target'],
      boot,
      outline,
    });
  } catch {
    return undefined;
  }
}
