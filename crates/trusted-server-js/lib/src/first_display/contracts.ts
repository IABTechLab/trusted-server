import type { FirstDisplaySliceId } from '../kernel/release_catalog';
import type { FirstDisplayGptDiagnosticsV1, FirstDisplayGptFactV1 } from '../shared/takeover';

export const FIRST_DISPLAY_CONTRACT_IDS: readonly FirstDisplaySliceId[] = Object.freeze([
  'first_display',
  'aps_initial',
  'creative_initial',
  'datadome_initial',
  'didomi_initial',
  'google_tag_manager_initial',
  'gpt_initial',
  'lockr_initial',
  'osano_initial',
  'permutive_initial',
  'sourcepoint_initial',
  'prebid_initial',
  'testlight_initial',
]);

export const MAX_FIRST_DISPLAY_SLOTS = 256;
export const MAX_FIRST_DISPLAY_NON_DIAGNOSTICS_BYTES = 8 * 1024 * 1024;
export const MAX_GPT_FACT_BYTES = 512 * 1024;
export const MAX_SINGLE_GPT_FACT_BYTES = 1_000;
export const MAX_FIRST_DISPLAY_HANDOFF_BYTES = 8.5 * 1024 * 1024;

const MAX_U32 = 4_294_967_295;
const MAX_STRING_BYTES = 4096;
const MAX_PROPERTY_BYTES = 128;
const MAX_TARGETING = 32;
const MAX_FORMATS = 32;
const MAX_ALIASES = 32;
const MAX_FACTS = 512;
const HASH = /^[0-9a-f]{64}$/;
const CAPABILITY = /^[a-z][a-z0-9_]*(?:[._][a-z0-9_]+)*$/;
const FIRST_DISPLAY_ORDER = new Map(
  FIRST_DISPLAY_CONTRACT_IDS.map((id, index) => [id, index + 1] as const)
);

export type TerminalAttemptState = 'accepted' | 'no_bid' | 'failed' | 'cancelled';
export type CommittedArtifactKind = 'none' | 'gpt_adm' | 'aps';

export interface TakeoverOutlineV1 {
  readonly version: 1;
  readonly releaseId: string;
  readonly generation: number;
  readonly projectionDigest: string;
  readonly slices: readonly FirstDisplaySliceId[];
  readonly slotCount: number;
  readonly outcomeCount: number;
  readonly capabilities: readonly string[];
  readonly objectKinds: readonly ('gpt_slot' | 'dom_artifact')[];
}

export interface FirstDisplayHandoffV1 {
  readonly version: 1;
  readonly releaseId: string;
  readonly generation: number;
  readonly projectionDigest: string;
  readonly slices: readonly FirstDisplaySliceId[];
  readonly slots: readonly Readonly<Record<string, unknown>>[];
  readonly attempts: readonly Readonly<Record<string, unknown>>[];
  readonly tombstones: readonly Readonly<Record<string, unknown>>[];
  readonly artifacts: readonly Readonly<Record<string, unknown>>[];
  readonly parserState: readonly Readonly<Record<string, unknown>>[];
  readonly gptDiagnostics: Readonly<FirstDisplayGptDiagnosticsV1>;
  readonly timing: Readonly<Record<string, number | null>>;
  readonly highWater: Readonly<Record<string, number | string>>;
  readonly cycles: readonly Readonly<Record<string, unknown>>[];
  readonly trace: Readonly<Record<string, unknown>>;
  readonly mutationRevision: number;
}

export interface FirstDisplayOwnershipCapsuleV1<T extends object = object> {
  readonly releaseId: string;
  readonly generation: number;
  readonly consume: (releaseId: string, generation: number) => readonly T[] | undefined;
  readonly clear: () => void;
}

function utf8Length(value: string): number {
  return new TextEncoder().encode(value).byteLength;
}

function isU32(value: unknown, allowZero = true): value is number {
  return (
    typeof value === 'number' &&
    Number.isInteger(value) &&
    value >= (allowZero ? 0 : 1) &&
    value <= MAX_U32
  );
}

function finiteNonnegative(value: unknown): value is number {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0;
}

function boundedString(
  value: unknown,
  maximum = MAX_STRING_BYTES,
  allowEmpty = false
): value is string {
  return (
    typeof value === 'string' &&
    (allowEmpty || value.length > 0) &&
    utf8Length(value) <= maximum &&
    !value.split('').some((character) => {
      const code = character.charCodeAt(0);
      return code <= 0x1f || code === 0x7f;
    })
  );
}

function exactRecord(
  value: unknown,
  keys: readonly string[]
): Readonly<Record<string, unknown>> | undefined {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) return undefined;
  if (Object.getPrototypeOf(value) !== Object.prototype) return undefined;
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
}

function exactArray(value: unknown, maximum: number): readonly unknown[] | undefined {
  if (!Array.isArray(value) || Object.getPrototypeOf(value) !== Array.prototype) return undefined;
  const lengthDescriptor = Object.getOwnPropertyDescriptor(value, 'length');
  if (!lengthDescriptor || !('value' in lengthDescriptor)) return undefined;
  const length = lengthDescriptor.value;
  if (!Number.isSafeInteger(length) || length < 0 || length > maximum) return undefined;
  const keys = Reflect.ownKeys(value);
  if (keys.length !== length + 1) return undefined;
  const result: unknown[] = [];
  for (let index = 0; index < length; index += 1) {
    const descriptor = Object.getOwnPropertyDescriptor(value, String(index));
    if (!descriptor?.enumerable || !('value' in descriptor)) return undefined;
    result.push(descriptor.value);
  }
  return result;
}

