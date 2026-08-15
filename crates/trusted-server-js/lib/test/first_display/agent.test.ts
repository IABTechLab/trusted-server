import { describe, expect, it, vi } from 'vitest';

import {
  createFirstDisplayAgent,
  type FirstDisplayAuctionProtocolId,
  type FirstDisplayBatchOutcomeV1,
  type FirstDisplayBootstrapController,
  type FirstDisplayDriver,
  type FirstDisplayTerminalResult,
} from '../../src/first_display/agent';
import { snapshotFirstDisplayBatchV1 } from '../../src/first_display/leaf/projection';
import { createTestNavigationIdentityIssuer } from '../../src/kernel/identity';

function batch(
  kinds: readonly ('no_bid' | 'failed' | 'gpt_adm' | 'aps')[]
): Readonly<Record<string, unknown>> {
  const decisions: object[] = [];
  const slots: object[] = [];
  const bids: object[] = [];
  let winner = 0;
  for (let index = 0; index < kinds.length; index += 1) {
    const kind = kinds[index];
    const slot = `slot-${index}`;
    slots.push(
      Object.freeze({
        slot,
        gamUnitPath: `/123/example-${index}`,
        divId: slot,
        formats: Object.freeze([Object.freeze([300, 250])]),
        targeting: Object.freeze({ placement: `article-${index}` }),
      })
    );
    if (kind === 'no_bid') {
      decisions.push(Object.freeze({ slot, outcome: 'no_bid' }));
      continue;
    }
    if (kind === 'failed') {
      decisions.push(Object.freeze({ slot, outcome: 'failed', reason: 'internal_error' }));
      continue;
    }
    const candidateId = `c${String(winner).padStart(11, '0')}`;
    winner += 1;
    decisions.push(Object.freeze({ slot, outcome: 'winner', candidateId }));
    bids.push(
      Object.freeze({
        candidateId,
        slot,
        provider: 'example',
        upstreamBidId: `upstream-${index}`,
        cpm: 1.25,
        currency: 'USD',
        targeting: Object.freeze({ hb_pb: '1.25' }),
        rendererReservationId: `r1_${String(index).padStart(22, 'a')}`,
        renderSource:
          kind === 'aps'
            ? Object.freeze({
                type: 'aps',
                version: 1,
                accountId: 'account-1',
                bidId: `bid-${index}`,
                tagType: 'iframe',
                creativeUrl: 'https://creative.example/render',
                aaxResponse: '',
                width: 300,
                height: 250,
              })
            : Object.freeze({
                type: 'adm',
                version: 1,
                adm: '<div>example</div>',
                width: 300,
                height: 250,
              }),
      })
    );
  }
  return Object.freeze({
    version: 1,
    projectionDigest: 'b'.repeat(64),
    projection: Object.freeze({
      version: 1,
      auction: Object.freeze({
        version: 1,
        auctionId: 'initial',
        results: Object.freeze(decisions),
      }),
      slots: Object.freeze(slots),
      bids: Object.freeze(bids),
    }),
  });
}

function harness(options: { now?: number; hidden?: boolean } = {}) {
  let now = options.now ?? 0;
  const marks: string[] = [];
  const measures: Array<readonly [string, string, string]> = [];
  const timers: Array<() => void> = [];
  const frames: Array<() => void> = [];
  const failures: string[] = [];
  const performance = {
    mark: (name: string) => marks.push(name),
    measure: (name: string, start: string, end: string) => measures.push([name, start, end]),
  };
  let bootstrapState: FirstDisplayBootstrapController['state'] = 'installing';
  const startedAtMs = now;
  const deadline = () => !Number.isFinite(now - startedAtMs) || now - startedAtMs >= 10_000;
  const fail = (reason: 'abi_mismatch' | 'bundle_partial'): boolean => {
    if (bootstrapState === 'settled' || bootstrapState === 'failed') return false;
    bootstrapState = 'failed';
    failures.push(reason);
    return true;
  };
  performance.mark('tsjs:bids-script');
  const bootstrap: FirstDisplayBootstrapController = Object.freeze({
    get state() {
      return bootstrapState;
    },
    startedAtMs,
    registerAgent: () => {
      if (bootstrapState !== 'installing') return false;
      if (deadline()) return fail('bundle_partial') && false;
      bootstrapState = 'agent_registered';
      return true;
    },
    startAction: () => {
      if (bootstrapState !== 'agent_registered') return false;
      if (deadline()) return fail('bundle_partial') && false;
      bootstrapState = 'action_started';
      return true;
    },
    settle: () => {
      if (bootstrapState !== 'agent_registered' && bootstrapState !== 'action_started') {
        return false;
      }
      bootstrapState = 'settled';
      return true;
    },
    fail,
  });
  return {
    bootstrap,
    failures,
    frames,
    marks,
    measures,
    timers,
    performance,
    now: () => now,
    setNow: (value: number) => {
      now = value;
    },
    paint: {
      hidden: () => options.hidden === true,
      requestFrame: (callback: () => void) => frames.push(callback),
      scheduleHidden: (callback: () => void) => timers.push(callback),
    },
  };
}

