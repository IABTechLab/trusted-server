import { DisposableStack, type DisposeCallback, type DisposalErrorHandler } from './disposable';
import type { IdentityGenerationResult, NavigationIdentityIssuer } from './identity';

const objectFreezeIntrinsic = Object.freeze;

function freezeValue<Value extends object>(value: Value): Readonly<Value> {
  return Reflect.apply(objectFreezeIntrinsic, Object, [value]) as Readonly<Value>;
}

/** Immutable price authority transferred from winner admission into one attempt. */
export interface WinnerContext {
  readonly selectedCpm: number;
}

/** Reversible admission of one exact winner context into an attempt. */
export interface WinnerContextAdmission {
  readonly commit: () => boolean;
  readonly rollback: () => boolean;
}

/** Factory that obtains one fresh eight-byte identity prefix per navigation. */
export type NavigationIdentityIssuerFactory =
  () => IdentityGenerationResult<NavigationIdentityIssuer>;

/** Immutable interfaces injected by the composition root for the runtime lifetime. */
export type RuntimeInterfaces = Readonly<Record<string, unknown>>;

/** Options used to construct one document-lifetime runtime session. */
export interface RuntimeSessionOptions {
  readonly createIdentityIssuer: NavigationIdentityIssuerFactory;
  readonly interfaces?: RuntimeInterfaces;
  readonly onDisposalError?: DisposalErrorHandler;
}

/** Result of creating the initial navigation or replacing the current navigation. */
export type NavigationSessionResult =
  | Readonly<{ ok: true; value: NavigationSession }>
  | Readonly<{
      ok: false;
      reason:
        | 'identity_generation_failed'
        | 'invalid_projection'
        | 'navigation_already_started'
        | 'navigation_transition_in_progress'
        | 'runtime_disposed';
    }>;

/** Result of creating one render attempt. */
export type RenderAttemptResult =
  | Readonly<{ ok: true; value: RenderAttemptScope }>
  | Readonly<{
      ok: false;
      reason: 'identity_generation_failed' | 'attempt_exists' | 'stale_owner';
    }>;

/** Frozen runtime inventory intended only for ownership tests. */
export interface RuntimeInventorySnapshot {
  readonly activeDisposers: number;
  readonly currentNavigationGeneration: object | undefined;
  readonly disposed: boolean;
  readonly disposedByKind: Readonly<Record<string, number>>;
  readonly disposedNavigations: number;
  readonly navigationCount: number;
}

/** Frozen navigation inventory intended only for ownership tests. */
export interface NavigationInventorySnapshot {
  readonly activeDisposers: number;
  readonly aliases: number;
  readonly attempts: number;
  readonly batches: number;
  readonly disposed: boolean;
  readonly disposedByKind: Readonly<Record<string, number>>;
  readonly hasAuctionProjection: boolean;
  readonly intents: number;
  readonly retainedAttemptScopes: number;
  readonly retainedBatchScopes: number;
  readonly targetingOwners: number;
}

/** A document-lifetime owner that atomically replaces route-local sessions. */
export interface RuntimeSession {
  readonly generation: object;
  readonly interfaces: RuntimeInterfaces;
  readonly disposed: boolean;
  readonly currentNavigation: NavigationSession | undefined;
  readonly startInitialNavigation: (projection?: Readonly<object>) => NavigationSessionResult;
  readonly replaceNavigation: () => NavigationSessionResult;
  readonly isCurrent: () => boolean;
  readonly onDispose: (kind: string, callback: DisposeCallback) => void;
  readonly dispose: () => void;
  readonly snapshotInventoryForTest: () => RuntimeInventorySnapshot;
}

/** One route-local owner for aliases, intent, targeting, batches, attempts, and projection. */
export interface NavigationSession {
  readonly generation: object;
  readonly interfaces: RuntimeInterfaces;
  readonly disposed: boolean;
  readonly currentAuctionProjection: Readonly<object> | undefined;
  readonly signal: AbortSignal;
  readonly capture: <Arguments extends readonly unknown[]>(
    callback: (...arguments_: Arguments) => unknown
  ) => (...arguments_: Arguments) => boolean;
  readonly claimAlias: (alias: string) => boolean;
  readonly claimIntent: (slot: string) => boolean;
  readonly claimTargeting: (slot: string) => boolean;
  readonly createAuctionBatch: (key: string) => AuctionBatchScope | undefined;
  readonly installAuctionProjection: (projection: Readonly<object>) => boolean;
  readonly isCurrent: () => boolean;
  readonly onDispose: (kind: string, callback: DisposeCallback) => void;
  readonly dispose: () => void;
  readonly snapshotInventoryForTest: () => NavigationInventorySnapshot;
}

