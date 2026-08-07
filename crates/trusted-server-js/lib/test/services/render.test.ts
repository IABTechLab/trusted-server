import { describe, expect, it, vi } from 'vitest';

import type { RenderAttemptScope, WinnerContext } from '../../src/kernel/sessions';
import {
  createCommittedArtifactStore,
  createRenderAttempt,
  createSlotOperation,
  type CommittedRenderArtifact,
  type RenderAttempt,
  type RenderAttemptState,
} from '../../src/services/render';

const ATTEMPT_ONE = 'a1_0000000000000000000000';
const ATTEMPT_TWO = 'a1_0000000000000000000001';

const ADM_SOURCE = Object.freeze({
  type: 'adm' as const,
  version: 1 as const,
  adm: '<div>fictional creative</div>',
  width: 300,
  height: 250,
});

const APS_SOURCE = Object.freeze({
  type: 'aps' as const,
  version: 1 as const,
  accountId: 'fictional-account',
  bidId: 'fictional-bid',
  tagType: 'iframe' as const,
  creativeUrl: 'https://creative.example/render',
  width: 300,
  height: 250,
  aaxResponse: 'e30=',
});

const WINNER_CONTEXT = Object.freeze({ selectedCpm: 1 });

function prepareRenderSource(candidate: unknown) {
  if (candidate === ADM_SOURCE) return ADM_SOURCE;
  if (candidate === APS_SOURCE) return APS_SOURCE;
  return undefined;
}

type TestOwner = RenderAttemptScope & {
  admitClaimedContext(context: WinnerContext): void;
  disposeFromNavigation(): void;
};

function owner(
  id = ATTEMPT_ONE,
  slot = 'fictional-slot',
  navigationGeneration = Object.freeze({})
): TestOwner {
  let current = true;
  let disposed = false;
  let winnerContext: WinnerContext | undefined;
  const callbacks: Array<() => void> = [];
  const controller = new AbortController();
  const scope = {
    id,
    slot,
    generation: Object.freeze({}),
    navigationGeneration,
    interfaces: Object.freeze({}),
    get disposed() {
      return disposed;
    },
    get signal() {
      return controller.signal;
    },
    get winnerContext() {
      return winnerContext;
    },
    capture:
      <Arguments extends readonly unknown[]>(callback: (...arguments_: Arguments) => unknown) =>
      (...arguments_: Arguments): boolean => {
        if (!scope.isCurrent()) return false;
        callback(...arguments_);
        return true;
      },
    isCurrent: () => current && !disposed,
    prepareWinnerContext: (context: WinnerContext) => {
      if (!scope.isCurrent() || winnerContext !== undefined) return undefined;
      let committed = false;
      return Object.freeze({
        commit: () => {
          if (committed) return winnerContext === context;
          if (!scope.isCurrent() || winnerContext !== undefined) return false;
          winnerContext = context;
          committed = true;
          return true;
        },
        rollback: () => {
          if (committed && winnerContext === context) winnerContext = undefined;
          committed = false;
          return winnerContext === undefined;
        },
      });
    },
    onDispose: (_kind: string, callback: () => void) => {
      callbacks.push(callback);
    },
    dispose: () => {
      if (disposed) return;
      disposed = true;
      controller.abort();
      for (let index = callbacks.length - 1; index >= 0; index -= 1) callbacks[index]?.();
    },
    disposeFromNavigation: () => {
      current = false;
      scope.dispose();
    },
    admitClaimedContext: (context: WinnerContext) => {
      winnerContext = context;
    },
  } satisfies TestOwner;
  return scope;
}

function artifact(
  render: Pick<RenderAttemptScope, 'id' | 'slot' | 'navigationGeneration'>,
  kind: CommittedRenderArtifact['kind'] = 'direct_iframe'
): CommittedRenderArtifact & { dispose: ReturnType<typeof vi.fn> } {
  return Object.freeze({
    kind,
    attemptId: render.id,
    slot: render.slot,
    navigationGeneration: render.navigationGeneration,
    dispose: vi.fn(),
  });
}

