import { describe, expect, it } from 'vitest';

import {
  MAX_FIRST_DISPLAY_HANDOFF_BYTES,
  MAX_FIRST_DISPLAY_NON_DIAGNOSTICS_BYTES,
  MAX_GPT_FACT_BYTES,
  createFirstDisplayOwnershipCapsuleV1,
  snapshotFirstDisplayHandoffV1,
  snapshotTakeoverOutlineV1,
} from '../../src/first_display/contracts';

const HASH = 'a'.repeat(64);

function outline(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    version: 1,
    releaseId: HASH,
    generation: 1,
    projectionDigest: 'b'.repeat(64),
    slices: ['first_display', 'gpt_initial'],
    slotCount: 1,
    outcomeCount: 1,
    capabilities: ['gpt_slot', 'dom_artifact'],
    objectKinds: ['gpt_slot', 'dom_artifact'],
    ...overrides,
  };
}

function handoff(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    version: 1,
    releaseId: HASH,
    generation: 1,
    projectionDigest: 'b'.repeat(64),
    slices: ['first_display', 'gpt_initial'],
    slots: [
      {
        id: 'slot-1',
        aliases: ['alias-1'],
        domId: 'div-1',
        gamPath: '/123/home',
        formats: [[300, 250]],
        owner: 'trusted_server',
        outcome: 'accepted',
        targeting: [['hb_adid', 'bid-1']],
        committedArtifact: 'gpt_adm',
        gptToken: 'gt1_1',
      },
    ],
    attempts: [
      {
        id: 'attempt-1',
        slotId: 'slot-1',
        ordinal: 1,
        state: 'accepted',
        reason: null,
      },
    ],
    tombstones: [
      {
        kind: 'reservation',
        value: 'opaque-1',
        expiresAtMs: 1_000,
        ordinal: 1,
      },
    ],
    artifacts: [
      { slotId: 'slot-1', kind: 'gpt_adm', owner: 'trusted_server', token: 'artifact-1' },
    ],
    parserState: [
      { sliceId: 'gpt_initial', observations: ['slot-defined'], values: [['ready', true]] },
    ],
    gptFacts: [{ kind: 'slotRenderEnded', observedAtMs: 12, slot: { token: 'gt1_1' } }],
    gptFactOverflow: 0,
    timing: {
      bidsScriptMs: 1,
      firstDisplayMs: 2,
      terminalMs: 3,
      paintMs: 4,
    },
    highWater: {
      navigationAttemptPrefix: 'nav1',
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
        records: [{ ordinal: 1, state: 'completed' }],
        quarantines: [],
      },
    ],
    trace: {
      nextSequence: 2,
      nextGlobalSlotOrdinal: 2,
      slots: [{ slotId: 'slot-1', impressions: 1, bindings: ['gt1_1'] }],
    },
    mutationRevision: 7,
    ...overrides,
  };
}

