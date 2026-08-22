import { prepareQueue, publishQueue, type PublishedQueue } from '../core/queue';
import { EMBEDDED_RELEASE_ID, EMBEDDED_RUNTIME_CATALOG } from '../core/release';
import type { GptDiagnosticsApi, IntegrationConfigIdV1, TsjsBootV1 } from '../core/types';

import {
  buildFallbackBoot,
  buildKernelBoot,
  captureTrustedSelectedRuntimeSrc,
  captureTrustedRuntimeSrc,
  createFallbackFields,
  publicLog,
  type BootFailureReason,
} from './fallback';
import {
  createIntegrationRegistry,
  type CoreActivationContext,
  type CorePreparationContext,
  type IntegrationBindings,
  type IntegrationCatalogEntry,
  type IntegrationInstallCallbacks,
  type IntegrationInstallResult,
  type IntegrationRegistry,
  type PreparedKernelTakeover,
} from './integration_registry';
import {
  createDeferredPhaseLoader,
  createProtectedFirstDisplayGate,
  type DeferredPhaseLoader,
  type PhaseScheduler,
  type ProtectedFirstDisplayGate,
} from './phase_loader';

export type RuntimeState = 'unclaimed' | 'installing' | 'kernel' | 'failed' | 'fallback';

type RuntimeTarget = object & { que?: unknown; boot?: unknown };

const TERMINAL_FIELDS = Object.freeze([
  'version',
  'releaseId',
  'boot',
  'log',
  'addAdUnits',
  'requestAds',
  'diagnostics',
  '_registerIntegration',
  '_internal',
  'que',
]);

function retainedProductConfig(
  boot: Readonly<TsjsBootV1>,
  id: IntegrationConfigIdV1
): TsjsBootV1['integrations']['entries'][number]['config'] | undefined {
  for (const entry of boot.integrations.entries) {
    if (entry.id === id) return entry.config;
  }
  return undefined;
}

function canClaimRuntimeTarget(target: RuntimeTarget): boolean {
  try {
    if (Object.getOwnPropertyDescriptor(target, '_registerIntegration')) return false;
    for (const key of TERMINAL_FIELDS) {
      const descriptor = Object.getOwnPropertyDescriptor(target, key);
      if (descriptor && !descriptor.configurable) return false;
    }
    return true;
  } catch {
    return false;
  }
}

function restoreOwnProperty(
  target: RuntimeTarget,
  key: string,
  descriptor: PropertyDescriptor | undefined
): void {
  try {
    if (descriptor) Object.defineProperty(target, key, descriptor);
    else Reflect.deleteProperty(target, key);
  } catch {
    // A hostile publisher Proxy cannot escape startup or block the remaining cleanup.
  }
}

export interface RuntimeKernel {
  readonly addAdUnits: (units: unknown) => unknown;
  readonly requestAds: (options?: unknown) => Promise<unknown>;
  readonly diagnostics: Readonly<object>;
}

/** Frozen activation boundary for document-lifetime owners created after preparation. */
export interface RuntimeOwnerActivationContext extends CoreActivationContext {
  readonly boot: Readonly<object>;
  readonly generation: object;
}

/** Frozen preparation boundary for inert document-lifetime owners. */
export interface RuntimeOwnerPreparationContext extends CorePreparationContext {
  readonly boot: Readonly<object>;
  readonly generation: object;
}

