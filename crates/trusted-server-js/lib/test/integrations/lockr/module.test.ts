import { afterEach, describe, expect, it, vi } from 'vitest';

import { createLockrRuntime } from '../../../src/integrations/lockr/module';

describe('transactional Lockr integration module', () => {
  afterEach(() => vi.useRealTimers());

  it('rewrites a later initialized SDK once and compare-restores its host', async () => {
    vi.useFakeTimers();
    const state: { sdk?: { host: string } } = {};
    const resetGuard = vi.fn();
    const runtime = createLockrRuntime({
      clearTimeout: (timer) => window.clearTimeout(timer),
      getSdk: () => state.sdk,
      installGuard: vi.fn(),
      location: { host: 'news.example', protocol: 'https:' },
      resetGuard,
      setTimeout: (callback, delay) => window.setTimeout(callback, delay),
      started: vi.fn(),
      timedOut: vi.fn(),
    });
    const release = runtime.activate(undefined);
    runtime.start(undefined);
    await vi.advanceTimersByTimeAsync(49);
    const sdk = { host: 'https://identity.loc.kr' };
    state.sdk = sdk;
    await vi.advanceTimersByTimeAsync(1);

    expect(sdk.host).toBe('https://news.example/integrations/lockr/api');
    sdk.host = 'https://publisher.example/replacement';
    release();
    expect(sdk.host).toBe('https://publisher.example/replacement');
    expect(resetGuard).toHaveBeenCalledOnce();
  });

  it('stops after 50 readiness checks and owns no later timer', async () => {
    vi.useFakeTimers();
    const timedOut = vi.fn();
    const setTimeout = vi.fn((callback: () => void, delay: number) =>
      window.setTimeout(callback, delay)
    );
    const runtime = createLockrRuntime({
      clearTimeout: (timer) => window.clearTimeout(timer),
      getSdk: () => undefined,
      installGuard: vi.fn(),
      location: { host: 'news.example', protocol: 'https:' },
      resetGuard: vi.fn(),
      setTimeout,
      started: vi.fn(),
      timedOut,
    });
    const release = runtime.activate(undefined);
    runtime.start(undefined);

    await vi.advanceTimersByTimeAsync(2_500);

    expect(setTimeout).toHaveBeenCalledTimes(49);
    expect(timedOut).toHaveBeenCalledOnce();
    expect(vi.getTimerCount()).toBe(0);
    release();
  });

  it('cancels readiness work on disposal before the SDK appears', async () => {
    vi.useFakeTimers();
    const sdk = { host: 'https://identity.loc.kr' };
    let available = false;
    const runtime = createLockrRuntime({
      clearTimeout: (timer) => window.clearTimeout(timer),
      getSdk: () => (available ? sdk : undefined),
      installGuard: vi.fn(),
      location: { host: 'news.example', protocol: 'https:' },
      resetGuard: vi.fn(),
      setTimeout: (callback, delay) => window.setTimeout(callback, delay),
      started: vi.fn(),
      timedOut: vi.fn(),
    });
    const release = runtime.activate(undefined);
    runtime.start(undefined);
    release();
    available = true;

    await vi.runAllTimersAsync();

    expect(sdk.host).toBe('https://identity.loc.kr');
    expect(vi.getTimerCount()).toBe(0);
  });
});
