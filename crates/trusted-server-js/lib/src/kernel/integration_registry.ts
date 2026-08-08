import type { BootManifestV1 } from '../core/types';

import { DisposableStack, type DisposeCallback } from './disposable';

const INTEGRATION_ID = /^[a-z0-9][a-z0-9_-]{0,63}$/;
const RELEASE_ID = /^[0-9a-f]{64}$/;
const MAX_INTEGRATIONS = 16;
const MAX_KNOWN_INTEGRATIONS = 256;
const BOOT_DEADLINE_MS = 10_000;
const EMPTY_BINDING = Object.freeze({});
const ABORTED = Symbol('aborted');

export type BootFailureReason = 'abi_mismatch' | 'bundle_partial';
export type IntegrationRegistryState =
  'collecting' | 'preparing' | 'activating' | 'publishing' | 'committed' | 'failed' | 'disposed';

export interface IntegrationBindings {
  readonly config: unknown;
  readonly interfaces: Readonly<Record<string, unknown>>;
}

export interface IntegrationPrepareContext extends IntegrationBindings {
  readonly signal: AbortSignal;
  readonly onDispose: (callback: DisposeCallback) => void;
}

export interface IntegrationActivationContext {
  readonly signal: AbortSignal;
  readonly onDispose: (callback: DisposeCallback) => void;
  readonly afterCommit: (callback: () => void) => void;
}

export interface CoreActivationContext {
  readonly signal: AbortSignal;
  readonly onDispose: (callback: DisposeCallback) => void;
}

export interface CorePreparationContext {
  readonly signal: AbortSignal;
  readonly onDispose: (callback: DisposeCallback) => void;
}

export interface PreparedIntegration {
  readonly activate: (context: IntegrationActivationContext) => void;
}

export interface IntegrationRegistration {
  readonly id: string;
  readonly release: string;
  readonly prepare: (
    context: IntegrationPrepareContext
  ) => PreparedIntegration | PromiseLike<PreparedIntegration>;
}

export interface IntegrationRuntimeFailure {
  readonly id: string;
  readonly phase: 'after_commit';
}

export interface IntegrationInstallCallbacks {
  /** Allocates inert composition-owned services before integration preparation. */
  readonly prepareCore?: (context: CorePreparationContext) => void;
  /** Installs reversible core listeners before any integration module activation. */
  readonly activateCore: (context: CoreActivationContext) => void;
  /** Installs the complete API synchronously. It must not yield or invoke publisher code. */
  readonly publish: () => void;
  /** Drains the already-committed preload queue. Callback isolation belongs to the queue. */
  readonly drainPreload: () => void;
}

export interface IntegrationKernelResult {
  readonly state: 'kernel';
  readonly runtimeFailures: readonly IntegrationRuntimeFailure[];
  readonly dispose: () => void;
}

export interface IntegrationFallbackResult {
  readonly state: 'fallback';
  readonly reason: BootFailureReason;
}

export type IntegrationInstallResult = IntegrationKernelResult | IntegrationFallbackResult;

export interface IntegrationRegistryOptions {
  readonly manifest: unknown;
  readonly releaseId: string;
  /** Frozen build/composition inventory of integration ids this core release knows. */
  readonly knownIntegrationIds: readonly string[];
  readonly startedAtMs: number;
  readonly now?: () => number;
  readonly signal?: AbortSignal;
  readonly getBindings?: (id: string) => IntegrationBindings;
  /** Monotonic bootstrap-generation guard supplied by the composition owner. */
  readonly isCurrentOwner?: () => boolean;
  readonly onDisposalError?: (error: unknown) => void;
  readonly onRuntimeFailure?: (failure: IntegrationRuntimeFailure) => void;
}

export interface IntegrationRegistry {
  readonly state: IntegrationRegistryState;
  readonly manifest: BootManifestV1 | undefined;
  readonly register: (candidate: unknown) => boolean;
  readonly install: (callbacks: IntegrationInstallCallbacks) => Promise<IntegrationInstallResult>;
  readonly dispose: () => void;
}

