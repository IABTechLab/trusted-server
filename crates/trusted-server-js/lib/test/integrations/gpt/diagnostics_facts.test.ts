import { describe, expect, expectTypeOf, it, vi } from 'vitest';

import type {
  GoogletagAdapter,
  GoogletagDiagnosticsFact,
  GoogletagDiagnosticsObserver,
  GoogletagFacade,
} from '../../../src/adapters/googletag';
import {
  activateGptDiagnosticsEventListeners,
  activateGptDiagnosticsFactCapture,
  createGptDiagnosticsFactBuffer,
  createTrustedServerOpportunityFact,
  projectGptTraceFact,
  type GptDiagnosticsFact,
} from '../../../src/integrations/gpt/diagnostics_facts';

function fact(index: number): Readonly<GoogletagDiagnosticsFact> {
  return Object.freeze({
    kind: 'slotRequested',
    observedAtMs: index,
    slot: Object.freeze({
      token: Object.freeze(Object.create(null) as object),
      elementId: `slot-${index}`,
    }),
  });
}

describe('GPT diagnostics fact transport', () => {
  it('copies and delivers exact Trusted Server requested-size evidence before GPT callbacks', () => {
    const slot = Object.freeze({
      token: Object.freeze(Object.create(null) as object),
      traceToken: 'gt1_1' as never,
      runtimeSlotNumber: 1,
      cycleOrdinal: 1 as never,
      elementId: 'fictional-slot',
    });
    const formats: Array<[number, number]> = [
      [300, 250],
      [728, 90],
    ];
    const opportunity = createTrustedServerOpportunityFact({
      auctionSlotId: 'fictional-slot',
      opportunity: 'renderable_candidate',
      requestedSlotSizes: formats,
      slot,
      trustedServerAuctionId: 'fictional-auction',
    });
    formats[0]![0] = 1;
    const received: Readonly<GptDiagnosticsFact>[] = [];
    const buffer = createGptDiagnosticsFactBuffer();

    expect(opportunity).toEqual({
      kind: 'trustedServerOpportunity',
      auctionSlotId: 'fictional-slot',
      opportunity: 'renderable_candidate',
      requestedSlotSizes: [
        [300, 250],
        [728, 90],
      ],
      slot,
      trustedServerAuctionId: 'fictional-auction',
    });
    expect(Object.isFrozen(opportunity)).toBe(true);
    expect(Object.isFrozen(opportunity?.requestedSlotSizes)).toBe(true);
    expect(Object.isFrozen(opportunity?.requestedSlotSizes?.[0])).toBe(true);
    expect(buffer.publish(opportunity!)).toBe(true);
    buffer.activate((candidate) => received.push(candidate));
    expect(received).toEqual([opportunity]);
  });

  it('projects only the data-safe exact trace identity and preserves event fields', () => {
    const opaqueToken = Object.freeze(Object.create(null) as object);
    const projected = projectGptTraceFact(
      Object.freeze({
        kind: 'slotRenderEnded',
        observedAtMs: 12.5,
        slot: Object.freeze({
          token: opaqueToken,
          traceToken: 'gt1_z',
          cycleOrdinal: 7,
          elementId: 'fictional-slot',
          adUnitPath: '/example/fictional-slot',
        }),
        isEmpty: false,
        responseIdentifier: 'fictional-response',
      }) as Readonly<GoogletagDiagnosticsFact>
    );

    expect(projected).toEqual({
      kind: 'slotRenderEnded',
      observedAtMs: 12.5,
      slot: { token: 'gt1_z', cycleOrdinal: 7, elementId: 'fictional-slot' },
      isEmpty: false,
      responseIdentifier: 'fictional-response',
    });
    expect(Object.isFrozen(projected)).toBe(true);
    expect(Object.isFrozen(projected?.slot)).toBe(true);
    expect(Reflect.ownKeys(projected?.slot ?? {}).sort()).toEqual([
      'cycleOrdinal',
      'elementId',
      'token',
    ]);
    expect(Object.values(projected?.slot ?? {})).not.toContain(opaqueToken);
    expect(JSON.stringify(projected)).not.toContain('/example/fictional-slot');
  });

  it.each([
    ['missing cycle', Object.freeze({ token: Object.freeze({}), traceToken: 'gt1_1' })],
    [
      'zero cycle',
      Object.freeze({ token: Object.freeze({}), traceToken: 'gt1_1', cycleOrdinal: 0 }),
    ],
    [
      'overflow cycle',
      Object.freeze({
        token: Object.freeze({}),
        traceToken: 'gt1_1',
        cycleOrdinal: 4_294_967_296,
      }),
    ],
    [
      'noncanonical token',
      Object.freeze({ token: Object.freeze({}), traceToken: 'gt1_01', cycleOrdinal: 1 }),
    ],
    [
      'overflow token',
      Object.freeze({ token: Object.freeze({}), traceToken: 'gt1_10000000', cycleOrdinal: 1 }),
    ],
  ])('omits %s trace projections without changing the raw fact', (_label, slot) => {
    const raw = Object.freeze({ kind: 'slotRequested', observedAtMs: 1, slot });

    expect(projectGptTraceFact(raw as Readonly<GoogletagDiagnosticsFact>)).toBeUndefined();
    expect(raw.slot).toBe(slot);
  });

  it('requires diagnostics observation on every GPT adapter', () => {
    expectTypeOf<GoogletagAdapter>().toMatchTypeOf<{
      observeDiagnostics(observer: GoogletagDiagnosticsObserver): (() => void) | undefined;
    }>();
  });

  it('buffers 512 facts, evicts the oldest, replays in order, then releases the buffer', () => {
    const buffer = createGptDiagnosticsFactBuffer();
    for (let index = 0; index < 513; index += 1) expect(buffer.publish(fact(index))).toBe(true);
    const received: number[] = [];

    const release = buffer.activate((item) => {
      received.push(Number(item.slot.elementId?.slice('slot-'.length)));
    });

    expect(received).toHaveLength(512);
    expect(received[0]).toBe(1);
    expect(received[511]).toBe(512);
    expect(buffer.publish(fact(513))).toBe(true);
    expect(received[512]).toBe(513);
    release?.();
    expect(buffer.publish(fact(514))).toBe(true);
    expect(received).toHaveLength(513);
    const replacement = vi.fn();
    expect(buffer.activate(replacement)).toEqual(expect.any(Function));
    expect(replacement).toHaveBeenCalledWith(fact(514));
    buffer.dispose();
    expect(buffer.publish(fact(515))).toBe(false);
  });

  it('validates and rehydrates the bounded first-display fact buffer once', () => {
    const overflows = vi.fn();
    const buffer = createGptDiagnosticsFactBuffer({ onOverflow: overflows });
    const transferredToken = Object.freeze(Object.create(null) as object);
    const base = {
      version: 1 as const,
      token: 'gt1_5',
      runtimeSlotNumber: 5,
      cycleOrdinal: 1,
      disposition: 'matched' as const,
      issueReason: null,
      capturedAtMs: 1,
      elementId: 'slot-1',
      adUnitPath: '/example/slot-1',
      isEmpty: null,
      renderedSize: null,
      isBackfill: null,
      slotContentChanged: null,
      visibilityPercent: null,
    };
    const adopted = Object.freeze({
      facts: Object.freeze([
        Object.freeze({ ...base, event: 'slotRequested' as const }),
        Object.freeze({
          ...base,
          event: 'slotRenderEnded' as const,
          capturedAtMs: 2,
          isEmpty: false,
          renderedSize: Object.freeze([300, 250] as const),
          isBackfill: false,
          slotContentChanged: true,
        }),
      ]),
      overflowCount: 7,
      dropCount: 3,
    });
    const received: Readonly<GptDiagnosticsFact>[] = [];

    expect(
      buffer.adoptFirstDisplay(
        adopted,
        (traceToken) =>
          traceToken === 'gt1_5'
            ? Object.freeze({
                token: transferredToken,
                traceToken: 'gt1_5' as never,
                runtimeSlotNumber: 5,
                cycleOrdinal: 1 as never,
                elementId: 'slot-1',
                adUnitPath: '/example/slot-1',
              })
            : undefined,
        (traceToken, slot) =>
          traceToken === 'gt1_5'
            ? createTrustedServerOpportunityFact({
                auctionSlotId: 'slot-1',
                opportunity: 'renderable_candidate',
                requestedSlotSizes: [[300, 250]],
                slot,
              })
            : undefined
      )
    ).toBe(true);
    const release = buffer.activate((value) => received.push(value));
    expect(received).toHaveLength(3);
    expect(received[0]).toMatchObject({
      kind: 'trustedServerOpportunity',
      auctionSlotId: 'slot-1',
      requestedSlotSizes: [[300, 250]],
    });
    expect(received.slice(1).map((value) => projectGptTraceFact(value as never))).toEqual([
      {
        kind: 'slotRequested',
        observedAtMs: 1,
        slot: { token: 'gt1_5', cycleOrdinal: 1, elementId: 'slot-1' },
      },
      {
        kind: 'slotRenderEnded',
        observedAtMs: 2,
        slot: { token: 'gt1_5', cycleOrdinal: 1, elementId: 'slot-1' },
        isEmpty: false,
      },
    ]);
    expect(typeof received[0]?.slot.token).toBe('object');
    expect(received[0]?.slot.token).toBe(received[1]?.slot.token);
    expect(received[0]?.slot.token).toBe(received[2]?.slot.token);
    expect(received[0]?.slot.token).toBe(transferredToken);
    expect(received[0]?.slot.runtimeSlotNumber).toBe(5);
    expect(Object.isFrozen(received[0])).toBe(true);
    expect(Object.isFrozen(received[0]?.slot)).toBe(true);
    release?.();
    for (let index = 0; index < 513; index += 1) buffer.publish(fact(index + 3));
    expect(overflows).toHaveBeenLastCalledWith(8);
    expect(
      buffer.adoptFirstDisplay(
        Object.freeze({ facts: Object.freeze([]), overflowCount: 0, dropCount: 0 })
      )
    ).toBe(false);
  });

  it('rejects malformed first-display facts without consuming adoption', () => {
    const buffer = createGptDiagnosticsFactBuffer();
    const malformed = Object.freeze({
      facts: Object.freeze([
        Object.freeze({
          version: 1,
          event: 'slotRequested',
          token: 'gt1_01',
          runtimeSlotNumber: 1,
          cycleOrdinal: 1,
          disposition: 'matched',
          issueReason: null,
          capturedAtMs: 1,
          elementId: null,
          adUnitPath: null,
          isEmpty: null,
          renderedSize: null,
          isBackfill: null,
          slotContentChanged: null,
          visibilityPercent: null,
        }),
      ]),
      overflowCount: 0,
      dropCount: 0,
    });

    expect(buffer.adoptFirstDisplay(malformed)).toBe(false);
    expect(
      buffer.adoptFirstDisplay(
        Object.freeze({ facts: Object.freeze([]), overflowCount: 0, dropCount: 0 })
      )
    ).toBe(true);
  });

  it('isolates consumer throws and admits only one live module consumer', () => {
    const errors: unknown[] = [];
    const buffer = createGptDiagnosticsFactBuffer({
      onConsumerError: (error) => errors.push(error),
    });
    buffer.publish(fact(1));
    const release = buffer.activate(() => {
      throw new Error('fictional consumer failure');
    });

    expect(errors).toHaveLength(1);
    expect(buffer.activate(vi.fn())).toBeUndefined();
    expect(buffer.publish(fact(2))).toBe(true);
    expect(errors).toHaveLength(2);
    release?.();
    expect(buffer.activate(vi.fn())).toEqual(expect.any(Function));
    buffer.dispose();
  });

  it('adds four diagnostics-only listeners while active and disposes all ownership', async () => {
    const subscriptions: Array<readonly [string, boolean | undefined]> = [];
    const releases: Array<ReturnType<typeof vi.fn>> = [];
    let observer: GoogletagDiagnosticsObserver | undefined;
    const operationDispose = vi.fn();
    const facade = Object.freeze({
      subscribe: (
        eventType: string,
        _listener: (event: unknown) => void,
        diagnosticsOwner?: boolean
      ) => {
        subscriptions.push([eventType, diagnosticsOwner]);
        const release = vi.fn();
        releases.push(release);
        return release;
      },
    }) as unknown as Readonly<GoogletagFacade>;
    const adapter = Object.freeze({
      observeDiagnostics: (candidate: GoogletagDiagnosticsObserver) => {
        observer = candidate;
        return () => {
          observer = undefined;
        };
      },
      run: <Value>(command: (gpt: Readonly<GoogletagFacade>) => Value) =>
        Object.freeze({
          status: 'present' as const,
          result: Promise.resolve(command(facade)),
          dispose: operationDispose,
        }),
    }) as unknown as GoogletagAdapter;
    const buffer = createGptDiagnosticsFactBuffer();

    const dispose = activateGptDiagnosticsFactCapture(adapter, buffer);
    await Promise.resolve();

    expect(observer).toEqual(expect.any(Function));
    expect(subscriptions).toEqual([
      ['slotResponseReceived', true],
      ['slotOnload', true],
      ['impressionViewable', true],
      ['slotVisibilityChanged', true],
    ]);
    dispose?.();
    dispose?.();
    expect(operationDispose).toHaveBeenCalledOnce();
    expect(releases.every((release) => release.mock.calls.length === 1)).toBe(true);
    expect(observer).toBeUndefined();
  });

  it('rejects capture when another diagnostics observer owns the adapter', () => {
    const run = vi.fn();
    const adapter = Object.freeze({
      observeDiagnostics: () => undefined,
      run,
    }) as unknown as Pick<GoogletagAdapter, 'observeDiagnostics' | 'run'>;

    expect(
      activateGptDiagnosticsFactCapture(adapter, createGptDiagnosticsFactBuffer())
    ).toBeUndefined();
    expect(run).not.toHaveBeenCalled();
  });

  it('lets the GPT owner install four diagnostics-only publishers without claiming observation', async () => {
    const subscriptions: Array<readonly [string, boolean | undefined]> = [];
    const releases: Array<ReturnType<typeof vi.fn>> = [];
    const operationDispose = vi.fn();
    const facade = Object.freeze({
      subscribe: (
        eventType: string,
        _listener: (event: unknown) => void,
        diagnosticsOwner?: boolean
      ) => {
        subscriptions.push([eventType, diagnosticsOwner]);
        const release = vi.fn();
        releases.push(release);
        return release;
      },
    }) as unknown as Readonly<GoogletagFacade>;
    const observeDiagnostics = vi.fn();
    const adapter = Object.freeze({
      observeDiagnostics,
      run: <Value>(command: (gpt: Readonly<GoogletagFacade>) => Value) =>
        Object.freeze({
          status: 'present' as const,
          result: Promise.resolve(command(facade)),
          dispose: operationDispose,
        }),
    }) as unknown as GoogletagAdapter;

    const dispose = activateGptDiagnosticsEventListeners(adapter);
    await Promise.resolve();

    expect(observeDiagnostics).not.toHaveBeenCalled();
    expect(subscriptions).toEqual([
      ['slotResponseReceived', true],
      ['slotOnload', true],
      ['impressionViewable', true],
      ['slotVisibilityChanged', true],
    ]);
    dispose?.();
    dispose?.();
    expect(operationDispose).toHaveBeenCalledOnce();
    expect(releases.every((release) => release.mock.calls.length === 1)).toBe(true);
  });
});