/** Navigation-owned scope for one shared auction request and its child attempts. */
export interface AuctionBatchScope {
  readonly generation: object;
  readonly interfaces: RuntimeInterfaces;
  readonly disposed: boolean;
  readonly signal: AbortSignal;
  readonly createRenderAttempt: (slot: string) => RenderAttemptResult;
  readonly isCurrent: () => boolean;
  readonly onDispose: (kind: string, callback: DisposeCallback) => void;
  readonly dispose: () => void;
}

/** Attempt-owned scope for timers, listeners, ports, and one terminal lifecycle. */
export interface RenderAttemptScope {
  readonly generation: object;
  readonly interfaces: RuntimeInterfaces;
  readonly id: string;
  readonly slot: string;
  readonly winnerContext: WinnerContext | undefined;
  readonly disposed: boolean;
  readonly signal: AbortSignal;
  readonly capture: <Arguments extends readonly unknown[]>(
    callback: (...arguments_: Arguments) => unknown
  ) => (...arguments_: Arguments) => boolean;
  readonly isCurrent: () => boolean;
  readonly prepareWinnerContext: (context: WinnerContext) => WinnerContextAdmission | undefined;
  readonly onDispose: (kind: string, callback: DisposeCallback) => void;
  readonly dispose: () => void;
}

const EMPTY_INTERFACES = Object.freeze({});

function frozenRecord(source: ReadonlyMap<string, number>): Readonly<Record<string, number>> {
  const output: Record<string, number> = {};
  for (const [key, value] of source) output[key] = value;
  return Object.freeze(output);
}

function recursivelyFrozen(value: unknown, visited = new Set<object>()): boolean {
  if ((typeof value !== 'object' && typeof value !== 'function') || value === null) return true;
  if (visited.has(value)) return true;
  visited.add(value);
  try {
    if (!Object.isFrozen(value)) return false;
    for (const key of Reflect.ownKeys(value)) {
      const descriptor = Object.getOwnPropertyDescriptor(value, key);
      if (
        !descriptor ||
        !('value' in descriptor) ||
        !recursivelyFrozen(descriptor.value, visited)
      ) {
        return false;
      }
    }
    return true;
  } catch {
    return false;
  }
}

class OwnerScope {
  public readonly generation = Object.freeze({});
  private readonly disposables: DisposableStack;
  private readonly activeByKind = new Map<string, number>();
  private readonly disposedByKind = new Map<string, number>();

  public constructor(onDisposalError?: DisposalErrorHandler) {
    this.disposables = new DisposableStack(onDisposalError);
  }

  public get disposed(): boolean {
    return this.disposables.disposed;
  }

  public get signal(): AbortSignal {
    return this.disposables.signal;
  }

  public onDispose(kind: string, callback: DisposeCallback): void {
    if (typeof kind !== 'string' || kind.length === 0)
      throw new TypeError('Disposer kind required');
    if (typeof callback !== 'function') throw new TypeError('A disposer must be a function');
    this.activeByKind.set(kind, (this.activeByKind.get(kind) ?? 0) + 1);
    this.disposables.onDispose(() => {
      const remaining = (this.activeByKind.get(kind) ?? 1) - 1;
      if (remaining === 0) this.activeByKind.delete(kind);
      else this.activeByKind.set(kind, remaining);
      this.disposedByKind.set(kind, (this.disposedByKind.get(kind) ?? 0) + 1);
      callback();
    });
  }

  public dispose(): void {
    this.disposables.dispose();
  }

  public activeDisposerCount(): number {
    let total = 0;
    for (const count of this.activeByKind.values()) total += count;
    return total;
  }

  public disposedInventory(): Readonly<Record<string, number>> {
    return frozenRecord(this.disposedByKind);
  }
}

class RenderAttemptOwner implements RenderAttemptScope {
  private readonly scope: OwnerScope;
  private acceptedWinnerContext: WinnerContext | undefined;
  private pendingWinnerAdmission: object | undefined;

