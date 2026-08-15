declare const __TSJS_SERVER_BOOT_INPUT_V1__: unknown;

import type {
  FirstDisplayAgent,
  FirstDisplayAgentRegistrationHostV1,
  FirstDisplayBootstrapController,
} from '../first_display/agent';
import type { PreparedKernelTakeover } from '../kernel/integration_registry';
import type { FirstDisplaySliceId } from '../kernel/release_catalog';
import type { FirstDisplaySliceActivationContext } from '../shared/first_display_transaction';

import { EMBEDDED_RELEASE_ID } from './release_id';

const FIRST_DISPLAY_SRC =
  /^\/static\/tsjs=tsjs-first-display\.min\.js\?m=[0-9a-f]{4}&v=[0-9a-f]{64}$/;
const RUNTIME_SRC = /^\/static\/tsjs=tsjs-unified\.min\.js\?v=[0-9a-f]{64}$/;
const HASH = /^[0-9a-f]{64}$/;
const RELEASE = EMBEDDED_RELEASE_ID;

interface BootstrapInputV1 {
  readonly target: object & { boot?: unknown; que?: unknown };
  readonly boot: Readonly<{
    abi: 1;
    releaseId: string;
    manifest: Readonly<{
      version: 1;
      releaseId: string;
      firstDisplay: Readonly<{ src: string; slices: readonly string[] }> | null;
      runtimeSrc: string;
      integrations: readonly unknown[];
    }>;
    auctionProjection: Readonly<{
      version: 1;
      auction: Readonly<{ version: 1; results: readonly Readonly<{ slot: string }>[] }>;
      slots: readonly unknown[];
      bids: readonly unknown[];
    }>;
    creative: Readonly<{
      version: 1;
      enabled: boolean;
      clickGuard: boolean;
      renderGuard: boolean;
    }>;
    diagnostics: Readonly<{
      version: 1;
      renderTraceOverlay: boolean;
      gpt: Readonly<{ active: boolean }>;
    }>;
  }>;
  readonly outline: Readonly<{
    version: 1;
    releaseId: string;
    generation: number;
    projectionDigest: string;
    slices: readonly string[];
    slotCount: number;
    outcomeCount: number;
    capabilities: readonly string[];
    objectKinds: readonly string[];
  }> | null;
}

interface ComponentRegistration {
  readonly id: string;
  readonly prepare: (host: unknown) => unknown;
}

type BootFailureReason = 'abi_mismatch' | 'bundle_partial';

class RuntimeUnavailableError extends Error {
  public readonly code = 'runtime_unavailable';

  public constructor(
    public readonly releaseId: string,
    public readonly reason: BootFailureReason
  ) {
    super('TSJS runtime is unavailable');
    this.name = 'TsjsUnavailableError';
  }
}

function deepFreeze(value: unknown): void {
  if (typeof value !== 'object' || value === null || Object.isFrozen(value)) return;
  for (const key of Reflect.ownKeys(value)) {
    const descriptor = Object.getOwnPropertyDescriptor(value, key);
    if (descriptor && 'value' in descriptor) deepFreeze(descriptor.value);
  }
  Object.freeze(value);
}