interface PreparedRecord {
  readonly id: string;
  readonly scope: DisposableStack;
  readonly module: PreparedIntegration;
  afterCommit: (() => void) | undefined;
}

function isRecord(value: unknown): value is Record<PropertyKey, unknown> {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) return false;
  const prototype = Object.getPrototypeOf(value) as unknown;
  return prototype === Object.prototype || prototype === null;
}

function readExactDataFields(
  value: unknown,
  expected: readonly string[]
): Readonly<Record<string, unknown>> | undefined {
  if (!isRecord(value)) return undefined;
  const keys = Reflect.ownKeys(value);
  if (
    keys.length !== expected.length ||
    !keys.every((key) => typeof key === 'string' && expected.includes(key))
  ) {
    return undefined;
  }

  const fields: Record<string, unknown> = Object.create(null) as Record<string, unknown>;
  for (const key of expected) {
    const descriptor = Object.getOwnPropertyDescriptor(value, key);
    if (!descriptor || !('value' in descriptor)) return undefined;
    fields[key] = descriptor.value;
  }
  return fields;
}

function snapshotExactArray(value: unknown, maximumLength: number): readonly unknown[] | undefined {
  if (!Array.isArray(value) || value.length > maximumLength) return undefined;
  const expectedKeys = Array.from({ length: value.length }, (_, index) => String(index));
  expectedKeys.push('length');
  const actualKeys = Reflect.ownKeys(value);
  if (
    actualKeys.length !== expectedKeys.length ||
    !actualKeys.every((key) => typeof key === 'string' && expectedKeys.includes(key))
  ) {
    return undefined;
  }

  const snapshot: unknown[] = [];
  for (let index = 0; index < value.length; index += 1) {
    const descriptor = Object.getOwnPropertyDescriptor(value, String(index));
    if (!descriptor || !('value' in descriptor)) return undefined;
    snapshot.push(descriptor.value);
  }
  return Object.freeze(snapshot);
}

function validateKnownIntegrationIds(candidate: unknown): ReadonlySet<string> | undefined {
  if (!Object.isFrozen(candidate)) return undefined;
  const ids = snapshotExactArray(candidate, MAX_KNOWN_INTEGRATIONS);
  if (!ids) return undefined;

  const known = new Set<string>();
  for (const id of ids) {
    if (typeof id !== 'string' || !INTEGRATION_ID.test(id) || known.has(id)) return undefined;
    known.add(id);
  }
  return known;
}

function validateManifest(
  candidate: unknown,
  embeddedReleaseId: string,
  knownIntegrationIds: ReadonlySet<string>
): BootManifestV1 | undefined {
  try {
    if (!RELEASE_ID.test(embeddedReleaseId)) return undefined;
    const manifestFields = readExactDataFields(candidate, ['version', 'releaseId', 'integrations']);
    if (!manifestFields) return undefined;
    if (manifestFields.version !== 1 || manifestFields.releaseId !== embeddedReleaseId) {
      return undefined;
    }
    const manifestIntegrations = snapshotExactArray(manifestFields.integrations, MAX_INTEGRATIONS);
    if (!manifestIntegrations) return undefined;

    const seen = new Set<string>();
    const integrations: { readonly id: string; readonly required: true }[] = [];
    for (const entry of manifestIntegrations) {
      const entryFields = readExactDataFields(entry, ['id', 'required']);
      if (!entryFields) return undefined;
      if (typeof entryFields.id !== 'string' || !INTEGRATION_ID.test(entryFields.id)) {
        return undefined;
      }
      if (!knownIntegrationIds.has(entryFields.id)) return undefined;
      if (entryFields.required !== true || seen.has(entryFields.id)) return undefined;
      seen.add(entryFields.id);
      integrations.push(Object.freeze({ id: entryFields.id, required: true }));
    }

    return Object.freeze({
      version: 1,
      releaseId: embeddedReleaseId,
      integrations: Object.freeze(integrations),
    });
  } catch {
    return undefined;
  }
}