function uniqueStrings(
  value: unknown,
  maximum: number,
  validate: (value: string) => boolean = (candidate) => boundedString(candidate)
): readonly string[] | undefined {
  const values = exactArray(value, maximum);
  if (!values) return undefined;
  const seen = new Set<string>();
  const result: string[] = [];
  for (const candidate of values) {
    if (typeof candidate !== 'string' || !validate(candidate) || seen.has(candidate))
      return undefined;
    seen.add(candidate);
    result.push(candidate);
  }
  return Object.freeze(result);
}

function snapshotSlices(value: unknown): readonly FirstDisplaySliceId[] | undefined {
  const slices = uniqueStrings(value, FIRST_DISPLAY_CONTRACT_IDS.length, (candidate) =>
    FIRST_DISPLAY_ORDER.has(candidate as FirstDisplaySliceId)
  );
  if (!slices || slices[0] !== 'first_display') return undefined;
  let previous = 0;
  for (const slice of slices) {
    const order = FIRST_DISPLAY_ORDER.get(slice as FirstDisplaySliceId);
    if (!order || order <= previous) return undefined;
    previous = order;
  }
  return slices as readonly FirstDisplaySliceId[];
}

function freezeRecord<T extends Record<string, unknown>>(value: T): Readonly<T> {
  return Object.freeze(value);
}

function canonicalBytes(value: unknown): number | undefined {
  try {
    return utf8Length(JSON.stringify(value));
  } catch {
    return undefined;
  }
}

export function snapshotTakeoverOutlineV1(candidate: unknown): TakeoverOutlineV1 | undefined {
  try {
    const fields = exactRecord(candidate, [
      'version',
      'releaseId',
      'generation',
      'projectionDigest',
      'slices',
      'slotCount',
      'outcomeCount',
      'capabilities',
      'objectKinds',
    ]);
    if (!fields) return undefined;
    const slices = snapshotSlices(fields.slices);
    const capabilities = uniqueStrings(fields.capabilities, 32, (value) => CAPABILITY.test(value));
    const objectKinds = uniqueStrings(
      fields.objectKinds,
      2,
      (value) => value === 'gpt_slot' || value === 'dom_artifact'
    ) as readonly ('gpt_slot' | 'dom_artifact')[] | undefined;
    if (
      fields.version !== 1 ||
      typeof fields.releaseId !== 'string' ||
      !HASH.test(fields.releaseId) ||
      !isU32(fields.generation, false) ||
      typeof fields.projectionDigest !== 'string' ||
      !HASH.test(fields.projectionDigest) ||
      !slices ||
      !isU32(fields.slotCount, false) ||
      fields.slotCount > MAX_FIRST_DISPLAY_SLOTS ||
      fields.outcomeCount !== fields.slotCount ||
      !capabilities ||
      !objectKinds
    ) {
      return undefined;
    }
    return Object.freeze({
      version: 1,
      releaseId: fields.releaseId,
      generation: fields.generation,
      projectionDigest: fields.projectionDigest,
      slices,
      slotCount: fields.slotCount,
      outcomeCount: fields.outcomeCount,
      capabilities,
      objectKinds,
    });
  } catch {
    return undefined;
  }
}

function snapshotStringPairs(
  value: unknown,
  maximum: number
): readonly (readonly [string, string])[] | undefined {
  const entries = exactArray(value, maximum);
  if (!entries) return undefined;
  const seen = new Set<string>();
  const result: Array<readonly [string, string]> = [];
  for (const entry of entries) {
    const pair = exactArray(entry, 2);
    if (
      !pair ||
      pair.length !== 2 ||
      !boundedString(pair[0], MAX_PROPERTY_BYTES) ||
      !boundedString(pair[1], MAX_STRING_BYTES, true) ||
      seen.has(pair[0])
    ) {
      return undefined;
    }
    seen.add(pair[0]);
    result.push(Object.freeze([pair[0], pair[1]]));
  }
  return Object.freeze(result);
}

function snapshotSlot(value: unknown): Readonly<Record<string, unknown>> | undefined {
  const fields = exactRecord(value, [
    'id',
    'aliases',
    'domId',
    'gamPath',
    'formats',
    'owner',
    'outcome',
    'targeting',
    'committedArtifact',
    'gptToken',
  ]);
  if (!fields) return undefined;
  const aliases = uniqueStrings(fields.aliases, MAX_ALIASES);
  const formatValues = exactArray(fields.formats, MAX_FORMATS);
  const formats: Array<readonly [number, number]> = [];
  if (!formatValues) return undefined;
  for (const format of formatValues) {
    const dimensions = exactArray(format, 2);
    if (
      !dimensions ||
      dimensions.length !== 2 ||
      !isU32(dimensions[0], false) ||
      !isU32(dimensions[1], false) ||
      dimensions[0] > 4096 ||
      dimensions[1] > 4096
    ) {
      return undefined;
    }
    formats.push(Object.freeze([dimensions[0], dimensions[1]]));
  }
  const targeting = snapshotStringPairs(fields.targeting, MAX_TARGETING);
  if (
    !boundedString(fields.id) ||
    !aliases ||
    !boundedString(fields.domId) ||
    !boundedString(fields.gamPath) ||
    (fields.owner !== 'trusted_server' && fields.owner !== 'publisher') ||
    !['accepted', 'no_bid', 'failed', 'cancelled'].includes(fields.outcome as string) ||
    !targeting ||
    !['none', 'gpt_adm', 'aps'].includes(fields.committedArtifact as string) ||
    (fields.gptToken !== null &&
      (!boundedString(fields.gptToken) || !/^gt1_[1-9a-z][0-9a-z]{0,6}$/.test(fields.gptToken)))
  ) {
    return undefined;
  }
  return freezeRecord({
    id: fields.id,
    aliases,
    domId: fields.domId,
    gamPath: fields.gamPath,
    formats: Object.freeze(formats),
    owner: fields.owner,
    outcome: fields.outcome,
    targeting,
    committedArtifact: fields.committedArtifact,
    gptToken: fields.gptToken,
  });
}