function snapshotInput(candidate: unknown): BootstrapInputV1 | undefined {
  try {
    // Rust emits this value as a same-script lexical. The two executable artifacts
    // independently validate the projection and takeover data at their boundaries.
    const input = candidate as BootstrapInputV1;
    const { target, boot, outline } = input;
    const { manifest } = boot;
    const { firstDisplay, integrations } = manifest;
    const slices = firstDisplay?.slices;
    if (
      (typeof target !== 'object' && typeof target !== 'function') ||
      target === null ||
      boot.abi !== 1 ||
      boot.releaseId !== RELEASE ||
      manifest.version !== 1 ||
      manifest.releaseId !== RELEASE ||
      !RUNTIME_SRC.test(manifest.runtimeSrc) ||
      !Array.isArray(integrations) ||
      integrations.length > 20 ||
      (firstDisplay === null
        ? outline !== null
        : !FIRST_DISPLAY_SRC.test(firstDisplay.src) ||
          !Array.isArray(slices) ||
          slices.length === 0 ||
          slices.length > 13 ||
          slices[0] !== 'first_display' ||
          new Set(slices).size !== slices.length ||
          outline === null ||
          outline.version !== 1 ||
          outline.releaseId !== RELEASE ||
          !Number.isInteger(outline.generation) ||
          outline.generation < 1 ||
          !HASH.test(outline.projectionDigest) ||
          !Array.isArray(outline.slices) ||
          outline.slices.length !== slices.length ||
          outline.slices.some((id, index) => id !== slices[index]) ||
          outline.slotCount < 1)
    ) {
      return undefined;
    }
    deepFreeze(boot);
    deepFreeze(outline);
    return { target, boot, outline };
  } catch {
    return undefined;
  }
}

function prepareIngress(target: BootstrapInputV1['target']): unknown[] {
  const descriptor = Object.getOwnPropertyDescriptor(target, 'que');
  const previous =
    descriptor && 'value' in descriptor && Array.isArray(descriptor.value) ? descriptor.value : [];
  const queue = Object.isExtensible(previous) ? previous : previous.slice();
  Object.defineProperty(queue, 'push', {
    configurable: true,
    value: Array.prototype.push,
    writable: true,
  });
  Object.defineProperty(target, 'que', {
    configurable: true,
    enumerable: true,
    value: queue,
    writable: false,
  });
  return queue;
}

function fallbackFields(
  boot: BootstrapInputV1['boot'],
  reason: BootFailureReason,
  initialDisplayCommitted: boolean
): Readonly<Record<string, unknown>> {
  const safeBoot = {
    abi: 1,
    releaseId: RELEASE,
    manifest: {
      version: 1,
      releaseId: RELEASE,
      firstDisplay: boot.manifest.firstDisplay,
      runtimeSrc: boot.manifest.runtimeSrc,
      integrations: [],
    },
    auctionProjection: boot.auctionProjection,
    creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
    diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
  };
  deepFreeze(safeBoot);
  let level = 'warn';
  const levels = ['silent', 'error', 'warn', 'info', 'debug'];
  const observe = (..._values: readonly unknown[]): void => undefined;
  const fields: Record<string, unknown> = {
    version: '1.0.0',
    releaseId: RELEASE,
    boot: safeBoot,
    log: Object.freeze({
      setLevel: (value: string) => {
        if (!levels.includes(value)) throw new TypeError('Invalid TSJS log level');
        level = value;
      },
      getLevel: () => level,
      error: observe,
      warn: observe,
      info: observe,
      debug: observe,
    }),
    _registerIntegration: () => false,
    addAdUnits: (_units: unknown): never => {
      throw new RuntimeUnavailableError(RELEASE, reason);
    },
    requestAds: async (): Promise<never> => {
      throw new RuntimeUnavailableError(RELEASE, reason);
    },
  };
  Object.defineProperty(fields, '_internal', {
    value: Object.freeze({
      state: 'fallback',
      releaseId: RELEASE,
      reason,
      initialDisplayCommitted,
    }),
  });
  return Object.freeze(fields);
}