function isFrozenBinding(value: unknown): boolean {
  return (
    value === null ||
    (typeof value !== 'object' && typeof value !== 'function') ||
    Object.isFrozen(value)
  );
}

function hasOnlyFrozenDataValues(value: Record<PropertyKey, unknown>): boolean {
  return Reflect.ownKeys(value).every((key) => {
    const descriptor = Object.getOwnPropertyDescriptor(value, key);
    return descriptor !== undefined && 'value' in descriptor && isFrozenBinding(descriptor.value);
  });
}

function isThenable(value: unknown): value is PromiseLike<unknown> {
  return (
    (typeof value === 'object' || typeof value === 'function') &&
    value !== null &&
    typeof (value as { then?: unknown }).then === 'function'
  );
}

function observeThenableRejection(
  value: PromiseLike<unknown>,
  onRejected: (error: unknown) => void = () => undefined
): void {
  try {
    void Promise.resolve(value).catch((error: unknown) => {
      try {
        onRejected(error);
      } catch {
        // Rejection observation must never create another unhandled rejection.
      }
    });
  } catch (error) {
    try {
      onRejected(error);
    } catch {
      // Rejection observation is bounded and never changes registry control flow.
    }
  }
}

class IntegrationRegistryOwner {
  private readonly manifestValue: BootManifestV1 | undefined;
  private readonly registrations = new Map<string, IntegrationRegistration>();
  private readonly prepared: PreparedRecord[] = [];
  private readonly abortController = new AbortController();
  private readonly coreScope: DisposableStack;
  private readonly now: () => number;
  private readonly getBindings: (id: string) => IntegrationBindings;
  private readonly isCurrentOwner: () => boolean;
  private readonly startedAtMs: number;
  private readonly releaseId: string;
  private readonly ownerSignal: AbortSignal | undefined;
  private readonly onDisposalError: (error: unknown) => void;
  private readonly onRuntimeFailure: (failure: IntegrationRuntimeFailure) => void;
  private deadlineTimer: ReturnType<typeof setTimeout> | undefined;
  private failureReason: BootFailureReason | undefined;
  private installPromise: Promise<IntegrationInstallResult> | undefined;
  private registrationWaiter: (() => void) | undefined;
  private registryState: IntegrationRegistryState = 'collecting';
  private ownedCallbackDepth = 0;
  private unwindPending = false;

  public constructor(options: IntegrationRegistryOptions) {
    this.releaseId = options.releaseId;
    this.startedAtMs = options.startedAtMs;
    this.now = options.now ?? (() => performance.now());
    this.getBindings =
      options.getBindings ??
      (() => ({
        config: EMPTY_BINDING,
        interfaces: EMPTY_BINDING,
      }));
    this.isCurrentOwner = options.isCurrentOwner ?? (() => true);
    this.ownerSignal = options.signal;
    this.onDisposalError = options.onDisposalError ?? (() => undefined);
    this.onRuntimeFailure = options.onRuntimeFailure ?? (() => undefined);
    this.coreScope = new DisposableStack(this.onDisposalError);
    const knownIntegrationIds = validateKnownIntegrationIds(options.knownIntegrationIds);
    this.manifestValue = knownIntegrationIds
      ? validateManifest(options.manifest, options.releaseId, knownIntegrationIds)
      : undefined;

    if (!this.manifestValue) {
      this.fail('abi_mismatch');
      return;
    }
    if (!Number.isFinite(this.startedAtMs) || this.deadlineExpired()) {
      this.fail('bundle_partial');
      return;
    }
    if (this.ownerSignal?.aborted) {
      this.fail('bundle_partial');
      return;
    }

    this.ownerSignal?.addEventListener('abort', this.onOwnerAbort, { once: true });
    const remaining = Math.max(0, BOOT_DEADLINE_MS - (this.now() - this.startedAtMs));
    this.deadlineTimer = setTimeout(() => this.fail('bundle_partial'), remaining);
  }

  public get state(): IntegrationRegistryState {
    return this.registryState;
  }

  public get manifest(): BootManifestV1 | undefined {
    return this.manifestValue;
  }