function snapshotAttempt(value: unknown): Readonly<Record<string, unknown>> | undefined {
  const fields = exactRecord(value, ['id', 'slotId', 'ordinal', 'state', 'reason']);
  if (
    !fields ||
    typeof fields.id !== 'string' ||
    !/^a1_[A-Za-z0-9_-]{22}$/.test(fields.id) ||
    !boundedString(fields.slotId) ||
    !isU32(fields.ordinal, false) ||
    !['accepted', 'no_bid', 'failed', 'cancelled'].includes(fields.state as string) ||
    (fields.state === 'accepted' || fields.state === 'no_bid'
      ? fields.reason !== null
      : !boundedString(fields.reason))
  ) {
    return undefined;
  }
  return freezeRecord({ ...fields });
}

function snapshotTombstone(value: unknown): Readonly<Record<string, unknown>> | undefined {
  const fields = exactRecord(value, ['kind', 'value', 'expiresAtMs', 'ordinal']);
  if (
    !fields ||
    (fields.kind !== 'reservation' && fields.kind !== 'ticket') ||
    typeof fields.value !== 'string' ||
    (fields.kind === 'reservation'
      ? !/^r1_[A-Za-z0-9_-]{22}$/.test(fields.value)
      : !/^t1_[A-Za-z0-9_-]{22}$/.test(fields.value)) ||
    !finiteNonnegative(fields.expiresAtMs) ||
    !isU32(fields.ordinal, false)
  ) {
    return undefined;
  }
  return freezeRecord({ ...fields });
}

function snapshotArtifact(value: unknown): Readonly<Record<string, unknown>> | undefined {
  const fields = exactRecord(value, ['slotId', 'kind', 'owner', 'token']);
  if (
    !fields ||
    !boundedString(fields.slotId) ||
    (fields.kind !== 'gpt_adm' && fields.kind !== 'aps') ||
    (fields.owner !== 'trusted_server' && fields.owner !== 'publisher') ||
    typeof fields.token !== 'string' ||
    !/^r1_[A-Za-z0-9_-]{22}$/.test(fields.token)
  ) {
    return undefined;
  }
  return freezeRecord({ ...fields });
}

function validParserValues(
  sliceId: unknown,
  values: readonly (readonly [string, string | number | boolean | null])[]
): boolean {
  const single = (
    key: string,
    validate: (value: string | number | boolean | null) => boolean
  ): boolean => values.length === 1 && values[0]?.[0] === key && validate(values[0][1]);
  const integer = (value: string | number | boolean | null): value is number =>
    typeof value === 'number' && Number.isInteger(value) && value >= 0;
  switch (sliceId) {
    case 'aps_initial':
    case 'gpt_initial':
    case 'prebid_initial':
      return single('protocol_version', (value) => value === 1);
    case 'creative_initial':
      return single('guard_count', (value) => integer(value) && value >= 1 && value <= 3);
    case 'datadome_initial':
      return single('route_guard', (value) => value === 'datadome');
    case 'didomi_initial':
      return single('sdk_path', (value) => typeof value === 'string' && value.length > 0);
    case 'google_tag_manager_initial':
      return single('route_guard', (value) => value === 'google_tag_manager');
    case 'lockr_initial':
      return (
        values[0]?.[0] === 'route_guard' &&
        values[0][1] === 'lockr' &&
        (values.length === 1 ||
          (values.length === 2 &&
            (values[1]?.[0] === 'sdk_host'
              ? typeof values[1][1] === 'string' && values[1][1].length > 0
              : values[1]?.[0] === 'readiness_timeout' && values[1][1] === 50)))
      );
    case 'osano_initial':
      return values.length === 0 || single('consent_snapshot', integer);
    case 'permutive_initial':
      return (
        values.length === 0 ||
        single('sdk_config', (value) => typeof value === 'string' && value.length > 0) ||
        single('readiness_timeout', (value) => value === 50)
      );
    case 'sourcepoint_initial':
      return single('gpp_snapshot', integer);
    case 'testlight_initial':
      return single('callback_count', integer);
    default:
      return false;
  }
}

