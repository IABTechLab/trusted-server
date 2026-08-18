const EXTERNAL_READY_TIMEOUT_MS = 10_000;
const MAX_PENDING_OPERATIONS = 64;

/** The live state of the publisher-owned `window.googletag` binding. */
export type GoogletagBindingStatus = 'present' | 'pending' | 'incompatible';

/** The readiness state owned by one GPT operation. */
export type GoogletagOperationStatus = GoogletagBindingStatus | 'timed_out';

/** Failure codes produced at the GPT adapter boundary. */
export type GoogletagAdapterErrorCode =
  | 'caller_aborted'
  | 'external_artifact_incompatible'
  | 'external_queue_full'
  | 'external_ready_timeout'
  | 'operation_disposed';

/** A typed failure contained by the GPT adapter. */
export class GoogletagAdapterError extends Error {
  public readonly code: GoogletagAdapterErrorCode;

  public constructor(code: GoogletagAdapterErrorCode) {
    super(code);
    this.name = 'GoogletagAdapterError';
    this.code = code;
  }
}

/** Immutable GPT definition used by the adapter-owned replacement transaction. */
export interface GoogletagReplacementDefinition {
  readonly adUnitPath: string;
  readonly elementId: string;
  readonly sizes: unknown;
}

/** Reversible synchronous admission for one newly defined GPT identity. */
export interface GoogletagReplacementCommitAdmission {
  commit(): boolean;
  rollback(): void;
}

/** Outcome of one adapter-owned initial GPT slot-definition transaction. */
export type GoogletagDefinitionResult = Readonly<
  { status: 'discarded' } | { status: 'defined'; slot: object }
>;

/** Failure to define or synchronously retire one adapter-owned GPT slot. */
export class GoogletagDefinitionError extends Error {
  public readonly code = 'gpt_definition_failed';
  public readonly cause: unknown;
  public readonly orphanedSlot: object | undefined;

  public constructor(orphanedSlot?: object, cause?: unknown) {
    super('gpt_definition_failed');
    this.name = 'GoogletagDefinitionError';
    this.orphanedSlot = orphanedSlot;
    this.cause = cause;
  }
}

/** Successful outcome of one GPT destroy/redefine transaction. */
export type GoogletagReplacementResult = Readonly<
  { status: 'destroyed' } | { status: 'replaced'; slot: object }
>;

/** Failure from a replacement transaction, including any candidate GPT could not destroy. */
export class GoogletagReplacementError extends Error {
  public readonly code = 'gpt_replacement_failed';
  public readonly cause: unknown;
  public readonly oldSlotDestroyed: boolean;
  public readonly orphanedSlot: object | undefined;
  public readonly preserveOldQuarantine: boolean;

  public constructor(
    orphanedSlot?: object,
    oldSlotDestroyed = false,
    cause?: unknown,
    preserveOldQuarantine = false
  ) {
    super('gpt_replacement_failed');
    this.name = 'GoogletagReplacementError';
    this.orphanedSlot = orphanedSlot;
    this.oldSlotDestroyed = oldSlotDestroyed;
    this.cause = cause;
    this.preserveOldQuarantine = preserveOldQuarantine;
  }
}

/** Internal signal that a defineSlot result is already owned by another live record. */
export class GoogletagReplacementCandidateCollisionError extends Error {
  public readonly candidate: object;

  public constructor(candidate: object) {
    super('gpt_replacement_candidate_collision');
    this.name = 'GoogletagReplacementCandidateCollisionError';
    this.candidate = candidate;
  }
}

/** Observer called before a publisher-originated targeting mutation is forwarded. */
export interface GoogletagTargetingObserver {
  readonly beforePublisherMutation: (slot: object, key?: string) => void;
}

/** Callable targeting observation release with an exact wrapper-identity latch. */
export interface GoogletagTargetingObservation {
  (): void;
  readonly isCurrent: () => boolean;
}

/** Reversible bookkeeping prepared before one publisher GPT call. */
export interface GoogletagPublisherCallAdmission {
  readonly commit: () => void;
  readonly rollback: () => void;
}

/** One publisher-originated GPT call observed outside Trusted Server operations. */
export interface GoogletagPublisherCallObserver {
  readonly defineSlot?: (
    call: Readonly<GoogletagPublisherDefineSlotCall>
  ) => Readonly<{ action: 'forward' }> | Readonly<{ action: 'handoff'; slot: object }>;
  readonly destroySlots?: (call: Readonly<GoogletagPublisherDestroySlotsCall>) => void;
  readonly display?: (
    call: Readonly<GoogletagPublisherDisplayCall>
  ) =>
    | Readonly<{ action: 'forward'; admission?: GoogletagPublisherCallAdmission }>
    | Readonly<{ action: 'suppress' }>;
  readonly refresh?: (call: Readonly<GoogletagPublisherRefreshCall>) =>
    | Readonly<{ action: 'forward'; admission?: GoogletagPublisherCallAdmission }>
    | Readonly<{
        action: 'replace';
        slots: readonly object[];
        admission?: GoogletagPublisherCallAdmission;
      }>
    | Readonly<{
        action: 'defer';
        slots: readonly object[];
        completion: PromiseLike<unknown>;
        admission?: GoogletagPublisherCallAdmission;
      }>
    | Readonly<{ action: 'suppress' }>;
}

/** Narrow data supplied before one publisher `defineSlot` call. */
export interface GoogletagPublisherDefineSlotCall {
  readonly adUnitPath: unknown;
  readonly elementId: unknown;
  readonly initialLoadDisabled: boolean;
  readonly sizes: unknown;
}

/** Narrow data supplied after one successful publisher `destroySlots` call. */
export interface GoogletagPublisherDestroySlotsCall {
  readonly slots: readonly object[];
}

/** Narrow data supplied before one publisher `display` call. */
export interface GoogletagPublisherDisplayCall {
  readonly initialLoadDisabled: boolean;
  readonly target: unknown;
}

/** Narrow data supplied before one publisher `refresh` call. */
export interface GoogletagPublisherRefreshCall {
  readonly requestedSlots: readonly object[] | undefined;
  readonly slots: readonly object[];
  readonly options?: unknown;
}

/** The small GPT surface exposed to an accepted operation. */
export interface GoogletagFacade {
  adUnitPath?(slot: object): unknown;
  bindingToken(): object;
  clearTargeting(slot: object, key?: string): unknown;
  transactionalDefine(
    definition: GoogletagReplacementDefinition,
    isGenerationCurrent: () => boolean,
    prepareCommit: (slot: object) => GoogletagReplacementCommitAdmission
  ): GoogletagDefinitionResult;
  display(slot: string | object): unknown;
  getTargeting(slot: object, key: string): readonly string[];
  observeTargeting(
    slot: object,
    observer: GoogletagTargetingObserver
  ): GoogletagTargetingObservation;
  refresh(slots?: readonly object[], options?: Readonly<{ changeCorrelator: boolean }>): unknown;
  serviceState(): Readonly<{
    apiReady: boolean;
    initialLoadDisabled: boolean;
    pubadsReady: boolean;
  }>;
  setTargeting(slot: object, key: string, value: string | readonly string[]): unknown;
  slotElementId?(slot: object): unknown;
  slots(): readonly object[];
  subscribe(
    eventType: string,
    listener: (event: unknown) => Readonly<GoogletagTraceCycleHandle> | void,
    diagnosticsOwner?: boolean
  ): () => void;
  transactionalReplace(
    oldSlot: object,
    definition: GoogletagReplacementDefinition | undefined,
    isGenerationCurrent: () => boolean,
    prepareCommit: (replacement: object) => GoogletagReplacementCommitAdmission
  ): GoogletagReplacementResult;
}

/** Options owned by one GPT operation. */
export interface GoogletagOperationOptions {
  readonly signal?: AbortSignal;
}

/** A disposable GPT operation and its readiness-scoped result. */
export interface GoogletagOperation<T> {
  readonly status: GoogletagOperationStatus;
  readonly result: Promise<T>;
  dispose(): void;
}

/** Narrow GPT boundary consumed by kernel sessions and services. */
export interface GoogletagAdapter {
  bindingStatus(): GoogletagBindingStatus;
  adoptDiagnosticsState?(input: GoogletagDiagnosticsAdoptionV1): boolean;
  diagnosticsIdentity(slot: object): Readonly<GoogletagDiagnosticsSlotSnapshot> | undefined;
  traceToken(slot: object): GptSlotTokenV1 | undefined;
  observeDiagnostics(observer: GoogletagDiagnosticsObserver): (() => void) | undefined;
  observePublisherCalls(observer: GoogletagPublisherCallObserver): () => void;
  run<T>(
    command: (googletag: Readonly<GoogletagFacade>) => T,
    options?: GoogletagOperationOptions
  ): GoogletagOperation<T>;
  notifyReady(): void;
  dispose(): void;
}

export interface GoogletagDiagnosticsAdoptionV1 {
  readonly nextTraceTokenOrdinal: number;
  readonly slots: readonly Readonly<{
    readonly nextCycleOrdinal: number;
    readonly physicalSlot: object;
    readonly records: readonly Readonly<{
      readonly ordinal: number;
      readonly responseIdentifier: string | null;
      readonly seen: readonly GoogletagDiagnosticsEventName[];
      readonly state: 'open' | 'completed' | 'retired';
    }>[];
    readonly traceToken: string;
    readonly unknownPriorCycle: boolean;
  }>[];
}

export type GoogletagDiagnosticsEventName =
  | 'slotRequested'
  | 'slotResponseReceived'
  | 'slotRenderEnded'
  | 'slotOnload'
  | 'impressionViewable'
  | 'slotVisibilityChanged';

/** Safe Ad Manager identifiers copied from one GPT render callback. */
export interface GoogletagDiagnosticsAdManagerIdentity {
  readonly lineItemId?: number;
  readonly creativeId?: number;
  readonly campaignId?: number;
  readonly advertiserId?: number;
  readonly sourceAgnosticLineItemId?: number;
  readonly sourceAgnosticCreativeId?: number;
  readonly yieldGroupIds?: readonly number[];
  readonly companyIds?: readonly number[];
}

export interface GoogletagDiagnosticsFact {
  readonly kind: GoogletagDiagnosticsEventName;
  readonly observedAtMs: number;
  readonly slot: GoogletagDiagnosticsSlotSnapshot;
  readonly isEmpty?: boolean;
  readonly size?: readonly [number, number];
  readonly isBackfill?: boolean;
  readonly slotContentChanged?: boolean;
  readonly inViewPercentage?: number;
  readonly responseIdentifier?: string;
  readonly adManager?: GoogletagDiagnosticsAdManagerIdentity;
}

export type GptSlotTokenV1 = string & { readonly __brand: 'GptSlotTokenV1' };
export type GptTraceCycleOrdinalV1 = number & {
  readonly __brand: 'GptTraceCycleOrdinalV1';
};

/** Exact lifecycle-owned attribution accepted by the GPT diagnostics producer. */
export interface GoogletagTraceCycleHandle {
  readonly isRetired: () => boolean;
}

const googletagTraceCycleHandles = new WeakSet<object>();

/** Create one opaque adapter-branded handle for an accepted physical request cycle. */
export function createGoogletagTraceCycleHandle(
  isRetired: () => boolean
): Readonly<GoogletagTraceCycleHandle> {
  if (typeof isRetired !== 'function') throw new TypeError('invalid GPT trace cycle retirement');
  const handle = Object.freeze({ isRetired });
  googletagTraceCycleHandles.add(handle);
  return handle;
}

function acceptedTraceCycleHandle(value: unknown): value is Readonly<GoogletagTraceCycleHandle> {
  return (
    typeof value === 'object' &&
    value !== null &&
    Object.isFrozen(value) &&
    googletagTraceCycleHandles.has(value)
  );
}

/** Frozen, non-authoritative identity and metadata captured from one physical GPT slot. */
export interface GoogletagDiagnosticsSlotSnapshot {
  readonly token: object;
  readonly traceToken?: GptSlotTokenV1;
  readonly runtimeSlotNumber?: number;
  readonly cycleOrdinal?: GptTraceCycleOrdinalV1;
  readonly elementId?: string;
  readonly adUnitPath?: string;
}

export type GoogletagDiagnosticsObserver = (fact: Readonly<GoogletagDiagnosticsFact>) => void;

/** Browser surface owned by the concrete GPT adapter. */
export interface GoogletagGlobalTarget {
  googletag?: unknown;
  performance?: unknown;
}

export type GoogletagDiagnosticsFailureCode =
  | 'trace_cycle_ambiguity'
  | 'trace_cycle_collision'
  | 'trace_cycle_exhausted'
  | 'trace_cycle_invalid'
  | 'trace_token_collision'
  | 'trace_token_exhausted'
  | 'trace_token_invalid';

/** Test seams and local reporting for diagnostics-only identity construction. */
export interface GoogletagDiagnosticsIdentityOptions {
  readonly initialTraceCycleOrdinal?: number;
  readonly initialTraceTokenOrdinal?: number;
  readonly mintTraceToken?: (ordinal: number) => unknown;
  readonly reportDiagnosticsFailure?: (code: GoogletagDiagnosticsFailureCode) => void;
}

interface CommandQueue {
  readonly binding: object;
  readonly push: (...arguments_: unknown[]) => unknown;
}

