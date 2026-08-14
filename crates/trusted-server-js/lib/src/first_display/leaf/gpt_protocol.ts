import type { FirstDisplaySliceActivationContext } from '../transaction';
import type {
  FirstDisplayGoogletagBatch,
  FirstDisplayGoogletagBatchInput,
} from '../adapters/googletag';

export interface FirstDisplayGptRequestPlanV1 {
  readonly operations: readonly ('display' | 'refresh')[];
  readonly requestOperation: 0 | 1;
}

export interface FirstDisplayGptProtocolV1 {
  readonly version: 1;
  readonly id: 'gpt';
  readonly deadlines: Readonly<{
    externalReadyMs: 10_000;
    requestStartMs: 3_000;
    completionMs: 10_000;
  }>;
  readonly createBatch: (input: FirstDisplayGoogletagBatchInput) => FirstDisplayGoogletagBatch;
  readonly requestPlan: (candidate: unknown) => FirstDisplayGptRequestPlanV1 | undefined;
  readonly validTargetingValue: (candidate: unknown) => candidate is string;
  readonly classifyRenderEnded: (candidate: unknown) => 'gam_empty' | 'nonempty_gam' | undefined;
}

interface GptInitialBindings {
  readonly observe: (name: 'protocol_version', value: number) => void;
  readonly register: (protocol: FirstDisplayGptProtocolV1) => () => void;
}

export type FirstDisplayGptBatchFactoryV1 = (
  input: FirstDisplayGoogletagBatchInput,
  protocol: FirstDisplayGptProtocolV1
) => FirstDisplayGoogletagBatch;

const textEncoder = new TextEncoder();

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

function bindings(candidate: unknown): GptInitialBindings | undefined {
  try {
    if (
      typeof candidate !== 'object' ||
      candidate === null ||
      Array.isArray(candidate) ||
      Object.getPrototypeOf(candidate) !== Object.prototype ||
      !Object.isFrozen(candidate)
    ) {
      return undefined;
    }
    const fields = exactRecord(candidate, ['observe', 'register']);
    return fields && typeof fields.observe === 'function' && typeof fields.register === 'function'
      ? (fields as unknown as GptInitialBindings)
      : undefined;
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

function validTargetingValue(candidate: unknown): candidate is string {
  if (typeof candidate !== 'string' || candidate.length === 0) return false;
  let scalars = 0;
  for (let index = 0; index < candidate.length; index += 1) {
    const code = candidate.charCodeAt(index);
    if (code <= 0x1f || code === 0x7f) return false;
    if (code >= 0xd800 && code <= 0xdbff) {
      const next = candidate.charCodeAt(index + 1);
      if (!(next >= 0xdc00 && next <= 0xdfff)) return false;
      index += 1;
    } else if (code >= 0xdc00 && code <= 0xdfff) {
      return false;
    }
    scalars += 1;
    if (scalars > 40) return false;
  }
  return textEncoder.encode(candidate).byteLength <= 160;
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
  createBatch: FirstDisplayGptBatchFactoryV1
): Readonly<{ version: 1; id: 'gpt' }> {
  const value = bindings(candidate);
  if (!value || typeof own !== 'function' || typeof createBatch !== 'function') {
    throw new TypeError('invalid GPT initial bindings');
  }
  const protocol: FirstDisplayGptProtocolV1 = Object.freeze({
    version: 1,
    id: 'gpt',
    deadlines: Object.freeze({
      externalReadyMs: 10_000,
      requestStartMs: 3_000,
      completionMs: 10_000,
    }),
    createBatch: (input: FirstDisplayGoogletagBatchInput): FirstDisplayGoogletagBatch =>
      createBatch(input, protocol),
    requestPlan,
    validTargetingValue,
    classifyRenderEnded,
  });
  const release = value.register(protocol);
  if (typeof release !== 'function') throw new TypeError('invalid GPT protocol disposer');
  own(release);
  value.observe('protocol_version', 1);
  return Object.freeze({ version: 1, id: 'gpt' });
}
