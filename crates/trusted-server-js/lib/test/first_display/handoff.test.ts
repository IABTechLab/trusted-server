import { describe, expect, it, vi } from 'vitest';

import { createFirstDisplayHandoffOwner } from '../../src/first_display/handoff';

const RELEASE_ID = 'a'.repeat(64);

function handoff(revision: number, overrides: Record<string, unknown> = {}): Record<string, unknown> {
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
    gptFacts: [],
    gptFactOverflow: 0,
    timing: { bidsScriptMs: 1, firstDisplayMs: null, terminalMs: 2, paintMs: 3 },
    highWater: {
      navigationAttemptPrefix: 'nav1',
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

    const final = h.value.finalize(handoff(2), [physicalSlot, artifact]);
    expect(final?.handoff.mutationRevision).toBe(2);
    expect(Object.isFrozen(final?.handoff)).toBe(true);
    expect(final?.capsule.consume(RELEASE_ID, 1)).toEqual([physicalSlot, artifact]);
    expect(final?.capsule.consume(RELEASE_ID, 1)).toBeUndefined();
    expect(h.value.state).toBe('finalized');
    expect(h.events).toEqual(['close-ingress']);
    expect(h.failures).toEqual([]);
  });

  it('clears the capsule and fails closed on duplicate finalization or stale revision', () => {
    const duplicate = owner();
    const identity = {};
    const final = duplicate.value.finalize(handoff(0), [identity]);
    expect(final).toBeDefined();
    expect(duplicate.value.finalize(handoff(0), [identity])).toBeUndefined();
    expect(final?.capsule.consume(RELEASE_ID, 1)).toBeUndefined();
    expect(duplicate.failures).toEqual(['bundle_partial']);

    const stale = owner();
    stale.value.observeMutation();
    expect(stale.value.finalize(handoff(0), [{}])).toBeUndefined();
    expect(stale.value.state).toBe('failed');
    expect(stale.failures).toEqual(['bundle_partial']);
  });

  it('rejects nonterminal/live-authority data, wrong identity, and revision exhaustion', () => {
    const nonterminal = owner();
    expect(
      nonterminal.value.finalize(
        handoff(0, {
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
        []
      )
    ).toBeUndefined();
    expect(nonterminal.failures).toEqual(['bundle_partial']);

    const badIdentity = owner();
    expect(badIdentity.value.finalize(handoff(0), [null as unknown as object])).toBeUndefined();
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
      expect(value.finalize(handoff(0), [])).toBeUndefined();
      expect(closeIngress).not.toHaveBeenCalled();
      expect(failures).toEqual(['bundle_partial']);
    }
  });
});
