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

/** The small GPT surface exposed to an accepted operation. */
export interface GoogletagFacade {
  clearTargeting(slot: object, key?: string): unknown;
  display(slot: string | object): unknown;
  getTargeting(slot: object, key: string): readonly string[];
  refresh(slots?: readonly object[], options?: Readonly<{ changeCorrelator: boolean }>): unknown;
  serviceState(): Readonly<{
    apiReady: boolean;
    initialLoadDisabled: boolean;
    pubadsReady: boolean;
  }>;
  setTargeting(slot: object, key: string, value: string | readonly string[]): unknown;
  slots(): readonly object[];
  subscribe(eventType: string, listener: (event: unknown) => void): () => void;
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
  run<T>(
    command: (googletag: Readonly<GoogletagFacade>) => T,
    options?: GoogletagOperationOptions
  ): GoogletagOperation<T>;
  notifyReady(): void;
  dispose(): void;
}

/** Browser surface owned by the concrete GPT adapter. */
export interface GoogletagGlobalTarget {
  googletag?: unknown;
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

const sharedInitialLoadTrackers = new WeakMap<object, SharedInitialLoadTracker>();

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
  initialLoadDisabled: (service: object) => boolean
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
    const result = Reflect.apply(callable, external, argumentsList);
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
  return Object.freeze({
    clearTargeting: (slot: object, key?: string): unknown =>
      call(slot, 'clearTargeting', key === undefined ? [] : [key]),
    display: (slot: string | object): unknown => call(binding.binding, 'display', [slot]),
    getTargeting: (slot: object, key: string): readonly string[] => {
      const targeting = call(slot, 'getTargeting', [key]);
      if (!Array.isArray(targeting) || targeting.some((entry) => typeof entry !== 'string')) {
        throw new GoogletagAdapterError('external_artifact_incompatible');
      }
      if (!isOperationCurrent()) throw new GoogletagAdapterError('external_artifact_incompatible');
      return Object.freeze([...targeting]);
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
    subscribe: (eventType: string, listener: (event: unknown) => void): (() => void) => {
      const currentService = service();
      const add = member(currentService, 'addEventListener');
      const remove = member(currentService, 'removeEventListener');
      const wrapped = (event: unknown): void => {
        if (!isBindingCurrent()) return;
        try {
          listener(event);
        } catch {
          // Publisher and service callbacks cannot escape the GPT boundary.
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
  });
}

/** Create the sole production reader/writer boundary for `window.googletag`. */
export function createBrowserGoogletagAdapter(
  target: GoogletagGlobalTarget = window as unknown as GoogletagGlobalTarget
): GoogletagAdapter {
  const pending: PendingOperation<unknown>[] = [];
  const live = new Set<PendingOperation<unknown>>();
  const effects = new Set<() => void>();
  const armedBindings = new WeakSet<object>();
  const initialLoadReleases = new Map<object, () => void>();
  const initialLoadOwner = Object.freeze({});
  let disposed = false;

  const registerAdapterEffect = (disposeEffect: () => void): void => {
    if (disposed) {
      try {
        disposeEffect();
      } catch {
        // Reentrant disposal keeps newly-created effects from escaping the adapter.
      }
      return;
    }
    effects.add(disposeEffect);
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
    const release = initialLoadReleases.get(binding);
    if (!release) return;
    initialLoadReleases.delete(binding);
    try {
      effects.delete(release);
    } catch {
      // A hostile registry cannot retain adapter ownership of an old binding.
    }
    try {
      release();
    } catch {
      // One historical binding cannot interrupt release of later bindings.
    }
  };

  const releaseHistoricalInitialLoadBindings = (current?: object): void => {
    for (const binding of [...initialLoadReleases.keys()]) {
      if (binding !== current) releaseInitialLoadBinding(binding);
    }
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

    let tracker = sharedInitialLoadTrackers.get(expected.binding);
    if (!tracker) {
      tracker = {
        disabled: false,
        rootWrapped: false,
        owners: new Set<object>(),
        restorers: new Set<() => void>(),
        services: new WeakMap<object, () => void>(),
      };
      sharedInitialLoadTrackers.set(expected.binding, tracker);
    }
    const trackingCurrent = (): boolean => {
      if (
        disposed ||
        sharedInitialLoadTrackers.get(expected.binding) !== tracker ||
        !tracker.owners.has(initialLoadOwner)
      ) {
        return false;
      }
      const current = sameBinding(expected);
      return (
        !disposed &&
        current &&
        sharedInitialLoadTrackers.get(expected.binding) === tracker &&
        tracker.owners.has(initialLoadOwner)
      );
    };
    let adoptedHere = false;
    if (!initialLoadReleases.has(expected.binding)) {
      if (!expectedCurrent()) {
        if (
          tracker.owners.size === 0 &&
          sharedInitialLoadTrackers.get(expected.binding) === tracker
        ) {
          sharedInitialLoadTrackers.delete(expected.binding);
        }
        return undefined;
      }
      tracker.owners.add(initialLoadOwner);
      const adoptedTracker = tracker;
      const release = (): void => {
        if (initialLoadReleases.get(expected.binding) === release) {
          initialLoadReleases.delete(expected.binding);
        }
        if (!adoptedTracker.owners.delete(initialLoadOwner) || adoptedTracker.owners.size > 0) {
          return;
        }
        if (sharedInitialLoadTrackers.get(expected.binding) === adoptedTracker) {
          sharedInitialLoadTrackers.delete(expected.binding);
        }
        for (const restore of [...adoptedTracker.restorers].reverse()) {
          try {
            restore();
          } catch {
            // One restoration cannot interrupt cleanup of the shared tracker.
          }
        }
        adoptedTracker.restorers.clear();
      };
      initialLoadReleases.set(expected.binding, release);
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
            tracker!.restorers.delete(cleanup);
            tracker!.rootWrapped = false;
            restore();
          };
          tracker.rootWrapped = true;
          tracker.restorers.add(cleanup);
          installedHere.push(cleanup);
        }
        if (!trackingCurrent()) return rollback();
      }
    }

    const trackService = (service: object): boolean => {
      if (tracker!.services.has(service)) return true;
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
          tracker!.restorers.delete(cleanup);
          tracker!.services.delete(service);
          restore();
        };
        tracker!.services.set(service, cleanup);
        tracker!.restorers.add(cleanup);
        installedHere.push(cleanup);
      }
      if (!trackingCurrent()) return false;
      return true;
    };

