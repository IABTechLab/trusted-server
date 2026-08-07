const ARTIFACT_PROPERTY = '__trustedServerArtifactV1';
const EXTERNAL_READY_TIMEOUT_MS = 10_000;
const MAX_PENDING_OPERATIONS = 64;
const MAX_NAME_BYTES = 128;
const MAX_EID_SOURCE_BYTES = 256;
const setDeleteIntrinsic = Set.prototype.delete;
const weakSetDeleteIntrinsic = WeakSet.prototype.delete;

function deleteSetValue<T>(set: Set<T>, value: T): boolean {
  return Reflect.apply(setDeleteIntrinsic, set, [value]) as boolean;
}

function deleteWeakSetValue<T extends object>(set: WeakSet<T>, value: T): boolean {
  return Reflect.apply(weakSetDeleteIntrinsic, set, [value]) as boolean;
}

/** The live state of the publisher-owned `window.pbjs` binding. */
export type PrebidBindingStatus = 'present' | 'pending' | 'incompatible';

/** The readiness state owned by one Prebid operation. */
export type PrebidOperationStatus = PrebidBindingStatus | 'timed_out';

/** Failure codes produced at the Prebid adapter boundary. */
export type PrebidAdapterErrorCode =
  | 'caller_aborted'
  | 'external_artifact_incompatible'
  | 'external_queue_full'
  | 'external_ready_timeout'
  | 'operation_disposed';

/** A typed failure contained by the Prebid adapter. */
export class PrebidAdapterError extends Error {
  public readonly code: PrebidAdapterErrorCode;

  public constructor(code: PrebidAdapterErrorCode) {
    super(code);
    this.name = 'PrebidAdapterError';
    this.code = code;
  }
}

/** The exact recursively frozen external Prebid artifact stamp. */
export interface ExternalPrebidArtifactV1 {
  readonly abi: 1;
  readonly artifactReleaseId: string;
  readonly prebidVersion: '10.26.0';
  readonly moduleStems: readonly string[];
  readonly bidderCodes: readonly string[];
  readonly bidderAliases: readonly Readonly<{ code: string; moduleStem: string }>[];
  readonly userIdModules: readonly Readonly<{
    moduleName: string;
    configNames: readonly string[];
    eidSources: readonly string[];
  }>[];
}

/** Required configured behavior that the artifact stamp must cover. */
export interface PrebidArtifactRequirements {
  readonly configuredClientSideBidders?: readonly string[];
  readonly requiredUserIdModules?: readonly Readonly<{
    moduleName: string;
    configNames?: readonly string[];
    eidSources?: readonly string[];
  }>[];
}

/** The small Prebid surface exposed to an accepted operation. */
export interface PrebidFacade {
  addAdUnits(adUnits: readonly unknown[]): unknown;
  addBidResponse(adUnitCode: string, bid: object): unknown;
  highestBids(adUnitCode?: string): readonly object[];
  processQueue(): unknown;
  renderAd(targetDocument: object, adId: string): unknown;
  requestBids(options: object): unknown;
  subscribe(eventType: string, listener: (event: unknown) => void): () => void;
}

/** Options owned by one Prebid operation. */
export interface PrebidOperationOptions {
  readonly signal?: AbortSignal;
}

/** A disposable Prebid operation and its readiness-scoped result. */
export interface PrebidOperation<T> {
  readonly status: PrebidOperationStatus;
  readonly result: Promise<T>;
  dispose(): void;
}

/** Narrow Prebid boundary consumed by kernel sessions and services. */
export interface PrebidAdapter {
  bindingStatus(): PrebidBindingStatus;
  run<T>(
    command: (prebid: Readonly<PrebidFacade>) => T,
    options?: PrebidOperationOptions
  ): PrebidOperation<T>;
  notifyReady(): void;
  dispose(): void;
}

/** Browser surface owned by the concrete Prebid adapter. */
export interface PrebidGlobalTarget {
  pbjs?: unknown;
}

interface CommandQueue {
  push(command: () => void): unknown;
}

