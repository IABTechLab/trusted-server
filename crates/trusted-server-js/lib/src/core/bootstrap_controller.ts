import type { BootFailureReason } from '../kernel/fallback';
import type { PreparedKernelTakeover } from '../kernel/integration_registry';
import type { FirstDisplaySliceId } from '../kernel/release_catalog';
import type { FirstDisplayAgent } from '../first_display/agent';
import { coordinatePreparedFirstDisplayTakeoverV1 } from '../first_display/handoff';
import {
  FIRST_DISPLAY_REGISTRATION_FIELD,
  snapshotFirstDisplayComponentRegistration,
} from '../first_display/registration';
import { createFirstDisplayTransaction } from '../first_display/transaction';
import { installFirstDisplayTakeoverTransport } from '../shared/takeover';

const BOOT_DEADLINE_MS = 10_000;
const POST_PAINT_DEADLINE_MS = 10_000;
const RUNTIME_SRC = /^\/static\/tsjs=tsjs-unified\.min\.js\?v=[0-9a-f]{64}$/;

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

export type FirstDisplayArtifactControllerState = 'collecting' | 'active' | 'failed' | 'disposed';

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

export type FirstDisplayTakeoverCoordinatorState =
  'waiting' | 'bound' | 'committed' | 'failed' | 'disposed';

export interface FirstDisplayTakeoverCoordinatorOptions {
  readonly outline: unknown;
  readonly isCurrentGeneration: () => boolean;
  readonly authenticateRuntimeScript: () => boolean;
  readonly disposeAgentOwner?: () => void;
  readonly onFailure: (reason: BootFailureReason) => void;
}

export interface FirstDisplayTakeoverCoordinator {
  readonly state: FirstDisplayTakeoverCoordinatorState;
  readonly bindAgent: (agent: FirstDisplayAgent) => boolean;
  readonly observeNativeMutation: () => boolean;
  readonly coordinateTakeover: (prepared: PreparedKernelTakeover) => void;
  readonly dispose: () => void;
}

export type PersistentRuntimeLoaderState =
  'idle' | 'loading' | 'loaded' | 'committed' | 'failed' | 'disposed';

export interface PersistentRuntimeLoaderOptions {
  readonly document: Document;
  readonly agentScript: HTMLScriptElement;
  readonly runtimeSrc: string;
  readonly onMutation: () => void;
  readonly onFailure: (reason: BootFailureReason) => void;
}

export interface PersistentRuntimeLoader {
  readonly state: PersistentRuntimeLoaderState;
  readonly request: () => boolean;
  readonly authenticate: () => boolean;
  readonly commit: () => boolean;
  readonly dispose: () => void;
}

export type FirstDisplayBootstrapRuntimeBridgeState =
  'waiting' | 'loading' | 'committed' | 'failed' | 'disposed';

export interface FirstDisplayBootstrapRuntimeBridgeOptions {
  readonly target: object;
  readonly document: Document;
  readonly agentScript: HTMLScriptElement;
  readonly runtimeSrc: string;
  readonly outline: unknown;
  readonly isCurrentGeneration: () => boolean;
  readonly now: () => number;
  readonly setTimer: (callback: () => void, delayMs: number) => unknown;
  readonly clearTimer: (handle: unknown) => void;
  readonly disposeAgentArtifact: () => void;
  readonly onFailure: (reason: BootFailureReason) => void;
}

export interface FirstDisplayBootstrapRuntimeBridge {
  readonly state: FirstDisplayBootstrapRuntimeBridgeState;
  readonly bindAgent: (agent: FirstDisplayAgent) => boolean;
  readonly onProtectedPaint: () => boolean;
  readonly observeNativeMutation: () => boolean;
  readonly dispose: () => void;
}

