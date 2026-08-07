import type { WinnerContext, WinnerContextAdmission } from '../kernel/sessions';

export const RENDER_RESERVATION_LIFETIME_MS = 15 * 60 * 1_000;
export const PREBID_ADMISSION_LEASE_MS = 10_000;

const RESERVATION_ID = /^r1_[A-Za-z0-9_-]{22}$/;
const ATTEMPT_ID = /^a1_[A-Za-z0-9_-]{22}$/;
const AUCTION_ID = /^[A-Za-z0-9._:-]{1,128}$/;
const MAX_RESERVATIONS = 320;
const textEncoder = new TextEncoder();

const objectFreezeIntrinsic = Object.freeze;
const regexpTestIntrinsic = RegExp.prototype.test;
const stringCharCodeAtIntrinsic = String.prototype.charCodeAt;
const textEncoderEncodeIntrinsic = TextEncoder.prototype.encode;
const mapDeleteIntrinsic = Map.prototype.delete;
const mapEntriesIntrinsic = Map.prototype.entries;
const mapGetIntrinsic = Map.prototype.get;
const mapSetIntrinsic = Map.prototype.set;
const mapValuesIntrinsic = Map.prototype.values;
const mapIteratorNextIntrinsic = Object.getPrototypeOf(new Map().values()).next as (
  this: IterableIterator<unknown>
) => IteratorResult<unknown>;
const mapEntryIteratorNextIntrinsic = Object.getPrototypeOf(new Map().entries()).next as (
  this: IterableIterator<unknown>
) => IteratorResult<unknown>;
const mapSizeGetter = Object.getOwnPropertyDescriptor(Map.prototype, 'size')?.get as (
  this: Map<unknown, unknown>
) => number;
const weakMapGetIntrinsic = WeakMap.prototype.get;
const weakMapSetIntrinsic = WeakMap.prototype.set;
const weakSetAddIntrinsic = WeakSet.prototype.add;
const weakSetHasIntrinsic = WeakSet.prototype.has;
const performanceNowIntrinsic = performance.now;
const reservationServices = new WeakSet<object>();

function mapValue<Key, Value>(map: Map<Key, Value>, key: Key): Value | undefined {
  return Reflect.apply(mapGetIntrinsic, map, [key]) as Value | undefined;
}

function setMapValue<Key, Value>(map: Map<Key, Value>, key: Key, value: Value): void {
  Reflect.apply(mapSetIntrinsic, map, [key, value]);
}

function deleteMapValue<Key, Value>(map: Map<Key, Value>, key: Key): boolean {
  return Reflect.apply(mapDeleteIntrinsic, map, [key]) as boolean;
}

function mapSize<Key, Value>(map: Map<Key, Value>): number {
  return Reflect.apply(mapSizeGetter, map, []) as number;
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

function addWeakSetValue<Value extends object>(set: WeakSet<Value>, value: Value): void {
  Reflect.apply(weakSetAddIntrinsic, set, [value]);
}

function hasWeakSetValue<Value extends object>(set: WeakSet<Value>, value: Value): boolean {
  return Reflect.apply(weakSetHasIntrinsic, set, [value]) as boolean;
}

function mapValueSnapshot<Key, Value>(map: Map<Key, Value>): Value[] {
  const iterator = Reflect.apply(mapValuesIntrinsic, map, []) as IterableIterator<Value>;
  const values: Value[] = [];
  while (true) {
    const step = Reflect.apply(mapIteratorNextIntrinsic, iterator, []) as IteratorResult<Value>;
    if (step.done) return values;
    values[values.length] = step.value;
  }
}

function entrySnapshot<Key, Value>(map: Map<Key, Value>): [Key, Value][] {
  const iterator = Reflect.apply(mapEntriesIntrinsic, map, []) as IterableIterator<[Key, Value]>;
  const values: [Key, Value][] = [];
  while (true) {
    const step = Reflect.apply(mapEntryIteratorNextIntrinsic, iterator, []) as IteratorResult<
      [Key, Value]
    >;
    if (step.done) return values;
    values[values.length] = step.value;
  }
}

function ownDataRecord(
  value: unknown,
  expectedKeys: readonly string[]
): Record<string, unknown> | undefined {
  try {
    if (typeof value !== 'object' || value === null || Array.isArray(value)) return undefined;
    if (Object.getPrototypeOf(value) !== Object.prototype) return undefined;
    if (Object.getOwnPropertySymbols(value).length !== 0) return undefined;
    const names = Object.getOwnPropertyNames(value);
    if (names.length !== expectedKeys.length) return undefined;
    for (let expectedIndex = 0; expectedIndex < expectedKeys.length; expectedIndex += 1) {
      const expected = expectedKeys[expectedIndex];
      let found = false;
      for (let nameIndex = 0; nameIndex < names.length; nameIndex += 1) {
        if (names[nameIndex] === expected) {
          found = true;
          break;
        }
      }
      if (!found) return undefined;
    }
    const output: Record<string, unknown> = Object.create(null) as Record<string, unknown>;
    for (let index = 0; index < names.length; index += 1) {
      const name = names[index];
      if (name === undefined) return undefined;
      const descriptor = Object.getOwnPropertyDescriptor(value, name);
      if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) return undefined;
      output[name] = descriptor.value;
    }
    return output;
  } catch {
    return undefined;
  }
}

function frozenResult<Value extends object>(value: Value): Readonly<Value> {
  return Reflect.apply(objectFreezeIntrinsic, Object, [value]) as Readonly<Value>;
}

function matches(pattern: RegExp, value: string): boolean {
  return Reflect.apply(regexpTestIntrinsic, pattern, [value]) as boolean;
}