  public register(candidate: unknown): boolean {
    if (!this.ownerIsCurrent()) {
      this.fail('bundle_partial');
      return false;
    }
    if (this.registryState === 'preparing' || this.registryState === 'activating') {
      this.fail('abi_mismatch');
      return false;
    }
    if (this.registryState !== 'collecting') return false;
    if (this.deadlineExpired()) {
      this.fail('bundle_partial');
      return false;
    }
    try {
      const fields = readExactDataFields(candidate, ['id', 'release', 'prepare']);
      if (!fields) {
        this.fail('abi_mismatch');
        return false;
      }
      const { id, release, prepare } = fields;
      if (
        typeof id !== 'string' ||
        !INTEGRATION_ID.test(id) ||
        typeof release !== 'string' ||
        release !== this.releaseId ||
        typeof prepare !== 'function' ||
        !this.manifestValue?.integrations.some((entry) => entry.id === id) ||
        this.registrations.has(id)
      ) {
        this.fail('abi_mismatch');
        return false;
      }

      if (!this.ownerIsCurrent()) {
        this.fail('bundle_partial');
        return false;
      }
      if (this.registryState !== 'collecting') return false;

      this.registrations.set(
        id,
        Object.freeze({
          id,
          release,
          prepare: prepare as IntegrationRegistration['prepare'],
        })
      );
      if (this.hasEveryRequiredRegistration()) this.wakeRegistrationWaiter();
      return true;
    } catch {
      this.fail('abi_mismatch');
      return false;
    }
  }

  public install(callbacks: IntegrationInstallCallbacks): Promise<IntegrationInstallResult> {
    if (this.installPromise) return this.installPromise;

    let resolveInstall: ((result: IntegrationInstallResult) => void) | undefined;
    this.installPromise = new Promise<IntegrationInstallResult>((resolve) => {
      resolveInstall = resolve;
    });

    let acceptedCallbacks: IntegrationInstallCallbacks;
    try {
      acceptedCallbacks = Object.freeze({
        ...(callbacks.prepareCore ? { prepareCore: callbacks.prepareCore } : {}),
        activateCore: callbacks.activateCore,
        publish: callbacks.publish,
        drainPreload: callbacks.drainPreload,
      });
    } catch {
      this.fail('bundle_partial');
      resolveInstall?.(this.fallbackResult());
      return this.installPromise;
    }

    void this.installTransaction(acceptedCallbacks).then(
      (result) => resolveInstall?.(result),
      () => {
        this.fail('bundle_partial');
        resolveInstall?.(this.fallbackResult());
      }
    );
    return this.installPromise;
  }

  public dispose(): void {
    if (this.registryState === 'disposed') return;
    if (this.registryState !== 'failed') this.registryState = 'disposed';
    this.abortController.abort();
    this.clearBootOwnership();
    this.wakeRegistrationWaiter();
    this.requestUnwind();
  }

  private readonly onOwnerAbort = (): void => {
    this.fail('bundle_partial');
  };

  private deadlineExpired(): boolean {
    const elapsed = this.now() - this.startedAtMs;
    return !Number.isFinite(elapsed) || elapsed >= BOOT_DEADLINE_MS;
  }

  private ownerIsCurrent(): boolean {
    try {
      return this.isCurrentOwner();
    } catch {
      return false;
    }
  }

  private canContinue(phase: 'preparing' | 'activating'): boolean {
    if (this.registryState !== phase) return false;
    if (!this.ownerIsCurrent() || this.deadlineExpired()) {
      this.fail('bundle_partial');
      return false;
    }
    return true;
  }

  private fail(reason: BootFailureReason): void {
    if (
      this.registryState === 'committed' ||
      this.registryState === 'failed' ||
      this.registryState === 'disposed'
    ) {
      return;
    }
    this.failureReason = reason;
    this.registryState = 'failed';
    this.abortController.abort();
    this.clearBootOwnership();
    this.wakeRegistrationWaiter();
    this.requestUnwind();
  }

  private hasEveryRequiredRegistration(): boolean {
    return Boolean(
      this.manifestValue?.integrations.every((entry) => this.registrations.has(entry.id))
    );
  }

