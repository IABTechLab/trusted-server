import type { RenderAttemptScope, WinnerContext } from '../kernel/sessions';

import type { ReservationRenderSource } from './reservations';

const ATTEMPT_ID = /^a1_[A-Za-z0-9_-]{22}$/;
const objectFreezeIntrinsic = Object.freeze;
const arrayIncludesIntrinsic = Array.prototype.includes;
const artifactDisposals = new WeakMap<object, boolean>();

function frozen<const Value extends object>(value: Value): Readonly<Value> {
  return Reflect.apply(objectFreezeIntrinsic, Object, [value]) as Readonly<Value>;
}

export const RENDER_FAILURE_REASONS = frozen([
  'auction_timeout',
  'auction_disabled',
  'consent_denied',
  'slot_not_eligible',
  'provider_timeout',
  'provider_error',
  'invalid_provider_response',
  'mediation_failed',
  'winner_not_renderable',
  'internal_error',
  'network_error',
  'http_error',
  'invalid_response',
  'slot_unresolved',
  'descriptor_invalid',
  'invalid_dimensions',
  'dimensions_out_of_range',
  'no_render_source',
  'registry_full',
  'capability_registry_full',
  'external_queue_full',
  'external_ready_timeout',
  'external_artifact_incompatible',
  'prebid_admission_failed',
  'prebid_contract_violation',
  'prebid_selection_timeout',
  'reservation_collision',
  'identity_generation_failed',
  'cycle_unattributable',
  'slot_quarantined',
  'gpt_request_failed',
  'gpt_request_timeout',
  'gpt_completion_timeout',
  'reconciliation_capacity',
  'gam_empty',
  'bridge_claim_timeout',
  'bridge_id_mismatch',
  'owner_registration_timeout',
  'owner_insertion_timeout',
  'renderer_document_no_load',
  'runner_no_load',
  'runner_failed',
  'cache_network_error',
  'cache_http_error',
  'cache_invalid_response',
  'adm_document_no_load',
  'abi_mismatch',
  'bundle_partial',
] as const);

export type RenderFailureReason = (typeof RENDER_FAILURE_REASONS)[number];

export const RENDER_CANCELLATION_REASONS = frozen([
  'caller_aborted',
  'superseded',
  'navigation_disposed',
] as const);

export type RenderCancellationReason = (typeof RENDER_CANCELLATION_REASONS)[number];

export type RenderOutcome =
  | Readonly<{ outcome: 'accepted' }>
  | Readonly<{ outcome: 'no_bid' }>
  | Readonly<{ outcome: 'failed'; reason: RenderFailureReason }>
  | Readonly<{ outcome: 'cancelled'; reason: RenderCancellationReason }>;

export type RenderAttemptActiveState =
  | 'created'
  | 'waiting_for_gam_and_claim'
  | 'waiting_for_owner'
  | 'waiting_for_insertion'
  | 'rendering_direct'
  | 'waiting_for_document'
  | 'waiting_for_aps_completion'
  | 'waiting_for_adm';

export type RenderAttemptState =
  RenderAttemptActiveState | 'accepted' | 'no_bid' | 'failed' | 'cancelled';

export interface CommittedRenderArtifact {
  readonly kind: 'direct_iframe' | 'puc';
  readonly attemptId: string;
  readonly slot: string;
  readonly navigationGeneration: object;
  readonly dispose: () => void;
}

export interface CommittedArtifactStore {
  readonly promote: (artifact: CommittedRenderArtifact, stillCurrent?: () => boolean) => boolean;
  readonly current: (slot: string) => CommittedRenderArtifact | undefined;
  readonly release: (artifact: CommittedRenderArtifact) => boolean;
  readonly disposeNavigation: (navigationGeneration: object) => void;
  readonly dispose: () => void;
}

export interface RenderDeadline {
  readonly milliseconds: number;
  readonly reason: RenderFailureReason;
}

export interface RenderScheduler {
  readonly set: (callback: () => void, milliseconds: number) => unknown;
  readonly clear: (handle: unknown) => void;
}

/** Fixed deadlines owned by the state transition that enters each wait. */
export const RENDER_STATE_DEADLINES: Readonly<
  Partial<Record<RenderAttemptActiveState, RenderDeadline>>
> = frozen({
  waiting_for_owner: frozen({
    milliseconds: 3_000,
    reason: 'owner_registration_timeout',
  }),
  waiting_for_insertion: frozen({
    milliseconds: 1_000,
    reason: 'owner_insertion_timeout',
  }),
  waiting_for_document: frozen({
    milliseconds: 3_000,
    reason: 'renderer_document_no_load',
  }),
  waiting_for_aps_completion: frozen({ milliseconds: 10_000, reason: 'runner_failed' }),
  waiting_for_adm: frozen({ milliseconds: 5_000, reason: 'adm_document_no_load' }),
});

