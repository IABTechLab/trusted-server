import type { BootManifestV1 } from '../core/types';
import {
  snapshotOutlinedFirstDisplayHandoffV1,
  type FirstDisplayHandoffV1,
} from '../shared/first_display_contracts';
import { snapshotPersistentFirstDisplayAdoptionV1 } from '../shared/takeover';

import type { ReleaseConfigSourceV1 } from './release_catalog';
import { EMBEDDED_MAX_MANIFEST_MODULES } from './contracts/release_capacity';
import { DisposableStack, type DisposeCallback } from './disposable';
import { trustedArtifactOrigin, type BootFailureReason } from './fallback';

const INTEGRATION_ID = /^[a-z0-9][a-z0-9_-]{0,63}$/;
const RELEASE_ID = /^[0-9a-f]{64}$/;
const MAX_INTEGRATIONS = EMBEDDED_MAX_MANIFEST_MODULES;
const MAX_KNOWN_INTEGRATIONS = 256;
const BOOT_DEADLINE_MS = 10_000;
const EMPTY_BINDING = Object.freeze({});
const ABORTED = Symbol('aborted');

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
  /** Closure-private first-display state, present only during an atomic takeover activation. */
  readonly adoption?: unknown;
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
  /** Exact catalog-declared capability facades owned by this provider. */
  readonly interfaces?: Readonly<Record<string, unknown>>;
}

export interface TakeoverIntegrationRegistration {
  readonly abi: 1;
  readonly id: string;
  readonly phase: 'takeover';
  readonly releaseId: string;
  readonly prepareSync: (context: IntegrationPrepareContext) => PreparedIntegration;
  readonly prepare: (
    context: IntegrationPrepareContext
  ) => PreparedIntegration | PromiseLike<PreparedIntegration>;
}

export interface DeferredIntegrationRegistration {
  readonly abi: 1;
  readonly id: string;
  readonly phase: 'deferred';
  readonly releaseId: string;
  readonly prepare: (
    context: IntegrationPrepareContext
  ) => PreparedIntegration | PromiseLike<PreparedIntegration>;
}

export type IntegrationRegistration =
  TakeoverIntegrationRegistration | DeferredIntegrationRegistration;

export interface IntegrationRuntimeFailure {
  readonly id: string;
  readonly phase: 'after_commit';
}

/** One-use, non-yielding barrier exposed only after every inert preparation succeeds. */
export interface PreparedKernelTakeover {
  /** Validate and freeze the complete handoff before either owner mutates state. */
  readonly validateHandoff: (
    handoff: unknown,
    outline: unknown
  ) => FirstDisplayHandoffV1 | undefined;
  readonly activate: (adoption?: unknown) => void;
  readonly commit: () => void;
  readonly rollback: () => void;
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
  /** Coordinates an old-owner handoff around the synchronous activation and publication barrier. */
  readonly coordinateTakeover?: (prepared: PreparedKernelTakeover) => void;
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
  /** Exact release-owned phase/order authority; production uses the embedded catalog. */
  readonly catalog?: readonly IntegrationCatalogEntry[];
  /** Kernel-owned root capability; never published or returned by the registry facade. */
  readonly runtimeCapability?: Readonly<Record<string, unknown>>;
  /** Captured parser-inserted takeover script; required by production runtime. */
  readonly takeoverScript?: HTMLScriptElement;
  readonly takeoverScriptId?: 'trustedserver-js' | 'trustedserver-js-runtime';
  readonly document?: Document;
  readonly startedAtMs: number;
  readonly now?: () => number;
  readonly signal?: AbortSignal;
  readonly getBindings?: (id: string) => IntegrationBindings;
  /** Monotonic bootstrap-generation guard supplied by the composition owner. */
  readonly isCurrentOwner?: () => boolean;
  readonly onDisposalError?: (error: unknown) => void;
  readonly onCapabilityStaged?: (
    key: string,
    facade: Readonly<Record<string, unknown>>
  ) => void | DisposeCallback;
  readonly onRuntimeFailure?: (failure: IntegrationRuntimeFailure) => void;
  /** Production transport hook; invoked inline after the final takeover registration. */
  readonly onTakeoverRegistrationsReady?: () => void;
}

export interface IntegrationCatalogEntry {
  readonly id: string;
  readonly phase: 'takeover' | 'deferred';
  readonly trigger: 'first_display_or_idle' | null;
  readonly config?: ReleaseConfigSourceV1;
  readonly consumes: readonly string[];
  readonly provides: readonly string[];
}

