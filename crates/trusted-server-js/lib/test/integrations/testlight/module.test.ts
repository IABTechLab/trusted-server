import { describe, expect, it, vi } from 'vitest';

import { createTestlightRuntime } from '../../../src/integrations/testlight/module';

describe('transactional Testlight integration module', () => {
  it('bridges preexisting and later callbacks once while isolating invalid and throwing work', () => {
    const calls: string[] = [];
    const first = () => calls.push('first');
    const throwing = () => {
      calls.push('throwing');
      throw new Error('publisher callback failed');
    };
    const second = () => calls.push('second');
    const beforeCommit = () => calls.push('before-commit');
    const afterCommit = () => calls.push('after-commit');
    const original = [first, 'invalid', throwing, second];
    const target = { testlight: { publisher: true, que: original } };
    const enqueue = vi.fn((callback: () => void) => callback());
    const runtime = createTestlightRuntime({ enqueue, started: vi.fn(), target });

    const release = runtime.activate(undefined);
    target.testlight.que.push(beforeCommit);
    expect(calls).toEqual([]);

    runtime.start(undefined);
    target.testlight.que.push(afterCommit);

    expect(calls).toEqual(['first', 'throwing', 'second', 'before-commit', 'after-commit']);
    expect(enqueue).toHaveBeenCalledTimes(5);
    release();
    release();
    expect(target.testlight).toEqual({ publisher: true, que: original });
  });

  it('returns callbacks added during activation to the publisher queue on rollback', () => {
    const original = [vi.fn()];
    const later = vi.fn();
    const target = { testlight: { que: original } };
    const runtime = createTestlightRuntime({
      enqueue: vi.fn(),
      started: vi.fn(),
      target,
    });

    const release = runtime.activate(undefined);
    target.testlight.que.push(later);
    release();

    expect(target.testlight.que).toBe(original);
    expect(original).toEqual([expect.any(Function), later]);
  });

  it('does not overwrite a publisher queue replacement during disposal', () => {
    const target = { testlight: { que: [] as unknown[] } };
    const runtime = createTestlightRuntime({
      enqueue: vi.fn(),
      started: vi.fn(),
      target,
    });
    const release = runtime.activate(undefined);
    const replacement: unknown[] = [];
    target.testlight.que = replacement;

    release();

    expect(target.testlight.que).toBe(replacement);
  });

  it('preserves publisher fields added to a runtime-created global', () => {
    const target: { testlight?: { publisher?: boolean; que?: unknown[] } } = {};
    const runtime = createTestlightRuntime({
      enqueue: vi.fn(),
      started: vi.fn(),
      target,
    });
    const release = runtime.activate(undefined);
    if (!target.testlight) throw new Error('should create the Testlight global');
    target.testlight.publisher = true;

    release();

    expect(target.testlight).toEqual({ publisher: true });
  });

  it('snapshots queue data without invoking a publisher iterator', () => {
    const callback = vi.fn();
    const original = [callback];
    Object.defineProperty(original, Symbol.iterator, {
      configurable: true,
      value: () => {
        throw new Error('publisher iterator must remain inert');
      },
    });
    const target = { testlight: { que: original } };
    const enqueue = vi.fn((candidate: () => void) => candidate());
    const runtime = createTestlightRuntime({ enqueue, started: vi.fn(), target });

    const release = runtime.activate(undefined);
    expect(() => runtime.start(undefined)).not.toThrow();

    expect(callback).toHaveBeenCalledOnce();
    release();
  });
});
