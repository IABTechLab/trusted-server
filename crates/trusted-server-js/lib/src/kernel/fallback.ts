import { parseBrowserAuctionProjectionV1 } from '../core/contracts/auction_projection';
import { validateRequestAdsOptions } from '../core/contracts/request_ads';
import { log } from '../core/log';
import { prepareProgrammaticAdUnits } from '../core/registry';
import type { BootManifestV1 } from '../core/types';

export { AdUnitRegistrationError, type AdUnitRegistrationErrorCode } from '../core/registry';
export { RequestAdsInputError, type RequestAdsInputErrorCode } from '../core/contracts/request_ads';

import { MAX_MANIFEST_MODULES } from './release_catalog';

const SAFE_PROJECTION = {
  version: 1,
  auction: { version: 1, auctionId: 'fallback', results: [] },
  slots: [],
  bids: [],
} as const;
const CRITICAL_SRC_PATTERN = /^\/static\/tsjs=tsjs-unified\.min\.js\?v=[0-9a-f]{64}$/;

export type BootFailureReason = 'abi_mismatch' | 'bundle_partial';

function exactHttpOrigin(candidate: unknown): string | undefined {
  if (typeof candidate !== 'string') return undefined;
  try {
    const parsed = new URL(candidate);
    return (parsed.protocol === 'http:' || parsed.protocol === 'https:') &&
      parsed.username === '' &&
      parsed.password === '' &&
      parsed.origin === candidate
      ? parsed.origin
      : undefined;
  } catch {
    return undefined;
  }
}

/** Resolve the document's authentication origin, including stamped opaque creatives. */
export function trustedCriticalOrigin(runtimeDocument: Document): string | undefined {
  try {
    const view = runtimeDocument.defaultView;
    const documentOrigin = exactHttpOrigin(view?.location.origin);
    if (documentOrigin) return documentOrigin;
    if (view?.location.origin !== 'null') return undefined;
    const stamp = Object.getOwnPropertyDescriptor(view, '__tsCreativeOrigin');
    if (!stamp || stamp.configurable || stamp.enumerable || !('value' in stamp) || stamp.writable) {
      return undefined;
    }
    return exactHttpOrigin(stamp.value);
  } catch {
    return undefined;
  }
}

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

function ownPlainDataRecord(value: unknown): Record<string, unknown> | undefined {
  const record = ownDataRecord(value);
  if (!record) return undefined;
  try {
    return Object.getPrototypeOf(value) === Object.prototype ? record : undefined;
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
  if (!record || !exactKeys(record, ['version', 'releaseId', 'criticalSrc', 'integrations'])) {
    return false;
  }
  if (
    record.version !== 1 ||
    record.releaseId !== expected.releaseId ||
    record.criticalSrc !== expected.criticalSrc
  ) {
    return false;
  }
  const integrations = snapshotOwnArray(record.integrations, MAX_MANIFEST_MODULES);
  if (!integrations || integrations.length !== expected.integrations.length) return false;
  return integrations.every((entry, index) => {
    const fields = ownDataRecord(entry);
    const accepted = expected.integrations[index];
    if (!accepted || !fields || fields.id !== accepted.id || fields.phase !== accepted.phase) {
      return false;
    }
    return accepted.phase === 'critical'
      ? exactKeys(fields, ['id', 'phase'])
      : exactKeys(fields, ['id', 'phase', 'trigger', 'src']) &&
          fields.trigger === accepted.trigger &&
          fields.src === accepted.src;
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

/** Capture the canonical source owned by one connected exact critical-artifact tag. */
export function captureTrustedCriticalSrc(
  runtimeDocument: Document,
  script: HTMLScriptElement
): string | undefined {
  try {
    const Script = runtimeDocument.defaultView?.HTMLScriptElement;
    const origin = trustedCriticalOrigin(runtimeDocument);
    if (
      !Script ||
      !origin ||
      !(script instanceof Script) ||
      script.ownerDocument !== runtimeDocument ||
      script.id !== 'trustedserver-js' ||
      !script.isConnected
    ) {
      return undefined;
    }
    const matches = runtimeDocument.querySelectorAll('script#trustedserver-js');
    if (matches.length !== 1 || matches[0] !== script) return undefined;
    const absolute = new URL(script.src);
    const criticalSrc = `${absolute.pathname}${absolute.search}`;
    if (
      absolute.origin !== origin ||
      absolute.hash !== '' ||
      !CRITICAL_SRC_PATTERN.test(criticalSrc) ||
      new URL(criticalSrc, origin).href !== absolute.href
    ) {
      return undefined;
    }
    return criticalSrc;
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
  const auctionProjection = parseBrowserAuctionProjectionV1(record.auctionProjection);
  const creative = ownPlainDataRecord(record.creative);
  const diagnostics = ownPlainDataRecord(record.diagnostics);
  const gptDiagnostics = ownPlainDataRecord(diagnostics?.gpt);
  if (
    !auctionProjection ||
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
    !gptDiagnostics ||
    !exactKeys(gptDiagnostics, ['active']) ||
    typeof gptDiagnostics.active !== 'boolean'
  ) {
    return undefined;
  }
  const diagnosticsModule = manifest.integrations.filter(({ id }) => id === 'gpt_diagnostics');
  const diagnosticsPresentationModule = manifest.integrations.filter(
    ({ id }) => id === 'diagnostics_presentation'
  );
  const creativeModule = manifest.integrations.filter(({ id }) => id === 'creative');
  const diagnosticsPresentationRequired = diagnostics.renderTraceOverlay || gptDiagnostics.active;
  if (
    (creative.enabled && creativeModule.length !== 1) ||
    (!creative.enabled && creativeModule.length !== 0) ||
    (gptDiagnostics.active && diagnosticsModule.length !== 1) ||
    (!gptDiagnostics.active && diagnosticsModule.length !== 0) ||
    (diagnosticsPresentationRequired && diagnosticsPresentationModule.length !== 1) ||
    (!diagnosticsPresentationRequired && diagnosticsPresentationModule.length !== 0)
  ) {
    return undefined;
  }
  return deepFreeze({
    abi: 1,
    releaseId,
    manifest,
    auctionProjection,
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
export function buildFallbackBoot(
  releaseId: string,
  candidate: unknown,
  trustedCriticalSrc: string
): Readonly<object> | undefined {
  if (!CRITICAL_SRC_PATTERN.test(trustedCriticalSrc)) {
    return undefined;
  }
  const projection =
    parseBrowserAuctionProjectionV1(readBootField(candidate, 'auctionProjection')) ??
    SAFE_PROJECTION;
  const manifest: BootManifestV1 = {
    version: 1,
    releaseId,
    criticalSrc: trustedCriticalSrc,
    integrations: [],
  };
  return deepFreeze({
    abi: 1,
    releaseId,
    manifest,
    auctionProjection: projection,
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
  readonly trustedCriticalSrc: string;
}

/** Construct the complete, non-rendering public shell without runtime services. */
export function createFallbackFields(
  options: FallbackFieldsOptions
): Readonly<Record<string, unknown>> | undefined {
  const boot = buildFallbackBoot(options.releaseId, options.boot, options.trustedCriticalSrc);
  if (!boot) return undefined;
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