interface PresentGoogletag {
  readonly binding: object;
  readonly commandQueue: CommandQueue;
  readonly display: (...arguments_: unknown[]) => unknown;
  readonly pubads: (...arguments_: unknown[]) => unknown;
}

interface ProvisionalEffect {
  promote(): void;
  release(): void;
}

interface AbortRegistration {
  readonly binding: object;
  readonly listener: () => void;
  readonly remove: (...arguments_: unknown[]) => unknown;
  attempted: boolean;
  cleanupRequested: boolean;
  installing: boolean;
}

interface PendingOperation<T> {
  state: GoogletagOperationStatus;
  settled: boolean;
  pendingReservation: boolean;
  timeout: ReturnType<typeof setTimeout> | undefined;
  readonly command: (googletag: Readonly<GoogletagFacade>) => T;
  readonly resolve: (value: T | PromiseLike<T>) => void;
  readonly reject: (reason: unknown) => void;
  abortRegistration: AbortRegistration | undefined;
  readinessBinding: object | undefined;
  readonly provisionalEffects: ProvisionalEffect[];
}

interface SharedInitialLoadTracker {
  disabled: boolean;
  rootWrapped: boolean;
  readonly owners: Set<object>;
  readonly restorers: Set<() => void>;
  readonly services: WeakMap<object, () => void>;
}

interface TargetingObservation {
  readonly isCurrent: () => boolean;
  readonly observers: Set<GoogletagTargetingObserver>;
  readonly restore: () => void;
}

const sharedInitialLoadTrackers = new WeakMap<object, SharedInitialLoadTracker>();
const mapDeleteIntrinsic = Map.prototype.delete;
const mapGetIntrinsic = Map.prototype.get;
const mapKeysIntrinsic = Map.prototype.keys;
const setDeleteIntrinsic = Set.prototype.delete;
const setAddIntrinsic = Set.prototype.add;
const setHasIntrinsic = Set.prototype.has;
const setSizeGetter = Object.getOwnPropertyDescriptor(Set.prototype, 'size')?.get as (
  this: Set<unknown>
) => number;
const setValuesIntrinsic = Set.prototype.values;
const setIteratorNextIntrinsic = Object.getPrototypeOf(new Set().values()).next as (
  this: IterableIterator<unknown>
) => IteratorResult<unknown>;
const weakMapDeleteIntrinsic = WeakMap.prototype.delete;
const weakMapGetIntrinsic = WeakMap.prototype.get;
const weakMapSetIntrinsic = WeakMap.prototype.set;
const weakSetDeleteIntrinsic = WeakSet.prototype.delete;

function mapValue<K, V>(map: Map<K, V>, key: K): V | undefined {
  return Reflect.apply(mapGetIntrinsic, map, [key]) as V | undefined;
}

function mapKeys<K, V>(map: Map<K, V>): IterableIterator<K> {
  return Reflect.apply(mapKeysIntrinsic, map, []) as IterableIterator<K>;
}

function deleteMapValue<K, V>(map: Map<K, V>, key: K): boolean {
  return Reflect.apply(mapDeleteIntrinsic, map, [key]) as boolean;
}

function deleteSetValue<T>(set: Set<T>, value: T): boolean {
  return Reflect.apply(setDeleteIntrinsic, set, [value]) as boolean;
}

function addSetValue<T>(set: Set<T>, value: T): void {
  Reflect.apply(setAddIntrinsic, set, [value]);
}

function setHasValue<T>(set: Set<T>, value: T): boolean {
  return Reflect.apply(setHasIntrinsic, set, [value]) as boolean;
}

function setValues<T>(set: Set<T>): IterableIterator<T> {
  return Reflect.apply(setValuesIntrinsic, set, []) as IterableIterator<T>;
}

function setValueSnapshot<T>(set: Set<T>): T[] {
  const iterator = setValues(set);
  const values: T[] = [];
  while (true) {
    const step = Reflect.apply(setIteratorNextIntrinsic, iterator, []) as IteratorResult<T>;
    if (step.done) return values;
    values[values.length] = step.value;
  }
}

function setSize(set: Set<unknown>): number {
  return Reflect.apply(setSizeGetter, set, []) as number;
}

function weakMapValue<K extends object, V>(map: WeakMap<K, V>, key: K): V | undefined {
  return Reflect.apply(weakMapGetIntrinsic, map, [key]) as V | undefined;
}

function setWeakMapValue<K extends object, V>(map: WeakMap<K, V>, key: K, value: V): void {
  Reflect.apply(weakMapSetIntrinsic, map, [key, value]);
}

function deleteWeakMapValue<K extends object, V>(map: WeakMap<K, V>, key: K): boolean {
  return Reflect.apply(weakMapDeleteIntrinsic, map, [key]) as boolean;
}

function deleteWeakSetValue<T extends object>(set: WeakSet<T>, value: T): boolean {
  return Reflect.apply(weakSetDeleteIntrinsic, set, [value]) as boolean;
}

function safeMember(binding: object, key: PropertyKey): unknown {
  try {
    return Reflect.get(binding, key);
  } catch {
    return undefined;
  }
}

function commandQueue(binding: object): CommandQueue | undefined {
  const candidate = safeMember(binding, 'cmd');
  if ((typeof candidate !== 'object' || candidate === null) && typeof candidate !== 'function') {
    return undefined;
  }
  const push = safeMember(candidate, 'push');
  return typeof push === 'function'
    ? {
        binding: candidate as object,
        push: push as (...arguments_: unknown[]) => unknown,
      }
    : undefined;
}

function inspectBinding(
  value: unknown
):
  | { readonly status: 'pending'; readonly binding?: object; readonly commandQueue?: CommandQueue }
  | { readonly status: 'incompatible'; readonly binding?: object }
  | { readonly status: 'present'; readonly value: PresentGoogletag } {
  if (value === undefined || value === null) return { status: 'pending' };
  if ((typeof value !== 'object' || value === null) && typeof value !== 'function') {
    return { status: 'incompatible' };
  }
  const binding = value as object;
  const queue = commandQueue(binding);
  if (!queue) return { status: 'incompatible', binding };
  if (safeMember(binding, 'apiReady') !== true) {
    return { status: 'pending', binding, commandQueue: queue };
  }
  const display = safeMember(binding, 'display');
  const pubads = safeMember(binding, 'pubads');
  if (typeof display !== 'function' || typeof pubads !== 'function') {
    return { status: 'incompatible', binding };
  }
  return {
    status: 'present',
    value: {
      binding,
      commandQueue: queue,
      display: display as (...arguments_: unknown[]) => unknown,
      pubads: pubads as (...arguments_: unknown[]) => unknown,
    },
  };
}

function readTarget(target: GoogletagGlobalTarget): unknown {
  try {
    return target.googletag;
  } catch {
    return false;
  }
}

function queueCommand(queue: CommandQueue, command: () => void, guard?: () => boolean): void {
  if (guard && !guard()) throw new GoogletagAdapterError('external_artifact_incompatible');
  Reflect.apply(queue.push, queue.binding, [command]);
  if (guard && !guard()) throw new GoogletagAdapterError('external_artifact_incompatible');
}

function asObject(value: unknown): object {
  if ((typeof value !== 'object' || value === null) && typeof value !== 'function') {
    throw new GoogletagAdapterError('external_artifact_incompatible');
  }
  return value;
}

