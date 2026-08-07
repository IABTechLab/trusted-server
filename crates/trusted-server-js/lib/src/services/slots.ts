import type {
  GoogletagAdapter,
  GoogletagFacade,
  GoogletagOperation,
  GoogletagReplacementCommitAdmission,
  GoogletagReplacementDefinition,
  GoogletagReplacementResult,
} from '../adapters/googletag';
import {
  GoogletagReplacementCandidateCollisionError,
  GoogletagReplacementError,
} from '../adapters/googletag';
import type { NavigationSession } from '../kernel/sessions';

import type { PreparedProjectionSlots, ProjectionSlotRegistry } from './projections';

/** Shared maximum across server-projected and programmatically admitted slots. */
export const MAX_ACTIVE_SLOT_RECORDS = 256;

const GPT_REQUEST_START_TIMEOUT_MS = 3_000;
const GPT_COMPLETION_TIMEOUT_MS = 10_000;
const MAX_PENDING_PUBLISHER_INTENTS = 64;
const MAX_SLOT_ALIASES = 256;
const MAX_PLACEMENT_QUARANTINE_KEYS = 2_048;

export type SlotSource = 'programmatic' | 'server';
export type GptSlotOwnership = 'publisher' | 'trusted_server';

/** Immutable registration input owned by one navigation. */
export interface SlotRegistration {
  readonly registeredSlotId: string;
  readonly source: SlotSource;
  readonly adUnitCode?: string;
  readonly domAliases?: readonly string[];
}

/** Public immutable view of a registered slot. */
export interface SlotRecord {
  readonly adUnitCode: string | undefined;
  readonly domAliases: readonly string[];
  readonly navigationGeneration: object;
  readonly ordinal: number;
  readonly registeredSlotId: string;
  readonly source: SlotSource;
}

export type SlotRegistrationFailure =
  'duplicate_slot' | 'invalid_slot_id' | 'registry_capacity' | 'slot_quarantined' | 'stale_owner';

export type SlotRegistrationResult =
  | Readonly<{ ok: true; records: readonly SlotRecord[] }>
  | Readonly<{ ok: false; reason: SlotRegistrationFailure }>;

/** Binding metadata required for safe TS-owned replacement. */
export interface GptSlotBinding {
  readonly definition?: GoogletagReplacementDefinition;
  readonly ownership: GptSlotOwnership;
  readonly slot: object;
}

export type GptSlotAdoptionResult = Readonly<
  | { ok: true }
  | {
      ok: false;
      reason:
        | 'gpt_object_collision'
        | 'gpt_request_failed'
        | 'slot_quarantined'
        | 'slot_unresolved'
        | 'stale_owner';
    }
>;

export type SlotRequestFailure =
  | 'cycle_unattributable'
  | 'gpt_completion_timeout'
  | 'gpt_request_failed'
  | 'gpt_request_timeout'
  | 'slot_quarantined'
  | 'slot_unresolved';

export type SlotRequestOutcome =
  | Readonly<{
      status: 'empty' | 'rendered';
      responseIdentifier?: string;
    }>
  | Readonly<{ status: 'failed'; reason: SlotRequestFailure }>
  | Readonly<{ status: 'cancelled'; reason: 'navigation_disposed' | 'superseded' }>;

export interface SlotRequestInput {
  readonly intentId: string;
  readonly navigationGeneration: object;
  readonly operation: 'display' | 'refresh';
  readonly registeredSlotId: string;
  readonly requestClass: string;
}

/** Refresh-only input accepted by one shared GPT SRA operation. */
export type SlotBatchRequestInput = Omit<SlotRequestInput, 'operation'> &
  Readonly<{ operation: 'refresh' }>;

export interface SlotRequestHandle {
  readonly status: 'active' | 'queued' | 'terminal';
  readonly result: Promise<SlotRequestOutcome>;
  readonly dispose: () => void;
}

export type GptEventType = 'slotRenderEnded' | 'slotRequested';

export interface SlotServiceInventory {
  readonly cycles: number;
  readonly intents: number;
  readonly physicalSlots: number;
  readonly records: number;
}

/** Runtime-owned slot registry and physical-cycle boundary. */
export interface SlotService {
  readonly activate: () => GoogletagOperation<void>;
  readonly adoptGptSlot: (
    navigationGeneration: object,
    registeredSlotId: string,
    binding: GptSlotBinding
  ) => GptSlotAdoptionResult;
  readonly dispose: () => void;
  readonly handleGptEvent: (type: GptEventType, event: unknown) => void;
  readonly prepareProjectionSlots: (
    owner: NavigationSession,
    slots: readonly string[]
  ) => PreparedProjectionSlots | undefined;
  readonly projectionRegistry: (owner: NavigationSession) => ProjectionSlotRegistry;
  readonly recordPublisherDestruction: (slot: object) => boolean;
  readonly recordPublisherIntent: (slot: object) => boolean;
  readonly registeredSlotIdsForTest: () => readonly string[];
  readonly register: (
    owner: NavigationSession,
    registrations: readonly SlotRegistration[]
  ) => SlotRegistrationResult;
  readonly request: (input: SlotRequestInput) => SlotRequestHandle;
  readonly requestBatch: (inputs: readonly SlotBatchRequestInput[]) => readonly SlotRequestHandle[];
  readonly resolveAdUnitCode: (adUnitCode: string) => SlotRecord | undefined;
  readonly resolveDomAlias: (alias: string) => SlotRecord | undefined;
  readonly resolveRegisteredSlot: (registeredSlotId: string) => SlotRecord | undefined;
  readonly snapshotForTest: () => SlotServiceInventory;
}

export interface SlotServiceOptions {
  readonly googletag: GoogletagAdapter;
  readonly now?: () => number;
}

interface NavigationState {
  disposed: boolean;
  nextOrdinal: number;
  readonly owner: NavigationSession;
  readonly records: Map<string, InternalSlotRecord>;
}

interface InternalSlotRecord {
  activeIntent: RequestIntent | undefined;
  physical: PhysicalSlot | undefined;
  queuedIntent: RequestIntent | undefined;
  readonly state: NavigationState;
  readonly view: SlotRecord;
}

type PhysicalSlotState = 'live' | 'quarantined' | 'retired';

interface PhysicalCycle {
  intent: RequestIntent | undefined;
  readonly kind: 'publisher' | 'trusted_server';
}

interface PhysicalSlot {
  activeCycle: PhysicalCycle | undefined;
  definition: GoogletagReplacementDefinition | undefined;
  lastResponseIdentifier: string | undefined;
  ownership: GptSlotOwnership;
  placementKeys: readonly string[];
  publisherIntentCount: number;
  quarantineReason: 'completion' | 'navigation' | 'request' | undefined;
  record: InternalSlotRecord | undefined;
  saturationOwner: boolean;
  readonly slot: object;
  state: PhysicalSlotState;
  destroyAttempted: boolean;
}

interface RequestIntent {
  completionTimer: ReturnType<typeof setTimeout> | undefined;
  readonly input: SlotRequestInput;
  invocation: { dispose(): void } | undefined;
  requestStartedAt: number | undefined;
  requestDeadlineAt: number | undefined;
  completionDeadlineAt: number | undefined;
  requestTimer: ReturnType<typeof setTimeout> | undefined;
  readonly resolve: (outcome: SlotRequestOutcome) => void;
  readonly result: Promise<SlotRequestOutcome>;
  record: InternalSlotRecord;
  state: 'active' | 'cycle' | 'queued' | 'terminal';
  terminal: boolean;
}

interface BindingSubscriptions {
  readonly release: () => void;
  readonly token: object;
}

interface BindingSubscriptionAdmission {
  readonly installed: boolean;
  readonly ownership: BindingSubscriptions;
}

const mapDeleteIntrinsic = Map.prototype.delete;
const mapGetIntrinsic = Map.prototype.get;
const mapSetIntrinsic = Map.prototype.set;
const mapValuesIntrinsic = Map.prototype.values;
const mapIteratorNextIntrinsic = Object.getPrototypeOf(new Map().values()).next as (
  this: IterableIterator<unknown>
) => IteratorResult<unknown>;
const mapSizeGetter = Object.getOwnPropertyDescriptor(Map.prototype, 'size')?.get as (
  this: Map<unknown, unknown>
) => number;
const setAddIntrinsic = Set.prototype.add;
const setDeleteIntrinsic = Set.prototype.delete;
const setHasIntrinsic = Set.prototype.has;
const setValuesIntrinsic = Set.prototype.values;
const setIteratorNextIntrinsic = Object.getPrototypeOf(new Set().values()).next as (
  this: IterableIterator<unknown>
) => IteratorResult<unknown>;
const setSizeGetter = Object.getOwnPropertyDescriptor(Set.prototype, 'size')?.get as (
  this: Set<unknown>
) => number;
const weakMapDeleteIntrinsic = WeakMap.prototype.delete;
const weakMapGetIntrinsic = WeakMap.prototype.get;
const weakMapSetIntrinsic = WeakMap.prototype.set;

