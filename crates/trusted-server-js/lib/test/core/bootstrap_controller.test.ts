import { describe, expect, it, vi } from 'vitest';

import { createBootstrapController } from '../../src/core/bootstrap_controller';

describe('first-display bootstrap controller', () => {
  it('owns the bids mark, one deadline, and exact registration/action transitions', () => {
    let now = 0;
    const marks: string[] = [];
    const deadline = vi.fn();
    const timers: Array<() => void> = [];
    const controller = createBootstrapController({
      performance: { mark: (name) => marks.push(name) },
      now: () => now,
      startedAtMs: 0,
      setTimer: (callback) => {
        timers.push(callback);
        return callback;
      },
      clearTimer: (handle) => {
        const index = timers.indexOf(handle as () => void);
        if (index >= 0) timers.splice(index, 1);
      },
      onFailure: deadline,
    });

    expect(marks).toEqual(['tsjs:bids-script']);
    expect(controller.registerAgent()).toBe(true);
    now = 9_999;
    expect(controller.startAction()).toBe(true);
    controller.settle();
    expect(timers).toEqual([]);
    expect(deadline).not.toHaveBeenCalled();
  });

  it('fails at exactly 10 seconds and never admits a late or duplicate transition', () => {
    let now = 10_000;
    const failures: string[] = [];
    const controller = createBootstrapController({
      performance: { mark: () => undefined },
      now: () => now,
      startedAtMs: 0,
      setTimer: (callback) => callback,
      clearTimer: () => undefined,
      onFailure: (reason) => failures.push(reason),
    });
    expect(controller.registerAgent()).toBe(false);
    expect(controller.startAction()).toBe(false);
    expect(failures).toEqual(['bundle_partial']);
    now = 0;
    expect(controller.registerAgent()).toBe(false);
  });
});
