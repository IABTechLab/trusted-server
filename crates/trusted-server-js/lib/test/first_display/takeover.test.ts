import { describe, expect, it } from 'vitest';

import {
  coordinatePreparedFirstDisplayTakeoverV1,
  createFirstDisplayHandoffOwner,
  performFirstDisplayTakeoverV1,
  type FinalizedFirstDisplayHandoffV1,
} from '../../src/first_display/handoff';

const RELEASE_ID = 'a'.repeat(64);
const DIGEST = 'b'.repeat(64);
const RESERVATION_ID = `r1_${'a'.repeat(22)}`;

function handoff(revision = 0): Record<string, unknown> {
  return {
    version: 1,
    releaseId: RELEASE_ID,
    generation: 1,
    projectionDigest: DIGEST,
    slices: ['first_display', 'gpt_initial'],
    slots: [
      {
        id: 'slot-1',
        aliases: [],
        domId: 'div-1',
        gamPath: '/123/slot-1',
        formats: [[300, 250]],
        owner: 'trusted_server',
        outcome: 'accepted',
        targeting: [['hb_adid', RESERVATION_ID]],
        committedArtifact: 'gpt_adm',
        gptToken: 'gt1_1',
      },
    ],
    attempts: [
      {
        id: 'a1_AAECAwQFBgcAAAAAAAAAAQ',
        slotId: 'slot-1',
        ordinal: 1,
        state: 'accepted',
        reason: null,
      },
    ],
    tombstones: [],
    artifacts: [
      {
        slotId: 'slot-1',
        kind: 'gpt_adm',
        owner: 'trusted_server',
        token: RESERVATION_ID,
      },
    ],
    parserState: [
      {
        sliceId: 'gpt_initial',
        observations: ['protocol_version'],
        values: [['protocol_version', 1]],
      },
    ],
    gptDiagnostics: { facts: [], overflowCount: 0, dropCount: 0 },
    timing: { bidsScriptMs: 1, firstDisplayMs: 2, terminalMs: 3, paintMs: 4 },
    highWater: {
      navigationAttemptPrefix: 'AAECAwQFBgc',
      nextNavigationAttemptOrdinal: 2,
      nextAttemptOrdinal: 2,
      nextSlotRegistrationOrdinal: 2,
      reservationClockEpochMs: 0,
      nextReservationOrdinal: 2,
      nextTicketOrdinal: 1,
    },
    cycles: [
      {
        slotId: 'slot-1',
        token: 'gt1_1',
        nextCycleOrdinal: 2,
        unknownPriorCycle: false,
        records: [
          {
            ordinal: 1,
            responseIdentifier: 'response-one',
            seen: ['slotRequested', 'slotRenderEnded'],
            state: 'completed',
          },
        ],
        quarantines: [],
      },
    ],
    trace: {
      nextSequence: 2,
      nextGlobalSlotOrdinal: 2,
      slots: [
        {
          slotId: 'slot-1',
          impressions: 1,
          bindings: [
            {
              atMs: 3,
              cycleOrdinal: 1,
              historySequence: 1,
              state: 'completed',
              token: 'gt1_1',
            },
          ],
        },
      ],
    },
    mutationRevision: revision,
  };
}

function outline(): Record<string, unknown> {
  return {
    version: 1,
    releaseId: RELEASE_ID,
    generation: 1,
    projectionDigest: DIGEST,
    slices: ['first_display', 'gpt_initial'],
    slotCount: 1,
    outcomeCount: 1,
    capabilities: [],
    objectKinds: ['gpt_slot', 'dom_artifact'],
  };
}

function finalized(
  physicalSlot: object = {},
  artifact: object = {}
): FinalizedFirstDisplayHandoffV1 {
  const owner = createFirstDisplayHandoffOwner({
    releaseId: RELEASE_ID,
    generation: 1,
    isCurrentGeneration: () => true,
    isTerminal: () => true,
    isPainted: () => true,
    closeIngress: () => undefined,
    onFailure: () => undefined,
  });
  const value = owner.finalize(() => ({
    candidate: handoff(),
    identities: [physicalSlot, artifact],
  }));
  if (!value) throw new Error('should finalize test handoff');
  return value;
}