export interface RenderAttemptOptions {
  readonly owner: RenderAttemptScope;
  readonly artifacts: CommittedArtifactStore;
  readonly prepareRenderSource: (candidate: unknown) => ReservationRenderSource | undefined;
  readonly parentAttemptId?: string;
  readonly scheduler?: RenderScheduler;
}

export type RenderAttemptCreationResult =
  | Readonly<{ ok: true; value: RenderAttempt }>
  | Readonly<{
      ok: false;
      reason: 'identity_generation_failed' | 'invalid_attempt' | 'stale_owner';
    }>;

export interface RenderAttemptSnapshot {
  readonly history: readonly RenderAttemptState[];
  readonly outcome: RenderOutcome | undefined;
  readonly state: RenderAttemptState;
}

export interface RenderAttempt {
  readonly id: string;
  readonly slot: string;
  readonly generation: object;
  readonly navigationGeneration: object;
  readonly parentAttemptId: string | undefined;
  readonly renderSource: ReservationRenderSource | undefined;
  readonly winnerContext: WinnerContext | undefined;
  readonly admitDirectWinner: (source: unknown, context: WinnerContext) => boolean;
  readonly admitClaimedWinner: (source: unknown) => boolean;
  readonly beginGamClaim: () => boolean;
  readonly ownerClaimed: () => boolean;
  readonly ownerRegistered: () => boolean;
  readonly beginDirect: () => boolean;
  readonly beginApsDocument: (artifact: CommittedRenderArtifact) => boolean;
  readonly beginAdm: (artifact: CommittedRenderArtifact) => boolean;
  readonly apsDocumentAccepted: () => boolean;
  readonly accept: () => boolean;
  readonly noBid: () => boolean;
  readonly fail: (reason: RenderFailureReason) => boolean;
  readonly cancel: (reason: RenderCancellationReason) => boolean;
  readonly onSettled: (callback: (outcome: RenderOutcome) => void) => boolean;
  readonly snapshot: () => RenderAttemptSnapshot;
}

export interface SlotOperationResult {
  readonly path: 'primary' | 'fallback';
  readonly outcome: RenderOutcome;
  readonly primaryAttemptId: string;
  readonly primary: RenderOutcome;
  readonly fallbackAttemptId?: string;
  readonly fallback?: RenderOutcome;
}

export interface SlotOperationSnapshot {
  readonly settled: boolean;
  readonly result?: SlotOperationResult;
}

export interface SlotOperation {
  readonly snapshot: () => SlotOperationSnapshot;
  readonly onSettled: (callback: (result: SlotOperationResult) => void) => boolean;
}

export interface SlotOperationOptions {
  readonly primary: RenderAttempt;
  readonly createFallback?: (parentAttemptId: string) => RenderAttemptCreationResult;
}

function validAttemptId(value: unknown): value is string {
  return typeof value === 'string' && ATTEMPT_ID.test(value);
}

function validFailureReason(value: unknown): value is RenderFailureReason {
  return (
    typeof value === 'string' &&
    (Reflect.apply(arrayIncludesIntrinsic, RENDER_FAILURE_REASONS, [value]) as boolean)
  );
}

function validCancellationReason(value: unknown): value is RenderCancellationReason {
  return (
    typeof value === 'string' &&
    (Reflect.apply(arrayIncludesIntrinsic, RENDER_CANCELLATION_REASONS, [value]) as boolean)
  );
}

function validOutcome(value: unknown): value is RenderOutcome {
  try {
    if (
      typeof value !== 'object' ||
      value === null ||
      !Object.isFrozen(value) ||
      Object.getPrototypeOf(value) !== Object.prototype ||
      Object.getOwnPropertySymbols(value).length !== 0
    ) {
      return false;
    }
    const names = Object.getOwnPropertyNames(value);
    const outcome = Object.getOwnPropertyDescriptor(value, 'outcome');
    if (!outcome || !('value' in outcome) || outcome.enumerable !== true) return false;
    if (outcome.value === 'accepted' || outcome.value === 'no_bid') {
      return names.length === 1;
    }
    const reason = Object.getOwnPropertyDescriptor(value, 'reason');
    if (!reason || !('value' in reason) || reason.enumerable !== true || names.length !== 2) {
      return false;
    }
    return outcome.value === 'failed'
      ? validFailureReason(reason.value)
      : outcome.value === 'cancelled' && validCancellationReason(reason.value);
  } catch {
    return false;
  }
}

