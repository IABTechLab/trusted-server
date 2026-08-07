import { afterEach, describe, expect, expectTypeOf, it, vi } from 'vitest';

import {
  AdUnitRegistrationError,
  RequestAdsInputError,
  TsjsUnavailableError,
  type AdUnitRegistrationErrorCode,
} from '../../src/kernel/fallback';
import { createRuntime } from '../../src/kernel/runtime';

const RELEASE = 'a'.repeat(64);

function boot(results: readonly object[] = []) {
  return {
    auctionProjection: {
      version: 1,
      auction: { version: 1, auctionId: 'boot', results },
      bids: [],
    },
    creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
    diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
  };
}

function manifest(ids: readonly string[]) {
  return {
    version: 1,
    releaseId: RELEASE,
    integrations: ids.map((id) => ({ id, required: true })),
  };
}

type ReflectionTrap = 'getPrototypeOf' | 'ownKeys' | 'getOwnPropertyDescriptor';

function hostileRecord(trap: ReflectionTrap, target: object = {}): object {
  const fail = () => {
    throw new Error(`hostile ${trap}`);
  };
  const handler: ProxyHandler<object> = {};
  if (trap === 'getPrototypeOf') handler.getPrototypeOf = fail;
  if (trap === 'ownKeys') handler.ownKeys = fail;
  if (trap === 'getOwnPropertyDescriptor') handler.getOwnPropertyDescriptor = fail;
  return new Proxy(target, handler);
}

function thrownBy(callback: () => unknown): unknown {
  try {
    callback();
  } catch (error) {
    return error;
  }
  throw new Error('Expected callback to throw');
}

