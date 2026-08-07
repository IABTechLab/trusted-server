import type { WinnerContext } from '../kernel/sessions';

export const RENDER_RESERVATION_LIFETIME_MS = 15 * 60 * 1_000;
export const PREBID_ADMISSION_LEASE_MS = 10_000;

const RESERVATION_ID = /^r1_[A-Za-z0-9_-]{22}$/;
const ATTEMPT_ID = /^a1_[A-Za-z0-9_-]{22}$/;
const AUCTION_ID = /^[A-Za-z0-9._:-]{1,128}$/;
const MAX_RESERVATIONS = 320;
const textEncoder = new TextEncoder();

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
const performanceNowIntrinsic = performance.now;

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
  return Object.freeze(value);
}

function validBoundedString(value: unknown, maximumBytes: number): value is string {
  if (typeof value !== 'string' || value.length === 0) return false;
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    if (code <= 0x1f || code === 0x7f) return false;
    if (code >= 0xd800 && code <= 0xdbff) {
      const next = value.charCodeAt(index + 1);
      if (!(next >= 0xdc00 && next <= 0xdfff)) return false;
      index += 1;
    } else if (code >= 0xdc00 && code <= 0xdfff) return false;
  }
  return textEncoder.encode(value).length <= maximumBytes;
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
    return Object.freeze(output) as ReservationRenderSource;
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
  readonly adoptWinnerContext: (context: WinnerContext) => boolean;
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
      renderSource: ReservationRenderSource;
      winnerContext: WinnerContext;
      pucSource: object;
      expiresAt: number;
    }>;

export interface ReservationServiceInventory {
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
  readonly ownerToken: object;
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

type ReservationEntry = LiveReservation | ReservationTombstone;

interface OwnerSnapshot {
  readonly generation: object;
  readonly isCurrent: () => boolean;
  readonly onDispose: (kind: string, callback: () => void) => void;
  readonly readGeneration: () => object | undefined;
}

function liveEntry(entry: ReservationEntry): entry is LiveReservation {
  return entry.state === 'awaiting_prebid_selection' || entry.state === 'renderable';
}

function ownerDisposalState(entry: LiveReservation): 'aborted' | 'disposed' {
  return entry.state === 'awaiting_prebid_selection' ? 'aborted' : 'disposed';
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
    if (!ATTEMPT_ID.test(id) || !validBoundedString(slot, 256)) return undefined;
    return { id, slot };
  } catch {
    return undefined;
  }
}