interface PresentPrebid {
  readonly binding: object;
  readonly commandQueue: CommandQueue;
  readonly stamp: ExternalPrebidArtifactV1;
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
  state: PrebidOperationStatus;
  settled: boolean;
  pendingReservation: boolean;
  timeout: ReturnType<typeof setTimeout> | undefined;
  readonly command: (prebid: Readonly<PrebidFacade>) => T;
  readonly resolve: (value: T | PromiseLike<T>) => void;
  readonly reject: (reason: unknown) => void;
  abortRegistration: AbortRegistration | undefined;
  readinessBinding: object | undefined;
  readonly provisionalEffects: ProvisionalEffect[];
}

const encoder = new TextEncoder();

function validUnicodeScalars(value: string): boolean {
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    if (code >= 0xd800 && code <= 0xdbff) {
      const next = value.charCodeAt(index + 1);
      if (!Number.isInteger(next) || next < 0xdc00 || next > 0xdfff) return false;
      index += 1;
    } else if (code >= 0xdc00 && code <= 0xdfff) {
      return false;
    }
  }
  return true;
}

function safeMember(binding: object, key: PropertyKey): unknown {
  try {
    return Reflect.get(binding, key);
  } catch {
    return undefined;
  }
}

function safeOwnDescriptor(binding: object, key: PropertyKey): PropertyDescriptor | undefined {
  try {
    return Object.getOwnPropertyDescriptor(binding, key);
  } catch {
    return undefined;
  }
}

function frozenRecordValues(
  value: unknown,
  keys: readonly string[]
): Readonly<Record<string, unknown>> | undefined {
  if (
    typeof value !== 'object' ||
    value === null ||
    Object.getPrototypeOf(value) !== Object.prototype
  ) {
    return undefined;
  }
  if (!Object.isFrozen(value)) return undefined;
  let ownKeys: PropertyKey[];
  let descriptors: Record<PropertyKey, PropertyDescriptor>;
  try {
    ownKeys = Reflect.ownKeys(value);
    descriptors = Object.getOwnPropertyDescriptors(value);
  } catch {
    return undefined;
  }
  if (ownKeys.length !== keys.length || ownKeys.some((key) => typeof key !== 'string')) {
    return undefined;
  }
  if (keys.some((key) => !ownKeys.includes(key))) return undefined;
  const values: Record<string, unknown> = {};
  for (const key of keys) {
    const descriptor = descriptors[key];
    if (
      descriptor === undefined ||
      !Object.prototype.hasOwnProperty.call(descriptor, 'value') ||
      descriptor.enumerable !== true ||
      descriptor.writable !== false ||
      descriptor.configurable !== false
    ) {
      return undefined;
    }
    values[key] = descriptor.value;
  }
  return values;
}

function validString(value: unknown, maximumBytes: number, lowercase = false): value is string {
  return (
    typeof value === 'string' &&
    value.length > 0 &&
    validUnicodeScalars(value) &&
    encoder.encode(value).byteLength <= maximumBytes &&
    (!lowercase || value === value.toLowerCase())
  );
}

function frozenArrayValues(value: unknown, maximumLength: number): readonly unknown[] | undefined {
  if (!Array.isArray(value) || Object.getPrototypeOf(value) !== Array.prototype) return undefined;
  if (!Object.isFrozen(value)) return undefined;
  const descriptors = Object.getOwnPropertyDescriptors(value);
  const lengthDescriptor = Object.getOwnPropertyDescriptor(value, 'length');
  if (
    !lengthDescriptor ||
    !Object.prototype.hasOwnProperty.call(lengthDescriptor, 'value') ||
    typeof lengthDescriptor.value !== 'number' ||
    lengthDescriptor.value > maximumLength ||
    lengthDescriptor.enumerable !== false ||
    lengthDescriptor.writable !== false ||
    lengthDescriptor.configurable !== false
  ) {
    return undefined;
  }
  const length = lengthDescriptor.value;
  const ownKeys = Reflect.ownKeys(value);
  if (ownKeys.length !== length + 1 || ownKeys.some((key) => typeof key !== 'string')) {
    return undefined;
  }
  const values: unknown[] = [];
  for (let index = 0; index < length; index += 1) {
    const descriptor = descriptors[String(index)];
    if (
      !descriptor ||
      !Object.prototype.hasOwnProperty.call(descriptor, 'value') ||
      descriptor.enumerable !== true ||
      descriptor.writable !== false ||
      descriptor.configurable !== false
    ) {
      return undefined;
    }
    values.push(descriptor.value);
  }
  return values;
}

