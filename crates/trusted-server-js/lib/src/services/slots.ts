import type {
  GoogletagAdapter,
  GoogletagFacade,
  GoogletagOperation,
  GoogletagReplacementDefinition,
} from '../adapters/googletag';
import type { NavigationSession } from '../kernel/sessions';

import type { PreparedProjectionSlots, ProjectionSlotRegistry } from './projections';

/** Shared maximum across server-projected and programmatically admitted slots. */
export const MAX_ACTIVE_SLOT_RECORDS = 256;

const GPT_REQUEST_START_TIMEOUT_MS = 3_000;
const GPT_COMPLETION_TIMEOUT_MS = 10_000;

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
  'duplicate_slot' | 'invalid_slot_id' | 'registry_capacity' | 'stale_owner';

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
  readonly recordPublisherIntent: (slot: object) => boolean;
  readonly registeredSlotIdsForTest: () => readonly string[];
  readonly register: (
    owner: NavigationSession,
    registrations: readonly SlotRegistration[]
  ) => SlotRegistrationResult;
  readonly request: (input: SlotRequestInput) => SlotRequestHandle;
  readonly requestBatch: (inputs: readonly SlotRequestInput[]) => readonly SlotRequestHandle[];
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
  publisherIntent: boolean;
  quarantineReason: 'completion' | 'navigation' | 'request' | undefined;
  record: InternalSlotRecord | undefined;
  readonly slot: object;
  state: PhysicalSlotState;
}

interface RequestIntent {
  completionTimer: ReturnType<typeof setTimeout> | undefined;
  readonly input: SlotRequestInput;
  invocation: GoogletagOperation<unknown> | undefined;
  requestStartedAt: number | undefined;
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
const mapSizeGetter = Object.getOwnPropertyDescriptor(Map.prototype, 'size')?.get as (
  this: Map<unknown, unknown>
) => number;
const setAddIntrinsic = Set.prototype.add;
const setDeleteIntrinsic = Set.prototype.delete;
const setHasIntrinsic = Set.prototype.has;
const setValuesIntrinsic = Set.prototype.values;
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
    setMapValue(index, key, records);
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
  const first = iterator.next();
  return first.done ? undefined : first.value.view;
}

function validSlotIdentity(value: string): boolean {
  return (
    value.length > 0 && new TextEncoder().encode(value).length <= 256 && !/[\p{Cc}]/u.test(value)
  );
}

