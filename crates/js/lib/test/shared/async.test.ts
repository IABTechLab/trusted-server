import { describe, it, expect, vi } from 'vitest';

import { delay, queueTask } from '../../src/shared/async';

describe('shared/async', () => {
  it('queueTask uses queueMicrotask when available', async () => {
    const microtaskSpy = vi.fn();
    const originalQueue = global.queueMicrotask;

    global.queueMicrotask = (cb: () => void) => {
      microtaskSpy();
      cb();
    };

    const callback = vi.fn();
    queueTask(callback);
    await Promise.resolve();

    expect(microtaskSpy).toHaveBeenCalled();
    expect(callback).toHaveBeenCalled();

    global.queueMicrotask = originalQueue;
  });

  it('queueTask falls back to setTimeout and delay resolves after given time', async () => {
    vi.useFakeTimers();
    const callback = vi.fn();
    queueTask(callback);
    expect(callback).not.toHaveBeenCalled();

    const resolver = vi.fn();
    const promise = delay(50).then(resolver);
    expect(callback).not.toHaveBeenCalled();
    expect(resolver).not.toHaveBeenCalled();

    vi.advanceTimersByTime(50);
    await promise;
    expect(callback).toHaveBeenCalled();
    expect(resolver).toHaveBeenCalled();

    vi.useRealTimers();
  });
});
