import { afterEach, describe, expect, it, vi } from 'vitest';

import { createGptLaterIntegrationRegistration } from '../../../src/integrations/gpt/later';
import type {
  IntegrationActivationContext,
  IntegrationPrepareContext,
  PreparedIntegration,
} from '../../../src/kernel/integration_registry';

const RELEASE_ID = 'a'.repeat(64);

type NavigationResult =
  | Readonly<{
      status: 'committed';
      navigationGeneration: object;
      current: true;
    }>
  | Readonly<{
      status: 'rejected';
      navigationGeneration: object;
      current: boolean;
    }>;

function harness() {
  const navigationGeneration = Object.freeze({});
  const navigate = vi.fn<(_path: string) => Promise<NavigationResult>>(async (_path: string) =>
    Object.freeze({ status: 'committed', navigationGeneration, current: true })
  );
  const release = vi.fn();
  const activateLaterLifecycle = vi.fn(() => Object.freeze({ navigate, release }));
  const preparationDisposers: Array<() => void> = [];
  const activationDisposers: Array<() => void> = [];
  const interfaces = Object.freeze({
    'runtime.v1': Object.freeze({ document }),
    'slots.v1': Object.freeze({}),
    'auction.v1': Object.freeze({}),
    'render.v1': Object.freeze({}),
    'trace.v1': Object.freeze({}),
    'gpt.v1': Object.freeze({ activateLaterLifecycle }),
  });
  const prepared = createGptLaterIntegrationRegistration(RELEASE_ID).prepare(
    Object.freeze({
      config: Object.freeze({ gamAttributionEnabled: false }),
      interfaces,
      onDispose: (callback: () => void) => preparationDisposers.push(callback),
      signal: new AbortController().signal,
    } satisfies IntegrationPrepareContext)
  ) as PreparedIntegration;
  return Object.freeze({
    activateLaterLifecycle,
    activationContext: Object.freeze({
      afterCommit: vi.fn(),
      onDispose: (callback: () => void) => activationDisposers.push(callback),
      signal: new AbortController().signal,
    } satisfies IntegrationActivationContext),
    activationDisposers,
    navigate,
    navigationGeneration,
    prepared,
    preparationDisposers,
    release,
  });
}