function attempt(
  scope = owner(),
  options: Partial<Parameters<typeof createRenderAttempt>[0]> = {}
): RenderAttempt {
  const result = createRenderAttempt({
    artifacts: options.artifacts ?? createCommittedArtifactStore(),
    owner: scope,
    prepareRenderSource: options.prepareRenderSource ?? prepareRenderSource,
    ...(options.parentAttemptId === undefined ? {} : { parentAttemptId: options.parentAttemptId }),
    ...(options.scheduler === undefined ? {} : { scheduler: options.scheduler }),
  });
  expect(result).toMatchObject({ ok: true });
  if (!result.ok) throw new Error('should create an attempt');
  return result.value;
}

describe('RenderAttempt state machine', () => {
  it('implements the exact PUC APS state table and makes invalid/replay transitions inert', () => {
    const scope = owner();
    const candidate = artifact(scope);
    const render = attempt(scope);
    const observed: RenderAttemptState[] = [];

    expect(render.beginGamClaim()).toBe(true);
    expect(render.beginDirect()).toBe(false);
    scope.admitClaimedContext(WINNER_CONTEXT);
    expect(render.admitClaimedWinner(APS_SOURCE)).toBe(true);
    expect(render.ownerClaimed()).toBe(true);
    expect(render.ownerRegistered()).toBe(true);
    expect(render.beginApsDocument(candidate)).toBe(true);
    expect(render.beginAdm(candidate)).toBe(false);
    expect(render.apsDocumentAccepted()).toBe(true);
    expect(render.accept()).toBe(true);
    expect(render.accept()).toBe(false);
    expect(render.fail('runner_failed')).toBe(false);
    expect(candidate.dispose).not.toHaveBeenCalled();

    for (const state of render.snapshot().history) observed.push(state);
    expect(observed).toEqual([
      'created',
      'waiting_for_gam_and_claim',
      'waiting_for_owner',
      'waiting_for_insertion',
      'waiting_for_document',
      'waiting_for_aps_completion',
      'accepted',
    ]);
    expect(render.snapshot()).toMatchObject({
      state: 'accepted',
      outcome: { outcome: 'accepted' },
    });
    expect(scope.disposed).toBe(true);
  });

  it('implements direct and owner ADM paths without permitting APS-only transitions', () => {
    const directOwner = owner();
    const directArtifact = artifact(directOwner);
    const direct = attempt(directOwner);
    expect(direct.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT)).toBe(true);
    expect(direct.beginDirect()).toBe(true);
    expect(direct.beginAdm(directArtifact)).toBe(true);
    expect(direct.apsDocumentAccepted()).toBe(false);
    expect(direct.accept()).toBe(true);

    const pucOwner = owner(ATTEMPT_TWO);
    const pucArtifact = artifact(pucOwner, 'puc');
    const puc = attempt(pucOwner);
    expect(puc.beginGamClaim()).toBe(true);
    pucOwner.admitClaimedContext(WINNER_CONTEXT);
    expect(puc.admitClaimedWinner(ADM_SOURCE)).toBe(true);
    expect(puc.ownerClaimed()).toBe(true);
    expect(puc.ownerRegistered()).toBe(true);
    expect(puc.beginAdm(pucArtifact)).toBe(true);
    expect(puc.accept()).toBe(true);
    expect(puc.snapshot().history).toEqual([
      'created',
      'waiting_for_gam_and_claim',
      'waiting_for_owner',
      'waiting_for_insertion',
      'waiting_for_adm',
      'accepted',
    ]);
  });

  it('owns the exact admitted source and winner context for a direct APS path', () => {
    const scope = owner();
    const render = attempt(scope);
    expect(render.beginDirect()).toBe(false);
    expect(render.admitDirectWinner(APS_SOURCE, WINNER_CONTEXT)).toBe(true);
    expect(render.renderSource).toBe(APS_SOURCE);
    expect(render.winnerContext).toBe(WINNER_CONTEXT);
    expect(render.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT)).toBe(false);

    const candidate = artifact(scope);
    expect(render.beginDirect()).toBe(true);
    expect(render.beginApsDocument(candidate)).toBe(true);
    expect(render.apsDocumentAccepted()).toBe(true);
    expect(render.accept()).toBe(true);
    expect(render.renderSource).toBeUndefined();
    expect(render.winnerContext).toBeUndefined();
  });

  it('allows no_bid only for the exact parsed decision before rendering starts', () => {
    const noBid = attempt();
    expect(noBid.noBid()).toBe(true);
    expect(noBid.snapshot()).toMatchObject({ state: 'no_bid', outcome: { outcome: 'no_bid' } });
    expect(noBid.beginDirect()).toBe(false);

    const rendering = attempt(owner(ATTEMPT_TWO));
    expect(rendering.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT)).toBe(true);
    expect(rendering.beginDirect()).toBe(true);
    expect(rendering.noBid()).toBe(false);
    expect(rendering.fail('invalid_response')).toBe(true);
  });

  it('races state-owned timeout, success, failure, abort, and navigation disposal through one latch', () => {
    vi.useFakeTimers();
    try {
      const timedOwner = owner();
      const timedArtifact = artifact(timedOwner);
      const timed = attempt(timedOwner, {
        owner: timedOwner,
        artifacts: createCommittedArtifactStore(),
      });
      expect(timed.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT)).toBe(true);
      expect(timed.beginDirect()).toBe(true);
      expect(timed.beginAdm(timedArtifact)).toBe(true);
      vi.advanceTimersByTime(5_000);
      expect(timed.snapshot()).toMatchObject({
        outcome: { outcome: 'failed', reason: 'adm_document_no_load' },
      });
      expect(timedArtifact.dispose).toHaveBeenCalledOnce();
      expect(timed.accept()).toBe(false);
      expect(timed.cancel('caller_aborted')).toBe(false);

      const aborted = attempt(owner(ATTEMPT_TWO));
      expect(aborted.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT)).toBe(true);
      expect(aborted.beginDirect()).toBe(true);
      expect(aborted.cancel('caller_aborted')).toBe(true);
      expect(aborted.fail('internal_error')).toBe(false);

      const navigationOwner = owner('a1_0000000000000000000002');
      const navigationAttempt = attempt(navigationOwner);
      expect(navigationAttempt.beginGamClaim()).toBe(true);
      navigationOwner.disposeFromNavigation();
      expect(navigationAttempt.snapshot()).toMatchObject({
        outcome: { outcome: 'cancelled', reason: 'navigation_disposed' },
      });
    } finally {
      vi.useRealTimers();
    }
  });

  it('uses fixed transition-owned deadline timings and failure mappings', () => {
    vi.useFakeTimers();
    try {
      const registrationOwner = owner();
      const registration = attempt(registrationOwner);
      registration.beginGamClaim();
      registrationOwner.admitClaimedContext(WINNER_CONTEXT);
      registration.admitClaimedWinner(APS_SOURCE);
      registration.ownerClaimed();
      vi.advanceTimersByTime(2_999);
      expect(registration.snapshot().state).toBe('waiting_for_owner');
      vi.advanceTimersByTime(1);
      expect(registration.snapshot().outcome).toEqual({
        outcome: 'failed',
        reason: 'owner_registration_timeout',
      });

      const insertionOwner = owner(ATTEMPT_TWO);
      const insertion = attempt(insertionOwner);
      insertion.beginGamClaim();
      insertionOwner.admitClaimedContext(WINNER_CONTEXT);
      insertion.admitClaimedWinner(APS_SOURCE);
      insertion.ownerClaimed();
      insertion.ownerRegistered();
      vi.advanceTimersByTime(1_000);
      expect(insertion.snapshot().outcome).toEqual({
        outcome: 'failed',
        reason: 'owner_insertion_timeout',
      });

      const documentOwner = owner('a1_0000000000000000000002');
      const documentAttempt = attempt(documentOwner);
      documentAttempt.admitDirectWinner(APS_SOURCE, WINNER_CONTEXT);
      documentAttempt.beginDirect();
      documentAttempt.beginApsDocument(artifact(documentOwner));
      vi.advanceTimersByTime(3_000);
      expect(documentAttempt.snapshot().outcome).toEqual({
        outcome: 'failed',
        reason: 'renderer_document_no_load',
      });

      const completionOwner = owner('a1_0000000000000000000003');
      const completion = attempt(completionOwner);
      completion.admitDirectWinner(APS_SOURCE, WINNER_CONTEXT);
      completion.beginDirect();
      completion.beginApsDocument(artifact(completionOwner));
      completion.apsDocumentAccepted();
      vi.advanceTimersByTime(10_000);
      expect(completion.snapshot().outcome).toEqual({
        outcome: 'failed',
        reason: 'runner_failed',
      });
    } finally {
      vi.useRealTimers();
    }
  });

  it('reserves transition and terminal latches before hostile scheduler and artifact cleanup', () => {
    const transitionReference: { current?: RenderAttempt } = {};
    let clearReenters = false;
    const scheduler = {
      set: vi.fn(() => Object.freeze({})),
      clear: vi.fn(() => {
        if (clearReenters) transitionReference.current?.cancel('caller_aborted');
      }),
    };
    const transitionOwner = owner();
    const transitionAttempt = attempt(transitionOwner, { scheduler });
    transitionReference.current = transitionAttempt;
    transitionAttempt.admitDirectWinner(APS_SOURCE, WINNER_CONTEXT);
    transitionAttempt.beginDirect();
    transitionAttempt.beginApsDocument(artifact(transitionOwner));
    clearReenters = true;

    expect(transitionAttempt.apsDocumentAccepted()).toBe(true);
    expect(transitionAttempt.snapshot().outcome).toEqual({
      outcome: 'cancelled',
      reason: 'caller_aborted',
    });
    expect(transitionAttempt.snapshot().history.slice(-2)).toEqual([
      'waiting_for_aps_completion',
      'cancelled',
    ]);

    const disposalReference: { current?: RenderAttempt } = {};
    const disposalOwner = owner(ATTEMPT_TWO);
    const hostileArtifact = Object.freeze({
      kind: 'direct_iframe' as const,
      attemptId: disposalOwner.id,
      slot: disposalOwner.slot,
      navigationGeneration: disposalOwner.navigationGeneration,
      dispose: vi.fn(() => disposalReference.current?.cancel('superseded')),
    });
    const disposalAttempt = attempt(disposalOwner);
    disposalReference.current = disposalAttempt;
    disposalAttempt.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT);
    disposalAttempt.beginDirect();
    disposalAttempt.beginAdm(hostileArtifact);

    expect(disposalAttempt.fail('internal_error')).toBe(true);
    expect(disposalAttempt.snapshot().outcome).toEqual({
      outcome: 'failed',
      reason: 'internal_error',
    });
    expect(disposalAttempt.snapshot().history.filter((state) => state === 'failed')).toHaveLength(
      1
    );
    expect(disposalAttempt.snapshot().history).not.toContain('cancelled');
  });

  it('rejects malformed or stale attempt ownership before registering work', () => {
    const malformed = owner('bad-attempt');
    expect(
      createRenderAttempt({
        owner: malformed,
        artifacts: createCommittedArtifactStore(),
        prepareRenderSource,
      })
    ).toEqual({ ok: false, reason: 'invalid_attempt' });

    const stale = owner();
    stale.disposeFromNavigation();
    expect(
      createRenderAttempt({
        owner: stale,
        artifacts: createCommittedArtifactStore(),
        prepareRenderSource,
      })
    ).toEqual({ ok: false, reason: 'stale_owner' });
  });

  it('runtime-rejects invalid terminal reasons instead of publishing malformed outcomes', () => {
    const render = attempt();
    expect(render.fail('invented_failure' as never)).toBe(false);
    expect(render.cancel('invented_cancellation' as never)).toBe(false);
    expect(render.snapshot()).toMatchObject({ state: 'created', outcome: undefined });
    expect(render.fail('internal_error')).toBe(true);
  });
});

