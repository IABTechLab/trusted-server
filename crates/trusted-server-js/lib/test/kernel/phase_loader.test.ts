import { afterEach, describe, expect, it, vi } from 'vitest';

import type { BootManifestV1 } from '../../src/core/types';
import {
  createDeferredPhaseLoader,
  createProtectedFirstDisplayGate,
  type DeferredPhaseLoaderOptions,
  type PhaseScheduler,
} from '../../src/kernel/phase_loader';

const RELEASE_ID = 'a'.repeat(64);
const HASH = 'b'.repeat(64);

function scheduler(options: { idle?: boolean } = {}): {
  readonly frames: FrameRequestCallback[];
  readonly idle: Array<() => void>;
  readonly value: PhaseScheduler;
} {
  const frames: FrameRequestCallback[] = [];
  const idle: Array<() => void> = [];
  return {
    frames,
    idle,
    value: {
      cancelAnimationFrame: vi.fn(),
      clearTimeout,
      requestAnimationFrame: (callback) => {
        frames.push(callback);
        return frames.length;
      },
      ...(options.idle
        ? {
            cancelIdleCallback: vi.fn(),
            requestIdleCallback: (callback: () => void) => {
              idle.push(callback);
              return idle.length;
            },
          }
        : {}),
      setTimeout,
    },
  };
}

function deferredManifest(ids: readonly string[]): BootManifestV1 {
  return Object.freeze({
    version: 1,
    releaseId: RELEASE_ID,
    firstDisplay: null,
    runtimeSrc: `/static/tsjs=tsjs-unified.min.js?v=${HASH}`,
    integrations: Object.freeze([
      Object.freeze({ id: 'render_runtime', phase: 'takeover' as const }),
      ...ids.map((id) =>
        Object.freeze({
          id,
          phase: 'deferred' as const,
          trigger: 'first_display_or_idle' as const,
          src: `/static/tsjs=tsjs-${id}.min.js?v=${HASH}`,
        })
      ),
    ]),
  });
}

function deferredRegistration(
  id: string,
  prepare = vi.fn(() => Object.freeze({ activate: () => undefined }))
): object {
  return Object.freeze({
    abi: 1,
    id,
    phase: 'deferred',
    releaseId: RELEASE_ID,
    prepare,
  });
}

afterEach(() => {
  vi.useRealTimers();
  document.head.replaceChildren();
  vi.restoreAllMocks();
});