export interface RuntimeOptions {
  readonly target: RuntimeTarget;
  /** Server assertion only; every decision and published value is bound to the build stamp. */
  readonly releaseId: string;
  readonly manifest: unknown;
  readonly knownIntegrationIds: readonly string[];
  readonly catalog?: readonly IntegrationCatalogEntry[];
  readonly document?: Document;
  readonly phaseScheduler?: PhaseScheduler;
  readonly boot?: unknown;
  readonly now?: () => number;
  readonly getBindings?: (id: string) => IntegrationBindings;
  /** Resolve the complete frozen namespace after every diagnostics module activates. */
  readonly getDiagnosticsForPublish?: () => Readonly<object>;
  readonly prepareOwner?: (context: RuntimeOwnerPreparationContext) => void;
  readonly activateOwner?: (context: RuntimeOwnerActivationContext) => void;
  readonly activateCore?: (context: CoreActivationContext) => void;
  /** Runs the prepared persistent activation and publication inside an old-owner handoff. */
  readonly coordinateTakeover?: (prepared: PreparedKernelTakeover) => void;
  /** Production monolith hook: install inline when its final registration arrives. */
  readonly autoInstall?: boolean;
  readonly onInstallComplete?: (result: IntegrationInstallResult) => void;
  readonly kernel: RuntimeKernel;
}

export interface Runtime {
  readonly state: RuntimeState;
  readonly generation: object;
  readonly start: () => boolean;
  readonly registerIntegration: (registration: unknown) => boolean;
  readonly install: () => Promise<IntegrationInstallResult>;
  readonly protectFirstDisplayAttemptBatch: (
    terminalLatches: readonly PromiseLike<unknown>[]
  ) => boolean;
  readonly dispose: () => void;
}

/** Closure-private kernel capability consumed only by release-bound providers. */
export interface RuntimeCapabilityV1 {
  readonly attachAuctionContextService: (
    service: RuntimeAuctionContextService
  ) => (() => void) | undefined;
  readonly boot: () => Readonly<object> | undefined;
  readonly document: Document | undefined;
  readonly enqueue: (callback: () => void) => boolean;
  readonly generation: object;
  readonly protectFirstDisplayAttemptBatch: (
    terminalLatches: readonly PromiseLike<unknown>[]
  ) => boolean;
  readonly registerAuctionContext: (
    integrationId: string,
    contributor: RuntimeAuctionContextContributor
  ) => (() => void) | undefined;
}

export type RuntimeAuctionContextContributor = () => Readonly<Record<string, unknown>> | undefined;

/** Sole runtime-private bridge between the render owner and context contributors. */
export interface RuntimeAuctionContextService {
  readonly register: (
    integrationId: string,
    contributor: RuntimeAuctionContextContributor
  ) => (() => void) | undefined;
}

interface DirectCapabilityV1 {
  readonly addAdUnits: RuntimeKernel['addAdUnits'];
  readonly requestAds: RuntimeKernel['requestAds'];
  readonly diagnostics: Readonly<object>;
}

interface GptDiagnosticsCapabilityV1 {
  readonly api: GptDiagnosticsApi;
  readonly attachPresentation: (controls: Readonly<Record<string, unknown>>) => () => void;
}

function snapshotGptDiagnosticsCapability(
  candidate: unknown
): GptDiagnosticsCapabilityV1 | undefined {
  if (
    typeof candidate !== 'object' ||
    candidate === null ||
    Array.isArray(candidate) ||
    Object.getPrototypeOf(candidate) !== Object.prototype ||
    !Object.isFrozen(candidate) ||
    Reflect.ownKeys(candidate).sort().join(',') !== 'api,attachPresentation'
  ) {
    return undefined;
  }
  const fields = candidate as Readonly<Record<string, unknown>>;
  const api = fields['api'];
  if (
    typeof fields['attachPresentation'] !== 'function' ||
    typeof api !== 'object' ||
    api === null ||
    !Object.isFrozen(api) ||
    Reflect.ownKeys(api).sort().join(',') !== 'export,hide,show,snapshot,subscribe' ||
    !Reflect.ownKeys(api).every(
      (key) => typeof (api as Record<PropertyKey, unknown>)[key] === 'function'
    )
  ) {
    return undefined;
  }
  return candidate as GptDiagnosticsCapabilityV1;
}

