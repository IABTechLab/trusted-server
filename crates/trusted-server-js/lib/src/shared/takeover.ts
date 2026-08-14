import type { PreparedKernelTakeover } from '../kernel/integration_registry';

export type FirstDisplayGptDiagnosticEventV1 =
  | 'slotRequested'
  | 'slotResponseReceived'
  | 'slotRenderEnded'
  | 'slotOnload'
  | 'impressionViewable'
  | 'slotVisibilityChanged';

export type FirstDisplayGptDiagnosticDispositionV1 = 'matched' | 'unmatched' | 'ambiguous';

export type FirstDisplayGptDiagnosticIssueReasonV1 =
  'no_request_cycle' | 'overlapping_request_cycles' | 'unknown_prior_cycle' | 'invalid_event_order';

/** Exact ordinary-data GPT observation permitted to cross the first-display handoff. */
export interface FirstDisplayGptFactV1 {
  readonly version: 1;
  readonly event: FirstDisplayGptDiagnosticEventV1;
  readonly token: string;
  readonly runtimeSlotNumber: number;
  readonly cycleOrdinal: number | null;
  readonly disposition: FirstDisplayGptDiagnosticDispositionV1;
  readonly issueReason: FirstDisplayGptDiagnosticIssueReasonV1 | null;
  readonly capturedAtMs: number;
  readonly elementId: string | null;
  readonly adUnitPath: string | null;
  readonly isEmpty: boolean | null;
  readonly renderedSize: readonly [number, number] | null;
  readonly isBackfill: boolean | null;
  readonly slotContentChanged: boolean | null;
  readonly visibilityPercent: number | null;
}

export interface FirstDisplayGptDiagnosticsV1 {
  readonly facts: readonly Readonly<FirstDisplayGptFactV1>[];
  readonly overflowCount: number;
  readonly dropCount: number;
}

export interface PersistentFirstDisplayAdoptionV1 {
  readonly version: 1;
  readonly adoptInitialDisplay: true;
  readonly handoff: Readonly<Record<string, unknown>>;
  readonly identities: readonly object[];
}

export interface PersistentFirstDisplaySliceStateV1 {
  readonly sliceId: string;
  readonly observations: readonly string[];
  readonly values: readonly (readonly [string, string | number | boolean | null])[];
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

/** Select one exact parser-time slice snapshot from a validated takeover carrier. */
export function snapshotPersistentFirstDisplaySliceStateV1(
  candidate: unknown,
  sliceId: string
): Readonly<PersistentFirstDisplaySliceStateV1> | undefined {
  try {
    const adoption = snapshotPersistentFirstDisplayAdoptionV1(candidate);
    if (!adoption || typeof sliceId !== 'string' || sliceId.length === 0) return undefined;
    const slicesDescriptor = Object.getOwnPropertyDescriptor(adoption.handoff, 'slices');
    const parserDescriptor = Object.getOwnPropertyDescriptor(adoption.handoff, 'parserState');
    const slices = slicesDescriptor && 'value' in slicesDescriptor ? slicesDescriptor.value : null;
    const parserState =
      parserDescriptor && 'value' in parserDescriptor ? parserDescriptor.value : null;
    if (
      !Array.isArray(slices) ||
      !Object.isFrozen(slices) ||
      !slices.includes(sliceId) ||
      !Array.isArray(parserState) ||
      !Object.isFrozen(parserState)
    ) {
      return undefined;
    }
    const matches = parserState.filter((entry) => {
      const descriptor =
        typeof entry === 'object' && entry !== null
          ? Object.getOwnPropertyDescriptor(entry, 'sliceId')
          : undefined;
      return descriptor && 'value' in descriptor && descriptor.value === sliceId;
    });
    if (matches.length !== 1) return undefined;
    const row = matches[0];
    if (!Object.isFrozen(row)) return undefined;
    const fields = exactDataRecord(row, ['sliceId', 'observations', 'values']);
    if (!fields || !Array.isArray(fields.observations) || !Array.isArray(fields.values)) {
      return undefined;
    }
    if (
      !Object.isFrozen(fields.observations) ||
      !Object.isFrozen(fields.values) ||
      fields.observations.length > 256 ||
      fields.values.length !== fields.observations.length
    ) {
      return undefined;
    }
    const observations: string[] = [];
    const values: Array<readonly [string, string | number | boolean | null]> = [];
    for (let index = 0; index < fields.observations.length; index += 1) {
      const key = fields.observations[index];
      const pair = fields.values[index];
      if (
        typeof key !== 'string' ||
        key.length === 0 ||
        observations.includes(key) ||
        !Array.isArray(pair) ||
        !Object.isFrozen(pair) ||
        pair.length !== 2 ||
        pair[0] !== key
      ) {
        return undefined;
      }
      const value = pair[1];
      if (
        value !== null &&
        typeof value !== 'string' &&
        typeof value !== 'boolean' &&
        !(typeof value === 'number' && Number.isFinite(value))
      ) {
        return undefined;
      }
      observations.push(key);
      values.push(Object.freeze([key, value] as const));
    }
    return Object.freeze({
      sliceId,
      observations: Object.freeze(observations),
      values: Object.freeze(values),
    });
  } catch {
    return undefined;
  }
}

/** Report whether one validated takeover selected an exact parser-time slice. */
export function persistentFirstDisplaySliceSelectedV1(
  candidate: unknown,
  sliceId: string
): boolean | undefined {
  try {
    const adoption = snapshotPersistentFirstDisplayAdoptionV1(candidate);
    if (!adoption || typeof sliceId !== 'string' || sliceId.length === 0) return undefined;
    const descriptor = Object.getOwnPropertyDescriptor(adoption.handoff, 'slices');
    const slices = descriptor && 'value' in descriptor ? descriptor.value : undefined;
    if (
      !Array.isArray(slices) ||
      !Object.isFrozen(slices) ||
      slices.length === 0 ||
      slices[0] !== 'first_display' ||
      slices.some((value) => typeof value !== 'string') ||
      new Set(slices).size !== slices.length
    ) {
      return undefined;
    }
    return slices.includes(sliceId);
  } catch {
    return undefined;
  }
}

/** Validate one selected parser row while allowing an integration absent from the initial batch. */
export function validatePersistentFirstDisplaySliceAdoptionV1(
  candidate: unknown,
  sliceId: string,
  validate?: (state: Readonly<PersistentFirstDisplaySliceStateV1>) => boolean
): boolean {
  const selected = persistentFirstDisplaySliceSelectedV1(candidate, sliceId);
  if (selected === undefined) return false;
  if (!selected) return true;
  const state = snapshotPersistentFirstDisplaySliceStateV1(candidate, sliceId);
  if (!state) return false;
  try {
    return validate?.(state) ?? true;
  } catch {
    return false;
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
