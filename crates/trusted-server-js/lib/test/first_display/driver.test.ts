import { describe, expect, it, vi } from 'vitest';

import type {
  FirstDisplayGoogletagBatchCallbacks,
  FirstDisplayGptBoundCycleV1,
} from '../../src/first_display/adapters/googletag';
import { createFirstDisplayProjectedDriver } from '../../src/first_display/driver';
import type { FirstDisplayGptProtocolV1 } from '../../src/first_display/leaf/gpt_protocol';
import { snapshotFirstDisplayBatchV1 } from '../../src/first_display/leaf/projection';

function batch() {
  return snapshotFirstDisplayBatchV1(
    Object.freeze({
      version: 1,
      projectionDigest: 'b'.repeat(64),
      projection: Object.freeze({
        version: 1,
        auction: Object.freeze({
          version: 1,
          auctionId: 'initial',
          results: Object.freeze([
            Object.freeze({ slot: 'slot-1', outcome: 'winner', candidateId: 'candidate001' }),
          ]),
        }),
        slots: Object.freeze([
          Object.freeze({
            slot: 'slot-1',
            gamUnitPath: '/123/example',
            divId: 'slot-1',
            formats: Object.freeze([Object.freeze([300, 250])]),
            targeting: Object.freeze({}),
          }),
        ]),
        bids: Object.freeze([
          Object.freeze({
            candidateId: 'candidate001',
            slot: 'slot-1',
            provider: 'example',
            upstreamBidId: 'upstream-1',
            cpm: 1.25,
            currency: 'USD',
            targeting: Object.freeze({}),
            rendererReservationId: `r1_${'a'.repeat(22)}`,
            renderSource: Object.freeze({
              type: 'adm',
              version: 1,
              adm: '<div>example</div>',
              width: 300,
              height: 250,
            }),
          }),
        ]),
      }),
    })
  )!;
}

function harness() {
  const value = batch();
  let gptCallbacks: FirstDisplayGoogletagBatchCallbacks | undefined;
  let renderTerminal:
    ((result: 'accepted' | 'failed' | 'cancelled', reason: string | null) => void) | undefined;
  const gptBatch = {
    start: vi.fn((callbacks: FirstDisplayGoogletagBatchCallbacks) => {
      gptCallbacks = callbacks;
      return true;
    }),
    captureDiagnosticsHandoff: vi.fn(() => ({
      cycles: [cycleOwner.value]
        .filter((cycle): cycle is FirstDisplayGptBoundCycleV1 => cycle !== undefined)
        .map((cycle) => ({
          nextCycleOrdinal: 2,
          quarantines: [],
          records: [
            {
              ordinal: 1,
              responseIdentifier: 'response-one',
              seen: ['slotRequested', 'slotRenderEnded'] as const,
              state: 'completed' as const,
            },
          ],
          slotId: cycle.slotId,
          token: cycle.traceToken,
          unknownPriorCycle: false,
        })),
      facts: [],
      nextTraceTokenOrdinal: 2,
      overflowCount: 0,
      dropCount: 0,
    })),
    captureHandoff: vi.fn(() => [cycleOwner.value].filter(Boolean)),
    closeIngress: vi.fn(() => true),
    detachCommittedSlots: vi.fn(() => true),
    dispose: vi.fn(),
  };
  const protocol = {
    createBatch: vi.fn(() => gptBatch),
  } as unknown as FirstDisplayGptProtocolV1;
  const events: string[] = [];
  const cycleOwner: { value?: FirstDisplayGptBoundCycleV1 } = {};
  const renderer = {
    bind: vi.fn((_cycle: FirstDisplayGptBoundCycleV1, terminal: typeof renderTerminal) => {
      events.push('render:bind');
      renderTerminal = terminal;
      return true;
    }),
    recordGam: vi.fn((_cycle: FirstDisplayGptBoundCycleV1, result: string) => {
      events.push(`render:gam:${result}`);
      return true;
    }),
    recordFailure: vi.fn(() => true),
    captureHandoff: vi.fn(() => ({
      artifacts: [],
      clockEpochMs: 0,
      nextReservationOrdinal: 1,
      nextTicketOrdinal: 1,
      tombstones: [],
    })),
    closeIngress: vi.fn(() => true),
    detachCommittedArtifacts: vi.fn(() => true),
    sealTsAdmission: vi.fn(() => events.push('render:seal')),
    dispose: vi.fn(() => events.push('render:dispose')),
  };
  const driver = createFirstDisplayProjectedDriver({
    batch: value,
    gpt: protocol,
    gptInput: {
      browser: window,
      clearTimer: () => undefined,
      document,
      setTimer: (callback) => callback,
    },
    renderer,
  });
  const terminals: unknown[] = [];
  driver.start(
    value.outcomes,
    () => {
      events.push('action');
      return true;
    },
    (slotId, result, reason) => terminals.push([slotId, result, reason])
  );
  const cycle = Object.freeze({
    bid: value.projection.bids[0]!,
    element: document.createElement('div'),
    ownership: 'trusted_server' as const,
    physicalSlot: {},
    placement: value.projection.slots[0]!,
    slotId: 'slot-1',
    traceToken: 'gt1_1',
  });
  cycleOwner.value = cycle;
  return {
    cycle,
    driver,
    events,
    gptBatch,
    getGptCallbacks: () => gptCallbacks!,
    renderer,
    getRenderTerminal: () => renderTerminal!,
    terminals,
  };
}