function driver(events: string[]): FirstDisplayDriver & {
  settleForTest: (slotId: string, result: 'accepted' | 'failed' | 'cancelled') => void;
} {
  let settle:
    | ((slotId: string, result: 'accepted' | 'failed' | 'cancelled', reason: string | null) => void)
    | undefined;
  return Object.freeze({
    captureHandoff: () =>
      Object.freeze({
        artifacts: Object.freeze([]),
        clockEpochMs: 0,
        cycles: Object.freeze([]),
        diagnosticCycles: Object.freeze([]),
        gptDiagnostics: Object.freeze({
          facts: Object.freeze([]),
          overflowCount: 0,
          dropCount: 0,
        }),
        identities: Object.freeze([]),
        nextReservationOrdinal: 1,
        nextTraceTokenOrdinal: 1,
        nextTicketOrdinal: 1,
        tombstones: Object.freeze([]),
      }),
    closeIngress: () => true,
    detachCommittedArtifacts: () => true,
    start: (
      _outcomes: readonly FirstDisplayBatchOutcomeV1[],
      onFirstAction: () => boolean,
      onTerminal: (
        slotId: string,
        result: FirstDisplayTerminalResult,
        reason: string | null
      ) => void
    ) => {
      events.push('driver:start');
      onFirstAction();
      settle = onTerminal;
    },
    settleForTest: (slotId: string, result: FirstDisplayTerminalResult) =>
      settle?.(slotId, result, result === 'accepted' ? null : 'internal_error'),
    sealTsAdmission: () => events.push('driver:seal'),
    dispose: () => events.push('driver:dispose'),
  });
}

