import type { BootManifestDeferredIntegrationV1, BootManifestV1 } from '../core/types';

import { DisposableStack, type DisposeCallback } from './disposable';
import type {
  IntegrationActivationContext,
  IntegrationRegistration,
  PreparedIntegration,
} from './integration_registry';
import { snapshotIntegrationRegistration } from './integration_registry';

const ATTEMPT_CREATION_GUARD_MS = 10_000;
const HIDDEN_PAINT_TIMEOUT_MS = 2_000;
const IDLE_TIMEOUT_MS = 2_000;
const IDLE_FALLBACK_MS = 50;
const MODULE_TRANSACTION_TIMEOUT_MS = 10_000;
const RELEASE_ID = /^[0-9a-f]{64}$/;

type TimerHandle = ReturnType<typeof setTimeout>;

export interface PhaseScheduler {
  readonly setTimeout: (callback: () => void, delayMs: number) => TimerHandle;
  readonly clearTimeout: (handle: TimerHandle) => void;
  readonly requestAnimationFrame: (callback: FrameRequestCallback) => number;
  readonly cancelAnimationFrame: (handle: number) => void;
  readonly requestIdleCallback?: (
    callback: () => void,
    options: Readonly<{ timeout: number }>
  ) => number;
  readonly cancelIdleCallback?: (handle: number) => void;
}

export interface ProtectedFirstDisplayGate {
  readonly ready: Promise<boolean>;
  readonly commit: () => void;
  readonly protectAttemptBatch: (terminalLatches: readonly PromiseLike<unknown>[]) => boolean;
  readonly dispose: () => void;
}

export interface ProtectedFirstDisplayGateOptions {
  readonly document: Document;
  readonly scheduler?: PhaseScheduler;
  readonly markPaint?: () => void;
  /** The provisional agent already completed the terminal/two-frame paint gate. */
  readonly paintAlreadyRecorded?: boolean;
}

function browserScheduler(document: Document): PhaseScheduler {
  const view = document.defaultView;
  if (!view) throw new TypeError('Phase scheduler requires a browser window');
  const idle = view as Window & {
    cancelIdleCallback?: (handle: number) => void;
    requestIdleCallback?: (callback: () => void, options: { timeout: number }) => number;
  };
  return Object.freeze({
    cancelAnimationFrame: (handle: number) => view.cancelAnimationFrame(handle),
    clearTimeout: (handle: TimerHandle) => clearTimeout(handle),
    requestAnimationFrame: (callback: FrameRequestCallback) => view.requestAnimationFrame(callback),
    ...(typeof idle.requestIdleCallback === 'function'
      ? {
          cancelIdleCallback: (handle: number) => idle.cancelIdleCallback?.(handle),
          requestIdleCallback: (callback: () => void, options: { timeout: number }) =>
            idle.requestIdleCallback?.(callback, options) ?? 0,
        }
      : {}),
    setTimeout: (callback: () => void, delayMs: number) => setTimeout(callback, delayMs),
  });
}