function snapshotDirectCapability(candidate: unknown): DirectCapabilityV1 | undefined {
  if (
    typeof candidate !== 'object' ||
    candidate === null ||
    Array.isArray(candidate) ||
    Object.getPrototypeOf(candidate) !== Object.prototype ||
    !Object.isFrozen(candidate)
  ) {
    return undefined;
  }
  const keys = Reflect.ownKeys(candidate);
  if (
    keys.length !== 3 ||
    !['addAdUnits', 'requestAds', 'diagnostics'].every((key) => keys.includes(key))
  ) {
    return undefined;
  }
  const values: Record<string, unknown> = {};
  for (const key of keys) {
    if (typeof key !== 'string') return undefined;
    const descriptor = Object.getOwnPropertyDescriptor(candidate, key);
    if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) return undefined;
    values[key] = descriptor.value;
  }
  if (
    typeof values['addAdUnits'] !== 'function' ||
    typeof values['requestAds'] !== 'function' ||
    typeof values['diagnostics'] !== 'object' ||
    values['diagnostics'] === null ||
    !Object.isFrozen(values['diagnostics'])
  ) {
    return undefined;
  }
  return Object.freeze({
    addAdUnits: values['addAdUnits'] as RuntimeKernel['addAdUnits'],
    requestAds: values['requestAds'] as RuntimeKernel['requestAds'],
    diagnostics: values['diagnostics'] as Readonly<object>,
  });
}

class RuntimeOwner implements Runtime {
  public readonly generation = Object.freeze({});
  private readonly options: RuntimeOptions;
  private readonly registrationHandshake = (candidate: unknown): boolean =>
    this.registerIntegration(candidate);
  private runtimeState: RuntimeState = 'unclaimed';
  private registry: IntegrationRegistry | undefined;
  private ingress: unknown[] | undefined;
  private installPromise: Promise<IntegrationInstallResult> | undefined;
  private kernelBoot: Readonly<TsjsBootV1> | undefined;
  private fallbackBoot: Readonly<object> | undefined;
  private selectedScript: HTMLScriptElement | undefined;
  private trustedRuntimeSrc: string | undefined;
  private phaseGate: ProtectedFirstDisplayGate | undefined;
  private phaseLoader: DeferredPhaseLoader | undefined;
  private auctionContextService: RuntimeAuctionContextService | undefined;
  private directProvider: DirectCapabilityV1 | undefined;
  private gptDiagnosticsProvider: GptDiagnosticsCapabilityV1 | undefined;

  public constructor(options: RuntimeOptions) {
    this.options = options;
  }

  public get state(): RuntimeState {
    return this.runtimeState;
  }