function validBoundedString(value: unknown, maximumBytes: number): value is string {
  if (typeof value !== 'string' || value.length === 0) return false;
  for (let index = 0; index < value.length; index += 1) {
    const code = Reflect.apply(stringCharCodeAtIntrinsic, value, [index]) as number;
    if (code <= 0x1f || code === 0x7f) return false;
    if (code >= 0xd800 && code <= 0xdbff) {
      const next = Reflect.apply(stringCharCodeAtIntrinsic, value, [index + 1]) as number;
      if (!(next >= 0xdc00 && next <= 0xdfff)) return false;
      index += 1;
    } else if (code >= 0xdc00 && code <= 0xdfff) return false;
  }
  return (
    (Reflect.apply(textEncoderEncodeIntrinsic, textEncoder, [value]) as Uint8Array).length <=
    maximumBytes
  );
}

function copyTaggedRenderSource(value: unknown): ReservationRenderSource | undefined {
  try {
    if (typeof value !== 'object' || value === null || Array.isArray(value)) return undefined;
    if (Object.getPrototypeOf(value) !== Object.prototype) return undefined;
    if (Object.getOwnPropertySymbols(value).length !== 0) return undefined;
    const names = Object.getOwnPropertyNames(value);
    const output: Record<string, unknown> = {};
    for (let index = 0; index < names.length; index += 1) {
      const name = names[index];
      if (name === undefined) return undefined;
      const descriptor = Object.getOwnPropertyDescriptor(value, name);
      if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) return undefined;
      if (typeof descriptor.value !== 'string' && typeof descriptor.value !== 'number') {
        return undefined;
      }
      Object.defineProperty(output, name, {
        configurable: true,
        enumerable: true,
        value: descriptor.value,
        writable: true,
      });
    }
    if (
      (output.type !== 'aps' && output.type !== 'adm' && output.type !== 'cache') ||
      output.version !== 1
    ) {
      return undefined;
    }
    return frozenResult(output) as ReservationRenderSource;
  } catch {
    return undefined;
  }
}

export interface ReservationOwner {
  readonly generation: object;
  readonly isCurrent: () => boolean;
  readonly onDispose: (kind: string, callback: () => void) => void;
}

export interface ReservationAttempt {
  readonly id: string;
  readonly slot: string;
  readonly winnerContext: WinnerContext | undefined;
  readonly isCurrent: () => boolean;
  readonly prepareWinnerContext: (context: WinnerContext) => WinnerContextAdmission | undefined;
}

export interface ReservationServiceOptions {
  readonly now?: () => number;
  readonly prepareRenderSource: (candidate: unknown) => ReservationRenderSource | undefined;
}

/** Tagged browser render source copied after an injected exact parser accepts it. */
export type ReservationRenderSource = Readonly<{
  type: 'aps' | 'adm' | 'cache';
  version: 1;
}> &
  Readonly<object>;

export interface ReservationRegistrationInput {
  readonly reservationId: unknown;
  readonly slot: unknown;
  readonly navigation: ReservationOwner;
  readonly attemptId: unknown;
  readonly renderSource: unknown;
  readonly winnerContext: unknown;
}

export interface PrebidLeaseRegistrationInput {
  readonly reservationId: unknown;
  readonly slot: unknown;
  readonly navigation: ReservationOwner;
  readonly auctionId: unknown;
  readonly adUnitCode: unknown;
  readonly renderSource: unknown;
  readonly winnerContext: unknown;
  readonly prebidBid: unknown;
}

export type ReservationRegistrationResult =
  | Readonly<{ ok: true; expiresAt: number }>
  | Readonly<{
      ok: false;
      reason:
        | 'invalid_reservation_id'
        | 'invalid_slot'
        | 'invalid_render_source'
        | 'invalid_winner_context'
        | 'invalid_attempt'
        | 'prebid_cpm_mismatch'
        | 'reservation_collision'
        | 'reservation_not_live'
        | 'registry_full'
        | 'service_disposed'
        | 'stale_owner';
    }>;

export type ReservationTombstoneState =
  | 'aborted'
  | 'consumed'
  | 'disposed'
  | 'prebid_admission_failed'
  | 'prebid_contract_violation'
  | 'prebid_selection_timeout'
  | 'stale'
  | 'unselected';

export type ReservationState =
  'awaiting_prebid_selection' | 'renderable' | ReservationTombstoneState;

export type ReservationRecognition =
  | Readonly<{ recognized: false }>
  | Readonly<{ recognized: true; state: ReservationState; expiresAt: number }>;

export interface PromotePrebidSelectionInput {
  readonly reservationId: unknown;
  readonly auctionId: unknown;
  readonly adUnitCode: unknown;
  readonly navigationGeneration: object;
  readonly attempt: ReservationAttempt;
  readonly prebidBid: unknown;
}

export interface ReservationClaimInput {
  readonly reservationId: unknown;
  readonly slot: unknown;
  readonly navigationGeneration: object;
  readonly attempt: ReservationAttempt;
  readonly pucSource: unknown;
}

export interface ReservationTombstoneInput {
  readonly reservationId: unknown;
  readonly slot: unknown;
  readonly navigationGeneration: object;
  readonly attemptId: unknown;
}

export interface PrebidGroupOwnerInput {
  readonly auctionId: unknown;
  readonly adUnitCode: unknown;
  readonly navigationGeneration: object;
}

export interface PrebidLeaseOwnerInput extends PrebidGroupOwnerInput {
  readonly reservationId: unknown;
}

export type ReservationClaimResult =
  | Readonly<{ recognized: false }>
  | Readonly<{ recognized: true; claimed: false; state: ReservationState }>
  | Readonly<{
      recognized: true;
      claimed: true;
      pucSource: object;
      expiresAt: number;
    }>;

