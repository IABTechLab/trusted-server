import type { FirstDisplaySliceActivationContext } from '../../shared/first_display_transaction';
import { createFirstDisplayApsRenderStrategy } from '../render_bridge';
import type {
  FirstDisplayRenderOwnerOptionsV1,
  FirstDisplayRenderStrategyV1,
} from '../render_journal';

export type FirstDisplayApsDocumentMessageV1 =
  | Readonly<{ kind: 'document_accepted' }>
  | Readonly<{ kind: 'runner_loaded' }>
  | Readonly<{ kind: 'render_completed' }>
  | Readonly<{
      kind: 'render_failed';
      reason: 'descriptor_invalid' | 'runner_no_load' | 'runner_failed';
    }>;

export type FirstDisplayApsWindowMessageV1 =
  | Readonly<{ kind: 'bootstrap_ready'; bootstrap: string }>
  | Readonly<{
      kind: 'container_ready';
      bootstrap: string;
      renderer: string;
    }>;

export interface FirstDisplayApsBootstrapPolicyV2 {
  readonly creativeOrigin: string;
  readonly tagType: 'iframe' | 'script';
}

export interface FirstDisplayApsProtocolV1 {
  readonly version: 1;
  readonly id: 'aps';
  readonly publisherOrigin: string;
  readonly rendererUrl: string;
  readonly sandbox: string;
  readonly permanentSandbox: string;
  readonly deadlines: Readonly<{
    documentAcceptanceMs: 3_000;
    completionMs: 10_000;
  }>;
  readonly isBootstrapNonce: (candidate: unknown) => candidate is string;
  readonly isRendererNonce: (candidate: unknown) => candidate is string;
  readonly bootstrapPolicy: (
    renderer: unknown
  ) => Readonly<FirstDisplayApsBootstrapPolicyV2> | undefined;
  readonly parseDocumentMessage: (
    candidate: unknown,
    expectedNonce: string
  ) => FirstDisplayApsDocumentMessageV1 | undefined;
  readonly parseWindowMessage: (candidate: unknown) => FirstDisplayApsWindowMessageV1 | undefined;
  readonly createRenderStrategy: (
    options: FirstDisplayRenderOwnerOptionsV1
  ) => FirstDisplayRenderStrategyV1;
}

interface ApsInitialBindings {
  readonly observe: (name: 'protocol_version', value: number) => void;
  readonly publisherOrigin: string;
  readonly register: (protocol: FirstDisplayApsProtocolV1) => () => void;
}

const OPAQUE_ID = /^[bn]1_[A-Za-z0-9_-]{22}$/;
const LOOPBACK_IPV4 = /^127(?:\.\d{1,3}){3}$/;
const FAILURE_REASONS = new Set(['descriptor_invalid', 'runner_no_load', 'runner_failed']);
const SANDBOX =
  'allow-forms allow-pointer-lock allow-popups allow-popups-to-escape-sandbox allow-scripts allow-top-navigation-by-user-activation';