  public start(): boolean {
    if (this.runtimeState !== 'unclaimed') return false;
    let queueDescriptor: PropertyDescriptor | undefined;
    let bootDescriptor: PropertyDescriptor | undefined;
    let registrationDescriptor: PropertyDescriptor | undefined;
    let claimMutationStarted = false;
    try {
      if (!canClaimRuntimeTarget(this.options.target)) return false;
      const runtimeDocument = this.runtimeDocument();
      const Script = runtimeDocument?.defaultView?.HTMLScriptElement;
      const currentScript = runtimeDocument?.currentScript;
      this.selectedScript = Script && currentScript instanceof Script ? currentScript : undefined;
      const takeoverMode = this.options.coordinateTakeover !== undefined;
      const capturedRuntimeSrc =
        runtimeDocument && this.selectedScript
          ? takeoverMode
            ? captureTrustedRuntimeSrc(runtimeDocument, this.selectedScript)
            : captureTrustedSelectedRuntimeSrc(runtimeDocument, this.selectedScript)
          : undefined;
      if (!capturedRuntimeSrc) return false;
      this.trustedRuntimeSrc = capturedRuntimeSrc;
      const startedAtMs = (this.options.now ?? (() => performance.now()))();
      queueDescriptor = Object.getOwnPropertyDescriptor(this.options.target, 'que');
      bootDescriptor = Object.getOwnPropertyDescriptor(this.options.target, 'boot');
      registrationDescriptor = Object.getOwnPropertyDescriptor(
        this.options.target,
        '_registerIntegration'
      );
      if (registrationDescriptor) return false;
      claimMutationStarted = true;
      if (
        !bootDescriptor ||
        !('value' in bootDescriptor) ||
        typeof bootDescriptor.value !== 'object' ||
        bootDescriptor.value === null
      ) {
        Object.defineProperty(this.options.target, 'boot', {
          configurable: true,
          enumerable: true,
          value: {},
          writable: true,
        });
      }
      this.ingress = prepareQueue(this.options.target);
      const bootCandidate = this.bootCandidate();
      this.registry = createIntegrationRegistry({
        // The manifest validator binds releaseId directly to the embedded build
        // stamp, so a separate comparison would duplicate the stamp in minified
        // core output without strengthening the ABI check.
        manifest: this.options.manifest,
        releaseId: EMBEDDED_RELEASE_ID,
        knownIntegrationIds: this.options.knownIntegrationIds,
        catalog:
          this.options.catalog ??
          Object.freeze(
            this.options.knownIntegrationIds.map((id) =>
              Object.freeze({
                id,
                phase: 'takeover' as const,
                trigger: null,
                config: EMBEDDED_RUNTIME_CATALOG.find((entry) => entry.id === id)?.config ?? null,
                consumes: Object.freeze([]),
                provides: Object.freeze([]),
              })
            )
          ),
        runtimeCapability: Object.freeze({
          attachAuctionContextService: (service: RuntimeAuctionContextService) => {
            if (
              this.auctionContextService ||
              typeof service !== 'object' ||
              service === null ||
              !Object.isFrozen(service) ||
              typeof service.register !== 'function'
            ) {
              return undefined;
            }
            this.auctionContextService = service;
            let released = false;
            return () => {
              if (released) return;
              released = true;
              if (this.auctionContextService === service) this.auctionContextService = undefined;
            };
          },
          boot: () => this.kernelBoot,
          document: runtimeDocument,
          enqueue: (callback: () => void) => {
            if (typeof callback !== 'function') return false;
            try {
              const queue = Object.getOwnPropertyDescriptor(this.options.target, 'que');
              const queueValue = queue && 'value' in queue ? queue.value : undefined;
              const push =
                queueValue !== undefined && queueValue !== null
                  ? Object.getOwnPropertyDescriptor(queueValue, 'push')
                  : undefined;
              if (!push || !('value' in push) || typeof push.value !== 'function') return false;
              Reflect.apply(push.value, queueValue, [callback]);
              return true;
            } catch {
              return false;
            }
          },
          generation: this.generation,
          protectFirstDisplayAttemptBatch: (terminalLatches: readonly PromiseLike<unknown>[]) =>
            this.protectFirstDisplayAttemptBatch(terminalLatches),
          registerAuctionContext: (integrationId, contributor) => {
            try {
              return this.auctionContextService?.register(integrationId, contributor);
            } catch {
              return undefined;
            }
          },
        } satisfies RuntimeCapabilityV1),
        onCapabilityStaged: (key, facade) => {
          if (key === 'direct.v1') {
            const direct = snapshotDirectCapability(facade);
            if (!direct) throw new TypeError('direct.v1 capability is malformed');
            if (this.directProvider) throw new TypeError('direct.v1 capability is already staged');
            this.directProvider = direct;
            return () => {
              if (this.directProvider === direct) this.directProvider = undefined;
            };
          }
          if (key === 'gpt_diag.v1') {
            const diagnostics = snapshotGptDiagnosticsCapability(facade);
            if (!diagnostics) throw new TypeError('gpt_diag.v1 capability is malformed');
            if (this.gptDiagnosticsProvider) {
              throw new TypeError('gpt_diag.v1 capability is already staged');
            }
            this.gptDiagnosticsProvider = diagnostics;
            return () => {
              if (this.gptDiagnosticsProvider === diagnostics) {
                this.gptDiagnosticsProvider = undefined;
              }
            };
          }
          return undefined;
        },
        ...(this.selectedScript && runtimeDocument
          ? {
              takeoverScript: this.selectedScript,
              takeoverScriptId: takeoverMode
                ? ('trustedserver-js-runtime' as const)
                : ('trustedserver-js' as const),
              document: runtimeDocument,
            }
          : {}),
        startedAtMs,
        ...(this.options.now ? { now: this.options.now } : {}),
        getBindings: (id) => this.integrationBindings(id),
        isCurrentOwner: () => this.ownsRegistrationHandshake(),
        onTakeoverRegistrationsReady: () => {
          if (this.options.autoInstall && !this.installPromise) void this.install();
        },
      });
      this.fallbackBoot = buildFallbackBoot(EMBEDDED_RELEASE_ID, this.trustedRuntimeSrc);
      if (!this.fallbackBoot) throw new Error('Trusted runtime artifact source is unavailable');
      if (this.registry.manifest) {
        this.kernelBoot = buildKernelBoot(
          EMBEDDED_RELEASE_ID,
          this.options.manifest as TsjsBootV1['manifest'],
          bootCandidate
        );
      }
      Object.defineProperty(this.options.target, '_registerIntegration', {
        configurable: true,
        enumerable: false,
        value: this.registrationHandshake,
        writable: false,
      });
      if (!this.ownsRegistrationHandshake()) {
        throw new Error('Runtime owner handshake changed during claim');
      }
      this.runtimeState = 'installing';
      if (
        this.options.autoInstall &&
        !this.registry.manifest?.integrations.some((entry) => entry.phase === 'takeover')
      ) {
        void this.install();
      }
      return true;
    } catch {
      try {
        this.registry?.dispose();
      } catch {
        // Disposal is best-effort while unwinding a failed claim.
      }
      if (claimMutationStarted) {
        restoreOwnProperty(this.options.target, 'que', queueDescriptor);
        restoreOwnProperty(this.options.target, 'boot', bootDescriptor);
        restoreOwnProperty(this.options.target, '_registerIntegration', registrationDescriptor);
      }
      this.runtimeState = 'unclaimed';
      this.registry = undefined;
      this.ingress = undefined;
      this.kernelBoot = undefined;
      this.fallbackBoot = undefined;
      this.selectedScript = undefined;
      this.trustedRuntimeSrc = undefined;
      return false;
    }
  }