/** Exact lifecycle authority required to consume one successful claim object. */
export interface ReservationClaimExpectation {
  readonly attemptId: string;
  readonly slot: string;
  readonly navigationGeneration: object;
  readonly winnerContext: WinnerContext;
}

/** Source and winner context atomically recovered from one valid claim object. */
export interface ReservationClaimAdmission {
  readonly renderSource: ReservationRenderSource;
  readonly winnerContext: WinnerContext;
}

export interface ReservationServiceInventory {
  readonly clockFaulted: boolean;
  readonly disposed: boolean;
  readonly size: number;
  readonly live: number;
  readonly tombstones: number;
  readonly entriesWithRenderSource: number;
  readonly entriesWithWinnerContext: number;
  readonly entriesWithPucSource: number;
}

export interface ReservationService {
  readonly registerRender: (input: ReservationRegistrationInput) => ReservationRegistrationResult;
  readonly registerPrebidLease: (
    input: PrebidLeaseRegistrationInput
  ) => ReservationRegistrationResult;
  readonly promotePrebidSelection: (
    input: PromotePrebidSelectionInput
  ) => ReservationRegistrationResult;
  readonly claim: (input: ReservationClaimInput) => ReservationClaimResult;
  readonly consumeClaim: (
    claim: unknown,
    expectation: ReservationClaimExpectation
  ) => ReservationClaimAdmission | undefined;
  readonly recognize: (reservationId: unknown) => ReservationRecognition;
  readonly tombstone: (input: ReservationTombstoneInput, state: 'disposed' | 'stale') => boolean;
  readonly tombstonePrebidGroup: (
    input: PrebidGroupOwnerInput,
    state: 'aborted' | 'prebid_selection_timeout'
  ) => number;
  readonly tombstonePrebidLease: (
    input: PrebidLeaseOwnerInput,
    state: 'prebid_admission_failed' | 'prebid_contract_violation'
  ) => boolean;
  readonly dispose: () => void;
  readonly snapshotInventoryForTest: () => ReservationServiceInventory;
}

interface LiveReservation {
  readonly reservationId: string;
  readonly slot: string;
  readonly navigationGeneration: object;
  readonly renderSource: ReservationRenderSource;
  readonly winnerContext: WinnerContext;
  expiresAt: number;
  state: 'awaiting_prebid_selection' | 'renderable';
  attemptId: string | undefined;
  auctionId: string | undefined;
  adUnitCode: string | undefined;
  busy: boolean;
  pucSource: object | undefined;
}

interface ReservationTombstone {
  readonly expiresAt: number;
  readonly state: ReservationTombstoneState;
}

interface ClaimedAdmission {
  readonly attempt: ReservationAttempt;
  readonly attemptId: string;
  readonly expiresAt: number;
  readonly slot: string;
  readonly navigationGeneration: object;
  active: boolean;
  renderSource: ReservationRenderSource | undefined;
  winnerContext: WinnerContext | undefined;
}

type ReservationEntry = LiveReservation | ReservationTombstone;

interface OwnerSnapshot {
  readonly identity: object;
  readonly generation: object;
  readonly isCurrent: () => boolean;
  readonly onDispose: (kind: string, callback: () => void) => void;
  readonly readGeneration: () => object | undefined;
}

interface OwnerRegistration {
  readonly callbackState: OwnerCallbackState;
  readonly identity: object;
  ready: boolean;
}

interface OwnerCallbackState {
  active: boolean;
  disposed: boolean;
}

function liveEntry(entry: ReservationEntry): entry is LiveReservation {
  return entry.state === 'awaiting_prebid_selection' || entry.state === 'renderable';
}

function ownerDisposalState(entry: LiveReservation): 'aborted' | 'disposed' {
  return entry.state === 'awaiting_prebid_selection' ? 'aborted' : 'disposed';
}

function validRenderTombstoneState(value: unknown): value is 'disposed' | 'stale' {
  return value === 'disposed' || value === 'stale';
}

function validPrebidLeaseTombstoneState(
  value: unknown
): value is 'prebid_admission_failed' | 'prebid_contract_violation' {
  return value === 'prebid_admission_failed' || value === 'prebid_contract_violation';
}

function validPrebidGroupTombstoneState(
  value: unknown
): value is 'aborted' | 'prebid_selection_timeout' {
  return value === 'aborted' || value === 'prebid_selection_timeout';
}

function winnerContext(value: unknown): WinnerContext | undefined {
  const record = ownDataRecord(value, ['selectedCpm']);
  if (
    !record ||
    typeof record.selectedCpm !== 'number' ||
    !Number.isFinite(record.selectedCpm) ||
    record.selectedCpm < 0
  ) {
    return undefined;
  }
  return frozenResult({ selectedCpm: record.selectedCpm });
}

function prebidCpmMatches(value: unknown, context: WinnerContext): boolean {
  try {
    if (
      (typeof value !== 'object' && typeof value !== 'function') ||
      value === null ||
      !Object.isFrozen(value)
    ) {
      return false;
    }
    const descriptor = Object.getOwnPropertyDescriptor(value, 'cpm');
    return (
      !!descriptor && 'value' in descriptor && Object.is(descriptor.value, context.selectedCpm)
    );
  } catch {
    return false;
  }
}