  public constructor(
    public readonly id: string,
    public readonly slot: string,
    public readonly interfaces: RuntimeInterfaces,
    private readonly ownerIsCurrent: () => boolean,
    onDisposalError?: DisposalErrorHandler
  ) {
    this.scope = new OwnerScope(onDisposalError);
  }

  public get generation(): object {
    return this.scope.generation;
  }

  public get disposed(): boolean {
    return this.scope.disposed;
  }

  public get signal(): AbortSignal {
    return this.scope.signal;
  }

  public get winnerContext(): WinnerContext | undefined {
    return this.acceptedWinnerContext;
  }

  public prepareWinnerContext(context: WinnerContext): WinnerContextAdmission | undefined {
    if (!this.isCurrent() || this.pendingWinnerAdmission !== undefined) return undefined;
    try {
      const descriptor = Object.getOwnPropertyDescriptor(context, 'selectedCpm');
      if (
        !Object.isFrozen(context) ||
        Object.getPrototypeOf(context) !== Object.prototype ||
        Object.getOwnPropertyNames(context).length !== 1 ||
        Object.getOwnPropertySymbols(context).length !== 0 ||
        !descriptor ||
        !descriptor.enumerable ||
        !('value' in descriptor) ||
        typeof descriptor.value !== 'number' ||
        !Number.isFinite(descriptor.value) ||
        descriptor.value < 0
      ) {
        return undefined;
      }
      const previous = this.acceptedWinnerContext;
      if (previous !== undefined && previous !== context) return undefined;
      const token = freezeValue({});
      this.pendingWinnerAdmission = token;
      let committed = false;
      return freezeValue({
        commit: (): boolean => {
          if (committed) return this.acceptedWinnerContext === context;
          if (
            this.pendingWinnerAdmission !== token ||
            !this.isCurrent() ||
            this.acceptedWinnerContext !== previous
          ) {
            return false;
          }
          this.acceptedWinnerContext = context;
          this.pendingWinnerAdmission = undefined;
          committed = true;
          return true;
        },
        rollback: (): boolean => {
          if (this.pendingWinnerAdmission === token) this.pendingWinnerAdmission = undefined;
          if (committed && previous === undefined && this.acceptedWinnerContext === context) {
            this.acceptedWinnerContext = undefined;
            committed = false;
            return true;
          }
          committed = false;
          return this.acceptedWinnerContext === previous;
        },
      });
    } catch {
      return undefined;
    }
  }

  public capture<Arguments extends readonly unknown[]>(
    callback: (...arguments_: Arguments) => unknown
  ): (...arguments_: Arguments) => boolean {
    return (...arguments_: Arguments): boolean => {
      if (!this.isCurrent()) return false;
      callback(...arguments_);
      return true;
    };
  }

  public isCurrent(): boolean {
    return !this.disposed && this.ownerIsCurrent();
  }

  public onDispose(kind: string, callback: DisposeCallback): void {
    this.scope.onDispose(kind, callback);
  }

  public dispose(): void {
    this.pendingWinnerAdmission = undefined;
    this.scope.dispose();
  }
}

class AuctionBatchOwner implements AuctionBatchScope {
  private readonly scope: OwnerScope;
  private readonly attempts = new Map<string, RenderAttemptOwner>();
  private readonly attemptOrder: RenderAttemptOwner[] = [];
  private isDisposing = false;

  public constructor(
    private readonly issuer: NavigationIdentityIssuer,
    public readonly interfaces: RuntimeInterfaces,
    private readonly ownerIsCurrent: () => boolean,
    private readonly attemptExists: (slot: string) => boolean,
    private readonly registerAttempt: (slot: string, attempt: RenderAttemptOwner) => boolean,
    private readonly releaseAttempt: (slot: string, attempt: RenderAttemptOwner) => void,
    private readonly onDisposalError?: DisposalErrorHandler
  ) {
    this.scope = new OwnerScope(onDisposalError);
  }

  public get generation(): object {
    return this.scope.generation;
  }

  public get disposed(): boolean {
    return this.isDisposing || this.scope.disposed;
  }

  public get signal(): AbortSignal {
    return this.scope.signal;
  }