const PERMANENT_SANDBOX =
  'allow-forms allow-pointer-lock allow-popups allow-popups-to-escape-sandbox allow-same-origin allow-scripts allow-top-navigation-by-user-activation';

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
    const publisherOrigin = fields?.['publisherOrigin'];
    if (
      !fields ||
      typeof fields.observe !== 'function' ||
      typeof publisherOrigin !== 'string' ||
      typeof fields.register !== 'function'
    ) {
      return undefined;
    }
    const origin = new URL(publisherOrigin);
    const loopbackHttp =
      origin.protocol === 'http:' &&
      (origin.hostname === 'localhost' ||
        origin.hostname === '[::1]' ||
        LOOPBACK_IPV4.test(origin.hostname));
    if (
      origin.origin !== publisherOrigin ||
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

function exactOpaqueId(candidate: unknown, prefix: 'b1_' | 'n1_'): candidate is string {
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

function parseWindowMessage(candidate: unknown): FirstDisplayApsWindowMessageV1 | undefined {
  if (typeof candidate !== 'string' || candidate.length > 4_096) return undefined;
  let parsed: unknown;
  try {
    parsed = JSON.parse(candidate) as unknown;
  } catch {
    return undefined;
  }
  const bootstrap = exactRecord(parsed, ['message', 'version', 'bootstrapNonce']);
  if (
    bootstrap?.message === 'TS APS Bootstrap Ready' &&
    bootstrap.version === 1 &&
    exactOpaqueId(bootstrap.bootstrapNonce, 'b1_') &&
    candidate ===
      JSON.stringify({
        message: 'TS APS Bootstrap Ready',
        version: 1,
        bootstrapNonce: bootstrap.bootstrapNonce,
      })
  ) {
    return Object.freeze({ kind: 'bootstrap_ready', bootstrap: bootstrap.bootstrapNonce });
  }
  const container = exactRecord(parsed, ['message', 'version', 'bootstrapNonce', 'rendererNonce']);
  if (
    container?.message === 'TS APS Container Ready' &&
    container.version === 1 &&
    exactOpaqueId(container.bootstrapNonce, 'b1_') &&
    exactOpaqueId(container.rendererNonce, 'n1_') &&
    candidate ===
      JSON.stringify({
        message: 'TS APS Container Ready',
        version: 1,
        bootstrapNonce: container.bootstrapNonce,
        rendererNonce: container.rendererNonce,
      })
  ) {
    return Object.freeze({
      kind: 'container_ready',
      bootstrap: container.bootstrapNonce,
      renderer: container.rendererNonce,
    });
  }
  return undefined;
}

function bootstrapPolicy(
  candidate: unknown,
  publisherOrigin: string
): Readonly<FirstDisplayApsBootstrapPolicyV2> | undefined {
  try {
    const renderer = exactRecord(
      candidate,
      Object.prototype.hasOwnProperty.call(candidate, 'creativeId')
        ? [
            'aaxResponse',
            'accountId',
            'bidId',
            'creativeId',
            'creativeUrl',
            'height',
            'tagType',
            'type',
            'version',
            'width',
          ]
        : [
            'aaxResponse',
            'accountId',
            'bidId',
            'creativeUrl',
            'height',
            'tagType',
            'type',
            'version',
            'width',
          ]
    );
    if (
      !renderer ||
      renderer.type !== 'aps' ||
      renderer.version !== 1 ||
      (renderer.tagType !== 'iframe' && renderer.tagType !== 'script') ||
      typeof renderer.creativeUrl !== 'string'
    ) {
      return undefined;
    }
    const creative = new URL(renderer.creativeUrl);
    if (
      creative.protocol !== 'https:' ||
      creative.hostname === '' ||
      creative.username !== '' ||
      creative.password !== '' ||
      creative.origin === publisherOrigin
    ) {
      return undefined;
    }
    return Object.freeze({ creativeOrigin: creative.origin, tagType: renderer.tagType });
  } catch {
    return undefined;
  }
}

/** Register the exact APS reservation identity and renderer-document protocol. */
export function installApsInitial(
  candidate: unknown,
  own: FirstDisplaySliceActivationContext['own']
): Readonly<{ version: 1; id: 'aps' }> {
  const value = bindings(candidate);
  if (!value || typeof own !== 'function') throw new TypeError('tsjs');
  const publisherOrigin = value['publisherOrigin'];
  const rendererUrl = new URL('/integrations/aps/renderer/v2', publisherOrigin).href;
  const protocol: FirstDisplayApsProtocolV1 = Object.freeze({
    version: 1,
    id: 'aps',
    publisherOrigin,
    rendererUrl,
    sandbox: SANDBOX,
    permanentSandbox: PERMANENT_SANDBOX,
    deadlines: Object.freeze({
      documentAcceptanceMs: 3_000,
      completionMs: 10_000,
    }),
    isBootstrapNonce: (input: unknown): input is string => exactOpaqueId(input, 'b1_'),
    isRendererNonce: (input: unknown): input is string => exactOpaqueId(input, 'n1_'),
    bootstrapPolicy: (renderer: unknown) => bootstrapPolicy(renderer, publisherOrigin),
    parseDocumentMessage,
    parseWindowMessage,
    createRenderStrategy: (options: FirstDisplayRenderOwnerOptionsV1) =>
      createFirstDisplayApsRenderStrategy(options, protocol),
  });
  const release = value.register(protocol);
  if (typeof release !== 'function') throw new TypeError('tsjs');
  own(release);
  value.observe('protocol_version', 1);
  return Object.freeze({ version: 1, id: 'aps' });
}
