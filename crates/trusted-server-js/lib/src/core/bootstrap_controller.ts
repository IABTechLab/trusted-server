import type { BootFailureReason } from '../kernel/fallback';
import type { FirstDisplaySliceId } from '../kernel/release_catalog';
import {
  FIRST_DISPLAY_REGISTRATION_FIELD,
  snapshotFirstDisplayComponentRegistration,
} from '../first_display/registration';
import { createFirstDisplayTransaction } from '../first_display/transaction';

const BOOT_DEADLINE_MS = 10_000;

export type BootstrapControllerState =
  'installing' | 'agent_registered' | 'action_started' | 'settled' | 'failed';

export interface BootstrapControllerOptions {
  readonly performance: Readonly<{ mark: (name: string) => void }>;
  readonly now: () => number;
  readonly startedAtMs?: number;
  readonly setTimer: (callback: () => void, delayMs: number) => unknown;
  readonly clearTimer: (handle: unknown) => void;
  readonly onFailure: (reason: BootFailureReason) => void;
}

export interface BootstrapController {
  readonly state: BootstrapControllerState;
  readonly startedAtMs: number;
  readonly registerAgent: () => boolean;
  readonly startAction: () => boolean;
  readonly settle: () => boolean;
  readonly fail: (reason: BootFailureReason) => boolean;
}

export type FirstDisplayArtifactControllerState =
  | 'collecting'
  | 'active'
  | 'failed'
  | 'disposed';

export interface FirstDisplayArtifactControllerOptions {
  readonly bootstrap: BootstrapController;
  readonly target: object;
  readonly document: Document;
  readonly script: HTMLScriptElement;
  readonly releaseId: string;
  readonly generation: number;
  readonly expectedSliceIds: readonly FirstDisplaySliceId[];
  readonly isCurrentGeneration: () => boolean;
  /** Supplies only base dependencies; the base creates the private optional-slice host. */
  readonly baseHost: unknown;
  readonly onDisposalError?: (error: unknown) => void;
}

export interface FirstDisplayArtifactController {
  readonly state: FirstDisplayArtifactControllerState;
  readonly dispose: () => void;
}

/** Own the one protected bootstrap deadline and the preceding bids-script mark. */
export function createBootstrapController(
  options: BootstrapControllerOptions
): BootstrapController {
  const startedAtMs = options.startedAtMs ?? options.now();
  let state: BootstrapControllerState = 'installing';
  let timer: unknown;

  const expired = (): boolean => {
    const elapsed = options.now() - startedAtMs;
    return !Number.isFinite(elapsed) || elapsed >= BOOT_DEADLINE_MS;
  };
  const clearDeadline = (): void => {
    if (timer === undefined) return;
    try {
      options.clearTimer(timer);
    } catch {
      // A hostile timer primitive cannot reopen bootstrap ownership.
    }
    timer = undefined;
  };
  const fail = (reason: BootFailureReason): boolean => {
    if (state === 'settled' || state === 'failed') return false;
    state = 'failed';
    clearDeadline();
    try {
      options.onFailure(reason);
    } catch {
      // Failure publication cannot make the terminal controller live again.
    }
    return true;
  };

  try {
    options.performance.mark('tsjs:bids-script');
  } catch {
    // Timing observability cannot affect the boot transaction.
  }
  const remaining = Math.max(0, BOOT_DEADLINE_MS - (options.now() - startedAtMs));
  timer = options.setTimer(() => fail('bundle_partial'), remaining);

  return Object.freeze({
    get state() {
      return state;
    },
    startedAtMs,
    registerAgent: (): boolean => {
      if (state !== 'installing') return false;
      if (expired()) return fail('bundle_partial') && false;
      state = 'agent_registered';
      return true;
    },
    startAction: (): boolean => {
      if (state !== 'agent_registered') return false;
      if (expired()) return fail('bundle_partial') && false;
      state = 'action_started';
      return true;
    },
    settle: (): boolean => {
      if (state !== 'agent_registered' && state !== 'action_started') return false;
      state = 'settled';
      clearDeadline();
      return true;
    },
    fail,
  });
}

/**
 * Install the bootstrap-owned, ephemeral registration sink for one composed agent.
 * The final component closes the sink before any preparation or effect is allowed.
 */