  public createRenderAttempt(slot: string): RenderAttemptResult {
    if (!this.isCurrent()) return Object.freeze({ ok: false, reason: 'stale_owner' });
    if (this.attempts.has(slot) || this.attemptExists(slot)) {
      return Object.freeze({ ok: false, reason: 'attempt_exists' });
    }
    const identity = this.issuer.mintAttemptId();
    if (!identity.ok) return identity;
    const attemptReference: { current?: RenderAttemptOwner } = {};
    const attempt = new RenderAttemptOwner(
      identity.value,
      slot,
      this.interfaces,
      (): boolean => this.isCurrent() && this.attempts.get(slot) === attemptReference.current,
      this.onDisposalError
    );
    attemptReference.current = attempt;
    if (!this.registerAttempt(slot, attempt)) {
      attempt.dispose();
      return Object.freeze({ ok: false, reason: 'stale_owner' });
    }
    this.attempts.set(slot, attempt);
    this.attemptOrder.push(attempt);
    attempt.onDispose('attempt-index', () => {
      this.attempts.delete(slot);
      this.releaseAttempt(slot, attempt);
      const orderIndex = this.attemptOrder.indexOf(attempt);
      if (orderIndex >= 0) this.attemptOrder.splice(orderIndex, 1);
    });
    return Object.freeze({ ok: true, value: attempt });
  }

  public isCurrent(): boolean {
    return !this.disposed && this.ownerIsCurrent();
  }

  public retainedAttemptCount(): number {
    return this.attemptOrder.length;
  }

  public onDispose(kind: string, callback: DisposeCallback): void {
    this.scope.onDispose(kind, callback);
  }

  public dispose(): void {
    if (this.disposed) return;
    this.isDisposing = true;
    for (let index = this.attemptOrder.length - 1; index >= 0; index -= 1) {
      this.attemptOrder[index]?.dispose();
    }
    this.attemptOrder.length = 0;
    this.attempts.clear();
    this.scope.dispose();
  }
}

class NavigationSessionOwner implements NavigationSession {
  private readonly scope: OwnerScope;
  private readonly aliases = new Set<string>();
  private readonly intents = new Set<string>();
  private readonly targetingOwners = new Set<string>();
  private readonly batches = new Map<string, AuctionBatchOwner>();
  private readonly attempts = new Map<string, RenderAttemptOwner>();
  private readonly batchOrder: AuctionBatchOwner[] = [];
  private issuer: NavigationIdentityIssuer | undefined;
  private ownerIsCurrentCallback: (() => boolean) | undefined;
  private onDisposingCallback: (() => DisposeCallback | undefined) | undefined;
  private onDisposedCallback: (() => void) | undefined;
  private projection: Readonly<object> | undefined;
  private isDisposing = false;

  public constructor(
    issuer: NavigationIdentityIssuer,
    initialProjection: Readonly<object> | undefined,
    public readonly interfaces: RuntimeInterfaces,
    ownerIsCurrent: () => boolean,
    onDisposing: () => DisposeCallback | undefined,
    onDisposed: () => void,
    private readonly onDisposalError?: DisposalErrorHandler
  ) {
    this.scope = new OwnerScope(onDisposalError);
    this.issuer = issuer;
    this.ownerIsCurrentCallback = ownerIsCurrent;
    this.onDisposingCallback = onDisposing;
    this.onDisposedCallback = onDisposed;
    this.projection = initialProjection;
  }

  public get generation(): object {
    return this.scope.generation;
  }

  public get disposed(): boolean {
    return this.isDisposing || this.scope.disposed;
  }

  public get signal(): AbortSignal {
    return this.scope.signal;
  }

  public get currentAuctionProjection(): Readonly<object> | undefined {
    return this.projection;
  }

  public capture<Arguments extends readonly unknown[]>(
    callback: (...arguments_: Arguments) => unknown
  ): (...arguments_: Arguments) => boolean {
    return (...arguments_: Arguments): boolean => {
      if (!this.isCurrent()) return false;
      callback(...arguments_);
      return true;
    };
  }

  public claimAlias(alias: string): boolean {
    return this.claim(this.aliases, alias);
  }

  public claimIntent(slot: string): boolean {
    return this.claim(this.intents, slot);
  }

  public claimTargeting(slot: string): boolean {
    return this.claim(this.targetingOwners, slot);
  }