  private wakeRegistrationWaiter(): void {
    const wake = this.registrationWaiter;
    this.registrationWaiter = undefined;
    wake?.();
  }

  private waitForRequiredRegistrations(): Promise<void> {
    if (this.registryState !== 'collecting' || this.hasEveryRequiredRegistration()) {
      return Promise.resolve();
    }
    return new Promise<void>((resolve) => {
      this.registrationWaiter = resolve;
      if (this.registryState !== 'collecting' || this.hasEveryRequiredRegistration()) {
        this.wakeRegistrationWaiter();
      }
    });
  }

  private enterOwnedCallback(): void {
    this.ownedCallbackDepth += 1;
  }

  private leaveOwnedCallback(): void {
    this.ownedCallbackDepth -= 1;
    if (this.ownedCallbackDepth === 0 && this.unwindPending) {
      this.unwindPending = false;
      this.disposePrepared();
    }
  }

  private requestUnwind(): void {
    if (this.ownedCallbackDepth > 0) {
      this.unwindPending = true;
      return;
    }
    this.disposePrepared();
  }

  private clearBootOwnership(): void {
    if (this.deadlineTimer !== undefined) {
      clearTimeout(this.deadlineTimer);
      this.deadlineTimer = undefined;
    }
    this.ownerSignal?.removeEventListener('abort', this.onOwnerAbort);
  }

  private disposePrepared(): void {
    for (let index = this.prepared.length - 1; index >= 0; index -= 1) {
      this.prepared[index]?.scope.dispose();
    }
    this.coreScope.dispose();
  }

  private fallbackResult(): IntegrationFallbackResult {
    return Object.freeze({
      state: 'fallback',
      reason: this.failureReason ?? 'bundle_partial',
    });
  }

  private async awaitPreparation(
    promise: PromiseLike<PreparedIntegration>
  ): Promise<PreparedIntegration | typeof ABORTED> {
    const observed = Promise.resolve(promise);
    if (this.abortController.signal.aborted) {
      observeThenableRejection(observed);
      return ABORTED;
    }

    let removeAbortListener: () => void = () => undefined;
    const aborted = new Promise<typeof ABORTED>((resolve) => {
      const onAbort = () => resolve(ABORTED);
      this.abortController.signal.addEventListener('abort', onAbort, { once: true });
      removeAbortListener = () => this.abortController.signal.removeEventListener('abort', onAbort);
    });

    try {
      return await Promise.race([observed, aborted]);
    } finally {
      removeAbortListener();
    }
  }

  private createPreparationContext(
    id: string,
    scope: DisposableStack
  ): { readonly context: IntegrationPrepareContext; readonly close: () => void } {
    const bindings = this.getBindings(id);
    const fields = readExactDataFields(bindings, ['config', 'interfaces']);
    if (
      !fields ||
      !isFrozenBinding(fields.config) ||
      !isRecord(fields.interfaces) ||
      !Object.isFrozen(fields.interfaces) ||
      !hasOnlyFrozenDataValues(fields.interfaces)
    ) {
      throw new TypeError('Integration bindings must expose exact frozen values');
    }

    let open = true;
    const context: IntegrationPrepareContext = Object.freeze({
      config: fields.config,
      interfaces: fields.interfaces,
      signal: this.abortController.signal,
      onDispose: (callback: DisposeCallback) => {
        if (!open && !scope.disposed) {
          throw new Error('Preparation disposal registration is closed');
        }
        scope.onDispose(callback);
      },
    });
    return Object.freeze({
      context,
      close: () => {
        open = false;
      },
    });
  }

