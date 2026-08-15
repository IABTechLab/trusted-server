import { parseBrowserAuctionProjectionV1 } from '../core/contracts/auction_projection';
import { validateRequestAdsOptions } from '../core/contracts/request_ads';
import { prepareProgrammaticAdUnits } from '../core/registry';
import { EMBEDDED_MAX_MANIFEST_MODULES } from '../core/release_capacity';
import type { BootManifestV1 } from '../core/types';

import { publicLog, TsjsUnavailableError, type BootFailureReason } from './fallback_surface';

export { AdUnitRegistrationError, type AdUnitRegistrationErrorCode } from '../core/registry';
export { RequestAdsInputError, type RequestAdsInputErrorCode } from '../core/contracts/request_ads';
export { publicLog, TsjsUnavailableError, type BootFailureReason } from './fallback_surface';

const SAFE_PROJECTION = {
  version: 1,
  auction: { version: 1, auctionId: 'fallback', results: [] },
  slots: [],
  bids: [],
} as const;
const RUNTIME_SRC_PATTERN = /^\/static\/tsjs=tsjs-unified\.min\.js\?v=[0-9a-f]{64}$/;
const FIRST_DISPLAY_SRC_PATTERN =
  /^\/static\/tsjs=tsjs-first-display\.min\.js\?m=[0-9a-f]{4}&v=[0-9a-f]{64}$/;

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
export function trustedArtifactOrigin(runtimeDocument: Document): string | undefined {
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
  if (
    !record ||
    !exactKeys(record, ['version', 'releaseId', 'firstDisplay', 'runtimeSrc', 'integrations'])
  ) {
    return false;
  }
  if (
    record.version !== 1 ||
    record.releaseId !== expected.releaseId ||
    record.runtimeSrc !== expected.runtimeSrc
  ) {
    return false;
  }
  const firstDisplay = snapshotFirstDisplay(record.firstDisplay);
  if (
    (expected.firstDisplay === null && firstDisplay !== null) ||
    (expected.firstDisplay !== null &&
      (firstDisplay === null ||
        firstDisplay === undefined ||
        firstDisplay.src !== expected.firstDisplay.src ||
        firstDisplay.slices.length !== expected.firstDisplay.slices.length ||
        firstDisplay.slices.some((id, index) => id !== expected.firstDisplay?.slices[index])))
  ) {
    return false;
  }
  const integrations = snapshotOwnArray(record.integrations, EMBEDDED_MAX_MANIFEST_MODULES);
  if (!integrations || integrations.length !== expected.integrations.length) return false;
  return integrations.every((entry, index) => {
    const fields = ownDataRecord(entry);
    const accepted = expected.integrations[index];
    if (!accepted || !fields || fields.id !== accepted.id || fields.phase !== accepted.phase) {
      return false;
    }
    return accepted.phase === 'takeover'
      ? exactKeys(fields, ['id', 'phase'])
      : exactKeys(fields, ['id', 'phase', 'trigger', 'src']) &&
          fields.trigger === accepted.trigger &&
          fields.src === accepted.src;
  });
}

function snapshotFirstDisplay(candidate: unknown): BootManifestV1['firstDisplay'] | undefined {
  if (candidate === null) return null;
  const fields = ownDataRecord(candidate);
  if (!fields || !exactKeys(fields, ['src', 'slices'])) return undefined;
  const slices = snapshotOwnArray(fields.slices, 13);
  if (
    typeof fields.src !== 'string' ||
    !FIRST_DISPLAY_SRC_PATTERN.test(fields.src) ||
    !slices ||
    slices.length === 0 ||
    slices[0] !== 'first_display' ||
    slices.some((id) => typeof id !== 'string') ||
    new Set(slices).size !== slices.length
  ) {
    return undefined;
  }
  return Object.freeze({ src: fields.src, slices: Object.freeze([...slices] as string[]) });
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

function captureTrustedArtifactSrc(
  runtimeDocument: Document,
  script: HTMLScriptElement,
  expectedId: 'trustedserver-js' | 'trustedserver-js-runtime'
): string | undefined {
  try {
    const Script = runtimeDocument.defaultView?.HTMLScriptElement;
    const origin = trustedArtifactOrigin(runtimeDocument);
    if (
      !Script ||
      !origin ||
      !(script instanceof Script) ||
      script.ownerDocument !== runtimeDocument ||
      script.id !== expectedId ||
      !script.isConnected
    ) {
      return undefined;
    }
    const matches = runtimeDocument.querySelectorAll(`script#${expectedId}`);
    if (matches.length !== 1 || matches[0] !== script) return undefined;
    const absolute = new URL(script.src);
    const runtimeSrc = `${absolute.pathname}${absolute.search}`;
    if (
      absolute.origin !== origin ||
      absolute.hash !== '' ||
      !RUNTIME_SRC_PATTERN.test(runtimeSrc) ||
      new URL(runtimeSrc, origin).href !== absolute.href
    ) {
      return undefined;
    }
    return runtimeSrc;
  } catch {
    return undefined;
  }
}

/** Capture the canonical source owned by one parser-inserted persistent artifact tag. */
export function captureTrustedSelectedRuntimeSrc(
  runtimeDocument: Document,
  script: HTMLScriptElement
): string | undefined {
  return captureTrustedArtifactSrc(runtimeDocument, script, 'trustedserver-js');
}

/** Capture the canonical source owned by the one post-paint runtime artifact tag. */
export function captureTrustedRuntimeSrc(
  runtimeDocument: Document,
  script: HTMLScriptElement
): string | undefined {
  return captureTrustedArtifactSrc(runtimeDocument, script, 'trustedserver-js-runtime');
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
  trustedRuntimeSrc: string
): Readonly<object> | undefined {
  if (!RUNTIME_SRC_PATTERN.test(trustedRuntimeSrc)) {
    return undefined;
  }
  const projection =
    parseBrowserAuctionProjectionV1(readBootField(candidate, 'auctionProjection')) ??
    SAFE_PROJECTION;
  const manifest: BootManifestV1 = {
    version: 1,
    releaseId,
    firstDisplay:
      snapshotFirstDisplay(readBootField(readBootField(candidate, 'manifest'), 'firstDisplay')) ??
      null,
    runtimeSrc: trustedRuntimeSrc,
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

export interface FallbackFieldsOptions {
  readonly releaseId: string;
  readonly reason: BootFailureReason;
  readonly boot: unknown;
  readonly trustedRuntimeSrc: string;
}

/** Construct the complete, non-rendering public shell without runtime services. */
export function createFallbackFields(
  options: FallbackFieldsOptions
): Readonly<Record<string, unknown>> | undefined {
  const boot = buildFallbackBoot(options.releaseId, options.boot, options.trustedRuntimeSrc);
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
