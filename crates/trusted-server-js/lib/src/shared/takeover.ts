import type { PreparedKernelTakeover } from '../kernel/integration_registry';

export interface PersistentFirstDisplayAdoptionV1 {
  readonly version: 1;
  readonly adoptInitialDisplay: true;
  readonly handoff: Readonly<Record<string, unknown>>;
  readonly identities: readonly object[];
}

function exactDataRecord(
  candidate: unknown,
  expected: readonly string[]
): Readonly<Record<string, unknown>> | undefined {
  if (
    typeof candidate !== 'object' ||
    candidate === null ||
    Array.isArray(candidate) ||
    Object.getPrototypeOf(candidate) !== Object.prototype
  ) {
    return undefined;
  }
  const keys = Reflect.ownKeys(candidate);
  if (
    keys.length !== expected.length ||
    !keys.every((key) => typeof key === 'string' && expected.includes(key))
  ) {
    return undefined;
  }
  const fields: Record<string, unknown> = {};
  for (const key of expected) {
    const descriptor = Object.getOwnPropertyDescriptor(candidate, key);
    if (!descriptor?.enumerable || !('value' in descriptor)) return undefined;
    fields[key] = descriptor.value;
  }
  return fields;
}

function exactFrozenIdentities(candidate: unknown): candidate is readonly object[] {
  if (!Array.isArray(candidate) || !Object.isFrozen(candidate) || candidate.length > 512) {
    return false;
  }
  const expectedKeys = Array.from({ length: candidate.length }, (_, index) => String(index));
  expectedKeys.push('length');
  const actualKeys = Reflect.ownKeys(candidate);
  if (
    actualKeys.length !== expectedKeys.length ||
    !actualKeys.every((key) => typeof key === 'string' && expectedKeys.includes(key))
  ) {
    return false;
  }
  for (let index = 0; index < candidate.length; index += 1) {
    const descriptor = Object.getOwnPropertyDescriptor(candidate, String(index));
    const identity = descriptor && 'value' in descriptor ? descriptor.value : undefined;
    if (
      !descriptor?.enumerable ||
      (typeof identity !== 'object' && typeof identity !== 'function') ||
      identity === null
    ) {
      return false;
    }
  }
  return true;
}

/** Validate only the closure-private outer adoption carrier without cloning object identities. */
export function snapshotPersistentFirstDisplayAdoptionV1(
  candidate: unknown
): PersistentFirstDisplayAdoptionV1 | undefined {
  try {
    if (!Object.isFrozen(candidate)) return undefined;
    const fields = exactDataRecord(candidate, [
      'version',
      'adoptInitialDisplay',
      'handoff',
      'identities',
    ]);
    if (
      !fields ||
      fields.version !== 1 ||
      fields.adoptInitialDisplay !== true ||
      typeof fields.handoff !== 'object' ||
      fields.handoff === null ||
      Array.isArray(fields.handoff) ||
      Object.getPrototypeOf(fields.handoff) !== Object.prototype ||
      !Object.isFrozen(fields.handoff) ||
      !exactFrozenIdentities(fields.identities)
    ) {
      return undefined;
    }
    return candidate as PersistentFirstDisplayAdoptionV1;
  } catch {
    return undefined;
  }
}

export const FIRST_DISPLAY_TAKEOVER_FIELD = '_firstDisplayTakeover' as const;

export type FirstDisplayTakeoverCallback = (prepared: PreparedKernelTakeover) => void;

export type FirstDisplayTakeoverTransportResult =
  | Readonly<{ status: 'absent' }>
  | Readonly<{ status: 'invalid' }>
  | Readonly<{ status: 'accepted'; coordinate: FirstDisplayTakeoverCallback }>;

/** Install one non-enumerable, one-use handoff sink shared only by bootstrap and core IIFEs. */
export function installFirstDisplayTakeoverTransport(
  target: object,
  coordinate: FirstDisplayTakeoverCallback
): (() => void) | undefined {
  if (
    typeof coordinate !== 'function' ||
    Object.getOwnPropertyDescriptor(target, FIRST_DISPLAY_TAKEOVER_FIELD)
  ) {
    return undefined;
  }
  try {
    Object.defineProperty(target, FIRST_DISPLAY_TAKEOVER_FIELD, {
      configurable: true,
      enumerable: false,
      value: coordinate,
      writable: false,
    });
  } catch {
    return undefined;
  }
  let live = true;
  return (): void => {
    if (!live) return;
    live = false;
    try {
      const descriptor = Object.getOwnPropertyDescriptor(target, FIRST_DISPLAY_TAKEOVER_FIELD);
      if (descriptor && 'value' in descriptor && descriptor.value === coordinate) {
        Reflect.deleteProperty(target, FIRST_DISPLAY_TAKEOVER_FIELD);
      }
    } catch {
      // Generation invalidation makes a hostile replacement inert.
    }
  };
}

/** Consume the exact bootstrap-owned sink before persistent preparation starts. */
export function consumeFirstDisplayTakeoverTransport(
  target: object
): FirstDisplayTakeoverTransportResult {
  try {
    const descriptor = Object.getOwnPropertyDescriptor(target, FIRST_DISPLAY_TAKEOVER_FIELD);
    if (!descriptor) return Object.freeze({ status: 'absent' });
    if (
      !descriptor.configurable ||
      descriptor.enumerable ||
      descriptor.writable ||
      !('value' in descriptor) ||
      typeof descriptor.value !== 'function'
    ) {
      return Object.freeze({ status: 'invalid' });
    }
    if (!Reflect.deleteProperty(target, FIRST_DISPLAY_TAKEOVER_FIELD)) {
      return Object.freeze({ status: 'invalid' });
    }
    return Object.freeze({
      status: 'accepted',
      coordinate: descriptor.value as FirstDisplayTakeoverCallback,
    });
  } catch {
    return Object.freeze({ status: 'invalid' });
  }
}