function bootstrapDocumentOrigin(document: Document): string | undefined {
  try {
    const origin = document.defaultView?.location.origin;
    if (origin !== 'null') {
      const parsed = new URL(origin ?? '');
      return (parsed.protocol === 'http:' || parsed.protocol === 'https:') &&
        parsed.username === '' &&
        parsed.password === '' &&
        parsed.origin === origin
        ? parsed.origin
        : undefined;
    }
    const view = document.defaultView as
      (Window & { readonly __tsCreativeOrigin?: unknown }) | null;
    const stamp = view && Object.getOwnPropertyDescriptor(view, '__tsCreativeOrigin');
    if (!stamp || stamp.configurable || stamp.enumerable || !('value' in stamp) || stamp.writable) {
      return undefined;
    }
    const parsed = new URL(String(stamp.value));
    return (parsed.protocol === 'http:' || parsed.protocol === 'https:') &&
      parsed.username === '' &&
      parsed.password === '' &&
      parsed.origin === stamp.value
      ? parsed.origin
      : undefined;
  } catch {
    return undefined;
  }
}

/** Request the sole persistent artifact only when the protected-paint callback invokes `request`. */
export function createPersistentRuntimeLoader(
  options: PersistentRuntimeLoaderOptions
): PersistentRuntimeLoader {
  let state: PersistentRuntimeLoaderState = 'idle';
  let runtimeScript: HTMLScriptElement | undefined;
  let failurePublished = false;
  const origin = bootstrapDocumentOrigin(options.document);
  const expectedUrl =
    origin && RUNTIME_SRC.test(options.runtimeSrc)
      ? new URL(options.runtimeSrc, origin).href
      : undefined;
  const fail = (): false => {
    if (state === 'disposed') return false;
    state = 'failed';
    const script = runtimeScript;
    runtimeScript = undefined;
    if (script) {
      script.onload = null;
      script.onerror = null;
      try {
        script.remove();
      } catch {
        // A hostile publisher mutation cannot restore loader authority.
      }
    }
    if (!failurePublished) {
      failurePublished = true;
      try {
        options.onFailure('bundle_partial');
      } catch {
        // Failure publication cannot restart the runtime request.
      }
    }
    return false;
  };
  const authentic = (): boolean => {
    if (!runtimeScript || !expectedUrl || !runtimeScript.isConnected) return false;
    try {
      const matches = options.document.querySelectorAll('script#trustedserver-js-runtime');
      return (
        matches.length === 1 &&
        matches[0] === runtimeScript &&
        runtimeScript.ownerDocument === options.document &&
        runtimeScript.src === expectedUrl
      );
    } catch {
      return false;
    }
  };

  return Object.freeze({
    get state() {
      return state;
    },
    request: (): boolean => {
      if (state !== 'idle' || !expectedUrl) return fail();
      try {
        const agentMatches = options.document.querySelectorAll('script#trustedserver-js');
        if (
          agentMatches.length !== 1 ||
          agentMatches[0] !== options.agentScript ||
          !options.agentScript.isConnected ||
          options.agentScript.ownerDocument !== options.document
        ) {
          return fail();
        }
        const script = options.document.createElement('script');
        script.id = 'trustedserver-js-runtime';
        script.async = true;
        if (options.agentScript.nonce !== '') script.nonce = options.agentScript.nonce;
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
        const policy = trustedTypes?.trustedTypes?.createPolicy('trusted-server#tsjs-runtime-v1', {
          createScriptURL: (value: string) => {
            if (value !== expectedUrl) throw new TypeError('Runtime script URL is not admitted');
            return value;
          },
        });
        const assigned = policy ? policy.createScriptURL(expectedUrl) : expectedUrl;
        script.src = assigned as string;
        if (script.src !== expectedUrl) return fail();
        script.onload = () => {
          if (state !== 'loading' || !authentic()) {
            fail();
            return;
          }
          state = 'loaded';
        };
        script.onerror = () => fail();
        runtimeScript = script;
        state = 'loading';
        (options.document.head ?? options.document.documentElement).append(script);
        options.onMutation();
        return authentic();
      } catch {
        return fail();
      }
    },
    authenticate: authentic,
    commit: (): boolean => {
      if ((state !== 'loading' && state !== 'loaded') || !authentic() || !runtimeScript) {
        return false;
      }
      runtimeScript.onload = null;
      runtimeScript.onerror = null;
      state = 'committed';
      return true;
    },
    dispose: (): void => {
      if (state === 'disposed') return;
      const script = runtimeScript;
      runtimeScript = undefined;
      const remove = state !== 'committed';
      state = 'disposed';
      if (!script) return;
      script.onload = null;
      script.onerror = null;
      try {
        if (remove) script.remove();
      } catch {
        // Disposal is best-effort after generation invalidation.
      }
    },
  });
}