describe('first-display immutable contracts', () => {
  it('snapshots an exact takeover outline into a fresh recursively frozen value', () => {
    const input = outline();
    const accepted = snapshotTakeoverOutlineV1(input);

    expect(accepted).toEqual(input);
    expect(accepted).not.toBe(input);
    expect(Object.isFrozen(accepted)).toBe(true);
    expect(Object.isFrozen(accepted?.slices)).toBe(true);
  });

  it('rejects unknown, missing, inherited, accessor, malformed, and noncanonical outline data', () => {
    expect(snapshotTakeoverOutlineV1({ ...outline(), extra: true })).toBeUndefined();
    const missing = outline();
    delete missing.projectionDigest;
    expect(snapshotTakeoverOutlineV1(missing)).toBeUndefined();
    expect(snapshotTakeoverOutlineV1(Object.create(outline()))).toBeUndefined();
    const accessor = outline();
    Object.defineProperty(accessor, 'generation', { enumerable: true, get: () => 1 });
    expect(snapshotTakeoverOutlineV1(accessor)).toBeUndefined();
    expect(snapshotTakeoverOutlineV1(outline({ releaseId: HASH.toUpperCase() }))).toBeUndefined();
    expect(
      snapshotTakeoverOutlineV1(outline({ slices: ['gpt_initial', 'first_display'] }))
    ).toBeUndefined();
    expect(
      snapshotTakeoverOutlineV1(outline({ capabilities: ['gpt_slot', 'gpt_slot'] }))
    ).toBeUndefined();
  });

  it('accepts the complete exact handoff and rejects payload or live-authority fields', () => {
    const input = handoff();
    const accepted = snapshotFirstDisplayHandoffV1(input);

    expect(accepted).toEqual(input);
    expect(accepted).not.toBe(input);
    expect(Object.isFrozen(accepted)).toBe(true);
    expect(Object.isFrozen(accepted?.slots[0]?.targeting)).toBe(true);

    const withAdm = handoff();
    (withAdm.slots as Array<Record<string, unknown>>)[0]!.adm = '<script>bad()</script>';
    expect(snapshotFirstDisplayHandoffV1(withAdm)).toBeUndefined();
    expect(
      snapshotFirstDisplayHandoffV1(handoff({ livePort: new MessageChannel().port1 }))
    ).toBeUndefined();
    expect(
      snapshotFirstDisplayHandoffV1(
        handoff({
          attempts: [{ ...((handoff().attempts as object[])[0] as object), state: 'pending' }],
        })
      )
    ).toBeUndefined();
  });

  it('enforces 255/256/257 slot and outcome boundaries and strict high-water counters', () => {
    const base = handoff();
    const slot = (base.slots as object[])[0]!;
    const attempt = (base.attempts as object[])[0]!;
    for (const count of [255, 256]) {
      const slots = Array.from({ length: count }, (_, index) => ({
        ...(slot as Record<string, unknown>),
        id: `slot-${index}`,
        domId: `div-${index}`,
        gptToken: `gt1_${(index + 1).toString(36)}`,
      }));
      const attempts = Array.from({ length: count }, (_, index) => ({
        ...(attempt as Record<string, unknown>),
        id: `attempt-${index}`,
        slotId: `slot-${index}`,
        ordinal: index + 1,
      }));
      expect(
        snapshotFirstDisplayHandoffV1(
          handoff({
            slots,
            attempts,
            artifacts: [],
            parserState: [],
            gptFacts: [],
            tombstones: [],
            cycles: [],
            trace: { nextSequence: 1, nextGlobalSlotOrdinal: count + 1, slots: [] },
            highWater: {
              ...(base.highWater as object),
              nextAttemptOrdinal: count + 1,
              nextSlotRegistrationOrdinal: count + 1,
            },
          })
        )
      ).toBeDefined();
    }

    const tooMany = Array.from({ length: 257 }, (_, index) => ({
      ...(slot as Record<string, unknown>),
      id: `slot-${index}`,
      domId: `div-${index}`,
    }));
    expect(snapshotFirstDisplayHandoffV1(handoff({ slots: tooMany }))).toBeUndefined();
    expect(
      snapshotFirstDisplayHandoffV1(
        handoff({ highWater: { ...(base.highWater as object), nextAttemptOrdinal: 1 } })
      )
    ).toBeUndefined();
  });

  it('publishes the exact independent size ceilings', () => {
    expect(MAX_FIRST_DISPLAY_NON_DIAGNOSTICS_BYTES).toBe(8 * 1024 * 1024);
    expect(MAX_GPT_FACT_BYTES).toBe(512 * 1024);
    expect(MAX_FIRST_DISPLAY_HANDOFF_BYTES).toBe(8.5 * 1024 * 1024);
  });

  it('enforces the exact 512 KiB diagnostics subsection boundary', () => {
    const exactFacts = [...Array.from({ length: 127 }, () => 'x'.repeat(4096)), 'x'.repeat(3711)];
    expect(new TextEncoder().encode(JSON.stringify(exactFacts))).toHaveLength(MAX_GPT_FACT_BYTES);
    expect(snapshotFirstDisplayHandoffV1(handoff({ gptFacts: exactFacts }))).toBeDefined();
    exactFacts[127] += 'x';
    expect(snapshotFirstDisplayHandoffV1(handoff({ gptFacts: exactFacts }))).toBeUndefined();
    exactFacts[127] = exactFacts[127]!.slice(0, -2);
    expect(snapshotFirstDisplayHandoffV1(handoff({ gptFacts: exactFacts }))).toBeDefined();
  });

  it('enforces the exact 8 MiB non-diagnostics canonical boundary', () => {
    const base = handoff({
      attempts: [],
      tombstones: [],
      artifacts: [],
      parserState: [],
      gptFacts: [],
      cycles: [],
      trace: { nextSequence: 1, nextGlobalSlotOrdinal: 257, slots: [] },
      highWater: {
        ...(handoff().highWater as object),
        nextAttemptOrdinal: 1,
        nextSlotRegistrationOrdinal: 257,
      },
    });
    const slot = (handoff().slots as Array<Record<string, unknown>>)[0]!;
    base.slots = Array.from({ length: 256 }, (_, slotIndex) => ({
      ...slot,
      id: `slot-${slotIndex}`,
      aliases: [],
      domId: `div-${slotIndex}`,
      gptToken: null,
      targeting: Array.from({ length: 32 }, (_, targetingIndex) => [`k${targetingIndex}`, '']),
    }));
    const encoder = new TextEncoder();
    let remaining =
      MAX_FIRST_DISPLAY_NON_DIAGNOSTICS_BYTES - encoder.encode(JSON.stringify(base)).byteLength;
    expect(remaining).toBeGreaterThan(0);
    for (const slotValue of base.slots as Array<Record<string, unknown>>) {
      for (const pair of slotValue.targeting as string[][]) {
        const next = Math.min(4096, remaining);
        pair[1] = 'x'.repeat(next);
        remaining -= next;
        if (remaining === 0) break;
      }
      if (remaining === 0) break;
    }
    expect(remaining).toBe(0);
    expect(encoder.encode(JSON.stringify(base))).toHaveLength(
      MAX_FIRST_DISPLAY_NON_DIAGNOSTICS_BYTES
    );
    expect(snapshotFirstDisplayHandoffV1(base)).toBeDefined();

    const firstPair = (
      (base.slots as Array<Record<string, unknown>>)[0]!.targeting as string[][]
    )[0]!;
    firstPair[1] += 'x';
    expect(snapshotFirstDisplayHandoffV1(base)).toBeUndefined();
    firstPair[1] = firstPair[1]!.slice(0, -2);
    expect(snapshotFirstDisplayHandoffV1(base)).toBeDefined();
  });

  it('mints a release-bound one-use capsule and clears every outcome', () => {
    const physicalSlot = {};
    const artifact = {};
    const capsule = createFirstDisplayOwnershipCapsuleV1(HASH, 7, [physicalSlot, artifact]);
    expect(capsule?.consume('b'.repeat(64), 7)).toBeUndefined();
    expect(capsule?.consume(HASH, 8)).toBeUndefined();
    expect(capsule?.consume(HASH, 7)).toEqual([physicalSlot, artifact]);
    expect(capsule?.consume(HASH, 7)).toBeUndefined();

    const cleared = createFirstDisplayOwnershipCapsuleV1(HASH, 7, [physicalSlot]);
    cleared?.clear();
    expect(cleared?.consume(HASH, 7)).toBeUndefined();
  });
});