describe('bounded first-display agent', () => {
  it('derives the protected action from the immutable projection and rejects outcome summaries', () => {
    const accepted = harness();
    const received: FirstDisplayBatchOutcomeV1[][] = [];
    const projectionAgent = createFirstDisplayAgent({
      batch: batch(['gpt_adm']),
      bootstrap: accepted.bootstrap,
      driver: Object.freeze({
        captureHandoff: () => undefined,
        closeIngress: () => true,
        detachCommittedArtifacts: () => true,
        start: (outcomes: readonly FirstDisplayBatchOutcomeV1[]) => {
          received.push([...outcomes]);
        },
        sealTsAdmission: () => undefined,
        dispose: () => undefined,
      }),
      performance: accepted.performance,
      paint: accepted.paint,
      onProtectedPaint: () => undefined,
      onFailure: (reason) => accepted.failures.push(reason),
    });

    expect(projectionAgent.start()).toBe(true);
    expect(received).toEqual([[{ slotId: 'slot-0', kind: 'gpt_adm' }]]);

    const rejected = harness();
    const summaryAgent = createFirstDisplayAgent({
      batch: Object.freeze({
        version: 1,
        projectionDigest: 'b'.repeat(64),
        requiredProtocols: Object.freeze(['gpt']),
        outcomes: Object.freeze([Object.freeze({ slotId: 'slot-0', kind: 'gpt_adm' })]),
      }),
      bootstrap: rejected.bootstrap,
      driver: driver([]),
      performance: rejected.performance,
      paint: rejected.paint,
      onProtectedPaint: () => undefined,
      onFailure: (reason) => rejected.failures.push(reason),
    });
    expect(summaryAgent.start()).toBe(false);
    expect(rejected.failures).toEqual(['abi_mismatch']);
  });

  it('requires exact activated protocol coverage for the immutable batch', () => {
    const h = harness();
    const coverage = createFirstDisplayAgent({
      batch: batch(['aps']),
      bootstrap: h.bootstrap,
      driver: driver([]),
      performance: h.performance,
      paint: h.paint,
      onProtectedPaint: () => undefined,
      onFailure: () => undefined,
    });
    const aps = Object.freeze({ version: 1, id: 'aps' });
    const gpt = Object.freeze({ version: 1, id: 'gpt' });
    expect(
      coverage.coversProtocols(
        new Map<FirstDisplayAuctionProtocolId, unknown>([
          ['aps', aps],
          ['gpt', gpt],
        ])
      )
    ).toBe(true);
    expect(
      coverage.coversProtocols(new Map<FirstDisplayAuctionProtocolId, unknown>([['aps', aps]]))
    ).toBe(false);
    expect(
      coverage.coversProtocols(
        new Map<FirstDisplayAuctionProtocolId, unknown>([
          ['aps', aps],
          ['gpt', Object.freeze({ version: 1, id: 'prebid' })],
        ])
      )
    ).toBe(false);
    expect(
      coverage.coversProtocols(
        new Map<FirstDisplayAuctionProtocolId, unknown>([
          ['aps', Object.freeze({ version: 1, id: 'aps', extra: true })],
          ['gpt', gpt],
        ])
      )
    ).toBe(false);
  });
  it('emits the four authoritative marks around one mixed protected batch', () => {
    const h = harness();
    const events: string[] = [];
    const ownedDriver = driver(events);
    const agent = createFirstDisplayAgent({
      batch: batch(['no_bid', 'gpt_adm', 'aps']),
      bootstrap: h.bootstrap,
      driver: ownedDriver,
      performance: h.performance,
      paint: h.paint,
      onProtectedPaint: () => events.push('protected:paint'),
      onFailure: (reason) => h.failures.push(reason),
    });

    expect(agent.start()).toBe(true);
    expect(h.marks).toEqual(['tsjs:bids-script', 'tsjs:first-display']);
    expect(h.measures).toEqual([
      ['tsjs:boot-to-first-display', 'tsjs:bids-script', 'tsjs:first-display'],
    ]);
    expect(events).toEqual(['driver:start']);
    ownedDriver.settleForTest('slot-1', 'accepted');
    expect(h.marks).toEqual(['tsjs:bids-script', 'tsjs:first-display']);
    ownedDriver.settleForTest('slot-2', 'failed');
    expect(h.marks).toEqual([
      'tsjs:bids-script',
      'tsjs:first-display',
      'tsjs:first-display-terminal',
    ]);
    expect(h.frames).toHaveLength(1);
    h.frames.shift()?.();
    expect(h.frames).toHaveLength(1);
    h.frames.shift()?.();
    expect(h.marks).toEqual([
      'tsjs:bids-script',
      'tsjs:first-display',
      'tsjs:first-display-terminal',
      'tsjs:first-display-paint',
    ]);
    expect(events).toEqual(['driver:start', 'driver:seal', 'protected:paint']);
    expect(agent.state).toBe('painted');
  });

  it('does not manufacture a first-action mark for an all-terminal empty action set', () => {
    const h = harness({ hidden: true });
    const events: string[] = [];
    const agent = createFirstDisplayAgent({
      batch: batch(['no_bid', 'failed', 'failed']),
      bootstrap: h.bootstrap,
      driver: driver(events),
      performance: h.performance,
      paint: h.paint,
      onProtectedPaint: () => events.push('protected:paint'),
      onFailure: (reason) => h.failures.push(reason),
    });

    expect(agent.start()).toBe(true);
    expect(h.marks).toEqual(['tsjs:bids-script', 'tsjs:first-display-terminal']);
    h.timers.shift()?.();
    h.timers.shift()?.();
    expect(h.marks).toEqual([
      'tsjs:bids-script',
      'tsjs:first-display-terminal',
      'tsjs:first-display-paint',
    ]);
    expect(events).toEqual(['driver:seal', 'protected:paint']);
  });

  it('records first-display only at the responsible action and rejects a late or replayed action', () => {
    const h = harness();
    let action: (() => boolean) | undefined;
    const events: string[] = [];
    const ownedDriver: FirstDisplayDriver = Object.freeze({
      captureHandoff: () => undefined,
      closeIngress: () => true,
      detachCommittedArtifacts: () => true,
      start: (_outcomes: readonly FirstDisplayBatchOutcomeV1[], onFirstAction: () => boolean) => {
        events.push('driver:prepared');
        action = onFirstAction;
      },
      sealTsAdmission: () => undefined,
      dispose: () => events.push('driver:dispose'),
    });
    const agent = createFirstDisplayAgent({
      batch: batch(['gpt_adm']),
      bootstrap: h.bootstrap,
      driver: ownedDriver,
      performance: h.performance,
      paint: h.paint,
      onProtectedPaint: () => undefined,
      onFailure: (reason) => h.failures.push(reason),
    });

    expect(agent.start()).toBe(true);
    expect(events).toEqual(['driver:prepared']);
    expect(h.marks).toEqual(['tsjs:bids-script']);
    expect(action?.()).toBe(true);
    expect(h.marks).toEqual(['tsjs:bids-script', 'tsjs:first-display']);
    expect(action?.()).toBe(false);
    expect(agent.state).toBe('failed');
    expect(h.failures).toEqual(['bundle_partial']);

    const late = harness();
    let lateAction: (() => boolean) | undefined;
    const lateAgent = createFirstDisplayAgent({
      batch: batch(['aps']),
      bootstrap: late.bootstrap,
      driver: Object.freeze({
        captureHandoff: () => undefined,
        closeIngress: () => true,
        detachCommittedArtifacts: () => true,
        start: (_outcomes: readonly FirstDisplayBatchOutcomeV1[], onFirstAction: () => boolean) => {
          lateAction = onFirstAction;
        },
        sealTsAdmission: () => undefined,
        dispose: () => undefined,
      }),
      performance: late.performance,
      paint: late.paint,
      onProtectedPaint: () => undefined,
      onFailure: (reason) => late.failures.push(reason),
    });
    expect(lateAgent.start()).toBe(true);
    late.setNow(10_000);
    expect(lateAction?.()).toBe(false);
    expect(late.failures).toEqual(['bundle_partial']);
    expect(late.marks).toEqual(['tsjs:bids-script']);
  });

  it('uses the same 10-second deadline for registration, activation, and action start', () => {
    for (const [startedAt, accepted] of [
      [9_999, true],
      [10_000, false],
      [10_001, false],
    ] as const) {
      const h = harness();
      h.setNow(startedAt);
      const events: string[] = [];
      const agent = createFirstDisplayAgent({
        batch: batch(['gpt_adm']),
        bootstrap: h.bootstrap,
        driver: driver(events),
        performance: h.performance,
        paint: h.paint,
        onProtectedPaint: () => undefined,
        onFailure: (reason) => h.failures.push(reason),
      });
      expect(agent.start()).toBe(accepted);
      expect(events).toEqual(accepted ? ['driver:start'] : []);
      expect(h.failures).toEqual(accepted ? [] : ['bundle_partial']);
    }
  });

  it('seals TS bidder admission at paint while native activity remains observable', () => {
    const h = harness();
    const events: string[] = [];
    const complete = vi.fn();
    const agent = createFirstDisplayAgent({
      batch: batch(['no_bid']),
      bootstrap: h.bootstrap,
      driver: driver(events),
      performance: h.performance,
      paint: h.paint,
      onProtectedPaint: () => undefined,
      onFailure: (reason) => h.failures.push(reason),
      onPrebidAdmissionFailure: (reason) => events.push(reason),
    });
    agent.start();
    h.frames.shift()?.();
    h.frames.shift()?.();

    expect(agent.admitTsBid(complete)).toBe(false);
    expect(complete).toHaveBeenCalledTimes(1);
    expect(events).toContain('prebid_admission_failed');
    expect(agent.observeNativeMutation()).toBe(true);
    expect(agent.snapshot().mutationRevision).toBe(1);
  });

  it('drains pending DOM mutation records into the final handoff revision', () => {
    const h = harness();
    const host = document.createElement('div');
    document.body.append(host);
    const issuer = createTestNavigationIdentityIssuer({
      getRandomValues: (target) => target,
    });
    if (!issuer.ok) throw new Error('Expected first-display identity issuer');
    const agent = createFirstDisplayAgent({
      batch: batch(['no_bid']),
      bootstrap: h.bootstrap,
      driver: driver([]),
      performance: h.performance,
      paint: h.paint,
      mutationDocument: document,
      handoff: {
        releaseId: 'a'.repeat(64),
        generation: 1,
        slices: ['first_display'],
      },
      identityIssuer: issuer.value,
      onProtectedPaint: () => undefined,
      onFailure: (reason) => h.failures.push(reason),
    });
    expect(agent.start()).toBe(true);
    h.frames.shift()?.();
    h.frames.shift()?.();

    host.append(document.createElement('span'));
    const finalized = agent.finalizeHandoff();
    expect(finalized?.handoff.mutationRevision).toBe(1);
    expect(h.failures).toEqual([]);

    agent.dispose();
    host.remove();
  });

  it('fails closed on malformed batches, driver throws, replay, and revision exhaustion', () => {
    const malformed = harness();
    const invalidAgent = createFirstDisplayAgent({
      batch: { ...batch(['no_bid']), extra: true },
      bootstrap: malformed.bootstrap,
      driver: driver([]),
      performance: malformed.performance,
      paint: malformed.paint,
      onProtectedPaint: () => undefined,
      onFailure: (reason) => malformed.failures.push(reason),
    });
    expect(invalidAgent.start()).toBe(false);
    expect(malformed.failures).toEqual(['abi_mismatch']);

    const throwing = harness();
    const throwingDriver: FirstDisplayDriver = Object.freeze({
      captureHandoff: () => undefined,
      closeIngress: () => true,
      detachCommittedArtifacts: () => true,
      start: () => {
        throw new Error('boom');
      },
      sealTsAdmission: () => undefined,
      dispose: () => undefined,
    });
    const failedAgent = createFirstDisplayAgent({
      batch: batch(['aps']),
      bootstrap: throwing.bootstrap,
      driver: throwingDriver,
      performance: throwing.performance,
      paint: throwing.paint,
      onProtectedPaint: () => undefined,
      onFailure: (reason) => throwing.failures.push(reason),
    });
    expect(failedAgent.start()).toBe(false);
    expect(failedAgent.start()).toBe(false);
    expect(throwing.failures).toEqual(['bundle_partial']);

    const revision = harness();
    const revisionAgent = createFirstDisplayAgent({
      batch: batch(['no_bid']),
      bootstrap: revision.bootstrap,
      driver: driver([]),
      performance: revision.performance,
      paint: revision.paint,
      onProtectedPaint: () => undefined,
      onFailure: (reason) => revision.failures.push(reason),
      initialMutationRevision: 4_294_967_295,
    });
    expect(revisionAgent.start()).toBe(true);
    revision.frames.shift()?.();
    revision.frames.shift()?.();
    expect(revisionAgent.observeNativeMutation()).toBe(false);
    expect(revision.failures).toEqual(['bundle_partial']);
  });

  it('mints the final immutable handoff only after paint and closes ingress before capture', () => {
    const h = harness();
    const events: string[] = [];
    const physicalSlot = {};
    const artifact = {};
    const projectedBatch = snapshotFirstDisplayBatchV1(batch(['gpt_adm', 'gpt_adm']))!;
    const issuer = createTestNavigationIdentityIssuer({
      getRandomValues: (target) => {
        target.set([0, 1, 2, 3, 4, 5, 6, 7]);
        return target;
      },
    });
    if (!issuer.ok) throw new Error('Expected first-display identity issuer');
    const element = document.createElement('div');
    element.id = 'slot-0';
    const ownedDriver: FirstDisplayDriver = Object.freeze({
      start: (
        _outcomes: readonly FirstDisplayBatchOutcomeV1[],
        onFirstAction: () => boolean,
        onTerminal: (
          slotId: string,
          result: FirstDisplayTerminalResult,
          reason: string | null
        ) => void
      ) => {
        onFirstAction();
        onTerminal('slot-0', 'accepted', null);
        onTerminal('slot-1', 'failed', 'renderer_document_no_load');
      },
      sealTsAdmission: () => events.push('seal'),
      closeIngress: () => {
        events.push('close');
        return true;
      },
      captureHandoff: () => {
        events.push('capture');
        return Object.freeze({
          artifacts: Object.freeze([
            Object.freeze({
              identity: artifact,
              kind: 'gpt_adm' as const,
              owner: 'trusted_server' as const,
              slotId: 'slot-0',
              token: projectedBatch.projection.bids[0]!.rendererReservationId,
            }),
          ]),
          cycles: Object.freeze([
            Object.freeze({
              bid: projectedBatch.projection.bids[0]!,
              element,
              ownership: 'trusted_server' as const,
              physicalSlot,
              placement: projectedBatch.projection.slots[0]!,
              slotId: 'slot-0',
              traceToken: 'gt1_1',
            }),
          ]),
          diagnosticCycles: Object.freeze([
            Object.freeze({
              nextCycleOrdinal: 2,
              quarantines: Object.freeze([]),
              records: Object.freeze([
                Object.freeze({
                  ordinal: 1,
                  responseIdentifier: 'response-one',
                  seen: Object.freeze(['slotRequested', 'slotRenderEnded'] as const),
                  state: 'completed' as const,
                }),
              ]),
              slotId: 'slot-0',
              token: 'gt1_1',
              unknownPriorCycle: false,
            }),
          ]),
          gptDiagnostics: Object.freeze({
            facts: Object.freeze([]),
            overflowCount: 0,
            dropCount: 0,
          }),
          identities: Object.freeze([physicalSlot, artifact]),
          clockEpochMs: 0,
          nextReservationOrdinal: 1,
          nextTraceTokenOrdinal: 2,
          nextTicketOrdinal: 1,
          tombstones: Object.freeze([]),
        });
      },
      detachCommittedArtifacts: () => {
        events.push('detach');
        return true;
      },
      dispose: () => events.push('dispose'),
    });
    const agent = createFirstDisplayAgent({
      batch: batch(['gpt_adm', 'gpt_adm']),
      bootstrap: h.bootstrap,
      driver: ownedDriver,
      performance: h.performance,
      paint: h.paint,
      onProtectedPaint: () => undefined,
      onFailure: (reason) => h.failures.push(reason),
      identityIssuer: issuer.value,
      parserState: () =>
        Object.freeze([
          Object.freeze({
            sliceId: 'gpt_initial',
            observations: Object.freeze(['protocol_version']),
            values: Object.freeze([Object.freeze(['protocol_version', 1] as const)]),
          }),
        ]),
      handoff: Object.freeze({
        releaseId: 'a'.repeat(64),
        generation: 1,
        slices: Object.freeze(['first_display', 'gpt_initial'] as const),
      }),
    });
    expect(agent.start()).toBe(true);
    expect(agent.finalizeHandoff()).toBeUndefined();
    h.frames.shift()?.();
    h.frames.shift()?.();

    const finalized = agent.finalizeHandoff();
    expect(events.slice(-2)).toEqual(['close', 'capture']);
    expect(finalized?.handoff).toMatchObject({
      releaseId: 'a'.repeat(64),
      generation: 1,
      projectionDigest: 'b'.repeat(64),
      slices: ['first_display', 'gpt_initial'],
      mutationRevision: 0,
      parserState: [
        {
          sliceId: 'gpt_initial',
          observations: ['protocol_version'],
          values: [['protocol_version', 1]],
        },
      ],
      attempts: [
        expect.objectContaining({ id: 'a1_AAECAwQFBgcAAAAAAAAAAQ', ordinal: 1 }),
        expect.objectContaining({
          id: 'a1_AAECAwQFBgcAAAAAAAAAAg',
          ordinal: 2,
          reason: 'renderer_document_no_load',
          state: 'failed',
        }),
      ],
      highWater: expect.objectContaining({
        navigationAttemptPrefix: 'AAECAwQFBgc',
        nextNavigationAttemptOrdinal: 3,
      }),
      trace: {
        nextGlobalSlotOrdinal: 2,
        nextSequence: 2,
        slots: [
          {
            bindings: [
              {
                atMs: 0,
                cycleOrdinal: 1,
                historySequence: 1,
                state: 'completed',
                token: 'gt1_1',
              },
            ],
            impressions: 1,
            slotId: 'slot-0',
          },
          { bindings: [], impressions: 0, slotId: 'slot-1' },
        ],
      },
    });
    expect(finalized?.capsule.consume('a'.repeat(64), 1)).toEqual([physicalSlot, artifact]);
    expect(agent.detachCommittedArtifacts()).toBe(true);
    agent.dispose();
    expect(events).toContain('detach');
  });
});
