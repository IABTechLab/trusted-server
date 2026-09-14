import { describe, expect, it, vi } from 'vitest';

import { DisposableStack, TerminalLatch } from '../../src/kernel/disposable';

describe('DisposableStack', () => {
  it('aborts and disposes in reverse order exactly once while isolating failures', () => {
    const calls: string[] = [];
    const errors: unknown[] = [];
    const stack = new DisposableStack((error) => errors.push(error));

    stack.onDispose(() => calls.push('first'));
    stack.onDispose(() => {
      calls.push('second');
      throw new Error('fictional disposer failure');
    });
    stack.onDispose(() => calls.push('third'));
    stack.signal.addEventListener('abort', () => calls.push('abort'));

    stack.dispose();
    stack.dispose();

    expect(stack.disposed).toBe(true);
    expect(stack.signal.aborted).toBe(true);
    expect(calls).toEqual(['abort', 'third', 'second', 'first']);
    expect(errors).toHaveLength(1);
  });

  it('runs a disposer registered after disposal immediately and isolates its failure', () => {
    const calls: string[] = [];
    const onError = vi.fn();
    const stack = new DisposableStack(onError);
    stack.dispose();

    stack.onDispose(() => calls.push('late'));
    stack.onDispose(() => {
      throw new Error('late fictional failure');
    });

    expect(calls).toEqual(['late']);
    expect(onError).toHaveBeenCalledTimes(1);
  });

  it('observes a rejecting async disposer without delaying terminal disposal', async () => {
    const onError = vi.fn();
    const stack = new DisposableStack(onError);
    stack.onDispose(async () => {
      throw new Error('fictional async disposer failure');
    });

    stack.dispose();

    expect(stack.disposed).toBe(true);
    expect(stack.signal.aborted).toBe(true);
    await vi.waitFor(() => expect(onError).toHaveBeenCalledTimes(1));
  });
});

describe('TerminalLatch', () => {
  it('lets only the first terminal result win and disposes before completion', async () => {
    const events: string[] = [];
    const latch = new TerminalLatch<{ outcome: string }>();
    latch.onDispose(() => events.push('disposed'));
    latch.completion.then(() => events.push('completed'));

    expect(latch.trySettle({ outcome: 'accepted' })).toBe(true);
    expect(latch.trySettle({ outcome: 'failed' })).toBe(false);
    expect(latch.terminal).toBe(true);
    expect(latch.value).toEqual({ outcome: 'accepted' });
    await expect(latch.completion).resolves.toEqual({ outcome: 'accepted' });
    expect(events).toEqual(['disposed', 'completed']);
  });

  it('supports undefined as a terminal value without reopening the latch', async () => {
    const latch = new TerminalLatch<undefined>();

    expect(latch.trySettle(undefined)).toBe(true);
    expect(latch.terminal).toBe(true);
    expect(latch.trySettle(undefined)).toBe(false);
    await expect(latch.completion).resolves.toBeUndefined();
  });
});