describe('committed artifact ownership', () => {
  it('promotes before attempt disposal, preserves accepted DOM, and disposes the prior artifact before replacement', () => {
    const store = createCommittedArtifactStore();
    const generation = Object.freeze({});
    const firstOwner = owner(ATTEMPT_ONE, 'fictional-slot', generation);
    const firstArtifact = artifact(firstOwner);
    const first = attempt(firstOwner, { owner: firstOwner, artifacts: store });
    first.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT);
    first.beginDirect();
    first.beginAdm(firstArtifact);
    expect(first.accept()).toBe(true);
    expect(firstOwner.disposed).toBe(true);
    expect(firstArtifact.dispose).not.toHaveBeenCalled();
    expect(store.current('fictional-slot')).toBe(firstArtifact);

    const secondOwner = owner(ATTEMPT_TWO, 'fictional-slot', generation);
    const secondArtifact = artifact(secondOwner);
    const second = attempt(secondOwner, { owner: secondOwner, artifacts: store });
    second.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT);
    second.beginDirect();
    second.beginAdm(secondArtifact);
    expect(second.accept()).toBe(true);
    expect(firstArtifact.dispose).toHaveBeenCalledOnce();
    expect(store.current('fictional-slot')).toBe(secondArtifact);
    expect(secondArtifact.dispose).not.toHaveBeenCalled();

    store.disposeNavigation(generation);
    expect(secondArtifact.dispose).toHaveBeenCalledOnce();
    expect(store.current('fictional-slot')).toBeUndefined();
  });

  it('disposes only uncommitted artifacts on failure or cancellation', () => {
    for (const [index, settle] of (['failed', 'cancelled'] as const).entries()) {
      const scope = owner(`a1_000000000000000000000${index}`);
      const candidate = artifact(scope);
      const render = attempt(scope);
      render.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT);
      render.beginDirect();
      render.beginAdm(candidate);
      if (settle === 'failed') expect(render.fail('adm_document_no_load')).toBe(true);
      else expect(render.cancel('superseded')).toBe(true);
      expect(candidate.dispose).toHaveBeenCalledOnce();
    }
  });

  it('does not publish a replacement when prior-artifact disposal reentrantly cancels it', () => {
    const store = createCommittedArtifactStore();
    const generation = Object.freeze({});
    const secondOwner = owner(ATTEMPT_TWO, 'fictional-slot', generation);
    const firstOwner = owner(ATTEMPT_ONE, 'fictional-slot', generation);
    const firstArtifact = Object.freeze({
      kind: 'direct_iframe' as const,
      attemptId: firstOwner.id,
      slot: 'fictional-slot',
      navigationGeneration: generation,
      dispose: vi.fn(() => secondOwner.disposeFromNavigation()),
    });
    const first = attempt(firstOwner, { owner: firstOwner, artifacts: store });
    first.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT);
    first.beginDirect();
    first.beginAdm(firstArtifact);
    first.accept();

    const secondArtifact = artifact(secondOwner);
    const second = attempt(secondOwner, { owner: secondOwner, artifacts: store });
    second.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT);
    second.beginDirect();
    second.beginAdm(secondArtifact);

    expect(second.accept()).toBe(false);
    expect(second.snapshot()).toMatchObject({
      outcome: { outcome: 'cancelled', reason: 'navigation_disposed' },
    });
    expect(firstArtifact.dispose).toHaveBeenCalledOnce();
    expect(secondArtifact.dispose).toHaveBeenCalledOnce();
    expect(store.current('fictional-slot')).toBeUndefined();
  });

  it('requires an immutable exact-attempt artifact without invoking accessors', () => {
    const scope = owner();
    const render = attempt(scope);
    render.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT);
    render.beginDirect();
    const wrongAttempt = Object.freeze({
      ...artifact(scope),
      attemptId: ATTEMPT_TWO,
    });
    expect(render.beginAdm(wrongAttempt)).toBe(false);

    const getter = vi.fn(() => 'direct_iframe');
    const hostile = Object.freeze(
      Object.defineProperties(
        {},
        {
          attemptId: { enumerable: true, value: scope.id },
          dispose: { enumerable: true, value: vi.fn() },
          kind: { enumerable: true, get: getter },
          navigationGeneration: { enumerable: true, value: scope.navigationGeneration },
          slot: { enumerable: true, value: scope.slot },
        }
      )
    );
    expect(render.beginAdm(hostile as CommittedRenderArtifact)).toBe(false);
    expect(getter).not.toHaveBeenCalled();
  });

  it('defers reentrant navigation disposal and never publishes into a disposed generation', () => {
    const store = createCommittedArtifactStore();
    const generation = Object.freeze({});
    const firstOwner = owner(ATTEMPT_ONE, 'slot-one', generation);
    const secondOwner = owner(ATTEMPT_TWO, 'slot-two', generation);
    const replacementOwner = owner('a1_0000000000000000000002', 'slot-one', generation);
    const first = Object.freeze({
      kind: 'direct_iframe' as const,
      attemptId: firstOwner.id,
      slot: firstOwner.slot,
      navigationGeneration: generation,
      dispose: vi.fn(() => store.disposeNavigation(generation)),
    });
    const second = artifact(secondOwner);
    const replacement = artifact(replacementOwner);
    expect(store.promote(first)).toBe(true);
    expect(store.promote(second)).toBe(true);

    expect(store.promote(replacement)).toBe(false);
    expect(first.dispose).toHaveBeenCalledOnce();
    expect(second.dispose).toHaveBeenCalledOnce();
    expect(store.current('slot-one')).toBeUndefined();
    expect(store.current('slot-two')).toBeUndefined();
  });

  it('never retries a throwing artifact disposer', () => {
    const store = createCommittedArtifactStore();
    const generation = Object.freeze({});
    const firstOwner = owner(ATTEMPT_ONE, 'fictional-slot', generation);
    const replacementOwner = owner(ATTEMPT_TWO, 'fictional-slot', generation);
    const dispose = vi.fn(() => {
      throw new Error('partial artifact disposal');
    });
    const first = Object.freeze({
      kind: 'direct_iframe' as const,
      attemptId: firstOwner.id,
      slot: firstOwner.slot,
      navigationGeneration: generation,
      dispose,
    });
    expect(store.promote(first)).toBe(true);
    expect(store.promote(artifact(replacementOwner))).toBe(false);
    expect(store.current('fictional-slot')).toBe(first);

    store.disposeNavigation(generation);
    expect(dispose).toHaveBeenCalledOnce();
    expect(store.current('fictional-slot')).toBeUndefined();
  });
});