function frozenSortedStrings(
  value: unknown,
  maximumLength: number,
  maximumBytes: number,
  lowercase = false
): value is readonly string[] {
  const values = frozenArrayValues(value, maximumLength);
  if (!values) return false;
  let previous: string | undefined;
  for (const entry of values) {
    if (!validString(entry, maximumBytes, lowercase)) return false;
    if (previous !== undefined && previous >= entry) return false;
    previous = entry;
  }
  return true;
}

function validateStamp(
  candidate: unknown,
  requirements: PrebidArtifactRequirements
): candidate is ExternalPrebidArtifactV1 {
  try {
    const stamp = frozenRecordValues(candidate, [
      'abi',
      'artifactReleaseId',
      'prebidVersion',
      'moduleStems',
      'bidderCodes',
      'bidderAliases',
      'userIdModules',
    ]);
    if (!stamp) return false;
    if (
      stamp.abi !== 1 ||
      stamp.prebidVersion !== '10.26.0' ||
      typeof stamp.artifactReleaseId !== 'string' ||
      !/^[0-9a-f]{64}$/.test(stamp.artifactReleaseId) ||
      !frozenSortedStrings(stamp.moduleStems, 256, MAX_NAME_BYTES) ||
      !frozenSortedStrings(stamp.bidderCodes, 512, MAX_NAME_BYTES)
    ) {
      return false;
    }
    const moduleStems = frozenArrayValues(stamp.moduleStems, 256) as readonly string[];
    const bidderCodes = frozenArrayValues(stamp.bidderCodes, 512) as readonly string[];
    const bidderAliases = frozenArrayValues(stamp.bidderAliases, 512);
    const userIdModules = frozenArrayValues(stamp.userIdModules, 128);
    if (!bidderAliases || !userIdModules) return false;

    let previousAlias = '';
    for (const aliasCandidate of bidderAliases) {
      const alias = frozenRecordValues(aliasCandidate, ['code', 'moduleStem']);
      if (!alias) return false;
      if (
        !validString(alias.code, MAX_NAME_BYTES) ||
        !validString(alias.moduleStem, MAX_NAME_BYTES)
      ) {
        return false;
      }
      const identity = `${alias.code}\u0000${alias.moduleStem}`;
      if (previousAlias !== '' && previousAlias >= identity) return false;
      previousAlias = identity;
      if (!bidderCodes.includes(alias.code) || !moduleStems.includes(alias.moduleStem)) {
        return false;
      }
    }

    let previousModule = '';
    const admittedUserIdModules: Array<{
      moduleName: string;
      configNames: readonly string[];
      eidSources: readonly string[];
    }> = [];
    for (const moduleCandidate of userIdModules) {
      const userIdModule = frozenRecordValues(moduleCandidate, [
        'moduleName',
        'configNames',
        'eidSources',
      ]);
      if (!userIdModule) return false;
      if (
        !validString(userIdModule.moduleName, MAX_NAME_BYTES) ||
        (previousModule !== '' && previousModule >= userIdModule.moduleName) ||
        !moduleStems.includes(userIdModule.moduleName) ||
        !frozenSortedStrings(userIdModule.configNames, 64, MAX_NAME_BYTES) ||
        !frozenSortedStrings(userIdModule.eidSources, 64, MAX_EID_SOURCE_BYTES, true)
      ) {
        return false;
      }
      previousModule = userIdModule.moduleName;
      admittedUserIdModules.push({
        moduleName: userIdModule.moduleName,
        configNames: frozenArrayValues(userIdModule.configNames, 64) as readonly string[],
        eidSources: frozenArrayValues(userIdModule.eidSources, 64) as readonly string[],
      });
    }

    for (const bidder of requirements.configuredClientSideBidders ?? []) {
      if (!bidderCodes.includes(bidder)) return false;
    }
    for (const required of requirements.requiredUserIdModules ?? []) {
      const included = admittedUserIdModules.find(
        (module) => module.moduleName === required.moduleName
      );
      if (
        !included ||
        (required.configNames ?? []).some((name) => !included.configNames.includes(name)) ||
        (required.eidSources ?? []).some((source) => !included.eidSources.includes(source))
      ) {
        return false;
      }
    }
    return true;
  } catch {
    return false;
  }
}