/** Own the only attempt-terminal, paint, and idle gate for deferred work. */
export function createProtectedFirstDisplayGate(
  options: ProtectedFirstDisplayGateOptions
): ProtectedFirstDisplayGate {
  const scheduler = options.scheduler ?? browserScheduler(options.document);
  let committed = false;
  let disposed = false;
  let protectedBatch = false;
  let releasedFromAttemptGuard = false;
  let attemptTimer: TimerHandle | undefined;
  let hiddenTimer: TimerHandle | undefined;
  let idleTimer: TimerHandle | undefined;
  let frameOne: number | undefined;
  let frameTwo: number | undefined;
  let idleHandle: number | undefined;
  let resolveReady: ((ready: boolean) => void) | undefined;
  const ready = new Promise<boolean>((resolve) => {
    resolveReady = resolve;
  });

  const clearOwnedScheduling = (): void => {
    if (attemptTimer !== undefined) scheduler.clearTimeout(attemptTimer);
    if (hiddenTimer !== undefined) scheduler.clearTimeout(hiddenTimer);
    if (idleTimer !== undefined) scheduler.clearTimeout(idleTimer);
    if (frameOne !== undefined) scheduler.cancelAnimationFrame(frameOne);
    if (frameTwo !== undefined) scheduler.cancelAnimationFrame(frameTwo);
    if (idleHandle !== undefined) scheduler.cancelIdleCallback?.(idleHandle);
    attemptTimer = undefined;
    hiddenTimer = undefined;
    idleTimer = undefined;
    frameOne = undefined;
    frameTwo = undefined;
    idleHandle = undefined;
  };

  const finish = (): void => {
    if (disposed) return;
    clearOwnedScheduling();
    resolveReady?.(true);
    resolveReady = undefined;
  };

  const scheduleIdle = (): void => {
    if (disposed) return;
    if (scheduler.requestIdleCallback) {
      idleHandle = scheduler.requestIdleCallback(finish, { timeout: IDLE_TIMEOUT_MS });
      return;
    }
    idleTimer = scheduler.setTimeout(finish, IDLE_FALLBACK_MS);
  };

  const markAndScheduleIdle = (): void => {
    if (disposed) return;
    try {
      options.markPaint?.();
    } finally {
      scheduleIdle();
    }
  };

  const waitForTwoFrames = (): void => {
    if (disposed) return;
    frameOne = scheduler.requestAnimationFrame(() => {
      frameOne = undefined;
      if (disposed) return;
      frameTwo = scheduler.requestAnimationFrame(() => {
        frameTwo = undefined;
        markAndScheduleIdle();
      });
    });
  };

  const onVisibility = (): void => {
    if (options.document.visibilityState !== 'visible' || disposed) return;
    options.document.removeEventListener('visibilitychange', onVisibility);
    if (hiddenTimer !== undefined) scheduler.clearTimeout(hiddenTimer);
    hiddenTimer = undefined;
    waitForTwoFrames();
  };

  const enterPaintGate = (): void => {
    if (releasedFromAttemptGuard || disposed) return;
    releasedFromAttemptGuard = true;
    if (options.document.visibilityState === 'hidden') {
      options.document.addEventListener('visibilitychange', onVisibility);
      hiddenTimer = scheduler.setTimeout(() => {
        hiddenTimer = undefined;
        options.document.removeEventListener('visibilitychange', onVisibility);
        markAndScheduleIdle();
      }, HIDDEN_PAINT_TIMEOUT_MS);
      return;
    }
    waitForTwoFrames();
  };

  return Object.freeze({
    ready,
    commit: () => {
      if (committed || disposed) return;
      committed = true;
      if (options.paintAlreadyRecorded) {
        releasedFromAttemptGuard = true;
        scheduleIdle();
        return;
      }
      attemptTimer = scheduler.setTimeout(() => {
        attemptTimer = undefined;
        enterPaintGate();
      }, ATTEMPT_CREATION_GUARD_MS);
    },
    protectAttemptBatch: (terminalLatches: readonly PromiseLike<unknown>[]) => {
      if (!committed || disposed || protectedBatch || releasedFromAttemptGuard) return false;
      protectedBatch = true;
      if (attemptTimer !== undefined) scheduler.clearTimeout(attemptTimer);
      attemptTimer = undefined;
      void Promise.allSettled([...terminalLatches]).then(enterPaintGate);
      return true;
    },
    dispose: () => {
      if (disposed) return;
      disposed = true;
      options.document.removeEventListener('visibilitychange', onVisibility);
      clearOwnedScheduling();
      resolveReady?.(false);
      resolveReady = undefined;
    },
  });
}

export type DeferredModuleState =
  'not_triggered' | 'loading' | 'registered' | 'preparing' | 'activating' | 'ready' | 'unavailable';

export type DeferredModuleUnavailableReason =
  | 'load_error'
  | 'load_without_registration'
  | 'registration_rejected'
  | 'prepare_failed'
  | 'activation_failed'
  | 'after_commit_failed'
  | 'policy_blocked'
  | 'module_timeout'
  | 'disposed';

export interface DeferredPhaseLoader {
  readonly register: (candidate: unknown) => boolean;
  readonly state: (id: string) => DeferredModuleState | undefined;
  readonly reason: (id: string) => DeferredModuleUnavailableReason | undefined;
  readonly waitFor: (
    id: string,
    timeoutMs: number
  ) => Promise<'ready' | 'unavailable' | 'caller_timeout'>;
  readonly dispose: () => void;
}