  public registerIntegration(registration: unknown): boolean {
    if (this.runtimeState === 'installing') {
      if (!this.ownsRegistrationHandshake()) return false;
      return this.registry?.register(registration) ?? false;
    }
    if (this.runtimeState === 'kernel' && this.ownsTerminalRegistrationHandshake()) {
      return this.phaseLoader?.register(registration) ?? false;
    }
    return false;
  }

  public install(): Promise<IntegrationInstallResult> {
    if (this.installPromise) return this.installPromise;
    if (!this.registry || !this.ingress || this.runtimeState !== 'installing') {
      return Promise.resolve(Object.freeze({ state: 'fallback', reason: 'bundle_partial' }));
    }
    const notifyInstallComplete = (result: IntegrationInstallResult): void => {
      try {
        this.options.onInstallComplete?.(result);
      } catch {
        // Completion observation cannot change the already committed terminal state.
      }
    };
    if (!this.kernelBoot) {
      this.registry.dispose();
      const result = Object.freeze({ state: 'fallback' as const, reason: 'abi_mismatch' as const });
      this.runtimeState = 'failed';
      if (!this.options.coordinateTakeover) this.commitFallback(result.reason);
      notifyInstallComplete(result);
      this.installPromise = Promise.resolve(result);
      return this.installPromise;
    }
    let published: PublishedQueue | undefined;
    const callbacks: IntegrationInstallCallbacks = {
      prepareCore: (context) => {
        if (!this.ownsInstallingPreparation(context)) {
          throw new Error('Runtime owner generation changed');
        }
        const ownerContext: RuntimeOwnerPreparationContext = Object.freeze({
          boot: this.kernelBoot as Readonly<object>,
          generation: this.generation,
          onDispose: context.onDispose,
          signal: context.signal,
        });
        this.invokeSynchronousActivation(this.options.prepareOwner, ownerContext);
        if (!this.ownsInstallingPreparation(context)) {
          throw new Error('Runtime owner generation changed');
        }
      },
      activateCore: (context) => {
        if (!this.ownsInstallingActivation(context)) {
          throw new Error('Runtime owner generation changed');
        }
        const ownerContext: RuntimeOwnerActivationContext = Object.freeze({
          boot: this.kernelBoot as Readonly<object>,
          generation: this.generation,
          onDispose: context.onDispose,
          signal: context.signal,
        });
        this.invokeSynchronousActivation(this.options.activateOwner, ownerContext);
        if (!this.ownsInstallingActivation(context)) {
          throw new Error('Runtime owner generation changed');
        }
        this.invokeSynchronousActivation(this.options.activateCore, context);
        if (!this.ownsInstallingActivation(context)) {
          throw new Error('Runtime owner generation changed');
        }
      },
      publish: () => {
        if (!this.ownsRegistrationHandshake()) {
          throw new Error('Runtime owner generation changed');
        }
        published = publishQueue(
          this.options.target,
          this.ingress as unknown[],
          this.kernelFields()
        );
        this.startDeferredPhase();
      },
      drainPreload: () => published?.drain(),
      ...(this.options.coordinateTakeover
        ? { coordinateTakeover: this.options.coordinateTakeover }
        : {}),
    };
    let resolveInstall: ((result: IntegrationInstallResult) => void) | undefined;
    this.installPromise = new Promise<IntegrationInstallResult>((resolve) => {
      resolveInstall = resolve;
    });
    const settle = (result: IntegrationInstallResult): void => {
      if (result.state === 'kernel') {
        this.runtimeState = 'kernel';
      } else {
        this.runtimeState = 'failed';
        if (!this.options.coordinateTakeover) this.commitFallback(result.reason);
      }
      notifyInstallComplete(result);
      resolveInstall?.(result);
    };
    if (!this.options.coordinateTakeover) {
      settle(this.registry.installSync(callbacks));
      return this.installPromise;
    }
    void this.registry.install(callbacks).then(settle, () => {
      settle(Object.freeze({ state: 'fallback', reason: 'bundle_partial' }));
    });
    return this.installPromise;
  }