function snapshotParserState(value: unknown): Readonly<Record<string, unknown>> | undefined {
  const fields = exactRecord(value, ['sliceId', 'observations', 'values']);
  const observations = uniqueStrings(fields?.observations, 256);
  const entries = exactArray(fields?.values, 256);
  if (
    !fields ||
    !snapshotSlices(['first_display', fields.sliceId]) ||
    !observations ||
    !entries ||
    observations.length !== entries.length
  ) {
    return undefined;
  }
  const seen = new Set<string>();
  const values: Array<readonly [string, string | number | boolean | null]> = [];
  for (const entry of entries) {
    const pair = exactArray(entry, 2);
    if (
      !pair ||
      pair.length !== 2 ||
      !boundedString(pair[0], MAX_PROPERTY_BYTES) ||
      pair[0] !== observations[values.length] ||
      seen.has(pair[0])
    ) {
      return undefined;
    }
    const scalar = pair[1];
    if (
      scalar !== null &&
      typeof scalar !== 'string' &&
      typeof scalar !== 'boolean' &&
      !(typeof scalar === 'number' && Number.isFinite(scalar))
    ) {
      return undefined;
    }
    if (typeof scalar === 'string' && !boundedString(scalar, MAX_STRING_BYTES, true))
      return undefined;
    seen.add(pair[0]);
    values.push(Object.freeze([pair[0], scalar]));
  }
  if (!validParserValues(fields.sliceId, values)) return undefined;
  return freezeRecord({
    sliceId: fields.sliceId,
    observations,
    values: Object.freeze(values),
  });
}

function snapshotTiming(value: unknown): Readonly<Record<string, number | null>> | undefined {
  const fields = exactRecord(value, ['bidsScriptMs', 'firstDisplayMs', 'terminalMs', 'paintMs']);
  if (!fields) return undefined;
  for (const key of ['bidsScriptMs', 'terminalMs', 'paintMs'] as const) {
    if (!finiteNonnegative(fields[key])) return undefined;
  }
  if (fields.firstDisplayMs !== null && !finiteNonnegative(fields.firstDisplayMs)) return undefined;
  if (
    (fields.bidsScriptMs as number) > (fields.terminalMs as number) ||
    (fields.terminalMs as number) > (fields.paintMs as number) ||
    (fields.firstDisplayMs !== null &&
      ((fields.bidsScriptMs as number) > (fields.firstDisplayMs as number) ||
        (fields.firstDisplayMs as number) > (fields.terminalMs as number)))
  ) {
    return undefined;
  }
  return freezeRecord({
    bidsScriptMs: fields.bidsScriptMs as number,
    firstDisplayMs: fields.firstDisplayMs as number | null,
    terminalMs: fields.terminalMs as number,
    paintMs: fields.paintMs as number,
  });
}

function snapshotHighWater(value: unknown): Readonly<Record<string, number | string>> | undefined {
  const fields = exactRecord(value, [
    'navigationAttemptPrefix',
    'nextNavigationAttemptOrdinal',
    'nextAttemptOrdinal',
    'nextSlotRegistrationOrdinal',
    'reservationClockEpochMs',
    'nextReservationOrdinal',
    'nextTicketOrdinal',
  ]);
  if (
    !fields ||
    typeof fields.navigationAttemptPrefix !== 'string' ||
    !/^[A-Za-z0-9_-]{11}$/.test(fields.navigationAttemptPrefix)
  ) {
    return undefined;
  }
  for (const key of [
    'nextNavigationAttemptOrdinal',
    'nextAttemptOrdinal',
    'nextSlotRegistrationOrdinal',
    'nextReservationOrdinal',
    'nextTicketOrdinal',
  ] as const) {
    if (!isU32(fields[key], false)) return undefined;
  }
  if (!finiteNonnegative(fields.reservationClockEpochMs)) return undefined;
  return freezeRecord({ ...fields }) as Readonly<Record<string, number | string>>;
}

const GPT_DIAGNOSTIC_EVENTS = Object.freeze([
  'slotRequested',
  'slotResponseReceived',
  'slotRenderEnded',
  'slotOnload',
  'impressionViewable',
  'slotVisibilityChanged',
] as const);

function snapshotCycle(value: unknown): Readonly<Record<string, unknown>> | undefined {
  const fields = exactRecord(value, [
    'slotId',
    'token',
    'nextCycleOrdinal',
    'unknownPriorCycle',
    'records',
    'quarantines',
  ]);
  const records = exactArray(fields?.records, 10);
  const quarantines = uniqueStrings(fields?.quarantines, 10);
  if (
    !fields ||
    !boundedString(fields.slotId) ||
    !boundedString(fields.token) ||
    !/^gt1_[1-9a-z][0-9a-z]{0,6}$/.test(fields.token) ||
    !isU32(fields.nextCycleOrdinal, false) ||
    typeof fields.unknownPriorCycle !== 'boolean' ||
    !records ||
    !quarantines
  ) {
    return undefined;
  }
  const normalized: Array<Readonly<Record<string, unknown>>> = [];
  let maximum = 0;
  for (const record of records) {
    const recordFields = exactRecord(record, ['ordinal', 'responseIdentifier', 'seen', 'state']);
    const seen = uniqueStrings(recordFields?.seen, GPT_DIAGNOSTIC_EVENTS.length, (event) =>
      GPT_DIAGNOSTIC_EVENTS.includes(event as (typeof GPT_DIAGNOSTIC_EVENTS)[number])
    );
    if (
      !recordFields ||
      !isU32(recordFields.ordinal, false) ||
      (recordFields.responseIdentifier !== null &&
        !boundedString(recordFields.responseIdentifier, 256)) ||
      !seen ||
      !seen.includes('slotRequested') ||
      !['open', 'completed', 'retired'].includes(recordFields.state as string) ||
      (recordFields.state === 'open' && seen.includes('slotRenderEnded')) ||
      (recordFields.state === 'completed' && !seen.includes('slotRenderEnded')) ||
      recordFields.ordinal <= maximum
    ) {
      return undefined;
    }
    maximum = Math.max(maximum, recordFields.ordinal);
    normalized.push(freezeRecord({ ...recordFields, seen }));
  }
  if (fields.nextCycleOrdinal <= maximum) return undefined;
  return freezeRecord({
    slotId: fields.slotId,
    token: fields.token,
    nextCycleOrdinal: fields.nextCycleOrdinal,
    unknownPriorCycle: fields.unknownPriorCycle,
    records: Object.freeze(normalized),
    quarantines,
  });
}

