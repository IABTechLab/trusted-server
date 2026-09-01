import { describe, expect, it } from 'vitest';

import {
  createFirstDisplayAgent,
  prepareFirstDisplayBase,
  type FirstDisplayAgentOptions,
  type FirstDisplayAgentRegistrationHostV1,
  type FirstDisplayBatchOutcomeV1,
  type FirstDisplayDriver,
  type FirstDisplayTerminalResult,
} from '../../src/first_display/agent';
import type { FirstDisplayRenderBridgeCapabilityV1 } from '../../src/first_display/driver';
import type { FirstDisplayGptProtocolV1 } from '../../src/first_display/leaf/gpt_protocol';
import type {
  FirstDisplayGoogletagBatchCallbacks,
  FirstDisplayGoogletagBatchInput,
  FirstDisplayGptBoundCycleV1,
} from '../../src/first_display/adapters/googletag';
import { snapshotFirstDisplayBatchV1 } from '../../src/first_display/leaf/projection';
import { createTestNavigationIdentityIssuer } from '../../src/kernel/identity';
import { finalizeFirstDisplayAgentCaptureV1 } from '../../src/shared/first_display_handoff';

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
  const startedAtMs = now;
  performance.mark('tsjs:bids-script');
  const owner = Object.freeze({
    startedAtMs,
    now: () => now,
    onSettled: () => undefined,
  });
  return {
    owner,
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
    sweepCommittedArtifacts: () => 0,
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

function production(
  driver: FirstDisplayDriver
): NonNullable<FirstDisplayAgentOptions['production']> {
  let captured: ReturnType<FirstDisplayDriver['captureHandoff']>;
  let closed: boolean | undefined;
  let detached: boolean | undefined;
  let disposed = false;
  const terminals = new Map<
    string,
    (result: FirstDisplayTerminalResult, reason: string | null) => void
  >();
  const capture = () => {
    if (captured === undefined) captured = driver.captureHandoff();
    return captured;
  };
  const close = () => {
    if (closed === undefined) closed = driver.closeIngress();
    return closed;
  };
  const detach = () => {
    if (detached === undefined) detached = driver.detachCommittedArtifacts();
    return detached;
  };
  const dispose = () => {
    if (disposed) return;
    disposed = true;
    driver.dispose();
  };
  const gpt: FirstDisplayGptProtocolV1 = Object.freeze([
    1,
    'gpt',
    (input: FirstDisplayGoogletagBatchInput) =>
      Object.freeze([
        (callbacks: FirstDisplayGoogletagBatchCallbacks) => {
          const outcomes: FirstDisplayBatchOutcomeV1[] = [];
          for (const decision of input[4].auction.results) {
            if (decision.outcome !== 'winner') continue;
            const bid = input[4].bids.find(
              ({ candidateId }) => candidateId === decision.candidateId
            );
            if (bid) {
              outcomes.push(
                Object.freeze({
                  kind: bid.renderSource.type === 'aps' ? 'aps' : 'gpt_adm',
                  slotId: decision.slot,
                })
              );
            }
          }
          for (const outcome of outcomes) {
            const bid = input[4].bids.find(({ slot }) => slot === outcome.slotId)!;
            const placement = input[4].slots.find(({ slot }) => slot === outcome.slotId)!;
            callbacks[0](
              Object.freeze([
                bid,
                document.createElement('div'),
                () => true,
                'trusted_server' as const,
                {},
                placement,
                outcome.slotId,
                `gt1_${outcome.slotId}`,
              ])
            );
          }
          driver.start(outcomes, callbacks[2], (slotId, result, reason) => {
            terminals.get(slotId)?.(result, reason);
          });
          return true;
        },
        () => close(),
        () =>
          capture()?.cycles.map((cycle) =>
            Object.freeze([cycle[6], cycle[1].id, cycle[3], cycle[8], cycle[7], cycle[4]] as const)
          ),
        () => {
          const value = capture();
          return value
            ? Object.freeze([
                value.diagnosticCycles.map((cycle) =>
                  Object.freeze([
                    cycle.slotId,
                    cycle.token,
                    cycle.nextCycleOrdinal,
                    cycle.unknownPriorCycle,
                    cycle.quarantines,
                    cycle.records.map((record) =>
                      Object.freeze([
                        record.ordinal,
                        record.responseIdentifier,
                        record.seen,
                        record.state,
                      ] as const)
                    ),
                  ] as const)
                ),
                value.gptDiagnostics.facts,
                value.nextTraceTokenOrdinal,
                value.gptDiagnostics.overflowCount,
                value.gptDiagnostics.dropCount,
              ] as const)
            : undefined;
        },
        () => detach(),
        dispose,
      ] as const),
  ] as const);
  const renderer: FirstDisplayRenderBridgeCapabilityV1 = Object.freeze([
    (
      cycle: FirstDisplayGptBoundCycleV1,
      onTerminal: (result: FirstDisplayTerminalResult, reason: string | null) => void
    ) => {
      terminals.set(cycle[6], onTerminal);
      return true;
    },
    () => true,
    () => true,
    () => true,
    () => driver.sweepCommittedArtifacts(),
    () => driver.sealTsAdmission(),
    () => close(),
    () => {
      const value = capture();
      return value
        ? Object.freeze([
            value.artifacts.map((artifact) =>
              Object.freeze([
                artifact.hostPosition,
                artifact.hostPositionPriority,
                artifact.identity,
                artifact.kind,
                artifact.owner,
                artifact.slotId,
                artifact.token,
              ] as const)
            ),
            value.tombstones.map((entry) =>
              Object.freeze([entry.kind, entry.value, entry.expiresAtMs, entry.ordinal] as const)
            ),
            value.clockEpochMs,
            value.nextReservationOrdinal,
            value.nextTicketOrdinal,
          ] as const)
        : undefined;
    },
    () => detach(),
    dispose,
  ] as const);
  return Object.freeze({
    gpt,
    gptInput: Object.freeze([
      window,
      (handle: unknown) => window.clearTimeout(handle as number),
      document,
      (callback: () => void, delayMs: number) => window.setTimeout(callback, delayMs),
    ] as const),
    renderer,
  });
}

describe('bounded first-display agent', () => {
  it.each(['gpt', 'render_owner'] as const)(
    'accepts the exact two-field %s installer receipt after registering its full protocol',
    (protocolId) => {
      const sliceId = `${protocolId}_initial` as const;
      const releases: Array<() => void> = [];
      const prepared = prepareFirstDisplayBase({
        options: {
          handoff: { slices: [sliceId] },
        },
        sliceBindings: (
          _id: string,
          _observe: (key: unknown, value: unknown) => void,
          register: ((protocol: unknown) => () => void) | undefined
        ) => Object.freeze([Object.freeze({ register }), undefined]),
      } as unknown as FirstDisplayAgentRegistrationHostV1);

      expect(() =>
        prepared.sliceHost.activate(
          sliceId,
          (release) => releases.push(release),
          (bindings) => {
            const register = (bindings as Readonly<{ register: (protocol: unknown) => () => void }>)
              .register;
            releases.push(register(Object.freeze([1, protocolId, Object.freeze({})] as const)));
            return Object.freeze([1, protocolId] as const);
          }
        )
      ).not.toThrow();
      expect(releases).toHaveLength(1);
    }
  );

  it('derives the protected action from the immutable projection and rejects outcome summaries', () => {
    const accepted = harness();
    const received: FirstDisplayBatchOutcomeV1[][] = [];
    const projectionAgent = createFirstDisplayAgent({
      batch: batch(['gpt_adm']),
      ...accepted.owner,
      production: production(
        Object.freeze({
          captureHandoff: () => undefined,
          closeIngress: () => true,
          detachCommittedArtifacts: () => true,
          sweepCommittedArtifacts: () => 0,
          start: (outcomes: readonly FirstDisplayBatchOutcomeV1[]) => {
            received.push([...outcomes]);
          },
          sealTsAdmission: () => undefined,
          dispose: () => undefined,
        })
      ),
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
      ...rejected.owner,
      production: production(driver([])),
      performance: rejected.performance,
      paint: rejected.paint,
      onProtectedPaint: () => undefined,
      onFailure: (reason) => rejected.failures.push(reason),
    });
    expect(summaryAgent.start()).toBe(false);
    expect(rejected.failures).toEqual(['abi_mismatch']);
  });

  it('emits the four authoritative marks around one mixed protected batch', () => {
    const h = harness();
    const events: string[] = [];
    const ownedDriver = driver(events);
    const agent = createFirstDisplayAgent({
      batch: batch(['no_bid', 'gpt_adm', 'aps']),
      ...h.owner,
      production: production(ownedDriver),
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
      ...h.owner,
      production: production(driver(events)),
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
      sweepCommittedArtifacts: () => 0,
      start: (_outcomes: readonly FirstDisplayBatchOutcomeV1[], onFirstAction: () => boolean) => {
        events.push('driver:prepared');
        action = onFirstAction;
      },
      sealTsAdmission: () => undefined,
      dispose: () => events.push('driver:dispose'),
    });
    const agent = createFirstDisplayAgent({
      batch: batch(['gpt_adm']),
      ...h.owner,
      production: production(ownedDriver),
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
      ...late.owner,
      production: production(
        Object.freeze({
          captureHandoff: () => undefined,
          closeIngress: () => true,
          detachCommittedArtifacts: () => true,
          sweepCommittedArtifacts: () => 0,
          start: (
            _outcomes: readonly FirstDisplayBatchOutcomeV1[],
            onFirstAction: () => boolean
          ) => {
            lateAction = onFirstAction;
          },
          sealTsAdmission: () => undefined,
          dispose: () => undefined,
        })
      ),
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
        ...h.owner,
        production: production(driver(events)),
        performance: h.performance,
        paint: h.paint,
        onProtectedPaint: () => undefined,
        onFailure: (reason) => h.failures.push(reason),
      });
      expect(agent.start()).toBe(accepted);
      expect(events).toEqual(accepted ? ['driver:start'] : ['driver:dispose']);
      expect(h.failures).toEqual(accepted ? [] : ['bundle_partial']);
    }
  });

  it('seals the immutable TS batch at paint while native activity remains observable', () => {
    const h = harness();
    const events: string[] = [];
    const agent = createFirstDisplayAgent({
      batch: batch(['no_bid']),
      ...h.owner,
      production: production(driver(events)),
      performance: h.performance,
      paint: h.paint,
      onProtectedPaint: () => undefined,
      onFailure: (reason) => h.failures.push(reason),
    });
    agent.start();
    h.frames.shift()?.();
    h.frames.shift()?.();

    expect(agent.observeNativeMutation()).toBe(true);
    expect(agent.mutationRevision).toBe(1);
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
      ...h.owner,
      production: production(driver([])),
      performance: h.performance,
      paint: h.paint,
      mutationDocument: document,
      handoff: {
        releaseId: 'a'.repeat(64),
        generation: 1,
        integrationConfigDigest: 'c'.repeat(64),
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
    const finalized = agent.finalizeHandoff(finalizeFirstDisplayAgentCaptureV1);
    expect(finalized?.handoff.mutationRevision).toBe(1);
    expect(h.failures).toEqual([]);

    agent.dispose();
    host.remove();
  });

  it('fails closed on malformed batches, driver throws, replay, and revision exhaustion', () => {
    const malformed = harness();
    const invalidAgent = createFirstDisplayAgent({
      batch: { ...batch(['no_bid']), extra: true },
      ...malformed.owner,
      production: production(driver([])),
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
      sweepCommittedArtifacts: () => 0,
      start: () => {
        throw new Error('boom');
      },
      sealTsAdmission: () => undefined,
      dispose: () => undefined,
    });
    const failedAgent = createFirstDisplayAgent({
      batch: batch(['aps']),
      ...throwing.owner,
      production: production(throwingDriver),
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
      ...revision.owner,
      production: production(driver([])),
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

  it('runs every independent driver disposer when an earlier disposer throws', () => {
    const h = harness();
    const cleanup: string[] = [];
    const gpt: FirstDisplayGptProtocolV1 = Object.freeze([
      1,
      'gpt',
      () =>
        Object.freeze([
          (callbacks: FirstDisplayGoogletagBatchCallbacks) => callbacks[2](),
          () => true,
          () => undefined,
          () => undefined,
          () => true,
          () => {
            cleanup.push('gpt');
            throw new Error('fictional GPT cleanup failure');
          },
        ] as const),
    ]);
    const renderer: FirstDisplayRenderBridgeCapabilityV1 = Object.freeze([
      () => true,
      () => true,
      () => true,
      () => true,
      () => 0,
      () => undefined,
      () => true,
      () => undefined,
      () => true,
      () => cleanup.push('renderer'),
    ]);
    const agent = createFirstDisplayAgent({
      batch: batch(['gpt_adm']),
      ...h.owner,
      production: Object.freeze({
        gpt,
        gptInput: Object.freeze([
          window,
          (handle: unknown) => window.clearTimeout(handle as number),
          document,
          (callback: () => void, delayMs: number) => window.setTimeout(callback, delayMs),
        ] as const),
        renderer,
      }),
      performance: h.performance,
      paint: h.paint,
      onProtectedPaint: () => undefined,
      onFailure: (reason) => h.failures.push(reason),
    });

    expect(agent.start()).toBe(true);
    expect(() => agent.dispose()).not.toThrow();
    expect(cleanup).toEqual(['gpt', 'renderer']);
    expect(agent.state).toBe('disposed');
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
              hostPosition: null,
              hostPositionPriority: null,
              identity: artifact,
              kind: 'gpt_adm' as const,
              owner: 'trusted_server' as const,
              slotId: 'slot-0',
              token: projectedBatch.projection.bids[0]!.rendererReservationId,
            }),
          ]),
          cycles: Object.freeze([
            Object.freeze([
              projectedBatch.projection.bids[0]!,
              element,
              () => true,
              'trusted_server' as const,
              physicalSlot,
              projectedBatch.projection.slots[0]!,
              'slot-0',
              'gt1_1',
              Object.freeze([]),
            ] as const),
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
      sweepCommittedArtifacts: () => 0,
      dispose: () => events.push('dispose'),
    });
    const agent = createFirstDisplayAgent({
      batch: batch(['gpt_adm', 'gpt_adm']),
      ...h.owner,
      production: production(ownedDriver),
      performance: h.performance,
      paint: h.paint,
      onProtectedPaint: () => undefined,
      onFailure: (reason) => h.failures.push(reason),
      identityIssuer: issuer.value,
      parserState: () =>
        Object.freeze([
          Object.freeze([
            'gpt_initial',
            Object.freeze([
              Object.freeze(['gam', false] as const),
              Object.freeze(['v', 1] as const),
            ]),
          ] as const),
        ]),
      handoff: Object.freeze({
        releaseId: 'a'.repeat(64),
        generation: 1,
        integrationConfigDigest: 'c'.repeat(64),
        slices: Object.freeze(['first_display', 'gpt_initial'] as const),
      }),
    });
    expect(agent.start()).toBe(true);
    expect(agent.finalizeHandoff(finalizeFirstDisplayAgentCaptureV1)).toBeUndefined();
    h.frames.shift()?.();
    h.frames.shift()?.();

    const finalized = agent.finalizeHandoff(finalizeFirstDisplayAgentCaptureV1);
    expect(events.slice(-2)).toEqual(['close', 'capture']);
    expect(finalized?.handoff).toMatchObject({
      captureVersion: 1,
      releaseId: 'a'.repeat(64),
      generation: 1,
      mutationRevision: 0,
      identityCount: 2,
    });
    const data = finalized?.handoff['data'] as readonly unknown[];
    expect(data).toHaveLength(14);
    expect(data.slice(0, 3)).toEqual([
      'b'.repeat(64),
      'c'.repeat(64),
      ['first_display', 'gpt_initial'],
    ]);
    expect(data[3]).toEqual([
      ['accepted', null, 0, 1],
      ['failed', 'renderer_document_no_load', null, null],
    ]);
    expect(data[4]).toEqual([['slot-0', 'slot-0', 'trusted_server', [], 'gt1_1']]);
    expect(data[7]).toEqual([
      [
        'gpt_initial',
        [
          ['gam', false],
          ['v', 1],
        ],
      ],
    ]);
    expect(finalized?.handoff).not.toHaveProperty('attempts');
    expect(finalized?.capsule.consume('a'.repeat(64), 1)).toEqual([physicalSlot, artifact]);
    expect(agent.detachCommittedArtifacts()).toBe(true);
    agent.dispose();
    expect(events).toContain('detach');
  });
});