    if (knownService) {
      if (!trackService(knownService)) return rollback();
    } else {
      if (!trackingCurrent()) return rollback();
      try {
        const service = Reflect.apply(expected.pubads, expected.binding, []);
        if (!trackingCurrent()) return rollback();
        if ((typeof service === 'object' && service !== null) || typeof service === 'function') {
          if (!trackService(service as object)) return rollback();
        }
      } catch {
        return rollback();
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
    const matchesCapturedBinding = (): boolean => {
      const inspected = inspectBinding(expected.binding);
      return (
        inspected.status === 'present' &&
        inspected.value.commandQueue.binding === expected.commandQueue.binding &&
        inspected.value.commandQueue.push === expected.commandQueue.push &&
        inspected.value.display === expected.display &&
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

  const clearReadiness = (operation: PendingOperation<unknown>): void => {
    if (operation.timeout !== undefined) {
      clearTimeout(operation.timeout);
      operation.timeout = undefined;
    }
    removePending(operation);
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
        live.delete(operation);
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
    ensureInitialLoadTracking(binding);
    if (disposed) {
      fail(operation, 'operation_disposed');
      return;
    }
    if (operation.settled) return;
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
            effects.delete(release);
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
    const facade = createFacade(
      binding,
      registerOperationEffect,
      isDispatchCurrent,
      () => !disposed && sameBinding(binding),
      (service) => {
        const tracker = ensureInitialLoadTracking(binding, service);
        return tracker?.disabled === true;
      }
    );
    try {
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
    if (
      current.status !== 'pending' ||
      !current.binding ||
      !current.commandQueue ||
      armedBindings.has(current.binding)
    ) {
      return;
    }
    for (const operation of pending) operation.readinessBinding = current.binding;
    armedBindings.add(current.binding);
    try {
      queueCommand(current.commandQueue, () => notifyReady(current.binding));
    } catch {
      // A later script-owned notification or operation may observe a replacement.
    }
  };

  const run = <T>(
    command: (googletag: Readonly<GoogletagFacade>) => T,
    options: GoogletagOperationOptions = {}
  ): GoogletagOperation<T> => {
    if (disposed) throw new GoogletagAdapterError('operation_disposed');
    const current = currentBinding();
    if (disposed) throw new GoogletagAdapterError('operation_disposed');
    if (current.status === 'pending' && pending.length >= MAX_PENDING_OPERATIONS) {
      throw new GoogletagAdapterError('external_queue_full');
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
      timeout: undefined,
      command,
      resolve,
      reject,
      abortRegistration: undefined,
      readinessBinding: current.status === 'pending' ? current.binding : undefined,
      provisionalEffects: [],
    };
    live.add(operation as PendingOperation<unknown>);
    const handle = Object.freeze({
      get status(): GoogletagOperationStatus {
        return operation.state;
      },
      result,
      dispose: (): void => fail(operation as PendingOperation<unknown>, 'operation_disposed'),
    });

    if (current.status === 'pending') {
      pending.push(operation as PendingOperation<unknown>);
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

  return Object.freeze({
    bindingStatus: (): GoogletagBindingStatus => currentBinding().status,
    run,
    notifyReady,
    dispose: (): void => {
      if (disposed) return;
      disposed = true;
      for (const operation of [...live]) fail(operation, 'operation_disposed');
      for (const binding of [...initialLoadReleases.keys()]) {
        releaseInitialLoadBinding(binding);
      }
      initialLoadReleases.clear();
      for (const disposeEffect of [...effects]) {
        try {
          effects.delete(disposeEffect);
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

/** Create a side-effect-free GPT boundary for tests and unavailable environments. */
export function createNoopGoogletagAdapter(): GoogletagAdapter {
  return createBrowserGoogletagAdapter({});
}
