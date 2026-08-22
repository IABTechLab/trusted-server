import type { TakeoverOutlineV1 } from '../../shared/first_display_contracts';
import type { BootManifestV1, TsjsBootV1 } from '../types';

const MAX_TRANSPORT_BYTES = 10 * 1024 * 1024;
const MAX_TAKEOVER_INTEGRATIONS = 14;
const RELEASE_ID = /^[0-9a-f]{64}$/;
const HASH = /^[0-9a-f]{64}$/;
const INTEGRATION_ID = /^[a-z0-9][a-z0-9_-]{0,63}$/;
const FIRST_DISPLAY_SRC =
  /^\/static\/tsjs=tsjs-first-display\.min\.js\?m=([0-9a-f]{4})&v=[0-9a-f]{64}$/;
const RUNTIME_SRC = /^\/static\/tsjs=tsjs-unified\.min\.js\?v=[0-9a-f]{64}$/;
const DEFERRED_SRC = /^\/static\/tsjs=tsjs-([a-z0-9][a-z0-9_-]{0,63})\.min\.js\?v=[0-9a-f]{64}$/;
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
const CONFIG_IDS = Object.freeze([
  'aps',
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
] as const);

export interface ServerBootIntegrityV1 {
  readonly version: 1;
  readonly projectionDigest: string;
  readonly integrationConfigDigest: string;
}

export interface ServerBootTransportSnapshotV1 {
  readonly version: 1;
  readonly boot: Readonly<TsjsBootV1>;
  readonly integrity: Readonly<ServerBootIntegrityV1>;
  readonly outline: TakeoverOutlineV1 | null;
}

function record(value: unknown): Record<string, unknown> | undefined {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : undefined;
}

function exact(value: unknown, keys: readonly string[]): Record<string, unknown> | undefined {
  const candidate = record(value);
  if (!candidate || Object.getPrototypeOf(candidate) !== Object.prototype) return undefined;
  const actual = Object.keys(candidate);
  return actual.length === keys.length && actual.every((key) => keys.includes(key))
    ? candidate
    : undefined;
}

function deepFreeze(value: unknown): void {
  if (typeof value !== 'object' || value === null || Object.isFrozen(value)) return;
  for (const key of Object.keys(value)) deepFreeze((value as Record<string, unknown>)[key]);
  Object.freeze(value);
}

function validManifest(candidate: unknown, releaseId: string): BootManifestV1 | undefined {
  const manifest = exact(candidate, [
    'version',
    'releaseId',
    'firstDisplay',
    'runtimeSrc',
    'integrations',
  ]);
  if (
    !manifest ||
    manifest.version !== 1 ||
    manifest.releaseId !== releaseId ||
    typeof manifest.runtimeSrc !== 'string' ||
    !RUNTIME_SRC.test(manifest.runtimeSrc) ||
    !Array.isArray(manifest.integrations) ||
    manifest.integrations.length > 20
  ) {
    return undefined;
  }
  const seen = new Set<string>();
  let deferred = false;
  let takeoverCount = 0;
  for (const value of manifest.integrations) {
    const integration = record(value);
    if (
      !integration ||
      typeof integration.id !== 'string' ||
      !INTEGRATION_ID.test(integration.id) ||
      seen.has(integration.id)
    ) {
      return undefined;
    }
    seen.add(integration.id);
    if (integration.phase === 'takeover') {
      takeoverCount += 1;
      if (
        deferred ||
        takeoverCount > MAX_TAKEOVER_INTEGRATIONS ||
        !exact(integration, ['id', 'phase'])
      ) {
        return undefined;
      }
      continue;
    }
    const deferredSource =
      typeof integration.src === 'string' ? DEFERRED_SRC.exec(integration.src) : null;
    if (
      integration.phase !== 'deferred' ||
      !exact(integration, ['id', 'phase', 'trigger', 'src']) ||
      integration.trigger !== 'first_display_or_idle' ||
      deferredSource?.[1] !== integration.id
    ) {
      return undefined;
    }
    deferred = true;
  }
  if (manifest.firstDisplay === null) return manifest as unknown as BootManifestV1;
  const firstDisplay = exact(manifest.firstDisplay, ['src', 'slices']);
  const slices = firstDisplay?.slices;
  if (
    !firstDisplay ||
    typeof firstDisplay.src !== 'string' ||
    !Array.isArray(slices) ||
    slices.length === 0 ||
    slices.length > FIRST_DISPLAY_IDS.length
  ) {
    return undefined;
  }
  const match = FIRST_DISPLAY_SRC.exec(firstDisplay.src);
  if (!match) return undefined;
  const mask = Number.parseInt(match[1]!, 16);
  const selected = FIRST_DISPLAY_IDS.filter((_id, index) => (mask & (1 << index)) !== 0);
  if (
    (mask & 1) === 0 ||
    mask >>> FIRST_DISPLAY_IDS.length !== 0 ||
    ((mask & 4) !== 0 && (mask & 2) === 0) ||
    ((mask & 2) !== 0 && (mask & 128) === 0) ||
    selected.length !== slices.length ||
    selected.some((id, index) => slices[index] !== id)
  ) {
    return undefined;
  }
  return manifest as unknown as BootManifestV1;
}