describe('SlotOperation result isolation', () => {
  it('retains immutable primary gam_empty and settles from one distinct fallback child', () => {
    const primary = attempt();
    let fallback: RenderAttempt | undefined;
    const operation = createSlotOperation({
      primary,
      createFallback: (parentAttemptId) => {
        const childOwner = owner(ATTEMPT_TWO, primary.slot, primary.navigationGeneration);
        const result = createRenderAttempt({
          owner: childOwner,
          artifacts: createCommittedArtifactStore(),
          prepareRenderSource,
          parentAttemptId,
        });
        if (result.ok) fallback = result.value;
        return result;
      },
    });

    primary.beginGamClaim();
    expect(primary.fail('gam_empty')).toBe(true);
    expect(fallback).toBeDefined();
    expect(fallback?.parentAttemptId).toBe(primary.id);
    expect(fallback?.id).not.toBe(primary.id);
    expect(fallback?.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT)).toBe(true);
    fallback?.beginDirect();
    const fallbackArtifact = artifact(fallback!);
    fallback?.beginAdm(fallbackArtifact);
    expect(fallback?.accept()).toBe(true);

    expect(operation.snapshot()).toEqual({
      settled: true,
      result: {
        path: 'fallback',
        outcome: { outcome: 'accepted' },
        primaryAttemptId: ATTEMPT_ONE,
        primary: { outcome: 'failed', reason: 'gam_empty' },
        fallbackAttemptId: ATTEMPT_TWO,
        fallback: { outcome: 'accepted' },
      },
    });
    expect(Object.isFrozen(operation.snapshot().result)).toBe(true);
  });

  it('does not start fallback for ineligible primary results or settle twice', () => {
    const primary = attempt();
    const createFallback = vi.fn();
    const operation = createSlotOperation({ primary, createFallback });
    primary.admitDirectWinner(ADM_SOURCE, WINNER_CONTEXT);
    primary.beginDirect();
    primary.fail('runner_failed');

    expect(createFallback).not.toHaveBeenCalled();
    expect(operation.snapshot()).toMatchObject({
      settled: true,
      result: {
        path: 'primary',
        outcome: { outcome: 'failed', reason: 'runner_failed' },
      },
    });
    expect(primary.cancel('superseded')).toBe(false);
  });

  it('cannot forge fallback with gam_empty outside an attributable GAM state', () => {
    const primary = attempt();
    const createFallback = vi.fn();
    const operation = createSlotOperation({ primary, createFallback });

    expect(primary.fail('gam_empty')).toBe(false);
    expect(createFallback).not.toHaveBeenCalled();
    expect(operation.snapshot()).toEqual({ settled: false });
    expect(primary.cancel('caller_aborted')).toBe(true);
  });

  it('rejects a fallback child from another navigation generation', () => {
    const primary = attempt();
    let child: RenderAttempt | undefined;
    const operation = createSlotOperation({
      primary,
      createFallback: (parentAttemptId) => {
        const result = createRenderAttempt({
          owner: owner(ATTEMPT_TWO),
          artifacts: createCommittedArtifactStore(),
          prepareRenderSource,
          parentAttemptId,
        });
        if (result.ok) child = result.value;
        return result;
      },
    });
    primary.beginGamClaim();
    primary.fail('gam_empty');

    expect(child?.snapshot().outcome).toEqual({
      outcome: 'cancelled',
      reason: 'superseded',
    });
    expect(operation.snapshot()).toMatchObject({
      settled: true,
      result: {
        path: 'fallback',
        outcome: { outcome: 'failed', reason: 'internal_error' },
      },
    });
  });

  it('fails closed when fallback identity issuance fails', () => {
    const primary = attempt();
    const operation = createSlotOperation({
      primary,
      createFallback: () => ({ ok: false, reason: 'identity_generation_failed' }),
    });
    primary.beginGamClaim();
    primary.fail('gam_empty');

    expect(operation.snapshot()).toMatchObject({
      settled: true,
      result: {
        path: 'fallback',
        primary: { outcome: 'failed', reason: 'gam_empty' },
        outcome: { outcome: 'failed', reason: 'identity_generation_failed' },
      },
    });
  });

  it('contains hostile fallback result getters and child subscription failures', () => {
    const getterPrimary = attempt();
    const getterOperation = createSlotOperation({
      primary: getterPrimary,
      createFallback: () =>
        Object.defineProperty({}, 'ok', {
          get: () => {
            throw new Error('hostile result getter');
          },
        }) as never,
    });
    getterPrimary.beginGamClaim();
    getterPrimary.fail('gam_empty');
    expect(getterOperation.snapshot()).toMatchObject({
      settled: true,
      result: { outcome: { outcome: 'failed', reason: 'internal_error' } },
    });

    const subscriptionPrimary = attempt(owner(ATTEMPT_ONE, 'fictional-slot', Object.freeze({})));
    const hostileChild = {
      id: ATTEMPT_TWO,
      slot: subscriptionPrimary.slot,
      parentAttemptId: subscriptionPrimary.id,
      navigationGeneration: subscriptionPrimary.navigationGeneration,
      cancel: vi.fn(() => true),
      onSettled: () => {
        throw new Error('hostile child subscription');
      },
    } as unknown as RenderAttempt;
    const subscriptionOperation = createSlotOperation({
      primary: subscriptionPrimary,
      createFallback: () => ({ ok: true, value: hostileChild }),
    });
    subscriptionPrimary.beginGamClaim();
    subscriptionPrimary.fail('gam_empty');
    expect(subscriptionOperation.snapshot()).toMatchObject({
      settled: true,
      result: { outcome: { outcome: 'failed', reason: 'internal_error' } },
    });
  });
});