describe('GPT deferred navigation and reconciliation owner', () => {
  afterEach(() => {
    vi.useRealTimers();
    window.history.replaceState({}, '', '/');
  });

  it('leaves takeover history, listeners, timers, and reconciliation unchanged before activation', () => {
    vi.useFakeTimers();
    const beforePush = window.history.pushState;
    const beforeReplace = window.history.replaceState;
    const owner = harness();

    expect(owner.activateLaterLifecycle).not.toHaveBeenCalled();
    expect(window.history.pushState).toBe(beforePush);
    expect(window.history.replaceState).toBe(beforeReplace);
    expect(vi.getTimerCount()).toBe(0);

    owner.preparationDisposers.reverse().forEach((release) => release());
    expect(owner.activateLaterLifecycle).not.toHaveBeenCalled();
  });

  it('owns one deferred history listener and coalesced navigation timer across repeated routes', async () => {
    vi.useFakeTimers();
    const owner = harness();
    owner.prepared.activate(owner.activationContext);

    expect(owner.activateLaterLifecycle).toHaveBeenCalledOnce();
    expect(owner.navigate).not.toHaveBeenCalled();
    window.history.pushState({}, '', '/first?section=one');
    expect(owner.navigate).not.toHaveBeenCalled();
    expect(vi.getTimerCount()).toBe(1);
    await vi.advanceTimersByTimeAsync(0);
    expect(owner.navigate).toHaveBeenLastCalledWith('/first?section=one');

    window.history.pushState({}, '', '/second');
    window.history.replaceState({}, '', '/third?latest=yes');
    expect(vi.getTimerCount()).toBe(1);
    await vi.advanceTimersByTimeAsync(0);
    expect(owner.navigate).toHaveBeenLastCalledWith('/third?latest=yes');
    expect(owner.navigate).toHaveBeenCalledTimes(2);

    window.dispatchEvent(new PopStateEvent('popstate'));
    await vi.advanceTimersByTimeAsync(0);
    expect(owner.navigate).toHaveBeenCalledTimes(2);

    owner.activationDisposers.reverse().forEach((release) => release());
    owner.preparationDisposers.reverse().forEach((release) => release());
  });

  it('restores exact history ownership and cancels a pending navigation on disposal', async () => {
    vi.useFakeTimers();
    const beforePush = Object.getOwnPropertyDescriptor(window.history, 'pushState');
    const beforeReplace = Object.getOwnPropertyDescriptor(window.history, 'replaceState');
    const owner = harness();
    owner.prepared.activate(owner.activationContext);
    window.history.pushState({}, '', '/pending');
    expect(vi.getTimerCount()).toBe(1);

    owner.activationDisposers.reverse().forEach((release) => release());
    expect(owner.release).toHaveBeenCalledOnce();
    expect(Object.getOwnPropertyDescriptor(window.history, 'pushState')).toEqual(beforePush);
    expect(Object.getOwnPropertyDescriptor(window.history, 'replaceState')).toEqual(beforeReplace);
    expect(vi.getTimerCount()).toBe(0);
    window.dispatchEvent(new PopStateEvent('popstate'));
    await vi.runAllTimersAsync();
    expect(owner.navigate).not.toHaveBeenCalled();

    owner.preparationDisposers.reverse().forEach((release) => release());
  });

  it('retries the same current route after its page-bids navigation is rejected', async () => {
    vi.useFakeTimers();
    const owner = harness();
    owner.navigate.mockResolvedValueOnce(
      Object.freeze({
        status: 'rejected' as const,
        navigationGeneration: owner.navigationGeneration,
        current: true as const,
      })
    );
    owner.prepared.activate(owner.activationContext);

    window.history.pushState({}, '', '/retry-current');
    await vi.advanceTimersByTimeAsync(0);
    expect(owner.navigate).toHaveBeenCalledExactlyOnceWith('/retry-current');

    window.history.replaceState({}, '', '/retry-current');
    expect(vi.getTimerCount()).toBe(1);
    await vi.advanceTimersByTimeAsync(0);
    expect(owner.navigate).toHaveBeenCalledTimes(2);
    expect(owner.navigate).toHaveBeenLastCalledWith('/retry-current');

    owner.activationDisposers.reverse().forEach((release) => release());
    owner.preparationDisposers.reverse().forEach((release) => release());
  });

  it('retries the same current route after the navigation promise rejects', async () => {
    vi.useFakeTimers();
    const owner = harness();
    owner.navigate.mockRejectedValueOnce(new Error('fictional current navigation failure'));
    owner.prepared.activate(owner.activationContext);

    window.history.pushState({}, '', '/retry-rejection');
    await vi.advanceTimersByTimeAsync(0);
    expect(owner.navigate).toHaveBeenCalledExactlyOnceWith('/retry-rejection');

    window.history.replaceState({}, '', '/retry-rejection');
    expect(vi.getTimerCount()).toBe(1);
    await vi.advanceTimersByTimeAsync(0);
    expect(owner.navigate).toHaveBeenCalledTimes(2);
    expect(owner.navigate).toHaveBeenLastCalledWith('/retry-rejection');

    owner.activationDisposers.reverse().forEach((release) => release());
    owner.preparationDisposers.reverse().forEach((release) => release());
  });

  it('does not let a stale generation failure roll back a newer committed route', async () => {
    vi.useFakeTimers();
    const owner = harness();
    const firstGeneration = Object.freeze({});
    const secondGeneration = Object.freeze({});
    let rejectFirst!: (
      result: Readonly<{
        status: 'rejected';
        navigationGeneration: object;
        current: false;
      }>
    ) => void;
    let commitSecond!: (
      result: Readonly<{
        status: 'committed';
        navigationGeneration: object;
        current: true;
      }>
    ) => void;
    owner.navigate
      .mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            rejectFirst = resolve;
          })
      )
      .mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            commitSecond = resolve;
          })
      );
    owner.prepared.activate(owner.activationContext);

    window.history.pushState({}, '', '/stale-first');
    await vi.advanceTimersByTimeAsync(0);
    window.history.pushState({}, '', '/committed-second');
    await vi.advanceTimersByTimeAsync(0);
    expect(owner.navigate).toHaveBeenCalledTimes(2);

    commitSecond(
      Object.freeze({
        status: 'committed',
        navigationGeneration: secondGeneration,
        current: true,
      })
    );
    await Promise.resolve();
    rejectFirst(
      Object.freeze({
        status: 'rejected',
        navigationGeneration: firstGeneration,
        current: false,
      })
    );
    await Promise.resolve();

    window.history.replaceState({}, '', '/committed-second');
    await vi.runAllTimersAsync();
    expect(owner.navigate).toHaveBeenCalledTimes(2);

    owner.activationDisposers.reverse().forEach((release) => release());
    owner.preparationDisposers.reverse().forEach((release) => release());
  });
});