const REQUIRED_API_METHODS = [
  'addAdUnits',
  'addBidResponse',
  'getHighestCpmBids',
  'offEvent',
  'onEvent',
  'processQueue',
  'renderAd',
  'requestBids',
] as const;

function commandQueue(binding: object): CommandQueue | undefined {
  const candidate = safeMember(binding, 'que');
  if ((typeof candidate !== 'object' || candidate === null) && typeof candidate !== 'function') {
    return undefined;
  }
  return typeof safeMember(candidate, 'push') === 'function'
    ? (candidate as CommandQueue)
    : undefined;
}

function inspectBinding(
  value: unknown,
  requirements: PrebidArtifactRequirements
):
  | { readonly status: 'pending'; readonly binding?: object; readonly commandQueue?: CommandQueue }
  | { readonly status: 'incompatible'; readonly binding?: object }
  | { readonly status: 'present'; readonly value: PresentPrebid } {
  if (value === undefined || value === null) return { status: 'pending' };
  if ((typeof value !== 'object' || value === null) && typeof value !== 'function') {
    return { status: 'incompatible' };
  }
  const binding = value as object;
  const queue = commandQueue(binding);
  if (!queue) return { status: 'incompatible', binding };
  const descriptor = safeOwnDescriptor(binding, ARTIFACT_PROPERTY);
  if (!descriptor) {
    const hasRealApi = REQUIRED_API_METHODS.some(
      (method) => safeMember(binding, method) !== undefined
    );
    return hasRealApi
      ? { status: 'incompatible', binding }
      : { status: 'pending', binding, commandQueue: queue };
  }
  if (
    !Object.prototype.hasOwnProperty.call(descriptor, 'value') ||
    descriptor.enumerable !== false ||
    descriptor.writable !== false ||
    descriptor.configurable !== false ||
    !validateStamp(descriptor.value, requirements) ||
    REQUIRED_API_METHODS.some((method) => typeof safeMember(binding, method) !== 'function')
  ) {
    return { status: 'incompatible', binding };
  }
  return {
    status: 'present',
    value: { binding, commandQueue: queue, stamp: descriptor.value },
  };
}

function readTarget(target: PrebidGlobalTarget): unknown {
  try {
    return target.pbjs;
  } catch {
    return false;
  }
}

function queueCommand(queue: CommandQueue, command: () => void, guard?: () => boolean): void {
  const push = safeMember(queue as object, 'push');
  if (guard && !guard()) throw new PrebidAdapterError('external_artifact_incompatible');
  if (typeof push !== 'function') throw new PrebidAdapterError('external_artifact_incompatible');
  if (guard && !guard()) throw new PrebidAdapterError('external_artifact_incompatible');
  Reflect.apply(push, queue, [command]);
  if (guard && !guard()) throw new PrebidAdapterError('external_artifact_incompatible');
}