export interface DeferredPhaseLoaderOptions {
  readonly runtimeScript: HTMLScriptElement;
  readonly document: Document;
  readonly prepare: (
    registration: IntegrationRegistration,
    owner: Readonly<{
      signal: AbortSignal;
      onDispose: (callback: DisposeCallback) => void;
    }>
  ) => PreparedIntegration | PromiseLike<PreparedIntegration>;
  readonly gate: PromiseLike<boolean | void>;
  readonly manifest: BootManifestV1;
  readonly releaseId: string;
  readonly scheduler?: Pick<PhaseScheduler, 'clearTimeout' | 'setTimeout'>;
}

interface DeferredTransaction {
  readonly entry: BootManifestDeferredIntegrationV1;
  readonly expectedUrl: string;
  readonly abortController: AbortController;
  readonly scope: DisposableStack;
  script: HTMLScriptElement | undefined;
  registration: IntegrationRegistration | undefined;
  state: DeferredModuleState;
  reason: DeferredModuleUnavailableReason | undefined;
  timeout: TimerHandle | undefined;
  readonly waiters: Set<{
    readonly resolve: (result: 'ready' | 'unavailable') => void;
    readonly timer: TimerHandle;
  }>;
}

function manifestDeferredTransactions(
  manifest: BootManifestV1,
  releaseId: string,
  document: Document
): readonly DeferredTransaction[] {
  if (manifest.releaseId !== releaseId || !RELEASE_ID.test(releaseId)) return Object.freeze([]);
  const origin = document.defaultView?.location.origin;
  if (!origin || origin === 'null') return Object.freeze([]);
  const transactions: DeferredTransaction[] = [];
  for (const entry of manifest.integrations) {
    if (entry.phase !== 'deferred') continue;
    try {
      const expectedUrl: URL = new URL(entry.src, origin);
      if (
        expectedUrl.origin !== origin ||
        expectedUrl.hash !== '' ||
        `${expectedUrl.pathname}${expectedUrl.search}` !== entry.src
      ) {
        return Object.freeze([]);
      }
      transactions.push({
        entry,
        expectedUrl: expectedUrl.href,
        abortController: new AbortController(),
        scope: new DisposableStack(),
        script: undefined,
        registration: undefined,
        state: 'not_triggered',
        reason: undefined,
        timeout: undefined,
        waiters: new Set(),
      });
    } catch {
      return Object.freeze([]);
    }
  }
  return Object.freeze(transactions);
}