function snapshotTrace(value: unknown): Readonly<Record<string, unknown>> | undefined {
  const fields = exactRecord(value, ['nextSequence', 'nextGlobalSlotOrdinal', 'slots']);
  const slots = exactArray(fields?.slots, MAX_FIRST_DISPLAY_SLOTS);
  if (
    !fields ||
    !isU32(fields.nextSequence, false) ||
    !isU32(fields.nextGlobalSlotOrdinal, false) ||
    !slots
  ) {
    return undefined;
  }
  const normalized: Array<Readonly<Record<string, unknown>>> = [];
  for (const slot of slots) {
    const slotFields = exactRecord(slot, ['slotId', 'impressions', 'bindings']);
    const rawBindings = exactArray(slotFields?.bindings, 10);
    if (
      !slotFields ||
      !boundedString(slotFields.slotId) ||
      !isU32(slotFields.impressions) ||
      !rawBindings
    ) {
      return undefined;
    }
    const bindings: Array<Readonly<Record<string, unknown>>> = [];
    const compoundKeys = new Set<string>();
    const historySequences = new Set<number>();
    for (const binding of rawBindings) {
      const bindingFields = exactRecord(binding, [
        'atMs',
        'cycleOrdinal',
        'historySequence',
        'state',
        'token',
      ]);
      if (
        !bindingFields ||
        !finiteNonnegative(bindingFields.atMs) ||
        !isU32(bindingFields.cycleOrdinal, false) ||
        !isU32(bindingFields.historySequence, false) ||
        bindingFields.state !== 'completed' ||
        typeof bindingFields.token !== 'string' ||
        !/^gt1_[1-9a-z][0-9a-z]{0,6}$/.test(bindingFields.token)
      ) {
        return undefined;
      }
      const compoundKey = `${bindingFields.token}:${bindingFields.cycleOrdinal}`;
      if (compoundKeys.has(compoundKey) || historySequences.has(bindingFields.historySequence)) {
        return undefined;
      }
      compoundKeys.add(compoundKey);
      historySequences.add(bindingFields.historySequence);
      bindings.push(freezeRecord({ ...bindingFields }));
    }
    normalized.push(freezeRecord({ ...slotFields, bindings: Object.freeze(bindings) }));
  }
  return freezeRecord({
    nextSequence: fields.nextSequence,
    nextGlobalSlotOrdinal: fields.nextGlobalSlotOrdinal,
    slots: Object.freeze(normalized),
  });
}

const GPT_DIAGNOSTIC_DISPOSITIONS = Object.freeze(['matched', 'unmatched', 'ambiguous'] as const);
const GPT_DIAGNOSTIC_ISSUES = Object.freeze([
  'no_request_cycle',
  'overlapping_request_cycles',
  'unknown_prior_cycle',
  'invalid_event_order',
] as const);