function ownerSnapshot(value: unknown): OwnerSnapshot | undefined {
  try {
    if ((typeof value !== 'object' && typeof value !== 'function') || value === null) {
      return undefined;
    }
    const owner = value as ReservationOwner;
    const generation = owner.generation;
    const isCurrentMethod = owner.isCurrent;
    const onDisposeMethod = owner.onDispose;
    if (
      (typeof generation !== 'object' && typeof generation !== 'function') ||
      generation === null ||
      typeof isCurrentMethod !== 'function' ||
      typeof onDisposeMethod !== 'function'
    ) {
      return undefined;
    }
    return {
      identity: value as object,
      generation,
      isCurrent: () => Reflect.apply(isCurrentMethod, value, []) as boolean,
      onDispose: (kind, callback) => {
        Reflect.apply(onDisposeMethod, value, [kind, callback]);
      },
      readGeneration: () => {
        try {
          const current = (value as ReservationOwner).generation;
          return (typeof current === 'object' || typeof current === 'function') && current !== null
            ? current
            : undefined;
        } catch {
          return undefined;
        }
      },
    };
  } catch {
    return undefined;
  }
}

function currentOwner(owner: OwnerSnapshot): boolean {
  try {
    return owner.isCurrent() === true;
  } catch {
    return false;
  }
}

function currentAttempt(attempt: ReservationAttempt): boolean {
  try {
    return attempt.isCurrent() === true;
  } catch {
    return false;
  }
}

function attemptIdentity(attempt: ReservationAttempt): { id: string; slot: string } | undefined {
  try {
    const id = attempt.id;
    const slot = attempt.slot;
    if (!matches(ATTEMPT_ID, id) || !validBoundedString(slot, 256)) return undefined;
    return { id, slot };
  } catch {
    return undefined;
  }
}

function prepareWinnerAdmission(
  attempt: ReservationAttempt,
  context: WinnerContext
): WinnerContextAdmission | undefined {
  try {
    const prepare = attempt.prepareWinnerContext;
    if (typeof prepare !== 'function') return undefined;
    const admission = Reflect.apply(prepare, attempt, [context]) as unknown;
    if ((typeof admission !== 'object' && typeof admission !== 'function') || admission === null) {
      return undefined;
    }
    const commit = (admission as WinnerContextAdmission).commit;
    const rollback = (admission as WinnerContextAdmission).rollback;
    if (typeof commit !== 'function' || typeof rollback !== 'function') return undefined;
    return frozenResult({
      commit: (): boolean => Reflect.apply(commit, admission, []) === true,
      rollback: (): boolean => Reflect.apply(rollback, admission, []) === true,
    });
  } catch {
    return undefined;
  }
}

function commitWinnerAdmission(
  attempt: ReservationAttempt,
  admission: WinnerContextAdmission,
  context: WinnerContext
): boolean {
  try {
    return admission.commit() === true && attempt.winnerContext === context;
  } catch {
    return false;
  }
}

function rollbackWinnerAdmission(admission: WinnerContextAdmission | undefined): void {
  try {
    admission?.rollback();
  } catch {
    // The reservation is terminally suppressed even when a hostile rollback fails.
  }
}

function monotonicClock(source: () => number): () => number | undefined {
  let last = Number.NEGATIVE_INFINITY;
  return (): number | undefined => {
    try {
      const value = source();
      if (typeof value !== 'number' || !Number.isFinite(value) || value < 0 || value < last) {
        return undefined;
      }
      last = value;
      return value;
    } catch {
      return undefined;
    }
  };
}

function fixedExpiry(now: number, lifetime: number): number | undefined {
  const expiresAt = now + lifetime;
  return Number.isFinite(expiresAt) && expiresAt > now ? expiresAt : undefined;
}

function defaultNow(): number {
  return Reflect.apply(performanceNowIntrinsic, performance, []) as number;
}

/** Whether a candidate is one exact server-minted renderer reservation id. */
export function isRendererReservationId(value: unknown): value is string {
  return typeof value === 'string' && matches(RESERVATION_ID, value);
}

/** Whether a candidate is an exact service instance created by this module. */
export function isReservationService(value: unknown): value is ReservationService {
  try {
    return (
      ((typeof value === 'object' && value !== null) || typeof value === 'function') &&
      hasWeakSetValue(reservationServices, value)
    );
  } catch {
    return false;
  }
}