describe('projected first-display driver', () => {
  it('joins exact GPT delivery with renderer-owned completion', () => {
    const h = harness();
    expect(h.getGptCallbacks().onBound(h.cycle)).toBeUndefined();
    expect(h.getGptCallbacks().onFirstAction()).toBe(true);
    h.getGptCallbacks().onRenderEnded(h.cycle, 'nonempty_gam');
    expect(h.terminals).toEqual([]);

    h.getRenderTerminal()('accepted', null);
    h.getRenderTerminal()('failed', 'internal_error');
    expect(h.events).toEqual(['render:bind', 'action', 'render:gam:nonempty_gam']);
    expect(h.terminals).toEqual([['slot-1', 'accepted', null]]);
  });

  it('fails an empty or mismatched physical GPT cycle without guessing', () => {
    const empty = harness();
    empty.getGptCallbacks().onBound(empty.cycle);
    empty.getGptCallbacks().onFirstAction();
    empty.getGptCallbacks().onRenderEnded(empty.cycle, 'gam_empty');
    expect(empty.renderer.recordGam).toHaveBeenCalledWith(empty.cycle, 'gam_empty');

    const mismatched = harness();
    mismatched.getGptCallbacks().onBound(mismatched.cycle);
    mismatched.getGptCallbacks().onFirstAction();
    mismatched
      .getGptCallbacks()
      .onRenderEnded(Object.freeze({ ...mismatched.cycle, physicalSlot: {} }), 'nonempty_gam');
    expect(mismatched.terminals).toEqual([['slot-1', 'failed', 'gpt_request_failed']]);
    expect(mismatched.renderer.recordGam).not.toHaveBeenCalled();
  });

  it('owns exact-once sealing and disposal', () => {
    const h = harness();
    h.getGptCallbacks().onBound(h.cycle);
    h.getGptCallbacks().onFirstAction();
    h.getGptCallbacks().onFailure('slot-1', 'gpt_request_timeout');
    expect(h.terminals).toEqual([['slot-1', 'failed', 'gpt_request_timeout']]);

    h.driver.sealTsAdmission();
    h.driver.dispose();
    h.driver.dispose();
    expect(h.renderer.sealTsAdmission).toHaveBeenCalledOnce();
    expect(h.gptBatch.dispose).toHaveBeenCalledOnce();
    expect(h.renderer.dispose).toHaveBeenCalledOnce();
  });

  it('captures accepted objects before detaching them from both provisional owners', () => {
    const h = harness();
    h.getGptCallbacks().onBound(h.cycle);
    h.getGptCallbacks().onFirstAction();
    h.getGptCallbacks().onRenderEnded(h.cycle, 'nonempty_gam');
    h.getRenderTerminal()('accepted', null);
    h.driver.sealTsAdmission();

    expect(h.driver.closeIngress()).toBe(true);
    expect(h.driver.captureHandoff()).toEqual({
      artifacts: [],
      clockEpochMs: 0,
      cycles: [h.cycle],
      diagnosticCycles: [
        {
          nextCycleOrdinal: 2,
          quarantines: [],
          records: [
            {
              ordinal: 1,
              responseIdentifier: 'response-one',
              seen: ['slotRequested', 'slotRenderEnded'],
              state: 'completed',
            },
          ],
          slotId: 'slot-1',
          token: 'gt1_1',
          unknownPriorCycle: false,
        },
      ],
      gptDiagnostics: { facts: [], overflowCount: 0, dropCount: 0 },
      identities: [h.cycle.physicalSlot],
      nextReservationOrdinal: 1,
      nextTraceTokenOrdinal: 2,
      nextTicketOrdinal: 1,
      tombstones: [],
    });
    expect(h.driver.detachCommittedArtifacts()).toBe(true);
    expect(h.gptBatch.detachCommittedSlots).toHaveBeenCalledWith(['slot-1']);
    expect(h.renderer.detachCommittedArtifacts).toHaveBeenCalledOnce();
  });

  it('rejects an action list that does not exactly match the immutable batch', () => {
    const value = batch();
    const driver = createFirstDisplayProjectedDriver({
      batch: value,
      gpt: {
        createBatch: () => ({
          start: () => true,
          captureHandoff: () => [],
          closeIngress: () => true,
          detachCommittedSlots: () => true,
          dispose: () => undefined,
        }),
      } as unknown as FirstDisplayGptProtocolV1,
      gptInput: {
        browser: window,
        clearTimer: () => undefined,
        document,
        setTimer: (callback) => callback,
      },
      renderer: {
        bind: () => true,
        recordGam: () => true,
        recordFailure: () => true,
        captureHandoff: () => ({
          artifacts: [],
          clockEpochMs: 0,
          nextReservationOrdinal: 1,
          nextTicketOrdinal: 1,
          tombstones: [],
        }),
        closeIngress: () => true,
        detachCommittedArtifacts: () => true,
        sealTsAdmission: () => undefined,
        dispose: () => undefined,
      },
    });
    expect(() =>
      driver.start(
        Object.freeze([{ slotId: 'other', kind: 'gpt_adm' }]),
        () => true,
        () => undefined
      )
    ).toThrow('first-display action list');
  });
});