function snapshotGptFact(value: unknown): Readonly<FirstDisplayGptFactV1> | undefined {
  const fields = exactRecord(value, [
    'version',
    'event',
    'token',
    'runtimeSlotNumber',
    'cycleOrdinal',
    'disposition',
    'issueReason',
    'capturedAtMs',
    'elementId',
    'adUnitPath',
    'isEmpty',
    'renderedSize',
    'isBackfill',
    'slotContentChanged',
    'visibilityPercent',
  ]);
  if (!fields) return undefined;
  const event = GPT_DIAGNOSTIC_EVENTS.includes(
    fields.event as (typeof GPT_DIAGNOSTIC_EVENTS)[number]
  )
    ? (fields.event as FirstDisplayGptFactV1['event'])
    : undefined;
  const disposition = GPT_DIAGNOSTIC_DISPOSITIONS.includes(
    fields.disposition as (typeof GPT_DIAGNOSTIC_DISPOSITIONS)[number]
  )
    ? (fields.disposition as FirstDisplayGptFactV1['disposition'])
    : undefined;
  const issueReason =
    fields.issueReason === null ||
    GPT_DIAGNOSTIC_ISSUES.includes(fields.issueReason as (typeof GPT_DIAGNOSTIC_ISSUES)[number])
      ? (fields.issueReason as FirstDisplayGptFactV1['issueReason'])
      : undefined;
  const renderedSize = (() => {
    const dimensions = exactArray(fields.renderedSize, 2);
    if (
      !dimensions ||
      dimensions.length !== 2 ||
      !dimensions.every(
        (dimension) =>
          typeof dimension === 'number' &&
          Number.isInteger(dimension) &&
          dimension >= 1 &&
          dimension <= 4096
      )
    ) {
      return undefined;
    }
    return Object.freeze([dimensions[0] as number, dimensions[1] as number] as const);
  })();
  const tokenOrdinal =
    typeof fields.token === 'string' && /^gt1_[1-9a-z][0-9a-z]{0,6}$/.test(fields.token)
      ? Number.parseInt(fields.token.slice(4), 36)
      : undefined;
  if (
    fields.version !== 1 ||
    !event ||
    tokenOrdinal === undefined ||
    tokenOrdinal > MAX_U32 ||
    !isU32(fields.runtimeSlotNumber, false) ||
    fields.runtimeSlotNumber !== tokenOrdinal ||
    (fields.cycleOrdinal !== null && !isU32(fields.cycleOrdinal, false)) ||
    !disposition ||
    issueReason === undefined ||
    !finiteNonnegative(fields.capturedAtMs) ||
    (fields.elementId !== null && !boundedString(fields.elementId, 256)) ||
    (fields.adUnitPath !== null && !boundedString(fields.adUnitPath, 256)) ||
    (fields.isEmpty !== null && typeof fields.isEmpty !== 'boolean') ||
    (fields.renderedSize !== null && !renderedSize) ||
    (fields.isBackfill !== null && typeof fields.isBackfill !== 'boolean') ||
    (fields.slotContentChanged !== null && typeof fields.slotContentChanged !== 'boolean') ||
    (fields.visibilityPercent !== null &&
      (typeof fields.visibilityPercent !== 'number' ||
        !Number.isFinite(fields.visibilityPercent) ||
        fields.visibilityPercent < 0 ||
        fields.visibilityPercent > 100)) ||
    (event !== 'slotRenderEnded' &&
      (fields.isEmpty !== null ||
        fields.renderedSize !== null ||
        fields.isBackfill !== null ||
        fields.slotContentChanged !== null)) ||
    (event !== 'slotVisibilityChanged' && fields.visibilityPercent !== null) ||
    (issueReason === 'invalid_event_order' && disposition !== 'matched') ||
    (issueReason === 'overlapping_request_cycles' && disposition !== 'ambiguous') ||
    ((issueReason === 'no_request_cycle' || issueReason === 'unknown_prior_cycle') &&
      disposition !== 'unmatched') ||
    (disposition === 'matched' && event !== 'slotVisibilityChanged' && fields.cycleOrdinal === null)
  ) {
    return undefined;
  }
  const result = Object.freeze({
    version: 1 as const,
    event,
    token: fields.token as string,
    runtimeSlotNumber: fields.runtimeSlotNumber,
    cycleOrdinal: fields.cycleOrdinal as number | null,
    disposition,
    issueReason,
    capturedAtMs: fields.capturedAtMs as number,
    elementId: fields.elementId as string | null,
    adUnitPath: fields.adUnitPath as string | null,
    isEmpty: fields.isEmpty as boolean | null,
    renderedSize: fields.renderedSize === null ? null : renderedSize!,
    isBackfill: fields.isBackfill as boolean | null,
    slotContentChanged: fields.slotContentChanged as boolean | null,
    visibilityPercent: fields.visibilityPercent as number | null,
  });
  const bytes = canonicalBytes(result);
  return bytes !== undefined && bytes <= MAX_SINGLE_GPT_FACT_BYTES ? result : undefined;
}

function snapshotGptDiagnostics(
  value: unknown
): Readonly<FirstDisplayGptDiagnosticsV1> | undefined {
  const fields = exactRecord(value, ['facts', 'overflowCount', 'dropCount']);
  if (!fields) return undefined;
  const values = exactArray(fields.facts, MAX_FACTS);
  if (!values || !isU32(fields.overflowCount) || !isU32(fields.dropCount)) {
    return undefined;
  }
  const facts: Array<Readonly<FirstDisplayGptFactV1>> = [];
  for (const value of values) {
    const fact = snapshotGptFact(value);
    if (!fact) return undefined;
    facts.push(fact);
  }
  const result = Object.freeze({
    facts: Object.freeze(facts),
    overflowCount: fields.overflowCount,
    dropCount: fields.dropCount,
  });
  const bytes = canonicalBytes(result);
  return bytes !== undefined && bytes <= MAX_GPT_FACT_BYTES ? result : undefined;
}

function snapshotList(
  value: unknown,
  maximum: number,
  snapshot: (value: unknown) => Readonly<Record<string, unknown>> | undefined
): readonly Readonly<Record<string, unknown>>[] | undefined {
  const values = exactArray(value, maximum);
  if (!values) return undefined;
  const result: Array<Readonly<Record<string, unknown>>> = [];
  for (const entry of values) {
    const accepted = snapshot(entry);
    if (!accepted) return undefined;
    result.push(accepted);
  }
  return Object.freeze(result);
}

