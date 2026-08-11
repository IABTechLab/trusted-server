import { describe, expect, it, vi } from 'vitest';

import {
  createRenderTraceStore,
  DiagnosticsSubscriberLimitError,
  type RenderTraceGptFactV1,
  type RenderTracePresentationSource,
  type RenderTraceRuntimeOptions,
} from '../../src/core/trace';
import {
  createRenderTracePresentation,
  TRACE_BADGE_CLASS,
  TRACE_PANEL_ID,
} from '../../src/integrations/gpt_diagnostics/presentation';

function createPresentedRenderTrace(
  options: RenderTraceRuntimeOptions & {
    readonly document: Document;
    readonly exportRecord?: (record: Readonly<Record<string, unknown>>) => void;
    readonly overlayEnabled: boolean;
  }
) {
  const owner = createRenderTraceStore({
    ...options,
    schedule:
      options.schedule ??
      ((callback) => {
        callback();
        return () => undefined;
      }),
  });
  if (options.overlayEnabled) {
    owner.attachPresentation((source) =>
      createRenderTracePresentation(source, {
        document: options.document,
        ...(options.exportRecord === undefined ? {} : { exportRecord: options.exportRecord }),
        ...(options.onPresentationError === undefined
          ? {}
          : { onError: options.onPresentationError }),
      })
    );
  }
  return owner;
}

function harness() {
  const tasks: Array<() => void> = [];
  const owner = createRenderTraceStore({
    scheduler: {
      set: (callback) => {
        tasks.push(callback);
        return callback;
      },
      clear: (handle) => {
        const index = tasks.indexOf(handle as () => void);
        if (index >= 0) tasks.splice(index, 1);
      },
    },
  });
  return {
    owner,
    tasks,
    drain: (): void => {
      while (tasks.length > 0) tasks.shift()?.();
    },
  };
}