describe('atomic first-display takeover', () => {
  it('binds the exact handoff and one-use identities to the prepared persistent barrier', () => {
    const physicalSlot = {};
    const artifact = {};
    const events: string[] = [];
    let adoption: unknown;
    const prepared = Object.freeze({
      activate: (candidate?: unknown) => {
        adoption = candidate;
        events.push('activate');
      },
      commit: () => events.push('commit'),
      rollback: () => events.push('rollback'),
    });

    expect(
      coordinatePreparedFirstDisplayTakeoverV1({
        prepared,
        finalized: finalized(physicalSlot, artifact),
        outline: outline(),
        isCurrentGeneration: () => true,
        authenticateRuntimeScript: () => true,
        currentMutationRevision: () => 0,
        quiesceAgent: () => events.push('quiesce'),
        detachCommittedArtifacts: () => events.push('detach'),
        disposeAgent: () => events.push('dispose'),
        onFailure: () => events.push('fallback'),
      })
    ).toBe(true);
    expect(adoption).toMatchObject({
      version: 1,
      adoptInitialDisplay: true,
      identities: [physicalSlot, artifact],
      handoff: { releaseId: RELEASE_ID, generation: 1 },
    });
    expect(Object.isFrozen(adoption)).toBe(true);
    expect(events).toEqual(['quiesce', 'detach', 'dispose', 'activate', 'commit']);
  });

  it('transfers one-use identities and commits in the exact non-yielding order', () => {
    const events: string[] = [];
    const physicalSlot = {};
    const artifact = {};
    const result = performFirstDisplayTakeoverV1({
      finalized: finalized(physicalSlot, artifact),
      outline: outline(),
      isCurrentGeneration: () => true,
      authenticateRuntimeScript: () => true,
      currentMutationRevision: () => 0,
      quiesceAgent: () => events.push('quiesce-agent'),
      detachCommittedArtifacts: () => events.push('detach-artifacts'),
      disposeAgent: () => events.push('dispose-agent'),
      activatePersistent: (_snapshot, identities, own) => {
        expect(identities).toEqual([physicalSlot, artifact]);
        own(() => events.push('rollback-persistent'));
        events.push('activate-persistent');
      },
      commitPersistent: () => events.push('commit-persistent'),
      onFailure: () => events.push('fallback'),
    });

    expect(result).toBe(true);
    expect(events).toEqual([
      'quiesce-agent',
      'detach-artifacts',
      'dispose-agent',
      'activate-persistent',
      'commit-persistent',
    ]);
  });

  it('rolls back partial persistent effects without resurrecting the agent', () => {
    const events: string[] = [];
    const result = performFirstDisplayTakeoverV1({
      finalized: finalized(),
      outline: outline(),
      isCurrentGeneration: () => true,
      authenticateRuntimeScript: () => true,
      currentMutationRevision: () => 0,
      quiesceAgent: () => events.push('quiesce-agent'),
      detachCommittedArtifacts: () => events.push('detach-artifacts'),
      disposeAgent: () => events.push('dispose-agent'),
      activatePersistent: (_snapshot, _identities, own) => {
        own(() => events.push('rollback-a'));
        own(() => events.push('rollback-b'));
        throw new Error('activation failed');
      },
      commitPersistent: () => events.push('commit-persistent'),
      onFailure: () => events.push('fallback'),
    });

    expect(result).toBe(false);
    expect(events).toEqual([
      'quiesce-agent',
      'detach-artifacts',
      'dispose-agent',
      'rollback-b',
      'rollback-a',
      'fallback',
    ]);
  });

  it('rejects a mutation during quiesce or persistent activation', () => {
    for (const phase of ['quiesce', 'activate'] as const) {
      let revision = 0;
      const events: string[] = [];
      const result = performFirstDisplayTakeoverV1({
        finalized: finalized(),
        outline: outline(),
        isCurrentGeneration: () => true,
        authenticateRuntimeScript: () => true,
        currentMutationRevision: () => revision,
        quiesceAgent: () => {
          events.push('quiesce');
          if (phase === 'quiesce') revision += 1;
        },
        detachCommittedArtifacts: () => events.push('detach'),
        disposeAgent: () => events.push('dispose'),
        activatePersistent: (_snapshot, _identities, own) => {
          own(() => events.push('rollback'));
          if (phase === 'activate') revision += 1;
        },
        commitPersistent: () => events.push('commit'),
        onFailure: () => events.push('fallback'),
      });
      expect(result).toBe(false);
      expect(events).not.toContain('commit');
      expect(events[events.length - 1]).toBe('fallback');
    }
  });

  it('revalidates runtime authentication immediately before persistent commit', () => {
    let authenticated = true;
    const events: string[] = [];
    expect(
      performFirstDisplayTakeoverV1({
        finalized: finalized(),
        outline: outline(),
        isCurrentGeneration: () => true,
        authenticateRuntimeScript: () => authenticated,
        currentMutationRevision: () => 0,
        quiesceAgent: () => events.push('quiesce'),
        detachCommittedArtifacts: () => events.push('detach'),
        disposeAgent: () => events.push('dispose'),
        activatePersistent: (_snapshot, _identities, own) => {
          own(() => events.push('rollback'));
          events.push('activate');
          authenticated = false;
        },
        commitPersistent: () => events.push('commit'),
        onFailure: () => events.push('fallback'),
      })
    ).toBe(false);
    expect(events).toEqual(['quiesce', 'detach', 'dispose', 'activate', 'rollback', 'fallback']);
  });

  it('fails before quiesce for a stale outline, generation, or runtime script', () => {
    for (const failure of ['outline', 'generation', 'script'] as const) {
      const events: string[] = [];
      const candidate = outline();
      if (failure === 'outline') candidate.projectionDigest = 'c'.repeat(64);
      expect(
        performFirstDisplayTakeoverV1({
          finalized: finalized(),
          outline: candidate,
          isCurrentGeneration: () => failure !== 'generation',
          authenticateRuntimeScript: () => failure !== 'script',
          currentMutationRevision: () => 0,
          quiesceAgent: () => events.push('quiesce'),
          detachCommittedArtifacts: () => events.push('detach'),
          disposeAgent: () => events.push('dispose'),
          activatePersistent: () => events.push('activate'),
          commitPersistent: () => events.push('commit'),
          onFailure: () => events.push('fallback'),
        })
      ).toBe(false);
      expect(events).toEqual(['fallback']);
    }
  });

  it('performs full semantic handoff validation at takeover before any owner effect', () => {
    const owner = createFirstDisplayHandoffOwner({
      releaseId: RELEASE_ID,
      generation: 1,
      isCurrentGeneration: () => true,
      isTerminal: () => true,
      isPainted: () => true,
      closeIngress: () => undefined,
      onFailure: () => undefined,
    });
    const candidate = handoff();
    candidate.trace = { ...(candidate.trace as object), nextSequence: 1 };
    const sealed = owner.finalize(() => ({ candidate, identities: [{}, {}] }));
    expect(sealed).toBeDefined();

    const events: string[] = [];
    expect(
      performFirstDisplayTakeoverV1({
        finalized: sealed!,
        outline: outline(),
        isCurrentGeneration: () => true,
        authenticateRuntimeScript: () => true,
        currentMutationRevision: () => 0,
        quiesceAgent: () => events.push('quiesce'),
        detachCommittedArtifacts: () => events.push('detach'),
        disposeAgent: () => events.push('dispose'),
        activatePersistent: () => events.push('activate'),
        commitPersistent: () => events.push('commit'),
        onFailure: () => events.push('fallback'),
      })
    ).toBe(false);
    expect(events).toEqual(['fallback']);
  });

  it('rejects an outline that did not prepare every transferred object kind', () => {
    for (const objectKinds of [[], ['gpt_slot'], ['dom_artifact']] as const) {
      const events: string[] = [];
      expect(
        performFirstDisplayTakeoverV1({
          finalized: finalized(),
          outline: { ...outline(), objectKinds },
          isCurrentGeneration: () => true,
          authenticateRuntimeScript: () => true,
          currentMutationRevision: () => 0,
          quiesceAgent: () => events.push('quiesce'),
          detachCommittedArtifacts: () => events.push('detach'),
          disposeAgent: () => events.push('dispose'),
          activatePersistent: () => events.push('activate'),
          commitPersistent: () => events.push('commit'),
          onFailure: () => events.push('fallback'),
        })
      ).toBe(false);
      expect(events).toEqual(['fallback']);
    }
  });
});
