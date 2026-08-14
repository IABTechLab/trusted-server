import type { FirstDisplaySliceId } from '../kernel/release_catalog';

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
export const MAX_FIRST_DISPLAY_HANDOFF_BYTES = 8.5 * 1024 * 1024;

const MAX_U32 = 4_294_967_295;
const MAX_STRING_BYTES = 4096;
const MAX_PROPERTY_BYTES = 128;
const MAX_DATA_DEPTH = 16;
const MAX_DATA_NODES = 32_768;
const MAX_TARGETING = 32;
const MAX_FORMATS = 32;
const MAX_ALIASES = 32;
const MAX_FACTS = 512;
const HASH = /^[0-9a-f]{64}$/;
const CAPABILITY = /^[a-z][a-z0-9_]*(?:[._][a-z0-9_]+)*$/;
const FORBIDDEN_DATA_KEYS = new Set([
  'adm',
  'creative',
  'creativePayload',
  'descriptor',
  'listener',
  'timer',
  'observer',
  'messagePort',
  'windowProxy',
  'networkHandle',
]);
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
  readonly gptFacts: readonly unknown[];
  readonly gptFactOverflow: number;
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

function copyDataTree(value: unknown): unknown | undefined {
  const seen = new Set<object>();
  let nodes = 0;
  const copy = (candidate: unknown, depth: number): unknown => {
    nodes += 1;
    if (nodes > MAX_DATA_NODES || depth > MAX_DATA_DEPTH) throw new TypeError('data bound');
    if (candidate === null || typeof candidate === 'boolean') return candidate;
    if (typeof candidate === 'number') {
      if (!Number.isFinite(candidate)) throw new TypeError('nonfinite');
      return candidate;
    }
    if (typeof candidate === 'string') {
      if (utf8Length(candidate) > MAX_STRING_BYTES) throw new TypeError('string bound');
      return candidate;
    }
    if (typeof candidate !== 'object' || seen.has(candidate)) throw new TypeError('live data');
    seen.add(candidate);
    const array = exactArray(candidate, MAX_DATA_NODES);
    if (array) return Object.freeze(array.map((entry) => copy(entry, depth + 1)));
    if (Array.isArray(candidate)) throw new TypeError('invalid array');
    const prototype = Object.getPrototypeOf(candidate) as unknown;
    if (prototype !== Object.prototype && prototype !== null) throw new TypeError('prototype');
    const result: Record<string, unknown> = {};
    for (const key of Reflect.ownKeys(candidate)) {
      if (
        typeof key !== 'string' ||
        !boundedString(key, MAX_PROPERTY_BYTES) ||
        FORBIDDEN_DATA_KEYS.has(key)
      ) {
        throw new TypeError('invalid key');
      }
      const descriptor = Object.getOwnPropertyDescriptor(candidate, key);
      if (!descriptor?.enumerable || !('value' in descriptor)) throw new TypeError('accessor');
      result[key] = copy(descriptor.value, depth + 1);
    }
    return freezeRecord(result);
  };
  try {
    return copy(value, 0);
  } catch {
    return undefined;
  }
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
    (fields.gptToken !== null && !boundedString(fields.gptToken))
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
    !boundedString(fields.id) ||
    !boundedString(fields.slotId) ||
    !isU32(fields.ordinal, false) ||
    !['accepted', 'no_bid', 'failed', 'cancelled'].includes(fields.state as string) ||
    (fields.reason !== null && !boundedString(fields.reason))
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
    !boundedString(fields.value) ||
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
    !boundedString(fields.token)
  ) {
    return undefined;
  }
  return freezeRecord({ ...fields });
}

function snapshotParserState(value: unknown): Readonly<Record<string, unknown>> | undefined {
  const fields = exactRecord(value, ['sliceId', 'observations', 'values']);
  const observations = uniqueStrings(fields?.observations, 256);
  const entries = exactArray(fields?.values, 256);
  if (!fields || !snapshotSlices(['first_display', fields.sliceId]) || !observations || !entries) {
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
  if ((fields.terminalMs as number) > (fields.paintMs as number)) return undefined;
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
  if (!fields || !boundedString(fields.navigationAttemptPrefix)) return undefined;
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
    const recordFields = exactRecord(record, ['ordinal', 'state']);
    if (
      !recordFields ||
      !isU32(recordFields.ordinal, false) ||
      !['open', 'completed', 'retired'].includes(recordFields.state as string)
    ) {
      return undefined;
    }
    maximum = Math.max(maximum, recordFields.ordinal);
    normalized.push(freezeRecord({ ...recordFields }));
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
    const bindings = uniqueStrings(slotFields?.bindings, 10);
    if (
      !slotFields ||
      !boundedString(slotFields.slotId) ||
      !isU32(slotFields.impressions) ||
      !bindings
    ) {
      return undefined;
    }
    normalized.push(freezeRecord({ ...slotFields, bindings }));
  }
  return freezeRecord({
    nextSequence: fields.nextSequence,
    nextGlobalSlotOrdinal: fields.nextGlobalSlotOrdinal,
    slots: Object.freeze(normalized),
  });
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
      'gptFacts',
      'gptFactOverflow',
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
    const factValues = exactArray(fields.gptFacts, MAX_FACTS);
    const gptFacts = factValues ? copyDataTree(factValues) : undefined;
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
      !Array.isArray(gptFacts) ||
      !isU32(fields.gptFactOverflow) ||
      !timing ||
      !highWater ||
      !cycles ||
      !trace ||
      !isU32(fields.mutationRevision)
    ) {
      return undefined;
    }
    const slotIds = new Set(slots.map((slot) => slot.id as string));
    if (
      slotIds.size !== slots.length ||
      attempts.some((attempt) => !slotIds.has(attempt.slotId as string)) ||
      artifacts.some((artifact) => !slotIds.has(artifact.slotId as string)) ||
      cycles.some((cycle) => !slotIds.has(cycle.slotId as string))
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
      gptFacts,
      gptFactOverflow: fields.gptFactOverflow,
      timing,
      highWater,
      cycles,
      trace,
      mutationRevision: fields.mutationRevision,
    });
    const factBytes = canonicalBytes(gptFacts);
    const nonDiagnostics = Object.freeze({ ...result, gptFacts: Object.freeze([]) });
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
  if (accepted.some((identity) => typeof identity !== 'object' || identity === null))
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