describe('protected first-display paint gate', () => {
  it('adopts an agent-owned protected paint and proceeds directly to idle', async () => {
    const platform = scheduler({ idle: true });
    const markPaint = vi.fn();
    const gate = createProtectedFirstDisplayGate({
      document,
      markPaint,
      paintAlreadyRecorded: true,
      scheduler: platform.value,
    });

    gate.commit();

    expect(platform.frames).toEqual([]);
    expect(platform.idle).toHaveLength(1);
    expect(markPaint).not.toHaveBeenCalled();
    expect(gate.protectAttemptBatch([Promise.resolve()])).toBe(false);
    platform.idle.shift()?.();
    await expect(gate.ready).resolves.toBe(true);
  });

  it('releases a no-attempt page only at 10 seconds, after two frames and idle', async () => {
    vi.useFakeTimers();
    const platform = scheduler({ idle: true });
    const marks: string[] = [];
    const gate = createProtectedFirstDisplayGate({
      document,
      markPaint: () => marks.push('paint'),
      scheduler: platform.value,
    });
    let released = false;
    void gate.ready.then(() => (released = true));

    gate.commit();
    await vi.advanceTimersByTimeAsync(9_999);
    expect(released).toBe(false);
    await vi.advanceTimersByTimeAsync(1);
    expect(platform.frames).toHaveLength(1);
    platform.frames.shift()?.(10_000);
    expect(platform.frames).toHaveLength(1);
    expect(marks).toEqual([]);
    platform.frames.shift()?.(10_016);
    expect(marks).toEqual(['paint']);
    expect(released).toBe(false);
    expect(platform.idle).toHaveLength(1);
    platform.idle.shift()?.();
    await Promise.resolve();
    expect(released).toBe(true);
  });

  it('protects the first batch created at 9,999 ms until every terminal latch settles', async () => {
    vi.useFakeTimers();
    const platform = scheduler({ idle: true });
    let settle: (() => void) | undefined;
    const terminal = new Promise<void>((resolve) => (settle = resolve));
    const gate = createProtectedFirstDisplayGate({ document, scheduler: platform.value });

    gate.commit();
    await vi.advanceTimersByTimeAsync(9_999);
    expect(gate.protectAttemptBatch(Object.freeze([terminal]))).toBe(true);
    await vi.advanceTimersByTimeAsync(10_001);
    expect(platform.frames).toEqual([]);
    settle?.();
    await Promise.resolve();
    await Promise.resolve();
    expect(platform.frames).toHaveLength(1);
    platform.frames.shift()?.(20_000);
    platform.frames.shift()?.(20_016);
    platform.idle.shift()?.();
    await expect(gate.ready).resolves.toBe(true);
  });

  it('uses a post-paint 50 ms fallback only when requestIdleCallback is unavailable', async () => {
    vi.useFakeTimers();
    const platform = scheduler();
    const gate = createProtectedFirstDisplayGate({ document, scheduler: platform.value });
    let released = false;
    void gate.ready.then(() => (released = true));

    gate.commit();
    await vi.advanceTimersByTimeAsync(10_000);
    platform.frames.shift()?.(10_000);
    platform.frames.shift()?.(10_016);
    await vi.advanceTimersByTimeAsync(49);
    expect(released).toBe(false);
    await vi.advanceTimersByTimeAsync(1);
    expect(released).toBe(true);
  });

  it.each([10_000, 10_001])(
    'does not protect a first attempt created at %i ms after the no-attempt release',
    async (createdAtMs) => {
      vi.useFakeTimers();
      const platform = scheduler({ idle: true });
      const gate = createProtectedFirstDisplayGate({ document, scheduler: platform.value });
      gate.commit();

      await vi.advanceTimersByTimeAsync(createdAtMs);
      expect(gate.protectAttemptBatch([Promise.resolve()])).toBe(false);
    }
  );

  it('freezes the first protected batch and waits for every one of its members', async () => {
    vi.useFakeTimers();
    const platform = scheduler({ idle: true });
    let settleFirst: (() => void) | undefined;
    let settleSecond: (() => void) | undefined;
    const first = new Promise<void>((resolve) => (settleFirst = resolve));
    const second = new Promise<void>((resolve) => (settleSecond = resolve));
    const gate = createProtectedFirstDisplayGate({ document, scheduler: platform.value });
    gate.commit();

    expect(gate.protectAttemptBatch([first, second])).toBe(true);
    expect(gate.protectAttemptBatch([Promise.resolve()])).toBe(false);
    settleFirst?.();
    await Promise.resolve();
    await Promise.resolve();
    expect(platform.frames).toEqual([]);
    settleSecond?.();
    await Promise.resolve();
    await Promise.resolve();
    expect(platform.frames).toHaveLength(1);
  });

  it('waits for visibility and two frames when a hidden page becomes visible first', async () => {
    vi.useFakeTimers();
    const platform = scheduler({ idle: true });
    Object.defineProperty(document, 'visibilityState', { configurable: true, value: 'hidden' });
    const gate = createProtectedFirstDisplayGate({ document, scheduler: platform.value });
    gate.commit();
    await vi.advanceTimersByTimeAsync(10_000);
    await vi.advanceTimersByTimeAsync(1_999);
    expect(platform.frames).toEqual([]);

    Object.defineProperty(document, 'visibilityState', { configurable: true, value: 'visible' });
    document.dispatchEvent(new Event('visibilitychange'));
    expect(platform.frames).toHaveLength(1);
    platform.frames.shift()?.(12_000);
    platform.frames.shift()?.(12_016);
    platform.idle.shift()?.();
    await expect(gate.ready).resolves.toBe(true);
  });

  it('uses the two-second hidden timeout without requesting a frame', async () => {
    vi.useFakeTimers();
    const platform = scheduler({ idle: true });
    Object.defineProperty(document, 'visibilityState', { configurable: true, value: 'hidden' });
    const gate = createProtectedFirstDisplayGate({ document, scheduler: platform.value });
    gate.commit();
    await vi.advanceTimersByTimeAsync(11_999);
    expect(platform.idle).toEqual([]);
    await vi.advanceTimersByTimeAsync(1);
    expect(platform.frames).toEqual([]);
    expect(platform.idle).toHaveLength(1);
    platform.idle.shift()?.();
    await expect(gate.ready).resolves.toBe(true);
  });
});

