import type { FirstDisplaySliceActivationContext } from '../transaction';

export type FirstDisplayApsDocumentMessageV1 =
  | Readonly<{ kind: 'document_accepted' }>
  | Readonly<{ kind: 'runner_loaded' }>
  | Readonly<{ kind: 'render_completed' }>
  | Readonly<{
      kind: 'render_failed';
      reason: 'descriptor_invalid' | 'runner_no_load' | 'runner_failed';
    }>;

export interface FirstDisplayApsProtocolV1 {
  readonly version: 1;
  readonly id: 'aps';
  readonly rendererUrl: string;
  readonly sandbox: string;
  readonly deadlines: Readonly<{
    insertionMs: 1_000;
    documentAcceptanceMs: 3_000;
    completionMs: 10_000;
    ownerSettlementMs: 20_000;
  }>;
  readonly isReservationId: (candidate: unknown) => candidate is string;
  readonly isLifecycleTicket: (candidate: unknown) => candidate is string;
  readonly isRendererNonce: (candidate: unknown) => candidate is string;
  readonly parseDocumentMessage: (
    candidate: unknown,
    expectedNonce: string
  ) => FirstDisplayApsDocumentMessageV1 | undefined;
}

interface ApsInitialBindings {
  readonly observe: (name: 'protocol_version', value: number) => void;
  readonly publisherOrigin: string;
  readonly register: (protocol: FirstDisplayApsProtocolV1) => () => void;
}

const OPAQUE_ID = /^[artn]1_[A-Za-z0-9_-]{22}$/;
const LOOPBACK_IPV4 = /^127(?:\.\d{1,3}){3}$/;
const FAILURE_REASONS = new Set(['descriptor_invalid', 'runner_no_load', 'runner_failed']);
const SANDBOX =
  'allow-forms allow-pointer-lock allow-popups allow-popups-to-escape-sandbox allow-scripts allow-top-navigation-by-user-activation';

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

function bindings(candidate: unknown): ApsInitialBindings | undefined {
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
    const fields = exactRecord(candidate, ['observe', 'publisherOrigin', 'register']);
    if (
      !fields ||
      typeof fields.observe !== 'function' ||
      typeof fields.publisherOrigin !== 'string' ||
      typeof fields.register !== 'function'
    ) {
      return undefined;
    }
    const origin = new URL(fields.publisherOrigin);
    const loopbackHttp =
      origin.protocol === 'http:' &&
      (origin.hostname === 'localhost' ||
        origin.hostname === '[::1]' ||
        LOOPBACK_IPV4.test(origin.hostname));
    if (
      origin.origin !== fields.publisherOrigin ||
      (origin.protocol !== 'https:' && !loopbackHttp) ||
      origin.username !== '' ||
      origin.password !== ''
    ) {
      return undefined;
    }
    return fields as unknown as ApsInitialBindings;
  } catch {
    return undefined;
  }
}

function exactOpaqueId(candidate: unknown, prefix: 'r1_' | 't1_' | 'n1_'): candidate is string {
  return typeof candidate === 'string' && candidate.startsWith(prefix) && OPAQUE_ID.test(candidate);
}

function parseDocumentMessage(
  candidate: unknown,
  expectedNonce: string
): FirstDisplayApsDocumentMessageV1 | undefined {
  if (!exactOpaqueId(expectedNonce, 'n1_')) return undefined;
  const base = exactRecord(candidate, ['message', 'version', 'nonce']);
  if (base && base.version === 1 && base.nonce === expectedNonce) {
    if (base.message === 'TS APS Document Accepted') {
      return Object.freeze({ kind: 'document_accepted' });
    }
    if (base.message === 'TS APS Runner Loaded') {
      return Object.freeze({ kind: 'runner_loaded' });
    }
    if (base.message === 'TS APS Render Completed') {
      return Object.freeze({ kind: 'render_completed' });
    }
  }
  const failed = exactRecord(candidate, ['message', 'version', 'nonce', 'reason']);
  if (
    failed?.message === 'TS APS Render Failed' &&
    failed.version === 1 &&
    failed.nonce === expectedNonce &&
    typeof failed.reason === 'string' &&
    FAILURE_REASONS.has(failed.reason)
  ) {
    return Object.freeze({
      kind: 'render_failed',
      reason: failed.reason as 'descriptor_invalid' | 'runner_no_load' | 'runner_failed',
    });
  }
  return undefined;
}

/** Register the exact APS reservation identity and renderer-document protocol. */
export function installApsInitial(
  candidate: unknown,
  own: FirstDisplaySliceActivationContext['own']
): void {
  const value = bindings(candidate);
  if (!value || typeof own !== 'function') throw new TypeError('invalid APS initial bindings');
  const rendererUrl = new URL('/integrations/aps/renderer/v1', value.publisherOrigin).href;
  const protocol: FirstDisplayApsProtocolV1 = Object.freeze({
    version: 1,
    id: 'aps',
    rendererUrl,
    sandbox: SANDBOX,
    deadlines: Object.freeze({
      insertionMs: 1_000,
      documentAcceptanceMs: 3_000,
      completionMs: 10_000,
      ownerSettlementMs: 20_000,
    }),
    isReservationId: (input: unknown): input is string => exactOpaqueId(input, 'r1_'),
    isLifecycleTicket: (input: unknown): input is string => exactOpaqueId(input, 't1_'),
    isRendererNonce: (input: unknown): input is string => exactOpaqueId(input, 'n1_'),
    parseDocumentMessage,
  });
  const release = value.register(protocol);
  if (typeof release !== 'function') throw new TypeError('invalid APS protocol disposer');
  own(release);
  value.observe('protocol_version', 1);
}
