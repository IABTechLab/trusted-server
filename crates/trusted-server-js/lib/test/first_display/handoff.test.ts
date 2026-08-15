import { describe, expect, it, vi } from 'vitest';

import { createFirstDisplayHandoffOwner } from '../../src/shared/first_display_handoff';

const RELEASE_ID = 'a'.repeat(64);
const RESERVATION_ID = `r1_${'a'.repeat(22)}`;

function handoff(
  revision: number,
  overrides: Record<string, unknown> = {}
): Record<string, unknown> {
  return {
    version: 1,
    releaseId: RELEASE_ID,
    generation: 1,
    projectionDigest: 'b'.repeat(64),
    slices: ['first_display'],
    slots: [],
    attempts: [],
    tombstones: [],
    artifacts: [],
    parserState: [],
    gptDiagnostics: { facts: [], overflowCount: 0, dropCount: 0 },
    timing: { bidsScriptMs: 1, firstDisplayMs: null, terminalMs: 2, paintMs: 3 },
    highWater: {
      navigationAttemptPrefix: 'AAECAwQFBgc',
      nextNavigationAttemptOrdinal: 1,
      nextAttemptOrdinal: 1,
      nextSlotRegistrationOrdinal: 1,
      reservationClockEpochMs: 0,
      nextReservationOrdinal: 1,
      nextTicketOrdinal: 1,
    },
    cycles: [],
    trace: { nextSequence: 1, nextGlobalSlotOrdinal: 1, slots: [] },
    mutationRevision: revision,
    ...overrides,
  };
}

function acceptedHandoff(revision: number): Record<string, unknown> {
  return handoff(revision, {
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
  });
}

function owner(options: { initialRevision?: number } = {}) {
  const failures: string[] = [];
  const events: string[] = [];
  return {
    events,
    failures,
    value: createFirstDisplayHandoffOwner({
      releaseId: RELEASE_ID,
      generation: 1,
      ...(options.initialRevision === undefined
        ? {}
        : { initialMutationRevision: options.initialRevision }),
      isCurrentGeneration: () => true,
      isTerminal: () => true,
      isPainted: () => true,
      closeIngress: () => events.push('close-ingress'),
      onFailure: (reason) => failures.push(reason),
    }),
  };
}

describe('first-display final handoff owner', () => {
  it('seals the final revision and mints one release-bound capsule in the same task', () => {
    const h = owner();
    const physicalSlot = {};
    const artifact = {};
    expect(h.value.observeMutation()).toBe(true);
    expect(h.value.observeMutation()).toBe(true);

    const final = h.value.finalize(() => ({
      candidate: acceptedHandoff(2),
      identities: [physicalSlot, artifact],
    }));
    expect(final?.handoff.mutationRevision).toBe(2);
    expect(Object.isFrozen(final?.handoff)).toBe(true);
    expect(final?.capsule.consume(RELEASE_ID, 1)).toEqual([physicalSlot, artifact]);
    expect(final?.capsule.consume(RELEASE_ID, 1)).toBeUndefined();
    expect(h.value.state).toBe('finalized');
    expect(h.events).toEqual(['close-ingress']);
    expect(h.failures).toEqual([]);
  });

  it('drains a synchronous native mutation while closing ingress before final capture', () => {
    const failures: string[] = [];
    const value = createFirstDisplayHandoffOwner({
      releaseId: RELEASE_ID,
      generation: 1,
      isCurrentGeneration: () => true,
      isTerminal: () => true,
      isPainted: () => true,
      closeIngress: () => {
        expect(value.observeMutation()).toBe(true);
      },
      onFailure: (reason) => failures.push(reason),
    });

    expect(
      value.finalize(() => ({
        candidate: handoff(1),
        identities: [],
      }))
    ).toBeDefined();
    expect(value.mutationRevision).toBe(1);
    expect(failures).toEqual([]);
  });

  it('clears the capsule and fails closed on duplicate finalization or stale revision', () => {
    const duplicate = owner();
    const identity = {};
    const artifact = {};
    const final = duplicate.value.finalize(() => ({
      candidate: acceptedHandoff(0),
      identities: [identity, artifact],
    }));
    expect(final).toBeDefined();
    expect(
      duplicate.value.finalize(() => ({
        candidate: acceptedHandoff(0),
        identities: [identity, artifact],
      }))
    ).toBeUndefined();
    expect(final?.capsule.consume(RELEASE_ID, 1)).toBeUndefined();
    expect(duplicate.failures).toEqual(['bundle_partial']);

    const extraIdentity = owner();
    expect(
      extraIdentity.value.finalize(() => ({ candidate: handoff(0), identities: [{}] }))
    ).toBeUndefined();
    expect(extraIdentity.failures).toEqual(['bundle_partial']);

    const stale = owner();
    stale.value.observeMutation();
    expect(
      stale.value.finalize(() => ({ candidate: handoff(0), identities: [{}] }))
    ).toBeUndefined();
    expect(stale.value.state).toBe('failed');
    expect(stale.failures).toEqual(['bundle_partial']);
  });

  it('rejects nonterminal/live-authority data, wrong identity, and revision exhaustion', () => {
    const nonterminal = owner();
    expect(
      nonterminal.value.finalize(() => ({
        candidate: handoff(0, {
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
          attempts: [
            { id: 'attempt-1', slotId: 'slot-1', ordinal: 1, state: 'pending', reason: null },
          ],
          highWater: {
            ...((handoff(0).highWater as object) ?? {}),
            nextAttemptOrdinal: 2,
            nextSlotRegistrationOrdinal: 2,
          },
        }),
        identities: [],
      }))
    ).toBeUndefined();
    expect(nonterminal.failures).toEqual(['bundle_partial']);

    const badIdentity = owner();
    expect(
      badIdentity.value.finalize(() => ({
        candidate: handoff(0),
        identities: [null as unknown as object],
      }))
    ).toBeUndefined();
    expect(badIdentity.failures).toEqual(['bundle_partial']);

    const exhausted = owner({ initialRevision: 4_294_967_295 });
    expect(exhausted.value.observeMutation()).toBe(false);
    expect(exhausted.value.state).toBe('failed');
    expect(exhausted.failures).toEqual(['bundle_partial']);
  });

  it('requires terminal paint and the current generation before closing ingress', () => {
    for (const failedGate of ['generation', 'terminal', 'paint'] as const) {
      const closeIngress = vi.fn();
      const failures: string[] = [];
      const value = createFirstDisplayHandoffOwner({
        releaseId: RELEASE_ID,
        generation: 1,
        isCurrentGeneration: () => failedGate !== 'generation',
        isTerminal: () => failedGate !== 'terminal',
        isPainted: () => failedGate !== 'paint',
        closeIngress,
        onFailure: (reason) => failures.push(reason),
      });
      expect(value.finalize(() => ({ candidate: handoff(0), identities: [] }))).toBeUndefined();
      expect(closeIngress).not.toHaveBeenCalled();
      expect(failures).toEqual(['bundle_partial']);
    }
  });
});