/** Join one provisional agent to one prepared persistent transaction without public state. */
export function createFirstDisplayTakeoverCoordinator(
  options: FirstDisplayTakeoverCoordinatorOptions
): FirstDisplayTakeoverCoordinator {
  let state: FirstDisplayTakeoverCoordinatorState = 'waiting';
  let agent: FirstDisplayAgent | undefined;
  let agentOwnerDisposed = false;
  let failurePublished = false;
  const disposeAgent = (selected: FirstDisplayAgent): void => {
    if (agentOwnerDisposed) return;
    agentOwnerDisposed = true;
    try {
      if (options.disposeAgentOwner) options.disposeAgentOwner();
      else selected.dispose();
    } catch {
      try {
        selected.dispose();
      } catch {
        // Generation latching keeps an unsuccessfully disposed owner inert.
      }
    }
  };
  const fail = (selected?: FirstDisplayAgent): void => {
    if (state === 'committed' || state === 'disposed') return;
    state = 'failed';
    agent = undefined;
    if (selected) disposeAgent(selected);
    if (failurePublished) return;
    failurePublished = true;
    try {
      options.onFailure('bundle_partial');
    } catch {
      // Failure publication cannot retain either ownership epoch.
    }
  };

  return Object.freeze({
    get state() {
      return state;
    },
    bindAgent: (candidate: FirstDisplayAgent): boolean => {
      if (
        state !== 'waiting' ||
        !Object.isFrozen(candidate) ||
        typeof candidate.finalizeHandoff !== 'function' ||
        typeof candidate.detachCommittedArtifacts !== 'function' ||
        typeof candidate.snapshot !== 'function' ||
        typeof candidate.dispose !== 'function' ||
        !options.isCurrentGeneration()
      ) {
        fail(candidate);
        return false;
      }
      agent = candidate;
      agentOwnerDisposed = false;
      state = 'bound';
      return true;
    },
    observeNativeMutation: (): boolean =>
      state === 'bound' && agent?.observeNativeMutation() === true,
    coordinateTakeover: (prepared: PreparedKernelTakeover): void => {
      const selected = agent;
      if (state !== 'bound' || !selected || selected.state !== 'painted') {
        fail(selected);
        throw new Error('First-display agent is unavailable for takeover');
      }
      const finalized = selected.finalizeHandoff();
      if (!finalized) {
        fail(selected);
        throw new Error('First-display handoff did not finalize');
      }
      const succeeded = coordinatePreparedFirstDisplayTakeoverV1({
        prepared,
        finalized,
        outline: options.outline,
        isCurrentGeneration: options.isCurrentGeneration,
        authenticateRuntimeScript: options.authenticateRuntimeScript,
        currentMutationRevision: () => selected.snapshot().mutationRevision,
        quiesceAgent: () => {
          if (selected.state !== 'painted') throw new Error('First-display agent is not quiesced');
        },
        detachCommittedArtifacts: () => {
          if (!selected.detachCommittedArtifacts()) {
            throw new Error('First-display artifacts did not detach');
          }
        },
        disposeAgent: () => disposeAgent(selected),
        onFailure: () => fail(selected),
      });
      if (!succeeded) {
        fail(selected);
        throw new Error('First-display takeover failed');
      }
      agent = undefined;
      state = 'committed';
    },
    dispose: (): void => {
      if (state === 'disposed') return;
      const selected = agent;
      agent = undefined;
      state = 'disposed';
      if (selected) disposeAgent(selected);
    },
  });
}

