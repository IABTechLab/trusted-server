declare const __TSJS_SERVER_BOOT_TRANSPORT_V1__: unknown;

import {
  prepareFirstDisplayBase,
  type FirstDisplayAgent,
  type FirstDisplayAgentRegistrationHostV1,
} from '../first_display/agent';
import type { InitialSliceInstaller } from '../first_display/slices/definition';
import type { FirstDisplaySliceId } from '../kernel/release_catalog';
import type { FirstDisplaySliceActivationContext } from '../shared/first_display_transaction';
import type { ClaimedFirstDisplayTakeoverV1, FirstDisplayTakeoverClaim } from '../shared/takeover';

import {
  deepFreezeTransportV1,
  snapshotServerBootTransportV1,
  type ServerBootTransportSnapshotV1,
} from './contracts/server_boot_transport';
import { EMBEDDED_RELEASE_ID } from './release_id';

const RELEASE = EMBEDDED_RELEASE_ID;

interface ComponentRegistration {
  readonly id: Exclude<FirstDisplaySliceId, 'first_display'>;
  readonly install: InitialSliceInstaller;
}

type BootstrapTarget = object & { boot?: unknown; que?: unknown };

interface BootstrapInputSnapshotV1 extends ServerBootTransportSnapshotV1 {
  readonly target: BootstrapTarget;
}

type BootFailureReason = 'abi_mismatch' | 'bundle_partial';
type BootClaimOutcome = 'kernel' | BootFailureReason;

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

function prepareIngress(target: BootstrapTarget): unknown[] {
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
  boot: BootstrapInputSnapshotV1['boot'],
  reason: BootFailureReason,
  initialDisplayCommitted: boolean
): Readonly<Record<string, unknown>> {
  const unavailable = (): never => {
    throw new RuntimeUnavailableError(RELEASE, reason);
  };
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
    auctionProjection: {
      version: 1,
      auction: { version: 1, auctionId: 'fallback', results: [] },
      slots: [],
      bids: [],
    },
    integrations: { version: 1, entries: [] },
    creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
    diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
  };
  deepFreezeTransportV1(safeBoot);
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
    addAdUnits: unavailable,
    requestAds: async (): Promise<never> => unavailable(),
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
  target: BootstrapInputSnapshotV1['target'],
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