function monotonicClock(source: () => number): () => number | undefined {
  let last = Number.NEGATIVE_INFINITY;
  return (): number | undefined => {
    try {
      const value = source();
      if (typeof value !== 'number' || !Number.isFinite(value) || value < 0) return undefined;
      last = Math.max(last, value);
      return last;
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
  return typeof value === 'string' && RESERVATION_ID.test(value);
}

/** Construct the runtime-owned renderer reservation service. */
export function createReservationService(options: ReservationServiceOptions): ReservationService {
  let nowSource: () => number = defaultNow;
  let prepareRenderSource: ReservationServiceOptions['prepareRenderSource'] | undefined;
  let disposed = false;
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

  const disposeStore = (): void => {
    disposed = true;
    const snapshot = entrySnapshot(entries);
    for (let index = 0; index < snapshot.length; index += 1) {
      const pair = snapshot[index];
      if (pair) deleteMapValue(entries, pair[0]);
    }
  };

  const prune = (now: number): void => {
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
    if (disposed) return undefined;
    const now = readNow();
    if (now === undefined) {
      disposeStore();
      return undefined;
    }
    prune(now);
    return now;
  };

  const replaceWithTombstone = (
    reservationId: string,
    expected: LiveReservation,
    state: ReservationTombstoneState
  ): boolean => {
    if (mapValue(entries, reservationId) !== expected) return false;
    setMapValue(entries, reservationId, frozenResult({ expiresAt: expected.expiresAt, state }));
    return true;
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
        !AUCTION_ID.test(input.auctionId) ||
        !validBoundedString(input.adUnitCode, 256) ||
        input.adUnitCode !== input.slot ||
        !prebidCpmMatches(input.prebidBid, context)
      ) {
        return failure('prebid_cpm_mismatch');
      }
      auctionId = input.auctionId;
      adUnitCode = input.adUnitCode;
    } else {
      if (typeof input.attemptId !== 'string' || !ATTEMPT_ID.test(input.attemptId)) {
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
      ownerToken: Object.freeze({}),
      expiresAt,
      state: prebid ? 'awaiting_prebid_selection' : 'renderable',
      attemptId,
      auctionId,
      adUnitCode,
      busy: false,
      pucSource: undefined,
    };
    setMapValue(entries, input.reservationId, entry);
    const publishedReservationId = input.reservationId;
    const publishedOwnerToken = entry.ownerToken;
    try {
      owner.onDispose('reservation', () => {
        const current = mapValue(entries, publishedReservationId);
        if (current && liveEntry(current) && current.ownerToken === publishedOwnerToken) {
          replaceWithTombstone(publishedReservationId, current, ownerDisposalState(current));
        }
      });
      if (
        mapValue(entries, input.reservationId) !== entry ||
        !currentOwner(owner) ||
        owner.readGeneration() !== entry.navigationGeneration
      ) {
        replaceWithTombstone(input.reservationId, entry, ownerDisposalState(entry));
        return failure('stale_owner');
      }
    } catch {
      replaceWithTombstone(input.reservationId, entry, ownerDisposalState(entry));
      return failure('stale_owner');
    }
    return frozenResult({ ok: true, expiresAt: entry.expiresAt });
  };

  const recognize = (reservationId: unknown): ReservationRecognition => {
    if (clock() === undefined || typeof reservationId !== 'string') {
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
      let adopted: boolean;
      try {
        adopted = attempt.adoptWinnerContext(entry.winnerContext) === true;
      } catch {
        adopted = false;
      }
      if (
        !adopted ||
        mapValue(entries, fields.reservationId) !== entry ||
        !currentAttempt(attempt)
      ) {
        if (mapValue(entries, fields.reservationId) === entry) entry.busy = false;
        return failure('invalid_attempt');
      }
      entry.attemptId = identity.id;
      entry.state = 'renderable';
      entry.expiresAt = promotedExpiry;
      entry.busy = false;
      const candidates = mapValueSnapshot(entries);
      for (let index = 0; index < candidates.length; index += 1) {
        const candidate = candidates[index];
        if (
          candidate &&
          candidate !== entry &&
          liveEntry(candidate) &&
          candidate.state === 'awaiting_prebid_selection' &&
          candidate.auctionId === entry.auctionId &&
          candidate.adUnitCode === entry.adUnitCode &&
          candidate.navigationGeneration === entry.navigationGeneration
        ) {
          replaceWithTombstone(candidate.reservationId, candidate, 'unselected');
        }
      }
      return frozenResult({ ok: true, expiresAt: entry.expiresAt });
    },
    claim(input): ReservationClaimResult {
      const minimalId = (() => {
        try {
          return input.reservationId;
        } catch {
          return undefined;
        }
      })();
      if (clock() === undefined || typeof minimalId !== 'string') {
        return frozenResult({ recognized: false });
      }
      const entry = mapValue(entries, minimalId);
      if (!entry) return frozenResult({ recognized: false });
      if (!liveEntry(entry)) {
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
      entry.busy = true;
      entry.pucSource = fields.pucSource;
      let adopted: boolean;
      try {
        adopted = attempt.adoptWinnerContext(entry.winnerContext) === true;
      } catch {
        adopted = false;
      }
      if (!adopted || mapValue(entries, minimalId) !== entry || !currentAttempt(attempt)) {
        if (mapValue(entries, minimalId) === entry) {
          entry.pucSource = undefined;
          entry.busy = false;
          return refusedClaim(entry.state);
        }
        const replacement = mapValue(entries, minimalId);
        return refusedClaim(replacement?.state ?? 'stale');
      }
      const result = frozenResult({
        recognized: true as const,
        claimed: true as const,
        renderSource: entry.renderSource,
        winnerContext: entry.winnerContext,
        pucSource: entry.pucSource,
        expiresAt: entry.expiresAt,
      });
      replaceWithTombstone(minimalId, entry, 'consumed');
      return result;
    },
    recognize,
    tombstone(input, state): boolean {
      const fields = ownDataRecord(input, [
        'reservationId',
        'slot',
        'navigationGeneration',
        'attemptId',
      ]);
      if (clock() === undefined || !fields || typeof fields.reservationId !== 'string') {
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
      const fields = ownDataRecord(input, [
        'reservationId',
        'auctionId',
        'adUnitCode',
        'navigationGeneration',
      ]);
      if (clock() === undefined || !fields || typeof fields.reservationId !== 'string') {
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
      const fields = ownDataRecord(input, ['auctionId', 'adUnitCode', 'navigationGeneration']);
      if (clock() === undefined || !fields) {
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
  return frozenResult(service);
}