/** Construct the runtime-owned renderer reservation service. */
export function createReservationService(options: ReservationServiceOptions): ReservationService {
  let nowSource: () => number = defaultNow;
  let prepareRenderSource: ReservationServiceOptions['prepareRenderSource'] | undefined;
  let disposed = false;
  let clockFaulted = false;
  let storeFaulted = false;
  try {
    if (options.now !== undefined) {
      if (typeof options.now !== 'function') disposed = true;
      else nowSource = options.now;
    }
    if (typeof options.prepareRenderSource !== 'function') disposed = true;
    else prepareRenderSource = options.prepareRenderSource;
  } catch {
    disposed = true;
  }
  const readNow = monotonicClock(nowSource);
  const entries = new Map<string, ReservationEntry>();
  const ownerRegistrations = new WeakMap<object, OwnerRegistration>();
  const claimAdmissions = new Map<object, ClaimedAdmission>();

  const invalidateClaimAdmission = (claim: object, admission: ClaimedAdmission): void => {
    if (mapValue(claimAdmissions, claim) === admission) deleteMapValue(claimAdmissions, claim);
    admission.active = false;
    admission.renderSource = undefined;
    admission.winnerContext = undefined;
  };

  const invalidateClaimAdmissions = (predicate: (admission: ClaimedAdmission) => boolean): void => {
    const snapshot = entrySnapshot(claimAdmissions);
    for (let index = 0; index < snapshot.length; index += 1) {
      const pair = snapshot[index];
      if (pair && predicate(pair[1])) invalidateClaimAdmission(pair[0], pair[1]);
    }
  };

  const disposeStore = (): void => {
    disposed = true;
    invalidateClaimAdmissions(() => true);
    const snapshot = entrySnapshot(entries);
    for (let index = 0; index < snapshot.length; index += 1) {
      const pair = snapshot[index];
      if (pair) deleteMapValue(entries, pair[0]);
    }
  };

  const prune = (now: number): void => {
    invalidateClaimAdmissions((admission) => admission.expiresAt <= now);
    const snapshot = entrySnapshot(entries);
    for (let index = 0; index < snapshot.length; index += 1) {
      const pair = snapshot[index];
      if (!pair) continue;
      const id = pair[0];
      const entry = pair[1];
      if (entry.expiresAt <= now) {
        if (mapValue(entries, id) === entry) deleteMapValue(entries, id);
      }
    }
  };

  const clock = (): number | undefined => {
    if (disposed || clockFaulted || storeFaulted) return undefined;
    const now = readNow();
    if (now === undefined) {
      if (mapSize(entries) === 0) disposeStore();
      else clockFaulted = true;
      return undefined;
    }
    prune(now);
    return now;
  };

  const refreshForLookup = (): boolean => {
    if (disposed) return false;
    if (clockFaulted || storeFaulted) return true;
    const now = clock();
    return now !== undefined || clockFaulted || storeFaulted;
  };

  const refreshForTerminalMutation = (): boolean => {
    if (disposed || storeFaulted) return false;
    if (clockFaulted) return true;
    const now = clock();
    return now !== undefined || clockFaulted;
  };

  const publishEntry = (
    reservationId: string,
    expected: ReservationEntry | undefined,
    next: ReservationEntry,
    allowClockFault = false
  ): boolean => {
    if (disposed || storeFaulted || (clockFaulted && !allowClockFault)) return false;
    let before: ReservationEntry | undefined;
    try {
      before = mapValue(entries, reservationId);
    } catch {
      storeFaulted = true;
      return false;
    }
    if (before !== expected) return false;
    try {
      setMapValue(entries, reservationId, next);
    } catch {
      // The captured operation may have applied the exact value before throwing.
    }
    let after: ReservationEntry | undefined;
    try {
      after = mapValue(entries, reservationId);
    } catch {
      storeFaulted = true;
      return false;
    }
    if (after === next) return true;
    storeFaulted = true;
    return false;
  };

  const replaceWithTombstone = (
    reservationId: string,
    expected: LiveReservation,
    state: ReservationTombstoneState
  ): boolean => {
    const tombstone = frozenResult({ expiresAt: expected.expiresAt, state });
    return publishEntry(reservationId, expected, tombstone, true);
  };

  const readOwnerRegistration = (
    generation: object
  ): Readonly<{ ok: true; value: OwnerRegistration | undefined }> | Readonly<{ ok: false }> => {
    try {
      return { ok: true, value: weakMapValue(ownerRegistrations, generation) };
    } catch {
      return { ok: false };
    }
  };

  const publishOwnerRegistration = (
    generation: object,
    registration: OwnerRegistration
  ): boolean => {
    try {
      setWeakMapValue(ownerRegistrations, generation, registration);
    } catch {
      // The captured operation may have applied the exact registration before throwing.
    }
    const current = readOwnerRegistration(generation);
    if (!current.ok || current.value !== registration) {
      registration.callbackState.active = false;
      return false;
    }
    return true;
  };

  const disposeOwnerEntries = (generation: object, state: OwnerCallbackState): void => {
    if (!state.active || state.disposed) return;
    state.disposed = true;
    invalidateClaimAdmissions((admission) => admission.navigationGeneration === generation);
    const snapshot = entrySnapshot(entries);
    for (let index = 0; index < snapshot.length; index += 1) {
      const pair = snapshot[index];
      if (!pair) continue;
      const entry = pair[1];
      if (liveEntry(entry) && entry.navigationGeneration === generation) {
        replaceWithTombstone(pair[0], entry, ownerDisposalState(entry));
      }
    }
  };

  const ensureOwnerRegistration = (owner: OwnerSnapshot): OwnerRegistration | undefined => {
    const initial = readOwnerRegistration(owner.generation);
    if (!initial.ok) return undefined;
    const existing = initial.value;
    if (existing) {
      if (existing.identity !== owner.identity) return undefined;
      const ownerIsCurrent = currentOwner(owner);
      const currentGeneration = owner.readGeneration();
      const current = readOwnerRegistration(owner.generation);
      return ownerIsCurrent &&
        currentGeneration === owner.generation &&
        current.ok &&
        current.value === existing &&
        existing.ready &&
        existing.callbackState.active &&
        !existing.callbackState.disposed
        ? existing
        : undefined;
    }

    const callbackState: OwnerCallbackState = {
      active: true,
      disposed: false,
    };
    const registration: OwnerRegistration = {
      callbackState,
      identity: owner.identity,
      ready: false,
    };
    const generation = owner.generation;
    if (!publishOwnerRegistration(generation, registration)) return undefined;
    try {
      owner.onDispose('reservation', () => disposeOwnerEntries(generation, callbackState));
    } catch {
      callbackState.disposed = true;
      return undefined;
    }
    const ownerIsCurrent = currentOwner(owner);
    const currentGeneration = owner.readGeneration();
    const current = readOwnerRegistration(generation);
    if (
      !ownerIsCurrent ||
      currentGeneration !== generation ||
      !current.ok ||
      current.value !== registration ||
      !callbackState.active ||
      callbackState.disposed
    ) {
      callbackState.disposed = true;
      return undefined;
    }
    registration.ready = true;
    return registration;
  };

  const currentOwnerRegistration = (owner: OwnerSnapshot, expected: OwnerRegistration): boolean => {
    const current = readOwnerRegistration(owner.generation);
    return (
      current.ok &&
      current.value === expected &&
      expected.ready &&
      expected.callbackState.active &&
      !expected.callbackState.disposed
    );
  };

  const failure = (
    reason: Extract<ReservationRegistrationResult, { ok: false }>['reason']
  ): ReservationRegistrationResult => frozenResult({ ok: false, reason });

  const register = (candidate: unknown, prebid: boolean): ReservationRegistrationResult => {
    const keys = prebid
      ? [
          'reservationId',
          'slot',
          'navigation',
          'auctionId',
          'adUnitCode',
          'renderSource',
          'winnerContext',
          'prebidBid',
        ]
      : ['reservationId', 'slot', 'navigation', 'attemptId', 'renderSource', 'winnerContext'];
    const input = ownDataRecord(candidate, keys);
    if (!input || !isRendererReservationId(input.reservationId)) {
      return failure('invalid_reservation_id');
    }
    if (!validBoundedString(input.slot, 256)) return failure('invalid_slot');
    const context = winnerContext(input.winnerContext);
    if (!context) return failure('invalid_winner_context');
    let renderSource: ReservationRenderSource | undefined;
    try {
      renderSource = copyTaggedRenderSource(prepareRenderSource?.(input.renderSource));
    } catch {
      renderSource = undefined;
    }
    if (!renderSource) return failure('invalid_render_source');
    const owner = ownerSnapshot(input.navigation);
    if (!owner || !currentOwner(owner)) return failure('stale_owner');

    let attemptId: string | undefined;
    let auctionId: string | undefined;
    let adUnitCode: string | undefined;
    if (prebid) {
      if (
        typeof input.auctionId !== 'string' ||
        !matches(AUCTION_ID, input.auctionId) ||
        !validBoundedString(input.adUnitCode, 256) ||
        input.adUnitCode !== input.slot ||
        !prebidCpmMatches(input.prebidBid, context)
      ) {
        return failure('prebid_cpm_mismatch');
      }
      auctionId = input.auctionId;
      adUnitCode = input.adUnitCode;
    } else {
      if (typeof input.attemptId !== 'string' || !matches(ATTEMPT_ID, input.attemptId)) {
        return failure('invalid_attempt');
      }
      attemptId = input.attemptId;
    }

    const now = clock();
    if (now === undefined) return failure('service_disposed');
    const expiresAt = fixedExpiry(
      now,
      prebid ? PREBID_ADMISSION_LEASE_MS : RENDER_RESERVATION_LIFETIME_MS
    );
    if (expiresAt === undefined) {
      disposeStore();
      return failure('service_disposed');
    }
    if (mapValue(entries, input.reservationId) !== undefined) {
      return failure('reservation_collision');
    }
    if (mapSize(entries) >= MAX_RESERVATIONS) return failure('registry_full');
    const entry: LiveReservation = {
      reservationId: input.reservationId,
      slot: input.slot,
      navigationGeneration: owner.generation,
      renderSource,
      winnerContext: context,
      expiresAt,
      state: prebid ? 'awaiting_prebid_selection' : 'renderable',
      attemptId,
      auctionId,
      adUnitCode,
      busy: false,
      pucSource: undefined,
    };
    const success = frozenResult({ ok: true as const, expiresAt: entry.expiresAt });
    const staleOwner = failure('stale_owner');
    const ownerRegistration = ensureOwnerRegistration(owner);
    if (!ownerRegistration) {
      const rejected = frozenResult({
        expiresAt: entry.expiresAt,
        state: ownerDisposalState(entry),
      });
      return publishEntry(input.reservationId, undefined, rejected)
        ? staleOwner
        : failure('service_disposed');
    }
    if (!publishEntry(input.reservationId, undefined, entry)) {
      return failure('service_disposed');
    }
    const ownerIsCurrent = currentOwner(owner);
    const currentGeneration = owner.readGeneration();
    const ownerRegistrationIsCurrent = currentOwnerRegistration(owner, ownerRegistration);
    const entryIsCurrent = mapValue(entries, input.reservationId) === entry;
    if (
      !ownerIsCurrent ||
      currentGeneration !== entry.navigationGeneration ||
      !ownerRegistrationIsCurrent ||
      !entryIsCurrent
    ) {
      replaceWithTombstone(input.reservationId, entry, ownerDisposalState(entry));
      return staleOwner;
    }
    return success;
  };

  const recognize = (reservationId: unknown): ReservationRecognition => {
    if (!refreshForLookup() || typeof reservationId !== 'string') {
      return frozenResult({ recognized: false });
    }
    const entry = mapValue(entries, reservationId);
    return entry
      ? frozenResult({ recognized: true, state: entry.state, expiresAt: entry.expiresAt })
      : frozenResult({ recognized: false });
  };

  const refusedClaim = (state: ReservationState): ReservationClaimResult =>
    frozenResult({ recognized: true as const, claimed: false as const, state });

  const service: ReservationService = {
    registerRender: (input) => register(input, false),
    registerPrebidLease: (input) => register(input, true),
    promotePrebidSelection(input): ReservationRegistrationResult {
      const fields = ownDataRecord(input, [
        'reservationId',
        'auctionId',
        'adUnitCode',
        'navigationGeneration',
        'attempt',
        'prebidBid',
      ]);
      const now = clock();
      if (now === undefined) return failure('service_disposed');
      if (!fields || typeof fields.reservationId !== 'string') {
        return failure('reservation_not_live');
      }
      const entry = mapValue(entries, fields.reservationId);
      if (
        !entry ||
        !liveEntry(entry) ||
        entry.state !== 'awaiting_prebid_selection' ||
        entry.busy ||
        fields.auctionId !== entry.auctionId ||
        fields.adUnitCode !== entry.adUnitCode ||
        fields.navigationGeneration !== entry.navigationGeneration ||
        !prebidCpmMatches(fields.prebidBid, entry.winnerContext)
      ) {
        return failure('reservation_not_live');
      }
      const promotedExpiry = fixedExpiry(now, RENDER_RESERVATION_LIFETIME_MS);
      if (promotedExpiry === undefined) {
        disposeStore();
        return failure('service_disposed');
      }
      const attempt = fields.attempt as ReservationAttempt;
      const identity = attemptIdentity(attempt);
      if (!identity || identity.slot !== entry.slot || !currentAttempt(attempt)) {
        return failure('invalid_attempt');
      }
      entry.busy = true;
      const admission = prepareWinnerAdmission(attempt, entry.winnerContext);
      const committed = admission
        ? commitWinnerAdmission(attempt, admission, entry.winnerContext)
        : false;
      const attemptIsCurrent = currentAttempt(attempt);
      if (!committed || !attemptIsCurrent || mapValue(entries, fields.reservationId) !== entry) {
        rollbackWinnerAdmission(admission);
        replaceWithTombstone(fields.reservationId, entry, 'stale');
        return failure('invalid_attempt');
      }
      const promoted: LiveReservation = {
        reservationId: entry.reservationId,
        slot: entry.slot,
        navigationGeneration: entry.navigationGeneration,
        renderSource: entry.renderSource,
        winnerContext: entry.winnerContext,
        expiresAt: promotedExpiry,
        state: 'renderable',
        attemptId: identity.id,
        auctionId: entry.auctionId,
        adUnitCode: entry.adUnitCode,
        busy: true,
        pucSource: undefined,
      };
      const success = frozenResult({ ok: true as const, expiresAt: promotedExpiry });
      if (!publishEntry(fields.reservationId, entry, promoted)) {
        rollbackWinnerAdmission(admission);
        replaceWithTombstone(fields.reservationId, entry, 'stale');
        return failure('service_disposed');
      }
      let losersSuppressed = true;
      const candidates = mapValueSnapshot(entries);
      for (let index = 0; index < candidates.length; index += 1) {
        const candidate = candidates[index];
        if (
          candidate &&
          candidate !== promoted &&
          liveEntry(candidate) &&
          candidate.state === 'awaiting_prebid_selection' &&
          candidate.auctionId === promoted.auctionId &&
          candidate.adUnitCode === promoted.adUnitCode &&
          candidate.navigationGeneration === promoted.navigationGeneration &&
          !replaceWithTombstone(candidate.reservationId, candidate, 'unselected')
        ) {
          losersSuppressed = false;
        }
      }
      if (!losersSuppressed || storeFaulted) {
        rollbackWinnerAdmission(admission);
        return failure('service_disposed');
      }
      promoted.busy = false;
      return success;
    },
    claim(input): ReservationClaimResult {
      const minimalId = (() => {
        try {
          return input.reservationId;
        } catch {
          return undefined;
        }
      })();
      if (!refreshForLookup() || typeof minimalId !== 'string') {
        return frozenResult({ recognized: false });
      }
      const entry = mapValue(entries, minimalId);
      if (!entry) return frozenResult({ recognized: false });
      if (!liveEntry(entry)) {
        return refusedClaim(entry.state);
      }
      if (clockFaulted || storeFaulted) {
        return refusedClaim(entry.state);
      }
      if (entry.busy) {
        return refusedClaim(entry.state);
      }
      if (entry.state === 'awaiting_prebid_selection') {
        replaceWithTombstone(minimalId, entry, 'prebid_contract_violation');
        return refusedClaim('prebid_contract_violation');
      }
      const fields = ownDataRecord(input, [
        'reservationId',
        'slot',
        'navigationGeneration',
        'attempt',
        'pucSource',
      ]);
      if (!fields) {
        return refusedClaim(entry.state);
      }
      const attempt = fields.attempt as ReservationAttempt;
      const identity = attemptIdentity(attempt);
      if (
        !identity ||
        fields.slot !== entry.slot ||
        fields.navigationGeneration !== entry.navigationGeneration ||
        identity.id !== entry.attemptId ||
        identity.slot !== entry.slot ||
        typeof fields.pucSource !== 'object' ||
        fields.pucSource === null ||
        !currentAttempt(attempt)
      ) {
        return refusedClaim(entry.state);
      }
      const result = frozenResult({
        recognized: true as const,
        claimed: true as const,
        pucSource: fields.pucSource,
        expiresAt: entry.expiresAt,
      });
      entry.busy = true;
      const admission = prepareWinnerAdmission(attempt, entry.winnerContext);
      const committed = admission
        ? commitWinnerAdmission(attempt, admission, entry.winnerContext)
        : false;
      const attemptIsCurrent = currentAttempt(attempt);
      if (!committed || !attemptIsCurrent || mapValue(entries, minimalId) !== entry) {
        rollbackWinnerAdmission(admission);
        replaceWithTombstone(minimalId, entry, 'stale');
        const replacement = mapValue(entries, minimalId);
        return refusedClaim(replacement?.state ?? 'stale');
      }
      if (!replaceWithTombstone(minimalId, entry, 'consumed')) {
        rollbackWinnerAdmission(admission);
        const replacement = mapValue(entries, minimalId);
        return refusedClaim(replacement?.state ?? 'stale');
      }
      try {
        const claimedAdmission: ClaimedAdmission = {
          attempt,
          attemptId: identity.id,
          expiresAt: entry.expiresAt,
          slot: identity.slot,
          navigationGeneration: entry.navigationGeneration,
          active: true,
          renderSource: entry.renderSource,
          winnerContext: entry.winnerContext,
        };
        setMapValue(claimAdmissions, result, claimedAdmission);
        if (mapValue(claimAdmissions, result) !== claimedAdmission) {
          throw new Error('claim admission publication failed');
        }
      } catch {
        storeFaulted = true;
        rollbackWinnerAdmission(admission);
        return refusedClaim('consumed');
      }
      return result;
    },
    consumeClaim(claim, expectation): ReservationClaimAdmission | undefined {
      const fields = ownDataRecord(expectation, [
        'attemptId',
        'slot',
        'navigationGeneration',
        'winnerContext',
      ]);
      if (!fields || (typeof claim !== 'object' && typeof claim !== 'function') || claim === null) {
        return undefined;
      }
      const admission = mapValue(claimAdmissions, claim);
      const now = clock();
      const currentIdentity = admission ? attemptIdentity(admission.attempt) : undefined;
      let currentContext: WinnerContext | undefined;
      try {
        currentContext = admission?.attempt.winnerContext;
      } catch {
        currentContext = undefined;
      }
      if (
        admission &&
        (!admission.active ||
          !admission.renderSource ||
          !admission.winnerContext ||
          now === undefined ||
          now >= admission.expiresAt ||
          !currentAttempt(admission.attempt) ||
          !currentIdentity ||
          currentIdentity.id !== admission.attemptId ||
          currentIdentity.slot !== admission.slot ||
          currentContext !== admission.winnerContext)
      ) {
        invalidateClaimAdmission(claim, admission);
        return undefined;
      }
      if (
        !admission ||
        fields.attemptId !== admission.attemptId ||
        fields.slot !== admission.slot ||
        fields.navigationGeneration !== admission.navigationGeneration ||
        fields.winnerContext !== admission.winnerContext
      ) {
        return undefined;
      }
      const renderSource = admission.renderSource;
      const winnerContext = admission.winnerContext;
      invalidateClaimAdmission(claim, admission);
      return renderSource && winnerContext
        ? frozenResult({ renderSource, winnerContext })
        : undefined;
    },
    recognize,
    tombstone(input, state): boolean {
      if (!validRenderTombstoneState(state)) return false;
      const fields = ownDataRecord(input, [
        'reservationId',
        'slot',
        'navigationGeneration',
        'attemptId',
      ]);
      if (!refreshForTerminalMutation() || !fields || typeof fields.reservationId !== 'string') {
        return false;
      }
      const entry = mapValue(entries, fields.reservationId);
      if (
        !entry ||
        !liveEntry(entry) ||
        entry.state !== 'renderable' ||
        fields.slot !== entry.slot ||
        fields.navigationGeneration !== entry.navigationGeneration ||
        fields.attemptId !== entry.attemptId
      ) {
        return false;
      }
      return replaceWithTombstone(fields.reservationId, entry, state);
    },
    tombstonePrebidLease(input, state): boolean {
      if (!validPrebidLeaseTombstoneState(state)) return false;
      const fields = ownDataRecord(input, [
        'reservationId',
        'auctionId',
        'adUnitCode',
        'navigationGeneration',
      ]);
      if (!refreshForTerminalMutation() || !fields || typeof fields.reservationId !== 'string') {
        return false;
      }
      const entry = mapValue(entries, fields.reservationId);
      if (
        !entry ||
        !liveEntry(entry) ||
        entry.state !== 'awaiting_prebid_selection' ||
        entry.busy ||
        entry.auctionId !== fields.auctionId ||
        entry.adUnitCode !== fields.adUnitCode ||
        entry.navigationGeneration !== fields.navigationGeneration
      ) {
        return false;
      }
      return replaceWithTombstone(fields.reservationId, entry, state);
    },
    tombstonePrebidGroup(input, state): number {
      if (!validPrebidGroupTombstoneState(state)) return 0;
      const fields = ownDataRecord(input, ['auctionId', 'adUnitCode', 'navigationGeneration']);
      if (!refreshForTerminalMutation() || !fields) {
        return 0;
      }
      let count = 0;
      const candidates = mapValueSnapshot(entries);
      for (let index = 0; index < candidates.length; index += 1) {
        const entry = candidates[index];
        if (
          entry &&
          liveEntry(entry) &&
          entry.state === 'awaiting_prebid_selection' &&
          entry.auctionId === fields.auctionId &&
          entry.adUnitCode === fields.adUnitCode &&
          entry.navigationGeneration === fields.navigationGeneration &&
          replaceWithTombstone(entry.reservationId, entry, state)
        ) {
          count += 1;
        }
      }
      return count;
    },
    dispose(): void {
      if (disposed) return;
      disposeStore();
    },
    snapshotInventoryForTest(): ReservationServiceInventory {
      let live = 0;
      let tombstones = 0;
      let entriesWithRenderSource = 0;
      let entriesWithWinnerContext = 0;
      let entriesWithPucSource = 0;
      const snapshot = mapValueSnapshot(entries);
      for (let index = 0; index < snapshot.length; index += 1) {
        const entry = snapshot[index];
        if (!entry) continue;
        if (liveEntry(entry)) {
          live += 1;
          entriesWithRenderSource += 1;
          entriesWithWinnerContext += 1;
          if (entry.pucSource !== undefined) entriesWithPucSource += 1;
        } else tombstones += 1;
      }
      return frozenResult({
        clockFaulted,
        disposed,
        size: mapSize(entries),
        live,
        tombstones,
        entriesWithRenderSource,
        entriesWithWinnerContext,
        entriesWithPucSource,
      });
    },
  };
  addWeakSetValue(reservationServices, service);
  return frozenResult(service);
}