export interface IntegrationRegistry {
  readonly state: IntegrationRegistryState;
  readonly manifest: BootManifestV1 | undefined;
  readonly register: (candidate: unknown) => boolean;
  readonly prepareDeferred: (
    registration: DeferredIntegrationRegistration,
    owner: Readonly<{
      signal: AbortSignal;
      onDispose: (callback: DisposeCallback) => void;
    }>
  ) => PreparedIntegration | PromiseLike<PreparedIntegration>;
  readonly installSync: (callbacks: IntegrationInstallCallbacks) => IntegrationInstallResult;
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
    if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) return undefined;
    fields[key] = descriptor.value;
  }
  return fields;
}

/** Snapshot the exact phase-discriminated registrar value without invoking bundle code. */
export function snapshotIntegrationRegistration(
  candidate: unknown
): IntegrationRegistration | undefined {
  try {
    if (
      typeof candidate !== 'object' ||
      candidate === null ||
      Array.isArray(candidate) ||
      Object.getPrototypeOf(candidate) !== Object.prototype
    ) {
      return undefined;
    }
    const phaseDescriptor = Object.getOwnPropertyDescriptor(candidate, 'phase');
    if (!phaseDescriptor?.enumerable || !('value' in phaseDescriptor)) return undefined;
    const phase = phaseDescriptor.value;
    const fields = readExactDataFields(
      candidate,
      phase === 'takeover'
        ? ['abi', 'id', 'phase', 'releaseId', 'prepareSync', 'prepare']
        : phase === 'deferred'
          ? ['abi', 'id', 'phase', 'releaseId', 'prepare']
          : []
    );
    if (!fields) return undefined;
    const { abi, id, releaseId, prepare } = fields;
    if (
      abi !== 1 ||
      typeof id !== 'string' ||
      !INTEGRATION_ID.test(id) ||
      typeof releaseId !== 'string' ||
      !RELEASE_ID.test(releaseId) ||
      typeof prepare !== 'function' ||
      (phase === 'takeover' && typeof fields.prepareSync !== 'function')
    ) {
      return undefined;
    }
    return phase === 'takeover'
      ? Object.freeze({
          abi: 1,
          id,
          phase,
          releaseId,
          prepareSync: fields.prepareSync as TakeoverIntegrationRegistration['prepareSync'],
          prepare: prepare as TakeoverIntegrationRegistration['prepare'],
        })
      : Object.freeze({
          abi: 1,
          id,
          phase,
          releaseId,
          prepare: prepare as DeferredIntegrationRegistration['prepare'],
        });
  } catch {
    return undefined;
  }
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

const CAPABILITY = /^[a-z][a-z0-9_]*(?:\.[a-z][a-z0-9_]*)+$/;
const CONDITIONAL_CAPABILITY = /^([a-z][a-z0-9_]*(?:\.[a-z][a-z0-9_]*)+)\?([a-z][a-z0-9_]*)$/;

function capabilityKey(declaration: string): string | undefined {
  if (CAPABILITY.test(declaration)) return declaration;
  return CONDITIONAL_CAPABILITY.exec(declaration)?.[1];
}

function validateCapabilityList(
  candidate: unknown,
  allowConditional: boolean
): readonly string[] | undefined {
  const declarations = snapshotExactArray(candidate, MAX_INTEGRATIONS);
  if (!declarations || !Object.isFrozen(candidate)) return undefined;
  const seen = new Set<string>();
  const accepted: string[] = [];
  for (const declaration of declarations) {
    if (
      typeof declaration !== 'string' ||
      (!CAPABILITY.test(declaration) &&
        !(allowConditional && CONDITIONAL_CAPABILITY.test(declaration))) ||
      seen.has(declaration)
    ) {
      return undefined;
    }
    seen.add(declaration);
    accepted.push(declaration);
  }
  return Object.freeze(accepted);
}

function validateCatalog(
  candidate: unknown,
  knownIntegrationIds: ReadonlySet<string>
): readonly IntegrationCatalogEntry[] | undefined {
  const entries = snapshotExactArray(candidate, MAX_INTEGRATIONS);
  if (!entries || entries.length !== knownIntegrationIds.size) return undefined;
  const seen = new Set<string>();
  const catalog: IntegrationCatalogEntry[] = [];
  let sawDeferred = false;
  let takeoverCount = 0;
  const providerIndex = new Map<string, number>();
  for (const entry of entries) {
    const keys = Reflect.ownKeys(entry as object);
    const hasConfig = keys.includes('config');
    const fields = readExactDataFields(entry, [
      'id',
      'phase',
      'trigger',
      ...(hasConfig ? ['config'] : []),
      'consumes',
      'provides',
    ]);
    if (!fields || typeof fields.id !== 'string' || !knownIntegrationIds.has(fields.id)) {
      return undefined;
    }
    const consumes = validateCapabilityList(fields.consumes, true);
    const provides = validateCapabilityList(fields.provides, false);
    if (!consumes || !provides) return undefined;
    if (
      hasConfig &&
      fields.config !== null &&
      ![
        'aps',
        'datadome',
        'didomi',
        'google_tag_manager',
        'gpt',
        'lockr',
        'osano',
        'permutive',
        'prebid',
        'sourcepoint',
        'testlight',
        'creative',
        'diagnostics',
      ].includes(fields.config as string)
    ) {
      return undefined;
    }
    if (seen.has(fields.id)) return undefined;
    seen.add(fields.id);
    if (fields.phase === 'takeover') {
      if (fields.trigger !== null || sawDeferred || ++takeoverCount > 14) return undefined;
    } else if (fields.phase === 'deferred') {
      sawDeferred = true;
      if (fields.trigger !== 'first_display_or_idle' || provides.length !== 0) return undefined;
    } else {
      return undefined;
    }
    const index = catalog.length;
    for (const capability of provides) {
      if (capability === 'runtime.v1' || providerIndex.has(capability)) return undefined;
      providerIndex.set(capability, index);
    }
    catalog.push(
      Object.freeze({
        id: fields.id,
        phase: fields.phase,
        trigger: fields.trigger,
        ...(hasConfig ? { config: fields.config as ReleaseConfigSourceV1 } : {}),
        consumes,
        provides,
      }) as IntegrationCatalogEntry
    );
  }
  for (let index = 0; index < catalog.length; index += 1) {
    const entry = catalog[index];
    if (!entry) return undefined;
    for (const declaration of entry.consumes) {
      const key = capabilityKey(declaration);
      const provider = key === 'runtime.v1' ? -1 : key ? providerIndex.get(key) : undefined;
      if (provider === undefined || provider >= index) return undefined;
    }
  }
  return Object.freeze(catalog);
}

export function validateRuntimeManifestV1(
  candidate: unknown,
  embeddedReleaseId: string,
  catalog: readonly IntegrationCatalogEntry[],
  requireRenderRuntime: boolean
): BootManifestV1 | undefined {
  try {
    if (!RELEASE_ID.test(embeddedReleaseId)) return undefined;
    const manifestFields = readExactDataFields(candidate, [
      'version',
      'releaseId',
      'firstDisplay',
      'runtimeSrc',
      'integrations',
    ]);
    if (!manifestFields) return undefined;
    if (manifestFields.version !== 1 || manifestFields.releaseId !== embeddedReleaseId) {
      return undefined;
    }
    const manifestIntegrations = snapshotExactArray(manifestFields.integrations, MAX_INTEGRATIONS);
    if (!manifestIntegrations) return undefined;

    let firstDisplay: BootManifestV1['firstDisplay'];
    if (manifestFields.firstDisplay === null) {
      firstDisplay = null;
    } else {
      const fields = readExactDataFields(manifestFields.firstDisplay, ['src', 'slices']);
      const slices = snapshotExactArray(fields?.slices, 13);
      if (
        !fields ||
        typeof fields.src !== 'string' ||
        !/^\/static\/tsjs=tsjs-first-display\.min\.js\?m=[0-9a-f]{4}&v=[0-9a-f]{64}$/.test(
          fields.src
        ) ||
        !slices ||
        slices.length === 0 ||
        slices[0] !== 'first_display' ||
        slices.some((id) => typeof id !== 'string') ||
        new Set(slices).size !== slices.length
      ) {
        return undefined;
      }
      firstDisplay = Object.freeze({
        src: fields.src,
        slices: Object.freeze([...slices] as string[]),
      });
    }

    if (
      typeof manifestFields.runtimeSrc !== 'string' ||
      !/^\/static\/tsjs=tsjs-unified\.min\.js\?v=[0-9a-f]{64}$/.test(manifestFields.runtimeSrc)
    ) {
      return undefined;
    }

    const seen = new Set<string>();
    const integrations: BootManifestV1['integrations'][number][] = [];
    let sawDeferred = false;
    let takeoverCount = 0;
    let previousCatalogIndex = -1;
    for (const entry of manifestIntegrations) {
      const phaseDescriptor = isRecord(entry) && Object.getOwnPropertyDescriptor(entry, 'phase');
      if (!phaseDescriptor || !('value' in phaseDescriptor)) return undefined;
      const phase = phaseDescriptor.value;
      const entryFields = readExactDataFields(
        entry,
        phase === 'takeover' ? ['id', 'phase'] : ['id', 'phase', 'trigger', 'src']
      );
      if (!entryFields || (phase !== 'takeover' && phase !== 'deferred')) return undefined;
      if (typeof entryFields.id !== 'string' || !INTEGRATION_ID.test(entryFields.id)) {
        return undefined;
      }
      const catalogIndex = catalog.findIndex(({ id }) => id === entryFields.id);
      const catalogEntry = catalog[catalogIndex];
      if (!catalogEntry || catalogIndex <= previousCatalogIndex || catalogEntry.phase !== phase) {
        return undefined;
      }
      previousCatalogIndex = catalogIndex;
      if (seen.has(entryFields.id) || (sawDeferred && phase === 'takeover')) return undefined;
      seen.add(entryFields.id);
      if (phase === 'takeover') {
        takeoverCount += 1;
        if (takeoverCount > 14 || catalogEntry.trigger !== null) return undefined;
        integrations.push(Object.freeze({ id: entryFields.id, phase }));
        continue;
      }
      sawDeferred = true;
      if (
        catalogEntry.trigger !== 'first_display_or_idle' ||
        entryFields.trigger !== 'first_display_or_idle' ||
        typeof entryFields.src !== 'string' ||
        !new RegExp(`^/static/tsjs=tsjs-${entryFields.id}\\.min\\.js\\?v=[0-9a-f]{64}$`).test(
          entryFields.src
        )
      ) {
        return undefined;
      }
      integrations.push(
        Object.freeze({
          id: entryFields.id,
          phase,
          trigger: 'first_display_or_idle',
          src: entryFields.src,
        })
      );
    }
    if (requireRenderRuntime && integrations[0]?.id !== 'render_runtime') return undefined;

    const selected = new Set(integrations.map(({ id }) => id));
    for (const integration of integrations) {
      const catalogEntry = catalog.find(({ id }) => id === integration.id);
      if (!catalogEntry) return undefined;
      for (const declaration of catalogEntry.consumes) {
        const conditional = CONDITIONAL_CAPABILITY.test(declaration);
        const key = capabilityKey(declaration);
        if (!key || key === 'runtime.v1') continue;
        const provider = catalog.find(({ provides }) => provides.includes(key));
        if (!provider || (!conditional && !selected.has(provider.id))) return undefined;
      }
    }

    return Object.freeze({
      version: 1,
      releaseId: embeddedReleaseId,
      firstDisplay,
      runtimeSrc: manifestFields.runtimeSrc,
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

function isCapabilityFacade(value: unknown): value is Readonly<Record<string, unknown>> {
  if (
    typeof value !== 'object' ||
    value === null ||
    Array.isArray(value) ||
    Object.getPrototypeOf(value) !== Object.prototype ||
    !Object.isFrozen(value)
  ) {
    return false;
  }
  return Reflect.ownKeys(value).every((key) => {
    if (typeof key !== 'string') return false;
    const descriptor = Object.getOwnPropertyDescriptor(value, key);
    return descriptor !== undefined && descriptor.enumerable && 'value' in descriptor;
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
  private readonly catalog: readonly IntegrationCatalogEntry[];
  private readonly takeoverScript: HTMLScriptElement | undefined;
  private readonly takeoverScriptId: 'trustedserver-js' | 'trustedserver-js-runtime';
  private readonly document: Document | undefined;
  private readonly registrations = new Map<string, TakeoverIntegrationRegistration>();
  private readonly capabilities = new Map<string, Readonly<Record<string, unknown>>>();
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
  private readonly onCapabilityStaged: NonNullable<
    IntegrationRegistryOptions['onCapabilityStaged']
  >;
  private readonly onRuntimeFailure: (failure: IntegrationRuntimeFailure) => void;
  private readonly onTakeoverRegistrationsReady: () => void;
  private deadlineTimer: ReturnType<typeof setTimeout> | undefined;
  private failureReason: BootFailureReason | undefined;
  private installPromise: Promise<IntegrationInstallResult> | undefined;
  private installResult: IntegrationInstallResult | undefined;
  private installSyncInProgress = false;
  private registrationWaiter: (() => void) | undefined;
  private registryState: IntegrationRegistryState = 'collecting';
  private nextTakeoverRegistrationIndex = 0;
  private ownedCallbackDepth = 0;
  private unwindPending = false;

  public constructor(options: IntegrationRegistryOptions) {
    this.releaseId = options.releaseId;
    this.takeoverScript = options.takeoverScript;
    this.takeoverScriptId = options.takeoverScriptId ?? 'trustedserver-js';
    this.document = options.document;
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
    this.onCapabilityStaged = options.onCapabilityStaged ?? (() => undefined);
    this.onRuntimeFailure = options.onRuntimeFailure ?? (() => undefined);
    this.onTakeoverRegistrationsReady = options.onTakeoverRegistrationsReady ?? (() => undefined);
    this.coreScope = new DisposableStack(this.onDisposalError);
    const knownIntegrationIds = validateKnownIntegrationIds(options.knownIntegrationIds);
    const catalogCandidate =
      options.catalog ??
      Object.freeze(
        options.knownIntegrationIds.map((id) =>
          Object.freeze({
            id,
            phase: 'takeover' as const,
            trigger: null,
            consumes: Object.freeze([]),
            provides: Object.freeze([]),
          })
        )
      );
    const catalog = knownIntegrationIds
      ? validateCatalog(catalogCandidate, knownIntegrationIds)
      : undefined;
    this.catalog = catalog ?? Object.freeze([]);
    this.manifestValue = catalog
      ? validateRuntimeManifestV1(
          options.manifest,
          options.releaseId,
          catalog,
          catalog.length === MAX_INTEGRATIONS
        )
      : undefined;

    const runtimeCapability = options.runtimeCapability ?? EMPTY_BINDING;
    if (catalog && isCapabilityFacade(runtimeCapability)) {
      this.capabilities.set('runtime.v1', runtimeCapability);
    } else if (catalog) {
      this.manifestValue = undefined;
    }

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
      const registration = snapshotIntegrationRegistration(candidate);
      if (!registration) {
        this.fail('abi_mismatch');
        return false;
      }
      const { id, phase, releaseId } = registration;
      const takeoverIntegrations = this.manifestValue?.integrations.filter(
        (entry) => entry.phase === 'takeover'
      );
      const nextTakeover = takeoverIntegrations?.[this.nextTakeoverRegistrationIndex];
      if (
        releaseId !== this.releaseId ||
        !this.manifestValue?.integrations.some(
          (entry) => entry.id === id && entry.phase === phase
        ) ||
        phase !== 'takeover' ||
        nextTakeover?.id !== id ||
        this.registrations.has(id) ||
        !this.ownsTakeoverScript()
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
          abi: 1,
          id,
          phase,
          releaseId,
          prepareSync: registration.prepareSync,
          prepare: registration.prepare,
        })
      );
      this.nextTakeoverRegistrationIndex += 1;
      if (this.hasEveryRequiredRegistration()) {
        this.wakeRegistrationWaiter();
        this.onTakeoverRegistrationsReady();
      }
      return true;
    } catch {
      this.fail('abi_mismatch');
      return false;
    }
  }

  private ownsTakeoverScript(): boolean {
    if (!this.takeoverScript && !this.document) return true;
    const script = this.takeoverScript;
    const document = this.document;
    const Script = document?.defaultView?.HTMLScriptElement;
    const manifest = this.manifestValue;
    if (!script || !document || !Script || !manifest) return false;
    try {
      const origin = trustedArtifactOrigin(document);
      if (!origin) return false;
      const expected = new URL(manifest.runtimeSrc, origin);
      const matches = document.querySelectorAll(`script#${this.takeoverScriptId}`);
      return (
        script instanceof Script &&
        script.id === this.takeoverScriptId &&
        script.isConnected &&
        matches.length === 1 &&
        matches[0] === script &&
        document.currentScript === script &&
        expected.origin === origin &&
        expected.hash === '' &&
        `${expected.pathname}${expected.search}` === manifest.runtimeSrc &&
        script.src === expected.href
      );
    } catch {
      return false;
    }
  }

  public install(callbacks: IntegrationInstallCallbacks): Promise<IntegrationInstallResult> {
    if (this.installResult) return Promise.resolve(this.installResult);
    if (this.installPromise) return this.installPromise;

    let resolveInstall: ((result: IntegrationInstallResult) => void) | undefined;
    this.installPromise = new Promise<IntegrationInstallResult>((resolve) => {
      resolveInstall = resolve;
    });

    const acceptedCallbacks = this.snapshotInstallCallbacks(callbacks);
    if (!acceptedCallbacks) {
      this.fail('bundle_partial');
      resolveInstall?.(this.fallbackResult());
      return this.installPromise;
    }

    void this.installTransaction(acceptedCallbacks).then(
      (result) => {
        this.installResult = result;
        resolveInstall?.(result);
      },
      () => {
        this.fail('bundle_partial');
        const result = this.fallbackResult();
        this.installResult = result;
        resolveInstall?.(result);
      }
    );
    return this.installPromise;
  }

  public installSync(callbacks: IntegrationInstallCallbacks): IntegrationInstallResult {
    if (this.installResult) return this.installResult;
    if (this.installPromise || this.installSyncInProgress) {
      this.fail('bundle_partial');
      return this.fallbackResult();
    }
    this.installSyncInProgress = true;
    try {
      const acceptedCallbacks = this.snapshotInstallCallbacks(callbacks);
      if (!acceptedCallbacks) {
        this.fail('bundle_partial');
        return (this.installResult = this.fallbackResult());
      }
      return (this.installResult = this.installSyncTransaction(acceptedCallbacks));
    } finally {
      this.installSyncInProgress = false;
      if (this.installResult) this.installPromise = Promise.resolve(this.installResult);
    }
  }

  private snapshotInstallCallbacks(
    callbacks: IntegrationInstallCallbacks
  ): IntegrationInstallCallbacks | undefined {
    try {
      return Object.freeze({
        ...(callbacks.prepareCore ? { prepareCore: callbacks.prepareCore } : {}),
        activateCore: callbacks.activateCore,
        publish: callbacks.publish,
        drainPreload: callbacks.drainPreload,
        ...(callbacks.coordinateTakeover
          ? { coordinateTakeover: callbacks.coordinateTakeover }
          : {}),
      });
    } catch {
      return undefined;
    }
  }

  public prepareDeferred(
    registration: DeferredIntegrationRegistration,
    owner: Readonly<{
      signal: AbortSignal;
      onDispose: (callback: DisposeCallback) => void;
    }>
  ): PreparedIntegration | PromiseLike<PreparedIntegration> {
    const manifestEntry = this.manifestValue?.integrations.find(
      (entry) => entry.id === registration.id && entry.phase === 'deferred'
    );
    if (
      this.registryState !== 'committed' ||
      !manifestEntry ||
      registration.phase !== 'deferred' ||
      registration.releaseId !== this.releaseId ||
      owner.signal.aborted
    ) {
      throw new TypeError('Deferred integration is not admitted by the committed runtime');
    }
    const bindings = this.resolveBindings(registration.id);
    let open = true;
    const context: IntegrationPrepareContext = Object.freeze({
      config: bindings.config,
      interfaces: bindings.interfaces,
      signal: owner.signal,
      onDispose: (callback: DisposeCallback) => {
        if (!open && !owner.signal.aborted) {
          throw new Error('Deferred preparation disposal registration is closed');
        }
        owner.onDispose(callback);
      },
    });
    let prepared: PreparedIntegration | PromiseLike<PreparedIntegration>;
    try {
      prepared = registration.prepare(context);
    } catch (error) {
      open = false;
      throw error;
    }
    if (!isThenable(prepared)) {
      open = false;
      return prepared;
    }
    return Promise.resolve(prepared).finally(() => {
      open = false;
    });
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
      this.manifestValue?.integrations
        .filter((entry) => entry.phase === 'takeover')
        .every((entry) => this.registrations.has(entry.id))
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
    const bindings = this.resolveBindings(id);
    let open = true;
    const context: IntegrationPrepareContext = Object.freeze({
      config: bindings.config,
      interfaces: bindings.interfaces,
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

  private resolveBindings(id: string): IntegrationBindings {
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

    const catalogEntry = this.catalog.find((entry) => entry.id === id);
    if (!catalogEntry) throw new TypeError('Integration catalog entry is unavailable');
    const brokerInterfaces: Record<string, unknown> = {};
    for (const declaration of catalogEntry.consumes) {
      const key = capabilityKey(declaration);
      const value = key ? this.capabilities.get(key) : undefined;
      if (!key || (!value && !CONDITIONAL_CAPABILITY.test(declaration))) {
        throw new TypeError('Required integration capability is unavailable');
      }
      if (value) brokerInterfaces[key] = value;
    }
    const interfaces =
      catalogEntry.consumes.length === 0 && catalogEntry.provides.length === 0
        ? fields.interfaces
        : Object.freeze(brokerInterfaces);

    return Object.freeze({
      config: fields.config,
      interfaces,
    });
  }

  private snapshotPreparedIntegration(
    id: string,
    candidate: unknown,
    scope: DisposableStack
  ): PreparedIntegration | undefined {
    const catalogEntry = this.catalog.find((entry) => entry.id === id);
    if (!catalogEntry) return undefined;
    const expectedFields =
      catalogEntry.provides.length === 0 &&
      isRecord(candidate) &&
      !Object.prototype.hasOwnProperty.call(candidate, 'interfaces')
        ? ['activate']
        : ['activate', 'interfaces'];
    const fields = readExactDataFields(candidate, expectedFields);
    if (!fields || typeof fields.activate !== 'function') return undefined;

    if (catalogEntry.provides.length === 0) {
      if (
        'interfaces' in fields &&
        (!isRecord(fields.interfaces) ||
          !Object.isFrozen(fields.interfaces) ||
          Reflect.ownKeys(fields.interfaces).length !== 0)
      ) {
        return undefined;
      }
      return Object.freeze({ activate: fields.activate as PreparedIntegration['activate'] });
    }

    if (!isRecord(fields.interfaces) || !Object.isFrozen(fields.interfaces)) return undefined;
    const providerInterfaces = fields.interfaces;
    const keys = Reflect.ownKeys(providerInterfaces);
    if (
      keys.length !== catalogEntry.provides.length ||
      !keys.every(
        (key) =>
          typeof key === 'string' &&
          catalogEntry.provides.includes(key) &&
          isCapabilityFacade(providerInterfaces[key])
      )
    ) {
      return undefined;
    }
    for (const key of catalogEntry.provides) {
      const facade = providerInterfaces[key];
      if (!isCapabilityFacade(facade) || this.capabilities.has(key)) return undefined;
      this.capabilities.set(key, facade);
      scope.onDispose(() => {
        if (this.capabilities.get(key) === facade) this.capabilities.delete(key);
      });
      const release = this.onCapabilityStaged(key, facade);
      if (release !== undefined) scope.onDispose(release);
    }
    return Object.freeze({
      activate: fields.activate as PreparedIntegration['activate'],
      interfaces: providerInterfaces as Readonly<Record<string, unknown>>,
    });
  }

  private installSyncTransaction(callbacks: IntegrationInstallCallbacks): IntegrationInstallResult {
    if (callbacks.coordinateTakeover) {
      this.fail('bundle_partial');
      return this.fallbackResult();
    }
    if (this.registryState === 'failed' || this.registryState === 'disposed') {
      return this.fallbackResult();
    }
    if (
      !this.manifestValue ||
      !this.ownerIsCurrent() ||
      this.deadlineExpired() ||
      !this.hasEveryRequiredRegistration()
    ) {
      this.fail('bundle_partial');
      return this.fallbackResult();
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
      if (entry.phase !== 'takeover') continue;
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
      const { context, close } = this.createPreparationContext(entry.id, scope);
      try {
        if (!this.canContinue('preparing')) return this.fallbackResult();
        this.enterOwnedCallback();
        let candidate: PreparedIntegration;
        try {
          candidate = registration.prepareSync(context);
        } finally {
          this.leaveOwnedCallback();
        }
        if (isThenable(candidate)) {
          observeThenableRejection(candidate);
          throw new TypeError('prepareSync must be synchronous');
        }
        if (!this.canContinue('preparing')) return this.fallbackResult();
        const acceptedPrepared = this.snapshotPreparedIntegration(entry.id, candidate, scope);
        if (!acceptedPrepared) {
          throw new TypeError('prepareSync must return one exact activation module');
        }
        if (!this.canContinue('preparing')) return this.fallbackResult();
        this.prepared[recordIndex] = {
          id: entry.id,
          scope,
          module: acceptedPrepared,
          afterCommit: undefined,
        };
      } catch {
        this.fail('bundle_partial');
        return this.fallbackResult();
      } finally {
        close();
      }
    }

    if (!this.canContinue('preparing')) return this.fallbackResult();
    return this.finishPreparedTransaction(callbacks);
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
      if (entry.phase !== 'takeover') continue;
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
        const acceptedPrepared = this.snapshotPreparedIntegration(entry.id, prepared, scope);
        if (!acceptedPrepared) {
          throw new TypeError('prepare must return one exact activation module');
        }
        if (!this.canContinue('preparing')) return this.fallbackResult();
        this.prepared[recordIndex] = {
          id: entry.id,
          scope,
          module: acceptedPrepared,
          afterCommit: undefined,
        };
      } catch {
        this.fail('bundle_partial');
        return this.fallbackResult();
      }

      if (!this.canContinue('preparing')) return this.fallbackResult();
    }

    if (!this.canContinue('preparing')) return this.fallbackResult();

    return this.finishPreparedTransaction(callbacks);
  }

  private finishPreparedTransaction(
    callbacks: IntegrationInstallCallbacks
  ): IntegrationInstallResult {
    let activated = false;
    let committed: IntegrationKernelResult | undefined;
    let validatedHandoff: FirstDisplayHandoffV1 | undefined;
    const prepared: PreparedKernelTakeover = Object.freeze({
      validateHandoff: (candidate: unknown, outlineCandidate: unknown) => {
        if (activated || committed || validatedHandoff) return undefined;
        const handoff = snapshotOutlinedFirstDisplayHandoffV1(candidate, outlineCandidate);
        if (!handoff) return undefined;
        validatedHandoff = handoff;
        return handoff;
      },
      activate: (adoption?: unknown): void => {
        if (callbacks.coordinateTakeover) {
          const accepted = snapshotPersistentFirstDisplayAdoptionV1(adoption);
          const handoff = validatedHandoff;
          if (
            !accepted ||
            !handoff ||
            !Object.is(accepted.handoff, handoff) ||
            accepted.identities.length !== handoff.cycles.length + handoff.artifacts.length
          ) {
            throw new Error('Prepared kernel adoption is unavailable');
          }
        }
        if (activated || committed || !this.activatePrepared(callbacks, adoption)) {
          throw new Error('Prepared kernel activation is unavailable');
        }
        activated = true;
      },
      commit: (): void => {
        if (!activated || committed) throw new Error('Prepared kernel commit is unavailable');
        const result = this.commitPrepared(callbacks);
        if (!result) throw new Error('Prepared kernel commit failed');
        committed = result;
      },
      rollback: (): void => {
        if (committed) throw new Error('Committed kernel cannot be rolled back');
        this.fail('bundle_partial');
      },
    });

    try {
      if (callbacks.coordinateTakeover) {
        this.enterOwnedCallback();
        try {
          const returned = callbacks.coordinateTakeover(prepared);
          if (isThenable(returned)) {
            observeThenableRejection(returned);
            throw new TypeError('Takeover coordination must be synchronous');
          }
        } finally {
          this.leaveOwnedCallback();
        }
        if (!committed) throw new Error('Takeover coordinator did not commit');
      } else {
        prepared.activate();
        prepared.commit();
      }
    } catch {
      this.fail('bundle_partial');
      return this.fallbackResult();
    }

    return committed ?? this.fallbackResult();
  }

  private activatePrepared(callbacks: IntegrationInstallCallbacks, adoption?: unknown): boolean {
    if (this.registryState !== 'preparing') return false;
    const acceptedAdoption =
      adoption === undefined ? undefined : snapshotPersistentFirstDisplayAdoptionV1(adoption);
    if (adoption !== undefined && !acceptedAdoption) {
      this.fail('bundle_partial');
      return false;
    }
    this.registryState = 'activating';
    if (!this.canContinue('activating')) return false;

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
      return false;
    } finally {
      coreActivationOpen = false;
      this.leaveOwnedCallback();
    }

    if (!this.canContinue('activating')) return false;
    for (const record of this.prepared) {
      if (!this.canContinue('activating')) return false;
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
        ...(acceptedAdoption === undefined ? {} : { adoption: acceptedAdoption }),
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
        return false;
      } finally {
        activationOpen = false;
        this.leaveOwnedCallback();
      }

      if (!this.canContinue('activating')) return false;
    }

    // This final monotonic check closes the timer-task delay gap. A same-thread
    // activation that never returns cannot be preempted by JavaScript.
    return this.canContinue('activating');
  }

  private commitPrepared(
    callbacks: IntegrationInstallCallbacks
  ): IntegrationKernelResult | undefined {
    if (this.registryState !== 'activating' || !this.canContinue('activating')) return undefined;
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
      return undefined;
    } finally {
      this.leaveOwnedCallback();
    }

    if (this.registryState !== 'publishing') {
      this.fail('bundle_partial');
      return undefined;
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
    prepareDeferred: (
      registration: DeferredIntegrationRegistration,
      deferredOwner: Readonly<{
        signal: AbortSignal;
        onDispose: (callback: DisposeCallback) => void;
      }>
    ) => owner.prepareDeferred(registration, deferredOwner),
    installSync: (callbacks: IntegrationInstallCallbacks) => owner.installSync(callbacks),
    install: (callbacks: IntegrationInstallCallbacks) => owner.install(callbacks),
    dispose: () => owner.dispose(),
  });
}