/** Join the painted agent, sole runtime loader, deadline, and one-use takeover transport. */
export function createFirstDisplayBootstrapRuntimeBridge(
  options: FirstDisplayBootstrapRuntimeBridgeOptions
): FirstDisplayBootstrapRuntimeBridge | undefined {
  let state: FirstDisplayBootstrapRuntimeBridgeState = 'waiting';
  let startedAtMs: number | undefined;
  let lastNow = Number.NEGATIVE_INFINITY;
  let deadline: unknown;
  let failurePublished = false;
  let releaseTransport: (() => void) | undefined;

  const clearDeadline = (): void => {
    if (deadline === undefined) return;
    const handle = deadline;
    deadline = undefined;
    try {
      options.clearTimer(handle);
    } catch {
      // The terminal state remains authoritative over a hostile timer primitive.
    }
  };
  const beforeDeadline = (): boolean => {
    if (startedAtMs === undefined) return false;
    try {
      const observedAt = options.now();
      if (!Number.isFinite(observedAt) || observedAt < lastNow) return false;
      lastNow = observedAt;
      return observedAt - startedAtMs < POST_PAINT_DEADLINE_MS;
    } catch {
      return false;
    }
  };
  const fail = (reason: BootFailureReason): false => {
    if (state === 'committed' || state === 'disposed' || state === 'failed') return false;
    state = 'failed';
    clearDeadline();
    releaseTransport?.();
    releaseTransport = undefined;
    loader?.dispose();
    coordinator?.dispose();
    if (!failurePublished) {
      failurePublished = true;
      try {
        options.onFailure(reason);
      } catch {
        // Failure publication cannot reopen either ownership epoch.
      }
    }
    return false;
  };

  const coordinator = createFirstDisplayTakeoverCoordinator({
    outline: options.outline,
    isCurrentGeneration: options.isCurrentGeneration,
    authenticateRuntimeScript: () =>
      state === 'loading' && beforeDeadline() && loader.authenticate(),
    disposeAgentOwner: options.disposeAgentArtifact,
    onFailure: (reason) => {
      fail(reason);
    },
  });
  const loader = createPersistentRuntimeLoader({
    document: options.document,
    agentScript: options.agentScript,
    runtimeSrc: options.runtimeSrc,
    onMutation: () => {
      if (!coordinator.observeNativeMutation()) {
        throw new TypeError('runtime insertion mutation was not admitted');
      }
    },
    onFailure: (reason) => {
      fail(reason);
    },
  });
  releaseTransport = installFirstDisplayTakeoverTransport(options.target, (prepared) => {
    if (state !== 'loading') {
      fail('bundle_partial');
      throw new Error('First-display takeover is not loading');
    }
    try {
      coordinator.coordinateTakeover(prepared);
      if (coordinator.state !== 'committed' || !beforeDeadline() || !loader.commit()) {
        throw new Error('First-display runtime did not commit');
      }
      clearDeadline();
      releaseTransport?.();
      releaseTransport = undefined;
      state = 'committed';
    } catch (error) {
      fail('bundle_partial');
      throw error;
    }
  });
  if (!releaseTransport) {
    fail('abi_mismatch');
    return undefined;
  }

  return Object.freeze({
    get state() {
      return state;
    },
    bindAgent: (agent: FirstDisplayAgent): boolean =>
      state === 'waiting' && coordinator.bindAgent(agent),
    onProtectedPaint: (): boolean => {
      if (state !== 'waiting' || coordinator.state !== 'bound') return fail('bundle_partial');
      try {
        const observedAt = options.now();
        if (!Number.isFinite(observedAt) || observedAt < 0) return fail('bundle_partial');
        startedAtMs = observedAt;
        lastNow = observedAt;
        state = 'loading';
        deadline = options.setTimer(() => fail('bundle_partial'), POST_PAINT_DEADLINE_MS);
        if (deadline === undefined || !loader.request()) return fail('bundle_partial');
        return true;
      } catch {
        return fail('bundle_partial');
      }
    },
    observeNativeMutation: (): boolean =>
      state === 'loading' && coordinator.observeNativeMutation(),
    dispose: (): void => {
      if (state === 'disposed') return;
      clearDeadline();
      releaseTransport?.();
      releaseTransport = undefined;
      loader.dispose();
      coordinator.dispose();
      state = 'disposed';
    },
  });
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
