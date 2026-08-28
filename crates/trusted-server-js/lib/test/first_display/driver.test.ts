import { describe, expect, it, vi } from 'vitest';

import type {
  FirstDisplayGoogletagBatchCallbacks,
  FirstDisplayGptBoundCycleV1,
  FirstDisplayGptHandoffCycleV1,
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
    captureDiagnosticsHandoff: vi.fn(() =>
      Object.freeze([
        [cycleOwner.value]
          .filter((cycle): cycle is FirstDisplayGptHandoffCycleV1 => cycle !== undefined)
          .map((cycle) =>
            Object.freeze([
              cycle[6],
              cycle[7],
              2,
              false,
              Object.freeze([]),
              Object.freeze([
                Object.freeze([
                  1,
                  'response-one',
                  Object.freeze(['slotRequested', 'slotRenderEnded'] as const),
                  'completed',
                ] as const),
              ]),
            ] as const)
          ),
        Object.freeze([]),
        2,
        0,
        0,
      ] as const)
    ),
    captureHandoff: vi.fn(() =>
      [cycleOwner.value]
        .filter((cycle): cycle is FirstDisplayGptHandoffCycleV1 => cycle !== undefined)
        .map((cycle) =>
          Object.freeze([cycle[6], cycle[1].id, cycle[3], cycle[8], cycle[7], cycle[4]] as const)
        )
    ),
    closeIngress: vi.fn(() => {
      events.push('gpt:close');
      return true;
    }),
    detachCommittedSlots: vi.fn(() => true),
    dispose: vi.fn(),
  };
  const protocol: FirstDisplayGptProtocolV1 = Object.freeze([
    1,
    'gpt',
    vi.fn(() =>
      Object.freeze([
        gptBatch.start,
        gptBatch.closeIngress,
        gptBatch.captureHandoff,
        gptBatch.captureDiagnosticsHandoff,
        gptBatch.detachCommittedSlots,
        gptBatch.dispose,
      ] as const)
    ),
  ]);
  const events: string[] = [];
  const cycleOwner: { value?: FirstDisplayGptHandoffCycleV1 } = {};
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
    retire: vi.fn(() => true),
    sweepCommittedArtifacts: vi.fn(() => 0),
    captureHandoff: vi.fn(() => [[], [], 0, 1, 1] as const),
    closeIngress: vi.fn(() => {
      events.push('render:close');
      return true;
    }),
    detachCommittedArtifacts: vi.fn(() => true),
    sealTsAdmission: vi.fn(() => events.push('render:seal')),
    dispose: vi.fn(() => events.push('render:dispose')),
  };
  const driver = createFirstDisplayProjectedDriver({
    batch: value,
    gpt: protocol,
    gptInput: [window, () => undefined, document, (callback) => callback],
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
  const cycle: FirstDisplayGptBoundCycleV1 = Object.freeze([
    value.projection.bids[0]!,
    document.createElement('div'),
    () => true,
    'trusted_server' as const,
    {},
    value.projection.slots[0]!,
    'slot-1',
    'gt1_1',
  ]);
  cycleOwner.value = Object.freeze([...cycle, Object.freeze([])] as const);
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
    expect(h.getGptCallbacks()[0](h.cycle)).toBeUndefined();
    expect(h.getGptCallbacks()[2]()).toBe(true);
    h.getGptCallbacks()[3](h.cycle, 'nonempty_gam');
    expect(h.terminals).toEqual([]);

    h.getRenderTerminal()('accepted', null);
    h.getRenderTerminal()('failed', 'internal_error');
    expect(h.events).toEqual(['render:bind', 'action', 'render:gam:nonempty_gam']);
    expect(h.terminals).toEqual([['slot-1', 'accepted', null]]);

    h.getGptCallbacks()[4]?.(h.cycle);
    h.getGptCallbacks()[4]?.(
      Object.freeze([
        h.cycle[0],
        h.cycle[1],
        h.cycle[2],
        h.cycle[3],
        {},
        h.cycle[5],
        h.cycle[6],
        h.cycle[7],
      ])
    );
    expect(h.renderer.retire).toHaveBeenCalledExactlyOnceWith(h.cycle);
  });

  it('fails an empty or mismatched physical GPT cycle without guessing', () => {
    const empty = harness();
    empty.getGptCallbacks()[0](empty.cycle);
    empty.getGptCallbacks()[2]();
    empty.getGptCallbacks()[3](empty.cycle, 'gam_empty');
    expect(empty.renderer.recordGam).toHaveBeenCalledWith(empty.cycle, 'gam_empty');

    const mismatched = harness();
    mismatched.getGptCallbacks()[0](mismatched.cycle);
    mismatched.getGptCallbacks()[2]();
    mismatched.getGptCallbacks()[3](
      Object.freeze([
        mismatched.cycle[0],
        mismatched.cycle[1],
        mismatched.cycle[2],
        mismatched.cycle[3],
        {},
        mismatched.cycle[5],
        mismatched.cycle[6],
        mismatched.cycle[7],
      ]),
      'nonempty_gam'
    );
    expect(mismatched.terminals).toEqual([['slot-1', 'failed', 'gpt_request_failed']]);
    expect(mismatched.renderer.recordGam).not.toHaveBeenCalled();
  });

  it('owns exact-once sealing and disposal', () => {
    const h = harness();
    h.getGptCallbacks()[0](h.cycle);
    h.getGptCallbacks()[2]();
    h.getGptCallbacks()[1]('slot-1', 'gpt_request_timeout');
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
    h.getGptCallbacks()[0](h.cycle);
    h.getGptCallbacks()[2]();
    h.getGptCallbacks()[3](h.cycle, 'nonempty_gam');
    h.getRenderTerminal()('accepted', null);
    h.driver.sealTsAdmission();

    expect(h.driver.closeIngress()).toBe(true);
    expect(h.gptBatch.closeIngress).toHaveBeenCalledExactlyOnceWith(['slot-1']);
    expect(h.events.slice(-2)).toEqual(['gpt:close', 'render:close']);
    expect(h.driver.captureHandoff()).toEqual({
      artifacts: [],
      clockEpochMs: 0,
      cycles: [[...h.cycle, []]],
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
      identities: [h.cycle[4]],
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
      gpt: Object.freeze([
        1,
        'gpt',
        () =>
          Object.freeze([
            () => true,
            () => true,
            () => [],
            () => undefined,
            () => true,
            () => undefined,
          ] as const),
      ]),
      gptInput: [window, () => undefined, document, (callback) => callback],
      renderer: {
        bind: () => true,
        recordGam: () => true,
        recordFailure: () => true,
        retire: () => true,
        sweepCommittedArtifacts: () => 0,
        captureHandoff: () => [[], [], 0, 1, 1] as const,
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
    ).toThrow('tsjs');
  });
});