export function snapshotFirstDisplayHandoffV1(
  candidate: unknown
): FirstDisplayHandoffV1 | undefined {
  try {
    const fields = exactRecord(candidate, [
      'version',
      'releaseId',
      'generation',
      'projectionDigest',
      'slices',
      'slots',
      'attempts',
      'tombstones',
      'artifacts',
      'parserState',
      'gptDiagnostics',
      'timing',
      'highWater',
      'cycles',
      'trace',
      'mutationRevision',
    ]);
    if (!fields) return undefined;
    const slices = snapshotSlices(fields.slices);
    const slots = snapshotList(fields.slots, MAX_FIRST_DISPLAY_SLOTS, snapshotSlot);
    const attempts = snapshotList(fields.attempts, MAX_FIRST_DISPLAY_SLOTS, snapshotAttempt);
    const tombstones = snapshotList(fields.tombstones, 512, snapshotTombstone);
    const artifacts = snapshotList(fields.artifacts, MAX_FIRST_DISPLAY_SLOTS, snapshotArtifact);
    const parserState = snapshotList(
      fields.parserState,
      FIRST_DISPLAY_CONTRACT_IDS.length,
      snapshotParserState
    );
    const gptDiagnostics = snapshotGptDiagnostics(fields.gptDiagnostics);
    const timing = snapshotTiming(fields.timing);
    const highWater = snapshotHighWater(fields.highWater);
    const cycles = snapshotList(fields.cycles, MAX_FIRST_DISPLAY_SLOTS, snapshotCycle);
    const trace = snapshotTrace(fields.trace);
    if (
      fields.version !== 1 ||
      typeof fields.releaseId !== 'string' ||
      !HASH.test(fields.releaseId) ||
      !isU32(fields.generation, false) ||
      typeof fields.projectionDigest !== 'string' ||
      !HASH.test(fields.projectionDigest) ||
      !slices ||
      !slots ||
      !attempts ||
      !tombstones ||
      !artifacts ||
      !parserState ||
      !gptDiagnostics ||
      !timing ||
      !highWater ||
      !cycles ||
      !trace ||
      !isU32(fields.mutationRevision)
    ) {
      return undefined;
    }
    const slotIds = new Set(slots.map((slot) => slot.id as string));
    const attemptIds = new Set(attempts.map((attempt) => attempt.id as string));
    const attemptOrdinals = new Set(attempts.map((attempt) => attempt.ordinal as number));
    const attemptSlotIds = new Set(attempts.map((attempt) => attempt.slotId as string));
    const tombstoneKeys = new Set(
      tombstones.map((tombstone) => `${String(tombstone.kind)}:${String(tombstone.value)}`)
    );
    const tombstoneOrdinals = new Set(
      tombstones.map((tombstone) => `${String(tombstone.kind)}:${String(tombstone.ordinal)}`)
    );
    if (
      parserState.length !== slices.length - 1 ||
      parserState.some((state, index) => state.sliceId !== slices[index + 1])
    ) {
      return undefined;
    }
    const cycleSlotIds = new Set(cycles.map((cycle) => cycle.slotId as string));
    const artifactSlotIds = new Set(artifacts.map((artifact) => artifact.slotId as string));
    const traceSlots = trace.slots as readonly Readonly<Record<string, unknown>>[];
    const traceSlotIds = new Set(traceSlots.map((slot) => slot.slotId as string));
    const slotById = new Map(slots.map((slot) => [slot.id as string, slot]));
    const traceBySlot = new Map(traceSlots.map((slot) => [slot.slotId as string, slot]));
    const artifactBySlot = new Map(
      artifacts.map((artifact) => [artifact.slotId as string, artifact])
    );
    const cycleBySlot = new Map(cycles.map((cycle) => [cycle.slotId as string, cycle]));
    if (
      ((cycles.length > 0 ||
        gptDiagnostics.facts.length > 0 ||
        slots.some((slot) => slot.gptToken !== null)) &&
        !slices.includes('gpt_initial')) ||
      slotIds.size !== slots.length ||
      attempts.length !== slots.length ||
      attemptIds.size !== attempts.length ||
      attemptOrdinals.size !== attempts.length ||
      attemptSlotIds.size !== attempts.length ||
      tombstoneKeys.size !== tombstones.length ||
      tombstoneOrdinals.size !== tombstones.length ||
      tombstones.some(
        (tombstone) =>
          (tombstone.expiresAtMs as number) <= (highWater.reservationClockEpochMs as number)
      ) ||
      cycleSlotIds.size !== cycles.length ||
      artifactSlotIds.size !== artifacts.length ||
      traceSlotIds.size !== traceSlots.length ||
      traceSlots.length !== slots.length ||
      traceSlots.some((traceSlot, index) => {
        const slot = slots[index];
        if (!slot || traceSlot.slotId !== slot.id) return true;
        const accepted = slot.outcome === 'accepted';
        const bindings = traceSlot.bindings as readonly Readonly<Record<string, unknown>>[];
        return (
          traceSlot.impressions !== (accepted ? 1 : 0) ||
          (accepted
            ? bindings.length !== 1 || bindings[0]?.token !== slot.gptToken
            : bindings.length !== 0)
        );
      }) ||
      attempts.some((attempt, index) => {
        const slot = slots[index];
        return (
          !slot ||
          attempt.slotId !== slot.id ||
          attempt.state !== slot.outcome ||
          attempt.ordinal !== index + 1 ||
          !(attempt.id as string).startsWith(`a1_${highWater.navigationAttemptPrefix as string}`)
        );
      }) ||
      slots.some((slot) => {
        const artifact = artifactBySlot.get(slot.id as string);
        const cycle = cycleBySlot.get(slot.id as string);
        if (slot.outcome !== 'accepted') {
          return slot.committedArtifact !== 'none' || slot.gptToken !== null || artifact || cycle;
        }
        const targeting = slot.targeting as readonly (readonly [string, string])[];
        const reservation = targeting.find(([key]) => key === 'hb_adid')?.[1];
        return (
          slot.committedArtifact === 'none' ||
          typeof slot.gptToken !== 'string' ||
          !artifact ||
          artifact.kind !== slot.committedArtifact ||
          artifact.token !== reservation ||
          !cycle ||
          cycle.token !== slot.gptToken
        );
      }) ||
      cycles.some((cycle) => {
        const slot = slotById.get(cycle.slotId as string);
        const traceSlot = traceBySlot.get(cycle.slotId as string);
        const bindings = traceSlot?.bindings as
          readonly Readonly<Record<string, unknown>>[] | undefined;
        const cycleRecords = cycle.records as readonly Readonly<Record<string, unknown>>[];
        return (
          !slot ||
          slot.gptToken !== cycle.token ||
          !bindings ||
          bindings.length !== 1 ||
          bindings[0]?.token !== cycle.token ||
          !cycleRecords.some(
            (record) =>
              record.ordinal === bindings[0]?.cycleOrdinal && record.state === bindings[0]?.state
          )
        );
      })
    ) {
      return undefined;
    }
    const maximumGlobalSlotOrdinal = [
      ...cycles.map((cycle) => Number.parseInt((cycle.token as string).slice(4), 36)),
      ...gptDiagnostics.facts.map((fact) => fact.runtimeSlotNumber),
    ].reduce((maximum, ordinal) => Math.max(maximum, ordinal), 0);
    if ((trace.nextGlobalSlotOrdinal as number) <= maximumGlobalSlotOrdinal) return undefined;
    const transferredImpressions = traceSlots.reduce(
      (count, slot) => count + (slot.impressions as number),
      0
    );
    const traceBindings = traceSlots.flatMap(
      (slot) => slot.bindings as readonly Readonly<Record<string, unknown>>[]
    );
    const traceHistorySequences = new Set(
      traceBindings.map((binding) => binding.historySequence as number)
    );
    const maximumTraceSequence = traceBindings.reduce(
      (maximum, binding) => Math.max(maximum, binding.historySequence as number),
      0
    );
    if (
      traceBindings.length !== transferredImpressions ||
      traceHistorySequences.size !== traceBindings.length ||
      traceBindings.some(
        (binding) =>
          (binding.atMs as number) < (timing.bidsScriptMs as number) ||
          (binding.atMs as number) > (timing.terminalMs as number)
      ) ||
      (trace.nextSequence as number) <= maximumTraceSequence
    ) {
      return undefined;
    }
    const maximumAttempt = attempts.reduce(
      (maximum, attempt) => Math.max(maximum, attempt.ordinal as number),
      0
    );
    const maximumReservation = tombstones.reduce(
      (maximum, tombstone) =>
        tombstone.kind === 'reservation' ? Math.max(maximum, tombstone.ordinal as number) : maximum,
      0
    );
    const maximumTicket = tombstones.reduce(
      (maximum, tombstone) =>
        tombstone.kind === 'ticket' ? Math.max(maximum, tombstone.ordinal as number) : maximum,
      0
    );
    if (
      (highWater.nextNavigationAttemptOrdinal as number) <= maximumAttempt ||
      (highWater.nextAttemptOrdinal as number) <= maximumAttempt ||
      (highWater.nextSlotRegistrationOrdinal as number) <= slots.length ||
      (highWater.nextReservationOrdinal as number) <= maximumReservation ||
      (highWater.nextTicketOrdinal as number) <= maximumTicket
    ) {
      return undefined;
    }
    const result = Object.freeze({
      version: 1 as const,
      releaseId: fields.releaseId,
      generation: fields.generation,
      projectionDigest: fields.projectionDigest,
      slices,
      slots,
      attempts,
      tombstones,
      artifacts,
      parserState,
      gptDiagnostics,
      timing,
      highWater,
      cycles,
      trace,
      mutationRevision: fields.mutationRevision,
    });
    const factBytes = canonicalBytes(gptDiagnostics);
    const nonDiagnostics = Object.freeze({
      ...result,
      gptDiagnostics: Object.freeze({
        facts: Object.freeze([]),
        overflowCount: 0,
        dropCount: 0,
      }),
    });
    const nonDiagnosticsBytes = canonicalBytes(nonDiagnostics);
    const totalBytes = canonicalBytes(result);
    if (
      factBytes === undefined ||
      nonDiagnosticsBytes === undefined ||
      totalBytes === undefined ||
      factBytes > MAX_GPT_FACT_BYTES ||
      nonDiagnosticsBytes > MAX_FIRST_DISPLAY_NON_DIAGNOSTICS_BYTES ||
      totalBytes > MAX_FIRST_DISPLAY_HANDOFF_BYTES
    ) {
      return undefined;
    }
    return result;
  } catch {
    return undefined;
  }
}

/** Mint a closure-private, release/generation-bound one-use object-identity capsule. */
export function createFirstDisplayOwnershipCapsuleV1<T extends object>(
  releaseId: string,
  generation: number,
  identities: readonly T[]
): FirstDisplayOwnershipCapsuleV1<T> | undefined {
  if (
    !HASH.test(releaseId) ||
    !isU32(generation, false) ||
    identities.length > MAX_FIRST_DISPLAY_SLOTS * 2
  ) {
    return undefined;
  }
  const accepted = [...identities];
  if (
    accepted.some((identity) => typeof identity !== 'object' || identity === null) ||
    new Set(accepted).size !== accepted.length
  )
    return undefined;
  let live: T[] | undefined = accepted;
  return Object.freeze({
    releaseId,
    generation,
    consume: (candidateReleaseId: string, candidateGeneration: number) => {
      if (!live || candidateReleaseId !== releaseId || candidateGeneration !== generation) {
        return undefined;
      }
      const result = Object.freeze(live);
      live = undefined;
      return result;
    },
    clear: () => {
      live = undefined;
    },
  });
}
