import { parseCacheFetchPolicyV1 } from '../core/config';
import { parseBrowserAuctionProjectionV1 } from '../core/contracts/auction_projection';
import { log } from '../core/log';
import { prepareProgrammaticAdUnits } from '../core/registry';
import { validateRequestAdsOptions } from '../core/request';
import type { BootManifestV1 } from '../core/types';

export { AdUnitRegistrationError, type AdUnitRegistrationErrorCode } from '../core/registry';
export { RequestAdsInputError, type RequestAdsInputErrorCode } from '../core/request';

import type { BootFailureReason } from './integration_registry';

const SAFE_PROJECTION = {
  version: 1,
  auction: { version: 1, auctionId: 'fallback', results: [] },
  bids: [],
} as const;

export class TsjsUnavailableError extends Error {
  public readonly code = 'runtime_unavailable' as const;
  public readonly releaseId: string;
  public readonly reason: BootFailureReason;

  public constructor(releaseId: string, reason: BootFailureReason) {
    super('TSJS runtime is unavailable');
    this.name = 'TsjsUnavailableError';
    this.releaseId = releaseId;
    this.reason = reason;
  }
}

function ownDataRecord(value: unknown): Record<string, unknown> | undefined {
  try {
    if (typeof value !== 'object' || value === null || Array.isArray(value)) return undefined;
    const prototype = Object.getPrototypeOf(value) as unknown;
    if (prototype !== Object.prototype && prototype !== null) return undefined;
    const output: Record<string, unknown> = Object.create(null) as Record<string, unknown>;
    for (const key of Reflect.ownKeys(value)) {
      if (typeof key !== 'string') return undefined;
      const descriptor = Object.getOwnPropertyDescriptor(value, key);
      if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) return undefined;
      output[key] = descriptor.value;
    }
    return output;
  } catch {
    return undefined;
  }
}

function exactKeys(record: Record<string, unknown>, keys: readonly string[]): boolean {
  const actual = Object.keys(record);
  return actual.length === keys.length && actual.every((key) => keys.includes(key));
}

function snapshotOwnArray(value: unknown, maximum: number): readonly unknown[] | undefined {
  try {
    if (!Array.isArray(value) || value.length > maximum) {
      return undefined;
    }
    if (Object.getOwnPropertySymbols(value).length !== 0) return undefined;
    const names = Object.getOwnPropertyNames(value);
    if (names.length !== value.length + 1 || !names.includes('length')) return undefined;
    const copy: unknown[] = [];
    for (let index = 0; index < value.length; index += 1) {
      const descriptor = Object.getOwnPropertyDescriptor(value, String(index));
      if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) return undefined;
      copy.push(descriptor.value);
    }
    return copy;
  } catch {
    return undefined;
  }
}

function manifestMatches(candidate: unknown, expected: BootManifestV1): boolean {
  const record = ownDataRecord(candidate);
  if (!record || !exactKeys(record, ['version', 'releaseId', 'integrations'])) return false;
  if (record.version !== 1 || record.releaseId !== expected.releaseId) return false;
  const integrations = snapshotOwnArray(record.integrations, 16);
  if (!integrations || integrations.length !== expected.integrations.length) return false;
  return integrations.every((entry, index) => {
    const fields = ownDataRecord(entry);
    const accepted = expected.integrations[index];
    return Boolean(
      accepted &&
      fields &&
      exactKeys(fields, ['id', 'required']) &&
      fields.id === accepted.id &&
      fields.required === true
    );
  });
}

function deepFreeze<T>(value: T): Readonly<T> {
  if ((typeof value !== 'object' && typeof value !== 'function') || value === null) return value;
  for (const key of Reflect.ownKeys(value)) {
    const descriptor = Object.getOwnPropertyDescriptor(value, key);
    if (descriptor && 'value' in descriptor) deepFreeze(descriptor.value);
  }
  return Object.freeze(value);
}

function readBootField(boot: unknown, key: string): unknown {
  const record = ownDataRecord(boot);
  return record?.[key];
}

function parseCachePolicy(candidate: unknown): ReturnType<typeof parseCacheFetchPolicyV1> {
  try {
    return parseCacheFetchPolicyV1(candidate);
  } catch {
    return undefined;
  }
}