function createFacade(
  binding: PresentGoogletag,
  registerEffect: (dispose: () => void) => () => void,
  isOperationCurrent: () => boolean,
  isBindingCurrent: () => boolean,
  initialLoadDisabled: (service: object) => boolean,
  targetingObservations: WeakMap<object, TargetingObservation>,
  bindingToken: object,
  markFirstDisplay: () => void,
  invokeFacadeCall: (
    callable: (...arguments_: unknown[]) => unknown,
    receiver: unknown,
    arguments_: readonly unknown[]
  ) => unknown,
  consumeFacadeCall: (callable: (...arguments_: unknown[]) => unknown) => boolean,
  publishDiagnostics: (
    eventType: string,
    event: unknown,
    handle: Readonly<GoogletagTraceCycleHandle> | undefined
  ) => void
): Readonly<GoogletagFacade> {
  const member = (external: object, key: PropertyKey): ((...args: unknown[]) => unknown) => {
    if (!isOperationCurrent()) throw new GoogletagAdapterError('external_artifact_incompatible');
    const candidate = safeMember(external, key);
    if (!isOperationCurrent()) throw new GoogletagAdapterError('external_artifact_incompatible');
    if (typeof candidate !== 'function') {
      throw new GoogletagAdapterError('external_artifact_incompatible');
    }
    if (!isOperationCurrent()) throw new GoogletagAdapterError('external_artifact_incompatible');
    return candidate as (...args: unknown[]) => unknown;
  };
  const call = (external: object, key: PropertyKey, argumentsList: readonly unknown[]): unknown => {
    const callable = member(external, key);
    if (!isOperationCurrent()) throw new GoogletagAdapterError('external_artifact_incompatible');
    const result = invokeFacadeCall(callable, external, argumentsList);
    if (!isOperationCurrent()) throw new GoogletagAdapterError('external_artifact_incompatible');
    return result;
  };
  const value = (external: object, key: PropertyKey): unknown => {
    if (!isOperationCurrent()) throw new GoogletagAdapterError('external_artifact_incompatible');
    const result = safeMember(external, key);
    if (!isOperationCurrent()) throw new GoogletagAdapterError('external_artifact_incompatible');
    return result;
  };
  const service = (): object => asObject(call(binding.binding, 'pubads', []));
  const replaceObservedMethod = (
    slot: object,
    key: 'clearTargeting' | 'setTargeting',
    observer: GoogletagTargetingObserver
  ): Readonly<{ isCurrent: () => boolean; restore: () => void }> | undefined => {
    if (!isOperationCurrent()) return undefined;
    const original = member(slot, key);
    let descriptor: PropertyDescriptor | undefined;
    let defineAttempted = false;
    const wrapper = function (this: unknown, ...arguments_: unknown[]): unknown {
      if (consumeFacadeCall(wrapper)) {
        return Reflect.apply(original, this, arguments_);
      }
      try {
        const mutationKey = typeof arguments_[0] === 'string' ? arguments_[0] : undefined;
        observer.beforePublisherMutation(slot, mutationKey);
      } catch {
        // Bookkeeping must not change publisher call arguments, order, return, or throw.
      }
      return Reflect.apply(original, this, arguments_);
    };
    const restore = (): void => {
      if (!defineAttempted) return;
      defineAttempted = false;
      try {
        const current = Object.getOwnPropertyDescriptor(slot, key);
        if (!current || current.value !== wrapper) return;
        if (descriptor) Reflect.defineProperty(slot, key, descriptor);
        else Reflect.deleteProperty(slot, key);
      } catch {
        // Publisher replacement wins once the installed method no longer matches.
      }
    };
    const wrapperIsCurrent = (): boolean => {
      try {
        if (!defineAttempted) return false;
        const current = Object.getOwnPropertyDescriptor(slot, key);
        return current !== undefined && current.value === wrapper;
      } catch {
        return false;
      }
    };
    try {
      descriptor = Object.getOwnPropertyDescriptor(slot, key);
      if (
        descriptor &&
        (!Object.prototype.hasOwnProperty.call(descriptor, 'value') ||
          (descriptor.configurable !== true && descriptor.writable !== true))
      ) {
        return undefined;
      }
      const replacement = descriptor
        ? { ...descriptor, value: wrapper }
        : { configurable: true, enumerable: true, value: wrapper, writable: true };
      if (!isOperationCurrent()) {
        return undefined;
      }
      defineAttempted = true;
      if (!Reflect.defineProperty(slot, key, replacement)) {
        restore();
        return undefined;
      }
      if (!isOperationCurrent() || safeMember(slot, key) !== wrapper) {
        restore();
        return undefined;
      }
      return Object.freeze({ isCurrent: wrapperIsCurrent, restore });
    } catch {
      restore();
      return undefined;
    }
  };
  return Object.freeze({
    adUnitPath: (slot: object): unknown => call(slot, 'getAdUnitPath', []),
    bindingToken: (): object => bindingToken,
    clearTargeting: (slot: object, key?: string): unknown =>
      call(slot, 'clearTargeting', key === undefined ? [] : [key]),
    transactionalDefine: (
      definition: GoogletagReplacementDefinition,
      isGenerationCurrent: () => boolean,
      prepareCommit: (slot: object) => GoogletagReplacementCommitAdmission
    ): GoogletagDefinitionResult => {
      if (
        typeof isGenerationCurrent !== 'function' ||
        typeof prepareCommit !== 'function' ||
        !isOperationCurrent()
      ) {
        throw new GoogletagAdapterError('external_artifact_incompatible');
      }
      const destroy = (slot: object): boolean => {
        try {
          return call(binding.binding, 'destroySlots', [[slot]]) === true;
        } catch {
          return false;
        }
      };
      const discarded = Object.freeze({ status: 'discarded' as const });
      let candidate: object | undefined;
      let admission: GoogletagReplacementCommitAdmission | undefined;
      let commitAttempted = false;
      const discard = (slot: object, cause?: unknown): GoogletagDefinitionResult => {
        if (!destroy(slot)) throw new GoogletagDefinitionError(slot, cause);
        return discarded;
      };
      try {
        if (!isGenerationCurrent() || !isOperationCurrent()) return discarded;
        const defined = call(binding.binding, 'defineSlot', [
          definition.adUnitPath,
          definition.sizes,
          definition.elementId,
        ]);
        if ((typeof defined !== 'object' || defined === null) && typeof defined !== 'function') {
          throw new GoogletagDefinitionError();
        }
        candidate = defined as object;
        if (!isGenerationCurrent() || !isOperationCurrent()) {
          const stale = candidate;
          candidate = undefined;
          return discard(stale);
        }
        admission = prepareCommit(candidate);
        if (
          !admission ||
          typeof admission.commit !== 'function' ||
          typeof admission.rollback !== 'function'
        ) {
          throw new GoogletagDefinitionError();
        }
        call(candidate, 'addService', [service()]);
        if (!isGenerationCurrent() || !isOperationCurrent()) {
          const stale = candidate;
          candidate = undefined;
          return discard(stale);
        }
        commitAttempted = true;
        if (!admission.commit()) throw new GoogletagDefinitionError();
        if (!isGenerationCurrent() || !isOperationCurrent()) {
          try {
            admission.rollback();
          } finally {
            commitAttempted = false;
          }
          const stale = candidate;
          candidate = undefined;
          return discard(stale);
        }
        return Object.freeze({ status: 'defined' as const, slot: candidate });
      } catch (error) {
        if (commitAttempted) {
          try {
            admission?.rollback();
          } catch {
            // Candidate retirement remains mandatory after bookkeeping rollback failure.
          }
        }
        if (candidate) {
          const failed = candidate;
          if (!destroy(failed)) throw new GoogletagDefinitionError(failed, error);
        }
        if (error instanceof GoogletagDefinitionError) throw error;
        throw new GoogletagDefinitionError(undefined, error);
      }
    },
    display: (slot: string | object): unknown => {
      const display = member(binding.binding, 'display');
      if (!isOperationCurrent()) throw new GoogletagAdapterError('external_artifact_incompatible');
      markFirstDisplay();
      if (!isOperationCurrent()) throw new GoogletagAdapterError('external_artifact_incompatible');
      const result = invokeFacadeCall(display, binding.binding, [slot]);
      if (!isOperationCurrent()) throw new GoogletagAdapterError('external_artifact_incompatible');
      return result;
    },
    getTargeting: (slot: object, key: string): readonly string[] => {
      const targeting = call(slot, 'getTargeting', [key]);
      if (!Array.isArray(targeting) || targeting.some((entry) => typeof entry !== 'string')) {
        throw new GoogletagAdapterError('external_artifact_incompatible');
      }
      if (!isOperationCurrent()) throw new GoogletagAdapterError('external_artifact_incompatible');
      return Object.freeze([...targeting]);
    },
    observeTargeting: (
      slot: object,
      observer: GoogletagTargetingObserver
    ): GoogletagTargetingObservation => {
      if (
        typeof observer !== 'object' ||
        observer === null ||
        typeof observer.beforePublisherMutation !== 'function'
      ) {
        throw new GoogletagAdapterError('external_artifact_incompatible');
      }
      let observation = weakMapValue(targetingObservations, slot);
      if (!observation) {
        const observers = new Set<GoogletagTargetingObserver>();
        const dispatcher: GoogletagTargetingObserver = Object.freeze({
          beforePublisherMutation: (mutatedSlot: object, key?: string): void => {
            const currentObservers = setValueSnapshot(observers);
            for (let index = 0; index < currentObservers.length; index += 1) {
              const current = currentObservers[index];
              if (!current) continue;
              try {
                current.beforePublisherMutation(mutatedSlot, key);
              } catch {
                // One observer cannot prevent another or alter the publisher mutation.
              }
            }
          },
        });
        const restoreSet = replaceObservedMethod(slot, 'setTargeting', dispatcher);
        if (!restoreSet) throw new GoogletagAdapterError('external_artifact_incompatible');
        const restoreClear = replaceObservedMethod(slot, 'clearTargeting', dispatcher);
        if (!restoreClear) {
          restoreSet.restore();
          throw new GoogletagAdapterError('external_artifact_incompatible');
        }
        let restored = false;
        observation = {
          isCurrent: (): boolean => restoreSet.isCurrent() && restoreClear.isCurrent(),
          observers,
          restore: (): void => {
            if (restored) return;
            restored = true;
            try {
              restoreClear.restore();
            } finally {
              restoreSet.restore();
            }
          },
        };
        try {
          setWeakMapValue(targetingObservations, slot, observation);
        } catch (error) {
          observation.restore();
          throw error;
        }
      }
      try {
        addSetValue(observation.observers, observer);
      } catch (error) {
        if (setSize(observation.observers) === 0) {
          if (weakMapValue(targetingObservations, slot) === observation) {
            deleteWeakMapValue(targetingObservations, slot);
          }
          observation.restore();
        }
        throw error;
      }
      let active = true;
      const releaseEffect = registerEffect(() => {
        if (!active) return;
        active = false;
        deleteSetValue(observation!.observers, observer);
        if (setSize(observation!.observers) === 0) {
          if (weakMapValue(targetingObservations, slot) === observation) {
            deleteWeakMapValue(targetingObservations, slot);
          }
          observation!.restore();
        }
      });
      const release = (() => releaseEffect()) as GoogletagTargetingObservation;
      Object.defineProperty(release, 'isCurrent', {
        configurable: false,
        enumerable: true,
        value: (): boolean => {
          try {
            return active && observation?.isCurrent() === true;
          } catch {
            return false;
          }
        },
        writable: false,
      });
      return Object.freeze(release);
    },
    refresh: (
      slots?: readonly object[],
      options?: Readonly<{ changeCorrelator: boolean }>
    ): unknown =>
      call(
        service(),
        'refresh',
        slots === undefined
          ? options === undefined
            ? []
            : [undefined, options]
          : options === undefined
            ? [[...slots]]
            : [[...slots], options]
      ),
    serviceState: () => {
      const currentService = service();
      const initialLoadDisabledValue = initialLoadDisabled(currentService);
      if (!isOperationCurrent()) throw new GoogletagAdapterError('external_artifact_incompatible');
      return Object.freeze({
        apiReady: value(binding.binding, 'apiReady') === true,
        initialLoadDisabled: initialLoadDisabledValue,
        pubadsReady: value(binding.binding, 'pubadsReady') === true,
      });
    },
    setTargeting: (slot: object, key: string, value: string | readonly string[]): unknown =>
      call(slot, 'setTargeting', [key, Array.isArray(value) ? [...value] : value]),
    slotElementId: (slot: object): unknown => call(slot, 'getSlotElementId', []),
    slots: (): readonly object[] => {
      const currentSlots = call(service(), 'getSlots', []);
      if (
        !Array.isArray(currentSlots) ||
        currentSlots.some((slot) => typeof slot !== 'object' || slot === null)
      ) {
        throw new GoogletagAdapterError('external_artifact_incompatible');
      }
      if (!isOperationCurrent()) throw new GoogletagAdapterError('external_artifact_incompatible');
      return Object.freeze([...currentSlots]);
    },
    subscribe: (
      eventType: string,
      listener: (event: unknown) => Readonly<GoogletagTraceCycleHandle> | void,
      diagnosticsOwner = false
    ): (() => void) => {
      const currentService = service();
      const add = member(currentService, 'addEventListener');
      const remove = member(currentService, 'removeEventListener');
      const wrapped = (event: unknown): void => {
        if (!isBindingCurrent()) return;
        let handle: Readonly<GoogletagTraceCycleHandle> | void = undefined;
        try {
          handle = listener(event);
        } catch {
          // Publisher and service callbacks cannot escape the GPT boundary.
        }
        if (diagnosticsOwner) {
          publishDiagnostics(eventType, event, undefined);
        } else if (eventType === 'slotRequested' || eventType === 'slotRenderEnded') {
          publishDiagnostics(
            eventType,
            event,
            acceptedTraceCycleHandle(handle) ? handle : undefined
          );
        }
      };
      let attempted = false;
      const rollback = (): void => {
        if (!attempted) return;
        attempted = false;
        try {
          Reflect.apply(remove, currentService, [eventType, wrapped]);
        } catch {
          // Transaction rollback remains best-effort and cannot replace the original failure.
        }
      };
      try {
        if (!isOperationCurrent())
          throw new GoogletagAdapterError('external_artifact_incompatible');
        attempted = true;
        Reflect.apply(add, currentService, [eventType, wrapped]);
        if (!isOperationCurrent())
          throw new GoogletagAdapterError('external_artifact_incompatible');
      } catch (error) {
        rollback();
        throw error;
      }
      let active = true;
      return registerEffect(() => {
        if (!active) return;
        active = false;
        rollback();
      });
    },
    transactionalReplace: (
      oldSlot: object,
      definition: GoogletagReplacementDefinition | undefined,
      isGenerationCurrent: () => boolean,
      prepareCommit: (replacement: object) => GoogletagReplacementCommitAdmission
    ): GoogletagReplacementResult => {
      if (
        typeof isGenerationCurrent !== 'function' ||
        typeof prepareCommit !== 'function' ||
        !isOperationCurrent()
      ) {
        throw new GoogletagAdapterError('external_artifact_incompatible');
      }
      const destroy = (slot: object): boolean => {
        try {
          return call(binding.binding, 'destroySlots', [[slot]]) === true;
        } catch {
          return false;
        }
      };
      const destroyed = Object.freeze({ status: 'destroyed' as const });
      const cleanup = (candidate: object, cause?: unknown): never => {
        if (!destroy(candidate)) {
          throw new GoogletagReplacementError(candidate, true, cause);
        }
        throw new GoogletagReplacementError(undefined, true, cause);
      };
      if (!destroy(oldSlot)) throw new GoogletagReplacementError(oldSlot);
      let replacement: object | undefined;
      let admission: GoogletagReplacementCommitAdmission | undefined;
      let commitAttempted = false;
      try {
        if (definition === undefined || !isGenerationCurrent() || !isOperationCurrent()) {
          return destroyed;
        }
        const candidate = call(binding.binding, 'defineSlot', [
          definition.adUnitPath,
          definition.sizes,
          definition.elementId,
        ]);
        if (
          (typeof candidate !== 'object' || candidate === null) &&
          typeof candidate !== 'function'
        ) {
          throw new GoogletagReplacementError(undefined, true);
        }
        replacement = candidate as object;
        if (replacement === oldSlot) {
          const invalid = replacement;
          replacement = undefined;
          cleanup(invalid);
        }
        if (!isGenerationCurrent() || !isOperationCurrent()) {
          const stale = replacement as object;
          replacement = undefined;
          if (!destroy(stale)) throw new GoogletagReplacementError(stale, true);
          return destroyed;
        }
        admission = prepareCommit(replacement as object);
        if (
          !admission ||
          typeof admission.commit !== 'function' ||
          typeof admission.rollback !== 'function'
        ) {
          throw new GoogletagReplacementError(undefined, true);
        }
        call(replacement as object, 'addService', [service()]);
        if (!isGenerationCurrent() || !isOperationCurrent()) {
          const stale = replacement as object;
          replacement = undefined;
          if (!destroy(stale)) throw new GoogletagReplacementError(stale, true);
          return destroyed;
        }
        commitAttempted = true;
        if (!admission.commit()) throw new GoogletagReplacementError(undefined, true);
        if (!isGenerationCurrent() || !isOperationCurrent()) {
          let rollbackFailed = false;
          let rollbackFailure: unknown;
          try {
            admission.rollback();
          } catch (error) {
            rollbackFailed = true;
            rollbackFailure = error;
          }
          commitAttempted = false;
          const stale = replacement as object;
          replacement = undefined;
          if (!destroy(stale)) {
            throw new GoogletagReplacementError(stale, true, rollbackFailure);
          }
          if (rollbackFailed) {
            throw new GoogletagReplacementError(undefined, true, rollbackFailure);
          }
          return destroyed;
        }
        return Object.freeze({ status: 'replaced' as const, slot: replacement as object });
      } catch (error) {
        if (commitAttempted) {
          try {
            admission?.rollback();
          } catch {
            // Candidate cleanup remains mandatory even when service rollback is hostile.
          }
        }
        if (error instanceof GoogletagReplacementCandidateCollisionError) {
          throw new GoogletagReplacementError(undefined, true, error, true);
        }
        if (replacement) cleanup(replacement, error);
        if (error instanceof GoogletagReplacementError) throw error;
        throw new GoogletagReplacementError(undefined, true, error);
      }
    },
  });
}