describe('authenticated deferred module loading', () => {
  it('starts every module in manifest order without awaiting a sibling', async () => {
    const prepare = vi.fn();
    const takeover = document.createElement('script');
    takeover.nonce = 'response-nonce';
    const loader = createDeferredPhaseLoader({
      runtimeScript: takeover,
      document,
      prepare,
      gate: Promise.resolve(),
      manifest: deferredManifest(['gpt_later', 'prebid_later']),
      releaseId: RELEASE_ID,
    });

    await Promise.resolve();
    const scripts = [...document.head.querySelectorAll('script')];
    expect(scripts.map((script) => new URL(script.src).pathname)).toEqual([
      '/static/tsjs=tsjs-gpt_later.min.js',
      '/static/tsjs=tsjs-prebid_later.min.js',
    ]);
    expect(scripts.every((script) => script.async && script.nonce === 'response-nonce')).toBe(true);
    expect(loader.state('gpt_later')).toBe('loading');
    expect(loader.state('prebid_later')).toBe('loading');
  });

  it('requires one exact registration from the exact connected current script', async () => {
    const prepare = vi.fn<DeferredPhaseLoaderOptions['prepare']>((registration, owner) =>
      registration.prepare(
        Object.freeze({
          config: Object.freeze({}),
          interfaces: Object.freeze({}),
          signal: owner.signal,
          onDispose: owner.onDispose,
        })
      )
    );
    const takeover = document.createElement('script');
    const loader = createDeferredPhaseLoader({
      runtimeScript: takeover,
      document,
      prepare,
      gate: Promise.resolve(),
      manifest: deferredManifest(['gpt_later']),
      releaseId: RELEASE_ID,
    });
    await Promise.resolve();
    const script = document.head.querySelector('script');
    expect(script).not.toBeNull();
    Object.defineProperty(document, 'currentScript', { configurable: true, value: script });

    const registration = deferredRegistration('gpt_later');
    expect(loader.register(registration)).toBe(true);
    script?.dispatchEvent(new Event('load'));
    await vi.waitFor(() => expect(prepare).toHaveBeenCalledOnce());
    expect(loader.state('gpt_later')).toBe('ready');
    expect(loader.register(registration)).toBe(false);
  });

  it('isolates a failed module while a sibling reaches ready', async () => {
    const prepare = vi.fn(async (registration: { readonly id: string }) => {
      if (registration.id === 'gpt_later') throw new Error('fictional module failure');
      return Object.freeze({ activate: () => undefined });
    });
    const loader = createDeferredPhaseLoader({
      runtimeScript: document.createElement('script'),
      document,
      prepare,
      gate: Promise.resolve(),
      manifest: deferredManifest(['gpt_later', 'prebid_later']),
      releaseId: RELEASE_ID,
    });
    await Promise.resolve();
    const scripts = [...document.head.querySelectorAll('script')];
    for (const [index, id] of ['gpt_later', 'prebid_later'].entries()) {
      Object.defineProperty(document, 'currentScript', {
        configurable: true,
        value: scripts[index],
      });
      expect(loader.register(deferredRegistration(id))).toBe(true);
      scripts[index]?.dispatchEvent(new Event('load'));
    }

    await vi.waitFor(() => expect(loader.state('prebid_later')).toBe('ready'));
    expect(loader.state('gpt_later')).toBe('unavailable');
    expect(loader.reason('gpt_later')).toBe('prepare_failed');
  });

  it('classifies exact URL mutation before insertion as policy_blocked', async () => {
    const takeover = document.createElement('script');
    const originalCreate = document.createElement.bind(document);
    vi.spyOn(document, 'createElement').mockImplementation(((name: string) => {
      const element = originalCreate(name);
      if (name === 'script') {
        Object.defineProperty(element, 'src', {
          configurable: true,
          get: () => 'https://publisher.example/mutated.js',
          set: () => undefined,
        });
      }
      return element;
    }) as typeof document.createElement);
    const loader = createDeferredPhaseLoader({
      runtimeScript: takeover,
      document,
      prepare: vi.fn(),
      gate: Promise.resolve(),
      manifest: deferredManifest(['gpt_later']),
      releaseId: RELEASE_ID,
    });

    await vi.waitFor(() => expect(loader.state('gpt_later')).toBe('unavailable'));
    expect(loader.reason('gpt_later')).toBe('policy_blocked');
    expect(document.head.querySelector('script')).toBeNull();
  });

  it.each([
    ['error', 'load_error'],
    ['load', 'load_without_registration'],
  ] as const)('classifies a script %s without accepted registration', async (event, reason) => {
    const loader = createDeferredPhaseLoader({
      runtimeScript: document.createElement('script'),
      document,
      prepare: vi.fn(),
      gate: Promise.resolve(),
      manifest: deferredManifest(['gpt_later']),
      releaseId: RELEASE_ID,
    });
    await Promise.resolve();
    document.head.querySelector('script')?.dispatchEvent(new Event(event));

    await vi.waitFor(() => expect(loader.state('gpt_later')).toBe('unavailable'));
    expect(loader.reason('gpt_later')).toBe(reason);
  });

  it('rejects registration after the expected node is removed or replaced', async () => {
    const loader = createDeferredPhaseLoader({
      runtimeScript: document.createElement('script'),
      document,
      prepare: vi.fn(),
      gate: Promise.resolve(),
      manifest: deferredManifest(['gpt_later']),
      releaseId: RELEASE_ID,
    });
    await Promise.resolve();
    const expected = document.head.querySelector('script');
    const replacement = document.createElement('script');
    expected?.replaceWith(replacement);
    Object.defineProperty(document, 'currentScript', { configurable: true, value: replacement });

    expect(loader.register(deferredRegistration('gpt_later'))).toBe(false);
    expect(loader.reason('gpt_later')).toBe('registration_rejected');
  });

  it.each([
    [
      'activation',
      () =>
        Object.freeze({
          activate: () => {
            throw new Error('activation');
          },
        }),
      'activation_failed',
    ],
    [
      'after commit',
      () =>
        Object.freeze({
          activate: ({ afterCommit }: { afterCommit: (callback: () => void) => void }) =>
            afterCommit(() => {
              throw new Error('after commit');
            }),
        }),
      'after_commit_failed',
    ],
  ] as const)('classifies an %s failure at its exact stage', async (_name, prepared, reason) => {
    const loader = createDeferredPhaseLoader({
      runtimeScript: document.createElement('script'),
      document,
      prepare: () => prepared(),
      gate: Promise.resolve(),
      manifest: deferredManifest(['gpt_later']),
      releaseId: RELEASE_ID,
    });
    await Promise.resolve();
    const script = document.head.querySelector('script');
    Object.defineProperty(document, 'currentScript', { configurable: true, value: script });
    expect(loader.register(deferredRegistration('gpt_later'))).toBe(true);
    script?.dispatchEvent(new Event('load'));

    await vi.waitFor(() => expect(loader.state('gpt_later')).toBe('unavailable'));
    expect(loader.reason('gpt_later')).toBe(reason);
  });

  it('keeps the shared module alive after one caller deadline expires', async () => {
    vi.useFakeTimers();
    const loader = createDeferredPhaseLoader({
      runtimeScript: document.createElement('script'),
      document,
      prepare: () => Object.freeze({ activate: () => undefined }),
      gate: Promise.resolve(),
      manifest: deferredManifest(['gpt_later']),
      releaseId: RELEASE_ID,
    });
    await Promise.resolve();
    const caller = loader.waitFor('gpt_later', 100);
    await vi.advanceTimersByTimeAsync(100);
    await expect(caller).resolves.toBe('caller_timeout');
    expect(loader.state('gpt_later')).toBe('loading');

    const script = document.head.querySelector('script');
    Object.defineProperty(document, 'currentScript', { configurable: true, value: script });
    expect(loader.register(deferredRegistration('gpt_later'))).toBe(true);
    script?.dispatchEvent(new Event('load'));
    await expect(loader.waitFor('gpt_later', 100)).resolves.toBe('ready');
  });

  it('retires a hung shared module at its independent ten-second deadline', async () => {
    vi.useFakeTimers();
    const loader = createDeferredPhaseLoader({
      runtimeScript: document.createElement('script'),
      document,
      prepare: vi.fn(),
      gate: Promise.resolve(),
      manifest: deferredManifest(['gpt_later']),
      releaseId: RELEASE_ID,
    });
    await Promise.resolve();
    await vi.advanceTimersByTimeAsync(9_999);
    expect(loader.state('gpt_later')).toBe('loading');
    await vi.advanceTimersByTimeAsync(1);
    expect(loader.state('gpt_later')).toBe('unavailable');
    expect(loader.reason('gpt_later')).toBe('module_timeout');
  });

  it('does not start after the owning gate is disposed', async () => {
    const gate = createProtectedFirstDisplayGate({ document, scheduler: scheduler().value });
    const loader = createDeferredPhaseLoader({
      runtimeScript: document.createElement('script'),
      document,
      prepare: vi.fn(),
      gate: gate.ready,
      manifest: deferredManifest(['gpt_later']),
      releaseId: RELEASE_ID,
    });

    gate.dispose();
    await Promise.resolve();
    await Promise.resolve();
    expect(document.head.querySelector('script')).toBeNull();
    expect(loader.reason('gpt_later')).toBe('disposed');
  });

  it('uses window origin rather than a hostile document base URL', async () => {
    const base = document.createElement('base');
    base.href = 'https://attacker.example/subtree/';
    document.head.append(base);
    createDeferredPhaseLoader({
      runtimeScript: document.createElement('script'),
      document,
      prepare: vi.fn(),
      gate: Promise.resolve(),
      manifest: deferredManifest(['gpt_later']),
      releaseId: RELEASE_ID,
    });
    await Promise.resolve();

    expect(document.head.querySelector('script')?.src).toBe(
      `${window.location.origin}/static/tsjs=tsjs-gpt_later.min.js?v=${HASH}`
    );
  });

  it('creates the fixed Trusted Types policy once and admits only canonical absolute URLs', async () => {
    const createPolicy = vi.fn((_name: string, rules: { createScriptURL(value: string): string }) =>
      Object.freeze({ createScriptURL: rules.createScriptURL })
    );
    Object.defineProperty(window, 'trustedTypes', {
      configurable: true,
      value: Object.freeze({ createPolicy }),
    });
    createDeferredPhaseLoader({
      runtimeScript: document.createElement('script'),
      document,
      prepare: vi.fn(),
      gate: Promise.resolve(),
      manifest: deferredManifest(['gpt_later', 'prebid_later']),
      releaseId: RELEASE_ID,
    });
    await Promise.resolve();

    expect(createPolicy).toHaveBeenCalledOnce();
    expect(createPolicy).toHaveBeenCalledWith(
      'trusted-server#tsjs-v1',
      expect.objectContaining({ createScriptURL: expect.any(Function) })
    );
    const rules = createPolicy.mock.calls[0]?.[1];
    expect(() => rules?.createScriptURL('https://attacker.example/x.js')).toThrow();
  });

  it('copies no nonce when the takeover script has no nonempty nonce', async () => {
    createDeferredPhaseLoader({
      runtimeScript: document.createElement('script'),
      document,
      prepare: vi.fn(),
      gate: Promise.resolve(),
      manifest: deferredManifest(['gpt_later']),
      releaseId: RELEASE_ID,
    });
    await Promise.resolve();
    expect(document.head.querySelector('script')?.nonce).toBe('');
  });
});