function mapValue<Key, Value>(map: Map<Key, Value>, key: Key): Value | undefined {
  return Reflect.apply(mapGetIntrinsic, map, [key]) as Value | undefined;
}

function setMapValue<Key, Value>(map: Map<Key, Value>, key: Key, value: Value): void {
  Reflect.apply(mapSetIntrinsic, map, [key, value]);
}

function deleteMapValue<Key, Value>(map: Map<Key, Value>, key: Key): boolean {
  return Reflect.apply(mapDeleteIntrinsic, map, [key]) as boolean;
}

function mapValues<Key, Value>(map: Map<Key, Value>): IterableIterator<Value> {
  return Reflect.apply(mapValuesIntrinsic, map, []) as IterableIterator<Value>;
}

function mapValueSnapshot<Key, Value>(map: Map<Key, Value>): Value[] {
  const iterator = mapValues(map);
  const values: Value[] = [];
  while (true) {
    const step = Reflect.apply(mapIteratorNextIntrinsic, iterator, []) as IteratorResult<Value>;
    if (step.done) return values;
    values[values.length] = step.value;
  }
}

function mapSize(map: Map<unknown, unknown>): number {
  return Reflect.apply(mapSizeGetter, map, []) as number;
}

function addSetValue<Value>(set: Set<Value>, value: Value): void {
  Reflect.apply(setAddIntrinsic, set, [value]);
}

function deleteSetValue<Value>(set: Set<Value>, value: Value): boolean {
  return Reflect.apply(setDeleteIntrinsic, set, [value]) as boolean;
}

function setHasValue<Value>(set: Set<Value>, value: Value): boolean {
  return Reflect.apply(setHasIntrinsic, set, [value]) as boolean;
}

function setValues<Value>(set: Set<Value>): IterableIterator<Value> {
  return Reflect.apply(setValuesIntrinsic, set, []) as IterableIterator<Value>;
}

function setValueSnapshot<Value>(set: Set<Value>): Value[] {
  const iterator = setValues(set);
  const values: Value[] = [];
  while (true) {
    const step = Reflect.apply(setIteratorNextIntrinsic, iterator, []) as IteratorResult<Value>;
    if (step.done) return values;
    values[values.length] = step.value;
  }
}

function setSize(set: Set<unknown>): number {
  return Reflect.apply(setSizeGetter, set, []) as number;
}

function weakMapValue<Key extends object, Value>(
  map: WeakMap<Key, Value>,
  key: Key
): Value | undefined {
  return Reflect.apply(weakMapGetIntrinsic, map, [key]) as Value | undefined;
}

function setWeakMapValue<Key extends object, Value>(
  map: WeakMap<Key, Value>,
  key: Key,
  value: Value
): void {
  Reflect.apply(weakMapSetIntrinsic, map, [key, value]);
}

function deleteWeakMapValue<Key extends object, Value>(
  map: WeakMap<Key, Value>,
  key: Key
): boolean {
  return Reflect.apply(weakMapDeleteIntrinsic, map, [key]) as boolean;
}

function setIndexValue(
  index: Map<string, Set<InternalSlotRecord>>,
  key: string,
  record: InternalSlotRecord
): void {
  let records = mapValue(index, key);
  if (!records) {
    records = new Set<InternalSlotRecord>();
    try {
      setMapValue(index, key, records);
    } catch (error) {
      if (mapValue(index, key) === records) deleteMapValue(index, key);
      throw error;
    }
  }
  try {
    addSetValue(records, record);
  } catch (error) {
    if (setSize(records) === 0) deleteMapValue(index, key);
    throw error;
  }
}

function deleteIndexValue(
  index: Map<string, Set<InternalSlotRecord>>,
  key: string,
  record: InternalSlotRecord
): void {
  const records = mapValue(index, key);
  if (!records) return;
  deleteSetValue(records, record);
  if (setSize(records) === 0) deleteMapValue(index, key);
}

function resolveUnique(
  index: Map<string, Set<InternalSlotRecord>>,
  key: string
): SlotRecord | undefined {
  const records = mapValue(index, key);
  if (!records || setSize(records) !== 1) return undefined;
  const iterator = setValues(records);
  const first = Reflect.apply(
    setIteratorNextIntrinsic,
    iterator,
    []
  ) as IteratorResult<InternalSlotRecord>;
  return first.done ? undefined : first.value.view;
}

function validSlotIdentity(value: string): boolean {
  return (
    value.length > 0 &&
    new TextEncoder().encode(value).length <= 256 &&
    !/[\p{Cc}]/u.test(value) &&
    !/[\uD800-\uDFFF]/u.test(value)
  );
}

function frozenAliases(aliases: readonly string[] | undefined): readonly string[] | undefined {
  if (aliases === undefined) return Object.freeze([]);
  if (!Array.isArray(aliases) || aliases.length > MAX_SLOT_ALIASES) return undefined;
  const output: string[] = [];
  const seen = new Set<string>();
  for (const alias of aliases) {
    if (typeof alias !== 'string' || !validSlotIdentity(alias) || setHasValue(seen, alias)) {
      return undefined;
    }
    addSetValue(seen, alias);
    output[output.length] = alias;
  }
  return Object.freeze(output);
}

function ownData(event: unknown, key: PropertyKey): unknown {
  if (typeof event !== 'object' || event === null) return undefined;
  try {
    const descriptor = Object.getOwnPropertyDescriptor(event, key);
    return descriptor && 'value' in descriptor ? descriptor.value : undefined;
  } catch {
    return undefined;
  }
}

function copyReplacementSizes(sizes: unknown): unknown | undefined {
  try {
    if (!Array.isArray(sizes)) return undefined;
    const length = sizes.length;
    const copyPair = (value: unknown): readonly [number, number] | undefined => {
      if (!Array.isArray(value) || value.length !== 2) return undefined;
      const width = value[0] as unknown;
      const height = value[1] as unknown;
      if (
        typeof width !== 'number' ||
        !Number.isInteger(width) ||
        width < 1 ||
        width > 4_096 ||
        typeof height !== 'number' ||
        !Number.isInteger(height) ||
        height < 1 ||
        height > 4_096
      ) {
        return undefined;
      }
      return Object.freeze([width, height]);
    };
    const single = copyPair(sizes);
    if (single) return single;
    if (length === 0 || length > MAX_ACTIVE_SLOT_RECORDS) return undefined;
    const copied: Array<readonly [number, number]> = [];
    for (let index = 0; index < length; index += 1) {
      const pair = copyPair(sizes[index] as unknown);
      if (!pair) return undefined;
      copied[copied.length] = pair;
    }
    return Object.freeze(copied);
  } catch {
    return undefined;
  }
}

function snapshotReplacementDefinition(input: unknown): GoogletagReplacementDefinition | undefined {
  if (typeof input !== 'object' || input === null || Array.isArray(input)) return undefined;
  let adUnitPath: unknown;
  let elementId: unknown;
  let sizes: unknown;
  try {
    const external = input as {
      readonly adUnitPath?: unknown;
      readonly elementId?: unknown;
      readonly sizes?: unknown;
    };
    adUnitPath = external.adUnitPath;
    elementId = external.elementId;
    sizes = external.sizes;
  } catch {
    return undefined;
  }
  const copiedSizes = copyReplacementSizes(sizes);
  if (
    typeof adUnitPath !== 'string' ||
    !validSlotIdentity(adUnitPath) ||
    typeof elementId !== 'string' ||
    !validSlotIdentity(elementId) ||
    copiedSizes === undefined
  ) {
    return undefined;
  }
  return Object.freeze({ adUnitPath, elementId, sizes: copiedSizes });
}

const failed = (reason: SlotRequestFailure): SlotRequestOutcome =>
  Object.freeze({ status: 'failed' as const, reason });
const cancelled = (reason: 'navigation_disposed' | 'superseded'): SlotRequestOutcome =>
  Object.freeze({ status: 'cancelled' as const, reason });

function placementKeysFor(
  registeredSlotId: string,
  adUnitCode: string | undefined,
  aliases: readonly string[],
  definition?: GoogletagReplacementDefinition
): readonly string[] {
  const keys: string[] = [`registered:${registeredSlotId}`];
  if (adUnitCode !== undefined) keys[keys.length] = `ad-unit:${adUnitCode}`;
  for (let index = 0; index < aliases.length; index += 1) {
    const alias = aliases[index];
    if (alias !== undefined) keys[keys.length] = `dom:${alias}`;
  }
  if (definition) {
    keys[keys.length] = `path:${definition.adUnitPath}`;
    keys[keys.length] = `dom:${definition.elementId}`;
  }
  return Object.freeze(keys);
}