/** Validate and freeze the complete boot snapshot used by a committed kernel. */
export function buildKernelBoot(
  releaseId: string,
  manifest: BootManifestV1,
  candidate: unknown
): Readonly<object> | undefined {
  const record = ownDataRecord(candidate);
  if (!record) return undefined;
  const transportKeys = ['auctionProjection', 'creative', 'diagnostics'];
  if (Object.prototype.hasOwnProperty.call(record, 'cachePolicy'))
    transportKeys.push('cachePolicy');
  const completeKeys = ['abi', 'releaseId', 'manifest', ...transportKeys];
  if (!exactKeys(record, transportKeys) && !exactKeys(record, completeKeys)) return undefined;
  if (
    completeKeys.length === Object.keys(record).length &&
    (record.abi !== 1 ||
      record.releaseId !== releaseId ||
      !manifestMatches(record.manifest, manifest))
  ) {
    return undefined;
  }
  const cachePolicy =
    record.cachePolicy === undefined ? undefined : parseCachePolicy(record.cachePolicy);
  if (record.cachePolicy !== undefined && !cachePolicy) return undefined;
  const auctionProjection = parseBrowserAuctionProjectionV1(record.auctionProjection, cachePolicy);
  const creative = ownDataRecord(record.creative);
  const diagnostics = ownDataRecord(record.diagnostics);
  const gptDiagnostics = ownDataRecord(diagnostics?.gpt);
  if (
    !auctionProjection ||
    !creative ||
    !exactKeys(creative, ['version', 'enabled', 'clickGuard', 'renderGuard']) ||
    creative.version !== 1 ||
    typeof creative.enabled !== 'boolean' ||
    typeof creative.clickGuard !== 'boolean' ||
    typeof creative.renderGuard !== 'boolean' ||
    !diagnostics ||
    !exactKeys(diagnostics, ['version', 'renderTraceOverlay', 'gpt']) ||
    diagnostics.version !== 1 ||
    typeof diagnostics.renderTraceOverlay !== 'boolean' ||
    !gptDiagnostics ||
    !exactKeys(gptDiagnostics, ['active']) ||
    typeof gptDiagnostics.active !== 'boolean'
  ) {
    return undefined;
  }
  const diagnosticsModule = manifest.integrations.filter(({ id }) => id === 'gpt_diagnostics');
  if (
    (gptDiagnostics.active && diagnosticsModule.length !== 1) ||
    (!gptDiagnostics.active && diagnosticsModule.length !== 0)
  ) {
    return undefined;
  }
  return deepFreeze({
    abi: 1,
    releaseId,
    manifest,
    auctionProjection,
    ...(cachePolicy ? { cachePolicy } : {}),
    creative: {
      version: 1,
      enabled: creative.enabled,
      clickGuard: creative.clickGuard,
      renderGuard: creative.renderGuard,
    },
    diagnostics: {
      version: 1,
      renderTraceOverlay: diagnostics.renderTraceOverlay,
      gpt: { active: gptDiagnostics.active },
    },
  });
}

/** Build the immutable boot snapshot shared by every terminal fallback. */
export function buildFallbackBoot(releaseId: string, candidate: unknown): Readonly<object> {
  const cacheCandidate = readBootField(candidate, 'cachePolicy');
  const cachePolicy = cacheCandidate === undefined ? undefined : parseCachePolicy(cacheCandidate);
  const projection =
    parseBrowserAuctionProjectionV1(readBootField(candidate, 'auctionProjection'), cachePolicy) ??
    SAFE_PROJECTION;
  return deepFreeze({
    abi: 1,
    releaseId,
    manifest: { version: 1, releaseId, integrations: [] },
    auctionProjection: projection,
    ...(cachePolicy ? { cachePolicy } : {}),
    creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
    diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
  });
}

const LOG_LEVELS = Object.freeze({
  silent: true,
  error: true,
  warn: true,
  info: true,
  debug: true,
});

function observeLog(callback: () => void): void {
  try {
    callback();
  } catch {
    // The public logger is observation only.
  }
}

export const publicLog = Object.freeze({
  setLevel: (level: Parameters<typeof log.setLevel>[0]) => {
    if (!Object.prototype.hasOwnProperty.call(LOG_LEVELS, level)) {
      throw new TypeError('Invalid TSJS log level');
    }
    log.setLevel(level);
  },
  getLevel: () => log.getLevel(),
  error: (...values: readonly unknown[]) => observeLog(() => log.error(...values)),
  warn: (...values: readonly unknown[]) => observeLog(() => log.warn(...values)),
  info: (...values: readonly unknown[]) => observeLog(() => log.info(...values)),
  debug: (...values: readonly unknown[]) => observeLog(() => log.debug(...values)),
});

export interface FallbackFieldsOptions {
  readonly releaseId: string;
  readonly reason: BootFailureReason;
  readonly boot: unknown;
}

/** Construct the complete, non-rendering public shell without runtime services. */
export function createFallbackFields(
  options: FallbackFieldsOptions
): Readonly<Record<string, unknown>> {
  const boot = buildFallbackBoot(options.releaseId, options.boot);
  const projection = boot as {
    readonly auctionProjection: {
      readonly auction: { readonly results: readonly { slot: string }[] };
    };
  };
  const knownSlots = Object.freeze(
    projection.auctionProjection.auction.results.map(({ slot }) => slot)
  );
  const known = new Set(knownSlots);
  const fields: Record<string, unknown> = {};
  Object.defineProperties(fields, {
    version: { enumerable: true, value: '1.0.0' },
    releaseId: { enumerable: true, value: options.releaseId },
    boot: { enumerable: true, value: boot },
    log: { enumerable: true, value: publicLog },
    _registerIntegration: { enumerable: true, value: () => false },
    addAdUnits: {
      enumerable: true,
      value: (units: unknown) => {
        prepareProgrammaticAdUnits(units, known);
        throw new TsjsUnavailableError(options.releaseId, options.reason);
      },
    },
    requestAds: {
      enumerable: true,
      value: async (requestOptions?: unknown) => {
        const validated = validateRequestAdsOptions(requestOptions);
        const selected = validated.slots ?? knownSlots;
        return deepFreeze({
          slots: selected.map((slot) =>
            known.has(slot)
              ? validated.aborted
                ? { slot, path: 'primary', outcome: 'cancelled', reason: 'caller_aborted' }
                : { slot, path: 'primary', outcome: 'failed', reason: options.reason }
              : { slot, path: 'primary', outcome: 'failed', reason: 'slot_unresolved' }
          ),
        });
      },
    },
    _internal: {
      enumerable: false,
      value: Object.freeze({
        state: 'fallback',
        releaseId: options.releaseId,
        reason: options.reason,
      }),
    },
  });
  return Object.freeze(fields);
}