function permitsPromotion(stillCurrent: (() => boolean) | undefined): boolean {
  if (!stillCurrent) return true;
  try {
    return stillCurrent() === true;
  } catch {
    return false;
  }
}

function validArtifact(
  value: unknown,
  slot?: string,
  navigationGeneration?: object,
  attemptId?: string
): value is CommittedRenderArtifact {
  try {
    if ((typeof value !== 'object' && typeof value !== 'function') || value === null) return false;
    if (!Object.isFrozen(value) || Object.getPrototypeOf(value) !== Object.prototype) return false;
    const names = Object.getOwnPropertyNames(value).sort();
    if (
      names.length !== 5 ||
      names[0] !== 'attemptId' ||
      names[1] !== 'dispose' ||
      names[2] !== 'kind' ||
      names[3] !== 'navigationGeneration' ||
      names[4] !== 'slot' ||
      Object.getOwnPropertySymbols(value).length !== 0
    ) {
      return false;
    }
    for (const name of names) {
      const descriptor = Object.getOwnPropertyDescriptor(value, name);
      if (!descriptor || !('value' in descriptor) || !descriptor.enumerable) return false;
    }
    const artifact = value as CommittedRenderArtifact;
    return (
      (artifact.kind === 'direct_iframe' || artifact.kind === 'puc') &&
      validAttemptId(artifact.attemptId) &&
      typeof artifact.slot === 'string' &&
      artifact.slot.length > 0 &&
      (typeof artifact.navigationGeneration === 'object' ||
        typeof artifact.navigationGeneration === 'function') &&
      artifact.navigationGeneration !== null &&
      typeof artifact.dispose === 'function' &&
      (slot === undefined || artifact.slot === slot) &&
      (navigationGeneration === undefined ||
        artifact.navigationGeneration === navigationGeneration) &&
      (attemptId === undefined || artifact.attemptId === attemptId)
    );
  } catch {
    return false;
  }
}

function disposeArtifact(artifact: CommittedRenderArtifact | undefined): boolean {
  if (!artifact) return true;
  try {
    if (artifactDisposals.has(artifact)) return artifactDisposals.get(artifact) === true;
    artifactDisposals.set(artifact, false);
  } catch {
    return false;
  }
  try {
    artifact.dispose();
    artifactDisposals.set(artifact, true);
    return true;
  } catch {
    return false;
  }
}

/** Own committed artifacts independently from terminal attempt scopes. */
export function createCommittedArtifactStore(): CommittedArtifactStore {
  const entries = new Map<string, CommittedRenderArtifact>();
  const pendingNavigationDisposals = new Set<object>();
  let disposed = false;
  let mutating = false;
  let disposeRequested = false;

  const disposeGeneration = (navigationGeneration: object): void => {
    const snapshot = Array.from(entries.entries());
    for (const [slot, artifact] of snapshot) {
      if (
        artifact.navigationGeneration === navigationGeneration &&
        entries.get(slot) === artifact
      ) {
        entries.delete(slot);
        disposeArtifact(artifact);
      }
    }
  };

  const drainDeferredDisposal = (): void => {
    if (mutating) return;
    if (disposeRequested) {
      disposeRequested = false;
      const snapshot = Array.from(entries.values());
      entries.clear();
      disposed = true;
      pendingNavigationDisposals.clear();
      for (const artifact of snapshot) disposeArtifact(artifact);
      return;
    }
    const generations = Array.from(pendingNavigationDisposals);
    pendingNavigationDisposals.clear();
    if (generations.length === 0) return;
    mutating = true;
    try {
      for (const generation of generations) disposeGeneration(generation);
    } finally {
      mutating = false;
      if (disposeRequested || pendingNavigationDisposals.size > 0) drainDeferredDisposal();
    }
  };

  const store: CommittedArtifactStore = {
    promote(artifact, stillCurrent): boolean {
      if (disposed || mutating || !validArtifact(artifact)) return false;
      const existing = entries.get(artifact.slot);
      if (existing === artifact) return false;
      mutating = true;
      try {
        if (existing) {
          if (!disposeArtifact(existing) || entries.get(artifact.slot) !== existing) return false;
        }
        if (
          disposeRequested ||
          pendingNavigationDisposals.has(artifact.navigationGeneration) ||
          !permitsPromotion(stillCurrent)
        ) {
          if (existing && entries.get(artifact.slot) === existing) entries.delete(artifact.slot);
          return false;
        }
        entries.set(artifact.slot, artifact);
        return entries.get(artifact.slot) === artifact;
      } catch {
        return false;
      } finally {
        mutating = false;
        drainDeferredDisposal();
      }
    },
    current(slot): CommittedRenderArtifact | undefined {
      if (disposed || typeof slot !== 'string') return undefined;
      try {
        return entries.get(slot);
      } catch {
        return undefined;
      }
    },
    release(artifact): boolean {
      if (disposed || mutating || !validArtifact(artifact)) return false;
      mutating = true;
      try {
        if (entries.get(artifact.slot) !== artifact) return false;
        entries.delete(artifact.slot);
        disposeArtifact(artifact);
        return entries.get(artifact.slot) !== artifact;
      } catch {
        return false;
      } finally {
        mutating = false;
        drainDeferredDisposal();
      }
    },
    disposeNavigation(navigationGeneration): void {
      if (disposed) return;
      pendingNavigationDisposals.add(navigationGeneration);
      if (mutating) return;
      mutating = true;
      try {
        pendingNavigationDisposals.delete(navigationGeneration);
        disposeGeneration(navigationGeneration);
      } catch {
        // Disposal is best-effort and never publishes a replacement artifact.
      } finally {
        mutating = false;
        drainDeferredDisposal();
      }
    },
    dispose(): void {
      if (disposed) return;
      disposeRequested = true;
      if (mutating) return;
      mutating = true;
      try {
        const snapshot = Array.from(entries.values());
        entries.clear();
        disposeRequested = false;
        disposed = true;
        pendingNavigationDisposals.clear();
        for (const artifact of snapshot) disposeArtifact(artifact);
      } catch {
        disposeRequested = false;
        disposed = true;
        entries.clear();
      } finally {
        mutating = false;
      }
    },
  };
  return frozen(store);
}