function publishFallback(
  target: BootstrapInputV1['target'],
  ingress: unknown[],
  fields: Readonly<Record<string, unknown>>
): boolean {
  try {
    const fieldNames = Object.getOwnPropertyNames(fields);
    for (const name of Object.getOwnPropertyNames(target)) {
      if (!Object.getOwnPropertyDescriptor(target, name)?.configurable) return false;
    }
    const callbacks: Array<() => void> = [];
    for (let index = 0; index < ingress.length; index += 1) {
      const descriptor = Object.getOwnPropertyDescriptor(ingress, String(index));
      if (descriptor && 'value' in descriptor && typeof descriptor.value === 'function') {
        callbacks.push(descriptor.value as () => void);
      }
    }
    const invoke = (callback: unknown): number => {
      if (typeof callback === 'function') {
        try {
          Reflect.apply(callback, target, []);
        } catch {
          // One publisher callback cannot block the remaining queue.
        }
      }
      return 0;
    };
    const queue: unknown[] = [];
    Object.defineProperty(queue, 'push', { value: invoke });
    Object.freeze(queue);
    ingress.length = 0;
    Object.defineProperty(ingress, 'push', { value: invoke });
    Object.freeze(ingress);
    for (const name of Object.getOwnPropertyNames(target)) Reflect.deleteProperty(target, name);
    for (const name of fieldNames) {
      const descriptor = Object.getOwnPropertyDescriptor(fields, name)!;
      Object.defineProperty(target, name, {
        enumerable: descriptor.enumerable ?? false,
        value: descriptor.value,
      });
    }
    Object.defineProperty(target, 'que', { enumerable: true, value: queue });
    for (const callback of callbacks) queue.push(callback);
    return true;
  } catch {
    return false;
  }
}