describe('render trace diagnostics runtime', () => {
  it('exposes one exact frozen read-only public surface with copied snapshots', () => {
    const { owner } = harness();
    const target = window as unknown as { tsjs?: Record<string, unknown> };
    const existingApi = (target.tsjs = {});
    const event = vi.fn();
    window.addEventListener('tsjs:adRendered', event);
    const record = owner.record({ slotId: 'slot-a', path: 'auction', rendered: true });

    expect(Reflect.ownKeys(owner.diagnostics).sort()).toEqual(['current', 'history', 'subscribe']);
    expect(Object.isFrozen(owner.diagnostics)).toBe(true);
    const current = owner.diagnostics.current();
    const history = owner.diagnostics.history();
    expect(Object.isFrozen(current)).toBe(true);
    expect(Object.isFrozen(history)).toBe(true);
    expect(Object.isFrozen(current['slot-a'])).toBe(true);
    expect(current['slot-a']).toEqual(record);
    expect(current['slot-a']).not.toBe(record);
    expect(history[0]).toEqual(record);
    expect(history[0]).not.toBe(record);
    expect(target.tsjs).toBe(existingApi);
    expect(target.tsjs).toEqual({});
    expect(event).not.toHaveBeenCalled();
    window.removeEventListener('tsjs:adRendered', event);
    delete target.tsjs;
  });

  it('commits before one asynchronous frozen public delivery', () => {
    const { owner, tasks, drain } = harness();
    const listener = vi.fn();
    owner.diagnostics.subscribe(listener);

    const record = owner.record({ slotId: 'slot-a', path: 'ssat', rendered: true });

    expect(owner.diagnostics.current()['slot-a']).toEqual(record);
    expect(listener).not.toHaveBeenCalled();
    expect(tasks).toHaveLength(1);
    drain();
    expect(listener).toHaveBeenCalledTimes(1);
    const delivered = listener.mock.calls[0]?.[0];
    expect(delivered).toEqual(record);
    expect(delivered).not.toBe(record);
    expect(Object.isFrozen(delivered)).toBe(true);
  });

  it('enforces the 32-subscriber cap after callable validation and reuses capacity', () => {
    const { owner } = harness();
    const releases = Array.from({ length: 32 }, () => owner.diagnostics.subscribe(() => undefined));

    expect(() => owner.diagnostics.subscribe(null as never)).toThrow(TypeError);
    expect(() => owner.diagnostics.subscribe(() => undefined)).toThrowError(
      expect.objectContaining({ code: 'subscriber_capacity', surface: 'renderTrace' })
    );
    expect(() => owner.diagnostics.subscribe(() => undefined)).toThrow(
      DiagnosticsSubscriberLimitError
    );
    releases[0]?.();
    releases[0]?.();
    expect(owner.diagnostics.subscribe(() => undefined)).toBeTypeOf('function');
  });

  it('reserves an independent internal presentation subscription outside public capacity', () => {
    const { owner, drain } = harness();
    const publicListeners = Array.from({ length: 32 }, () => vi.fn());
    const publicReleases = publicListeners.map((listener) => owner.diagnostics.subscribe(listener));
    const presentationListener = vi.fn();
    const disposePresentation = vi.fn();
    const attachPresentation = (
      owner as unknown as {
        attachPresentation: (
          factory: (source: {
            current: typeof owner.diagnostics.current;
            history: typeof owner.diagnostics.history;
            subscribe: (listener: () => void) => () => void;
          }) => Readonly<{ dispose: () => void }>
        ) => () => void;
      }
    ).attachPresentation;
    const detach = attachPresentation((source) => {
      source.subscribe(presentationListener);
      return Object.freeze({ dispose: disposePresentation });
    });

    expect(() => owner.diagnostics.subscribe(vi.fn())).toThrow(
      expect.objectContaining({ code: 'subscriber_capacity', surface: 'renderTrace' })
    );
    owner.record({ slotId: 'slot-a', path: 'auction', rendered: true });
    drain();
    expect(publicListeners.every((listener) => listener.mock.calls.length === 1)).toBe(true);
    expect(presentationListener).toHaveBeenCalledOnce();

    detach();
    detach();
    expect(disposePresentation).toHaveBeenCalledOnce();
    publicReleases.forEach((release) => release());
  });

  it('makes retained presentation sources empty and inert immediately after detach', () => {
    const { owner } = harness();
    let retainedSource:
      | Readonly<{
          current: typeof owner.diagnostics.current;
          history: typeof owner.diagnostics.history;
          subscribe: (listener: () => void) => () => void;
        }>
      | undefined;
    owner.record({ slotId: 'slot-a', path: 'auction', rendered: true });
    const detach = owner.attachPresentation((source) => {
      retainedSource = source;
      source.subscribe(() => undefined);
      return Object.freeze({ dispose: vi.fn() });
    });

    expect(retainedSource?.current()).toHaveProperty('slot-a');
    detach();
    const current = retainedSource?.current();
    const history = retainedSource?.history();
    expect(current).toEqual({});
    expect(Object.getPrototypeOf(current)).toBeNull();
    expect(Object.isFrozen(current)).toBe(true);
    expect(history).toEqual([]);
    expect(Object.isFrozen(history)).toBe(true);
    expect(() => retainedSource?.subscribe(() => undefined)).toThrow(TypeError);
  });

  it('preserves the first private presentation listener when a duplicate subscribe fails', () => {
    const { owner, drain } = harness();
    const first = vi.fn();
    const second = vi.fn();
    const detach = owner.attachPresentation((source) => {
      source.subscribe(first);
      expect(() => source.subscribe(second)).toThrow(
        'render trace presentation subscription is unavailable'
      );
      return Object.freeze({ dispose: vi.fn() });
    });

    owner.record({ slotId: 'first-listener-slot', path: 'auction', rendered: true });
    drain();
    expect(first).toHaveBeenCalledOnce();
    expect(second).not.toHaveBeenCalled();
    detach();
  });

  it('allows private presentation unsubscribe and resubscribe without reviving the first listener', () => {
    const { owner, drain } = harness();
    const first = vi.fn();
    const second = vi.fn();
    const detach = owner.attachPresentation((source) => {
      const releaseFirst = source.subscribe(first);
      releaseFirst();
      releaseFirst();
      source.subscribe(second);
      return Object.freeze({ dispose: vi.fn() });
    });

    owner.record({ slotId: 'resubscribed-slot', path: 'auction', rendered: true });
    drain();
    expect(first).not.toHaveBeenCalled();
    expect(second).toHaveBeenCalledOnce();
    detach();
  });

  it('makes retained private presentation references inert after trace-owner disposal', () => {
    const { owner } = harness();
    const disposeControls = vi.fn();
    let retainedSource: RenderTracePresentationSource | undefined;
    let retainedUnsubscribe: (() => void) | undefined;
    const detach = owner.attachPresentation((source) => {
      retainedSource = source;
      retainedUnsubscribe = source.subscribe(() => undefined);
      return Object.freeze({ dispose: disposeControls });
    });
    owner.record({ slotId: 'owner-disposed-slot', path: 'auction', rendered: true });

    owner.dispose();
    expect(retainedSource?.current()).toEqual({});
    expect(Object.getPrototypeOf(retainedSource?.current())).toBeNull();
    expect(retainedSource?.history()).toEqual([]);
    expect(() => retainedSource?.subscribe(() => undefined)).toThrow(TypeError);
    expect(() => retainedUnsubscribe?.()).not.toThrow();
    expect(() => detach()).not.toThrow();
    expect(disposeControls).toHaveBeenCalledOnce();
  });

  it('coalesces zero, one, and two presentation updates without letting a hostile late task steal resubscribed work', () => {
    const tasks: Array<() => void> = [];
    const owner = createRenderTraceStore({
      schedule: (callback) => {
        tasks.push(callback);
        return () => undefined;
      },
    });
    const firstSnapshots: number[][] = [];
    const secondSnapshots: number[][] = [];
    let source: RenderTracePresentationSource | undefined;
    let releaseFirst: (() => void) | undefined;
    const detach = owner.attachPresentation((candidate) => {
      source = candidate;
      releaseFirst = candidate.subscribe(() =>
        firstSnapshots.push(candidate.history().map(({ seq }) => seq))
      );
      return Object.freeze({ dispose: vi.fn() });
    });

    expect(tasks).toEqual([]);
    owner.record({ slotId: 'one-update', path: 'auction', rendered: true });
    expect(tasks).toHaveLength(1);
    tasks.shift()?.();
    expect(firstSnapshots).toEqual([[1]]);

    owner.record({ slotId: 'two-updates-a', path: 'auction', rendered: true });
    owner.record({ slotId: 'two-updates-b', path: 'auction', rendered: true });
    expect(tasks).toHaveLength(1);
    tasks.shift()?.();
    expect(firstSnapshots).toEqual([[1], [1, 2, 3]]);

    owner.record({ slotId: 'cancelled-update', path: 'auction', rendered: true });
    const hostileLateTask = tasks.shift()!;
    releaseFirst?.();
    source?.subscribe(() => secondSnapshots.push(source!.history().map(({ seq }) => seq)));
    owner.record({ slotId: 'resubscribed-update', path: 'auction', rendered: true });
    expect(tasks).toHaveLength(1);
    const currentTask = tasks.shift()!;
    hostileLateTask();
    expect(secondSnapshots).toEqual([]);
    currentTask();
    expect(secondSnapshots).toEqual([[1, 2, 3, 4, 5]]);
    detach();
  });

  it.each(['factory throw', 'malformed controls', 'missing listener'] as const)(
    'rolls back %s and permits a later presentation retry',
    (failure) => {
      const { owner } = harness();
      const disposeCandidate = vi.fn();

      expect(() =>
        owner.attachPresentation((source) => {
          if (failure === 'factory throw') throw new Error('fictional presentation failure');
          if (failure !== 'missing listener') source.subscribe(() => undefined);
          return Object.freeze(
            failure === 'malformed controls'
              ? { dispose: disposeCandidate, extra: true }
              : { dispose: disposeCandidate }
          ) as never;
        })
      ).toThrow();
      expect(disposeCandidate).toHaveBeenCalledTimes(failure === 'factory throw' ? 0 : 1);

      const retryDispose = vi.fn();
      const detach = owner.attachPresentation((source) => {
        source.subscribe(() => undefined);
        return Object.freeze({ dispose: retryDispose });
      });
      detach();
      expect(retryDispose).toHaveBeenCalledOnce();
    }
  );

  it('validates callability before duplicate state and rejects reentrant attachment', () => {
    const { owner } = harness();
    const nestedFactory = vi.fn();
    const detach = owner.attachPresentation((source) => {
      expect(() => owner.attachPresentation(nestedFactory)).toThrow(
        'render trace presentation is unavailable'
      );
      source.subscribe(() => undefined);
      return Object.freeze({ dispose: vi.fn() });
    });

    expect(nestedFactory).not.toHaveBeenCalled();
    expect(() => owner.attachPresentation(null as never)).toThrow(
      'render trace presentation factory must be callable'
    );
    expect(() => owner.attachPresentation(() => Object.freeze({ dispose: vi.fn() }))).toThrow(
      'render trace presentation is unavailable'
    );
    detach();
  });

  it('creates no DOM stamps, UI, or scheduled work before deferred presentation attaches', () => {
    const slot = document.createElement('div');
    slot.id = 'critical-only-slot';
    document.body.append(slot);
    const { owner, tasks } = harness();

    owner.record({
      slotId: 'critical-only-slot',
      elementId: 'critical-only-slot',
      path: 'ssat',
      rendered: true,
      injected: true,
      visible: true,
    });

    expect(tasks).toEqual([]);
    expect(slot.getAttributeNames().filter((name) => name.startsWith('data-ts-'))).toEqual([]);
    expect(slot.querySelector('.ts-render-badge')).toBeNull();
    expect(document.getElementById('ts-render-trace-panel')).toBeNull();
    owner.dispose();
    slot.remove();
  });

  it('captures membership per commit and suppresses unsubscribe before delivery', () => {
    const { owner, drain } = harness();
    const first = vi.fn();
    const second = vi.fn();
    const releaseFirst = owner.diagnostics.subscribe(first);
    owner.record({ slotId: 'slot-a', path: 'auction', rendered: true });
    releaseFirst();
    owner.diagnostics.subscribe(second);
    drain();
    expect(first).not.toHaveBeenCalled();
    expect(second).not.toHaveBeenCalled();

    owner.record({ slotId: 'slot-b', path: 'auction', rendered: true });
    drain();
    expect(second).toHaveBeenCalledTimes(1);
  });

  it('coalesces pending same-impression enrichment without changing FIFO order', () => {
    const { owner, drain, tasks } = harness();
    const received: Array<{ seq: number; injected?: boolean }> = [];
    owner.diagnostics.subscribe((record) => received.push(record));
    const first = owner.record({
      slotId: 'slot-a',
      path: 'ssat',
      rendered: true,
      injected: false,
    });
    const second = owner.record({ slotId: 'slot-b', path: 'auction', rendered: false });
    owner.enrich(first!, { injected: true, servedFrom: 'pbs-cache' });

    expect(tasks).toHaveLength(1);
    drain();
    expect(received.map(({ seq }) => seq)).toEqual([first!.seq, second!.seq]);
    expect(received[0]).toEqual(expect.objectContaining({ injected: true }));
  });

  it('bounds current state and history and prunes navigation-owned slots', () => {
    const { owner } = harness();
    for (let index = 0; index < 256; index += 1) {
      expect(
        owner.record({ slotId: `slot-${index}`, path: 'auction', rendered: true })
      ).toBeDefined();
    }
    owner.record({ slotId: 'slot-over-capacity', path: 'auction', rendered: true });
    expect(Object.keys(owner.diagnostics.current())).toHaveLength(256);
    owner.prune('slot-0');
    expect(owner.diagnostics.current()).not.toHaveProperty('slot-0');
    owner.record({ slotId: 'slot-after-prune', path: 'auction', rendered: true });

    for (let index = 0; index < 10; index += 1) {
      owner.record({ slotId: 'slot-1', path: 'gam-refresh', rendered: index % 2 === 0 });
    }
    const history = owner.diagnostics.history();
    expect(history).toHaveLength(200);
    expect(history[0]?.seq).toBeGreaterThan(1);
  });

  it('retains a bounded document-lifetime slot count after current-state pruning', () => {
    const { owner } = harness();
    const first = owner.record({ slotId: 'reused-slot', path: 'auction', rendered: true })!;

    expect(owner.prune('reused-slot', first.seq)).toBe(true);
    const second = owner.record({ slotId: 'reused-slot', path: 'gam-refresh', rendered: false })!;

    expect(second.count).toBe(2);
  });

  it('evicts the counter paired with current-state rollover without resetting retained slots', () => {
    const { owner } = harness();
    for (let index = 0; index < 256; index += 1) {
      owner.record({ slotId: `slot-${index}`, path: 'auction', rendered: true });
    }
    const refreshedA = owner.record({ slotId: 'slot-0', path: 'ssat', rendered: true })!;
    expect(refreshedA.count).toBe(2);

    owner.record({ slotId: 'slot-256', path: 'auction', rendered: true });
    expect(owner.diagnostics.current()).not.toHaveProperty('slot-0');
    const refreshedB = owner.record({ slotId: 'slot-1', path: 'ssat', rendered: true })!;

    expect(refreshedB.count).toBe(2);
    expect(owner.diagnostics.current()['slot-1']?.count).toBe(2);
    expect(Object.values(owner.diagnostics.current()).every(({ count }) => count >= 1)).toBe(true);
  });

  it('keeps a slot count monotonic across current-state and counter-capacity churn', () => {
    const { owner } = harness();
    const first = owner.record({ slotId: 'reused-slot', path: 'auction', rendered: true })!;
    expect(owner.prune('reused-slot', first.seq)).toBe(true);
    const second = owner.record({ slotId: 'reused-slot', path: 'ssat', rendered: true })!;
    expect(second.count).toBe(2);
    expect(owner.prune('reused-slot', second.seq)).toBe(true);

    for (let index = 0; index < 255; index += 1) {
      owner.record({ slotId: `capacity-${index}`, path: 'auction', rendered: true });
    }
    owner.record({ slotId: 'capacity-255', path: 'auction', rendered: true });
    const afterBoundedEviction = owner.record({
      slotId: 'reused-slot',
      path: 'auction',
      rendered: true,
    })!;

    expect(afterBoundedEviction.count).toBe(3);
  });

  it('bounds SPA slot counters at exactly 768 with protected access-order eviction', () => {
    const { owner } = harness();
    for (let index = 0; index < 768; index += 1) {
      owner.record({ slotId: `spa-slot-${index}`, path: 'auction', rendered: true });
    }

    const currentProtected = owner.record({
      slotId: 'spa-slot-0',
      path: 'ssat',
      rendered: true,
    });
    expect(currentProtected?.count).toBe(2);
    owner.record({ slotId: 'spa-slot-overflow-1', path: 'auction', rendered: true });
    expect(owner.record({ slotId: 'spa-slot-0', path: 'ssat', rendered: true })?.count).toBe(3);
    expect(owner.prune('spa-slot-0')).toBe(true);
    owner.record({ slotId: 'spa-slot-overflow-2', path: 'auction', rendered: true });
    expect(owner.record({ slotId: 'spa-slot-0', path: 'ssat', rendered: true })?.count).toBe(4);

    expect(owner.record({ slotId: 'spa-slot-1', path: 'auction', rendered: true })?.count).toBe(1);
  });

  it('protects a dormant counter referenced by an exact GPT binding', () => {
    const { owner } = harness();
    const navigationGeneration = Object.freeze({});
    owner.record({ slotId: 'bound-counter-slot', path: 'auction', rendered: true });
    expect(owner.prune('bound-counter-slot')).toBe(true);
    for (let index = 0; index < 200; index += 1) {
      owner.record({ slotId: `binding-spa-slot-${index}`, path: 'auction', rendered: true });
    }
    owner.observeGptFact(
      Object.freeze({
        kind: 'slotRequested',
        slot: Object.freeze({ token: 'gt1_1', cycleOrdinal: 1 }),
      }),
      () =>
        Object.freeze({
          slotId: 'bound-counter-slot',
          navigationGeneration,
          traceToken: 'gt1_1',
        })
    );
    for (let index = 200; index < 767; index += 1) {
      owner.record({ slotId: `binding-spa-slot-${index}`, path: 'auction', rendered: true });
    }

    owner.record({ slotId: 'binding-spa-overflow', path: 'auction', rendered: true });

    expect(
      owner.record({ slotId: 'bound-counter-slot', path: 'auction', rendered: true })?.count
    ).toBe(2);
    expect(
      owner.record({ slotId: 'binding-spa-slot-0', path: 'auction', rendered: true })?.count
    ).toBe(1);
  });

  it('retains impression bookkeeping and refuses truth-weakening enrichment', () => {
    const { owner } = harness();
    const record = owner.record({
      slotId: 'slot-a',
      path: 'ssat',
      rendered: true,
      injected: true,
    })!;

    const enriched = owner.enrich(record, {
      rendered: false,
      injected: false,
      visible: true,
      servedFrom: 'pbs-cache',
    })!;

    expect(enriched).toEqual(
      expect.objectContaining({
        at: record.at,
        count: record.count,
        seq: record.seq,
        rendered: true,
        injected: true,
        visible: true,
        servedFrom: 'pbs-cache',
      })
    );
    expect(owner.diagnostics.history()).toHaveLength(1);
  });

  it('records an unattributed GPT request as one GAM-refresh impression', () => {
    const { owner } = harness();
    const navigationGeneration = Object.freeze({});
    const resolve = () =>
      Object.freeze({
        slotId: 'publisher-slot',
        elementId: 'publisher-slot',
        navigationGeneration,
        traceToken: 'gt1_1',
        visible: true,
      });

    owner.observeGptFact(
      Object.freeze({
        kind: 'slotRequested',
        observedAtMs: 1,
        slot: Object.freeze({ token: 'gt1_1', cycleOrdinal: 1, elementId: 'publisher-slot' }),
      }),
      resolve
    );
    owner.observeGptFact(
      Object.freeze({
        kind: 'slotRenderEnded',
        observedAtMs: 2,
        slot: Object.freeze({ token: 'gt1_1', cycleOrdinal: 1, elementId: 'publisher-slot' }),
        isEmpty: false,
      }),
      resolve
    );

    expect(owner.diagnostics.current()['publisher-slot']).toEqual(
      expect.objectContaining({
        path: 'gam-refresh',
        rendered: true,
        gamEmpty: false,
        injected: false,
        visible: true,
        servedFrom: 'gam',
      })
    );
    expect(owner.diagnostics.history()).toHaveLength(1);
  });

  it('reconciles a later trusted terminal into the GPT-first impression', () => {
    const { owner, tasks, drain } = harness();
    const listener = vi.fn();
    const navigationGeneration = Object.freeze({});
    const slot = Object.freeze({ token: 'gt1_1', cycleOrdinal: 1, elementId: 'reverse-slot' });
    const resolve = () =>
      Object.freeze({
        slotId: 'reverse-slot',
        elementId: 'reverse-slot',
        navigationGeneration,
        traceToken: 'gt1_1',
        visible: true,
      });
    owner.diagnostics.subscribe(listener);

    owner.observeGptFact(Object.freeze({ kind: 'slotRequested', observedAtMs: 1, slot }), resolve);
    owner.observeGptFact(
      Object.freeze({ kind: 'slotRenderEnded', observedAtMs: 2, slot, isEmpty: false }),
      resolve
    );
    const provisional = owner.diagnostics.current()['reverse-slot'];
    expect(provisional).toEqual(
      expect.objectContaining({ path: 'gam-refresh', rendered: true, injected: false })
    );

    const terminal = owner.record({
      slotId: 'reverse-slot',
      path: 'ssat',
      rendered: true,
      injected: true,
      bidder: 'trusted-bidder',
      bidId: 'trusted-bid',
      creativeId: 'trusted-creative',
      servedFrom: 'pbs-cache',
    })!;

    expect(terminal).toEqual(
      expect.objectContaining({
        seq: provisional?.seq,
        count: provisional?.count,
        at: provisional?.at,
        path: 'ssat',
        bidder: 'trusted-bidder',
        bidId: 'trusted-bid',
        creativeId: 'trusted-creative',
        servedFrom: 'pbs-cache',
        rendered: true,
        injected: true,
        gamEmpty: false,
      })
    );
    expect(owner.diagnostics.history()).toEqual([terminal]);
    expect(tasks).toHaveLength(1);
    drain();
    expect(listener).toHaveBeenCalledOnce();
    expect(listener).toHaveBeenCalledWith(
      expect.objectContaining({ seq: terminal.seq, path: 'ssat' })
    );

    owner.observeGptFact(
      Object.freeze({
        kind: 'slotVisibilityChanged',
        observedAtMs: 3,
        slot,
        inViewPercentage: 0,
      }),
      resolve
    );
    expect(owner.diagnostics.current()['reverse-slot']).toEqual(
      expect.objectContaining({ seq: terminal.seq, path: 'ssat', visible: false })
    );
    expect(owner.diagnostics.history()).toHaveLength(1);
  });

  it('enriches only the same GPT impression without weakening TS placement truth', () => {
    const { owner } = harness();
    const navigationGeneration = Object.freeze({});
    const slot = Object.freeze({ token: 'gt1_1', cycleOrdinal: 1, elementId: 'ts-slot' });
    const resolve = () =>
      Object.freeze({
        slotId: 'ts-slot',
        elementId: 'ts-slot',
        navigationGeneration,
        traceToken: 'gt1_1',
        visible: true,
      });
    owner.record({ slotId: 'ts-slot', path: 'gam-refresh', rendered: false, injected: false });
    owner.observeGptFact(Object.freeze({ kind: 'slotRequested', observedAtMs: 1, slot }), resolve);
    const trusted = owner.record({
      slotId: 'ts-slot',
      path: 'ssat',
      rendered: true,
      injected: true,
      servedFrom: 'pbs-cache',
    })!;

    owner.observeGptFact(
      Object.freeze({ kind: 'slotRenderEnded', observedAtMs: 2, slot, isEmpty: false }),
      resolve
    );
    owner.observeGptFact(
      Object.freeze({ kind: 'slotRenderEnded', observedAtMs: 3, slot, isEmpty: true }),
      resolve
    );

    expect(owner.diagnostics.history()).toHaveLength(2);
    expect(owner.diagnostics.current()['ts-slot']).toEqual(
      expect.objectContaining({
        seq: trusted.seq,
        path: 'ssat',
        rendered: true,
        injected: true,
        gamEmpty: false,
        servedFrom: 'pbs-cache',
      })
    );
  });

  it('routes all GPT lifecycle facts and scopes visibility to the active physical request', () => {
    const { owner } = harness();
    const navigationGeneration = Object.freeze({});
    let currentTraceToken = 'gt1_1';
    const resolve = () =>
      Object.freeze({
        slotId: 'visible-slot',
        navigationGeneration,
        traceToken: currentTraceToken,
        visible: false,
      });
    const fact = (
      kind: RenderTraceGptFactV1['kind'],
      token: string,
      cycleOrdinal: number,
      fields: Readonly<Pick<RenderTraceGptFactV1, 'isEmpty' | 'inViewPercentage'>> = Object.freeze(
        {}
      )
    ): Readonly<RenderTraceGptFactV1> =>
      Object.freeze({
        kind,
        observedAtMs: 1,
        slot: Object.freeze({ token, cycleOrdinal }),
        ...fields,
      });

    owner.observeGptFact(fact('slotRequested', 'gt1_1', 1), resolve);
    owner.observeGptFact(fact('slotResponseReceived', 'gt1_1', 1), resolve);
    owner.observeGptFact(fact('slotRenderEnded', 'gt1_1', 1, { isEmpty: false }), resolve);
    owner.observeGptFact(fact('slotOnload', 'gt1_1', 1), resolve);
    owner.observeGptFact(fact('impressionViewable', 'gt1_1', 1), resolve);
    expect(owner.diagnostics.current()['visible-slot']?.visible).toBe(true);
    owner.observeGptFact(
      fact('slotVisibilityChanged', 'gt1_1', 1, { inViewPercentage: 0 }),
      resolve
    );
    expect(owner.diagnostics.current()['visible-slot']?.visible).toBe(false);

    currentTraceToken = 'gt1_2';
    owner.observeGptFact(fact('slotRequested', 'gt1_2', 1), resolve);
    owner.observeGptFact(fact('slotRenderEnded', 'gt1_1', 1, { isEmpty: true }), resolve);
    owner.observeGptFact(fact('impressionViewable', 'gt1_1', 1), resolve);
    owner.observeGptFact(fact('impressionViewable', 'gt1_2', 1), resolve);
    expect(
      owner.diagnostics.current()['visible-slot']?.visible,
      'a pre-render callback for a replacement physical request must not enrich the old impression'
    ).toBe(false);
    expect(owner.diagnostics.history()).toHaveLength(1);
  });

  it('rejects late onload visibility resolved through a same-element physical replacement', () => {
    const { owner } = harness();
    const navigationGeneration = Object.freeze({});
    const firstSlot = Object.freeze({
      token: 'gt1_1',
      cycleOrdinal: 1,
      elementId: 'reused-element',
    });
    const replacementSlot = Object.freeze({
      token: 'gt1_2',
      cycleOrdinal: 1,
      elementId: 'reused-element',
    });
    const resolution = (traceToken: string, visible: boolean) =>
      Object.freeze({
        slotId: 'registered-slot',
        elementId: 'reused-element',
        navigationGeneration,
        traceToken,
        visible,
      });

    owner.observeGptFact(Object.freeze({ kind: 'slotRequested', slot: firstSlot }), () =>
      resolution('gt1_1', false)
    );
    owner.observeGptFact(
      Object.freeze({ kind: 'slotRenderEnded', slot: firstSlot, isEmpty: false }),
      () => resolution('gt1_1', false)
    );
    owner.observeGptFact(Object.freeze({ kind: 'slotRequested', slot: replacementSlot }), () =>
      resolution('gt1_2', false)
    );
    owner.observeGptFact(
      Object.freeze({ kind: 'slotRenderEnded', slot: replacementSlot, isEmpty: false }),
      () => resolution('gt1_2', false)
    );

    owner.observeGptFact(Object.freeze({ kind: 'slotOnload', slot: firstSlot }), () =>
      resolution('gt1_2', true)
    );

    expect(owner.diagnostics.history()).toEqual([
      expect.objectContaining({ seq: 1, visible: false }),
      expect.objectContaining({ seq: 2, visible: false }),
    ]);
    expect(owner.diagnostics.current()['registered-slot']).toEqual(
      expect.objectContaining({ seq: 2, visible: false })
    );
  });

  it('joins GPT enrichment by the exact token-cycle pair and enriches retired history only', () => {
    const { owner } = harness();
    const navigationGeneration = Object.freeze({});
    const resolve = () =>
      Object.freeze({
        slotId: 'refresh-slot',
        navigationGeneration,
        traceToken: 'gt1_1',
        visible: false,
      });
    const fact = (
      kind: RenderTraceGptFactV1['kind'],
      cycleOrdinal: number,
      fields: Readonly<Pick<RenderTraceGptFactV1, 'isEmpty'>> = Object.freeze({})
    ): Readonly<RenderTraceGptFactV1> =>
      Object.freeze({
        kind,
        slot: Object.freeze({ token: 'gt1_1', cycleOrdinal }),
        ...fields,
      });

    owner.observeGptFact(fact('slotRequested', 1), resolve);
    owner.observeGptFact(fact('slotRenderEnded', 1, { isEmpty: false }), resolve);
    owner.observeGptFact(fact('slotRequested', 2), resolve);
    owner.observeGptFact(fact('slotRenderEnded', 2, { isEmpty: false }), resolve);
    const beforeLate = owner.diagnostics.history();
    expect(beforeLate).toHaveLength(2);
    expect(beforeLate[0]?.visible).toBe(false);
    expect(beforeLate[1]?.visible).toBe(false);

    owner.observeGptFact(fact('impressionViewable', 1), resolve);
    owner.observeGptFact(fact('impressionViewable', 3), resolve);

    const afterLate = owner.diagnostics.history();
    expect(afterLate[0]).toEqual(expect.objectContaining({ seq: 1, visible: true }));
    expect(afterLate[1]).toEqual(expect.objectContaining({ seq: 2, visible: false }));
    expect(owner.diagnostics.current()['refresh-slot']).toEqual(
      expect.objectContaining({ seq: 2, visible: false })
    );
  });

  it.each([
    ['zero cycle', 'gt1_1', 0],
    ['fractional cycle', 'gt1_1', 1.5],
    ['overflow cycle', 'gt1_1', 4_294_967_296],
    ['noncanonical token', 'gt1_01', 1],
  ])('drops %s GPT trace identities', (_label, token, cycleOrdinal) => {
    const { owner } = harness();

    owner.observeGptFact(
      Object.freeze({
        kind: 'slotRequested',
        slot: Object.freeze({ token, cycleOrdinal }),
      }),
      () =>
        Object.freeze({
          slotId: 'invalid-slot',
          navigationGeneration: Object.freeze({}),
          traceToken: token,
        })
    );

    expect(owner.diagnostics.current()).toEqual({});
    expect(owner.diagnostics.history()).toEqual([]);
  });

  it('rejects a GPT request whose resolved physical token does not match', () => {
    const { owner } = harness();
    const navigationGeneration = Object.freeze({});
    const slot = Object.freeze({ token: 'gt1_1', cycleOrdinal: 1, elementId: 'token-slot' });
    const resolve = () =>
      Object.freeze({
        slotId: 'token-slot',
        navigationGeneration,
        traceToken: 'gt1_2',
      });

    owner.observeGptFact(Object.freeze({ kind: 'slotRequested', slot }), resolve);
    owner.observeGptFact(Object.freeze({ kind: 'slotRenderEnded', slot, isEmpty: false }), resolve);

    expect(owner.diagnostics.current()).toEqual({});
    expect(owner.diagnostics.history()).toEqual([]);
  });

  it('admits exactly 256 live GPT impression bindings and refuses the 257th', () => {
    const { owner } = harness();
    const navigationGeneration = Object.freeze({});
    const facts = Array.from({ length: 257 }, (_, index) => {
      const ordinal = index + 1;
      const token = `gt1_${ordinal.toString(36)}`;
      const elementId = `binding-slot-${ordinal}`;
      const slot = Object.freeze({ token, cycleOrdinal: 1, elementId });
      const resolve = () =>
        Object.freeze({
          slotId: elementId,
          navigationGeneration,
          traceToken: token,
        });
      return { resolve, slot };
    });

    for (const { resolve, slot } of facts) {
      owner.observeGptFact(
        Object.freeze({ kind: 'slotRequested', observedAtMs: 1, slot }),
        resolve
      );
    }
    for (let index = 0; index < 255; index += 1) {
      const fact = facts[index]!;
      owner.observeGptFact(
        Object.freeze({
          kind: 'slotRenderEnded',
          observedAtMs: 2,
          slot: fact.slot,
          isEmpty: false,
        }),
        fact.resolve
      );
    }
    expect(Object.keys(owner.diagnostics.current())).toHaveLength(255);

    const atCapacity = facts[255]!;
    owner.observeGptFact(
      Object.freeze({
        kind: 'slotRenderEnded',
        observedAtMs: 2,
        slot: atCapacity.slot,
        isEmpty: false,
      }),
      atCapacity.resolve
    );
    expect(Object.keys(owner.diagnostics.current())).toHaveLength(256);

    const overflow = facts[256]!;
    owner.observeGptFact(
      Object.freeze({
        kind: 'slotRenderEnded',
        observedAtMs: 2,
        slot: overflow.slot,
        isEmpty: false,
      }),
      overflow.resolve
    );
    expect(Object.keys(owner.diagnostics.current())).toHaveLength(256);
    expect(owner.diagnostics.current()).not.toHaveProperty('binding-slot-257');
  });

  it('retires an open GPT binding on navigation disposal before it creates a row', () => {
    const { owner } = harness();
    const navigationGeneration = Object.freeze({});
    const slot = Object.freeze({ token: 'gt1_1', cycleOrdinal: 1, elementId: 'open-slot' });
    const resolve = () =>
      Object.freeze({
        slotId: 'open-slot',
        navigationGeneration,
        traceToken: 'gt1_1',
      });

    owner.observeGptFact(Object.freeze({ kind: 'slotRequested', slot }), resolve);
    expect(owner.pruneNavigation(navigationGeneration)).toBe(1);
    owner.observeGptFact(Object.freeze({ kind: 'slotRenderEnded', slot, isEmpty: false }), resolve);

    expect(owner.diagnostics.current()).toEqual({});
    expect(owner.diagnostics.history()).toEqual([]);
  });

  it('retires an open GPT binding when its render can no longer resolve the slot', () => {
    const { owner } = harness();
    const navigationGeneration = Object.freeze({});
    const slot = Object.freeze({ token: 'gt1_1', cycleOrdinal: 1, elementId: 'stale-slot' });
    const resolution = Object.freeze({
      slotId: 'stale-slot',
      navigationGeneration,
      traceToken: 'gt1_1',
    });

    owner.observeGptFact(Object.freeze({ kind: 'slotRequested', slot }), () => resolution);
    owner.observeGptFact(
      Object.freeze({ kind: 'slotRenderEnded', slot, isEmpty: false }),
      () => undefined
    );
    owner.observeGptFact(
      Object.freeze({ kind: 'slotRenderEnded', slot, isEmpty: false }),
      () => resolution
    );

    expect(owner.diagnostics.current()).toEqual({});
    expect(owner.diagnostics.history()).toEqual([]);
  });

  it('makes repeated retained record calls wholly inert after disposal', () => {
    const now = vi.fn(() => 41);
    const schedule = vi.fn(() => vi.fn());
    const owner = createRenderTraceStore({ now, schedule });
    const listener = vi.fn();
    const resolve = vi.fn(() =>
      Object.freeze({
        slotId: 'disposed-binding-slot',
        navigationGeneration: Object.freeze({}),
        traceToken: 'gt1_1',
      })
    );
    const retainedRecord = owner.record;
    owner.diagnostics.subscribe(listener);
    owner.record({ slotId: 'before-dispose', path: 'auction', rendered: true });
    owner.observeGptFact(
      Object.freeze({
        kind: 'slotRequested',
        slot: Object.freeze({ token: 'gt1_1', cycleOrdinal: 1 }),
      }),
      resolve
    );
    owner.dispose();
    now.mockClear();
    schedule.mockClear();
    listener.mockClear();
    resolve.mockClear();
    const reads = vi.fn();
    const hostileInput = new Proxy(
      { slotId: 'after-dispose', path: 'auction' as const, rendered: true },
      {
        get: (target, property, receiver) => (
          reads(property),
          Reflect.get(target, property, receiver)
        ),
      }
    );

    expect(retainedRecord(hostileInput)).toBeUndefined();
    expect(retainedRecord(hostileInput)).toBeUndefined();
    owner.observeGptFact(
      Object.freeze({
        kind: 'slotRenderEnded',
        slot: Object.freeze({ token: 'gt1_1', cycleOrdinal: 1 }),
        isEmpty: false,
      }),
      resolve
    );

    expect(reads).not.toHaveBeenCalled();
    expect(now).not.toHaveBeenCalled();
    expect(schedule).not.toHaveBeenCalled();
    expect(listener).not.toHaveBeenCalled();
    expect(resolve).not.toHaveBeenCalled();
    expect(owner.diagnostics.current()).toEqual({});
    expect(owner.diagnostics.history()).toEqual([]);
  });

  it('drops the oldest of 201 pending records and cancels work on disposal', () => {
    const { owner, tasks, drain } = harness();
    const listener = vi.fn();
    owner.diagnostics.subscribe(listener);
    for (let index = 0; index < 201; index += 1) {
      owner.record({ slotId: 'slot-a', path: 'auction', rendered: true });
    }
    expect(tasks).toHaveLength(1);
    drain();
    expect(listener).toHaveBeenCalledTimes(200);
    expect(listener.mock.calls[0]?.[0].seq).toBe(2);

    owner.record({ slotId: 'slot-a', path: 'auction', rendered: true });
    expect(tasks).toHaveLength(1);
    owner.dispose();
    owner.dispose();
    drain();
    expect(listener).toHaveBeenCalledTimes(200);
    const late = owner.record({ slotId: 'late', path: 'auction', rendered: true });
    expect(late).toBeUndefined();
    expect(owner.diagnostics.current()).toEqual({});
    expect(owner.diagnostics.history()).toEqual([]);
    expect(owner.diagnostics.subscribe(() => undefined)).toBeTypeOf('function');
    expect(() => owner.diagnostics.subscribe(null as never)).toThrow(TypeError);
  });

  it('uses the server-resolved boot bit instead of reading the trace cookie', () => {
    document.cookie = 'ts-trace=1; Path=/';
    const disarmedSlot = document.createElement('div');
    disarmedSlot.id = 'disarmed-slot';
    document.body.append(disarmedSlot);
    const disarmed = createPresentedRenderTrace({ document, overlayEnabled: false });

    disarmed.record({
      slotId: 'disarmed-slot',
      elementId: 'disarmed-slot',
      path: 'auction',
      rendered: true,
      injected: true,
      visible: true,
    });

    expect(disarmedSlot.getAttribute('data-ts-rendered')).toBeNull();
    expect(disarmedSlot.querySelector(`.${TRACE_BADGE_CLASS}`)).toBeNull();
    expect(document.getElementById(TRACE_PANEL_ID)).toBeNull();
    disarmed.dispose();
    disarmedSlot.remove();
    document.cookie = 'ts-trace=; Max-Age=0; Path=/';

    const armedSlot = document.createElement('div');
    armedSlot.id = 'armed-slot';
    document.body.append(armedSlot);
    const armed = createPresentedRenderTrace({ document, overlayEnabled: true });
    armed.record({
      slotId: 'armed-slot',
      elementId: 'armed-slot',
      path: 'ssat',
      rendered: true,
      injected: true,
      visible: true,
    });

    const badge = armedSlot.querySelector(`.${TRACE_BADGE_CLASS}`) as HTMLElement | null;
    expect(badge).not.toBeNull();
    expect(badge?.style.pointerEvents).toBe('none');
    expect(document.getElementById(TRACE_PANEL_ID)).not.toBeNull();
    armed.dispose();
    armedSlot.remove();
  });

  it('removes stale stamps and badges on a later physical impression', () => {
    const slot = document.createElement('div');
    slot.id = 'restamped-slot';
    document.body.append(slot);
    const owner = createPresentedRenderTrace({ document, overlayEnabled: true });
    owner.record({
      slotId: 'restamped-slot',
      elementId: 'restamped-slot',
      path: 'ssat',
      rendered: true,
      injected: true,
      visible: true,
      bidder: 'first-bidder',
      admHash: 'first-hash',
    });
    expect(slot.getAttribute('data-ts-bidder')).toBe('first-bidder');
    expect(slot.querySelector(`.${TRACE_BADGE_CLASS}`)).not.toBeNull();

    owner.record({
      slotId: 'restamped-slot',
      elementId: 'restamped-slot',
      path: 'gam-refresh',
      rendered: true,
      injected: true,
      visible: false,
    });

    expect(slot.hasAttribute('data-ts-bidder')).toBe(false);
    expect(slot.hasAttribute('data-ts-adm-hash')).toBe(false);
    expect(slot.querySelector(`.${TRACE_BADGE_CLASS}`)).toBeNull();
    owner.dispose();
    expect(slot.hasAttribute('data-ts-slot-id')).toBe(false);
    slot.remove();
  });

  it('stamps iframe slots without placing UI inside the creative frame', () => {
    const iframe = document.createElement('iframe');
    iframe.id = 'iframe-slot';
    document.body.append(iframe);
    const owner = createPresentedRenderTrace({ document, overlayEnabled: true });

    owner.record({
      slotId: 'iframe-slot',
      elementId: 'iframe-slot',
      path: 'ssat',
      rendered: true,
      injected: true,
      visible: true,
    });

    expect(iframe.getAttribute('data-ts-rendered')).toBe('true');
    expect(iframe.querySelector(`.${TRACE_BADGE_CLASS}`)).toBeNull();
    owner.dispose();
    iframe.remove();
  });

  it('does not claim an ok badge before visibility is positively observed', () => {
    const slot = document.createElement('div');
    slot.id = 'unobserved-slot';
    document.body.append(slot);
    const owner = createPresentedRenderTrace({ document, overlayEnabled: true });

    owner.record({
      slotId: 'unobserved-slot',
      elementId: 'unobserved-slot',
      path: 'auction',
      rendered: true,
      injected: true,
    });

    expect(slot.querySelector(`.${TRACE_BADGE_CLASS}`)).toBeNull();
    expect(document.getElementById(TRACE_PANEL_ID)?.textContent).toContain('hidden');
    owner.dispose();
    slot.remove();
  });

  it('does not claim or remove a publisher-owned overlay id collision', () => {
    const publisherPanel = document.createElement('div');
    publisherPanel.id = TRACE_PANEL_ID;
    publisherPanel.textContent = 'publisher';
    document.body.append(publisherPanel);
    const owner = createPresentedRenderTrace({ document, overlayEnabled: true });

    owner.record({ slotId: 'slot-a', path: 'auction', rendered: true });

    expect(document.getElementById(TRACE_PANEL_ID)).toBe(publisherPanel);
    expect(publisherPanel.textContent).toBe('publisher');
    owner.dispose();
    expect(document.getElementById(TRACE_PANEL_ID)).toBe(publisherPanel);
    publisherPanel.remove();
  });

  it('keeps a bounded newest-first overlay and exports frozen row data', () => {
    const exportRecord = vi.fn();
    const tasks: Array<() => void> = [];
    const owner = createPresentedRenderTrace({
      document,
      overlayEnabled: true,
      exportRecord,
      schedule: (callback) => {
        tasks.push(callback);
        return () => {
          const index = tasks.indexOf(callback);
          if (index >= 0) tasks.splice(index, 1);
        };
      },
    });
    for (let index = 1; index <= 201; index += 1) {
      owner.record({ slotId: `slot-${index}`, path: 'auction', rendered: true });
    }
    expect(tasks).toHaveLength(1);
    tasks.shift()?.();
    expect(tasks).toEqual([]);

    const panel = document.getElementById(TRACE_PANEL_ID)!;
    const rows = [...panel.querySelectorAll<HTMLElement>('[data-ts-trace-seq]')];
    expect(rows).toHaveLength(200);
    expect(rows[0]?.dataset['tsTraceSeq']).toBe('201');
    expect(rows[rows.length - 1]?.dataset['tsTraceSeq']).toBe('2');
    rows[0]?.click();
    expect(exportRecord).toHaveBeenCalledOnce();
    expect(exportRecord).toHaveBeenCalledWith(
      expect.objectContaining({ slotId: 'slot-201', seq: 201 })
    );
    expect(Object.isFrozen(exportRecord.mock.calls[0]?.[0])).toBe(true);
    owner.dispose();
    expect(document.getElementById(TRACE_PANEL_ID)).toBeNull();
  });

  it('isolates presentation failures after committing diagnostics state', () => {
    const onPresentationError = vi.fn();
    const hostileDocument = {
      getElementById: () => {
        throw new Error('hostile document');
      },
    } as unknown as Document;
    const owner = createPresentedRenderTrace({
      document: hostileDocument,
      overlayEnabled: true,
      onPresentationError,
    });

    expect(() => owner.record({ slotId: 'slot-a', path: 'auction', rendered: true })).not.toThrow();
    expect(owner.diagnostics.current()['slot-a']).toEqual(
      expect.objectContaining({ slotId: 'slot-a', rendered: true })
    );
    expect(onPresentationError).toHaveBeenCalledOnce();
  });
});