function defaultScheduler(): RenderScheduler {
  return frozen({
    set: (callback: () => void, milliseconds: number): unknown =>
      globalThis.setTimeout(callback, milliseconds),
    clear: (handle: unknown): void => {
      globalThis.clearTimeout(handle as ReturnType<typeof globalThis.setTimeout>);
    },
  });
}

function terminalState(outcome: RenderOutcome): RenderAttemptState {
  return outcome.outcome;
}

/** Construct one path-independent attempt lifecycle around an issued owner scope. */
export function createRenderAttempt(options: RenderAttemptOptions): RenderAttemptCreationResult {
  let owner: RenderAttemptScope;
  let artifacts: CommittedArtifactStore;
  let id: string;
  let slot: string;
  let generation: object;
  let navigationGeneration: object;
  let parentAttemptId: string | undefined;
  let prepareRenderSource: (candidate: unknown) => ReservationRenderSource | undefined;
  let ownerIsCurrentMethod: RenderAttemptScope['isCurrent'];
  let ownerDisposeMethod: RenderAttemptScope['dispose'];
  let ownerOnDisposeMethod: RenderAttemptScope['onDispose'];
  let ownerPrepareWinnerMethod: RenderAttemptScope['prepareWinnerContext'];
  let promoteArtifactMethod: CommittedArtifactStore['promote'];
  let currentArtifactMethod: CommittedArtifactStore['current'];
  let releaseArtifactMethod: CommittedArtifactStore['release'];
  try {
    owner = options.owner;
    artifacts = options.artifacts;
    id = owner.id;
    slot = owner.slot;
    generation = owner.generation;
    navigationGeneration = owner.navigationGeneration;
    parentAttemptId = options.parentAttemptId;
    prepareRenderSource = options.prepareRenderSource;
    ownerIsCurrentMethod = owner.isCurrent;
    ownerDisposeMethod = owner.dispose;
    ownerOnDisposeMethod = owner.onDispose;
    ownerPrepareWinnerMethod = owner.prepareWinnerContext;
    promoteArtifactMethod = artifacts.promote;
    currentArtifactMethod = artifacts.current;
    releaseArtifactMethod = artifacts.release;
    if (
      !validAttemptId(id) ||
      typeof slot !== 'string' ||
      slot.length === 0 ||
      (typeof generation !== 'object' && typeof generation !== 'function') ||
      generation === null ||
      (typeof navigationGeneration !== 'object' && typeof navigationGeneration !== 'function') ||
      navigationGeneration === null ||
      generation === navigationGeneration ||
      typeof promoteArtifactMethod !== 'function' ||
      typeof currentArtifactMethod !== 'function' ||
      typeof releaseArtifactMethod !== 'function' ||
      typeof ownerIsCurrentMethod !== 'function' ||
      typeof ownerDisposeMethod !== 'function' ||
      typeof ownerOnDisposeMethod !== 'function' ||
      typeof ownerPrepareWinnerMethod !== 'function' ||
      typeof prepareRenderSource !== 'function' ||
      (parentAttemptId !== undefined &&
        (!validAttemptId(parentAttemptId) || parentAttemptId === id))
    ) {
      return frozen({ ok: false, reason: 'invalid_attempt' });
    }
  } catch {
    return frozen({ ok: false, reason: 'invalid_attempt' });
  }
  const ownerIsCurrent = (): boolean => {
    try {
      return Reflect.apply(ownerIsCurrentMethod, owner, []) === true;
    } catch {
      return false;
    }
  };
  if (!ownerIsCurrent()) return frozen({ ok: false, reason: 'stale_owner' });

  const scheduler = options.scheduler ?? defaultScheduler();
  let schedulerSetMethod: RenderScheduler['set'];
  let schedulerClearMethod: RenderScheduler['clear'];
  try {
    schedulerSetMethod = scheduler.set;
    schedulerClearMethod = scheduler.clear;
    if (typeof schedulerSetMethod !== 'function' || typeof schedulerClearMethod !== 'function') {
      return frozen({ ok: false, reason: 'invalid_attempt' });
    }
  } catch {
    return frozen({ ok: false, reason: 'invalid_attempt' });
  }
  const history: RenderAttemptState[] = ['created'];
  const observers: Array<(outcome: RenderOutcome) => void> = [];
  let state: RenderAttemptState = 'created';
  let outcome: RenderOutcome | undefined;
  let pendingArtifact: CommittedRenderArtifact | undefined;
  let admittedRenderSource: ReservationRenderSource | undefined;
  let admittedWinnerContext: WinnerContext | undefined;
  let deadlineHandle: unknown;
  let deadlineState: RenderAttemptActiveState | undefined;
  let settlingInternally = false;

  const prepareSource = (candidate: unknown): ReservationRenderSource | undefined => {
    try {
      const source = prepareRenderSource(candidate);
      if (!source || !Object.isFrozen(source)) return undefined;
      const type = Object.getOwnPropertyDescriptor(source, 'type');
      const version = Object.getOwnPropertyDescriptor(source, 'version');
      if (
        !type ||
        !('value' in type) ||
        (type.value !== 'aps' && type.value !== 'adm' && type.value !== 'cache') ||
        !version ||
        !('value' in version) ||
        version.value !== 1
      ) {
        return undefined;
      }
      return source;
    } catch {
      return undefined;
    }
  };

  const validWinnerContext = (context: unknown): context is WinnerContext => {
    try {
      if (
        (typeof context !== 'object' && typeof context !== 'function') ||
        context === null ||
        !Object.isFrozen(context) ||
        Object.getPrototypeOf(context) !== Object.prototype ||
        Object.getOwnPropertyNames(context).length !== 1 ||
        Object.getOwnPropertySymbols(context).length !== 0
      ) {
        return false;
      }
      const selectedCpm = Object.getOwnPropertyDescriptor(context, 'selectedCpm');
      return (
        !!selectedCpm &&
        'value' in selectedCpm &&
        selectedCpm.enumerable === true &&
        typeof selectedCpm.value === 'number' &&
        Number.isFinite(selectedCpm.value) &&
        selectedCpm.value >= 0
      );
    } catch {
      return false;
    }
  };

  const readWinnerContext = (): WinnerContext | undefined => {
    try {
      return owner.winnerContext;
    } catch {
      return undefined;
    }
  };

  const admitDirectWinner = (candidate: unknown, context: WinnerContext): boolean => {
    if (
      state !== 'created' ||
      outcome !== undefined ||
      admittedRenderSource !== undefined ||
      admittedWinnerContext !== undefined ||
      !ownerIsCurrent() ||
      !validWinnerContext(context)
    ) {
      return false;
    }
    const source = prepareSource(candidate);
    if (!source) return false;
    let admission: ReturnType<RenderAttemptScope['prepareWinnerContext']>;
    try {
      admission = Reflect.apply(ownerPrepareWinnerMethod, owner, [context]);
    } catch {
      return false;
    }
    if (!admission) return false;
    let committed: boolean;
    try {
      committed = admission.commit() === true;
    } catch {
      committed = false;
    }
    if (!committed || !ownerIsCurrent() || readWinnerContext() !== context) {
      try {
        admission.rollback();
      } catch {
        // Failed admission retains no lifecycle source/context authority.
      }
      return false;
    }
    admittedRenderSource = source;
    admittedWinnerContext = context;
    return true;
  };

  const admitClaimedWinner = (candidate: unknown): boolean => {
    if (
      state !== 'waiting_for_gam_and_claim' ||
      outcome !== undefined ||
      admittedRenderSource !== undefined ||
      admittedWinnerContext !== undefined ||
      !ownerIsCurrent()
    ) {
      return false;
    }
    const context = readWinnerContext();
    if (!validWinnerContext(context)) return false;
    const source = prepareSource(candidate);
    if (!source || !ownerIsCurrent() || readWinnerContext() !== context) return false;
    admittedRenderSource = source;
    admittedWinnerContext = context;
    return true;
  };

  const clearDeadline = (): void => {
    if (deadlineState === undefined) return;
    const handle = deadlineHandle;
    deadlineHandle = undefined;
    deadlineState = undefined;
    try {
      Reflect.apply(schedulerClearMethod, scheduler, [handle]);
    } catch {
      // A cleared logical deadline remains inert even when the host clear throws.
    }
  };

  const notify = (terminal: RenderOutcome): void => {
    const snapshot = observers.splice(0, observers.length);
    for (const observer of snapshot) {
      try {
        observer(terminal);
      } catch {
        // Observation cannot change the terminal result.
      }
    }
  };

  const settle = (terminal: RenderOutcome, disposeOwner: boolean): boolean => {
    if (outcome !== undefined) return false;
    outcome = terminal;
    state = terminalState(terminal);
    history.push(state);
    clearDeadline();
    admittedRenderSource = undefined;
    admittedWinnerContext = undefined;
    if (terminal.outcome !== 'accepted') {
      const uncommitted = pendingArtifact;
      pendingArtifact = undefined;
      disposeArtifact(uncommitted);
    }
    if (disposeOwner) {
      settlingInternally = true;
      try {
        Reflect.apply(ownerDisposeMethod, owner, []);
      } catch {
        // The terminal latch and owned-resource cleanup remain authoritative.
      } finally {
        settlingInternally = false;
      }
    }
    notify(terminal);
    return true;
  };

  const fail = (reason: RenderFailureReason): boolean =>
    validFailureReason(reason) && (reason !== 'gam_empty' || state === 'waiting_for_gam_and_claim')
      ? settle(frozen({ outcome: 'failed', reason }), true)
      : false;

  const armDeadline = (entered: RenderAttemptActiveState): void => {
    const deadline = RENDER_STATE_DEADLINES[entered];
    if (!deadline) return;
    deadlineState = entered;
    try {
      const handle = Reflect.apply(schedulerSetMethod, scheduler, [
        () => {
          if (outcome === undefined && state === entered && deadlineState === entered) {
            fail(deadline.reason);
          }
        },
        deadline.milliseconds,
      ]);
      if (outcome === undefined && state === entered && deadlineState === entered) {
        deadlineHandle = handle;
      } else {
        try {
          Reflect.apply(schedulerClearMethod, scheduler, [handle]);
        } catch {
          // A synchronously-settled deadline remains inert through the terminal latch.
        }
      }
    } catch {
      deadlineState = undefined;
      deadlineHandle = undefined;
      fail('internal_error');
    }
  };

  const enter = (
    allowed: readonly RenderAttemptActiveState[],
    next: RenderAttemptActiveState
  ): boolean => {
    if (outcome !== undefined || !ownerIsCurrent() || !allowed.includes(state as never)) {
      return false;
    }
    state = next;
    history.push(next);
    clearDeadline();
    if (outcome === undefined) armDeadline(next);
    return true;
  };

  const stageAndEnter = (
    allowed: readonly RenderAttemptActiveState[],
    next: 'waiting_for_document' | 'waiting_for_adm',
    artifact: CommittedRenderArtifact
  ): boolean => {
    if (
      pendingArtifact !== undefined ||
      !validArtifact(artifact, slot, navigationGeneration, id) ||
      outcome !== undefined ||
      !ownerIsCurrent() ||
      !allowed.includes(state as never)
    ) {
      return false;
    }
    pendingArtifact = artifact;
    if (enter(allowed, next)) return true;
    pendingArtifact = undefined;
    return false;
  };

  const lifecycle: RenderAttempt = {
    id,
    slot,
    generation,
    navigationGeneration,
    parentAttemptId,
    get renderSource(): ReservationRenderSource | undefined {
      return admittedRenderSource;
    },
    get winnerContext(): WinnerContext | undefined {
      return admittedWinnerContext;
    },
    admitDirectWinner,
    admitClaimedWinner,
    beginGamClaim: () => enter(['created'], 'waiting_for_gam_and_claim'),
    ownerClaimed: () =>
      admittedRenderSource && admittedWinnerContext
        ? enter(['waiting_for_gam_and_claim'], 'waiting_for_owner')
        : false,
    ownerRegistered: () => enter(['waiting_for_owner'], 'waiting_for_insertion'),
    beginDirect: () =>
      admittedRenderSource && admittedWinnerContext
        ? enter(['created'], 'rendering_direct')
        : false,
    beginApsDocument: (artifact) =>
      stageAndEnter(
        ['waiting_for_insertion', 'rendering_direct'],
        'waiting_for_document',
        artifact
      ),
    beginAdm: (artifact) =>
      stageAndEnter(['waiting_for_insertion', 'rendering_direct'], 'waiting_for_adm', artifact),
    apsDocumentAccepted: () => enter(['waiting_for_document'], 'waiting_for_aps_completion'),
    accept(): boolean {
      if (
        outcome !== undefined ||
        !ownerIsCurrent() ||
        (state !== 'waiting_for_aps_completion' && state !== 'waiting_for_adm') ||
        !pendingArtifact
      ) {
        return false;
      }
      clearDeadline();
      const candidate = pendingArtifact;
      let promoted: boolean;
      try {
        promoted = Reflect.apply(promoteArtifactMethod, artifacts, [
          candidate,
          () =>
            outcome === undefined &&
            pendingArtifact === candidate &&
            (state === 'waiting_for_aps_completion' || state === 'waiting_for_adm') &&
            ownerIsCurrent(),
        ]);
        promoted =
          promoted && Reflect.apply(currentArtifactMethod, artifacts, [slot]) === candidate;
        if (
          promoted &&
          (outcome !== undefined ||
            pendingArtifact !== candidate ||
            (state !== 'waiting_for_aps_completion' && state !== 'waiting_for_adm') ||
            !ownerIsCurrent())
        ) {
          Reflect.apply(releaseArtifactMethod, artifacts, [candidate]);
          promoted = false;
        }
      } catch {
        promoted = false;
      }
      if (!promoted) {
        fail('internal_error');
        return false;
      }
      pendingArtifact = undefined;
      return settle(frozen({ outcome: 'accepted' }), true);
    },
    noBid: () =>
      state === 'created' &&
      admittedRenderSource === undefined &&
      admittedWinnerContext === undefined &&
      ownerIsCurrent()
        ? settle(frozen({ outcome: 'no_bid' }), true)
        : false,
    fail,
    cancel: (reason) =>
      validCancellationReason(reason)
        ? settle(frozen({ outcome: 'cancelled', reason }), true)
        : false,
    onSettled(callback): boolean {
      if (typeof callback !== 'function') return false;
      if (outcome) {
        try {
          callback(outcome);
        } catch {
          // Observation cannot change the terminal result.
        }
        return true;
      }
      observers.push(callback);
      return true;
    },
    snapshot: () =>
      frozen({
        history: frozen(history.slice()),
        outcome,
        state,
      }),
  };

  try {
    Reflect.apply(ownerOnDisposeMethod, owner, [
      'render-lifecycle',
      () => {
        if (!settlingInternally && outcome === undefined) {
          settle(frozen({ outcome: 'cancelled', reason: 'navigation_disposed' }), false);
        }
      },
    ]);
  } catch {
    return frozen({ ok: false, reason: 'stale_owner' });
  }
  if (!ownerIsCurrent()) {
    settle(frozen({ outcome: 'cancelled', reason: 'navigation_disposed' }), false);
    return frozen({ ok: false, reason: 'stale_owner' });
  }
  return frozen({ ok: true, value: frozen(lifecycle) });
}