function validBoot(candidate: unknown, releaseId: string): Readonly<TsjsBootV1> | undefined {
  const boot = exact(candidate, [
    'abi',
    'releaseId',
    'manifest',
    'auctionProjection',
    'integrations',
    'creative',
    'diagnostics',
  ]);
  if (!boot || boot.abi !== 1 || boot.releaseId !== releaseId) return undefined;
  if (!validManifest(boot.manifest, releaseId)) return undefined;

  const projection = exact(boot.auctionProjection, ['version', 'auction', 'slots', 'bids']);
  const auction = exact(projection?.auction, ['version', 'auctionId', 'results']);
  if (
    !projection ||
    projection.version !== 1 ||
    !auction ||
    auction.version !== 1 ||
    typeof auction.auctionId !== 'string' ||
    !Array.isArray(auction.results) ||
    auction.results.length > 256 ||
    !Array.isArray(projection.slots) ||
    projection.slots.length > 256 ||
    !Array.isArray(projection.bids)
  ) {
    return undefined;
  }

  const integrations = exact(boot.integrations, ['version', 'entries']);
  if (
    !integrations ||
    integrations.version !== 1 ||
    !Array.isArray(integrations.entries) ||
    integrations.entries.length > CONFIG_IDS.length
  ) {
    return undefined;
  }
  let previous = -1;
  for (const value of integrations.entries) {
    const entry = exact(value, ['id', 'config']);
    const order =
      entry && typeof entry.id === 'string'
        ? CONFIG_IDS.indexOf(entry.id as (typeof CONFIG_IDS)[number])
        : -1;
    if (!entry || order <= previous || !record(entry.config)) return undefined;
    previous = order;
  }

  const creative = exact(boot.creative, ['version', 'enabled', 'clickGuard', 'renderGuard']);
  const diagnostics = exact(boot.diagnostics, ['version', 'renderTraceOverlay', 'gpt']);
  const gpt = exact(diagnostics?.gpt, ['active']);
  if (
    !creative ||
    creative.version !== 1 ||
    typeof creative.enabled !== 'boolean' ||
    typeof creative.clickGuard !== 'boolean' ||
    typeof creative.renderGuard !== 'boolean' ||
    (!creative.enabled && (creative.clickGuard || creative.renderGuard)) ||
    !diagnostics ||
    diagnostics.version !== 1 ||
    typeof diagnostics.renderTraceOverlay !== 'boolean' ||
    !gpt ||
    typeof gpt.active !== 'boolean'
  ) {
    return undefined;
  }
  return boot as unknown as Readonly<TsjsBootV1>;
}