  public dispose(): void {
    this.phaseLoader?.dispose();
    this.phaseGate?.dispose();
    this.registry?.dispose();
  }

  public protectFirstDisplayAttemptBatch(
    terminalLatches: readonly PromiseLike<unknown>[]
  ): boolean {
    return this.phaseGate?.protectAttemptBatch(terminalLatches) ?? false;
  }

  private kernelFields(): Readonly<Record<string, unknown>> {
    const direct = this.directProvider;
    const requiresDirect = this.options.catalog?.some(
      ({ id, provides }) => id === 'render_runtime' && provides.includes('direct.v1')
    );
    if (requiresDirect && !direct) {
      throw new Error('Mandatory direct.v1 provider is unavailable');
    }
    const baseDiagnostics =
      direct?.diagnostics ??
      this.options.getDiagnosticsForPublish?.() ??
      this.options.kernel.diagnostics;
    const addAdUnits = direct?.addAdUnits ?? this.options.kernel.addAdUnits;
    const requestAds = direct?.requestAds ?? this.options.kernel.requestAds;
    if (
      (typeof baseDiagnostics !== 'object' && typeof baseDiagnostics !== 'function') ||
      baseDiagnostics === null ||
      !Object.isFrozen(baseDiagnostics)
    ) {
      throw new Error('Published diagnostics namespace must be frozen');
    }
    let diagnostics = baseDiagnostics;
    const gptDiagnostics = this.gptDiagnosticsProvider?.api;
    if (gptDiagnostics) {
      const descriptors = Object.getOwnPropertyDescriptors(baseDiagnostics);
      if (
        Object.getOwnPropertySymbols(baseDiagnostics).length !== 0 ||
        Object.prototype.hasOwnProperty.call(descriptors, 'gpt') ||
        !Object.values(descriptors).every(
          (descriptor) => descriptor.enumerable && 'value' in descriptor
        )
      ) {
        throw new Error('Published diagnostics namespace is malformed');
      }
      diagnostics = Object.freeze(
        Object.defineProperties(
          {},
          {
            ...descriptors,
            gpt: { enumerable: true, value: gptDiagnostics },
          }
        )
      );
    }
    const fields: Record<string, unknown> = {};
    Object.defineProperties(fields, {
      version: { enumerable: true, value: '1.0.0' },
      releaseId: { enumerable: true, value: EMBEDDED_RELEASE_ID },
      boot: {
        enumerable: true,
        value: this.kernelBoot,
      },
      log: { enumerable: true, value: publicLog },
      _registerIntegration: { enumerable: true, value: this.registrationHandshake },
      addAdUnits: { enumerable: true, value: addAdUnits },
      requestAds: { enumerable: true, value: requestAds },
      diagnostics: { enumerable: true, value: diagnostics },
      _internal: {
        enumerable: false,
        value: Object.freeze({ state: 'kernel', releaseId: EMBEDDED_RELEASE_ID }),
      },
    });
    return Object.freeze(fields);
  }