function installBootstrap({ target, boot, integrity, outline }: BootstrapInputSnapshotV1): void {
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
  let bootClaimState: 'available' | 'claiming' | 'claimed' = 'available';
  let bootClaimCompleted = false;
  let completeClaimedBoot: ((outcome: BootClaimOutcome) => void) | undefined;
  const removeBootClaim = (): void => {
    try {
      const descriptor = Object.getOwnPropertyDescriptor(target, '_claimBootSnapshot');
      if (descriptor?.configurable && 'value' in descriptor && descriptor.value === claimBoot) {
        Reflect.deleteProperty(target, '_claimBootSnapshot');
      }
    } catch {
      // The closure-retained snapshot remains authoritative after hostile replacement.
    }
  };
  const completeBootClaim = (outcome: BootClaimOutcome): void => {
    const acceptedOutcome =
      outcome === 'kernel' || outcome === 'abi_mismatch' || outcome === 'bundle_partial';
    if (
      bootClaimState !== 'claimed' ||
      bootClaimCompleted ||
      !completeClaimedBoot ||
      !acceptedOutcome
    ) {
      return;
    }
    bootClaimCompleted = true;
    const complete = completeClaimedBoot;
    completeClaimedBoot = undefined;
    complete(outcome);
  };
  const claimedBoot = Object.freeze({ boot, integrity, complete: completeBootClaim });
  const claimBoot = (source: unknown): Readonly<typeof claimedBoot> | undefined => {
    if (bootClaimState !== 'available') return undefined;
    bootClaimState = 'claiming';
    let accepted = false;
    try {
      const expectedId = firstDisplay === null ? 'trustedserver-js' : 'trustedserver-js-runtime';
      accepted =
        source instanceof HTMLScriptElement &&
        document.currentScript === source &&
        authentic(source, boot.manifest.runtimeSrc, expectedId);
    } catch {
      // The claim stays reserved only until this authentication attempt fails.
    }
    if (!accepted) {
      bootClaimState = 'available';
      return undefined;
    }
    bootClaimState = 'claimed';
    removeBootClaim();
    return claimedBoot;
  };
  const installBootClaim = (): void => {
    Object.defineProperty(target, '_claimBootSnapshot', {
      configurable: true,
      enumerable: false,
      value: claimBoot,
      writable: false,
    });
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
    const fallback = (reason: BootFailureReason = 'bundle_partial'): void => {
      if (terminal) return;
      terminal = true;
      if (timer !== undefined) window.clearTimeout(timer);
      try {
        cancelRuntime?.();
      } catch {
        // Continue to the terminal shell after releasing the persistent owner.
      }
      removeClaim();
      removeBootClaim();
      publishFallback(target, ingress, fallbackFields(boot, reason, false));
    };
    completeClaimedBoot = (outcome) => {
      if (outcome === 'abi_mismatch') fallback(outcome);
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
      installBootClaim();
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
    while (disposers.length > 0) {
      const dispose = disposers.pop();
      try {
        dispose?.();
      } catch {
        // Continue releasing every independently owned provisional effect.
      }
    }
    registrations.length = 0;
    agent = undefined;
  };
  const commitFallback = (reason: BootFailureReason): void => {
    if (terminal) return;
    const initialDisplayCommitted = agent?.initialDisplayCommitted ?? false;
    terminal = true;
    current = false;
    clear(bootstrapTimer);
    clear(takeoverTimer);
    removePrivate('_firstDisplayTakeover');
    removeBootClaim();
    if (runtimeScript) {
      runtimeScript.onload = null;
      runtimeScript.onerror = null;
      runtimeScript.remove();
    }
    disposeAgent();
    publishFallback(target, ingress, fallbackFields(boot, reason, initialDisplayCommitted));
  };
  completeClaimedBoot = (outcome) => {
    if (outcome !== 'kernel') commitFallback(outcome);
  };
  const now = (): number => performance.now();
  const startedAtMs = now();
  const bridge = (selected: FirstDisplayAgent): void => {
    agent = selected;
    let live = true;
    const claim: FirstDisplayTakeoverClaim = (
      finalize
    ): ClaimedFirstDisplayTakeoverV1 | undefined => {
      if (!live) return undefined;
      live = false;
      try {
        if (terminal || !current || selected.state !== 'painted') {
          throw new TypeError('tsjs');
        }
        const finalized = selected.finalizeHandoff(finalize);
        if (!finalized) throw new TypeError('tsjs');
        return Object.freeze([
          finalized,
          outline,
          () => current && !terminal && now() - takeoverStartedAt < 10_000,
          () =>
            Boolean(
              runtimeScript &&
              authentic(runtimeScript, boot.manifest.runtimeSrc, 'trustedserver-js-runtime')
            ),
          () => selected.mutationRevision,
          () => selected.detachCommittedArtifacts(),
          disposeAgent,
          (reason) => commitFallback(reason),
          () => {
            terminal = true;
            try {
              clear(takeoverTimer);
            } catch {
              // The committed persistent owner remains terminal if timer cleanup fails.
            }
            removePrivate('_firstDisplayTakeover');
          },
        ]);
      } catch (error) {
        commitFallback('bundle_partial');
        throw error;
      }
    };
    Object.defineProperty(target, '_firstDisplayTakeover', {
      configurable: true,
      enumerable: false,
      value: claim,
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
  const binding = (
    id: string,
    observe: (key: unknown, value: unknown) => void,
    register: ((protocol: unknown) => () => void) | undefined
  ): readonly [bindings: unknown, config: unknown] => {
    let bindings: unknown;
    if (id === 'gpt_initial') {
      bindings = Object.freeze({ browser: window, observe, register });
    } else if (id === 'render_owner_initial') {
      bindings = Object.freeze({ observe, register });
    } else if (id === 'aps_initial') {
      bindings = Object.freeze({
        observe,
        publisherOrigin: location.origin,
        register,
      });
    } else if (id === 'creative_initial') {
      bindings = Object.freeze({
        location: Object.freeze({ href: location.href, origin: location.origin }),
        observe,
        register: () => () => undefined,
      });
    } else if (id === 'testlight_initial') {
      bindings = Object.freeze({
        enqueue: (callback: () => void) => {
          ingress.push(callback);
        },
        observe,
        target: window,
      });
    } else {
      bindings = register ? Object.freeze({ observe, register }) : Object.freeze({ observe });
    }
    const product = id.endsWith('_initial') ? id.slice(0, -'_initial'.length) : '';
    let config: unknown =
      id === 'creative_initial'
        ? boot.creative
        : id === 'render_owner_initial'
          ? Object.freeze({})
          : undefined;
    if (config === undefined && product !== '') {
      for (const entry of boot.integrations.entries) {
        if (entry.id === product) {
          config = entry.config;
          break;
        }
      }
    }
    return Object.freeze([bindings, config]);
  };
  const host: FirstDisplayAgentRegistrationHostV1 = Object.freeze({
    options: Object.freeze({
      batch: Object.freeze({
        version: 1,
        projectionDigest: outline.projectionDigest,
        projection: boot.auctionProjection,
      }),
      startedAtMs,
      performance,
      paint: Object.freeze({
        hidden: () => document.visibilityState === 'hidden',
        requestFrame: (callback: () => void) => window.requestAnimationFrame(() => callback()),
        scheduleHidden: (callback: () => void) => window.setTimeout(callback, 0),
      }),
      onProtectedPaint: protectedPaint,
      onSettled: () => clear(bootstrapTimer),
      onFailure: commitFallback,
      gptInput: Object.freeze([
        window,
        (handle: unknown) => window.clearTimeout(handle as number),
        document,
        (callback: () => void, delayMs: number) => window.setTimeout(callback, delayMs),
        boot.diagnostics.gpt.active,
      ] as const),
      handoff: Object.freeze({
        releaseId: RELEASE,
        generation: outline.generation,
        integrationConfigDigest: outline.integrationConfigDigest,
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
  const activateComponents = (): boolean => {
    removePrivate('_registerFirstDisplay');
    const base = prepareFirstDisplayBase(host);
    let afterActivate: (() => void) | undefined;
    let ownershipOpen = true;
    try {
      const context: FirstDisplaySliceActivationContext = Object.freeze({
        own: (dispose: () => void) => {
          if (!ownershipOpen || terminal || typeof dispose !== 'function') {
            throw new TypeError('tsjs');
          }
          disposers.push(dispose);
        },
        afterActivate: (callback: () => void) => {
          if (!ownershipOpen || terminal || afterActivate || typeof callback !== 'function') {
            throw new TypeError('tsjs');
          }
          afterActivate = callback;
        },
      });
      base.activate(context);
      for (const component of registrations) {
        base.sliceHost.activate(component.id, context.own, component.install);
        if (terminal || !current) return false;
      }
    } finally {
      ownershipOpen = false;
    }
    afterActivate?.();
    if (terminal || !current) return false;
    return Boolean(agent && !terminal);
  };
  const register = function (this: unknown, candidate: unknown, source: unknown): boolean {
    try {
      const expectedId = firstDisplay.slices[registrations.length + 1];
      if (
        terminal ||
        this !== target ||
        !(source instanceof HTMLScriptElement) ||
        document.currentScript !== source ||
        (agentScript && source !== agentScript) ||
        !authentic(source, firstDisplay.src, 'trustedserver-js') ||
        !Array.isArray(candidate) ||
        !Object.isFrozen(candidate) ||
        candidate.length !== 4
      ) {
        return fail();
      }
      if (terminal || !current) return false;
      const fields = candidate as readonly unknown[];
      if (
        fields[0] !== 1 ||
        fields[1] !== expectedId ||
        fields[2] !== RELEASE ||
        typeof fields[3] !== 'function'
      ) {
        return fail();
      }
      agentScript = source;
      registrations.push({
        id: fields[1] as Exclude<FirstDisplaySliceId, 'first_display'>,
        install: fields[3] as InitialSliceInstaller,
      });
      if (registrations.length !== firstDisplay.slices.length - 1) return true;
      return activateComponents();
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
    installBootClaim();
    Object.defineProperty(target, '_registerFirstDisplay', {
      configurable: true,
      enumerable: false,
      value: register,
      writable: false,
    });
    performance.mark('tsjs:bids-script');
    bootstrapTimer = window.setTimeout(() => commitFallback('bundle_partial'), 10_000);
    if (firstDisplay.slices.length === 1) activateComponents();
  } catch {
    commitFallback('abi_mismatch');
  }
}

function bootstrapTarget(): BootstrapTarget | undefined {
  try {
    const namespace = window as unknown as { tsjs?: unknown };
    const current = namespace.tsjs;
    if ((typeof current === 'object' || typeof current === 'function') && current !== null) {
      return current as BootstrapTarget;
    }
    if (current) return undefined;
    const target: BootstrapTarget = {};
    namespace.tsjs = target;
    return target;
  } catch {
    return undefined;
  }
}

const transport = snapshotServerBootTransportV1(
  typeof __TSJS_SERVER_BOOT_TRANSPORT_V1__ === 'undefined'
    ? undefined
    : __TSJS_SERVER_BOOT_TRANSPORT_V1__,
  RELEASE
);
const target = transport && bootstrapTarget();
if (transport && target) installBootstrap(Object.freeze({ ...transport, target }));