function validIntegrity(candidate: unknown): Readonly<ServerBootIntegrityV1> | undefined {
  const integrity = exact(candidate, ['version', 'projectionDigest', 'integrationConfigDigest']);
  return integrity &&
    integrity.version === 1 &&
    typeof integrity.projectionDigest === 'string' &&
    HASH.test(integrity.projectionDigest) &&
    typeof integrity.integrationConfigDigest === 'string' &&
    HASH.test(integrity.integrationConfigDigest)
    ? (integrity as unknown as Readonly<ServerBootIntegrityV1>)
    : undefined;
}

function validOutline(
  candidate: unknown,
  releaseId: string,
  integrity: Readonly<ServerBootIntegrityV1>,
  boot: Readonly<TsjsBootV1>
): TakeoverOutlineV1 | null | undefined {
  if (candidate === null) return boot.manifest.firstDisplay === null ? null : undefined;
  const outline = exact(candidate, [
    'version',
    'releaseId',
    'generation',
    'projectionDigest',
    'integrationConfigDigest',
    'slices',
    'slotCount',
    'outcomeCount',
    'capabilities',
    'objectKinds',
  ]);
  const firstDisplay = boot.manifest.firstDisplay;
  const projection = boot.auctionProjection;
  if (
    !outline ||
    !firstDisplay ||
    outline.version !== 1 ||
    outline.releaseId !== releaseId ||
    !Number.isInteger(outline.generation) ||
    (outline.generation as number) < 1 ||
    (outline.generation as number) > 4_294_967_295 ||
    outline.projectionDigest !== integrity.projectionDigest ||
    outline.integrationConfigDigest !== integrity.integrationConfigDigest ||
    !Array.isArray(outline.slices) ||
    outline.slices.length !== firstDisplay.slices.length ||
    outline.slices.some((id, index) => id !== firstDisplay.slices[index]) ||
    !Number.isInteger(outline.slotCount) ||
    outline.slotCount !== projection.slots.length ||
    (outline.slotCount as number) < 1 ||
    !Number.isInteger(outline.outcomeCount) ||
    outline.outcomeCount !== projection.auction.results.length ||
    outline.outcomeCount !== outline.slotCount ||
    !Array.isArray(outline.capabilities) ||
    outline.capabilities.length !== 0 ||
    !Array.isArray(outline.objectKinds)
  ) {
    return undefined;
  }
  const expectedKinds = projection.bids.length === 0 ? [] : ['gpt_slot', 'dom_artifact'];
  if (
    outline.objectKinds.length !== expectedKinds.length ||
    outline.objectKinds.some((kind, index) => kind !== expectedKinds[index])
  ) {
    return undefined;
  }
  return outline as unknown as TakeoverOutlineV1;
}

/** Parse the one server-sealed lexical boot value without importing domain validators. */
export function snapshotServerBootTransportV1(
  payload: unknown,
  releaseId: string
): ServerBootTransportSnapshotV1 | undefined {
  try {
    if (
      typeof payload !== 'string' ||
      typeof releaseId !== 'string' ||
      !RELEASE_ID.test(releaseId) ||
      payload.length > MAX_TRANSPORT_BYTES ||
      new TextEncoder().encode(payload).byteLength > MAX_TRANSPORT_BYTES
    ) {
      return undefined;
    }
    const root = exact(JSON.parse(payload), ['version', 'boot', 'integrity', 'outline']);
    if (!root || root.version !== 1) return undefined;
    const boot = validBoot(root.boot, releaseId);
    const integrity = validIntegrity(root.integrity);
    if (!boot || !integrity) return undefined;
    const outline = validOutline(root.outline, releaseId, integrity, boot);
    if (outline === undefined) return undefined;
    deepFreeze(root);
    return root as unknown as ServerBootTransportSnapshotV1;
  } catch {
    return undefined;
  }
}