function installBootstrap({ target, boot, outline }: BootstrapInputV1): void {
  const ingress = prepareIngress(target);
  const firstDisplay = boot.manifest.firstDisplay;
  const trustedOrigin = (): string | undefined => {
    try {
      if (/^https?:\/\//.test(location.origin)) return location.origin;
      if (location.origin !== 'null') return undefined;
      const stamp = Object.getOwnPropertyDescriptor(window, '__tsCreativeOrigin');
      if (
        !stamp ||
        stamp.configurable ||
        stamp.enumerable ||
        !('value' in stamp) ||
        stamp.writable ||
        typeof stamp.value !== 'string'
      ) {
        return undefined;
      }
      const parsed = new URL(stamp.value);
      return (parsed.protocol === 'http:' || parsed.protocol === 'https:') &&
        parsed.username === '' &&
        parsed.password === '' &&
        parsed.origin === stamp.value
        ? parsed.origin
        : undefined;
    } catch {
      return undefined;
    }
  };
  const authentic = (script: HTMLScriptElement, source: string, id: string): boolean => {
    try {
      const origin = trustedOrigin();
      if (!origin) return false;
      const expected = new URL(source, origin);
      const matches = document.querySelectorAll(`script#${id}`);
      return (
        expected.origin === origin &&
        expected.hash === '' &&
        script.id === id &&
        script.isConnected &&
        script.ownerDocument === document &&
        script.src === expected.href &&
        matches.length === 1 &&
        matches[0] === script
      );
    } catch {
      return false;
    }
  };
  if (firstDisplay === null) {
    let terminal = false;
    let cancelRuntime: (() => void) | undefined;
    let timer: number | undefined;
    const removeClaim = (): void => {
      try {
        const descriptor = Object.getOwnPropertyDescriptor(target, '_claimDirectRuntime');
        if (descriptor?.configurable) Reflect.deleteProperty(target, '_claimDirectRuntime');
      } catch {
        // A hostile replacement cannot make the captured claim callable again.
      }
    };
    const fallback = (): void => {
      if (terminal) return;
      terminal = true;
      if (timer !== undefined) window.clearTimeout(timer);
      try {
        cancelRuntime?.();
      } catch {
        // Continue to the terminal shell after releasing the persistent owner.
      }
      removeClaim();
      publishFallback(target, ingress, fallbackFields(boot, 'bundle_partial', false));
    };
    const claim = (
      source: unknown,
      cancel: unknown
    ): ((outcome: 'kernel' | 'runtime_fallback' | 'failed_start') => void) | undefined => {
      if (
        terminal ||
        !(source instanceof HTMLScriptElement) ||
        document.currentScript !== source ||
        !authentic(source, boot.manifest.runtimeSrc, 'trustedserver-js') ||
        typeof cancel !== 'function' ||
        cancelRuntime
      ) {
        return undefined;
      }
      cancelRuntime = cancel as () => void;
      return (outcome): void => {
        if (terminal) return;
        if (outcome === 'failed_start') {
          fallback();
          return;
        }
        terminal = true;
        if (timer !== undefined) window.clearTimeout(timer);
        removeClaim();
      };
    };
    try {
      Object.defineProperty(target, 'boot', {
        configurable: true,
        enumerable: true,
        value: boot,
        writable: false,
      });
      Object.defineProperty(target, '_claimDirectRuntime', {
        configurable: true,
        enumerable: false,
        value: claim,
        writable: false,
      });
      performance.mark('tsjs:bids-script');
      timer = window.setTimeout(fallback, 10_000);
    } catch {
      fallback();
    }
    return;
  }
  if (outline === null) {
    publishFallback(target, ingress, fallbackFields(boot, 'abi_mismatch', false));
    return;
  }
  const disposers: Array<() => void> = [];
  const registrations: ComponentRegistration[] = [];
  let current = true;
  let terminal = false;
  let agent: FirstDisplayAgent | undefined;
  let agentScript: HTMLScriptElement | undefined;
  let runtimeScript: HTMLScriptElement | undefined;
  let bootstrapTimer: number | undefined;
  let takeoverTimer: number | undefined;
  let takeoverStartedAt = 0;
  let controllerState: 'installing' | 'agent_registered' | 'action_started' | 'settled' | 'failed' =
    'installing';

  const clear = (handle: number | undefined): void => {
    if (handle !== undefined) window.clearTimeout(handle);
  };
  const removePrivate = (key: string): void => {
    try {
      const descriptor = Object.getOwnPropertyDescriptor(target, key);
      if (descriptor?.configurable) Reflect.deleteProperty(target, key);
    } catch {
      // Generation invalidation makes an unremovable private field inert.
    }
  };
  const disposeAgent = (): void => {
    removePrivate('_registerFirstDisplay');
    for (let index = disposers.length - 1; index >= 0; index -= 1) {
      try {
        disposers[index]?.();
      } catch {
        // Continue releasing every independently owned provisional effect.
      }
    }
    disposers.length = 0;
    registrations.length = 0;
    agent = undefined;
  };
  const commitFallback = (reason: BootFailureReason): void => {
    if (terminal) return;
    const initialDisplayCommitted = agent?.snapshot().initialDisplayCommitted ?? false;
    terminal = true;
    current = false;
    controllerState = 'failed';
    clear(bootstrapTimer);
    clear(takeoverTimer);
    removePrivate('_firstDisplayTakeover');
    if (runtimeScript) {
      runtimeScript.onload = null;
      runtimeScript.onerror = null;
      runtimeScript.remove();
    }
    disposeAgent();
    publishFallback(target, ingress, fallbackFields(boot, reason, initialDisplayCommitted));
  };
  const now = (): number => performance.now();
  const startedAtMs = now();
  const controller: FirstDisplayBootstrapController = Object.freeze({
    get state() {
      return controllerState;
    },
    startedAtMs,
    registerAgent: (): boolean => {
      if (controllerState !== 'installing' || now() - startedAtMs >= 10_000) return false;
      controllerState = 'agent_registered';
      return true;
    },
    startAction: (): boolean => {
      if (controllerState !== 'agent_registered' || now() - startedAtMs >= 10_000) return false;
      controllerState = 'action_started';
      return true;
    },
    settle: (): boolean => {
      if (controllerState !== 'agent_registered' && controllerState !== 'action_started') {
        return false;
      }
      controllerState = 'settled';
      clear(bootstrapTimer);
      return true;
    },
    fail: (reason: BootFailureReason): boolean => {
      if (controllerState === 'settled' || controllerState === 'failed') return false;
      commitFallback(reason);
      return true;
    },
  });

  const bridge = (selected: FirstDisplayAgent): void => {
    agent = selected;
    const coordinate = (prepared: PreparedKernelTakeover): void => {
      let activated = false;
      try {
        if (
          terminal ||
          !current ||
          !runtimeScript ||
          now() - takeoverStartedAt >= 10_000 ||
          !authentic(runtimeScript, boot.manifest.runtimeSrc, 'trustedserver-js-runtime') ||
          agent?.state !== 'painted'
        ) {
          throw new TypeError('tsjs');
        }
        const finalized = agent.finalizeHandoff();
        const handoff = finalized && prepared.validateHandoff(finalized.handoff, outline);
        if (!handoff || agent.snapshot().mutationRevision !== handoff.mutationRevision) {
          throw new TypeError('tsjs');
        }
        const identities = finalized.capsule.consume(handoff.releaseId, handoff.generation);
        if (!identities || !agent.detachCommittedArtifacts()) throw new TypeError('tsjs');
        disposeAgent();
        prepared.activate(
          Object.freeze({ version: 1, adoptInitialDisplay: true, handoff, identities })
        );
        activated = true;
        if (
          !current ||
          now() - takeoverStartedAt >= 10_000 ||
          !authentic(runtimeScript, boot.manifest.runtimeSrc, 'trustedserver-js-runtime')
        ) {
          throw new TypeError('tsjs');
        }
        prepared.commit();
        terminal = true;
        clear(takeoverTimer);
        removePrivate('_firstDisplayTakeover');
      } catch (error) {
        if (activated) {
          try {
            prepared.rollback();
          } catch {
            // Fallback remains terminal after a persistent rollback error.
          }
        }
        commitFallback('bundle_partial');
        throw error;
      }
    };
    Object.defineProperty(target, '_firstDisplayTakeover', {
      configurable: true,
      enumerable: false,
      value: coordinate,
      writable: false,
    });
  };
  const protectedPaint = (): void => {
    try {
      if (terminal || !agent || agent.state !== 'painted' || runtimeScript) {
        throw new TypeError('tsjs');
      }
      takeoverStartedAt = now();
      takeoverTimer = window.setTimeout(() => commitFallback('bundle_partial'), 10_000);
      const script = document.createElement('script');
      script.id = 'trustedserver-js-runtime';
      script.async = true;
      if (agentScript?.nonce) script.nonce = agentScript.nonce;
      script.src = new URL(boot.manifest.runtimeSrc, location.origin).href;
      script.onerror = () => commitFallback('bundle_partial');
      runtimeScript = script;
      (document.head ?? document.documentElement).append(script);
      if (
        !authentic(script, boot.manifest.runtimeSrc, script.id) ||
        !agent.observeNativeMutation()
      ) {
        throw new TypeError('tsjs');
      }
    } catch {
      commitFallback('bundle_partial');
    }
  };
  const protocols = new Map<string, unknown>();
  const binding = (id: string): unknown => {
    const observe = (): void => undefined;
    const register = (value: unknown): (() => void) => {
      if (protocols.has(id)) throw new TypeError('tsjs');
      protocols.set(id, value);
      return () => {
        if (protocols.get(id) === value) protocols.delete(id);
      };
    };
    if (id === 'gpt_initial') return Object.freeze({ observe, register });
    if (id === 'aps_initial') {
      return Object.freeze({ observe, publisherOrigin: location.origin, register });
    }
    if (id === 'creative_initial') {
      return Object.freeze({
        config: Object.freeze({
          version: 1,
          enabled: true,
          clickGuard: boot.creative.clickGuard,
          renderGuard: boot.creative.renderGuard,
        }),
        location: Object.freeze({ href: location.href, origin: location.origin }),
        observe,
        register,
      });
    }
    return undefined;
  };
  const host: FirstDisplayAgentRegistrationHostV1 = Object.freeze({
    options: Object.freeze({
      batch: Object.freeze({
        version: 1,
        projectionDigest: outline.projectionDigest,
        projection: boot.auctionProjection,
      }),
      bootstrap: controller,
      performance,
      paint: Object.freeze({
        hidden: () => document.visibilityState === 'hidden',
        requestFrame: (callback: () => void) => window.requestAnimationFrame(() => callback()),
        scheduleHidden: (callback: () => void) => window.setTimeout(callback, 0),
      }),
      onProtectedPaint: protectedPaint,
      onFailure: commitFallback,
      gptInput: Object.freeze({
        browser: window,
        clearTimer: (handle: unknown) => window.clearTimeout(handle as number),
        diagnosticsActive: boot.diagnostics.gpt.active,
        document,
        setTimer: (callback: () => void, delayMs: number) => window.setTimeout(callback, delayMs),
      }),
      handoff: Object.freeze({
        releaseId: RELEASE,
        generation: outline.generation,
        slices: firstDisplay.slices as readonly FirstDisplaySliceId[],
      }),
      now,
      onAgentReady: bridge,
    }),
    sliceBindings: binding,
  });

  const fail = (): false => {
    commitFallback('abi_mismatch');
    return false;
  };
  const register = function (this: unknown, candidate: unknown, source: unknown): boolean {
    try {
      const expectedId = firstDisplay.slices[registrations.length];
      if (
        terminal ||
        this !== target ||
        !(source instanceof HTMLScriptElement) ||
        document.currentScript !== source ||
        (agentScript && source !== agentScript) ||
        !authentic(source, firstDisplay.src, 'trustedserver-js') ||
        typeof candidate !== 'object' ||
        candidate === null
      ) {
        return fail();
      }
      const fields = candidate as Readonly<{
        abi: unknown;
        id: unknown;
        releaseId: unknown;
        prepare: unknown;
      }>;
      if (
        fields.abi !== 1 ||
        fields.id !== expectedId ||
        fields.releaseId !== RELEASE ||
        typeof fields.prepare !== 'function'
      ) {
        return fail();
      }
      agentScript = source;
      registrations.push({
        id: fields.id as string,
        prepare: fields.prepare as (host: unknown) => unknown,
      });
      if (registrations.length !== firstDisplay.slices.length) return true;
      removePrivate('_registerFirstDisplay');
      const prepared: Array<(context: FirstDisplaySliceActivationContext) => void> = [];
      let sliceHost: unknown;
      for (const component of registrations) {
        const value = component.prepare(component.id === 'first_display' ? host : sliceHost);
        if (typeof value !== 'object' || value === null || !Object.isFrozen(value)) {
          return fail();
        }
        const activate = Object.getOwnPropertyDescriptor(value, 'activate')?.value;
        if (typeof activate !== 'function') return fail();
        if (component.id === 'first_display') {
          sliceHost = Object.getOwnPropertyDescriptor(value, 'sliceHost')?.value;
          if (typeof sliceHost !== 'object' || sliceHost === null || !Object.isFrozen(sliceHost)) {
            return fail();
          }
        }
        prepared.push(activate as (context: FirstDisplaySliceActivationContext) => void);
      }
      let afterActivate: (() => void) | undefined;
      for (let index = 0; index < prepared.length; index += 1) {
        prepared[index]!(
          Object.freeze({
            own: (dispose: () => void) => {
              if (typeof dispose !== 'function') throw new TypeError('tsjs');
              disposers.push(dispose);
            },
            afterActivate: (callback: () => void) => {
              if (index !== 0 || afterActivate || typeof callback !== 'function') {
                throw new TypeError('tsjs');
              }
              afterActivate = callback;
            },
          })
        );
      }
      afterActivate?.();
      return Boolean(agent && !terminal);
    } catch {
      return fail();
    }
  };

  try {
    Object.defineProperty(target, 'boot', {
      configurable: true,
      enumerable: true,
      value: boot,
      writable: false,
    });
    Object.defineProperty(target, '_registerFirstDisplay', {
      configurable: true,
      enumerable: false,
      value: register,
      writable: false,
    });
    performance.mark('tsjs:bids-script');
    bootstrapTimer = window.setTimeout(() => commitFallback('bundle_partial'), 10_000);
  } catch {
    commitFallback('abi_mismatch');
  }
}

const input = snapshotInput(
  typeof __TSJS_SERVER_BOOT_INPUT_V1__ === 'undefined' ? undefined : __TSJS_SERVER_BOOT_INPUT_V1__
);
if (input) installBootstrap(input);