/** Own one public per-slot result without overwriting either child attempt result. */
export function createSlotOperation(options: SlotOperationOptions): SlotOperation {
  const observers: Array<(result: SlotOperationResult) => void> = [];
  const primary = options.primary;
  const primaryId = primary.id;
  const primarySlot = primary.slot;
  const primaryNavigationGeneration = primary.navigationGeneration;
  const primaryOnSettledMethod = primary.onSettled;
  const primarySnapshotMethod = primary.snapshot;
  const createFallback = options.createFallback;
  let result: SlotOperationResult | undefined;

  const settle = (terminal: SlotOperationResult): boolean => {
    if (result) return false;
    result = frozen(terminal);
    const snapshot = observers.splice(0, observers.length);
    for (const observer of snapshot) {
      try {
        observer(result);
      } catch {
        // Observation cannot change the public result.
      }
    }
    return true;
  };

  const settleFallbackFailure = (primary: RenderOutcome, reason: RenderFailureReason): void => {
    settle({
      path: 'fallback',
      outcome: frozen({ outcome: 'failed', reason }),
      primaryAttemptId: primaryId,
      primary,
    });
  };

  const beginFallback = (primary: RenderOutcome): void => {
    let created: RenderAttemptCreationResult;
    try {
      created =
        (createFallback ? Reflect.apply(createFallback, options, [primaryId]) : undefined) ??
        frozen({ ok: false, reason: 'stale_owner' });
    } catch {
      settleFallbackFailure(primary, 'internal_error');
      return;
    }
    let createdOk: boolean;
    try {
      createdOk = created.ok === true;
    } catch {
      settleFallbackFailure(primary, 'internal_error');
      return;
    }
    if (!createdOk) {
      let reason: 'identity_generation_failed' | 'invalid_attempt' | 'stale_owner';
      try {
        reason = (created as Extract<RenderAttemptCreationResult, { ok: false }>).reason;
      } catch {
        settleFallbackFailure(primary, 'internal_error');
        return;
      }
      settleFallbackFailure(
        primary,
        reason === 'identity_generation_failed' ? 'identity_generation_failed' : 'internal_error'
      );
      return;
    }
    let child: RenderAttempt;
    let childId: string;
    let childOnSettledMethod: RenderAttempt['onSettled'];
    let childCancelMethod: RenderAttempt['cancel'];
    try {
      child = (created as Extract<RenderAttemptCreationResult, { ok: true }>).value;
      childId = child.id;
      childOnSettledMethod = child.onSettled;
      childCancelMethod = child.cancel;
      if (
        !validAttemptId(childId) ||
        childId === primaryId ||
        child.slot !== primarySlot ||
        child.parentAttemptId !== primaryId ||
        child.navigationGeneration !== primaryNavigationGeneration ||
        typeof childOnSettledMethod !== 'function' ||
        typeof childCancelMethod !== 'function'
      ) {
        throw new TypeError('invalid fallback child');
      }
    } catch {
      try {
        const candidate = (created as Partial<Extract<RenderAttemptCreationResult, { ok: true }>>)
          .value as Partial<RenderAttempt> | undefined;
        if (candidate && typeof candidate.cancel === 'function') {
          Reflect.apply(candidate.cancel, candidate, ['superseded']);
        }
      } catch {
        // Invalid fallback cleanup cannot change the operation result.
      }
      settleFallbackFailure(primary, 'internal_error');
      return;
    }
    try {
      const subscribed = Reflect.apply(childOnSettledMethod, child, [
        (fallbackOutcome: RenderOutcome) => {
          if (!validOutcome(fallbackOutcome)) {
            settleFallbackFailure(primary, 'internal_error');
            return;
          }
          settle({
            path: 'fallback',
            outcome: fallbackOutcome,
            primaryAttemptId: primaryId,
            primary,
            fallbackAttemptId: childId,
            fallback: fallbackOutcome,
          });
        },
      ]);
      if (subscribed !== true && !result) settleFallbackFailure(primary, 'internal_error');
    } catch {
      settleFallbackFailure(primary, 'internal_error');
    }
  };

  try {
    const subscribed = Reflect.apply(primaryOnSettledMethod, primary, [
      (primaryOutcome: RenderOutcome) => {
        if (!validOutcome(primaryOutcome)) {
          const internal = frozen({
            outcome: 'failed' as const,
            reason: 'internal_error' as const,
          });
          settle({
            path: 'primary',
            outcome: internal,
            primaryAttemptId: primaryId,
            primary: internal,
          });
          return;
        }
        let attributableGamEmpty = false;
        if (
          primaryOutcome.outcome === 'failed' &&
          primaryOutcome.reason === 'gam_empty' &&
          typeof createFallback === 'function'
        ) {
          try {
            const snapshot = Reflect.apply(primarySnapshotMethod, primary, []);
            attributableGamEmpty =
              snapshot.outcome === primaryOutcome &&
              snapshot.state === 'failed' &&
              snapshot.history[snapshot.history.length - 2] === 'waiting_for_gam_and_claim';
          } catch {
            attributableGamEmpty = false;
          }
        }
        if (attributableGamEmpty) {
          beginFallback(primaryOutcome);
          return;
        }
        settle({
          path: 'primary',
          outcome: primaryOutcome,
          primaryAttemptId: primaryId,
          primary: primaryOutcome,
        });
      },
    ]);
    if (subscribed !== true && !result) {
      const internal = frozen({ outcome: 'failed' as const, reason: 'internal_error' as const });
      settle({
        path: 'primary',
        outcome: internal,
        primaryAttemptId: primaryId,
        primary: internal,
      });
    }
  } catch {
    const internal = frozen({ outcome: 'failed' as const, reason: 'internal_error' as const });
    settle({
      path: 'primary',
      outcome: internal,
      primaryAttemptId: primaryId,
      primary: internal,
    });
  }

  return frozen({
    snapshot: (): SlotOperationSnapshot =>
      result ? frozen({ settled: true, result }) : frozen({ settled: false }),
    onSettled(callback: (terminal: SlotOperationResult) => void): boolean {
      if (typeof callback !== 'function') return false;
      if (result) {
        try {
          callback(result);
        } catch {
          // Observation cannot change the public result.
        }
      } else {
        observers.push(callback);
      }
      return true;
    },
  });
}