  private async installTransaction(
    callbacks: IntegrationInstallCallbacks
  ): Promise<IntegrationInstallResult> {
    if (this.registryState === 'failed' || this.registryState === 'disposed') {
      return this.fallbackResult();
    }
    if (!this.manifestValue || !this.ownerIsCurrent() || this.deadlineExpired()) {
      this.fail('bundle_partial');
      return this.fallbackResult();
    }
    if (!this.hasEveryRequiredRegistration()) {
      await this.waitForRequiredRegistrations();
      if (
        this.registryState !== 'collecting' ||
        !this.ownerIsCurrent() ||
        this.deadlineExpired() ||
        !this.hasEveryRequiredRegistration()
      ) {
        this.fail('bundle_partial');
        return this.fallbackResult();
      }
    }

    this.registryState = 'preparing';
    if (callbacks.prepareCore) {
      let corePreparationOpen = true;
      const corePreparationContext: CorePreparationContext = Object.freeze({
        signal: this.abortController.signal,
        onDispose: (callback: DisposeCallback) => {
          if (!corePreparationOpen && !this.coreScope.disposed) {
            throw new Error('Core preparation disposal registration is closed');
          }
          this.coreScope.onDispose(callback);
        },
      });
      this.enterOwnedCallback();
      try {
        const returned = callbacks.prepareCore(corePreparationContext);
        if (isThenable(returned)) {
          observeThenableRejection(returned);
          throw new TypeError('Core preparation must be synchronous');
        }
      } catch {
        this.fail('bundle_partial');
        return this.fallbackResult();
      } finally {
        corePreparationOpen = false;
        this.leaveOwnedCallback();
      }

      if (!this.canContinue('preparing')) return this.fallbackResult();
    }
    for (const entry of this.manifestValue.integrations) {
      if (!this.canContinue('preparing')) return this.fallbackResult();

      const scope = new DisposableStack(this.onDisposalError);
      const registration = this.registrations.get(entry.id);
      if (!registration) {
        this.fail('bundle_partial');
        return this.fallbackResult();
      }

      this.prepared.push({
        id: entry.id,
        scope,
        module: Object.freeze({ activate: () => undefined }),
        afterCommit: undefined,
      });
      const recordIndex = this.prepared.length - 1;

      try {
        const { context, close } = this.createPreparationContext(entry.id, scope);
        if (!this.canContinue('preparing')) {
          close();
          return this.fallbackResult();
        }
        let pending: PreparedIntegration | PromiseLike<PreparedIntegration>;
        this.enterOwnedCallback();
        try {
          pending = registration.prepare(context);
        } finally {
          this.leaveOwnedCallback();
        }
        let prepared: PreparedIntegration | typeof ABORTED;
        if (isThenable(pending)) {
          prepared = await this.awaitPreparation(pending as PromiseLike<PreparedIntegration>);
          close();
        } else {
          close();
          prepared = pending;
        }
        if (prepared === ABORTED || !this.canContinue('preparing')) {
          this.fail('bundle_partial');
          return this.fallbackResult();
        }
        const preparedFields = readExactDataFields(prepared, ['activate']);
        if (!preparedFields || typeof preparedFields.activate !== 'function') {
          throw new TypeError('prepare must return one exact activation module');
        }
        if (!this.canContinue('preparing')) return this.fallbackResult();
        this.prepared[recordIndex] = {
          id: entry.id,
          scope,
          module: Object.freeze({
            activate: preparedFields.activate as PreparedIntegration['activate'],
          }),
          afterCommit: undefined,
        };
      } catch {
        this.fail('bundle_partial');
        return this.fallbackResult();
      }

      if (!this.canContinue('preparing')) return this.fallbackResult();
    }

    if (!this.canContinue('preparing')) return this.fallbackResult();
    this.registryState = 'activating';
    if (!this.canContinue('activating')) return this.fallbackResult();

    let coreActivationOpen = true;
    const coreContext: CoreActivationContext = Object.freeze({
      signal: this.abortController.signal,
      onDispose: (callback: DisposeCallback) => {
        if (!coreActivationOpen && !this.coreScope.disposed) {
          throw new Error('Core activation disposal registration is closed');
        }
        this.coreScope.onDispose(callback);
      },
    });
    this.enterOwnedCallback();
    try {
      const returned = callbacks.activateCore(coreContext);
      if (isThenable(returned)) {
        observeThenableRejection(returned);
        throw new TypeError('Core activation must be synchronous');
      }
    } catch {
      this.fail('bundle_partial');
      return this.fallbackResult();
    } finally {
      coreActivationOpen = false;
      this.leaveOwnedCallback();
    }

    if (!this.canContinue('activating')) return this.fallbackResult();

    for (const record of this.prepared) {
      if (!this.canContinue('activating')) return this.fallbackResult();

      let activationOpen = true;
      let afterCommitRegistered = false;
      let activationInvalid = false;
      const context: IntegrationActivationContext = Object.freeze({
        signal: this.abortController.signal,
        onDispose: (callback: DisposeCallback) => {
          if (!activationOpen && !record.scope.disposed) {
            throw new Error('Activation disposal registration is closed');
          }
          record.scope.onDispose(callback);
        },
        afterCommit: (callback: () => void) => {
          if (!activationOpen) throw new Error('Activation is closed');
          if (typeof callback !== 'function') throw new TypeError('afterCommit must be a function');
          if (afterCommitRegistered) {
            activationInvalid = true;
            throw new Error('afterCommit may be registered only once');
          }
          afterCommitRegistered = true;
          record.afterCommit = callback;
        },
      });

      this.enterOwnedCallback();
      try {
        const returned = record.module.activate(context);
        if (isThenable(returned)) {
          observeThenableRejection(returned);
          throw new TypeError('Integration activation must be synchronous');
        }
        if (activationInvalid) {
          throw new Error('Integration activation violated the afterCommit contract');
        }
      } catch {
        this.fail('bundle_partial');
        return this.fallbackResult();
      } finally {
        activationOpen = false;
        this.leaveOwnedCallback();
      }

      if (!this.canContinue('activating')) return this.fallbackResult();
    }

    // This final monotonic check closes the timer-task delay gap. A same-thread
    // activation that never returns cannot be preempted by JavaScript.
    if (!this.canContinue('activating')) return this.fallbackResult();

    this.registryState = 'publishing';
    this.enterOwnedCallback();
    try {
      const published = callbacks.publish();
      if (isThenable(published)) {
        observeThenableRejection(published);
        throw new TypeError('Kernel publication must be synchronous');
      }
    } catch {
      this.fail('bundle_partial');
      return this.fallbackResult();
    } finally {
      this.leaveOwnedCallback();
    }

    if (this.registryState !== 'publishing') {
      this.fail('bundle_partial');
      return this.fallbackResult();
    }

    this.registryState = 'committed';
    this.clearBootOwnership();
    const runtimeFailures: IntegrationRuntimeFailure[] = [];
    for (const record of this.prepared) {
      if (!record.afterCommit) continue;
      try {
        record.afterCommit();
      } catch {
        record.scope.dispose();
        const failure = Object.freeze({ id: record.id, phase: 'after_commit' as const });
        runtimeFailures.push(failure);
        try {
          this.onRuntimeFailure(failure);
        } catch {
          // Runtime failure reporting is bounded observation, never control flow.
        }
      }
    }

    try {
      const drained = callbacks.drainPreload();
      if (isThenable(drained)) {
        observeThenableRejection(drained, (error) => this.reportDisposalError(error));
      }
    } catch (error) {
      this.reportDisposalError(error);
    }

    return Object.freeze({
      state: 'kernel',
      runtimeFailures: Object.freeze(runtimeFailures),
      dispose: () => this.dispose(),
    });
  }

  private reportDisposalError(error: unknown): void {
    try {
      this.onDisposalError(error);
    } catch {
      // The queue owns per-callback isolation; an observer cannot undo commit.
    }
  }
}

export function createIntegrationRegistry(
  options: IntegrationRegistryOptions
): IntegrationRegistry {
  const owner = new IntegrationRegistryOwner(options);
  return Object.freeze({
    get state() {
      return owner.state;
    },
    get manifest() {
      return owner.manifest;
    },
    register: (candidate: unknown) => owner.register(candidate),
    install: (callbacks: IntegrationInstallCallbacks) => owner.install(callbacks),
    dispose: () => owner.dispose(),
  });
}