/** Construct the document-lifetime slot registry and physical GPT cycle service. */
export function createSlotService(options: SlotServiceOptions): SlotService {
  const navigationStates = new Map<object, NavigationState>();
  const registeredSlots = new Map<string, InternalSlotRecord>();
  const adUnitCodes = new Map<string, Set<InternalSlotRecord>>();
  const domAliases = new Map<string, Set<InternalSlotRecord>>();
  const physicalByObject = new WeakMap<object, PhysicalSlot>();
  const physicalSlots = new Set<PhysicalSlot>();
  const placementQuarantine = new Map<string, number>();
  const quarantinedKeysByPhysical = new WeakMap<object, readonly string[]>();
  const now = options.now ?? (() => performance.now());
  let placementQuarantineSaturated = false;
  let placementQuarantinePoisoned = false;
  let saturationOwnerCount = 0;
  let disposed = false;
  let deferInvocations = false;
  let activation: GoogletagOperation<void> | undefined;
  const subscriptionsByBinding = new WeakMap<object, BindingSubscriptions>();
  const bindingSubscriptions = new Set<BindingSubscriptions>();

  const hasPlacementQuarantine = (keys: readonly string[]): boolean => {
    if (placementQuarantineSaturated || placementQuarantinePoisoned) return true;
    for (let index = 0; index < keys.length; index += 1) {
      const key = keys[index];
      if (key !== undefined && mapValue(placementQuarantine, key) !== undefined) return true;
    }
    return false;
  };

  const markSaturationOwner = (physical: PhysicalSlot): void => {
    if (physical.saturationOwner) return;
    physical.saturationOwner = true;
    saturationOwnerCount += 1;
    placementQuarantineSaturated = true;
  };

  const quarantinePhysicalPlacement = (physical: PhysicalSlot): void => {
    if (weakMapValue(quarantinedKeysByPhysical, physical.slot)) return;
    let additionalKeys = 0;
    for (let index = 0; index < physical.placementKeys.length; index += 1) {
      const key = physical.placementKeys[index];
      if (key !== undefined && mapValue(placementQuarantine, key) === undefined)
        additionalKeys += 1;
    }
    if (mapSize(placementQuarantine) + additionalKeys > MAX_PLACEMENT_QUARANTINE_KEYS) {
      markSaturationOwner(physical);
      return;
    }
    const confirmedKeys: string[] = [];
    try {
      setWeakMapValue(quarantinedKeysByPhysical, physical.slot, confirmedKeys);
    } catch {
      if (weakMapValue(quarantinedKeysByPhysical, physical.slot) !== confirmedKeys) {
        placementQuarantinePoisoned = true;
        return;
      }
    }
    if (weakMapValue(quarantinedKeysByPhysical, physical.slot) !== confirmedKeys) {
      placementQuarantinePoisoned = true;
      return;
    }
    for (let index = 0; index < physical.placementKeys.length; index += 1) {
      const key = physical.placementKeys[index];
      if (key === undefined) continue;
      const previous = mapValue(placementQuarantine, key) ?? 0;
      let publicationThrew = false;
      try {
        setMapValue(placementQuarantine, key, previous + 1);
      } catch {
        publicationThrew = true;
      }
      const actual = mapValue(placementQuarantine, key) ?? 0;
      if (actual === previous + 1) {
        confirmedKeys[confirmedKeys.length] = key;
        if (!publicationThrew) continue;
      } else if (actual !== previous) {
        placementQuarantinePoisoned = true;
        return;
      }
      if (publicationThrew || actual === previous) {
        // A hostile Map implementation either rejected this increment or threw
        // after applying it. Retain only the increments whose ownership can be
        // proven and fail closed until this physical slot is released.
        markSaturationOwner(physical);
        return;
      }
    }
    if (weakMapValue(quarantinedKeysByPhysical, physical.slot) !== confirmedKeys) {
      placementQuarantinePoisoned = true;
      markSaturationOwner(physical);
    }
  };

  const releasePhysicalPlacement = (physical: PhysicalSlot): void => {
    if (physical.saturationOwner) {
      physical.saturationOwner = false;
      saturationOwnerCount -= 1;
      if (saturationOwnerCount === 0) placementQuarantineSaturated = false;
    }
    const keys = weakMapValue(quarantinedKeysByPhysical, physical.slot);
    if (!keys) return;
    deleteWeakMapValue(quarantinedKeysByPhysical, physical.slot);
    for (let index = 0; index < keys.length; index += 1) {
      const key = keys[index];
      if (key === undefined) continue;
      const count = mapValue(placementQuarantine, key);
      if (count === undefined) continue;
      if (count <= 1) deleteMapValue(placementQuarantine, key);
      else setMapValue(placementQuarantine, key, count - 1);
    }
  };

  const settle = (intent: RequestIntent, outcome: SlotRequestOutcome): void => {
    if (intent.terminal) return;
    intent.terminal = true;
    intent.state = 'terminal';
    if (intent.requestTimer !== undefined) clearTimeout(intent.requestTimer);
    if (intent.completionTimer !== undefined) clearTimeout(intent.completionTimer);
    intent.requestTimer = undefined;
    intent.completionTimer = undefined;
    intent.requestDeadlineAt = undefined;
    intent.completionDeadlineAt = undefined;
    intent.invocation?.dispose();
    intent.invocation = undefined;
    if (intent.record.activeIntent === intent) intent.record.activeIntent = undefined;
    if (intent.record.queuedIntent === intent) intent.record.queuedIntent = undefined;
    intent.resolve(outcome);
  };

  const failQueued = (record: InternalSlotRecord, reason: SlotRequestFailure): void => {
    const queued = record.queuedIntent;
    if (queued) settle(queued, failed(reason));
  };

  const advanceQueued = (record: InternalSlotRecord): void => {
    if (record.activeIntent || record.state.disposed || !record.state.owner.isCurrent()) return;
    const queued = record.queuedIntent;
    if (!queued) return;
    const physical = record.physical;
    if (!physical || physical.state !== 'live' || physical.activeCycle) return;
    record.queuedIntent = undefined;
    record.activeIntent = queued;
    queued.state = 'active';
    invokeIntent(record, queued);
  };

  const prepareReplacementCommit = (
    record: InternalSlotRecord,
    oldPhysical: PhysicalSlot,
    replacement: object
  ): GoogletagReplacementCommitAdmission => {
    if (
      replacement === oldPhysical.slot ||
      record.physical !== oldPhysical ||
      record.state.disposed ||
      !record.state.owner.isCurrent()
    ) {
      throw new Error('gpt_request_failed');
    }
    const existing = weakMapValue(physicalByObject, replacement);
    if (existing) throw new GoogletagReplacementCandidateCollisionError(replacement);
    const physical: PhysicalSlot = {
      activeCycle: undefined,
      definition: oldPhysical.definition,
      destroyAttempted: false,
      lastResponseIdentifier: undefined,
      ownership: 'trusted_server',
      placementKeys: oldPhysical.placementKeys,
      publisherIntentCount: 0,
      quarantineReason: undefined,
      record,
      saturationOwner: false,
      slot: replacement,
      state: 'live',
    };
    let committed = false;
    const rollback = (): void => {
      if (record.physical === physical) record.physical = oldPhysical;
      if (oldPhysical.record === undefined && record.physical === oldPhysical) {
        oldPhysical.record = record;
      }
      deleteSetValue(physicalSlots, physical);
      if (weakMapValue(physicalByObject, replacement) === physical) {
        deleteWeakMapValue(physicalByObject, replacement);
      }
      if (committed) addSetValue(physicalSlots, oldPhysical);
      committed = false;
    };
    return Object.freeze({
      commit: (): boolean => {
        if (
          committed ||
          record.physical !== oldPhysical ||
          record.state.disposed ||
          !record.state.owner.isCurrent()
        ) {
          return false;
        }
        setWeakMapValue(physicalByObject, replacement, physical);
        if (weakMapValue(physicalByObject, replacement) !== physical) return false;
        addSetValue(physicalSlots, physical);
        if (!setHasValue(physicalSlots, physical)) return false;
        if (record.state.disposed || !record.state.owner.isCurrent()) return false;
        record.physical = physical;
        oldPhysical.record = undefined;
        deleteSetValue(physicalSlots, oldPhysical);
        committed = true;
        return true;
      },
      rollback,
    });
  };

  const recoverRequestTimeout = (record: InternalSlotRecord, physical: PhysicalSlot): void => {
    if (physical.destroyAttempted) {
      physical.state = 'quarantined';
      failQueued(record, 'gpt_request_failed');
      return;
    }
    physical.state = 'retired';
    physical.quarantineReason = 'request';
    physical.destroyAttempted = true;
    quarantinePhysicalPlacement(physical);
    if (
      physical.ownership !== 'trusted_server' ||
      !physical.definition ||
      record.state.disposed ||
      !record.state.owner.isCurrent()
    ) {
      physical.state = 'quarantined';
      failQueued(record, 'gpt_request_failed');
      return;
    }
    let operation: GoogletagOperation<GoogletagReplacementResult>;
    try {
      operation = options.googletag.run((gpt) =>
        gpt.transactionalReplace(
          physical.slot,
          physical.definition,
          () => !record.state.disposed && record.state.owner.isCurrent(),
          (replacement) => prepareReplacementCommit(record, physical, replacement)
        )
      );
    } catch {
      physical.state = 'quarantined';
      failQueued(record, 'gpt_request_failed');
      return;
    }
    const detachDestroyedOld = (): void => {
      if (record.physical === physical) record.physical = undefined;
      physical.record = undefined;
      releasePhysicalPlacement(physical);
      deleteSetValue(physicalSlots, physical);
      if (weakMapValue(physicalByObject, physical.slot) === physical) {
        deleteWeakMapValue(physicalByObject, physical.slot);
      }
    };
    void operation.result.then(
      (result) => {
        if (result.status !== 'replaced') {
          detachDestroyedOld();
          failQueued(record, 'gpt_request_failed');
          return;
        }
        releasePhysicalPlacement(physical);
        if (weakMapValue(physicalByObject, physical.slot) === physical) {
          deleteWeakMapValue(physicalByObject, physical.slot);
        }
        deleteSetValue(physicalSlots, physical);
        advanceQueued(record);
      },
      (error: unknown) => {
        physical.state = 'quarantined';
        const replacementError = error instanceof GoogletagReplacementError ? error : undefined;
        const reusedOldIdentity = replacementError?.orphanedSlot === physical.slot;
        if (
          replacementError?.oldSlotDestroyed &&
          !replacementError.preserveOldQuarantine &&
          !reusedOldIdentity
        ) {
          detachDestroyedOld();
        }
        if (replacementError?.orphanedSlot && replacementError.orphanedSlot !== physical.slot) {
          const orphan: PhysicalSlot = {
            activeCycle: undefined,
            definition: physical.definition,
            destroyAttempted: true,
            lastResponseIdentifier: undefined,
            ownership: 'trusted_server',
            placementKeys: physical.placementKeys,
            publisherIntentCount: 0,
            quarantineReason: 'request',
            record: undefined,
            saturationOwner: false,
            slot: replacementError.orphanedSlot,
            state: 'quarantined',
          };
          try {
            setWeakMapValue(physicalByObject, orphan.slot, orphan);
            if (weakMapValue(physicalByObject, orphan.slot) !== orphan) {
              throw new Error('orphan publication failed');
            }
            quarantinePhysicalPlacement(orphan);
          } catch {
            placementQuarantinePoisoned = true;
          }
        }
        failQueued(record, 'gpt_request_failed');
      }
    );
  };

  const cancelIntent = (intent: RequestIntent): void => {
    if (intent.terminal) return;
    const wasInvoked = intent.requestStartedAt !== undefined;
    const physical = intent.record.physical;
    if (intent.state === 'cycle' && physical?.activeCycle?.intent === intent) {
      physical.activeCycle.intent = undefined;
      physical.state = 'quarantined';
      physical.quarantineReason = 'completion';
      settle(intent, cancelled('superseded'));
      return;
    }
    settle(intent, cancelled('superseded'));
    if (wasInvoked && physical) recoverRequestTimeout(intent.record, physical);
    else advanceQueued(intent.record);
  };

  const onRequestTimeout = (intent: RequestIntent): void => {
    if (intent.terminal || intent.state !== 'active') return;
    const deadline = intent.requestDeadlineAt;
    if (deadline !== undefined && now() < deadline) {
      intent.requestTimer = setTimeout(
        () => onRequestTimeout(intent),
        Math.max(1, deadline - now())
      );
      return;
    }
    const physical = intent.record.physical;
    settle(intent, failed('gpt_request_timeout'));
    if (!physical) {
      failQueued(intent.record, 'gpt_request_failed');
      return;
    }
    recoverRequestTimeout(intent.record, physical);
  };

  const onCompletionTimeout = (intent: RequestIntent): void => {
    if (intent.terminal || intent.state !== 'cycle') return;
    const deadline = intent.completionDeadlineAt;
    if (deadline !== undefined && now() < deadline) {
      intent.completionTimer = setTimeout(
        () => onCompletionTimeout(intent),
        Math.max(1, deadline - now())
      );
      return;
    }
    const physical = intent.record.physical;
    if (physical?.activeCycle?.intent === intent) {
      physical.activeCycle.intent = undefined;
      physical.state = 'quarantined';
      physical.quarantineReason = 'completion';
    }
    settle(intent, failed('gpt_completion_timeout'));
  };

  const armRequestDeadline = (intent: RequestIntent): void => {
    intent.requestStartedAt = now();
    intent.requestDeadlineAt = intent.requestStartedAt + GPT_REQUEST_START_TIMEOUT_MS;
    intent.completionDeadlineAt = intent.requestStartedAt + GPT_COMPLETION_TIMEOUT_MS;
    intent.requestTimer = setTimeout(() => onRequestTimeout(intent), GPT_REQUEST_START_TIMEOUT_MS);
  };

  const failExternalInvocation = (record: InternalSlotRecord, intent: RequestIntent): void => {
    if (intent.terminal) return;
    const physical = record.physical;
    if (physical?.activeCycle?.intent === intent) {
      physical.activeCycle.intent = undefined;
      physical.state = 'quarantined';
      physical.quarantineReason = 'completion';
      settle(intent, failed('gpt_request_failed'));
      return;
    }
    const wasInvoked = intent.requestStartedAt !== undefined;
    settle(intent, failed('gpt_request_failed'));
    if (wasInvoked && physical) recoverRequestTimeout(record, physical);
    else advanceQueued(record);
  };

  const ensureBindingSubscriptions = (
    gpt: Readonly<GoogletagFacade>
  ): BindingSubscriptionAdmission => {
    const token = gpt.bindingToken();
    const existing = weakMapValue(subscriptionsByBinding, token);
    if (existing) return { installed: false, ownership: existing };
    const releaseRequested = gpt.subscribe('slotRequested', (event) =>
      handleGptEvent('slotRequested', event)
    );
    let releaseRendered: (() => void) | undefined;
    try {
      releaseRendered = gpt.subscribe('slotRenderEnded', (event) =>
        handleGptEvent('slotRenderEnded', event)
      );
    } catch (error) {
      releaseRequested();
      throw error;
    }
    let active = true;
    const ownership: BindingSubscriptions = {
      token,
      release: (): void => {
        if (!active) return;
        active = false;
        if (weakMapValue(subscriptionsByBinding, token) === ownership) {
          deleteWeakMapValue(subscriptionsByBinding, token);
        }
        deleteSetValue(bindingSubscriptions, ownership);
        try {
          releaseRendered?.();
        } finally {
          releaseRequested();
        }
      },
    };
    try {
      setWeakMapValue(subscriptionsByBinding, token, ownership);
      addSetValue(bindingSubscriptions, ownership);
      if (disposed) ownership.release();
    } catch (error) {
      ownership.release();
      throw error;
    }
    return { installed: true, ownership };
  };

  const retireHistoricalSubscriptions = (current: BindingSubscriptions): void => {
    const historical = setValueSnapshot(bindingSubscriptions);
    for (let index = 0; index < historical.length; index += 1) {
      const subscription = historical[index];
      if (subscription && subscription !== current) subscription.release();
    }
  };

  function invokeExternalIntent(
    record: InternalSlotRecord,
    intent: RequestIntent,
    expectedBindingToken: object
  ): void {
    if (
      intent.terminal ||
      record.activeIntent !== intent ||
      record.state.disposed ||
      !record.state.owner.isCurrent()
    ) {
      settle(intent, cancelled('navigation_disposed'));
      return;
    }
    const physical = record.physical;
    if (!physical || physical.state !== 'live' || physical.activeCycle) {
      settle(
        intent,
        failed(
          physical?.state === 'quarantined'
            ? 'slot_quarantined'
            : physical?.activeCycle
              ? 'cycle_unattributable'
              : 'slot_unresolved'
        )
      );
      return;
    }
    try {
      const operation = options.googletag.run((gpt) => {
        if (gpt.bindingToken() !== expectedBindingToken) {
          throw new Error('GPT binding changed before invocation');
        }
        if (
          intent.terminal ||
          record.activeIntent !== intent ||
          record.physical !== physical ||
          record.state.disposed ||
          !record.state.owner.isCurrent()
        ) {
          return;
        }
        const state = gpt.serviceState();
        if (intent.input.operation === 'display' && state.initialLoadDisabled) {
          gpt.display(physical.slot);
          if (intent.terminal || physical.activeCycle) return;
          armRequestDeadline(intent);
          gpt.refresh([physical.slot], Object.freeze({ changeCorrelator: false }));
          return;
        }
        armRequestDeadline(intent);
        if (intent.input.operation === 'display') gpt.display(physical.slot);
        else gpt.refresh([physical.slot], Object.freeze({ changeCorrelator: false }));
      });
      intent.invocation = operation;
      if (intent.terminal) operation.dispose();
      void operation.result.then(
        () => undefined,
        () => {
          failExternalInvocation(record, intent);
        }
      );
    } catch {
      failExternalInvocation(record, intent);
    }
  }

  function invokeIntent(record: InternalSlotRecord, intent: RequestIntent): void {
    if (intent.terminal) return;
    let operation: GoogletagOperation<BindingSubscriptionAdmission>;
    let provisionalSubscriptions: BindingSubscriptionAdmission | undefined;
    try {
      operation = options.googletag.run((gpt) => {
        provisionalSubscriptions = ensureBindingSubscriptions(gpt);
        return provisionalSubscriptions;
      });
    } catch {
      if (provisionalSubscriptions?.installed) provisionalSubscriptions.ownership.release();
      failExternalInvocation(record, intent);
      return;
    }
    intent.invocation = operation;
    void operation.result.then(
      (subscriptions) => {
        if (intent.invocation === operation) intent.invocation = undefined;
        retireHistoricalSubscriptions(subscriptions.ownership);
        if (intent.terminal) return;
        invokeExternalIntent(record, intent, subscriptions.ownership.token);
      },
      () => {
        if (intent.invocation === operation) intent.invocation = undefined;
        if (provisionalSubscriptions?.installed) provisionalSubscriptions.ownership.release();
        failExternalInvocation(record, intent);
      }
    );
  }

  const handleGptEvent = (type: GptEventType, event: unknown): void => {
    const slot = ownData(event, 'slot');
    if ((typeof slot !== 'object' || slot === null) && typeof slot !== 'function') return;
    const physical = weakMapValue(physicalByObject, slot as object);
    if (!physical) return;

    if (type === 'slotRequested') {
      if (physical.state !== 'live' || physical.activeCycle) return;
      const record = physical.record;
      const intent = record?.activeIntent;
      if (physical.publisherIntentCount > 0) {
        physical.publisherIntentCount -= 1;
        if (intent && !intent.terminal) settle(intent, failed('cycle_unattributable'));
        physical.activeCycle = { intent: undefined, kind: 'publisher' };
        return;
      }
      if (
        intent &&
        !intent.terminal &&
        intent.state === 'active' &&
        intent.requestStartedAt !== undefined
      ) {
        if (now() > (intent.requestDeadlineAt ?? Number.NEGATIVE_INFINITY)) {
          onRequestTimeout(intent);
          return;
        }
        if (intent.requestTimer !== undefined) clearTimeout(intent.requestTimer);
        intent.requestTimer = undefined;
        intent.state = 'cycle';
        physical.activeCycle = { intent, kind: 'trusted_server' };
        intent.completionTimer = setTimeout(
          () => onCompletionTimeout(intent),
          Math.max(0, (intent.completionDeadlineAt ?? now()) - now())
        );
        return;
      }
      if (intent && !intent.terminal) settle(intent, failed('cycle_unattributable'));
      physical.activeCycle = { intent: undefined, kind: 'publisher' };
      return;
    }

    const responseIdentifierValue = ownData(event, 'responseIdentifier');
    const responseIdentifier =
      typeof responseIdentifierValue === 'string' ? responseIdentifierValue : undefined;
    if (
      responseIdentifier !== undefined &&
      responseIdentifier === physical.lastResponseIdentifier
    ) {
      return;
    }
    const cycle = physical.activeCycle;
    if (!cycle) return;
    const intent = cycle.intent;
    if (
      intent &&
      !intent.terminal &&
      now() > (intent.completionDeadlineAt ?? Number.NEGATIVE_INFINITY)
    ) {
      onCompletionTimeout(intent);
    }
    const isEmptyValue = ownData(event, 'isEmpty');
    if (isEmptyValue !== true && isEmptyValue !== false) return;
    if (responseIdentifier !== undefined) physical.lastResponseIdentifier = responseIdentifier;
    physical.activeCycle = undefined;
    if (intent && !intent.terminal) {
      settle(
        intent,
        Object.freeze({
          ...(responseIdentifier === undefined ? {} : { responseIdentifier }),
          status: isEmptyValue ? ('empty' as const) : ('rendered' as const),
        })
      );
      advanceQueued(intent.record);
      return;
    }
    if (physical.quarantineReason === 'completion' || physical.quarantineReason === 'navigation') {
      const quarantineReason = physical.quarantineReason;
      physical.quarantineReason = undefined;
      if (quarantineReason === 'navigation' && physical.ownership === 'publisher') {
        releasePhysicalPlacement(physical);
      }
      if (quarantineReason === 'completion' || physical.ownership === 'publisher') {
        physical.state = 'live';
      }
      if (physical.record && physical.state === 'live') advanceQueued(physical.record);
      else deleteSetValue(physicalSlots, physical);
    }
  };

  const retirePhysicalForNavigation = (physical: PhysicalSlot): void => {
    physical.record = undefined;
    if (physical.ownership === 'publisher') {
      if (physical.activeCycle) {
        physical.state = 'quarantined';
        physical.quarantineReason = 'navigation';
        quarantinePhysicalPlacement(physical);
      }
      deleteSetValue(physicalSlots, physical);
      return;
    }
    if (physical.destroyAttempted) {
      deleteSetValue(physicalSlots, physical);
      return;
    }
    physical.state = 'retired';
    physical.quarantineReason = 'navigation';
    physical.destroyAttempted = true;
    quarantinePhysicalPlacement(physical);
    deleteSetValue(physicalSlots, physical);
    let operation: GoogletagOperation<unknown> | undefined;
    try {
      operation = options.googletag.run((gpt) =>
        gpt.transactionalReplace(
          physical.slot,
          undefined,
          () => false,
          () => {
            throw new Error('destroy-only replacement cannot commit');
          }
        )
      );
      void operation.result.then(
        () => {
          releasePhysicalPlacement(physical);
          if (weakMapValue(physicalByObject, physical.slot) === physical) {
            deleteWeakMapValue(physicalByObject, physical.slot);
          }
        },
        () => undefined
      );
    } catch {
      operation?.dispose();
    }
  };

  const disposeNavigationState = (state: NavigationState): void => {
    if (state.disposed) return;
    state.disposed = true;
    const records = mapValueSnapshot(state.records);
    for (const record of records) {
      const active = record.activeIntent;
      const queued = record.queuedIntent;
      if (active) settle(active, cancelled('navigation_disposed'));
      if (queued) settle(queued, cancelled('navigation_disposed'));
      const physical = record.physical;
      if (physical) retirePhysicalForNavigation(physical);
      record.physical = undefined;
      deleteMapValue(registeredSlots, record.view.registeredSlotId);
      if (record.view.adUnitCode !== undefined) {
        deleteIndexValue(adUnitCodes, record.view.adUnitCode, record);
      }
      for (const alias of record.view.domAliases) deleteIndexValue(domAliases, alias, record);
      deleteMapValue(state.records, record.view.registeredSlotId);
    }
    deleteMapValue(navigationStates, state.owner.generation);
  };

  const stateForOwner = (owner: NavigationSession): NavigationState | undefined => {
    if (disposed || !owner.isCurrent()) return undefined;
    const existing = mapValue(navigationStates, owner.generation);
    if (existing) return existing.disposed ? undefined : existing;
    const state: NavigationState = {
      disposed: false,
      nextOrdinal: 0,
      owner,
      records: new Map(),
    };
    let disposerInstalled = false;
    try {
      owner.onDispose('slot-records', () => disposeNavigationState(state));
      disposerInstalled = true;
      if (!owner.isCurrent() || state.disposed) return undefined;
      setMapValue(navigationStates, owner.generation, state);
      if (!owner.isCurrent() || state.disposed) {
        deleteMapValue(navigationStates, owner.generation);
        return undefined;
      }
      return state;
    } catch (error) {
      if (disposerInstalled) disposeNavigationState(state);
      throw error;
    }
  };

  const register = (
    owner: NavigationSession,
    registrations: readonly SlotRegistration[]
  ): SlotRegistrationResult => {
    if (disposed || !owner.isCurrent()) return Object.freeze({ ok: false, reason: 'stale_owner' });
    if (!Array.isArray(registrations)) {
      return Object.freeze({ ok: false, reason: 'invalid_slot_id' });
    }
    const prepared: Array<{
      readonly adUnitCode: string | undefined;
      readonly aliases: readonly string[];
      readonly id: string;
      readonly placementKeys: readonly string[];
      readonly source: SlotSource;
    }> = [];
    const ids = new Set<string>();
    for (const registration of registrations) {
      if (typeof registration !== 'object' || registration === null) {
        return Object.freeze({ ok: false, reason: 'invalid_slot_id' });
      }
      const id = registration.registeredSlotId;
      const source = registration.source;
      const adUnitCode = registration.adUnitCode;
      const aliases = frozenAliases(registration.domAliases);
      if (
        typeof id !== 'string' ||
        !validSlotIdentity(id) ||
        (source !== 'server' && source !== 'programmatic') ||
        (adUnitCode !== undefined &&
          (typeof adUnitCode !== 'string' || !validSlotIdentity(adUnitCode))) ||
        aliases === undefined
      ) {
        return Object.freeze({ ok: false, reason: 'invalid_slot_id' });
      }
      if (setHasValue(ids, id) || mapValue(registeredSlots, id)) {
        return Object.freeze({ ok: false, reason: 'duplicate_slot' });
      }
      const registrationPlacementKeys = placementKeysFor(id, adUnitCode, aliases);
      if (hasPlacementQuarantine(registrationPlacementKeys)) {
        return Object.freeze({ ok: false, reason: 'slot_quarantined' });
      }
      addSetValue(ids, id);
      prepared[prepared.length] = {
        adUnitCode,
        aliases,
        id,
        placementKeys: registrationPlacementKeys,
        source,
      };
    }

    let state: NavigationState | undefined;
    try {
      state = stateForOwner(owner);
    } catch {
      return Object.freeze({ ok: false, reason: 'stale_owner' });
    }
    if (!state) return Object.freeze({ ok: false, reason: 'stale_owner' });
    if (mapSize(state.records) + prepared.length > MAX_ACTIVE_SLOT_RECORDS) {
      return Object.freeze({ ok: false, reason: 'registry_capacity' });
    }

    const inserted: InternalSlotRecord[] = [];
    try {
      for (let index = 0; index < prepared.length; index += 1) {
        const registration = prepared[index];
        if (!registration || !owner.isCurrent() || state.disposed) throw new Error('stale owner');
        const ordinal = state.nextOrdinal + index;
        const view: SlotRecord = Object.freeze({
          adUnitCode: registration.adUnitCode,
          domAliases: registration.aliases,
          navigationGeneration: owner.generation,
          ordinal,
          registeredSlotId: registration.id,
          source: registration.source,
        });
        const record: InternalSlotRecord = {
          activeIntent: undefined,
          physical: undefined,
          queuedIntent: undefined,
          state,
          view,
        };
        inserted[inserted.length] = record;
        try {
          setMapValue(registeredSlots, registration.id, record);
          if (mapValue(registeredSlots, registration.id) !== record) {
            throw new Error('slot publication failed');
          }
          setMapValue(state.records, registration.id, record);
          if (mapValue(state.records, registration.id) !== record) {
            throw new Error('slot publication failed');
          }
          if (registration.adUnitCode !== undefined) {
            setIndexValue(adUnitCodes, registration.adUnitCode, record);
          }
          for (const alias of registration.aliases) setIndexValue(domAliases, alias, record);
        } catch (error) {
          deleteMapValue(registeredSlots, registration.id);
          deleteMapValue(state.records, registration.id);
          if (registration.adUnitCode !== undefined) {
            deleteIndexValue(adUnitCodes, registration.adUnitCode, record);
          }
          for (const alias of registration.aliases) deleteIndexValue(domAliases, alias, record);
          throw error;
        }
      }
      if (!owner.isCurrent() || state.disposed) throw new Error('stale owner');
      state.nextOrdinal += prepared.length;
      return Object.freeze({ ok: true, records: Object.freeze(inserted.map(({ view }) => view)) });
    } catch {
      for (let index = inserted.length - 1; index >= 0; index -= 1) {
        const record = inserted[index];
        if (!record) continue;
        deleteMapValue(registeredSlots, record.view.registeredSlotId);
        deleteMapValue(state.records, record.view.registeredSlotId);
        if (record.view.adUnitCode !== undefined) {
          deleteIndexValue(adUnitCodes, record.view.adUnitCode, record);
        }
        for (const alias of record.view.domAliases) deleteIndexValue(domAliases, alias, record);
      }
      return Object.freeze({ ok: false, reason: 'stale_owner' });
    }
  };

  const adoptGptSlot = (
    navigationGeneration: object,
    registeredSlotId: string,
    binding: GptSlotBinding
  ): GptSlotAdoptionResult => {
    const state = mapValue(navigationStates, navigationGeneration);
    if (!state || state.disposed || !state.owner.isCurrent()) {
      return Object.freeze({ ok: false, reason: 'stale_owner' });
    }
    const record = mapValue(state.records, registeredSlotId);
    if (!record) return Object.freeze({ ok: false, reason: 'slot_unresolved' });
    let slot: unknown;
    let ownership: unknown;
    let externalDefinition: unknown;
    try {
      slot = binding.slot;
      ownership = binding.ownership;
      externalDefinition = binding.definition;
    } catch {
      return Object.freeze({ ok: false, reason: 'gpt_request_failed' });
    }
    if (!state.owner.isCurrent() || state.disposed) {
      return Object.freeze({ ok: false, reason: 'stale_owner' });
    }
    if ((typeof slot !== 'object' || slot === null) && typeof slot !== 'function') {
      return Object.freeze({ ok: false, reason: 'gpt_request_failed' });
    }
    if (ownership !== 'publisher' && ownership !== 'trusted_server') {
      return Object.freeze({ ok: false, reason: 'gpt_request_failed' });
    }
    const definition =
      externalDefinition === undefined
        ? undefined
        : snapshotReplacementDefinition(externalDefinition);
    if (
      (externalDefinition !== undefined && definition === undefined) ||
      (ownership === 'trusted_server' && definition === undefined)
    ) {
      return Object.freeze({ ok: false, reason: 'gpt_request_failed' });
    }
    const slotObject = slot as object;
    let bindingPlacementKeys: readonly string[];
    try {
      bindingPlacementKeys = placementKeysFor(
        record.view.registeredSlotId,
        record.view.adUnitCode,
        record.view.domAliases,
        definition
      );
    } catch {
      return Object.freeze({ ok: false, reason: 'gpt_request_failed' });
    }
    if (hasPlacementQuarantine(bindingPlacementKeys)) {
      return Object.freeze({ ok: false, reason: 'slot_quarantined' });
    }
    const existing = weakMapValue(physicalByObject, slotObject);
    if (existing) {
      if (existing.state === 'quarantined') {
        return Object.freeze({ ok: false, reason: 'slot_quarantined' });
      }
      if (existing.state === 'retired') {
        return Object.freeze({ ok: false, reason: 'gpt_request_failed' });
      }
      if (existing.record && existing.record !== record) {
        return Object.freeze({ ok: false, reason: 'gpt_object_collision' });
      }
      if (record.physical && record.physical !== existing) {
        return Object.freeze({ ok: false, reason: 'gpt_object_collision' });
      }
      const wasStrong = setHasValue(physicalSlots, existing);
      const previousRecord = existing.record;
      const previousOwnership = existing.ownership;
      const previousDefinition = existing.definition;
      const previousPlacementKeys = existing.placementKeys;
      try {
        if (!wasStrong) addSetValue(physicalSlots, existing);
        if (!setHasValue(physicalSlots, existing)) throw new Error('physical publication failed');
        if (!state.owner.isCurrent() || state.disposed) throw new Error('stale owner');
        existing.record = record;
        existing.ownership = ownership;
        existing.definition = definition;
        existing.placementKeys = bindingPlacementKeys;
        record.physical = existing;
        return Object.freeze({ ok: true });
      } catch {
        if (record.physical === existing) record.physical = undefined;
        existing.record = previousRecord;
        existing.ownership = previousOwnership;
        existing.definition = previousDefinition;
        existing.placementKeys = previousPlacementKeys;
        if (!wasStrong) deleteSetValue(physicalSlots, existing);
        return Object.freeze({ ok: false, reason: 'stale_owner' });
      }
    }
    if (record.physical && record.physical.slot !== slotObject) {
      return Object.freeze({ ok: false, reason: 'gpt_object_collision' });
    }
    const physical: PhysicalSlot = {
      activeCycle: undefined,
      definition,
      destroyAttempted: false,
      lastResponseIdentifier: undefined,
      ownership,
      placementKeys: bindingPlacementKeys,
      publisherIntentCount: 0,
      quarantineReason: undefined,
      record,
      saturationOwner: false,
      slot: slotObject,
      state: 'live',
    };
    try {
      setWeakMapValue(physicalByObject, slotObject, physical);
      addSetValue(physicalSlots, physical);
      if (!state.owner.isCurrent() || state.disposed) throw new Error('stale owner');
      record.physical = physical;
      return Object.freeze({ ok: true });
    } catch {
      deleteSetValue(physicalSlots, physical);
      if (weakMapValue(physicalByObject, slotObject) === physical) {
        deleteWeakMapValue(physicalByObject, slotObject);
      }
      return Object.freeze({ ok: false, reason: 'stale_owner' });
    }
  };

  const request = (input: SlotRequestInput): SlotRequestHandle => {
    const state = mapValue(navigationStates, input.navigationGeneration);
    const record = state ? mapValue(state.records, input.registeredSlotId) : undefined;
    let resolve!: (outcome: SlotRequestOutcome) => void;
    const result = new Promise<SlotRequestOutcome>((resolveResult) => {
      resolve = resolveResult;
    });
    const placeholderRecord =
      record ?? ({ activeIntent: undefined, queuedIntent: undefined } as InternalSlotRecord);
    const intent: RequestIntent = {
      completionTimer: undefined,
      completionDeadlineAt: undefined,
      input,
      invocation: undefined,
      requestDeadlineAt: undefined,
      requestStartedAt: undefined,
      requestTimer: undefined,
      resolve,
      result,
      record: placeholderRecord,
      state: 'terminal',
      terminal: false,
    };
    const handle = Object.freeze({
      get status(): 'active' | 'queued' | 'terminal' {
        return intent.state === 'cycle' ? 'active' : intent.state;
      },
      result,
      dispose: (): void => cancelIntent(intent),
    });
    if (!state || state.disposed || !state.owner.isCurrent() || !record) {
      settle(intent, failed('slot_unresolved'));
      return handle;
    }
    intent.record = record;
    const physical = record.physical;
    if (!physical) {
      settle(intent, failed('slot_unresolved'));
      return handle;
    }
    if (physical.state === 'retired' || physical.quarantineReason === 'request') {
      settle(intent, failed('gpt_request_failed'));
      return handle;
    }
    if (physical.publisherIntentCount > 0) {
      settle(intent, failed('cycle_unattributable'));
      return handle;
    }
    if (physical.state === 'quarantined') {
      settle(
        intent,
        failed(
          physical.activeCycle?.kind === 'trusted_server' && physical.state === 'quarantined'
            ? 'slot_quarantined'
            : physical.activeCycle
              ? 'cycle_unattributable'
              : 'slot_quarantined'
        )
      );
      return handle;
    }
    if (record.activeIntent) {
      if (
        physical.activeCycle &&
        (physical.activeCycle.kind !== 'trusted_server' ||
          physical.activeCycle.intent !== record.activeIntent)
      ) {
        const active = record.activeIntent;
        settle(active, failed('cycle_unattributable'));
        if (record.queuedIntent) {
          settle(record.queuedIntent, failed('cycle_unattributable'));
        }
        settle(intent, failed('cycle_unattributable'));
        return handle;
      }
      if (record.activeIntent.input.requestClass !== input.requestClass) {
        const active = record.activeIntent;
        const activePhysical = record.physical;
        const hadStarted = active.requestStartedAt !== undefined;
        if (activePhysical?.activeCycle?.intent === active) {
          activePhysical.activeCycle.intent = undefined;
          activePhysical.state = 'quarantined';
          activePhysical.quarantineReason = 'completion';
        }
        settle(active, failed('cycle_unattributable'));
        if (record.queuedIntent) {
          settle(record.queuedIntent, failed('cycle_unattributable'));
        }
        settle(intent, failed('cycle_unattributable'));
        if (hadStarted && activePhysical && !activePhysical.activeCycle) {
          recoverRequestTimeout(record, activePhysical);
        }
        return handle;
      }
      intent.state = 'queued';
      const queued = record.queuedIntent;
      if (queued) {
        if (queued.input.requestClass !== input.requestClass) {
          settle(queued, failed('cycle_unattributable'));
          settle(intent, failed('cycle_unattributable'));
          return handle;
        }
        settle(queued, cancelled('superseded'));
      }
      record.queuedIntent = intent;
      return handle;
    }
    if (physical.activeCycle) {
      settle(intent, failed('cycle_unattributable'));
      return handle;
    }
    record.activeIntent = intent;
    intent.state = 'active';
    if (!deferInvocations) invokeIntent(record, intent);
    return handle;
  };

  const requestBatch = (inputs: readonly SlotBatchRequestInput[]): readonly SlotRequestHandle[] => {
    let inputCount: number;
    try {
      if (!Array.isArray(inputs)) return Object.freeze([]);
      inputCount = inputs.length;
    } catch {
      return Object.freeze([]);
    }
    if (inputCount === 0 || inputCount > MAX_ACTIVE_SLOT_RECORDS) {
      return Object.freeze([]);
    }
    const preparedInputs: SlotBatchRequestInput[] = [];
    const admittedIntents = new Set<string>();
    const admittedRecords = new Set<InternalSlotRecord>();
    const admittedPhysicalSlots = new Set<object>();
    let batchGeneration: object | undefined;
    try {
      for (let index = 0; index < inputCount; index += 1) {
        const input = inputs[index] as unknown;
        if (typeof input !== 'object' || input === null || Array.isArray(input)) {
          return Object.freeze([]);
        }
        const intentId = ownData(input, 'intentId');
        const navigationGeneration = ownData(input, 'navigationGeneration');
        const operation = ownData(input, 'operation');
        const registeredSlotId = ownData(input, 'registeredSlotId');
        const requestClass = ownData(input, 'requestClass');
        if (
          typeof intentId !== 'string' ||
          intentId.length === 0 ||
          typeof navigationGeneration !== 'object' ||
          navigationGeneration === null ||
          operation !== 'refresh' ||
          typeof registeredSlotId !== 'string' ||
          registeredSlotId.length === 0 ||
          typeof requestClass !== 'string' ||
          requestClass.length === 0 ||
          setHasValue(admittedIntents, intentId)
        ) {
          return Object.freeze([]);
        }
        if (batchGeneration === undefined) batchGeneration = navigationGeneration;
        else if (batchGeneration !== navigationGeneration) return Object.freeze([]);
        const state = mapValue(navigationStates, navigationGeneration);
        const record = state ? mapValue(state.records, registeredSlotId) : undefined;
        const physical = record?.physical;
        if (
          !state ||
          state.disposed ||
          !state.owner.isCurrent() ||
          !record ||
          record.activeIntent ||
          record.queuedIntent ||
          !physical ||
          physical.record !== record ||
          physical.state !== 'live' ||
          physical.activeCycle ||
          physical.publisherIntentCount > 0 ||
          setHasValue(admittedRecords, record) ||
          setHasValue(admittedPhysicalSlots, physical.slot)
        ) {
          return Object.freeze([]);
        }
        addSetValue(admittedIntents, intentId);
        addSetValue(admittedRecords, record);
        addSetValue(admittedPhysicalSlots, physical.slot);
        preparedInputs[preparedInputs.length] = Object.freeze({
          intentId,
          navigationGeneration,
          operation,
          registeredSlotId,
          requestClass,
        });
      }
    } catch {
      return Object.freeze([]);
    }
    deferInvocations = true;
    const handles: SlotRequestHandle[] = [];
    let admissionFailed = false;
    try {
      for (let index = 0; index < preparedInputs.length; index += 1) {
        const input = preparedInputs[index];
        if (!input) throw new Error('missing prepared SRA input');
        handles[handles.length] = request(input);
      }
    } catch {
      admissionFailed = true;
      for (let index = handles.length - 1; index >= 0; index -= 1) {
        try {
          handles[index]?.dispose();
        } catch {
          // Continue rolling back later admissions after one hostile owner callback.
        }
      }
    } finally {
      deferInvocations = false;
    }
    if (admissionFailed) return Object.freeze([]);
    const intents: RequestIntent[] = [];
    for (let index = 0; index < preparedInputs.length; index += 1) {
      const input = preparedInputs[index];
      if (!input) continue;
      const state = mapValue(navigationStates, input.navigationGeneration);
      const record = state ? mapValue(state.records, input.registeredSlotId) : undefined;
      const intent = record?.activeIntent;
      if (intent && intent.input === input && intent.state === 'active')
        intents[intents.length] = intent;
    }
    if (intents.length > 0) {
      const slots: object[] = [];
      for (let index = 0; index < intents.length; index += 1) {
        const intent = intents[index];
        if (!intent) continue;
        const physical = intent.record.physical;
        if (!physical || physical.state !== 'live' || physical.activeCycle) {
          settle(intent, failed('slot_unresolved'));
          continue;
        }
        slots[slots.length] = physical.slot;
      }
      if (slots.length === intents.length) {
        const allIntentsAdmitted = (): boolean => {
          for (let index = 0; index < intents.length; index += 1) {
            const intent = intents[index];
            const expectedSlot = slots[index];
            if (!intent || intent.terminal || intent.record.activeIntent !== intent) return false;
            const state = intent.record.state;
            if (state.disposed || !state.owner.isCurrent()) return false;
            const physical = intent.record.physical;
            if (
              !physical ||
              physical.state !== 'live' ||
              physical.activeCycle ||
              physical.slot !== expectedSlot
            ) {
              return false;
            }
          }
          return true;
        };
        const failInvalidAdmission = (): void => {
          for (let index = 0; index < intents.length; index += 1) {
            const intent = intents[index];
            if (!intent || intent.terminal) continue;
            const state = intent.record.state;
            settle(
              intent,
              state.disposed || !state.owner.isCurrent()
                ? cancelled('navigation_disposed')
                : failed('gpt_request_failed')
            );
          }
        };
        let subscriptionOperation: GoogletagOperation<BindingSubscriptionAdmission>;
        let provisionalSubscriptions: BindingSubscriptionAdmission | undefined;
        try {
          subscriptionOperation = options.googletag.run((gpt) => {
            provisionalSubscriptions = ensureBindingSubscriptions(gpt);
            return provisionalSubscriptions;
          });
          void subscriptionOperation.result.then(
            (subscriptions) => {
              retireHistoricalSubscriptions(subscriptions.ownership);
              if (!allIntentsAdmitted()) {
                failInvalidAdmission();
                return;
              }
              let operation: GoogletagOperation<unknown>;
              try {
                operation = options.googletag.run((gpt) => {
                  if (gpt.bindingToken() !== subscriptions.ownership.token) {
                    throw new Error('GPT binding changed before SRA invocation');
                  }
                  if (!allIntentsAdmitted()) {
                    throw new Error('SRA admission changed before invocation');
                  }
                  for (let index = 0; index < intents.length; index += 1) {
                    const intent = intents[index];
                    if (!intent) continue;
                    if (!intent.terminal) armRequestDeadline(intent);
                  }
                  gpt.refresh(slots, Object.freeze({ changeCorrelator: false }));
                });
              } catch {
                for (let index = 0; index < intents.length; index += 1) {
                  const intent = intents[index];
                  if (intent) failExternalInvocation(intent.record, intent);
                }
                return;
              }
              const liveIntents: RequestIntent[] = [];
              for (let index = 0; index < intents.length; index += 1) {
                const intent = intents[index];
                if (!intent) continue;
                if (!intent.terminal) liveIntents[liveIntents.length] = intent;
              }
              let remaining = liveIntents.length;
              for (let index = 0; index < liveIntents.length; index += 1) {
                const intent = liveIntents[index];
                if (!intent) continue;
                let released = false;
                intent.invocation = {
                  dispose: (): void => {
                    if (released) return;
                    released = true;
                    remaining -= 1;
                    if (remaining === 0) operation.dispose();
                  },
                };
              }
              if (remaining === 0) operation.dispose();
              void operation.result.then(
                () => undefined,
                () => {
                  for (let index = 0; index < intents.length; index += 1) {
                    const intent = intents[index];
                    if (intent) failExternalInvocation(intent.record, intent);
                  }
                }
              );
            },
            () => {
              if (provisionalSubscriptions?.installed) provisionalSubscriptions.ownership.release();
              for (let index = 0; index < intents.length; index += 1) {
                const intent = intents[index];
                if (intent) failExternalInvocation(intent.record, intent);
              }
            }
          );
        } catch {
          if (provisionalSubscriptions?.installed) provisionalSubscriptions.ownership.release();
          for (let index = 0; index < intents.length; index += 1) {
            const intent = intents[index];
            if (intent) failExternalInvocation(intent.record, intent);
          }
        }
      }
    }
    return Object.freeze(handles);
  };

  const service: SlotService = Object.freeze({
    activate: (): GoogletagOperation<void> => {
      if (activation) return activation;
      let subscriptions: BindingSubscriptionAdmission | undefined;
      const operation = options.googletag.run<void>((gpt) => {
        if (disposed) return;
        subscriptions = ensureBindingSubscriptions(gpt);
      });
      activation = operation;
      void operation.result.then(
        () => {
          if (subscriptions) retireHistoricalSubscriptions(subscriptions.ownership);
          if (activation === operation) activation = undefined;
        },
        () => {
          if (subscriptions?.installed) subscriptions.ownership.release();
          if (activation === operation) activation = undefined;
        }
      );
      return operation;
    },
    adoptGptSlot,
    dispose: (): void => {
      if (disposed) return;
      disposed = true;
      const states = mapValueSnapshot(navigationStates);
      for (let index = 0; index < states.length; index += 1) {
        const state = states[index];
        if (state) disposeNavigationState(state);
      }
      const subscriptions = setValueSnapshot(bindingSubscriptions);
      for (let index = subscriptions.length - 1; index >= 0; index -= 1) {
        try {
          subscriptions[index]?.release();
        } catch {
          // One adapter listener cleanup cannot escape service disposal.
        }
      }
      activation?.dispose();
    },
    handleGptEvent,
    prepareProjectionSlots: (
      owner: NavigationSession,
      slots: readonly string[]
    ): PreparedProjectionSlots | undefined => {
      if (!owner.isCurrent() || !Array.isArray(slots)) return undefined;
      const copied = Object.freeze([...slots]);
      let committedRecords: readonly SlotRecord[] | undefined;
      return Object.freeze({
        ownerGeneration: owner.generation,
        commit: (): boolean => {
          if (committedRecords) return false;
          const result = register(
            owner,
            copied.map((registeredSlotId) => ({ registeredSlotId, source: 'server' as const }))
          );
          if (!result.ok) return false;
          committedRecords = result.records;
          return true;
        },
        rollback: (): void => {
          if (!committedRecords) return;
          const state = mapValue(navigationStates, owner.generation);
          if (!state) return;
          for (const view of committedRecords) {
            const record = mapValue(state.records, view.registeredSlotId);
            if (!record || record.view !== view) continue;
            deleteMapValue(registeredSlots, view.registeredSlotId);
            deleteMapValue(state.records, view.registeredSlotId);
          }
          committedRecords = undefined;
        },
      });
    },
    projectionRegistry: (owner: NavigationSession): ProjectionSlotRegistry =>
      Object.freeze({
        prepareProjectionSlots: (
          ownerGeneration: object,
          slots: readonly string[],
          maximumActiveSlots: number
        ) => {
          if (
            ownerGeneration !== owner.generation ||
            maximumActiveSlots !== MAX_ACTIVE_SLOT_RECORDS
          ) {
            return undefined;
          }
          return service.prepareProjectionSlots(owner, slots);
        },
      }),
    recordPublisherDestruction: (slot: object): boolean => {
      const physical = weakMapValue(physicalByObject, slot);
      if (!physical) return false;
      const record = physical.record;
      const cycleIntent = physical.activeCycle?.intent;
      if (cycleIntent && !cycleIntent.terminal) settle(cycleIntent, failed('gpt_request_failed'));
      if (record?.activeIntent) settle(record.activeIntent, failed('gpt_request_failed'));
      if (record?.queuedIntent) settle(record.queuedIntent, failed('gpt_request_failed'));
      if (record?.physical === physical) record.physical = undefined;
      physical.record = undefined;
      physical.activeCycle = undefined;
      physical.publisherIntentCount = 0;
      physical.state = 'retired';
      releasePhysicalPlacement(physical);
      deleteSetValue(physicalSlots, physical);
      if (weakMapValue(physicalByObject, slot) === physical) {
        deleteWeakMapValue(physicalByObject, slot);
      }
      return true;
    },
    recordPublisherIntent: (slot: object): boolean => {
      const physical = weakMapValue(physicalByObject, slot);
      if (!physical || (physical.state !== 'live' && !physical.activeCycle)) return false;
      if (physical.publisherIntentCount >= MAX_PENDING_PUBLISHER_INTENTS) {
        physical.state = 'quarantined';
        physical.quarantineReason = 'request';
        quarantinePhysicalPlacement(physical);
        if (physical.record?.activeIntent) {
          settle(physical.record.activeIntent, failed('cycle_unattributable'));
        }
        if (physical.record?.queuedIntent) {
          settle(physical.record.queuedIntent, failed('cycle_unattributable'));
        }
        return false;
      }
      if (physical.record?.activeIntent) {
        settle(physical.record.activeIntent, failed('cycle_unattributable'));
      }
      if (physical.record?.queuedIntent) {
        settle(physical.record.queuedIntent, failed('cycle_unattributable'));
      }
      physical.publisherIntentCount += 1;
      if (physical.activeCycle?.kind === 'trusted_server') {
        physical.activeCycle = { intent: undefined, kind: 'publisher' };
        physical.state = 'quarantined';
        physical.quarantineReason = 'completion';
      }
      return true;
    },
    registeredSlotIdsForTest: (): readonly string[] => {
      const records = mapValueSnapshot(registeredSlots);
      records.sort((left, right) => left.view.ordinal - right.view.ordinal);
      return Object.freeze(records.map(({ view }) => view.registeredSlotId));
    },
    register,
    request,
    requestBatch,
    resolveAdUnitCode: (adUnitCode: string) => resolveUnique(adUnitCodes, adUnitCode),
    resolveDomAlias: (alias: string) => resolveUnique(domAliases, alias),
    resolveRegisteredSlot: (registeredSlotId: string) =>
      mapValue(registeredSlots, registeredSlotId)?.view,
    snapshotForTest: () => {
      let cycles = 0;
      let intents = 0;
      const physicalSnapshot = setValueSnapshot(physicalSlots);
      for (let index = 0; index < physicalSnapshot.length; index += 1) {
        if (physicalSnapshot[index]?.activeCycle) cycles += 1;
      }
      const stateSnapshot = mapValueSnapshot(navigationStates);
      for (let stateIndex = 0; stateIndex < stateSnapshot.length; stateIndex += 1) {
        const state = stateSnapshot[stateIndex];
        if (!state) continue;
        const recordSnapshot = mapValueSnapshot(state.records);
        for (let recordIndex = 0; recordIndex < recordSnapshot.length; recordIndex += 1) {
          const record = recordSnapshot[recordIndex];
          if (record?.activeIntent) intents += 1;
          if (record?.queuedIntent) intents += 1;
        }
      }
      return Object.freeze({
        cycles,
        intents,
        physicalSlots: setSize(physicalSlots),
        records: mapSize(registeredSlots),
      });
    },
  });

  return service;
}