/** Create the sole production reader/writer boundary for `window.googletag`. */
export function createBrowserGoogletagAdapter(
  target: GoogletagGlobalTarget = window as unknown as GoogletagGlobalTarget,
  diagnosticsOptions: GoogletagDiagnosticsIdentityOptions = {}
): GoogletagAdapter {
  const pending: PendingOperation<unknown>[] = [];
  const live = new Set<PendingOperation<unknown>>();
  const effects = new Set<() => void>();
  let armedBindings = new WeakSet<object>();
  const targetingObservations = new WeakMap<object, TargetingObservation>();
  const facadeCalls = new WeakMap<(...arguments_: unknown[]) => unknown, number>();
  const adapterMethodOrigins = new WeakMap<
    (...arguments_: unknown[]) => unknown,
    (...arguments_: unknown[]) => unknown
  >();
  const bindingTokens = new WeakMap<object, object>();
  interface TraceCycle {
    readonly handle: Readonly<GoogletagTraceCycleHandle>;
    readonly ordinal: GptTraceCycleOrdinalV1;
    readonly seen: Set<GoogletagDiagnosticsEventName>;
    responseIdentifier?: string;
    state: 'open' | 'completed' | 'retired';
  }
  interface DiagnosticsSlotState {
    readonly adUnitPath?: string;
    readonly cycles: TraceCycle[];
    readonly elementId?: string;
    nextCycleOrdinal: number;
    readonly token: object;
    readonly traceToken?: GptSlotTokenV1;
    unknownPriorCycle: boolean;
  }
  const diagnosticsSlots = new WeakMap<object, DiagnosticsSlotState>();
  const traceCycleHandleOwners = new WeakMap<
    Readonly<GoogletagTraceCycleHandle>,
    DiagnosticsSlotState
  >();
  const mintTraceToken =
    typeof diagnosticsOptions.mintTraceToken === 'function'
      ? diagnosticsOptions.mintTraceToken
      : undefined;
  const mintedTraceTokens = mintTraceToken ? new Set<string>() : undefined;
  const reportedDiagnosticsFailures = new Set<GoogletagDiagnosticsFailureCode>();
  const initialLoadReleases = new Map<object, () => void>();
  const initialLoadOwner = Object.freeze({});
  let diagnosticsObserver: GoogletagDiagnosticsObserver | undefined;
  let nextTraceTokenOrdinal = diagnosticsOptions.initialTraceTokenOrdinal ?? 1;
  let diagnosticsAdoptionOpen = true;
  let pendingReservations = 0;
  let disposed = false;
  let firstDisplayObserved = false;

  const reportDiagnosticsFailure = (code: GoogletagDiagnosticsFailureCode): void => {
    try {
      if (setHasValue(reportedDiagnosticsFailures, code)) return;
      addSetValue(reportedDiagnosticsFailures, code);
    } catch {
      return;
    }
    try {
      diagnosticsOptions.reportDiagnosticsFailure?.(code);
    } catch {
      // Local diagnostics reporting cannot affect GPT lifecycle behavior.
    }
  };

  const createDiagnosticsSlotState = (physicalSlot: object): DiagnosticsSlotState => {
    const optionalStringCall = (key: 'getSlotElementId' | 'getAdUnitPath'): string | undefined => {
      const method = safeMember(physicalSlot, key);
      if (typeof method !== 'function') return undefined;
      try {
        const value = Reflect.apply(method, physicalSlot, []);
        return typeof value === 'string' && value.length > 0 ? value : undefined;
      } catch {
        return undefined;
      }
    };
    const ordinal = nextTraceTokenOrdinal;
    let traceToken: GptSlotTokenV1 | undefined;
    if (Number.isInteger(ordinal) && ordinal >= 1 && ordinal <= 4_294_967_295) {
      let candidate: unknown;
      try {
        candidate = mintTraceToken ? mintTraceToken(ordinal) : `gt1_${ordinal.toString(36)}`;
      } catch {
        reportDiagnosticsFailure('trace_token_invalid');
      }
      if (
        typeof candidate === 'string' &&
        /^gt1_[1-9a-z][0-9a-z]{0,6}$/.test(candidate) &&
        candidate.length <= 11 &&
        Number.parseInt(candidate.slice(4), 36) <= 4_294_967_295
      ) {
        if (mintedTraceTokens && setHasValue(mintedTraceTokens, candidate)) {
          reportDiagnosticsFailure('trace_token_collision');
        } else {
          try {
            if (mintedTraceTokens) addSetValue(mintedTraceTokens, candidate);
            traceToken = candidate as GptSlotTokenV1;
            nextTraceTokenOrdinal += 1;
          } catch {
            if (mintedTraceTokens) deleteSetValue(mintedTraceTokens, candidate);
            reportDiagnosticsFailure('trace_token_invalid');
          }
        }
      } else if (candidate !== undefined) {
        reportDiagnosticsFailure('trace_token_invalid');
      }
    } else {
      reportDiagnosticsFailure(
        Number.isInteger(ordinal) && ordinal > 4_294_967_295
          ? 'trace_token_exhausted'
          : 'trace_token_invalid'
      );
    }
    const elementId = optionalStringCall('getSlotElementId');
    const adUnitPath = optionalStringCall('getAdUnitPath');
    return {
      ...(adUnitPath === undefined ? {} : { adUnitPath }),
      cycles: [],
      ...(elementId === undefined ? {} : { elementId }),
      nextCycleOrdinal: diagnosticsOptions.initialTraceCycleOrdinal ?? 1,
      token: Object.freeze(Object.create(null) as object),
      ...(traceToken === undefined ? {} : { traceToken }),
      unknownPriorCycle: false,
    };
  };

  const diagnosticsSlotState = (physicalSlot: object): DiagnosticsSlotState | undefined => {
    if (disposed) return undefined;
    diagnosticsAdoptionOpen = false;
    try {
      let state = weakMapValue(diagnosticsSlots, physicalSlot);
      if (!state) {
        state = createDiagnosticsSlotState(physicalSlot);
        setWeakMapValue(diagnosticsSlots, physicalSlot, state);
        if (weakMapValue(diagnosticsSlots, physicalSlot) !== state) return undefined;
      }
      return state;
    } catch {
      return undefined;
    }
  };

  const adoptDiagnosticsState = (input: GoogletagDiagnosticsAdoptionV1): boolean => {
    if (
      disposed ||
      !diagnosticsAdoptionOpen ||
      typeof input !== 'object' ||
      input === null ||
      !Number.isInteger(input.nextTraceTokenOrdinal) ||
      input.nextTraceTokenOrdinal < 1 ||
      input.nextTraceTokenOrdinal > 4_294_967_295 ||
      !Array.isArray(input.slots) ||
      input.slots.length > 256
    ) {
      return false;
    }
    const physicalSlots = new Set<object>();
    const traceTokens = new Set<string>();
    const prepared: Array<Readonly<{ physicalSlot: object; state: DiagnosticsSlotState }>> = [];
    let maximumTokenOrdinal = 0;
    for (const adopted of input.slots) {
      const tokenOrdinal =
        typeof adopted?.traceToken === 'string' &&
        /^gt1_[1-9a-z][0-9a-z]{0,6}$/.test(adopted.traceToken)
          ? Number.parseInt(adopted.traceToken.slice(4), 36)
          : Number.NaN;
      if (
        typeof adopted !== 'object' ||
        adopted === null ||
        (typeof adopted.physicalSlot !== 'object' && typeof adopted.physicalSlot !== 'function') ||
        adopted.physicalSlot === null ||
        physicalSlots.has(adopted.physicalSlot) ||
        !Number.isInteger(tokenOrdinal) ||
        tokenOrdinal < 1 ||
        tokenOrdinal > 4_294_967_295 ||
        traceTokens.has(adopted.traceToken) ||
        !Number.isInteger(adopted.nextCycleOrdinal) ||
        adopted.nextCycleOrdinal < 1 ||
        adopted.nextCycleOrdinal > 4_294_967_295 ||
        typeof adopted.unknownPriorCycle !== 'boolean' ||
        !Array.isArray(adopted.records) ||
        adopted.records.length > 10
      ) {
        return false;
      }
      const ordinals = new Set<number>();
      const cycles: TraceCycle[] = [];
      let maximumCycleOrdinal = 0;
      for (const record of adopted.records) {
        const seen = Array.isArray(record?.seen) ? (record.seen as readonly unknown[]) : undefined;
        const responseIdentifier = record?.responseIdentifier;
        if (
          typeof record !== 'object' ||
          record === null ||
          !Number.isInteger(record.ordinal) ||
          record.ordinal < 1 ||
          record.ordinal > 4_294_967_295 ||
          ordinals.has(record.ordinal) ||
          (record.state !== 'open' && record.state !== 'completed' && record.state !== 'retired') ||
          !seen ||
          seen.length === 0 ||
          seen.length > 6 ||
          new Set(seen).size !== seen.length ||
          seen.some(
            (event: unknown) =>
              event !== 'slotRequested' &&
              event !== 'slotResponseReceived' &&
              event !== 'slotRenderEnded' &&
              event !== 'slotOnload' &&
              event !== 'impressionViewable' &&
              event !== 'slotVisibilityChanged'
          ) ||
          !seen.includes('slotRequested') ||
          (record.state === 'open' && seen.includes('slotRenderEnded')) ||
          (record.state === 'completed' && !seen.includes('slotRenderEnded')) ||
          (responseIdentifier !== null &&
            (typeof responseIdentifier !== 'string' ||
              responseIdentifier.length === 0 ||
              new TextEncoder().encode(responseIdentifier).byteLength > 256 ||
              [...responseIdentifier].some((character) => {
                const code = character.charCodeAt(0);
                return code <= 0x1f || code === 0x7f;
              })))
        ) {
          return false;
        }
        const retired = record.state === 'retired';
        const handle = createGoogletagTraceCycleHandle(() => retired);
        cycles.push({
          handle,
          ordinal: record.ordinal as GptTraceCycleOrdinalV1,
          ...(responseIdentifier === null ? {} : { responseIdentifier }),
          seen: new Set(seen as readonly GoogletagDiagnosticsEventName[]),
          state: record.state,
        });
        ordinals.add(record.ordinal);
        maximumCycleOrdinal = Math.max(maximumCycleOrdinal, record.ordinal);
      }
      if (adopted.nextCycleOrdinal <= maximumCycleOrdinal) return false;
      physicalSlots.add(adopted.physicalSlot);
      traceTokens.add(adopted.traceToken);
      maximumTokenOrdinal = Math.max(maximumTokenOrdinal, tokenOrdinal);
      prepared.push({
        physicalSlot: adopted.physicalSlot,
        state: {
          cycles,
          nextCycleOrdinal: adopted.nextCycleOrdinal,
          token: Object.freeze(Object.create(null) as object),
          traceToken: adopted.traceToken as GptSlotTokenV1,
          unknownPriorCycle: adopted.unknownPriorCycle,
        },
      });
    }
    if (input.nextTraceTokenOrdinal <= maximumTokenOrdinal) return false;
    try {
      for (const adopted of prepared) {
        setWeakMapValue(diagnosticsSlots, adopted.physicalSlot, adopted.state);
        for (const cycle of adopted.state.cycles) {
          setWeakMapValue(traceCycleHandleOwners, cycle.handle, adopted.state);
        }
        if (mintedTraceTokens) addSetValue(mintedTraceTokens, adopted.state.traceToken!);
      }
      nextTraceTokenOrdinal = input.nextTraceTokenOrdinal;
      diagnosticsAdoptionOpen = false;
      return true;
    } catch {
      reportDiagnosticsFailure('trace_cycle_invalid');
      return false;
    }
  };

  const traceCycle = (
    state: DiagnosticsSlotState,
    eventType: GoogletagDiagnosticsEventName,
    responseIdentifier: string | undefined,
    acceptedHandle: Readonly<GoogletagTraceCycleHandle> | undefined
  ): GptTraceCycleOrdinalV1 | undefined => {
    if (!state.traceToken) return undefined;
    const isRetired = (handle: Readonly<GoogletagTraceCycleHandle>): boolean => {
      try {
        return handle.isRetired() === true;
      } catch {
        return true;
      }
    };
    for (let index = 0; index < state.cycles.length; index += 1) {
      const cycle = state.cycles[index];
      if (cycle && cycle.state !== 'retired' && isRetired(cycle.handle)) {
        cycle.state = 'retired';
      }
    }
    if (eventType === 'slotRequested') {
      if (!acceptedHandle || isRetired(acceptedHandle)) return undefined;
      if (
        weakMapValue(traceCycleHandleOwners, acceptedHandle) !== undefined ||
        state.cycles.some((cycle) => cycle.handle === acceptedHandle) ||
        state.cycles.some((cycle) => cycle.state === 'open')
      ) {
        reportDiagnosticsFailure('trace_cycle_collision');
        return undefined;
      }
      const ordinal = state.nextCycleOrdinal;
      if (!Number.isInteger(ordinal) || ordinal < 1 || ordinal > 4_294_967_295) {
        reportDiagnosticsFailure(
          Number.isInteger(ordinal) && ordinal > 4_294_967_295
            ? 'trace_cycle_exhausted'
            : 'trace_cycle_invalid'
        );
        return undefined;
      }
      if (state.cycles.length >= 10) {
        const pruneIndex = state.cycles.findIndex((cycle) => cycle.state !== 'open');
        if (pruneIndex < 0) {
          reportDiagnosticsFailure('trace_cycle_collision');
          return undefined;
        }
        state.cycles.splice(pruneIndex, 1);
        state.unknownPriorCycle = true;
      }
      const cycle: TraceCycle = {
        handle: acceptedHandle,
        ordinal: ordinal as GptTraceCycleOrdinalV1,
        seen: new Set([eventType]),
        state: 'open',
      };
      try {
        setWeakMapValue(traceCycleHandleOwners, acceptedHandle, state);
      } catch {
        reportDiagnosticsFailure('trace_cycle_invalid');
        return undefined;
      }
      state.cycles.push(cycle);
      state.nextCycleOrdinal += 1;
      return cycle.ordinal;
    }

    let candidates: TraceCycle[] = [];
    if (acceptedHandle !== undefined) {
      candidates = state.cycles.filter(
        (cycle) => cycle.handle === acceptedHandle && !cycle.seen.has(eventType)
      );
    } else if (responseIdentifier !== undefined) {
      candidates = state.cycles.filter(
        (cycle) => cycle.responseIdentifier === responseIdentifier && !cycle.seen.has(eventType)
      );
      if (candidates.length === 0) {
        const open = state.cycles.filter(
          (cycle) =>
            cycle.state === 'open' &&
            cycle.responseIdentifier === undefined &&
            !cycle.seen.has(eventType)
        );
        if (open.length === 1) candidates = open;
      }
    } else if (!state.unknownPriorCycle) {
      candidates = state.cycles.filter((cycle) => !cycle.seen.has(eventType));
    }
    if (candidates.length !== 1) {
      if (candidates.length > 1) reportDiagnosticsFailure('trace_cycle_ambiguity');
      return undefined;
    }
    const cycle = candidates[0]!;
    cycle.seen.add(eventType);
    if (responseIdentifier !== undefined && cycle.responseIdentifier === undefined) {
      cycle.responseIdentifier = responseIdentifier;
    }
    if (eventType === 'slotRenderEnded') cycle.state = 'completed';
    return cycle.ordinal;
  };

  const diagnosticFact = (
    eventType: string,
    event: unknown,
    observedAtMs: number,
    acceptedHandle: Readonly<GoogletagTraceCycleHandle> | undefined
  ): Readonly<GoogletagDiagnosticsFact> | undefined => {
    try {
      if ((typeof event !== 'object' || event === null) && typeof event !== 'function') {
        return undefined;
      }
      const slot = safeMember(event as object, 'slot');
      if ((typeof slot !== 'object' || slot === null) && typeof slot !== 'function') {
        return undefined;
      }
      const physicalSlot = slot as object;
      const state = diagnosticsSlotState(physicalSlot);
      if (!state) return undefined;
      const responseIdentifierValue = safeMember(event as object, 'responseIdentifier');
      const responseIdentifier =
        typeof responseIdentifierValue === 'string' && responseIdentifierValue.length > 0
          ? responseIdentifierValue
          : undefined;
      const kind = eventType as GoogletagDiagnosticsEventName;
      const cycleOrdinal = traceCycle(state, kind, responseIdentifier, acceptedHandle);
      const safeSlot = Object.freeze({
        token: state.token,
        ...(state.traceToken === undefined ? {} : { traceToken: state.traceToken }),
        ...(state.traceToken === undefined
          ? {}
          : { runtimeSlotNumber: Number.parseInt(state.traceToken.slice(4), 36) }),
        ...(cycleOrdinal === undefined ? {} : { cycleOrdinal }),
        ...(state.elementId === undefined ? {} : { elementId: state.elementId }),
        ...(state.adUnitPath === undefined ? {} : { adUnitPath: state.adUnitPath }),
      });
      const base = {
        kind,
        observedAtMs,
        slot: safeSlot,
        ...(responseIdentifier === undefined ? {} : { responseIdentifier }),
      };
      switch (eventType) {
        case 'slotRequested':
        case 'slotResponseReceived':
        case 'slotOnload':
        case 'impressionViewable':
          return Object.freeze({ ...base, kind: eventType });
        case 'slotVisibilityChanged': {
          const percentage = safeMember(event as object, 'inViewPercentage');
          return typeof percentage === 'number' && Number.isFinite(percentage)
            ? Object.freeze({ ...base, kind: eventType, inViewPercentage: percentage })
            : Object.freeze({ ...base, kind: eventType });
        }
        case 'slotRenderEnded': {
          const isEmpty = safeMember(event as object, 'isEmpty');
          const isBackfill = safeMember(event as object, 'isBackfill');
          const slotContentChanged = safeMember(event as object, 'slotContentChanged');
          const sizeCandidate = safeMember(event as object, 'size');
          let size: readonly [number, number] | undefined;
          if (Array.isArray(sizeCandidate) && sizeCandidate.length === 2) {
            const width = safeMember(sizeCandidate, '0');
            const height = safeMember(sizeCandidate, '1');
            if (
              typeof width === 'number' &&
              Number.isFinite(width) &&
              typeof height === 'number' &&
              Number.isFinite(height)
            ) {
              size = Object.freeze([width, height]);
            }
          }
          const positiveInteger = (name: string): number | undefined => {
            const value = safeMember(event as object, name);
            return typeof value === 'number' && Number.isSafeInteger(value) && value > 0
              ? value
              : undefined;
          };
          const positiveIntegerList = (name: string): readonly number[] | undefined => {
            const value = safeMember(event as object, name);
            if (!Array.isArray(value)) return undefined;
            const result = value
              .map((entry) =>
                typeof entry === 'number' && Number.isSafeInteger(entry) && entry > 0
                  ? entry
                  : undefined
              )
              .filter((entry): entry is number => entry !== undefined)
              .slice(0, 8);
            return result.length === 0 ? undefined : Object.freeze(result);
          };
          const adManagerCandidate = {
            lineItemId: positiveInteger('lineItemId'),
            creativeId: positiveInteger('creativeId'),
            campaignId: positiveInteger('campaignId'),
            advertiserId: positiveInteger('advertiserId'),
            sourceAgnosticLineItemId: positiveInteger('sourceAgnosticLineItemId'),
            sourceAgnosticCreativeId: positiveInteger('sourceAgnosticCreativeId'),
            yieldGroupIds: positiveIntegerList('yieldGroupIds'),
            companyIds: positiveIntegerList('companyIds'),
          };
          const adManager = Object.fromEntries(
            Object.entries(adManagerCandidate).filter(([, value]) => value !== undefined)
          ) as GoogletagDiagnosticsAdManagerIdentity;
          return Object.freeze({
            ...base,
            kind: eventType,
            ...(typeof isEmpty === 'boolean' ? { isEmpty } : {}),
            ...(size ? { size } : {}),
            ...(typeof isBackfill === 'boolean' ? { isBackfill } : {}),
            ...(typeof slotContentChanged === 'boolean' ? { slotContentChanged } : {}),
            ...(Object.keys(adManager).length === 0 ? {} : { adManager: Object.freeze(adManager) }),
          });
        }
        default:
          return undefined;
      }
    } catch {
      return undefined;
    }
  };

  const publishDiagnostics = (
    eventType: string,
    event: unknown,
    acceptedHandle: Readonly<GoogletagTraceCycleHandle> | undefined
  ): void => {
    const observer = diagnosticsObserver;
    if (!observer || disposed) return;
    let observedAtMs = 0;
    try {
      const performance = safeMember(target, 'performance');
      if (
        (typeof performance === 'object' && performance !== null) ||
        typeof performance === 'function'
      ) {
        const now = safeMember(performance as object, 'now');
        if (typeof now === 'function') {
          const value = Reflect.apply(now, performance, []);
          if (typeof value === 'number' && Number.isFinite(value)) observedAtMs = value;
        }
      }
    } catch {
      // A missing or hostile clock cannot suppress the observed GPT fact.
    }
    const fact = diagnosticFact(eventType, event, observedAtMs, acceptedHandle);
    if (!fact) return;
    try {
      observer(fact);
    } catch {
      // Diagnostics observation cannot escape the GPT correctness callback.
    }
  };

  const invokeFacadeCall = (
    callable: (...arguments_: unknown[]) => unknown,
    receiver: unknown,
    arguments_: readonly unknown[]
  ): unknown => {
    const depth = weakMapValue(facadeCalls, callable) ?? 0;
    setWeakMapValue(facadeCalls, callable, depth + 1);
    try {
      return Reflect.apply(callable, receiver, arguments_);
    } finally {
      if (depth === 0) deleteWeakMapValue(facadeCalls, callable);
      else setWeakMapValue(facadeCalls, callable, depth);
    }
  };
  const consumeFacadeCall = (callable: (...arguments_: unknown[]) => unknown): boolean => {
    const depth = weakMapValue(facadeCalls, callable) ?? 0;
    if (depth === 0) return false;
    if (depth === 1) deleteWeakMapValue(facadeCalls, callable);
    else setWeakMapValue(facadeCalls, callable, depth - 1);
    return true;
  };

  const markFirstDisplay = (): void => {
    if (firstDisplayObserved) return;
    firstDisplayObserved = true;
    try {
      const performance = safeMember(target, 'performance');
      if (
        (typeof performance !== 'object' || performance === null) &&
        typeof performance !== 'function'
      ) {
        return;
      }
      const mark = safeMember(performance as object, 'mark');
      if (typeof mark !== 'function') return;
      Reflect.apply(mark, performance, ['tsjs:first-display']);
      const measure = safeMember(performance as object, 'measure');
      if (typeof measure !== 'function') return;
      Reflect.apply(measure, performance, [
        'tsjs:boot-to-first-display',
        'tsjs:bids-script',
        'tsjs:first-display',
      ]);
    } catch {
      // Performance instrumentation cannot change GPT display behavior.
    }
  };

  const registerAdapterEffect = (disposeEffect: () => void): void => {
    const rollback = (): void => {
      try {
        deleteSetValue(effects, disposeEffect);
      } catch {
        // A hostile registry cannot retain the effect being rolled back.
      }
      try {
        disposeEffect();
      } catch {
        // Cleanup cannot replace the publication failure or escape disposal.
      }
    };
    if (disposed) {
      rollback();
      return;
    }
    try {
      effects.add(disposeEffect);
    } catch (error) {
      rollback();
      throw error;
    }
    if (disposed) rollback();
  };

  const replaceMethod = (
    binding: object,
    key: PropertyKey,
    wrapper: (...arguments_: unknown[]) => unknown,
    isCurrent: () => boolean
  ): (() => void) | undefined => {
    let descriptor: PropertyDescriptor | undefined;
    let installed = false;
    const restore = (): void => {
      if (!installed) return;
      installed = false;
      try {
        const current = Object.getOwnPropertyDescriptor(binding, key);
        if (!current || current.value !== wrapper) return;
        if (descriptor) Reflect.defineProperty(binding, key, descriptor);
        else Reflect.deleteProperty(binding, key);
      } catch {
        // Publisher replacement wins over best-effort adapter restoration.
      }
    };
    try {
      descriptor = Object.getOwnPropertyDescriptor(binding, key);
      if (!isCurrent()) return undefined;
      if (
        descriptor &&
        (!Object.prototype.hasOwnProperty.call(descriptor, 'value') ||
          (descriptor.configurable !== true && descriptor.writable !== true))
      ) {
        return undefined;
      }
      const replacement = descriptor
        ? { ...descriptor, value: wrapper }
        : { configurable: true, enumerable: true, value: wrapper, writable: true };
      if (!isCurrent()) return undefined;
      if (!Reflect.defineProperty(binding, key, replacement)) return undefined;
      installed = true;
      if (!isCurrent() || safeMember(binding, key) !== wrapper || !isCurrent()) {
        restore();
        return undefined;
      }
    } catch {
      restore();
      return undefined;
    }
    return restore;
  };

  const syncInitialLoadDisabled = (
    binding: object,
    tracker: { disabled: boolean },
    isCurrent?: () => boolean
  ): boolean => {
    const getConfig = safeMember(binding, 'getConfig');
    if (isCurrent && !isCurrent()) return false;
    if (typeof getConfig !== 'function') return false;
    try {
      const config = Reflect.apply(getConfig, binding, ['disableInitialLoad']);
      if (isCurrent && !isCurrent()) return false;
      if ((typeof config !== 'object' || config === null) && typeof config !== 'function') {
        return false;
      }
      const value = safeMember(config, 'disableInitialLoad');
      if (isCurrent && !isCurrent()) return false;
      if (value === undefined) return false;
      tracker.disabled = value === true;
      return true;
    } catch {
      return false;
    }
  };

  const syncExplicitInitialLoad = (candidate: unknown, tracker: { disabled: boolean }): boolean => {
    try {
      if (
        (typeof candidate !== 'object' || candidate === null) &&
        typeof candidate !== 'function'
      ) {
        return false;
      }
      const descriptor = Object.getOwnPropertyDescriptor(candidate, 'disableInitialLoad');
      if (!descriptor || !Object.prototype.hasOwnProperty.call(descriptor, 'value')) return false;
      tracker.disabled = descriptor.value === true;
      return true;
    } catch {
      return false;
    }
  };

  const releaseInitialLoadBinding = (binding: object): void => {
    const release = mapValue(initialLoadReleases, binding);
    if (!release) return;
    try {
      deleteMapValue(initialLoadReleases, binding);
    } finally {
      try {
        deleteSetValue(effects, release);
      } catch {
        // A hostile registry cannot retain adapter ownership of an old binding.
      } finally {
        try {
          release();
        } catch {
          // One historical binding cannot interrupt release of later bindings.
        }
      }
    }
  };

  const releaseHistoricalInitialLoadBindings = (current?: object): void => {
    for (const binding of [...mapKeys(initialLoadReleases)]) {
      if (binding === current) continue;
      try {
        releaseInitialLoadBinding(binding);
      } catch {
        // One historical binding cannot interrupt release of later bindings.
      }
    }
  };

  const rollbackNotificationArming = (binding: object): void => {
    let released = false;
    try {
      deleteWeakSetValue(armedBindings, binding);
      released = !armedBindings.has(binding);
    } catch {
      // A poisoned registry cannot prove that the exact marker was removed.
    }
    if (!released) armedBindings = new WeakSet<object>();
  };

  const ensureInitialLoadTracking = (
    expected: PresentGoogletag,
    knownService?: object
  ): SharedInitialLoadTracker | undefined => {
    const expectedCurrent = (): boolean => {
      if (disposed) return false;
      const current = sameBinding(expected);
      return !disposed && current;
    };
    if (!expectedCurrent()) return undefined;

    let tracker = weakMapValue(sharedInitialLoadTrackers, expected.binding);
    if (!tracker) {
      tracker = {
        disabled: false,
        rootWrapped: false,
        owners: new Set<object>(),
        restorers: new Set<() => void>(),
        services: new WeakMap<object, () => void>(),
      };
      try {
        sharedInitialLoadTrackers.set(expected.binding, tracker);
      } catch (error) {
        if (weakMapValue(sharedInitialLoadTrackers, expected.binding) === tracker) {
          deleteWeakMapValue(sharedInitialLoadTrackers, expected.binding);
        }
        throw error;
      }
    }
    const ownsInitialLoad = (): boolean => {
      try {
        return tracker!.owners.has(initialLoadOwner);
      } catch {
        return false;
      }
    };
    const trackingCurrent = (): boolean => {
      if (
        disposed ||
        weakMapValue(sharedInitialLoadTrackers, expected.binding) !== tracker ||
        !ownsInitialLoad()
      ) {
        return false;
      }
      const current = sameBinding(expected);
      return (
        !disposed &&
        current &&
        weakMapValue(sharedInitialLoadTrackers, expected.binding) === tracker &&
        ownsInitialLoad()
      );
    };
    let adoptedHere = false;
    let alreadyAdopted: boolean;
    try {
      alreadyAdopted = initialLoadReleases.has(expected.binding);
    } catch {
      if (
        tracker.owners.size === 0 &&
        weakMapValue(sharedInitialLoadTrackers, expected.binding) === tracker
      ) {
        deleteWeakMapValue(sharedInitialLoadTrackers, expected.binding);
      }
      return undefined;
    }
    if (!alreadyAdopted) {
      if (!expectedCurrent()) {
        if (
          tracker.owners.size === 0 &&
          weakMapValue(sharedInitialLoadTrackers, expected.binding) === tracker
        ) {
          deleteWeakMapValue(sharedInitialLoadTrackers, expected.binding);
        }
        return undefined;
      }
      try {
        tracker.owners.add(initialLoadOwner);
      } catch (error) {
        try {
          deleteSetValue(tracker.owners, initialLoadOwner);
        } finally {
          if (
            tracker.owners.size === 0 &&
            weakMapValue(sharedInitialLoadTrackers, expected.binding) === tracker
          ) {
            deleteWeakMapValue(sharedInitialLoadTrackers, expected.binding);
          }
        }
        throw error;
      }
      const adoptedTracker = tracker;
      const release = (): void => {
        try {
          if (mapValue(initialLoadReleases, expected.binding) === release) {
            deleteMapValue(initialLoadReleases, expected.binding);
          }
        } finally {
          const removedLastOwner =
            deleteSetValue(adoptedTracker.owners, initialLoadOwner) &&
            adoptedTracker.owners.size === 0;
          if (removedLastOwner) {
            if (weakMapValue(sharedInitialLoadTrackers, expected.binding) === adoptedTracker) {
              deleteWeakMapValue(sharedInitialLoadTrackers, expected.binding);
            }
            for (const restore of [...adoptedTracker.restorers].reverse()) {
              try {
                restore();
              } catch {
                // One restoration cannot interrupt cleanup of the shared tracker.
              }
            }
          }
        }
      };
      try {
        initialLoadReleases.set(expected.binding, release);
      } catch (error) {
        try {
          if (mapValue(initialLoadReleases, expected.binding) === release) {
            deleteMapValue(initialLoadReleases, expected.binding);
          }
        } finally {
          release();
        }
        throw error;
      }
      registerAdapterEffect(release);
      adoptedHere = true;
    }
    const installedHere: Array<() => void> = [];
    const rollback = (): undefined => {
      for (const restore of [...installedHere].reverse()) restore();
      if (adoptedHere) {
        releaseInitialLoadBinding(expected.binding);
      }
      return undefined;
    };
    if (!trackingCurrent()) return rollback();
    syncInitialLoadDisabled(expected.binding, tracker, trackingCurrent);
    if (!trackingCurrent()) return rollback();
    if (!tracker.rootWrapped) {
      const originalSetConfig = safeMember(expected.binding, 'setConfig');
      if (!trackingCurrent()) return rollback();
      if (typeof originalSetConfig === 'function') {
        const wrapper = function (this: unknown, ...arguments_: unknown[]): unknown {
          const result = Reflect.apply(originalSetConfig, this, arguments_);
          if (!syncInitialLoadDisabled(expected.binding, tracker!)) {
            syncExplicitInitialLoad(arguments_[0], tracker!);
          }
          return result;
        };
        const restore = replaceMethod(expected.binding, 'setConfig', wrapper, trackingCurrent);
        if (restore) {
          let active = true;
          const cleanup = (): void => {
            if (!active) return;
            active = false;
            try {
              deleteSetValue(tracker!.restorers, cleanup);
            } finally {
              tracker!.rootWrapped = false;
              restore();
            }
          };
          tracker.rootWrapped = true;
          try {
            tracker.restorers.add(cleanup);
          } catch (error) {
            cleanup();
            rollback();
            throw error;
          }
          installedHere.push(cleanup);
        }
        if (!trackingCurrent()) return rollback();
      }
    }

    const trackService = (service: object): boolean => {
      try {
        if (tracker!.services.has(service)) return true;
      } catch {
        return false;
      }
      if (!trackingCurrent()) return false;
      const originalDisable = safeMember(service, 'disableInitialLoad');
      if (!trackingCurrent()) return false;
      if (typeof originalDisable !== 'function') return true;
      const wrapper = function (this: unknown, ...arguments_: unknown[]): unknown {
        const result = Reflect.apply(originalDisable, this, arguments_);
        if (!syncInitialLoadDisabled(expected.binding, tracker!)) tracker!.disabled = true;
        return result;
      };
      const restore = replaceMethod(service, 'disableInitialLoad', wrapper, trackingCurrent);
      if (restore) {
        let active = true;
        const cleanup = (): void => {
          if (!active) return;
          active = false;
          try {
            deleteSetValue(tracker!.restorers, cleanup);
          } finally {
            try {
              if (weakMapValue(tracker!.services, service) === cleanup) {
                deleteWeakMapValue(tracker!.services, service);
              }
            } finally {
              restore();
            }
          }
        };
        try {
          tracker!.services.set(service, cleanup);
        } catch (error) {
          try {
            cleanup();
          } finally {
            rollback();
          }
          throw error;
        }
        try {
          tracker!.restorers.add(cleanup);
        } catch (error) {
          cleanup();
          rollback();
          throw error;
        }
        installedHere.push(cleanup);
      }
      if (!trackingCurrent()) return false;
      return true;
    };

    if (knownService) {
      if (!trackService(knownService)) return rollback();
    } else {
      if (!trackingCurrent()) return rollback();
      let service: unknown;
      try {
        service = Reflect.apply(expected.pubads, expected.binding, []);
      } catch {
        return rollback();
      }
      if (!trackingCurrent()) return rollback();
      if ((typeof service === 'object' && service !== null) || typeof service === 'function') {
        if (!trackService(service as object)) return rollback();
      }
    }
    if (!trackingCurrent()) return rollback();
    return tracker;
  };

  const currentBinding = (): ReturnType<typeof inspectBinding> => {
    for (let attempt = 0; attempt < 2; attempt += 1) {
      const value = readTarget(target);
      const inspected = inspectBinding(value);
      if (readTarget(target) === value) {
        const current =
          inspected.status === 'present' ? inspected.value.binding : inspected.binding;
        releaseHistoricalInitialLoadBindings(current);
        return inspected;
      }
    }
    releaseHistoricalInitialLoadBindings();
    return { status: 'incompatible' };
  };

  const sameBinding = (expected: PresentGoogletag): boolean => {
    const canonicalAdapterMethod = (
      candidate: (...arguments_: unknown[]) => unknown
    ): ((...arguments_: unknown[]) => unknown) | undefined => {
      let current = candidate;
      for (let depth = 0; depth < 16; depth += 1) {
        const origin = weakMapValue(adapterMethodOrigins, current);
        if (!origin) return current;
        if (origin === current) return undefined;
        current = origin;
      }
      return undefined;
    };
    const sameAdapterMethod = (
      left: (...arguments_: unknown[]) => unknown,
      right: (...arguments_: unknown[]) => unknown
    ): boolean => {
      if (left === right) return true;
      const canonicalLeft = canonicalAdapterMethod(left);
      return canonicalLeft !== undefined && canonicalLeft === canonicalAdapterMethod(right);
    };
    const matchesCapturedBinding = (): boolean => {
      const inspected = inspectBinding(expected.binding);
      return (
        inspected.status === 'present' &&
        inspected.value.commandQueue.binding === expected.commandQueue.binding &&
        inspected.value.commandQueue.push === expected.commandQueue.push &&
        sameAdapterMethod(inspected.value.display, expected.display) &&
        inspected.value.pubads === expected.pubads
      );
    };
    if (readTarget(target) !== expected.binding) {
      releaseInitialLoadBinding(expected.binding);
      return false;
    }
    const firstMatch = matchesCapturedBinding();
    if (readTarget(target) !== expected.binding) {
      releaseInitialLoadBinding(expected.binding);
      return false;
    }
    const secondMatch = matchesCapturedBinding();
    if (readTarget(target) !== expected.binding) {
      releaseInitialLoadBinding(expected.binding);
      return false;
    }
    return firstMatch && secondMatch;
  };

  const removePending = (operation: PendingOperation<unknown>): void => {
    const index = pending.indexOf(operation);
    if (index >= 0) pending.splice(index, 1);
  };

  const releasePendingReservation = (operation: PendingOperation<unknown>): void => {
    if (!operation.pendingReservation) return;
    operation.pendingReservation = false;
    if (pendingReservations > 0) pendingReservations -= 1;
  };

  const clearReadiness = (operation: PendingOperation<unknown>): void => {
    try {
      if (operation.timeout !== undefined) {
        clearTimeout(operation.timeout);
        operation.timeout = undefined;
      }
      removePending(operation);
    } finally {
      releasePendingReservation(operation);
    }
  };

  const detachAbort = (operation: PendingOperation<unknown>): void => {
    const registration = operation.abortRegistration;
    if (!registration || !registration.attempted) return;
    if (registration.installing) {
      registration.cleanupRequested = true;
      return;
    }
    registration.attempted = false;
    operation.abortRegistration = undefined;
    try {
      Reflect.apply(registration.remove, registration.binding, ['abort', registration.listener]);
    } catch {
      // Hostile signal cleanup cannot strand operation settlement.
    }
  };

  const clearOperation = (operation: PendingOperation<unknown>): void => {
    try {
      clearReadiness(operation);
    } finally {
      try {
        detachAbort(operation);
      } finally {
        deleteSetValue(live, operation);
      }
    }
  };

  const rollbackOperationEffects = (operation: PendingOperation<unknown>): void => {
    for (let index = operation.provisionalEffects.length - 1; index >= 0; index -= 1) {
      operation.provisionalEffects[index]?.release();
    }
    operation.provisionalEffects.length = 0;
  };

  const rejectOperation = (operation: PendingOperation<unknown>, error: unknown): void => {
    if (operation.settled) return;
    operation.settled = true;
    if (error instanceof GoogletagAdapterError && error.code === 'external_artifact_incompatible') {
      operation.state = 'incompatible';
    }
    try {
      rollbackOperationEffects(operation);
    } finally {
      try {
        clearOperation(operation);
      } finally {
        operation.reject(error);
      }
    }
  };

  const fail = (operation: PendingOperation<unknown>, code: GoogletagAdapterErrorCode): void => {
    if (operation.settled) return;
    if (code === 'external_ready_timeout') operation.state = 'timed_out';
    if (code === 'external_artifact_incompatible') operation.state = 'incompatible';
    rejectOperation(operation, new GoogletagAdapterError(code));
  };

  const dispatch = (operation: PendingOperation<unknown>, binding: PresentGoogletag): void => {
    if (operation.settled) return;
    if (disposed) {
      fail(operation, 'operation_disposed');
      return;
    }
    operation.state = 'present';
    clearReadiness(operation);
    const isDispatchCurrent = (): boolean => {
      if (disposed || operation.settled) return false;
      const current = sameBinding(binding);
      return !disposed && !operation.settled && current;
    };
    const registerOperationEffect = (disposeEffect: () => void): (() => void) => {
      let released = false;
      let promoted = false;
      const release = (): void => {
        if (promoted) {
          try {
            deleteSetValue(effects, release);
          } catch {
            // A hostile registry cannot prevent exact external cleanup.
          }
        }
        if (released) return;
        released = true;
        try {
          disposeEffect();
        } catch {
          // One effect cleanup cannot escape the adapter boundary.
        }
      };
      const promote = (): void => {
        if (released || promoted) return;
        promoted = true;
        try {
          effects.add(release);
        } catch (error) {
          release();
          throw error;
        }
        if (!isDispatchCurrent()) {
          release();
          throw new GoogletagAdapterError(
            disposed ? 'operation_disposed' : 'external_artifact_incompatible'
          );
        }
      };
      const provisional = { promote, release };
      operation.provisionalEffects[operation.provisionalEffects.length] = provisional;
      if (!isDispatchCurrent()) {
        release();
        throw new GoogletagAdapterError(
          disposed ? 'operation_disposed' : 'external_artifact_incompatible'
        );
      }
      return release;
    };
    const promoteOperationEffects = (): void => {
      for (const provisional of operation.provisionalEffects) provisional.promote();
      operation.provisionalEffects.length = 0;
    };
    const completeOperation = (value: unknown): void => {
      if (operation.settled) return;
      if (disposed) {
        fail(operation, 'operation_disposed');
        return;
      }
      if (!isDispatchCurrent()) {
        fail(operation, 'external_artifact_incompatible');
        return;
      }
      try {
        promoteOperationEffects();
      } catch (error) {
        if (!operation.settled) rejectOperation(operation, error);
        return;
      }
      if (operation.settled) return;
      if (disposed) {
        fail(operation, 'operation_disposed');
        return;
      }
      if (!isDispatchCurrent()) {
        fail(operation, 'external_artifact_incompatible');
        return;
      }
      operation.settled = true;
      try {
        clearOperation(operation);
      } finally {
        operation.resolve(value);
      }
    };
    const settleCommandValue = (value: unknown): void => {
      let then: unknown;
      try {
        if ((typeof value === 'object' && value !== null) || typeof value === 'function') {
          then = Reflect.get(value, 'then');
        }
      } catch (error) {
        rejectOperation(operation, error);
        return;
      }
      if (typeof then !== 'function') {
        completeOperation(value);
        return;
      }
      Promise.resolve(value).then(
        (resolved) => completeOperation(resolved),
        (error: unknown) => rejectOperation(operation, error)
      );
    };
    let bindingToken = weakMapValue(bindingTokens, binding.binding);
    if (!bindingToken) {
      bindingToken = Object.freeze({});
      setWeakMapValue(bindingTokens, binding.binding, bindingToken);
    }
    const facade = createFacade(
      binding,
      registerOperationEffect,
      isDispatchCurrent,
      () => !disposed && sameBinding(binding),
      (service) => {
        const tracker = ensureInitialLoadTracking(binding, service);
        return tracker?.disabled === true;
      },
      targetingObservations,
      bindingToken,
      markFirstDisplay,
      invokeFacadeCall,
      consumeFacadeCall,
      publishDiagnostics
    );
    try {
      if (disposed) {
        fail(operation, 'operation_disposed');
        return;
      }
      if (operation.settled) return;
      ensureInitialLoadTracking(binding);
      if (disposed) {
        fail(operation, 'operation_disposed');
        return;
      }
      if (operation.settled) return;
      if (!isDispatchCurrent()) {
        if (disposed) fail(operation, 'operation_disposed');
        else fail(operation, 'external_artifact_incompatible');
        return;
      }
      if (disposed) {
        fail(operation, 'operation_disposed');
        return;
      }
      if (operation.settled) {
        return;
      }
      queueCommand(
        binding.commandQueue,
        () => {
          if (operation.settled) return;
          if (disposed) {
            fail(operation, 'operation_disposed');
            return;
          }
          if (!isDispatchCurrent()) {
            if (disposed) fail(operation, 'operation_disposed');
            else fail(operation, 'external_artifact_incompatible');
            return;
          }
          try {
            const value = operation.command(facade);
            if (operation.settled) return;
            if (disposed) {
              fail(operation, 'operation_disposed');
              return;
            }
            if (!isDispatchCurrent()) {
              if (disposed) fail(operation, 'operation_disposed');
              else fail(operation, 'external_artifact_incompatible');
              return;
            }
            settleCommandValue(value);
          } catch (error) {
            if (operation.settled) return;
            if (disposed) {
              fail(operation, 'operation_disposed');
              return;
            }
            rejectOperation(operation, error);
          }
        },
        isDispatchCurrent
      );
    } catch (error) {
      if (operation.settled) return;
      if (disposed) {
        fail(operation, 'operation_disposed');
        return;
      }
      rejectOperation(operation, error);
    }
  };

  const notifyReady = (expectedBinding?: object): void => {
    if (disposed) return;
    const current = currentBinding();
    if (disposed) return;
    if (current.status === 'present') {
      for (const operation of [...pending]) dispatch(operation, current.value);
      return;
    }
    if (current.status === 'pending') {
      armNotification();
      return;
    }
    if (expectedBinding !== undefined && current.binding !== expectedBinding) {
      return;
    }
    for (const operation of [...pending]) {
      if (
        expectedBinding === undefined ||
        operation.readinessBinding === undefined ||
        operation.readinessBinding === expectedBinding
      ) {
        fail(operation, 'external_artifact_incompatible');
      }
    }
  };

  const armNotification = (): void => {
    const current = currentBinding();
    if (disposed) return;
    if (current.status !== 'pending' || !current.binding || !current.commandQueue) {
      return;
    }
    let alreadyArmed = false;
    try {
      alreadyArmed = armedBindings.has(current.binding);
    } catch {
      armedBindings = new WeakSet<object>();
    }
    if (alreadyArmed) return;
    for (const operation of pending) operation.readinessBinding = current.binding;
    try {
      armedBindings.add(current.binding);
    } catch {
      rollbackNotificationArming(current.binding);
      return;
    }
    let notificationActive = true;
    const notify = (): void => {
      if (!notificationActive) return;
      notificationActive = false;
      notifyReady(current.binding);
    };
    try {
      queueCommand(current.commandQueue, notify);
    } catch {
      notificationActive = false;
      rollbackNotificationArming(current.binding);
    }
  };

  const run = <T>(
    command: (googletag: Readonly<GoogletagFacade>) => T,
    options: GoogletagOperationOptions = {}
  ): GoogletagOperation<T> => {
    if (disposed) throw new GoogletagAdapterError('operation_disposed');
    const current = currentBinding();
    if (disposed) throw new GoogletagAdapterError('operation_disposed');
    if (current.status === 'pending') {
      if (pendingReservations >= MAX_PENDING_OPERATIONS) {
        throw new GoogletagAdapterError('external_queue_full');
      }
      pendingReservations += 1;
    }

    let resolve!: (value: T | PromiseLike<T>) => void;
    let reject!: (reason: unknown) => void;
    const result = new Promise<T>((resolveResult, rejectResult) => {
      resolve = resolveResult;
      reject = rejectResult;
    });
    const operation: PendingOperation<T> = {
      state: current.status,
      settled: false,
      pendingReservation: current.status === 'pending',
      timeout: undefined,
      command,
      resolve,
      reject,
      abortRegistration: undefined,
      readinessBinding: current.status === 'pending' ? current.binding : undefined,
      provisionalEffects: [],
    };
    const handle = Object.freeze({
      get status(): GoogletagOperationStatus {
        return operation.state;
      },
      result,
      dispose: (): void => fail(operation as PendingOperation<unknown>, 'operation_disposed'),
    });

    try {
      live.add(operation as PendingOperation<unknown>);
    } catch (error) {
      try {
        deleteSetValue(live, operation as PendingOperation<unknown>);
      } catch {
        // Publication rollback preserves the original registry failure.
      }
      releasePendingReservation(operation as PendingOperation<unknown>);
      throw error;
    }
    if (current.status === 'pending') {
      pending[pending.length] = operation as PendingOperation<unknown>;
      operation.timeout = setTimeout(
        () => fail(operation as PendingOperation<unknown>, 'external_ready_timeout'),
        EXTERNAL_READY_TIMEOUT_MS
      );
    }

    if (disposed) {
      fail(operation as PendingOperation<unknown>, 'operation_disposed');
      return handle;
    }
    if (operation.settled) return handle;

    let signal: unknown;
    try {
      signal = options.signal;
    } catch (error) {
      rejectOperation(operation as PendingOperation<unknown>, error);
      return handle;
    }
    if (operation.settled) return handle;
    if (disposed) {
      fail(operation as PendingOperation<unknown>, 'operation_disposed');
      return handle;
    }
    if (signal !== undefined) {
      if ((typeof signal !== 'object' || signal === null) && typeof signal !== 'function') {
        rejectOperation(
          operation as PendingOperation<unknown>,
          new TypeError('Invalid AbortSignal')
        );
        return handle;
      }
      let aborted: unknown;
      let add: unknown;
      let remove: unknown;
      try {
        aborted = Reflect.get(signal, 'aborted');
        if (operation.settled) return handle;
        add = Reflect.get(signal, 'addEventListener');
        if (operation.settled) return handle;
        remove = Reflect.get(signal, 'removeEventListener');
      } catch (error) {
        if (!operation.settled) rejectOperation(operation as PendingOperation<unknown>, error);
        return handle;
      }
      if (operation.settled) return handle;
      if (disposed) {
        fail(operation as PendingOperation<unknown>, 'operation_disposed');
        return handle;
      }
      if (aborted === true) {
        fail(operation as PendingOperation<unknown>, 'caller_aborted');
        return handle;
      }
      if (typeof add !== 'function' || typeof remove !== 'function') {
        rejectOperation(
          operation as PendingOperation<unknown>,
          new TypeError('Invalid AbortSignal')
        );
        return handle;
      }
      const registration: AbortRegistration = {
        binding: signal,
        listener: () => fail(operation as PendingOperation<unknown>, 'caller_aborted'),
        remove: remove as (...arguments_: unknown[]) => unknown,
        attempted: true,
        cleanupRequested: false,
        installing: true,
      };
      operation.abortRegistration = registration;
      try {
        Reflect.apply(add, signal, ['abort', registration.listener, { once: true }]);
      } catch (error) {
        registration.installing = false;
        detachAbort(operation as PendingOperation<unknown>);
        if (!operation.settled) rejectOperation(operation as PendingOperation<unknown>, error);
        return handle;
      }
      registration.installing = false;
      if (registration.cleanupRequested || operation.settled || disposed) {
        detachAbort(operation as PendingOperation<unknown>);
      }
      if (operation.settled) return handle;
      if (disposed) {
        fail(operation as PendingOperation<unknown>, 'operation_disposed');
        return handle;
      }
      let abortedAfterRegistration: unknown;
      try {
        abortedAfterRegistration = Reflect.get(signal, 'aborted');
      } catch (error) {
        if (!operation.settled) rejectOperation(operation as PendingOperation<unknown>, error);
        return handle;
      }
      if (operation.settled) return handle;
      if (disposed) {
        fail(operation as PendingOperation<unknown>, 'operation_disposed');
        return handle;
      }
      if (abortedAfterRegistration === true) {
        fail(operation as PendingOperation<unknown>, 'caller_aborted');
        return handle;
      }
    }

    if (current.status === 'incompatible') {
      operation.settled = true;
      clearOperation(operation as PendingOperation<unknown>);
      operation.reject(new GoogletagAdapterError('external_artifact_incompatible'));
    } else if (current.status === 'present') {
      dispatch(operation as PendingOperation<unknown>, current.value);
    } else {
      armNotification();
    }
    return handle;
  };

  const observePublisherCalls = (observer: GoogletagPublisherCallObserver): (() => void) => {
    if (disposed) throw new GoogletagAdapterError('operation_disposed');
    if (typeof observer !== 'object' || observer === null) {
      throw new TypeError('GPT publisher observer must be an object');
    }
    const observerMethod = <Key extends keyof GoogletagPublisherCallObserver>(
      key: Key
    ): GoogletagPublisherCallObserver[Key] | undefined => {
      const descriptor = Object.getOwnPropertyDescriptor(observer, key);
      if (!descriptor) return undefined;
      if (!Object.prototype.hasOwnProperty.call(descriptor, 'value')) {
        throw new TypeError('GPT publisher observer methods must be own data properties');
      }
      if (descriptor.value !== undefined && typeof descriptor.value !== 'function') {
        throw new TypeError('GPT publisher observer methods must be functions');
      }
      return descriptor.value as GoogletagPublisherCallObserver[Key] | undefined;
    };
    const defineObserver = observerMethod('defineSlot');
    const destroyObserver = observerMethod('destroySlots');
    const displayObserver = observerMethod('display');
    const refreshObserver = observerMethod('refresh');
    const current = currentBinding();
    if (current.status === 'pending' && current.commandQueue) {
      const normalizedObserver: GoogletagPublisherCallObserver = Object.freeze({
        ...(defineObserver ? { defineSlot: defineObserver } : {}),
        ...(destroyObserver ? { destroySlots: destroyObserver } : {}),
        ...(displayObserver ? { display: displayObserver } : {}),
        ...(refreshObserver ? { refresh: refreshObserver } : {}),
      });
      let released = false;
      let notificationActive = true;
      let installedRelease: (() => void) | undefined;
      const release = (): void => {
        if (released) return;
        released = true;
        notificationActive = false;
        try {
          deleteSetValue(effects, release);
        } catch {
          // Exact deferred restoration still runs when bookkeeping is hostile.
        }
        installedRelease?.();
      };
      try {
        queueCommand(current.commandQueue, () => {
          if (!notificationActive || released || disposed) return;
          notificationActive = false;
          const ready = currentBinding();
          if (ready.status !== 'present') return;
          try {
            installedRelease = observePublisherCalls(normalizedObserver);
            if (released) installedRelease();
          } catch {
            // Readiness mediation cannot escape the publisher-owned command queue.
          }
        });
      } catch (error) {
        notificationActive = false;
        released = true;
        throw error;
      }
      registerAdapterEffect(release);
      return release;
    }
    if (current.status !== 'present') return (): void => undefined;
    const service = Reflect.apply(current.value.pubads, current.value.binding, []);
    if ((typeof service !== 'object' || service === null) && typeof service !== 'function') {
      throw new GoogletagAdapterError('external_artifact_incompatible');
    }
    const serviceObject = service as object;
    const currentBindingObject = current.value.binding;
    const tracker = ensureInitialLoadTracking(current.value, serviceObject);
    const stillCurrent = (): boolean =>
      !disposed &&
      readTarget(target) === currentBindingObject &&
      Reflect.apply(current.value.pubads, currentBindingObject, []) === serviceObject;
    const safelyCurrent = (): boolean => {
      try {
        return stillCurrent();
      } catch {
        return false;
      }
    };
    const publisherAdmission = (decision: unknown): GoogletagPublisherCallAdmission | undefined => {
      if ((typeof decision !== 'object' || decision === null) && typeof decision !== 'function') {
        return undefined;
      }
      const candidate = safeMember(decision as object, 'admission');
      if (
        (typeof candidate !== 'object' || candidate === null) &&
        typeof candidate !== 'function'
      ) {
        return undefined;
      }
      const commit = safeMember(candidate as object, 'commit');
      const rollback = safeMember(candidate as object, 'rollback');
      if (typeof commit !== 'function' || typeof rollback !== 'function') return undefined;
      return Object.freeze({
        commit: (): void => {
          Reflect.apply(commit, candidate, []);
        },
        rollback: (): void => {
          Reflect.apply(rollback, candidate, []);
        },
      });
    };
    const commitAdmission = (admission: GoogletagPublisherCallAdmission | undefined): void => {
      try {
        admission?.commit();
      } catch {
        // Post-native bookkeeping cannot alter the publisher return value.
      }
    };
    const rollbackAdmission = (admission: GoogletagPublisherCallAdmission | undefined): void => {
      try {
        admission?.rollback();
      } catch {
        // Rollback cannot replace the exact publisher-native failure.
      }
    };
    const callWithAdmission = (
      original: (...arguments_: unknown[]) => unknown,
      receiver: unknown,
      arguments_: readonly unknown[],
      admission: GoogletagPublisherCallAdmission | undefined
    ): unknown => {
      let result: unknown;
      try {
        result = Reflect.apply(original, receiver, arguments_);
      } catch (error) {
        rollbackAdmission(admission);
        throw error;
      }
      commitAdmission(admission);
      return result;
    };
    const objectSlots = (candidate: unknown): readonly object[] | undefined => {
      if (
        !Array.isArray(candidate) ||
        candidate.some(
          (slot) => (typeof slot !== 'object' || slot === null) && typeof slot !== 'function'
        )
      ) {
        return undefined;
      }
      return Object.freeze([...candidate]) as readonly object[];
    };
    const allSlots = (): readonly object[] | undefined => {
      const getSlots = safeMember(serviceObject, 'getSlots');
      if (typeof getSlots !== 'function') return undefined;
      try {
        return objectSlots(Reflect.apply(getSlots, serviceObject, []));
      } catch {
        return undefined;
      }
    };
    const deferredRefreshes = new Set<() => void>();
    const restorers: Array<() => void> = [];
    const install = (
      external: object,
      key: PropertyKey,
      mediate: (
        original: (...arguments_: unknown[]) => unknown,
        receiver: unknown,
        arguments_: readonly unknown[]
      ) => unknown
    ): void => {
      const original = safeMember(external, key);
      if (typeof original !== 'function') return;
      const callable = original as (...arguments_: unknown[]) => unknown;
      const wrapper = function (this: unknown, ...arguments_: unknown[]): unknown {
        if (consumeFacadeCall(wrapper)) {
          return Reflect.apply(callable, this, arguments_);
        }
        if (!safelyCurrent()) {
          return Reflect.apply(callable, this, arguments_);
        }
        return mediate(callable, this, arguments_);
      };
      setWeakMapValue(adapterMethodOrigins, wrapper, callable);
      const restoreMethod = replaceMethod(external, key, wrapper, stillCurrent);
      if (!restoreMethod) {
        deleteWeakMapValue(adapterMethodOrigins, wrapper);
        throw new GoogletagAdapterError('external_artifact_incompatible');
      }
      restorers[restorers.length] = (): void => {
        try {
          restoreMethod();
        } finally {
          deleteWeakMapValue(adapterMethodOrigins, wrapper);
        }
      };
    };
    try {
      install(currentBindingObject, 'defineSlot', (original, receiver, arguments_) => {
        if (!defineObserver || arguments_.length !== 3) {
          return Reflect.apply(original, receiver, arguments_);
        }
        try {
          const decision = defineObserver(
            Object.freeze({
              adUnitPath: arguments_[0],
              sizes: arguments_[1],
              elementId: arguments_[2],
              initialLoadDisabled: tracker?.disabled === true,
            })
          );
          if (
            decision?.action === 'handoff' &&
            ((typeof decision.slot === 'object' && decision.slot !== null) ||
              typeof decision.slot === 'function')
          ) {
            return decision.slot;
          }
        } catch {
          // Observer failure must leave the publisher call native.
        }
        return Reflect.apply(original, receiver, arguments_);
      });
      install(currentBindingObject, 'display', (original, receiver, arguments_) => {
        if (displayObserver && arguments_.length === 1) {
          let decision: ReturnType<NonNullable<GoogletagPublisherCallObserver['display']>>;
          try {
            decision = displayObserver(
              Object.freeze({
                target: arguments_[0],
                initialLoadDisabled: tracker?.disabled === true,
              })
            );
          } catch {
            // Observer failure must leave the publisher call native.
            return Reflect.apply(original, receiver, arguments_);
          }
          const admission = publisherAdmission(decision);
          if (decision?.action === 'suppress') {
            rollbackAdmission(admission);
            return undefined;
          }
          return callWithAdmission(original, receiver, arguments_, admission);
        }
        return Reflect.apply(original, receiver, arguments_);
      });
      install(serviceObject, 'refresh', (original, receiver, arguments_) => {
        if (refreshObserver && arguments_.length <= 2) {
          const requested = arguments_[0] === undefined ? undefined : objectSlots(arguments_[0]);
          const effective = requested ?? (arguments_[0] === undefined ? allSlots() : undefined);
          if (effective) {
            let decision: ReturnType<NonNullable<GoogletagPublisherCallObserver['refresh']>>;
            try {
              decision = refreshObserver(
                Object.freeze({
                  requestedSlots: requested,
                  slots: effective,
                  options: arguments_[1],
                })
              );
            } catch {
              // Observer failure must leave the publisher call native.
              return Reflect.apply(original, receiver, arguments_);
            }
            const admission = publisherAdmission(decision);
            if (decision?.action === 'suppress') {
              rollbackAdmission(admission);
              return undefined;
            }
            if (decision?.action === 'replace') {
              const replacement = objectSlots(decision.slots);
              if (replacement) {
                return callWithAdmission(
                  original,
                  receiver,
                  [replacement, ...arguments_.slice(1)],
                  admission
                );
              }
              rollbackAdmission(admission);
              return Reflect.apply(original, receiver, arguments_);
            }
            if (decision?.action === 'defer') {
              const replacement = objectSlots(decision.slots);
              const completion = safeMember(decision, 'completion');
              const then =
                (typeof completion === 'object' && completion !== null) ||
                typeof completion === 'function'
                  ? safeMember(completion as object, 'then')
                  : undefined;
              if (!replacement || typeof then !== 'function') {
                rollbackAdmission(admission);
                return Reflect.apply(original, receiver, arguments_);
              }
              let forwarded = false;
              const forward = (): void => {
                if (forwarded) return;
                forwarded = true;
                try {
                  deleteSetValue(deferredRefreshes, forward);
                } catch {
                  // The exact-once latch remains authoritative under hostile bookkeeping.
                }
                try {
                  callWithAdmission(original, receiver, [replacement, arguments_[1]], admission);
                } catch {
                  // A deferred native throw has no synchronous publisher frame to receive it.
                }
              };
              try {
                addSetValue(deferredRefreshes, forward);
                Promise.resolve(completion).then(forward, forward);
              } catch {
                try {
                  deleteSetValue(deferredRefreshes, forward);
                } catch {
                  // Synchronous fail-open still owns the only native forward.
                }
                return callWithAdmission(
                  original,
                  receiver,
                  [replacement, arguments_[1]],
                  admission
                );
              }
              return undefined;
            }
            return callWithAdmission(original, receiver, arguments_, admission);
          }
        }
        return Reflect.apply(original, receiver, arguments_);
      });
      install(currentBindingObject, 'destroySlots', (original, receiver, arguments_) => {
        let destroyedSlots: readonly object[] | undefined;
        if (arguments_.length === 0 || (arguments_.length === 1 && arguments_[0] === undefined)) {
          destroyedSlots = allSlots();
        } else if (arguments_.length === 1) {
          destroyedSlots = objectSlots(arguments_[0]);
        }
        const result = Reflect.apply(original, receiver, arguments_);
        if (result === true && destroyedSlots && destroyObserver) {
          try {
            destroyObserver(Object.freeze({ slots: destroyedSlots }));
          } catch {
            // Post-call bookkeeping cannot alter the publisher return value.
          }
        }
        return result;
      });
    } catch (error) {
      for (let index = restorers.length - 1; index >= 0; index -= 1) restorers[index]?.();
      throw error;
    }
    let released = false;
    const release = (): void => {
      if (released) return;
      released = true;
      try {
        deleteSetValue(effects, release);
      } catch {
        // Exact wrapper restoration still runs when bookkeeping is hostile.
      }
      const deferred = setValueSnapshot(deferredRefreshes);
      for (let index = 0; index < deferred.length; index += 1) deferred[index]?.();
      for (let index = restorers.length - 1; index >= 0; index -= 1) restorers[index]?.();
    };
    registerAdapterEffect(release);
    return release;
  };

  const observeDiagnostics = (observer: GoogletagDiagnosticsObserver): (() => void) | undefined => {
    if (disposed || typeof observer !== 'function' || diagnosticsObserver) return undefined;
    diagnosticsObserver = observer;
    let active = true;
    const release = (): void => {
      if (!active) return;
      active = false;
      if (diagnosticsObserver === observer) diagnosticsObserver = undefined;
      try {
        deleteSetValue(effects, release);
      } catch {
        // Exact observer release remains authoritative under registry failure.
      }
    };
    try {
      registerAdapterEffect(release);
    } catch (error) {
      release();
      throw error;
    }
    return release;
  };

  return Object.freeze({
    bindingStatus: (): GoogletagBindingStatus => currentBinding().status,
    adoptDiagnosticsState,
    diagnosticsIdentity: (slot: object): Readonly<GoogletagDiagnosticsSlotSnapshot> | undefined => {
      const state = diagnosticsSlotState(slot);
      if (!state?.traceToken) return undefined;
      const currentCycle = state.cycles.reduce<TraceCycle | undefined>(
        (latest, cycle) => (!latest || cycle.ordinal > latest.ordinal ? cycle : latest),
        undefined
      );
      return Object.freeze({
        token: state.token,
        traceToken: state.traceToken,
        runtimeSlotNumber: Number.parseInt(state.traceToken.slice(4), 36),
        ...(currentCycle === undefined ? {} : { cycleOrdinal: currentCycle.ordinal }),
        ...(state.elementId === undefined ? {} : { elementId: state.elementId }),
        ...(state.adUnitPath === undefined ? {} : { adUnitPath: state.adUnitPath }),
      });
    },
    traceToken: (slot: object): GptSlotTokenV1 | undefined =>
      diagnosticsSlotState(slot)?.traceToken,
    observeDiagnostics,
    observePublisherCalls,
    run,
    notifyReady,
    dispose: (): void => {
      if (disposed) return;
      disposed = true;
      try {
        mintedTraceTokens?.clear();
      } catch {
        // Diagnostics identity cleanup cannot interrupt independent adapter disposal.
      }
      for (const operation of [...live]) fail(operation, 'operation_disposed');
      try {
        releaseHistoricalInitialLoadBindings();
      } catch {
        // Initial-load registry failure cannot interrupt independent adapter effects.
      } finally {
        for (const disposeEffect of [...effects]) {
          try {
            deleteSetValue(effects, disposeEffect);
          } catch {
            // A hostile registry cannot interrupt cleanup of remaining effects.
          }
          try {
            disposeEffect();
          } catch {
            // One cleanup cannot interrupt the remaining adapter disposers.
          }
        }
      }
    },
  });
}

/** Create a side-effect-free GPT boundary for tests and unavailable environments. */
export function createNoopGoogletagAdapter(): GoogletagAdapter {
  return createBrowserGoogletagAdapter({});
}