  public createAuctionBatch(key: string): AuctionBatchScope | undefined {
    const issuer = this.issuer;
    if (!issuer || !this.isCurrent() || this.batches.has(key)) return undefined;
    const batchReference: { current?: AuctionBatchOwner } = {};
    const batch = new AuctionBatchOwner(
      issuer,
      this.interfaces,
      (): boolean => this.isCurrent() && this.batches.get(key) === batchReference.current,
      (slot) => this.attempts.has(slot),
      (slot, attempt) => {
        if (!this.isCurrent() || this.attempts.has(slot)) return false;
        this.attempts.set(slot, attempt);
        return true;
      },
      (slot, attempt) => {
        if (this.attempts.get(slot) === attempt) this.attempts.delete(slot);
      },
      this.onDisposalError
    );
    batchReference.current = batch;
    this.batches.set(key, batch);
    this.batchOrder.push(batch);
    batch.onDispose('batch-index', () => {
      this.batches.delete(key);
      const orderIndex = this.batchOrder.indexOf(batch);
      if (orderIndex >= 0) this.batchOrder.splice(orderIndex, 1);
    });
    return batch;
  }

  public installAuctionProjection(projection: Readonly<object>): boolean {
    if (!this.isCurrent() || this.projection !== undefined || !recursivelyFrozen(projection)) {
      return false;
    }
    this.projection = projection;
    return true;
  }

  public isCurrent(): boolean {
    return !this.disposed && (this.ownerIsCurrentCallback?.() ?? false);
  }

  public onDispose(kind: string, callback: DisposeCallback): void {
    this.scope.onDispose(kind, callback);
  }

  public dispose(): void {
    if (this.disposed) return;
    this.isDisposing = true;
    const releaseTransition = this.onDisposingCallback?.();
    try {
      for (let index = this.batchOrder.length - 1; index >= 0; index -= 1) {
        this.batchOrder[index]?.dispose();
      }
      this.batchOrder.length = 0;
      this.batches.clear();
      this.attempts.clear();
      this.scope.dispose();
      this.aliases.clear();
      this.intents.clear();
      this.targetingOwners.clear();
      this.projection = undefined;
    } finally {
      const onDisposed = this.onDisposedCallback;
      this.issuer = undefined;
      this.ownerIsCurrentCallback = undefined;
      this.onDisposingCallback = undefined;
      this.onDisposedCallback = undefined;
      try {
        onDisposed?.();
      } finally {
        releaseTransition?.();
      }
    }
  }

  public snapshotInventoryForTest(): NavigationInventorySnapshot {
    let retainedAttemptScopes = 0;
    for (const batch of this.batchOrder) {
      retainedAttemptScopes += batch.retainedAttemptCount();
    }
    return Object.freeze({
      activeDisposers: this.scope.activeDisposerCount(),
      aliases: this.aliases.size,
      attempts: this.attempts.size,
      batches: this.batches.size,
      disposed: this.disposed,
      disposedByKind: this.scope.disposedInventory(),
      hasAuctionProjection: this.projection !== undefined,
      intents: this.intents.size,
      retainedAttemptScopes,
      retainedBatchScopes: this.batchOrder.length,
      targetingOwners: this.targetingOwners.size,
    });
  }

  private claim(index: Set<string>, key: string): boolean {
    if (!this.isCurrent() || index.has(key)) return false;
    index.add(key);
    return true;
  }
}

class RuntimeSessionOwner implements RuntimeSession {
  public readonly generation = Object.freeze({});
  public readonly interfaces: RuntimeInterfaces;
  private readonly scope: OwnerScope;
  private readonly createIdentityIssuer: NavigationIdentityIssuerFactory;
  private readonly onDisposalError: DisposalErrorHandler | undefined;
  private navigation: NavigationSessionOwner | undefined;
  private started = false;
  private disposedNavigations = 0;
  private isDisposing = false;
  private navigationTransitionInProgress = false;

  public constructor(options: RuntimeSessionOptions) {
    this.createIdentityIssuer = options.createIdentityIssuer;
    this.onDisposalError = options.onDisposalError;
    this.scope = new OwnerScope(options.onDisposalError);
    this.interfaces = options.interfaces ?? EMPTY_INTERFACES;
  }

  public get disposed(): boolean {
    return this.isDisposing || this.scope.disposed;
  }

  public get currentNavigation(): NavigationSession | undefined {
    return this.navigation;
  }

