import { describe, expect, it } from 'vitest';

import {
  createFirstDisplayHandoffOwner,
  performFirstDisplayTakeoverV1,
  type FinalizedFirstDisplayHandoffV1,
} from '../../src/first_display/handoff';

const RELEASE_ID = 'a'.repeat(64);
const DIGEST = 'b'.repeat(64);

function handoff(revision = 0): Record<string, unknown> {
  return {
    version: 1,
    releaseId: RELEASE_ID,
    generation: 1,
    projectionDigest: DIGEST,
    slices: ['first_display'],
    slots: [
      {
        id: 'slot-1',
        aliases: [],
        domId: 'div-1',
        gamPath: '/123/slot-1',
        formats: [[300, 250]],
        owner: 'trusted_server',
        outcome: 'failed',
        targeting: [],
        committedArtifact: 'none',
        gptToken: null,
      },
    ],
    attempts: [],
    tombstones: [],
    artifacts: [],
    parserState: [],
    gptFacts: [],
    gptFactOverflow: 0,
    timing: { bidsScriptMs: 1, firstDisplayMs: null, terminalMs: 2, paintMs: 3 },
    highWater: {
      navigationAttemptPrefix: 'nav1',
      nextNavigationAttemptOrdinal: 1,
      nextAttemptOrdinal: 1,
      nextSlotRegistrationOrdinal: 2,
      reservationClockEpochMs: 0,
      nextReservationOrdinal: 1,
      nextTicketOrdinal: 1,
    },
    cycles: [],
    trace: { nextSequence: 1, nextGlobalSlotOrdinal: 2, slots: [] },
    mutationRevision: revision,
  };
}

function outline(): Record<string, unknown> {
  return {
    version: 1,
    releaseId: RELEASE_ID,
    generation: 1,
    projectionDigest: DIGEST,
    slices: ['first_display'],
    slotCount: 1,
    outcomeCount: 1,
    capabilities: [],
    objectKinds: [],
  };
}

function finalized(identity: object = {}): FinalizedFirstDisplayHandoffV1 {
  const owner = createFirstDisplayHandoffOwner({
    releaseId: RELEASE_ID,
    generation: 1,
    isCurrentGeneration: () => true,
    isTerminal: () => true,
    isPainted: () => true,
    closeIngress: () => undefined,
    onFailure: () => undefined,
  });
  const value = owner.finalize(handoff(), [identity]);
  if (!value) throw new Error('should finalize test handoff');
  return value;
}

describe('atomic first-display takeover', () => {
  it('transfers one-use identities and commits in the exact non-yielding order', () => {
    const events: string[] = [];
    const identity = {};
    const result = performFirstDisplayTakeoverV1({
      finalized: finalized(identity),
      outline: outline(),
      isCurrentGeneration: () => true,
      authenticateRuntimeScript: () => true,
      currentMutationRevision: () => 0,
      quiesceAgent: () => events.push('quiesce-agent'),
      detachCommittedArtifacts: () => events.push('detach-artifacts'),
      disposeAgent: () => events.push('dispose-agent'),
      activatePersistent: (_snapshot, identities, own) => {
        expect(identities).toEqual([identity]);
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
});