  private commitFallback(reason: BootFailureReason): void {
    if (!this.ownsRegistrationHandshake() || !this.ingress || !this.trustedRuntimeSrc) return;
    const fields = createFallbackFields({
      releaseId: EMBEDDED_RELEASE_ID,
      reason,
      trustedRuntimeSrc: this.trustedRuntimeSrc,
    });
    if (!fields) return;
    const published = publishQueue(this.options.target, this.ingress, fields);
    this.runtimeState = 'fallback';
    published.drain();
  }

  private ownsRegistrationHandshake(): boolean {
    try {
      const descriptor = Object.getOwnPropertyDescriptor(
        this.options.target,
        '_registerIntegration'
      );
      return (
        descriptor !== undefined &&
        'value' in descriptor &&
        descriptor.value === this.registrationHandshake &&
        descriptor.configurable === true &&
        descriptor.enumerable === false &&
        descriptor.writable === false
      );
    } catch {
      return false;
    }
  }

  private ownsTerminalRegistrationHandshake(): boolean {
    try {
      const descriptor = Object.getOwnPropertyDescriptor(
        this.options.target,
        '_registerIntegration'
      );
      return (
        descriptor !== undefined &&
        'value' in descriptor &&
        descriptor.value === this.registrationHandshake &&
        descriptor.configurable === false &&
        descriptor.enumerable === true &&
        descriptor.writable === false
      );
    } catch {
      return false;
    }
  }

  private runtimeDocument(): Document | undefined {
    if (this.options.document) return this.options.document;
    return typeof document === 'undefined' ? undefined : document;
  }

  private startDeferredPhase(): void {
    const runtimeDocument = this.runtimeDocument();
    const manifest = this.registry?.manifest;
    if (!runtimeDocument || !manifest) return;
    const gate = createProtectedFirstDisplayGate({
      document: runtimeDocument,
      ...(this.options.coordinateTakeover ? { paintAlreadyRecorded: true } : {}),
      ...(this.options.phaseScheduler ? { scheduler: this.options.phaseScheduler } : {}),
      markPaint: () => {
        try {
          runtimeDocument.defaultView?.performance.mark('tsjs:first-display-paint');
        } catch {
          // A publisher performance shim cannot block the protected phase gate.
        }
      },
    });
    this.phaseGate = gate;
    const runtimeScript = this.selectedScript;
    if (runtimeScript) {
      this.phaseLoader = createDeferredPhaseLoader({
        runtimeScript,
        document: runtimeDocument,
        gate: gate.ready,
        manifest,
        prepare: (registration, owner) =>
          this.registry?.prepareDeferred(registration, owner) ?? {
            activate: () => undefined,
          },
        releaseId: EMBEDDED_RELEASE_ID,
        ...(this.options.phaseScheduler
          ? {
              scheduler: {
                clearTimeout: this.options.phaseScheduler.clearTimeout,
                setTimeout: this.options.phaseScheduler.setTimeout,
              },
            }
          : {}),
      });
    }
    gate.commit();
  }