/** Load and authenticate every deferred classic IIFE after the shared paint gate. */
export function createDeferredPhaseLoader(
  options: DeferredPhaseLoaderOptions
): DeferredPhaseLoader {
  const scheduler =
    options.scheduler ??
    Object.freeze({
      clearTimeout: (handle: TimerHandle) => clearTimeout(handle),
      setTimeout: (callback: () => void, delayMs: number) => setTimeout(callback, delayMs),
    });
  const transactions = manifestDeferredTransactions(
    options.manifest,
    options.releaseId,
    options.document
  );
  const byId = new Map(transactions.map((transaction) => [transaction.entry.id, transaction]));
  let disposed = false;
  let privatePolicy: Readonly<{ createScriptURL: (value: string) => unknown }> | null | undefined;
  const exactUrls = new Set(transactions.map(({ expectedUrl }) => expectedUrl));

  const removeNode = (transaction: DeferredTransaction): void => {
    const script = transaction.script;
    if (!script) return;
    script.onload = null;
    script.onerror = null;
    try {
      script.remove();
    } catch {
      // A hostile publisher mutation cannot prevent transaction retirement.
    }
    transaction.script = undefined;
  };

  const settleWaiters = (
    transaction: DeferredTransaction,
    result: 'ready' | 'unavailable'
  ): void => {
    for (const waiter of transaction.waiters) {
      scheduler.clearTimeout(waiter.timer);
      waiter.resolve(result);
    }
    transaction.waiters.clear();
  };

  const settleUnavailable = (
    transaction: DeferredTransaction,
    reason: DeferredModuleUnavailableReason
  ): void => {
    if (transaction.state === 'ready' || transaction.state === 'unavailable') return;
    transaction.state = 'unavailable';
    transaction.reason = reason;
    transaction.abortController.abort();
    transaction.scope.dispose();
    if (transaction.timeout !== undefined) scheduler.clearTimeout(transaction.timeout);
    transaction.timeout = undefined;
    removeNode(transaction);
    settleWaiters(transaction, 'unavailable');
  };

  const exactPreparedIntegration = (candidate: unknown): PreparedIntegration | undefined => {
    try {
      if (
        typeof candidate !== 'object' ||
        candidate === null ||
        Array.isArray(candidate) ||
        Object.getPrototypeOf(candidate) !== Object.prototype ||
        Reflect.ownKeys(candidate).length !== 1
      ) {
        return undefined;
      }
      const descriptor = Object.getOwnPropertyDescriptor(candidate, 'activate');
      if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) return undefined;
      if (typeof descriptor.value !== 'function') return undefined;
      return Object.freeze({ activate: descriptor.value as PreparedIntegration['activate'] });
    } catch {
      return undefined;
    }
  };

  const finishExecution = async (transaction: DeferredTransaction): Promise<void> => {
    const registration = transaction.registration;
    if (!registration) {
      settleUnavailable(transaction, 'load_without_registration');
      return;
    }
    if (!transaction.script?.isConnected) {
      settleUnavailable(transaction, 'registration_rejected');
      return;
    }
    removeNode(transaction);
    transaction.state = 'preparing';
    let pending: PreparedIntegration | PromiseLike<PreparedIntegration>;
    try {
      pending = options.prepare(
        registration,
        Object.freeze({
          signal: transaction.abortController.signal,
          onDispose: (callback: DisposeCallback) => transaction.scope.onDispose(callback),
        })
      );
    } catch {
      settleUnavailable(transaction, 'prepare_failed');
      return;
    }
    let prepared: PreparedIntegration | undefined;
    try {
      prepared = exactPreparedIntegration(await pending);
    } catch {
      settleUnavailable(transaction, 'prepare_failed');
      return;
    }
    if (!prepared) {
      settleUnavailable(transaction, 'prepare_failed');
      return;
    }
    if (transaction.abortController.signal.aborted) return;
    transaction.state = 'activating';
    let afterCommit: (() => void) | undefined;
    let afterCommitRegistered = false;
    let activationOpen = true;
    const activationContext: IntegrationActivationContext = Object.freeze({
      signal: transaction.abortController.signal,
      onDispose: (callback: DisposeCallback) => {
        if (!activationOpen && !transaction.scope.disposed) {
          throw new Error('Deferred activation disposal registration is closed');
        }
        transaction.scope.onDispose(callback);
      },
      afterCommit: (callback: () => void) => {
        if (!activationOpen || afterCommitRegistered || typeof callback !== 'function') {
          throw new Error('Deferred afterCommit may be registered exactly once');
        }
        afterCommitRegistered = true;
        afterCommit = callback;
      },
    });
    try {
      const returned = prepared.activate(activationContext) as unknown;
      if (
        (typeof returned === 'object' || typeof returned === 'function') &&
        returned !== null &&
        typeof (returned as { then?: unknown }).then === 'function'
      ) {
        void Promise.resolve(returned).catch(() => undefined);
        throw new TypeError('Deferred activation must be synchronous');
      }
    } catch {
      settleUnavailable(transaction, 'activation_failed');
      return;
    } finally {
      activationOpen = false;
    }
    if (transaction.abortController.signal.aborted) return;
    try {
      afterCommit?.();
    } catch {
      settleUnavailable(transaction, 'after_commit_failed');
      return;
    }
    if (transaction.abortController.signal.aborted) return;
    transaction.state = 'ready';
    if (transaction.timeout !== undefined) scheduler.clearTimeout(transaction.timeout);
    transaction.timeout = undefined;
    settleWaiters(transaction, 'ready');
  };

  const createPolicy = (): typeof privatePolicy => {
    if (privatePolicy !== undefined) return privatePolicy;
    privatePolicy = null;
    try {
      const trustedTypes = options.document.defaultView as
        | (Window & {
            trustedTypes?: {
              createPolicy: (
                name: string,
                rules: Readonly<{ createScriptURL: (value: string) => string }>
              ) => Readonly<{ createScriptURL: (value: string) => unknown }>;
            };
          })
        | null;
      if (!trustedTypes?.trustedTypes) return privatePolicy;
      privatePolicy = trustedTypes.trustedTypes.createPolicy('trusted-server#tsjs-v1', {
        createScriptURL: (value: string) => {
          if (!exactUrls.has(value)) throw new TypeError('Deferred script URL is not admitted');
          return value;
        },
      });
    } catch {
      privatePolicy = null;
    }
    return privatePolicy;
  };

  const start = (transaction: DeferredTransaction): void => {
    if (disposed || transaction.state !== 'not_triggered') return;
    transaction.state = 'loading';
    transaction.timeout = scheduler.setTimeout(
      () => settleUnavailable(transaction, 'module_timeout'),
      MODULE_TRANSACTION_TIMEOUT_MS
    );
    const script = options.document.createElement('script');
    script.async = true;
    const nonce = options.runtimeScript.nonce;
    if (nonce !== '') script.nonce = nonce;
    transaction.script = script;
    try {
      const policy = createPolicy();
      const assigned = policy
        ? policy.createScriptURL(transaction.expectedUrl)
        : transaction.expectedUrl;
      script.src = assigned as string;
      if (script.src !== transaction.expectedUrl) {
        settleUnavailable(transaction, 'policy_blocked');
        return;
      }
    } catch {
      settleUnavailable(transaction, 'policy_blocked');
      return;
    }
    script.onload = () => {
      void finishExecution(transaction);
    };
    script.onerror = () => settleUnavailable(transaction, 'load_error');
    try {
      (options.document.head ?? options.document.documentElement).append(script);
    } catch {
      settleUnavailable(transaction, 'load_error');
    }
  };

  void Promise.resolve(options.gate).then(
    (gateReady) => {
      if (disposed || gateReady === false) {
        for (const transaction of transactions) settleUnavailable(transaction, 'disposed');
        return;
      }
      for (const transaction of transactions) start(transaction);
    },
    () => {
      for (const transaction of transactions) settleUnavailable(transaction, 'disposed');
    }
  );

  return Object.freeze({
    register: (candidate: unknown) => {
      const registration = snapshotIntegrationRegistration(candidate);
      if (
        !registration ||
        registration.phase !== 'deferred' ||
        registration.releaseId !== options.releaseId
      ) {
        return false;
      }
      const transaction = byId.get(registration.id);
      if (!transaction || transaction.state !== 'loading' || transaction.registration) {
        if (transaction) settleUnavailable(transaction, 'registration_rejected');
        return false;
      }
      const currentScript = options.document.currentScript;
      const Script = options.document.defaultView?.HTMLScriptElement;
      if (
        !Script ||
        !(currentScript instanceof Script) ||
        currentScript !== transaction.script ||
        !currentScript.isConnected ||
        currentScript.src !== transaction.expectedUrl
      ) {
        settleUnavailable(transaction, 'registration_rejected');
        return false;
      }
      transaction.registration = registration;
      transaction.state = 'registered';
      return true;
    },
    state: (id: string) => byId.get(id)?.state,
    reason: (id: string) => byId.get(id)?.reason,
    waitFor: (
      id: string,
      timeoutMs: number
    ): Promise<'ready' | 'unavailable' | 'caller_timeout'> => {
      const transaction = byId.get(id);
      if (!transaction || transaction.state === 'unavailable') {
        return Promise.resolve('unavailable');
      }
      if (transaction.state === 'ready') return Promise.resolve('ready');
      if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) {
        return Promise.resolve('caller_timeout');
      }
      return new Promise<'ready' | 'unavailable' | 'caller_timeout'>((resolve) => {
        const waiter: {
          resolve: (result: 'ready' | 'unavailable') => void;
          timer: TimerHandle;
        } = {
          resolve,
          timer: undefined as unknown as TimerHandle,
        };
        waiter.timer = scheduler.setTimeout(() => {
          transaction.waiters.delete(waiter);
          resolve('caller_timeout');
        }, timeoutMs);
        transaction.waiters.add(waiter);
      });
    },
    dispose: () => {
      if (disposed) return;
      disposed = true;
      for (let index = transactions.length - 1; index >= 0; index -= 1) {
        const transaction = transactions[index];
        if (!transaction) continue;
        if (transaction.state === 'ready') {
          transaction.abortController.abort();
          transaction.scope.dispose();
          continue;
        }
        settleUnavailable(transaction, 'disposed');
      }
    },
  });
}
