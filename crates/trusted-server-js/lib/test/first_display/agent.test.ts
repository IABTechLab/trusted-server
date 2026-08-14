import { describe, expect, it, vi } from 'vitest';

import { createBootstrapController } from '../../src/core/bootstrap_controller';
import {
  createFirstDisplayAgent,
  firstDisplayProtocolCoverage,
  type FirstDisplayAuctionProtocolId,
  type FirstDisplayBatchOutcomeV1,
  type FirstDisplayBatchV1,
  type FirstDisplayDriver,
  type FirstDisplayTerminalResult,
} from '../../src/first_display/agent';

function batch(kinds: FirstDisplayBatchV1['outcomes'][number]['kind'][]): FirstDisplayBatchV1 {
  const requiredProtocols = Object.freeze([
    ...(kinds.includes('aps') ? (['aps'] as const) : []),
    ...(kinds.some((kind) => kind === 'aps' || kind === 'gpt_adm') ? (['gpt'] as const) : []),
  ]);
  return Object.freeze({
    version: 1,
    projectionDigest: 'a'.repeat(64),
    requiredProtocols,
    outcomes: Object.freeze(
      kinds.map((kind, index) => Object.freeze({ slotId: `slot-${index}`, kind }))
    ),
  });
}

function harness(options: { now?: number; hidden?: boolean } = {}) {
  let now = options.now ?? 0;
  const marks: string[] = [];
  const timers: Array<() => void> = [];
  const frames: Array<() => void> = [];
  const failures: string[] = [];
  const performance = { mark: (name: string) => marks.push(name) };
  const bootstrap = createBootstrapController({
    performance,
    now: () => now,
    setTimer: (callback) => {
      timers.push(callback);
      return callback;
    },
    clearTimer: (handle) => {
      const index = timers.indexOf(handle as () => void);
      if (index >= 0) timers.splice(index, 1);
    },
    onFailure: (reason) => failures.push(reason),
  });
  return {
    bootstrap,
    failures,
    frames,
    marks,
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
  let settle: ((slotId: string, result: 'accepted' | 'failed' | 'cancelled') => void) | undefined;
  return Object.freeze({
    start: (
      _outcomes: readonly FirstDisplayBatchOutcomeV1[],
      onFirstAction: () => boolean,
      onTerminal: (slotId: string, result: FirstDisplayTerminalResult) => void
    ) => {
      events.push('driver:start');
      onFirstAction();
      settle = onTerminal;
    },
    settleForTest: (slotId: string, result: FirstDisplayTerminalResult) => settle?.(slotId, result),
    sealTsAdmission: () => events.push('driver:seal'),
    dispose: () => events.push('driver:dispose'),
  });
}

describe('bounded first-display agent', () => {
  it('requires exact activated protocol coverage for the immutable batch', () => {
    const aps = Object.freeze({ version: 1, id: 'aps' });
    const gpt = Object.freeze({ version: 1, id: 'gpt' });
    const required = batch(['aps']);
    expect(
      firstDisplayProtocolCoverage(
        required,
        new Map<FirstDisplayAuctionProtocolId, unknown>([
          ['aps', aps],
          ['gpt', gpt],
        ])
      )
    ).toBe(true);
    expect(
      firstDisplayProtocolCoverage(
        required,
        new Map<FirstDisplayAuctionProtocolId, unknown>([['aps', aps]])
      )
    ).toBe(false);
    expect(
      firstDisplayProtocolCoverage(
        required,
        new Map<FirstDisplayAuctionProtocolId, unknown>([
          ['aps', aps],
          ['gpt', Object.freeze({ version: 1, id: 'prebid' })],
        ])
      )
    ).toBe(false);
    expect(
      firstDisplayProtocolCoverage(
        required,
        new Map<FirstDisplayAuctionProtocolId, unknown>([
          ['aps', Object.freeze({ version: 1, id: 'aps', extra: true })],
          ['gpt', gpt],
        ])
      )
    ).toBe(false);
    expect(
      firstDisplayProtocolCoverage(
        { ...required, requiredProtocols: Object.freeze(['gpt', 'aps']) },
        new Map<FirstDisplayAuctionProtocolId, unknown>([
          ['aps', aps],
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
      batch: batch(['no_bid', 'failed', 'cancelled']),
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
});
