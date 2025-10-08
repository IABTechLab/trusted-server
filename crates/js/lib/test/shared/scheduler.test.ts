import { describe, it, expect, vi } from 'vitest';

import { createMutationScheduler } from '../../src/shared/scheduler';

describe('shared/scheduler', () => {
  it('schedules unique elements exactly once', async () => {
    const perform = vi.fn();
    const schedule = createMutationScheduler(perform);
    const el = document.createElement('div');
    schedule(el);
    schedule(el);
    schedule(el);

    await Promise.resolve();
    expect(perform).toHaveBeenCalledTimes(1);
    expect(perform).toHaveBeenCalledWith(el);
  });

  it('handles multiple elements independently', async () => {
    const perform = vi.fn();
    const schedule = createMutationScheduler(perform);
    const first = document.createElement('span');
    const second = document.createElement('span');
    schedule(first);
    schedule(second);

    await Promise.resolve();
    expect(perform).toHaveBeenCalledTimes(2);
    expect(perform).toHaveBeenCalledWith(first);
    expect(perform).toHaveBeenCalledWith(second);
  });

  it('allows re-scheduling after flush', async () => {
    const perform = vi.fn();
    const schedule = createMutationScheduler(perform);
    const el = document.createElement('div');
    schedule(el);
    await Promise.resolve();
    expect(perform).toHaveBeenCalledTimes(1);

    schedule(el);
    await Promise.resolve();
    expect(perform).toHaveBeenCalledTimes(2);
  });
});