  public startInitialNavigation(projection?: Readonly<object>): NavigationSessionResult {
    if (this.disposed) return Object.freeze({ ok: false, reason: 'runtime_disposed' });
    if (this.started) return Object.freeze({ ok: false, reason: 'navigation_already_started' });
    if (this.navigationTransitionInProgress) {
      return Object.freeze({ ok: false, reason: 'navigation_transition_in_progress' });
    }
    if (projection !== undefined && !recursivelyFrozen(projection)) {
      return Object.freeze({ ok: false, reason: 'invalid_projection' });
    }
    this.navigationTransitionInProgress = true;
    try {
      const result = this.createNavigation(projection);
      if (result.ok) this.started = true;
      return result;
    } finally {
      this.navigationTransitionInProgress = false;
    }
  }

  public replaceNavigation(): NavigationSessionResult {
    if (this.disposed) return Object.freeze({ ok: false, reason: 'runtime_disposed' });
    if (this.navigationTransitionInProgress) {
      return Object.freeze({ ok: false, reason: 'navigation_transition_in_progress' });
    }
    this.navigationTransitionInProgress = true;
    try {
      const identity = this.obtainIdentityIssuer();
      if (!identity.ok) return identity;
      if (this.disposed) return Object.freeze({ ok: false, reason: 'runtime_disposed' });

      const previous = this.navigation;
      this.navigation = undefined;
      previous?.dispose();
      if (this.disposed) return Object.freeze({ ok: false, reason: 'runtime_disposed' });

      const next = this.buildNavigation(identity.value, undefined);
      this.navigation = next;
      this.started = true;
      return Object.freeze({ ok: true, value: next });
    } finally {
      this.navigationTransitionInProgress = false;
    }
  }

  public isCurrent(): boolean {
    return !this.disposed;
  }

  public onDispose(kind: string, callback: DisposeCallback): void {
    this.scope.onDispose(kind, callback);
  }

  public dispose(): void {
    if (this.disposed) return;
    this.isDisposing = true;
    this.navigation?.dispose();
    this.navigation = undefined;
    this.scope.dispose();
  }

  public snapshotInventoryForTest(): RuntimeInventorySnapshot {
    return Object.freeze({
      activeDisposers: this.scope.activeDisposerCount(),
      currentNavigationGeneration: this.navigation?.generation,
      disposed: this.disposed,
      disposedByKind: this.scope.disposedInventory(),
      disposedNavigations: this.disposedNavigations,
      navigationCount: this.navigation && !this.navigation.disposed ? 1 : 0,
    });
  }

  private createNavigation(projection?: Readonly<object>): NavigationSessionResult {
    const identity = this.obtainIdentityIssuer();
    if (!identity.ok) return identity;
    if (this.disposed) return Object.freeze({ ok: false, reason: 'runtime_disposed' });
    const navigation = this.buildNavigation(identity.value, projection);
    this.navigation = navigation;
    return Object.freeze({ ok: true, value: navigation });
  }

  private buildNavigation(
    identityIssuer: NavigationIdentityIssuer,
    projection: Readonly<object> | undefined
  ): NavigationSessionOwner {
    const navigationReference: { current?: NavigationSessionOwner } = {};
    const navigation = new NavigationSessionOwner(
      identityIssuer,
      projection,
      this.interfaces,
      () => !this.disposed && this.navigation === navigationReference.current,
      () => {
        const disposingNavigation = navigationReference.current;
        const ownsCurrent =
          disposingNavigation !== undefined && this.navigation === disposingNavigation;
        if (ownsCurrent) this.navigation = undefined;
        if (!ownsCurrent || this.navigationTransitionInProgress || this.disposed) return undefined;
        this.navigationTransitionInProgress = true;
        return () => {
          this.navigationTransitionInProgress = false;
        };
      },
      () => {
        const disposedNavigation = navigationReference.current;
        if (disposedNavigation && this.navigation === disposedNavigation) {
          this.navigation = undefined;
        }
        delete navigationReference.current;
        this.disposedNavigations += 1;
      },
      this.onDisposalError
    );
    navigationReference.current = navigation;
    return navigation;
  }

  private obtainIdentityIssuer(): IdentityGenerationResult<NavigationIdentityIssuer> {
    try {
      return this.createIdentityIssuer();
    } catch {
      return Object.freeze({ ok: false, reason: 'identity_generation_failed' });
    }
  }
}

/** Construct one document-lifetime runtime session from injected interfaces only. */
export function createRuntimeSession(options: RuntimeSessionOptions): RuntimeSession {
  return new RuntimeSessionOwner(options);
}