function frozenAliases(aliases: readonly string[] | undefined): readonly string[] | undefined {
  if (aliases === undefined) return Object.freeze([]);
  if (!Array.isArray(aliases)) return undefined;
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

const failed = (reason: SlotRequestFailure): SlotRequestOutcome =>
  Object.freeze({ status: 'failed' as const, reason });
const cancelled = (reason: 'navigation_disposed' | 'superseded'): SlotRequestOutcome =>
  Object.freeze({ status: 'cancelled' as const, reason });

/** Construct the document-lifetime slot registry and physical GPT cycle service. */
export function createSlotService(options: SlotServiceOptions): SlotService {
  const navigationStates = new Map<object, NavigationState>();
  const registeredSlots = new Map<string, InternalSlotRecord>();
  const adUnitCodes = new Map<string, Set<InternalSlotRecord>>();
  const domAliases = new Map<string, Set<InternalSlotRecord>>();
  const physicalByObject = new WeakMap<object, PhysicalSlot>();
  const physicalSlots = new Set<PhysicalSlot>();
  const now = options.now ?? (() => Date.now());
  let disposed = false;
  let deferInvocations = false;
  let activation: GoogletagOperation<void> | undefined;
  const subscriptionsByBinding = new WeakMap<object, BindingSubscriptions>();
  const bindingSubscriptions = new Set<BindingSubscriptions>();

  const settle = (intent: RequestIntent, outcome: SlotRequestOutcome): void => {
    if (intent.terminal) return;
    intent.terminal = true;
    intent.state = 'terminal';
    if (intent.requestTimer !== undefined) clearTimeout(intent.requestTimer);
    if (intent.completionTimer !== undefined) clearTimeout(intent.completionTimer);
    intent.requestTimer = undefined;
    intent.completionTimer = undefined;
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

  const bindReplacement = (
    record: InternalSlotRecord,
    oldPhysical: PhysicalSlot,
    replacement: object
  ): boolean => {
    if (
      record.physical !== oldPhysical ||
      record.state.disposed ||
      !record.state.owner.isCurrent()
    ) {
      return false;
    }
    const existing = weakMapValue(physicalByObject, replacement);
    if (existing && existing !== oldPhysical) return false;
    const physical: PhysicalSlot = {
      activeCycle: undefined,
      definition: oldPhysical.definition,
      lastResponseIdentifier: undefined,
      ownership: 'trusted_server',
      publisherIntent: false,
      quarantineReason: undefined,
      record,
      slot: replacement,
      state: 'live',
    };
    try {
      setWeakMapValue(physicalByObject, replacement, physical);
      addSetValue(physicalSlots, physical);
      if (record.state.disposed || !record.state.owner.isCurrent()) throw new Error('stale owner');
      oldPhysical.record = undefined;
      record.physical = physical;
      deleteSetValue(physicalSlots, oldPhysical);
      return true;
    } catch {
      deleteSetValue(physicalSlots, physical);
      if (weakMapValue(physicalByObject, replacement) === physical) {
        deleteWeakMapValue(physicalByObject, replacement);
      }
      return false;
    }
  };

  const recoverRequestTimeout = (record: InternalSlotRecord, physical: PhysicalSlot): void => {
    physical.state = 'retired';
    physical.quarantineReason = 'request';
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
    let operation: GoogletagOperation<object | undefined>;
    try {
      operation = options.googletag.run((gpt) =>
        gpt.transactionalReplace(
          physical.slot,
          physical.definition,
          () => !record.state.disposed && record.state.owner.isCurrent()
        )
      );
    } catch {
      physical.state = 'quarantined';
      failQueued(record, 'gpt_request_failed');
      return;
    }
    void operation.result.then(
      (replacement) => {
        if (!replacement || !bindReplacement(record, physical, replacement)) {
          physical.state = 'quarantined';
          failQueued(record, 'gpt_request_failed');
          return;
        }
        advanceQueued(record);
      },
      () => {
        physical.state = 'quarantined';
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
  };

  const onRequestTimeout = (intent: RequestIntent): void => {
    if (intent.terminal || intent.state !== 'active') return;
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
    intent.requestTimer = setTimeout(() => onRequestTimeout(intent), GPT_REQUEST_START_TIMEOUT_MS);
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

  function invokeIntent(record: InternalSlotRecord, intent: RequestIntent): void {
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
        failed(physical?.state === 'quarantined' ? 'slot_quarantined' : 'slot_unresolved')
      );
      return;
    }
    let subscriptions: BindingSubscriptionAdmission | undefined;
    try {
      const operation = options.googletag.run((gpt) => {
        subscriptions = ensureBindingSubscriptions(gpt);
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
          if (subscriptions?.installed) subscriptions.ownership.release();
          if (intent.terminal) return;
          settle(intent, failed('gpt_request_failed'));
          advanceQueued(record);
        }
      );
    } catch {
      if (subscriptions?.installed) subscriptions.ownership.release();
      settle(intent, failed('gpt_request_failed'));
      advanceQueued(record);
    }
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
      if (physical.publisherIntent) {
        physical.publisherIntent = false;
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
        if (intent.requestTimer !== undefined) clearTimeout(intent.requestTimer);
        intent.requestTimer = undefined;
        intent.state = 'cycle';
        physical.activeCycle = { intent, kind: 'trusted_server' };
        const elapsed = Math.max(0, now() - intent.requestStartedAt);
        intent.completionTimer = setTimeout(
          () => onCompletionTimeout(intent),
          Math.max(0, GPT_COMPLETION_TIMEOUT_MS - elapsed)
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
    if (responseIdentifier !== undefined) physical.lastResponseIdentifier = responseIdentifier;
    physical.activeCycle = undefined;
    const intent = cycle.intent;
    if (intent && !intent.terminal) {
      const isEmpty = ownData(event, 'isEmpty') === true;
      settle(
        intent,
        Object.freeze({
          ...(responseIdentifier === undefined ? {} : { responseIdentifier }),
          status: isEmpty ? ('empty' as const) : ('rendered' as const),
        })
      );
      advanceQueued(intent.record);
      return;
    }
    if (physical.quarantineReason === 'completion' || physical.quarantineReason === 'navigation') {
      const quarantineReason = physical.quarantineReason;
      physical.quarantineReason = undefined;
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
      }
      return;
    }
    if (physical.state === 'retired') return;
    physical.state = 'retired';
    physical.quarantineReason = 'navigation';
    let operation: GoogletagOperation<unknown> | undefined;
    try {
      operation = options.googletag.run((gpt) =>
        gpt.transactionalReplace(physical.slot, undefined, () => false)
      );
      void operation.result.then(
        () => {
          if (!physical.activeCycle && !physical.record) deleteSetValue(physicalSlots, physical);
        },
        () => {
          if (!physical.activeCycle && !physical.record) deleteSetValue(physicalSlots, physical);
        }
      );
    } catch {
      operation?.dispose();
      if (!physical.activeCycle && !physical.record) deleteSetValue(physicalSlots, physical);
    }
  };

  const disposeNavigationState = (state: NavigationState): void => {
    if (state.disposed) return;
    state.disposed = true;
    const records = [...mapValues(state.records)];
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
      addSetValue(ids, id);
      prepared[prepared.length] = { adUnitCode, aliases, id, source };
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
        setMapValue(registeredSlots, registration.id, record);
        try {
          setMapValue(state.records, registration.id, record);
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
        inserted[inserted.length] = record;
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
    let definition: GoogletagReplacementDefinition | undefined;
    try {
      slot = binding.slot;
      ownership = binding.ownership;
      definition = binding.definition;
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
    const slotObject = slot as object;
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
      existing.record = record;
      existing.ownership = ownership;
      existing.definition = definition;
      record.physical = existing;
      return Object.freeze({ ok: true });
    }
    if (record.physical && record.physical.slot !== slotObject) {
      return Object.freeze({ ok: false, reason: 'gpt_object_collision' });
    }
    const physical: PhysicalSlot = {
      activeCycle: undefined,
      definition,
      lastResponseIdentifier: undefined,
      ownership,
      publisherIntent: false,
      quarantineReason: undefined,
      record,
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
      input,
      invocation: undefined,
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
    if (physical.state === 'quarantined') {
      settle(intent, failed('slot_quarantined'));
      return handle;
    }
    if (physical.publisherIntent) {
      settle(intent, failed('cycle_unattributable'));
      return handle;
    }
    if (record.activeIntent) {
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
    record.activeIntent = intent;
    intent.state = 'active';
    if (!deferInvocations) invokeIntent(record, intent);
    return handle;
  };

  const requestBatch = (inputs: readonly SlotRequestInput[]): readonly SlotRequestHandle[] => {
    if (!Array.isArray(inputs)) return Object.freeze([]);
    deferInvocations = true;
    let handles: SlotRequestHandle[];
    try {
      handles = inputs.map((input) => request(input));
    } finally {
      deferInvocations = false;
    }
    const intents: RequestIntent[] = [];
    for (const input of inputs) {
      const state = mapValue(navigationStates, input.navigationGeneration);
      const record = state ? mapValue(state.records, input.registeredSlotId) : undefined;
      const intent = record?.activeIntent;
      if (intent && intent.input === input && intent.state === 'active')
        intents[intents.length] = intent;
    }
    if (intents.length > 0) {
      const slots: object[] = [];
      for (const intent of intents) {
        const physical = intent.record.physical;
        if (!physical || physical.state !== 'live' || physical.activeCycle) {
          settle(intent, failed('slot_unresolved'));
          continue;
        }
        slots[slots.length] = physical.slot;
      }
      if (slots.length === intents.length) {
        let subscriptions: BindingSubscriptionAdmission | undefined;
        try {
          const operation = options.googletag.run((gpt) => {
            subscriptions = ensureBindingSubscriptions(gpt);
            for (const intent of intents) {
              if (!intent.terminal) armRequestDeadline(intent);
            }
            gpt.refresh(slots, Object.freeze({ changeCorrelator: false }));
          });
          for (const intent of intents) {
            intent.invocation = operation;
            if (intent.terminal) operation.dispose();
          }
          void operation.result.then(
            () => undefined,
            () => {
              if (subscriptions?.installed) subscriptions.ownership.release();
              for (const intent of intents) {
                if (!intent.terminal) settle(intent, failed('gpt_request_failed'));
              }
            }
          );
        } catch {
          if (subscriptions?.installed) subscriptions.ownership.release();
          for (const intent of intents) {
            if (!intent.terminal) settle(intent, failed('gpt_request_failed'));
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
      for (const state of [...mapValues(navigationStates)]) disposeNavigationState(state);
      const subscriptions = [...setValues(bindingSubscriptions)];
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
    recordPublisherIntent: (slot: object): boolean => {
      const physical = weakMapValue(physicalByObject, slot);
      if (!physical || physical.state !== 'live' || physical.activeCycle) return false;
      if (physical.record?.activeIntent) {
        settle(physical.record.activeIntent, failed('cycle_unattributable'));
      }
      if (physical.record?.queuedIntent) {
        settle(physical.record.queuedIntent, failed('cycle_unattributable'));
      }
      physical.publisherIntent = true;
      return true;
    },
    registeredSlotIdsForTest: (): readonly string[] => {
      const records = [...mapValues(registeredSlots)];
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
      for (const physical of setValues(physicalSlots)) {
        if (physical.activeCycle) cycles += 1;
      }
      for (const state of mapValues(navigationStates)) {
        for (const record of mapValues(state.records)) {
          if (record.activeIntent) intents += 1;
          if (record.queuedIntent) intents += 1;
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
