import { validateRequestAdsOptions } from '../core/contracts/request_ads';
import { ownDataObject } from '../core/contracts/auction_projection';
import { prepareProgrammaticAdUnits } from '../core/registry';
import type { BootManifestV1, TsjsBootV1 } from '../core/types';

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

function deepFreeze<T>(value: T): Readonly<T> {
  if ((typeof value !== 'object' && typeof value !== 'function') || value === null) return value;
  for (const key of Reflect.ownKeys(value)) {
    const descriptor = Object.getOwnPropertyDescriptor(value, key);
    if (descriptor && 'value' in descriptor) deepFreeze(descriptor.value);
  }
  return Object.freeze(value);
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
): Readonly<TsjsBootV1> | undefined {
  try {
    const record = ownDataObject(candidate, [
      'abi',
      'releaseId',
      'manifest',
      'auctionProjection',
      'integrations',
      'creative',
      'diagnostics',
    ]);
    if (
      !record ||
      Object.getPrototypeOf(candidate) !== Object.prototype ||
      !Object.isFrozen(candidate) ||
      record.abi !== 1 ||
      record.releaseId !== releaseId ||
      record.manifest !== manifest ||
      !Object.isFrozen(manifest) ||
      !Object.isFrozen(record.auctionProjection) ||
      !Object.isFrozen(record.integrations) ||
      !Object.isFrozen(record.creative) ||
      !Object.isFrozen(record.diagnostics)
    ) {
      return undefined;
    }
    return candidate as Readonly<TsjsBootV1>;
  } catch {
    return undefined;
  }
}

/** Build the immutable boot snapshot shared by every terminal fallback. */
export function buildFallbackBoot(
  releaseId: string,
  trustedRuntimeSrc: string
): Readonly<object> | undefined {
  if (!RUNTIME_SRC_PATTERN.test(trustedRuntimeSrc)) {
    return undefined;
  }
  const manifest: BootManifestV1 = {
    version: 1,
    releaseId,
    firstDisplay: null,
    runtimeSrc: trustedRuntimeSrc,
    integrations: [],
  };
  return deepFreeze({
    abi: 1,
    releaseId,
    manifest,
    auctionProjection: SAFE_PROJECTION,
    integrations: { version: 1, entries: [] },
    creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
    diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
  });
}

export interface FallbackFieldsOptions {
  readonly releaseId: string;
  readonly reason: BootFailureReason;
  readonly trustedRuntimeSrc: string;
}

/** Construct the complete, non-rendering public shell without runtime services. */
export function createFallbackFields(
  options: FallbackFieldsOptions
): Readonly<Record<string, unknown>> | undefined {
  const boot = buildFallbackBoot(options.releaseId, options.trustedRuntimeSrc);
  if (!boot) return undefined;
  const knownSlots = Object.freeze([] as string[]);
  const known = new Set<string>();
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
            validated.aborted
              ? { slot, path: 'primary', outcome: 'cancelled', reason: 'caller_aborted' }
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
        initialDisplayCommitted: false,
      }),
    },
  });
  return Object.freeze(fields);
}