  private ownsInstallingActivation(context: CoreActivationContext): boolean {
    return (
      this.runtimeState === 'installing' &&
      !context.signal.aborted &&
      this.registry?.state === 'activating' &&
      this.ownsRegistrationHandshake()
    );
  }

  private ownsInstallingPreparation(context: CorePreparationContext): boolean {
    return (
      this.runtimeState === 'installing' &&
      !context.signal.aborted &&
      this.registry?.state === 'preparing' &&
      this.ownsRegistrationHandshake()
    );
  }

  private bootCandidate(): unknown {
    return this.options.boot;
  }

  private integrationBindings(id: string): IntegrationBindings {
    const fallback = Object.freeze({ config: undefined, interfaces: Object.freeze({}) });
    const bindings = this.options.getBindings?.(id) ?? fallback;
    const source = this.options.catalog?.find((entry) => entry.id === id)?.config;
    if (source === undefined) {
      if (id !== 'creative' && id !== 'gpt_diagnostics') return bindings;
    } else if (source === null) {
      return Object.freeze({ config: undefined, interfaces: bindings.interfaces });
    }
    const boot = this.kernelBoot;
    if (!boot) return bindings;
    try {
      if (source === 'creative' || (source === undefined && id === 'creative')) {
        const creative = Object.getOwnPropertyDescriptor(boot, 'creative');
        return creative && 'value' in creative
          ? Object.freeze({ config: creative.value, interfaces: bindings.interfaces })
          : bindings;
      }
      if (source && source !== 'diagnostics') {
        const integrations = Object.getOwnPropertyDescriptor(boot, 'integrations');
        const config =
          integrations && 'value' in integrations
            ? retainedProductConfig(boot, source as IntegrationConfigIdV1)
            : undefined;
        return Object.freeze({ config, interfaces: bindings.interfaces });
      }
      const diagnostics = Object.getOwnPropertyDescriptor(boot, 'diagnostics');
      const gpt =
        diagnostics && 'value' in diagnostics
          ? Object.getOwnPropertyDescriptor(diagnostics.value, 'gpt')
          : undefined;
      return gpt && 'value' in gpt
        ? Object.freeze({ config: gpt.value, interfaces: bindings.interfaces })
        : bindings;
    } catch {
      return bindings;
    }
  }

  private invokeSynchronousActivation<Context>(
    activation: ((context: Context) => void) | undefined,
    context: Context
  ): void {
    if (!activation) return;
    const returned = activation(context) as unknown;
    if (
      (typeof returned === 'object' || typeof returned === 'function') &&
      returned !== null &&
      typeof (returned as { then?: unknown }).then === 'function'
    ) {
      try {
        void Promise.resolve(returned).catch(() => undefined);
      } catch {
        // Rejection observation cannot make an asynchronous activation valid.
      }
      throw new TypeError('Runtime activation must be synchronous');
    }
  }
}

/** Create one dormant runtime owner; `start` performs the test-only claim. */
export function createRuntime(options: RuntimeOptions): Runtime {
  const owner = new RuntimeOwner(options);
  return Object.freeze({
    get state() {
      return owner.state;
    },
    generation: owner.generation,
    start: () => owner.start(),
    registerIntegration: (registration: unknown) => owner.registerIntegration(registration),
    install: () => owner.install(),
    protectFirstDisplayAttemptBatch: (terminalLatches: readonly PromiseLike<unknown>[]) =>
      owner.protectFirstDisplayAttemptBatch(terminalLatches),
    dispose: () => owner.dispose(),
  });
}