export function createFirstDisplayArtifactController(
  options: FirstDisplayArtifactControllerOptions
): FirstDisplayArtifactController | undefined {
  let state: FirstDisplayArtifactControllerState = 'collecting';
  let count = 0;
  let sliceHost: unknown;
  let sink: ((this: unknown, candidate: unknown, source: unknown) => boolean) | undefined;
  const transaction = createFirstDisplayTransaction({
    document: options.document,
    script: options.script,
    releaseId: options.releaseId,
    generation: options.generation,
    expectedSliceIds: options.expectedSliceIds,
    isCurrentGeneration: options.isCurrentGeneration,
    ...(options.onDisposalError ? { onDisposalError: options.onDisposalError } : {}),
  });

  const closeSink = (): boolean => {
    if (!sink) return true;
    try {
      const descriptor = Object.getOwnPropertyDescriptor(
        options.target,
        FIRST_DISPLAY_REGISTRATION_FIELD
      );
      if (!descriptor || !('value' in descriptor) || descriptor.value !== sink) return false;
      if (!Reflect.deleteProperty(options.target, FIRST_DISPLAY_REGISTRATION_FIELD)) return false;
      sink = undefined;
      return true;
    } catch {
      return false;
    }
  };
  const fail = (reason: BootFailureReason): false => {
    if (state === 'failed' || state === 'disposed') return false;
    state = 'failed';
    closeSink();
    transaction.dispose();
    options.bootstrap.fail(reason);
    return false;
  };

  if (
    options.expectedSliceIds.length === 0 ||
    options.expectedSliceIds[0] !== 'first_display' ||
    Object.getOwnPropertyDescriptor(options.target, FIRST_DISPLAY_REGISTRATION_FIELD)
  ) {
    fail('abi_mismatch');
    return undefined;
  }

  sink = function (this: unknown, candidate: unknown, source: unknown): boolean {
    if (state !== 'collecting' || this !== options.target || source !== options.script) {
      return fail('abi_mismatch');
    }
    const registration = snapshotFirstDisplayComponentRegistration(candidate);
    const expectedId = options.expectedSliceIds[count];
    if (
      !registration ||
      registration.releaseId !== options.releaseId ||
      registration.id !== expectedId
    ) {
      return fail('abi_mismatch');
    }
    const registered = transaction.register({
      abi: 1,
      id: registration.id,
      releaseId: registration.releaseId,
      generation: options.generation,
      order: registration.order,
      prepare: () => {
        const prepared = registration.prepare(
          registration.id === 'first_display' ? options.baseHost : sliceHost
        );
        if (registration.id !== 'first_display') return prepared as never;
        if (
          typeof prepared !== 'object' ||
          prepared === null ||
          Array.isArray(prepared) ||
          Object.getPrototypeOf(prepared) !== Object.prototype ||
          !Object.isFrozen(prepared) ||
          Reflect.ownKeys(prepared).length !== 2
        ) {
          throw new TypeError('invalid first-display base preparation');
        }
        const activate = Object.getOwnPropertyDescriptor(prepared, 'activate');
        const host = Object.getOwnPropertyDescriptor(prepared, 'sliceHost');
        if (
          !activate?.enumerable ||
          !('value' in activate) ||
          typeof activate.value !== 'function' ||
          !host?.enumerable ||
          !('value' in host) ||
          typeof host.value !== 'object' ||
          host.value === null ||
          !Object.isFrozen(host.value)
        ) {
          throw new TypeError('invalid first-display slice host');
        }
        sliceHost = host.value;
        return Object.freeze({ activate: activate.value });
      },
    });
    if (!registered) return fail('abi_mismatch');
    count += 1;
    if (count !== options.expectedSliceIds.length) return true;
    if (!closeSink()) return fail('bundle_partial');
    if (!transaction.activate()) return fail('bundle_partial');
    state = 'active';
    return true;
  };

  try {
    Object.defineProperty(options.target, FIRST_DISPLAY_REGISTRATION_FIELD, {
      configurable: true,
      enumerable: false,
      value: sink,
      writable: false,
    });
  } catch {
    fail('abi_mismatch');
    return undefined;
  }

  return Object.freeze({
    get state() {
      return state;
    },
    dispose: (): void => {
      if (state === 'disposed') return;
      state = 'disposed';
      closeSink();
      transaction.dispose();
    },
  });
}