describe('Runtime bootstrap owner', () => {
  afterEach(() => vi.useRealTimers());

  it('exports the exact programmatic registration error taxonomy', () => {
    type ExpectedCode =
      | 'invalid_units'
      | 'invalid_unit'
      | 'invalid_code'
      | 'duplicate_code'
      | 'slot_collision'
      | 'invalid_media_types'
      | 'invalid_dimensions'
      | 'dimensions_out_of_range'
      | 'invalid_bids'
      | 'invalid_bidder'
      | 'invalid_params'
      | 'request_body_too_large'
      | 'registry_capacity';

    expectTypeOf<AdUnitRegistrationErrorCode>().toEqualTypeOf<ExpectedCode>();
    expectTypeOf<AdUnitRegistrationError['code']>().toEqualTypeOf<ExpectedCode>();
  });

  it('commits one kernel after core/integration activation and afterCommit before queue drain', async () => {
    const order: string[] = [];
    const target = { que: [() => order.push('queued')], config: { publisher: true } };
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: manifest(['gpt']),
      knownIntegrationIds: Object.freeze(['gpt']),
      boot: boot(),
      activateCore: () => order.push('core'),
      kernel: {
        addAdUnits: () => ({ registered: [] }),
        diagnostics: Object.freeze({}),
        requestAds: async () => ({ slots: [] }),
      },
    });

    expect(runtime.state).toBe('unclaimed');
    expect(runtime.start()).toBe(true);
    expect(runtime.state).toBe('installing');
    expect(target.config).toEqual({ publisher: true });
    expect(
      runtime.registerIntegration({
        id: 'gpt',
        release: RELEASE,
        prepare: () => ({
          activate: ({ afterCommit }: { afterCommit(callback: () => void): void }) => {
            order.push('integration');
            afterCommit(() => order.push('after-commit'));
          },
        }),
      })
    ).toBe(true);

    await expect(runtime.install()).resolves.toMatchObject({ state: 'kernel' });
    expect(runtime.state).toBe('kernel');
    expect(order).toEqual(['core', 'integration', 'after-commit', 'queued']);
    expect(target).toMatchObject({ version: '1.0.0', releaseId: RELEASE });
    expect(Object.isFrozen(target.que)).toBe(true);
    expect(Object.getOwnPropertyDescriptor(target, '_internal')).toMatchObject({
      enumerable: false,
      writable: false,
      configurable: false,
    });
    expect(
      (target as { _registerIntegration?: (value: unknown) => boolean })._registerIntegration?.({
        id: 'late',
      })
    ).toBe(false);
  });

  it('runs queued work at the exact activation, commit, afterCommit, and FIFO drain boundaries', async () => {
    const order: string[] = [];
    let commitPushInstalled = false;
    const backing: { que?: unknown[]; version?: string } = {
      que: [
        function (this: unknown) {
          expect(this).toBe(target);
          order.push('preload-start');
          target.que?.push(() => order.push('preload-nested'));
          order.push('preload-end');
        },
      ],
    };
    const target = new Proxy(backing, {
      defineProperty(object, key, descriptor) {
        if (key === 'version' && !commitPushInstalled) {
          commitPushInstalled = true;
          object.que?.push(() => order.push('commit-enqueued'));
        }
        return Reflect.defineProperty(object, key, descriptor);
      },
    });
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: manifest(['gpt']),
      knownIntegrationIds: Object.freeze(['gpt']),
      boot: boot(),
      activateCore: () => {
        order.push('core-activation');
        target.que?.push(() => order.push('core-enqueued'));
      },
      kernel: {
        addAdUnits: vi.fn(),
        diagnostics: Object.freeze({}),
        requestAds: vi.fn(),
      },
    });

    expect(runtime.start()).toBe(true);
    expect(
      runtime.registerIntegration({
        id: 'gpt',
        release: RELEASE,
        prepare: () => ({
          activate: ({ afterCommit }: { afterCommit(callback: () => void): void }) => {
            order.push('module-activation');
            target.que?.push(() => order.push('module-enqueued'));
            afterCommit(() => {
              order.push('after-commit-start');
              target.que?.push(() => order.push('after-commit-enqueued'));
              order.push('after-commit-end');
            });
          },
        }),
      })
    ).toBe(true);

    await expect(runtime.install()).resolves.toMatchObject({ state: 'kernel' });

    expect(order).toEqual([
      'core-activation',
      'module-activation',
      'commit-enqueued',
      'after-commit-start',
      'after-commit-enqueued',
      'after-commit-end',
      'preload-start',
      'preload-nested',
      'preload-end',
      'core-enqueued',
      'module-enqueued',
    ]);
  });

  it.each([
    ['invalid manifest', { version: 2 }, 'abi_mismatch'],
    ['missing bundle', manifest(['gpt']), 'bundle_partial'],
  ] as const)('commits terminal fallback for %s', async (_name, candidateManifest, reason) => {
    const queued = vi.fn();
    const activateCore = vi.fn();
    const target = { que: [queued], boot: boot() };
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: candidateManifest,
      knownIntegrationIds: Object.freeze(['gpt']),
      boot: target.boot,
      activateCore,
      kernel: {
        addAdUnits: vi.fn(),
        diagnostics: Object.freeze({}),
        requestAds: vi.fn(),
      },
    });

    runtime.start();
    await expect(runtime.install()).resolves.toEqual({ state: 'fallback', reason });

    expect(runtime.state).toBe('fallback');
    expect(activateCore).not.toHaveBeenCalled();
    expect(queued).toHaveBeenCalledOnce();
    expect(target).toMatchObject({ version: '1.0.0', releaseId: RELEASE });
    expect((target as { _internal?: unknown })._internal).toEqual({
      state: 'fallback',
      releaseId: RELEASE,
      reason,
    });
    await expect(
      (target as unknown as { requestAds(options?: unknown): Promise<unknown> }).requestAds()
    ).resolves.toEqual({ slots: [] });
    expect(
      (target as unknown as { _registerIntegration(value: unknown): boolean })._registerIntegration(
        {
          id: 'gpt',
          release: RELEASE,
          prepare: vi.fn(),
        }
      )
    ).toBe(false);
  });

  it('removes legacy and fallback-forbidden surfaces at terminal publication', async () => {
    const target = {
      que: [] as unknown[],
      diagnostics: { legacy: true },
      adInit: vi.fn(),
      renderAdUnit: vi.fn(),
      setConfig: vi.fn(),
      publisher: { retained: true },
    };
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: { version: 2 },
      knownIntegrationIds: Object.freeze([]),
      boot: boot(),
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });

    expect(runtime.start()).toBe(true);
    await expect(runtime.install()).resolves.toEqual({
      state: 'fallback',
      reason: 'abi_mismatch',
    });

    expect(Object.prototype.hasOwnProperty.call(target, 'diagnostics')).toBe(false);
    expect(Object.prototype.hasOwnProperty.call(target, 'adInit')).toBe(false);
    expect(Object.prototype.hasOwnProperty.call(target, 'renderAdUnit')).toBe(false);
    expect(Object.prototype.hasOwnProperty.call(target, 'setConfig')).toBe(false);
    expect(target.publisher).toEqual({ retained: true });
  });

  it('allows exactly one bootstrap owner for a namespace', () => {
    const target = {};
    const options = {
      target,
      releaseId: RELEASE,
      manifest: manifest([]),
      knownIntegrationIds: Object.freeze([] as string[]),
      boot: boot(),
      kernel: {
        addAdUnits: () => ({ registered: [] }),
        diagnostics: Object.freeze({}),
        requestAds: async () => ({ slots: [] }),
      },
    };
    const first = createRuntime(options);
    const second = createRuntime(options);

    expect(first.start()).toBe(true);
    expect(second.start()).toBe(false);
    expect(first.generation).not.toBe(second.generation);
  });

  it('self-discards a stale async preparation before activation when a later owner commits', async () => {
    const target = {};
    const staleCoreActivation = vi.fn();
    const staleModuleActivation = vi.fn();
    const staleDisposal = vi.fn();
    let resolveStalePreparation: (() => void) | undefined;
    const stalePreparation = new Promise<void>((resolve) => {
      resolveStalePreparation = resolve;
    });
    const first = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: manifest(['gpt']),
      knownIntegrationIds: Object.freeze(['gpt']),
      boot: boot(),
      activateCore: staleCoreActivation,
      kernel: {
        addAdUnits: vi.fn(),
        diagnostics: Object.freeze({}),
        requestAds: vi.fn(),
      },
    });
    const secondRequestAds = vi.fn();
    const second = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: manifest(['gpt']),
      knownIntegrationIds: Object.freeze(['gpt']),
      boot: boot(),
      kernel: {
        addAdUnits: vi.fn(),
        diagnostics: Object.freeze({}),
        requestAds: secondRequestAds,
      },
    });

    expect(first.start()).toBe(true);
    expect(
      first.registerIntegration({
        id: 'gpt',
        release: RELEASE,
        prepare: ({ onDispose }: { onDispose(callback: () => void): void }) => {
          onDispose(staleDisposal);
          return stalePreparation.then(() => ({ activate: staleModuleActivation }));
        },
      })
    ).toBe(true);
    const staleInstall = first.install();
    await Promise.resolve();

    expect(Reflect.deleteProperty(target, '_registerIntegration')).toBe(true);
    expect(second.start()).toBe(true);
    expect(
      second.registerIntegration({
        id: 'gpt',
        release: RELEASE,
        prepare: () => ({ activate: vi.fn() }),
      })
    ).toBe(true);
    await expect(second.install()).resolves.toMatchObject({ state: 'kernel' });

    resolveStalePreparation?.();
    await expect(staleInstall).resolves.toEqual({ state: 'fallback', reason: 'bundle_partial' });

    expect(staleCoreActivation).not.toHaveBeenCalled();
    expect(staleModuleActivation).not.toHaveBeenCalled();
    expect(staleDisposal).toHaveBeenCalledOnce();
    expect(first.state).toBe('failed');
    expect(second.state).toBe('kernel');
    expect((target as { requestAds?: unknown }).requestAds).toBe(secondRequestAds);
    expect((target as { _internal?: unknown })._internal).toEqual({
      state: 'kernel',
      releaseId: RELEASE,
    });
  });

  it('rejects registration when candidate reflection replaces the owner handshake', async () => {
    const target = {};
    const staleCoreActivation = vi.fn();
    const staleModuleActivation = vi.fn();
    const options = {
      target,
      releaseId: RELEASE,
      manifest: manifest(['gpt']),
      knownIntegrationIds: Object.freeze(['gpt']),
      boot: boot(),
      kernel: {
        addAdUnits: vi.fn(),
        diagnostics: Object.freeze({}),
        requestAds: vi.fn(),
      },
    };
    const first = createRuntime({ ...options, activateCore: staleCoreActivation });
    const second = createRuntime(options);

    expect(first.start()).toBe(true);
    const registration = new Proxy(
      { id: 'gpt', release: RELEASE, prepare: () => ({ activate: staleModuleActivation }) },
      {
        ownKeys(candidate) {
          expect(Reflect.deleteProperty(target, '_registerIntegration')).toBe(true);
          return Reflect.ownKeys(candidate);
        },
      }
    );

    expect(first.registerIntegration(registration)).toBe(false);
    expect(second.start()).toBe(true);
    expect(
      second.registerIntegration({
        id: 'gpt',
        release: RELEASE,
        prepare: () => ({ activate: vi.fn() }),
      })
    ).toBe(true);
    await expect(second.install()).resolves.toMatchObject({ state: 'kernel' });
    await expect(first.install()).resolves.toEqual({
      state: 'fallback',
      reason: 'bundle_partial',
    });

    expect(staleCoreActivation).not.toHaveBeenCalled();
    expect(staleModuleActivation).not.toHaveBeenCalled();
    expect(first.state).toBe('failed');
    expect(second.state).toBe('kernel');
  });

  it('allows exactly one bootstrap owner across independently evaluated core modules', async () => {
    const target = {};
    const options = {
      target,
      releaseId: RELEASE,
      manifest: manifest([]),
      knownIntegrationIds: Object.freeze([] as string[]),
      boot: boot(),
      kernel: {
        addAdUnits: () => ({ registered: [] }),
        diagnostics: Object.freeze({}),
        requestAds: async () => ({ slots: [] }),
      },
    };
    const firstModule = await import('../../src/kernel/runtime');
    vi.resetModules();
    const secondModule = await import('../../src/kernel/runtime');
    const first = firstModule.createRuntime(options);
    const second = secondModule.createRuntime(options);

    expect(first.start()).toBe(true);
    expect(second.start()).toBe(false);
    expect(first.generation).not.toBe(second.generation);
  });

  it('refuses a conflicting terminal namespace before constructing an installing generation', () => {
    const target: { que: unknown[]; version?: string } = { que: [] };
    Object.defineProperty(target, 'version', {
      configurable: false,
      enumerable: true,
      value: 'publisher',
      writable: false,
    });
    const queueDescriptor = Object.getOwnPropertyDescriptor(target, 'que');
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: manifest([]),
      knownIntegrationIds: Object.freeze([] as string[]),
      boot: boot(),
      kernel: {
        addAdUnits: () => ({ registered: [] }),
        diagnostics: Object.freeze({}),
        requestAds: async () => ({ slots: [] }),
      },
    });

    expect(runtime.start()).toBe(false);
    expect(runtime.state).toBe('unclaimed');
    expect(Object.getOwnPropertyDescriptor(target, 'que')).toEqual(queueDescriptor);
    expect(Reflect.ownKeys(target)).toEqual(['que', 'version']);
  });

  it.each([
    ['wrong release', { id: 'gpt', release: 'b'.repeat(64), prepare: vi.fn() }],
    ['unknown id', { id: 'aps', release: RELEASE, prepare: vi.fn() }],
  ])(
    'classifies %s registration as abi_mismatch without invoking module code',
    async (_name, registration) => {
      const runtime = createRuntime({
        target: {},
        releaseId: RELEASE,
        manifest: manifest(['gpt']),
        knownIntegrationIds: Object.freeze(['gpt']),
        boot: boot(),
        kernel: {
          addAdUnits: vi.fn(),
          diagnostics: Object.freeze({}),
          requestAds: vi.fn(),
        },
      });
      runtime.start();

      expect(runtime.registerIntegration(registration)).toBe(false);
      await expect(runtime.install()).resolves.toEqual({
        state: 'fallback',
        reason: 'abi_mismatch',
      });
      expect(registration.prepare).not.toHaveBeenCalled();
    }
  );

  it('classifies duplicate registration as abi_mismatch', async () => {
    const prepare = vi.fn(() => ({ activate: vi.fn() }));
    const registration = { id: 'gpt', release: RELEASE, prepare };
    const runtime = createRuntime({
      target: {},
      releaseId: RELEASE,
      manifest: manifest(['gpt']),
      knownIntegrationIds: Object.freeze(['gpt']),
      boot: boot(),
      kernel: {
        addAdUnits: vi.fn(),
        diagnostics: Object.freeze({}),
        requestAds: vi.fn(),
      },
    });
    runtime.start();
    expect(runtime.registerIntegration(registration)).toBe(true);
    expect(runtime.registerIntegration(registration)).toBe(false);

    await expect(runtime.install()).resolves.toEqual({ state: 'fallback', reason: 'abi_mismatch' });
    expect(prepare).not.toHaveBeenCalled();
  });

  it.each(['prepare_throw', 'prepare_reject', 'activate_throw'] as const)(
    'unwinds %s as bundle_partial',
    async (checkpoint) => {
      const disposed = vi.fn();
      const prepare =
        checkpoint === 'prepare_throw'
          ? () => {
              throw new Error('prepare');
            }
          : checkpoint === 'prepare_reject'
            ? async ({ onDispose }: { onDispose(callback: () => void): void }) => {
                onDispose(disposed);
                throw new Error('prepare');
              }
            : ({ onDispose }: { onDispose(callback: () => void): void }) => {
                onDispose(disposed);
                return {
                  activate: () => {
                    throw new Error('activate');
                  },
                };
              };
      const runtime = createRuntime({
        target: {},
        releaseId: RELEASE,
        manifest: manifest(['gpt']),
        knownIntegrationIds: Object.freeze(['gpt']),
        boot: boot(),
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      });
      runtime.start();
      runtime.registerIntegration({ id: 'gpt', release: RELEASE, prepare });

      await expect(runtime.install()).resolves.toEqual({
        state: 'fallback',
        reason: 'bundle_partial',
      });
      if (checkpoint !== 'prepare_throw') expect(disposed).toHaveBeenCalledOnce();
    }
  );

  it('shares the ten-second watchdog with a hung preparation and ignores its late continuation', async () => {
    vi.useFakeTimers();
    let finish: ((value: { activate(): void }) => void) | undefined;
    const lateActivate = vi.fn();
    const target = {};
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: manifest(['gpt']),
      knownIntegrationIds: Object.freeze(['gpt']),
      boot: boot(),
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    runtime.start();
    runtime.registerIntegration({
      id: 'gpt',
      release: RELEASE,
      prepare: () =>
        new Promise<{ activate(): void }>((resolve) => {
          finish = resolve;
        }),
    });
    const installed = runtime.install();

    await vi.advanceTimersByTimeAsync(10_000);
    await expect(installed).resolves.toEqual({ state: 'fallback', reason: 'bundle_partial' });
    finish?.({ activate: lateActivate });
    await Promise.resolve();
    expect(lateActivate).not.toHaveBeenCalled();
    expect(runtime.state).toBe('fallback');
  });

  it('isolates afterCommit failure after kernel publication', async () => {
    const target = {};
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: manifest(['gpt']),
      knownIntegrationIds: Object.freeze(['gpt']),
      boot: boot(),
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    runtime.start();
    runtime.registerIntegration({
      id: 'gpt',
      release: RELEASE,
      prepare: () => ({
        activate: ({ afterCommit }: { afterCommit(callback: () => void): void }) =>
          afterCommit(() => {
            throw new Error('post commit');
          }),
      }),
    });

    await expect(runtime.install()).resolves.toMatchObject({
      state: 'kernel',
      runtimeFailures: [{ id: 'gpt', phase: 'after_commit' }],
    });
    expect(runtime.state).toBe('kernel');
  });

  it('validates fallback calls and settles known, unknown, and aborted slots', async () => {
    const target = {};
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: { version: 2 },
      knownIntegrationIds: Object.freeze([]),
      boot: boot([{ slot: 'known', outcome: 'no_bid' }]),
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    runtime.start();
    await runtime.install();
    const api = target as unknown as {
      addAdUnits(units: unknown): unknown;
      requestAds(options?: unknown): Promise<unknown>;
      boot: unknown;
    };

    await expect(api.requestAds({ slots: ['known', 'unknown'] })).resolves.toEqual({
      slots: [
        { slot: 'known', path: 'primary', outcome: 'failed', reason: 'abi_mismatch' },
        { slot: 'unknown', path: 'primary', outcome: 'failed', reason: 'slot_unresolved' },
      ],
    });
    const controller = new AbortController();
    controller.abort();
    await expect(api.requestAds({ slots: ['known'], signal: controller.signal })).resolves.toEqual({
      slots: [{ slot: 'known', path: 'primary', outcome: 'cancelled', reason: 'caller_aborted' }],
    });
    await expect(api.requestAds({ slots: [] })).rejects.toBeInstanceOf(RequestAdsInputError);
    expect(() => api.addAdUnits({ code: '', mediaTypes: {} })).toThrow(AdUnitRegistrationError);
    expect(() =>
      api.addAdUnits({
        code: 'programmatic',
        mediaTypes: { banner: { sizes: [[300, 250]] } },
      })
    ).toThrow(TsjsUnavailableError);
    expect(Object.isFrozen(api.boot)).toBe(true);
  });

  it('substitutes the exact safe auction projection when boot data is hostile', async () => {
    const getter = vi.fn(() => ({ version: 1 }));
    const hostile = {};
    Object.defineProperty(hostile, 'auctionProjection', { enumerable: true, get: getter });
    const target = {};
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: { version: 2 },
      knownIntegrationIds: Object.freeze([]),
      boot: hostile,
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    runtime.start();
    await runtime.install();

    expect(getter).not.toHaveBeenCalled();
    expect(
      (target as unknown as { boot: { auctionProjection: unknown } }).boot.auctionProjection
    ).toEqual({
      version: 1,
      auction: { version: 1, auctionId: 'fallback', results: [] },
      bids: [],
    });
  });

  it('snapshots fallback boot before publisher mutation during installation', async () => {
    const target = { boot: boot([{ slot: 'initial', outcome: 'no_bid' }]) };
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: { version: 2 },
      knownIntegrationIds: Object.freeze([]),
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    expect(runtime.start()).toBe(true);
    target.boot = boot([{ slot: 'mutated', outcome: 'no_bid' }]);

    await runtime.install();
    const api = target as unknown as {
      boot: { auctionProjection: { auction: { results: readonly { slot: string }[] } } };
      requestAds(options: unknown): Promise<unknown>;
    };
    expect(api.boot.auctionProjection.auction.results).toEqual([
      { slot: 'initial', outcome: 'no_bid' },
    ]);
    await expect(api.requestAds({ slots: ['initial', 'mutated'] })).resolves.toEqual({
      slots: [
        { slot: 'initial', path: 'primary', outcome: 'failed', reason: 'abi_mismatch' },
        { slot: 'mutated', path: 'primary', outcome: 'failed', reason: 'slot_unresolved' },
      ],
    });
  });

  it.each(['getPrototypeOf', 'ownKeys', 'getOwnPropertyDescriptor'] as const)(
    'maps a hostile request options %s trap to invalid_options',
    async (trap) => {
      const target = {};
      const runtime = createRuntime({
        target,
        releaseId: RELEASE,
        manifest: { version: 2 },
        knownIntegrationIds: Object.freeze([]),
        boot: boot(),
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      });
      runtime.start();
      await runtime.install();
      const requestAds = (target as unknown as { requestAds(value: unknown): Promise<unknown> })
        .requestAds;
      const optionsTarget = trap === 'getOwnPropertyDescriptor' ? { slots: ['known'] } : {};

      await expect(requestAds(hostileRecord(trap, optionsTarget))).rejects.toMatchObject({
        code: 'invalid_options',
      });
    }
  );

  it.each(['getPrototypeOf', 'ownKeys', 'getOwnPropertyDescriptor'] as const)(
    'maps a hostile addAdUnits unit %s trap to invalid_unit',
    async (trap) => {
      const target = {};
      const runtime = createRuntime({
        target,
        releaseId: RELEASE,
        manifest: { version: 2 },
        knownIntegrationIds: Object.freeze([]),
        boot: boot(),
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      });
      runtime.start();
      await runtime.install();
      const addAdUnits = (target as unknown as { addAdUnits(value: unknown): unknown }).addAdUnits;
      const unit = hostileRecord(trap, {
        code: 'hostile',
        mediaTypes: { banner: { sizes: [[300, 250]] } },
      });

      expect(() => addAdUnits(unit)).toThrow(
        expect.objectContaining({ code: 'invalid_unit', unitIndex: 0 })
      );
    }
  );

  it('maps hostile outer addAdUnits Array reflection to invalid_units', async () => {
    const target = {};
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: { version: 2 },
      knownIntegrationIds: Object.freeze([]),
      boot: boot(),
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    runtime.start();
    await runtime.install();
    const addAdUnits = (target as unknown as { addAdUnits(value: unknown): unknown }).addAdUnits;
    const units = new Proxy([], {
      ownKeys() {
        throw new Error('hostile outer Array');
      },
    });

    const error = thrownBy(() => addAdUnits(units));
    expect(error).toMatchObject({ code: 'invalid_units' });
    expect(Object.prototype.hasOwnProperty.call(error, 'unitIndex')).toBe(false);
  });

  it('maps a revoked outer addAdUnits Array proxy to invalid_units', async () => {
    const target = {};
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: { version: 2 },
      knownIntegrationIds: Object.freeze([]),
      boot: boot(),
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    runtime.start();
    await runtime.install();
    const { proxy, revoke } = Proxy.revocable([], {});
    revoke();

    const error = thrownBy(() =>
      (target as unknown as { addAdUnits(value: unknown): unknown }).addAdUnits(proxy)
    );
    expect(error).toMatchObject({ code: 'invalid_units' });
    expect(Object.prototype.hasOwnProperty.call(error, 'unitIndex')).toBe(false);
  });

  it.each(['getPrototypeOf', 'ownKeys', 'getOwnPropertyDescriptor'] as const)(
    'substitutes exact safe boot for a hostile boot %s trap',
    async (trap) => {
      const target = {};
      const runtime = createRuntime({
        target,
        releaseId: RELEASE,
        manifest: { version: 2 },
        knownIntegrationIds: Object.freeze([]),
        boot: hostileRecord(trap, trap === 'getOwnPropertyDescriptor' ? boot() : {}),
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      });
      expect(runtime.start()).toBe(true);

      await expect(runtime.install()).resolves.toEqual({
        state: 'fallback',
        reason: 'abi_mismatch',
      });
      expect(
        (target as unknown as { boot: { auctionProjection: unknown } }).boot.auctionProjection
      ).toEqual({
        version: 1,
        auction: { version: 1, auctionId: 'fallback', results: [] },
        bids: [],
      });
    }
  );

  it('substitutes exact safe boot when nested boot contract proxies throw', async () => {
    const target = {};
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: { version: 2 },
      knownIntegrationIds: Object.freeze([]),
      boot: {
        cachePolicy: hostileRecord('ownKeys'),
        auctionProjection: hostileRecord('getPrototypeOf'),
      },
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    expect(runtime.start()).toBe(true);

    await expect(runtime.install()).resolves.toEqual({
      state: 'fallback',
      reason: 'abi_mismatch',
    });
    expect(
      (target as unknown as { boot: { auctionProjection: unknown } }).boot.auctionProjection
    ).toEqual({
      version: 1,
      auction: { version: 1, auctionId: 'fallback', results: [] },
      bids: [],
    });
  });

  it('rejects a full boot whose server manifest disagrees with the accepted bundle manifest', async () => {
    const target = {};
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: manifest([]),
      knownIntegrationIds: Object.freeze([]),
      boot: { abi: 1, releaseId: RELEASE, manifest: manifest(['gpt']), ...boot() },
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    runtime.start();

    await expect(runtime.install()).resolves.toEqual({ state: 'fallback', reason: 'abi_mismatch' });
  });

  it('binds validation and fallback publication to the embedded bundle release', async () => {
    const serverRelease = 'b'.repeat(64);
    const serverManifest = { version: 1, releaseId: serverRelease, integrations: [] };
    const target = {};
    const runtime = createRuntime({
      target,
      releaseId: serverRelease,
      manifest: serverManifest,
      knownIntegrationIds: Object.freeze([]),
      boot: {
        abi: 1,
        releaseId: serverRelease,
        manifest: serverManifest,
        ...boot(),
      },
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    runtime.start();

    await expect(runtime.install()).resolves.toEqual({ state: 'fallback', reason: 'abi_mismatch' });
    expect(target).toMatchObject({
      releaseId: RELEASE,
      boot: { releaseId: RELEASE, manifest: { releaseId: RELEASE } },
      _internal: { state: 'fallback', releaseId: RELEASE, reason: 'abi_mismatch' },
    });
  });

  it('does not invoke hostile Array iterators at fallback input boundaries', async () => {
    const target = {};
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: { version: 2 },
      knownIntegrationIds: Object.freeze([]),
      boot: boot([{ slot: 'known', outcome: 'no_bid' }]),
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    runtime.start();
    await runtime.install();
    const iterator = vi.fn();
    const slots = ['known'];
    Object.defineProperty(slots, Symbol.iterator, { value: iterator });

    await expect(
      (target as unknown as { requestAds(value: unknown): Promise<unknown> }).requestAds({ slots })
    ).rejects.toMatchObject({ code: 'invalid_slots' });
    expect(iterator).not.toHaveBeenCalled();
  });

  it('uses exact addAdUnits dimension and bidder validation before refusing valid input', async () => {
    const target = {};
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: { version: 2 },
      knownIntegrationIds: Object.freeze([]),
      boot: boot(),
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    runtime.start();
    await runtime.install();
    const addAdUnits = (target as unknown as { addAdUnits(value: unknown): unknown }).addAdUnits;

    expect(() =>
      addAdUnits({ code: 'zero', mediaTypes: { banner: { sizes: [[0, 250]] } } })
    ).toThrow(expect.objectContaining({ code: 'invalid_dimensions' }));
    expect(() =>
      addAdUnits({ code: 'large', mediaTypes: { banner: { sizes: [[4097, 250]] } } })
    ).toThrow(expect.objectContaining({ code: 'dimensions_out_of_range' }));
    expect(() =>
      addAdUnits({
        code: 'bad-bidder',
        mediaTypes: { banner: { sizes: [[1, 1]] } },
        bids: [{ bidder: 'x'.repeat(65) }],
      })
    ).toThrow(expect.objectContaining({ code: 'invalid_bidder' }));
    expect(() =>
      addAdUnits({
        code: 'valid',
        mediaTypes: { banner: { sizes: [[1, 4096]] } },
        bids: [{ bidder: 'aps', params: { placement: 'one' } }],
      })
    ).toThrow(TsjsUnavailableError);
  });

  it.each([
    ['high', '\ud800'],
    ['low', '\udc00'],
  ] as const)(
    'rejects a lone %s UTF-16 surrogate in a programmatic slot code',
    async (_kind, code) => {
      const target = {};
      const runtime = createRuntime({
        target,
        releaseId: RELEASE,
        manifest: { version: 2 },
        knownIntegrationIds: Object.freeze([]),
        boot: boot(),
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      });
      runtime.start();
      await runtime.install();
      const addAdUnits = (target as unknown as { addAdUnits(value: unknown): unknown }).addAdUnits;

      expect(() => addAdUnits({ code, mediaTypes: { banner: { sizes: [[300, 250]] } } })).toThrow(
        expect.objectContaining({ code: 'invalid_code', unitIndex: 0 })
      );
    }
  );

  it('applies fallback slot collision and combined registry capacity validation', async () => {
    const makeFallback = async (slots: readonly string[]) => {
      const target = {};
      const runtime = createRuntime({
        target,
        releaseId: RELEASE,
        manifest: { version: 2 },
        knownIntegrationIds: Object.freeze([]),
        boot: boot(slots.map((slot) => ({ slot, outcome: 'no_bid' }))),
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      });
      runtime.start();
      await runtime.install();
      return (target as unknown as { addAdUnits(value: unknown): unknown }).addAdUnits;
    };
    const collision = await makeFallback(['server']);
    expect(() =>
      collision({ code: 'server', mediaTypes: { banner: { sizes: [[300, 250]] } } })
    ).toThrow(expect.objectContaining({ code: 'slot_collision', unitIndex: 0 }));

    const full = await makeFallback(Array.from({ length: 256 }, (_, index) => `slot-${index}`));
    const capacityError = thrownBy(() =>
      full({ code: 'overflow', mediaTypes: { banner: { sizes: [[300, 250]] } } })
    );
    expect(capacityError).toMatchObject({ code: 'registry_capacity' });
    expect(Object.prototype.hasOwnProperty.call(capacityError, 'unitIndex')).toBe(false);
  });

  it('reports aggregate request overflow before combined registry capacity', async () => {
    const target = {};
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: { version: 2 },
      knownIntegrationIds: Object.freeze([]),
      boot: boot(
        Array.from({ length: 256 }, (_, index) => ({
          slot: `server-${index}`,
          outcome: 'no_bid',
        }))
      ),
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    runtime.start();
    await runtime.install();
    const addAdUnits = (target as unknown as { addAdUnits(value: unknown): unknown }).addAdUnits;

    expect(() =>
      addAdUnits({
        code: 'programmatic-overflow',
        mediaTypes: { banner: { sizes: [[300, 250]] } },
        bids: [{ bidder: 'aps', params: { payload: 'x'.repeat(256 * 1024) } }],
      })
    ).toThrow(expect.objectContaining({ code: 'request_body_too_large' }));
  });

  it('accepts contract-valid large collections and deep params before refusing availability', async () => {
    const target = {};
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: { version: 2 },
      knownIntegrationIds: Object.freeze([]),
      boot: boot(),
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    runtime.start();
    await runtime.install();
    const addAdUnits = (target as unknown as { addAdUnits(value: unknown): unknown }).addAdUnits;
    const sizes = Array.from({ length: 257 }, () => [1, 1]);
    const bids = Array.from({ length: 257 }, () => ({ bidder: 'aps' }));
    const paramsArray = Array.from({ length: 4097 }, () => 0);
    let deepParams: object = { leaf: true };
    for (let depth = 0; depth < 128; depth += 1) deepParams = { child: deepParams };

    for (const unit of [
      { code: 'many-sizes', mediaTypes: { banner: { sizes } } },
      { code: 'many-bids', mediaTypes: { banner: { sizes: [[1, 1]] } }, bids },
      {
        code: 'large-params-array',
        mediaTypes: { banner: { sizes: [[1, 1]] } },
        bids: [{ bidder: 'aps', params: { values: paramsArray } }],
      },
      {
        code: 'deep-params',
        mediaTypes: { banner: { sizes: [[1, 1]] } },
        bids: [{ bidder: 'aps', params: deepParams }],
      },
    ]) {
      expect(() => addAdUnits(unit)).toThrow(TsjsUnavailableError);
    }
  });

  it('classifies an empty banner size list as invalid_media_types', async () => {
    const target = {};
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: { version: 2 },
      knownIntegrationIds: Object.freeze([]),
      boot: boot(),
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    runtime.start();
    await runtime.install();

    expect(() =>
      (target as unknown as { addAdUnits(value: unknown): unknown }).addAdUnits({
        code: 'empty-sizes',
        mediaTypes: { banner: { sizes: [] } },
      })
    ).toThrow(expect.objectContaining({ code: 'invalid_media_types', unitIndex: 0 }));
  });

  it('bounds an exponentially expanded shared params DAG without revisiting nodes', async () => {
    const target = {};
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: { version: 2 },
      knownIntegrationIds: Object.freeze([]),
      boot: boot(),
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    runtime.start();
    await runtime.install();
    const addAdUnits = (target as unknown as { addAdUnits(value: unknown): unknown }).addAdUnits;
    const descriptorReads: number[] = [];
    let shared: object = { value: 'leaf' };
    for (let depth = 0; depth < 24; depth += 1) {
      const node = { left: shared, right: shared };
      const nodeIndex = descriptorReads.length;
      descriptorReads.push(0);
      shared = new Proxy(node, {
        getOwnPropertyDescriptor(object, key) {
          descriptorReads[nodeIndex] = (descriptorReads[nodeIndex] ?? 0) + 1;
          if ((descriptorReads[nodeIndex] ?? 0) > Reflect.ownKeys(object).length) {
            throw new Error('shared DAG node was expanded more than once');
          }
          return Reflect.getOwnPropertyDescriptor(object, key);
        },
      });
    }

    expect(() =>
      addAdUnits({
        code: 'shared-dag',
        mediaTypes: { banner: { sizes: [[300, 250]] } },
        bids: [{ bidder: 'aps', params: shared }],
      })
    ).toThrow(expect.objectContaining({ code: 'request_body_too_large' }));
    expect(descriptorReads.every((reads) => reads <= 2)).toBe(true);
  });

  it('measures addAdUnits input without invoking inherited toJSON hooks', async () => {
    const target = {};
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: { version: 2 },
      knownIntegrationIds: Object.freeze([]),
      boot: boot(),
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    runtime.start();
    await runtime.install();
    const hook = vi.fn(() => {
      throw new Error('publisher toJSON');
    });
    Object.defineProperty(Object.prototype, 'toJSON', { configurable: true, value: hook });
    Object.defineProperty(Array.prototype, 'toJSON', { configurable: true, value: hook });
    try {
      expect(() =>
        (target as unknown as { addAdUnits(value: unknown): unknown }).addAdUnits({
          code: 'valid',
          mediaTypes: { banner: { sizes: [[300, 250]] } },
          bids: [{ bidder: 'aps', params: { placement: 'one' } }],
        })
      ).toThrow(TsjsUnavailableError);
      expect(hook).not.toHaveBeenCalled();
    } finally {
      Reflect.deleteProperty(Object.prototype, 'toJSON');
      Reflect.deleteProperty(Array.prototype, 'toJSON');
    }
  });

  it('publishes an immutable exact logger facade', async () => {
    const target = {};
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: { version: 2 },
      knownIntegrationIds: Object.freeze([]),
      boot: boot(),
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    runtime.start();
    await runtime.install();
    const publicLog = (target as unknown as { log: object }).log;

    expect(Object.isFrozen(publicLog)).toBe(true);
    expect(Object.keys(publicLog)).toEqual([
      'setLevel',
      'getLevel',
      'error',
      'warn',
      'info',
      'debug',
    ]);
    expect(Reflect.set(publicLog, 'warn', vi.fn())).toBe(false);
  });

  it('returns false when queue descriptor reflection becomes hostile after preflight', () => {
    let queueDescriptorReads = 0;
    const backing = {};
    const target = new Proxy(backing, {
      getOwnPropertyDescriptor(object, key) {
        if (key === 'que') {
          queueDescriptorReads += 1;
          if (queueDescriptorReads === 2) throw new Error('hostile second queue reflection');
        }
        return Reflect.getOwnPropertyDescriptor(object, key);
      },
    });
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: manifest([]),
      knownIntegrationIds: Object.freeze([]),
      boot: boot(),
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    let started: boolean | undefined;

    expect(() => {
      started = runtime.start();
    }).not.toThrow();
    expect(started).toBe(false);
    expect(runtime.state).toBe('unclaimed');
    expect(queueDescriptorReads).toBe(2);
    expect(Object.prototype.hasOwnProperty.call(backing, '_registerIntegration')).toBe(false);
  });

  it('returns false when a claim mutation and its rollback restoration both throw', () => {
    const ingress: unknown[] = [];
    const backing = { que: ingress, boot: boot() };
    let queueDefinitionCalls = 0;
    const target = new Proxy(backing, {
      defineProperty(object, key, descriptor) {
        if (key === 'que') {
          queueDefinitionCalls += 1;
          if (queueDefinitionCalls === 1) {
            Reflect.defineProperty(object, key, descriptor);
            throw new Error('hostile claim definition');
          }
          if (queueDefinitionCalls === 2) throw new Error('hostile rollback definition');
        }
        return Reflect.defineProperty(object, key, descriptor);
      },
    });
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: manifest([]),
      knownIntegrationIds: Object.freeze([]),
      boot: backing.boot,
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    let started: boolean | undefined;

    expect(() => {
      started = runtime.start();
    }).not.toThrow();
    expect(started).toBe(false);
    expect(runtime.state).toBe('unclaimed');
    expect(queueDefinitionCalls).toBe(2);
    expect(Object.prototype.hasOwnProperty.call(backing, '_registerIntegration')).toBe(false);
    expect(Object.prototype.hasOwnProperty.call(backing, 'version')).toBe(false);
    expect(Object.getOwnPropertyDescriptor(backing, 'que')).toMatchObject({
      configurable: true,
      enumerable: true,
      value: ingress,
      writable: false,
    });
  });

  it('rolls back a failed start claim without leaving a partial owner', () => {
    let fail = true;
    const backing = {};
    const target = new Proxy(backing, {
      defineProperty(object, key, descriptor) {
        if (fail) {
          fail = false;
          throw new Error('transient define failure');
        }
        return Reflect.defineProperty(object, key, descriptor);
      },
    });
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: manifest([]),
      knownIntegrationIds: Object.freeze([]),
      boot: boot(),
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });

    expect(runtime.start()).toBe(false);
    expect(runtime.state).toBe('unclaimed');
    expect(Object.prototype.hasOwnProperty.call(target, '_registerIntegration')).toBe(false);
    expect(
      createRuntime({
        target,
        releaseId: RELEASE,
        manifest: manifest([]),
        knownIntegrationIds: Object.freeze([]),
        boot: boot(),
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      }).start()
    ).toBe(true);
  });

  it('captures the monotonic start before queue normalization work', async () => {
    let time = 0;
    const backing = {};
    const target = new Proxy(backing, {
      defineProperty(object, key, descriptor) {
        time = 10_000;
        return Reflect.defineProperty(object, key, descriptor);
      },
    });
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: manifest([]),
      knownIntegrationIds: Object.freeze([]),
      boot: boot(),
      now: () => time,
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    expect(runtime.start()).toBe(true);

    await expect(runtime.install()).resolves.toEqual({
      state: 'fallback',
      reason: 'bundle_partial',
    });
  });
});