/** Create the sole production reader/writer boundary for `window.pbjs`. */
export function createBrowserPrebidAdapter(
  target: PrebidGlobalTarget = window as unknown as PrebidGlobalTarget,
  requirements: PrebidArtifactRequirements = {}
): PrebidAdapter {
  const pending: PendingOperation<unknown>[] = [];
  const live = new Set<PendingOperation<unknown>>();
  const effects = new Set<() => void>();
  let armedBindings = new WeakSet<object>();
  let diagnosedBindings = new WeakSet<object>();
  let diagnosedUnbound = false;
  let pendingReservations = 0;
  let disposed = false;

  const rollbackDiagnosticOwnership = (binding: object): void => {
    let released = false;
    try {
      deleteWeakSetValue(diagnosedBindings, binding);
      released = !diagnosedBindings.has(binding);
    } catch {
      // A poisoned registry cannot prove that the exact marker was removed.
    }
    if (!released) diagnosedBindings = new WeakSet<object>();
  };

  const currentBinding = (): ReturnType<typeof inspectBinding> => {
    const inspected = inspectBinding(readTarget(target), requirements);
    if (inspected.status === 'incompatible') {
      let shouldDiagnose = !diagnosedUnbound;
      if (inspected.binding) {
        try {
          shouldDiagnose = !diagnosedBindings.has(inspected.binding);
        } catch {
          shouldDiagnose = false;
        }
      }
      if (shouldDiagnose) {
        let diagnosticOwned = false;
        if (inspected.binding) {
          try {
            diagnosedBindings.add(inspected.binding);
          } catch {
            // A stateful add may still have published diagnostic ownership.
          }
          try {
            diagnosticOwned = diagnosedBindings.has(inspected.binding);
          } catch {
            rollbackDiagnosticOwnership(inspected.binding);
          }
        } else {
          diagnosedUnbound = true;
          diagnosticOwned = diagnosedUnbound;
        }
        if (diagnosticOwned) {
          try {
            console.warn('[tsjs-prebid] external Prebid artifact is incompatible');
          } catch {
            // Diagnostics cannot change readiness behavior.
          }
        }
      }
    }
    return inspected;
  };

  const sameBinding = (expected: PresentPrebid): boolean => {
    if (readTarget(target) !== expected.binding) return false;
    const descriptor = safeOwnDescriptor(expected.binding, ARTIFACT_PROPERTY);
    return (
      descriptor !== undefined &&
      Object.prototype.hasOwnProperty.call(descriptor, 'value') &&
      descriptor.value === expected.stamp &&
      descriptor.enumerable === false &&
      descriptor.writable === false &&
      descriptor.configurable === false
    );
  };

  const callBound = (
    expected: PresentPrebid,
    key: PropertyKey,
    argumentsList: readonly unknown[],
    isCurrent: () => boolean
  ): unknown => {
    if (!isCurrent()) throw new PrebidAdapterError('external_artifact_incompatible');
    const member = safeMember(expected.binding, key);
    if (!isCurrent()) throw new PrebidAdapterError('external_artifact_incompatible');
    if (typeof member !== 'function')
      throw new PrebidAdapterError('external_artifact_incompatible');
    if (!isCurrent()) throw new PrebidAdapterError('external_artifact_incompatible');
    const result = Reflect.apply(member, expected.binding, argumentsList);
    if (!isCurrent()) throw new PrebidAdapterError('external_artifact_incompatible');
    return result;
  };

  const createFacade = (
    binding: PresentPrebid,
    registerOperationEffect: (disposeEffect: () => void) => () => void,
    isOperationCurrent: () => boolean,
    isBindingCurrent: () => boolean
  ): Readonly<PrebidFacade> =>
    Object.freeze({
      addAdUnits: (adUnits: readonly unknown[]): unknown =>
        callBound(binding, 'addAdUnits', [[...adUnits]], isOperationCurrent),
      addBidResponse: (adUnitCode: string, bid: object): unknown =>
        callBound(binding, 'addBidResponse', [adUnitCode, bid], isOperationCurrent),
      highestBids: (adUnitCode?: string): readonly object[] => {
        const value = callBound(
          binding,
          'getHighestCpmBids',
          adUnitCode === undefined ? [] : [adUnitCode],
          isOperationCurrent
        );
        if (!Array.isArray(value) || value.some((bid) => typeof bid !== 'object' || bid === null)) {
          throw new PrebidAdapterError('external_artifact_incompatible');
        }
        if (!isOperationCurrent()) throw new PrebidAdapterError('external_artifact_incompatible');
        return Object.freeze([...value]);
      },
      processQueue: (): unknown => callBound(binding, 'processQueue', [], isOperationCurrent),
      renderAd: (targetDocument: object, adId: string): unknown =>
        callBound(binding, 'renderAd', [targetDocument, adId], isOperationCurrent),
      requestBids: (options: object): unknown =>
        callBound(binding, 'requestBids', [options], isOperationCurrent),
      subscribe: (eventType: string, listener: (event: unknown) => void): (() => void) => {
        if (!isOperationCurrent()) throw new PrebidAdapterError('external_artifact_incompatible');
        const add = safeMember(binding.binding, 'onEvent');
        if (!isOperationCurrent() || typeof add !== 'function')
          throw new PrebidAdapterError('external_artifact_incompatible');
        const remove = safeMember(binding.binding, 'offEvent');
        if (!isOperationCurrent() || typeof remove !== 'function')
          throw new PrebidAdapterError('external_artifact_incompatible');
        const wrapped = (event: unknown): void => {
          if (!isBindingCurrent()) return;
          try {
            listener(event);
          } catch {
            // Publisher callbacks cannot escape the Prebid boundary.
          }
        };
        let attempted = false;
        const rollback = (): void => {
          if (!attempted) return;
          attempted = false;
          try {
            Reflect.apply(remove, binding.binding, [eventType, wrapped]);
          } catch {
            // Transaction rollback remains best-effort and cannot replace the original failure.
          }
        };
        try {
          if (!isOperationCurrent()) throw new PrebidAdapterError('external_artifact_incompatible');
          attempted = true;
          Reflect.apply(add, binding.binding, [eventType, wrapped]);
          if (!isOperationCurrent()) throw new PrebidAdapterError('external_artifact_incompatible');
        } catch (error) {
          rollback();
          throw error;
        }
        let active = true;
        return registerOperationEffect(() => {
          if (!active) return;
          active = false;
          rollback();
        });
      },
    });

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
    if (error instanceof PrebidAdapterError && error.code === 'external_artifact_incompatible') {
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

  const fail = (operation: PendingOperation<unknown>, code: PrebidAdapterErrorCode): void => {
    if (operation.settled) return;
    if (code === 'external_ready_timeout') operation.state = 'timed_out';
    if (code === 'external_artifact_incompatible') operation.state = 'incompatible';
    rejectOperation(operation, new PrebidAdapterError(code));
  };

  const dispatch = (operation: PendingOperation<unknown>, binding: PresentPrebid): void => {
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
          throw new PrebidAdapterError(
            disposed ? 'operation_disposed' : 'external_artifact_incompatible'
          );
        }
      };
      const provisional = { promote, release };
      operation.provisionalEffects[operation.provisionalEffects.length] = provisional;
      if (!isDispatchCurrent()) {
        release();
        throw new PrebidAdapterError(
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
    const facade = createFacade(
      binding,
      registerOperationEffect,
      isDispatchCurrent,
      () => !disposed && sameBinding(binding)
    );
    try {
      if (!isDispatchCurrent()) {
        if (disposed) fail(operation, 'operation_disposed');
        else fail(operation, 'external_artifact_incompatible');
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
      if (!operation.settled && !isDispatchCurrent()) {
        if (disposed) fail(operation, 'operation_disposed');
        else fail(operation, 'external_artifact_incompatible');
      }
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
    command: (prebid: Readonly<PrebidFacade>) => T,
    options: PrebidOperationOptions = {}
  ): PrebidOperation<T> => {
    if (disposed) throw new PrebidAdapterError('operation_disposed');
    const current = currentBinding();
    if (disposed) throw new PrebidAdapterError('operation_disposed');
    if (current.status === 'pending') {
      if (pendingReservations >= MAX_PENDING_OPERATIONS) {
        throw new PrebidAdapterError('external_queue_full');
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
      get status(): PrebidOperationStatus {
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
      fail(operation as PendingOperation<unknown>, 'external_artifact_incompatible');
    } else if (current.status === 'present') {
      dispatch(operation as PendingOperation<unknown>, current.value);
    } else {
      armNotification();
    }
    return handle;
  };

  return Object.freeze({
    bindingStatus: (): PrebidBindingStatus => currentBinding().status,
    run,
    notifyReady,
    dispose: (): void => {
      if (disposed) return;
      disposed = true;
      for (const operation of [...live]) fail(operation, 'operation_disposed');
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
    },
  });
}

/** Create a side-effect-free Prebid boundary for tests and unavailable environments. */
export function createNoopPrebidAdapter(): PrebidAdapter {
  return createBrowserPrebidAdapter({});
}
