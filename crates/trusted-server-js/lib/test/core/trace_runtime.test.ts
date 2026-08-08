import { describe, expect, it, vi } from 'vitest';

import { createRenderTrace, DiagnosticsSubscriberLimitError } from '../../src/core/trace';

function harness() {
  const tasks: Array<() => void> = [];
  const owner = createRenderTrace({
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
    expect(Object.isFrozen(late)).toBe(true);
    expect(owner.diagnostics.current()).toEqual({});
    expect(owner.diagnostics.history()).toEqual([]);
    expect(owner.diagnostics.subscribe(() => undefined)).toBeTypeOf('function');
    expect(() => owner.diagnostics.subscribe(null as never)).toThrow(TypeError);
  });
});
