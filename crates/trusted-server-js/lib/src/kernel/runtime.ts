import { prepareQueue, publishQueue, type PublishedQueue } from '../core/queue';
import { EMBEDDED_RELEASE_ID } from '../core/release';
import { FALLBACK_REMOVED_FIELDS, LEGACY_TSJS_FIELDS } from '../core/surface';

import { buildFallbackBoot, buildKernelBoot, createFallbackFields, publicLog } from './fallback';
import {
  createIntegrationRegistry,
  type BootFailureReason,
  type CoreActivationContext,
  type IntegrationBindings,
  type IntegrationInstallResult,
  type IntegrationRegistry,
} from './integration_registry';

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
  '_internal',
  'que',
  ...LEGACY_TSJS_FIELDS,
]);

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

export interface RuntimeOptions {
  readonly target: RuntimeTarget;
  /** Server assertion only; every decision and published value is bound to the build stamp. */
  readonly releaseId: string;
  readonly manifest: unknown;
  readonly knownIntegrationIds: readonly string[];
  readonly boot?: unknown;
  readonly now?: () => number;
  readonly getBindings?: (id: string) => IntegrationBindings;
  readonly activateOwner?: (context: RuntimeOwnerActivationContext) => void;
  readonly activateCore?: (context: CoreActivationContext) => void;
  readonly kernel: RuntimeKernel;
}

export interface Runtime {
  readonly state: RuntimeState;
  readonly generation: object;
  readonly start: () => boolean;
  readonly registerIntegration: (registration: unknown) => boolean;
  readonly install: () => Promise<IntegrationInstallResult>;
  readonly dispose: () => void;
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
  private kernelBoot: Readonly<object> | undefined;
  private fallbackBoot: Readonly<object> | undefined;

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
      this.fallbackBoot = buildFallbackBoot(EMBEDDED_RELEASE_ID, bootCandidate);
      this.registry = createIntegrationRegistry({
        manifest:
          this.options.releaseId === EMBEDDED_RELEASE_ID ? this.options.manifest : undefined,
        releaseId: EMBEDDED_RELEASE_ID,
        knownIntegrationIds: this.options.knownIntegrationIds,
        startedAtMs,
        ...(this.options.now ? { now: this.options.now } : {}),
        ...(this.options.getBindings ? { getBindings: this.options.getBindings } : {}),
        isCurrentOwner: () => this.ownsRegistrationHandshake(),
      });
      if (this.registry.manifest) {
        this.kernelBoot = buildKernelBoot(
          EMBEDDED_RELEASE_ID,
          this.registry.manifest,
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
      return false;
    }
  }

  public registerIntegration(registration: unknown): boolean {
    if (this.runtimeState !== 'installing' || !this.ownsRegistrationHandshake()) return false;
    return this.registry?.register(registration) ?? false;
  }

  public install(): Promise<IntegrationInstallResult> {
    if (this.installPromise) return this.installPromise;
    if (!this.registry || !this.ingress || this.runtimeState !== 'installing') {
      return Promise.resolve(Object.freeze({ state: 'fallback', reason: 'bundle_partial' }));
    }
    if (!this.kernelBoot) {
      this.registry.dispose();
      const result = Object.freeze({ state: 'fallback' as const, reason: 'abi_mismatch' as const });
      this.runtimeState = 'failed';
      this.commitFallback(result.reason);
      this.installPromise = Promise.resolve(result);
      return this.installPromise;
    }
    let published: PublishedQueue | undefined;
    this.installPromise = this.registry
      .install({
        activateCore: (context) => {
          if (!this.ownsRegistrationHandshake()) {
            throw new Error('Runtime owner generation changed');
          }
          const ownerContext: RuntimeOwnerActivationContext = Object.freeze({
            boot: this.kernelBoot as Readonly<object>,
            generation: this.generation,
            onDispose: context.onDispose,
            signal: context.signal,
          });
          this.invokeSynchronousActivation(this.options.activateOwner, ownerContext);
          if (!this.ownsRegistrationHandshake()) {
            throw new Error('Runtime owner generation changed');
          }
          this.invokeSynchronousActivation(this.options.activateCore, context);
          if (!this.ownsRegistrationHandshake()) {
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
            this.kernelFields(),
            LEGACY_TSJS_FIELDS
          );
        },
        drainPreload: () => published?.drain(),
      })
      .then((result) => {
        if (result.state === 'kernel') {
          this.runtimeState = 'kernel';
          return result;
        }
        this.runtimeState = 'failed';
        this.commitFallback(result.reason);
        return result;
      });
    return this.installPromise;
  }

  public dispose(): void {
    this.registry?.dispose();
  }

  private kernelFields(): Readonly<Record<string, unknown>> {
    const fields: Record<string, unknown> = {};
    Object.defineProperties(fields, {
      version: { enumerable: true, value: '1.0.0' },
      releaseId: { enumerable: true, value: EMBEDDED_RELEASE_ID },
      boot: {
        enumerable: true,
        value: this.kernelBoot,
      },
      log: { enumerable: true, value: publicLog },
      _registerIntegration: { enumerable: true, value: () => false },
      addAdUnits: { enumerable: true, value: this.options.kernel.addAdUnits },
      requestAds: { enumerable: true, value: this.options.kernel.requestAds },
      diagnostics: { enumerable: true, value: this.options.kernel.diagnostics },
      _internal: {
        enumerable: false,
        value: Object.freeze({ state: 'kernel', releaseId: EMBEDDED_RELEASE_ID }),
      },
    });
    return Object.freeze(fields);
  }

  private commitFallback(reason: BootFailureReason): void {
    if (!this.ownsRegistrationHandshake() || !this.ingress) return;
    const published = publishQueue(
      this.options.target,
      this.ingress,
      createFallbackFields({
        releaseId: EMBEDDED_RELEASE_ID,
        reason,
        boot: this.fallbackBoot,
      }),
      FALLBACK_REMOVED_FIELDS
    );
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

  private bootCandidate(): unknown {
    if (this.options.boot !== undefined) return this.options.boot;
    const descriptor = Object.getOwnPropertyDescriptor(this.options.target, 'boot');
    return descriptor && 'value' in descriptor ? descriptor.value : undefined;
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
    dispose: () => owner.dispose(),
  });
}
