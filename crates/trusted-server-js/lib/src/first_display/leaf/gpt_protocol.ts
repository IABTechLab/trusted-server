import type { FirstDisplaySliceActivationContext } from '../../shared/first_display_transaction';
import type {
  FirstDisplayGoogletagBatch,
  FirstDisplayGoogletagBatchCallbacks,
  FirstDisplayGoogletagBatchInput,
  FirstDisplayGptCaptureCycleV1,
  FirstDisplayGptDiagnosticsHandoffV1,
} from '../adapters/googletag';
import { enqueueFirstDisplayGamAttribution } from '../adapters/googletag';

export type { FirstDisplayGptCaptureCycleV1 } from '../adapters/googletag';

export interface FirstDisplayGptRequestPlanV1 {
  readonly operations: readonly ('display' | 'refresh')[];
  readonly requestOperation: 0 | 1;
}

export type FirstDisplayGptProtocolV1 = readonly [
  version: 1,
  id: 'gpt',
  createBatch: (input: FirstDisplayGoogletagBatchInput) => FirstDisplayGptCapabilityV1,
];

export type FirstDisplayGptDiagnosticsCaptureV1 = FirstDisplayGptDiagnosticsHandoffV1;

export type FirstDisplayGptCapabilityV1 = readonly [
  start: (callbacks: FirstDisplayGoogletagBatchCallbacks) => boolean,
  closeIngress: (committedSlotIds: readonly string[]) => boolean,
  captureHandoff: () => readonly FirstDisplayGptCaptureCycleV1[] | undefined,
  captureDiagnosticsHandoff: () => FirstDisplayGptDiagnosticsCaptureV1 | undefined,
  detachCommittedSlots: (slotIds: readonly string[]) => boolean,
  dispose: () => void,
];

export interface FirstDisplayGptBatchPolicyV1 {
  readonly deadlines: Readonly<{
    externalReadyMs: 10_000;
    requestStartMs: 3_000;
    completionMs: 10_000;
  }>;
  readonly requestPlan: (candidate: unknown) => FirstDisplayGptRequestPlanV1 | undefined;
  readonly classifyRenderEnded: (candidate: unknown) => 'gam_empty' | 'nonempty_gam' | undefined;
}

interface GptInitialBindings {
  readonly browser: Window & { googletag?: unknown };
  readonly observe: (name: 'gam' | 'v', value: boolean | number) => void;
  readonly register: (protocol: FirstDisplayGptProtocolV1) => () => void;
}

export type FirstDisplayGptBatchFactoryV1 = (
  input: FirstDisplayGoogletagBatchInput,
  policy: FirstDisplayGptBatchPolicyV1
) => FirstDisplayGoogletagBatch;

function exactRecord(value: unknown, keys: readonly string[]): Record<string, unknown> | undefined {
  try {
    if (
      typeof value !== 'object' ||
      value === null ||
      Array.isArray(value) ||
      Object.getPrototypeOf(value) !== Object.prototype
    ) {
      return undefined;
    }
    const ownKeys = Reflect.ownKeys(value);
    if (
      ownKeys.length !== keys.length ||
      !ownKeys.every((key) => typeof key === 'string' && keys.includes(key))
    ) {
      return undefined;
    }
    const result: Record<string, unknown> = {};
    for (const key of keys) {
      const descriptor = Object.getOwnPropertyDescriptor(value, key);
      if (!descriptor?.enumerable || !('value' in descriptor)) return undefined;
      result[key] = descriptor.value;
    }
    return result;
  } catch {
    return undefined;
  }
}

function requestPlan(candidate: unknown): FirstDisplayGptRequestPlanV1 | undefined {
  const fields = exactRecord(candidate, ['initialLoadDisabled', 'ownership']);
  if (
    !fields ||
    !Object.isFrozen(candidate) ||
    typeof fields.initialLoadDisabled !== 'boolean' ||
    (fields.ownership !== 'publisher' && fields.ownership !== 'trusted_server')
  ) {
    return undefined;
  }
  if (fields.ownership === 'publisher') {
    return Object.freeze({ operations: Object.freeze(['refresh'] as const), requestOperation: 0 });
  }
  if (fields.initialLoadDisabled) {
    return Object.freeze({
      operations: Object.freeze(['display', 'refresh'] as const),
      requestOperation: 1,
    });
  }
  return Object.freeze({ operations: Object.freeze(['display'] as const), requestOperation: 0 });
}

function classifyRenderEnded(candidate: unknown): 'gam_empty' | 'nonempty_gam' | undefined {
  const fields = exactRecord(candidate, ['isEmpty']);
  if (!fields || typeof fields.isEmpty !== 'boolean') return undefined;
  return fields.isEmpty ? 'gam_empty' : 'nonempty_gam';
}

/** Register the sole provisional GPT request planning and cycle-attribution policy. */
export function installGptInitial(
  candidate: unknown,
  own: FirstDisplaySliceActivationContext['own'],
  createBatch: FirstDisplayGptBatchFactoryV1,
  configCandidate: unknown
): readonly [version: 1, id: 'gpt'] {
  // The authenticated bootstrap is the sole caller and owns this capability object.
  const value = candidate as GptInitialBindings;
  const gamAttributionEnabled = (
    configCandidate as Readonly<{ gamAttributionEnabled?: unknown }> | undefined
  )?.gamAttributionEnabled;
  if (
    typeof own !== 'function' ||
    typeof createBatch !== 'function' ||
    typeof gamAttributionEnabled !== 'boolean'
  ) {
    throw new TypeError('tsjs');
  }
  const policy: FirstDisplayGptBatchPolicyV1 = Object.freeze({
    deadlines: Object.freeze({
      externalReadyMs: 10_000,
      requestStartMs: 3_000,
      completionMs: 10_000,
    }),
    requestPlan,
    classifyRenderEnded,
  });
  const protocol: FirstDisplayGptProtocolV1 = Object.freeze([
    1,
    'gpt',
    (input: FirstDisplayGoogletagBatchInput): FirstDisplayGptCapabilityV1 => {
      const batch = createBatch(input, policy);
      return Object.freeze([
        batch.start,
        batch.closeIngress,
        batch.captureHandoff,
        batch.captureDiagnosticsHandoff,
        batch.detachCommittedSlots,
        batch.dispose,
      ]);
    },
  ]);
  const release = value.register(protocol);
  if (typeof release !== 'function') throw new TypeError('tsjs');
  own(release);
  value.observe('gam', gamAttributionEnabled);
  value.observe('v', 1);
  if (gamAttributionEnabled && !enqueueFirstDisplayGamAttribution(value.browser)) {
    throw new TypeError('tsjs');
  }
  return Object.freeze([1, 'gpt']);
}
