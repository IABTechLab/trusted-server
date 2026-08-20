import { afterEach, describe, expect, it, vi } from 'vitest';

import {
  createBrowserGoogletagAdapter,
  createGoogletagTraceCycleHandle,
  type GoogletagDiagnosticsFact,
} from '../../src/adapters/googletag';

type Command = () => void;

function createReadyGoogletag(
  options: { readonly deferCommands?: boolean; readonly initialLoadDisabled?: boolean } = {}
) {
  const commands: Command[] = [];
  const display = vi.fn();
  const initialLoad = { disabled: options.initialLoadDisabled === true };
  const listeners = new Map<string, Set<(event: unknown) => void>>();
  const pubads = {
    addEventListener: vi.fn((type: string, listener: (event: unknown) => void) => {
      const registered = listeners.get(type) ?? new Set();
      registered.add(listener);
      listeners.set(type, registered);
    }),
    disableInitialLoad: vi.fn(() => {
      initialLoad.disabled = true;
      return 'legacy-result';
    }),
    getSlots: vi.fn<() => object[]>(() => []),
    refresh: vi.fn(),
    removeEventListener: vi.fn((type: string, listener: (event: unknown) => void) => {
      listeners.get(type)?.delete(listener);
    }),
  };
  const googletag = {
    apiReady: true,
    pubadsReady: true,
    cmd: {
      push: vi.fn((command: Command): number => {
        if (options.deferCommands) commands.push(command);
        else command();
        return commands.length;
      }),
    },
    defineSlot: vi.fn(),
    destroySlots: vi.fn(),
    display,
    getConfig: vi.fn((key: string) =>
      key === 'disableInitialLoad' ? { disableInitialLoad: initialLoad.disabled } : {}
    ),
    pubads: vi.fn(() => pubads),
    setConfig: vi.fn((config: { readonly disableInitialLoad?: boolean | null }) => {
      if (Object.prototype.hasOwnProperty.call(config, 'disableInitialLoad')) {
        initialLoad.disabled = config.disableInitialLoad === true;
      }
      return 'config-result';
    }),
  };
  return { commands, display, googletag, initialLoad, listeners, pubads };
}

describe('browser googletag adapter readiness', () => {
  afterEach(() => vi.useRealTimers());

  it('enqueues GAM attribution once before later publisher commands', () => {
    const ready = createReadyGoogletag({ deferCommands: true });
    const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });
    const order: string[] = [];
    ready.googletag.setConfig.mockImplementation((config) => {
      order.push('trusted-server');
      return JSON.stringify(config);
    });

    expect(adapter.enqueueGamAttribution()).toBe(true);
    expect(adapter.enqueueGamAttribution()).toBe(true);
    ready.googletag.cmd.push(() => order.push('publisher'));

    expect(ready.commands).toHaveLength(2);
    ready.commands.splice(0).forEach((command) => command());
    expect(order).toEqual(['trusted-server', 'publisher']);
    expect(ready.googletag.setConfig).toHaveBeenCalledExactlyOnceWith({
      targeting: { ts: 'true' },
    });
  });

  it('creates an absent GPT queue and isolates a missing or throwing targeting API', () => {
    const target: { googletag?: unknown } = {};
    const adapter = createBrowserGoogletagAdapter(target);

    expect(adapter.enqueueGamAttribution()).toBe(true);
    const created = target.googletag as { cmd: Array<() => void> };
    expect(created.cmd).toHaveLength(1);
    expect(() => created.cmd[0]?.()).not.toThrow();

    const publisher = vi.fn();
    const throwing = createReadyGoogletag();
    throwing.googletag.setConfig.mockImplementation(() => {
      throw new Error('targeting unavailable');
    });
    const throwingAdapter = createBrowserGoogletagAdapter({ googletag: throwing.googletag });
    expect(throwingAdapter.enqueueGamAttribution()).toBe(true);
    expect(() => throwing.googletag.cmd.push(publisher)).not.toThrow();
    expect(publisher).toHaveBeenCalledOnce();
  });

  it('reports present and gives the command only a frozen narrow facade', async () => {
    const ready = createReadyGoogletag();
    const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });
    const operation = adapter.run((gpt) => {
      expect(Object.isFrozen(gpt)).toBe(true);
      expect('cmd' in gpt).toBe(false);
      expect('apiReady' in gpt).toBe(false);
      gpt.display('slot-a');
      return 'completed';
    });

    expect(operation.status).toBe('present');
    await expect(operation.result).resolves.toBe('completed');
    expect(ready.display).toHaveBeenCalledWith('slot-a');
  });

  it('defines and adopts one GPT slot as a synchronous rollback-capable transaction', async () => {
    const ready = createReadyGoogletag();
    const slot = { addService: vi.fn() };
    ready.googletag.defineSlot.mockReturnValue(slot);
    ready.googletag.destroySlots.mockReturnValue(true);
    const commit = vi.fn(() => true);
    const rollback = vi.fn();
    const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });

    const operation = adapter.run((gpt) =>
      gpt.transactionalDefine(
        {
          adUnitPath: '/123/slot-a',
          elementId: 'slot-a',
          sizes: [[300, 250]],
        },
        () => true,
        (candidate) => {
          expect(candidate).toBe(slot);
          return Object.freeze({ commit, rollback });
        }
      )
    );

    await expect(operation.result).resolves.toEqual({ status: 'defined', slot });
    expect(ready.googletag.defineSlot).toHaveBeenCalledExactlyOnceWith(
      '/123/slot-a',
      [[300, 250]],
      'slot-a'
    );
    expect(slot.addService).toHaveBeenCalledExactlyOnceWith(ready.pubads);
    expect(commit).toHaveBeenCalledOnce();
    expect(rollback).not.toHaveBeenCalled();
    expect(ready.googletag.destroySlots).not.toHaveBeenCalled();
    expect(slot.addService.mock.invocationCallOrder[0]).toBeLessThan(
      commit.mock.invocationCallOrder[0]!
    );
  });

  it('destroys a newly defined GPT slot when its navigation becomes stale before adoption', async () => {
    const ready = createReadyGoogletag();
    const slot = { addService: vi.fn() };
    ready.googletag.defineSlot.mockReturnValue(slot);
    ready.googletag.destroySlots.mockReturnValue(true);
    const isGenerationCurrent = vi.fn().mockReturnValueOnce(true).mockReturnValue(false);
    const prepareCommit = vi.fn();
    const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });

    const operation = adapter.run((gpt) =>
      gpt.transactionalDefine(
        {
          adUnitPath: '/123/slot-a',
          elementId: 'slot-a',
          sizes: [[300, 250]],
        },
        isGenerationCurrent,
        prepareCommit
      )
    );

    await expect(operation.result).resolves.toEqual({ status: 'discarded' });
    expect(prepareCommit).not.toHaveBeenCalled();
    expect(slot.addService).not.toHaveBeenCalled();
    expect(ready.googletag.destroySlots).toHaveBeenCalledExactlyOnceWith([slot]);
  });

  it('marks and measures only the first TS-authoritative display', async () => {
    const ready = createReadyGoogletag();
    const performance = {
      mark: vi.fn(),
      measure: vi.fn(),
    };
    const target = { googletag: ready.googletag, performance };
    const adapter = createBrowserGoogletagAdapter(target);

    ready.googletag.display('publisher-slot');
    expect(performance.mark).not.toHaveBeenCalled();

    const first = adapter.run((gpt) => {
      gpt.display('trusted-slot-one');
      gpt.display('trusted-slot-two');
    });
    await expect(first.result).resolves.toBeUndefined();
    const replay = adapter.run((gpt) => gpt.display('trusted-slot-three'));
    await expect(replay.result).resolves.toBeUndefined();

    expect(performance.mark).toHaveBeenCalledExactlyOnceWith('tsjs:first-display');
    expect(performance.measure).toHaveBeenCalledExactlyOnceWith(
      'tsjs:boot-to-first-display',
      'tsjs:bids-script',
      'tsjs:first-display'
    );
    expect(performance.mark.mock.invocationCallOrder[0]).toBeLessThan(
      ready.display.mock.invocationCallOrder[1]!
    );
  });

  it('contains missing and throwing Performance APIs at the first display boundary', async () => {
    const missing = createReadyGoogletag();
    const missingAdapter = createBrowserGoogletagAdapter({ googletag: missing.googletag });
    await expect(missingAdapter.run((gpt) => gpt.display('slot-missing')).result).resolves.toBe(
      undefined
    );
    expect(missing.display).toHaveBeenCalledExactlyOnceWith('slot-missing');

    const throwing = createReadyGoogletag();
    const mark = vi.fn(() => {
      throw new Error('performance unavailable');
    });
    const measure = vi.fn();
    const target = { googletag: throwing.googletag, performance: { mark, measure } };
    const throwingAdapter = createBrowserGoogletagAdapter(target);
    await expect(
      throwingAdapter.run((gpt) => {
        gpt.display('slot-throwing');
        gpt.display('slot-replay');
      }).result
    ).resolves.toBeUndefined();

    expect(mark).toHaveBeenCalledExactlyOnceWith('tsjs:first-display');
    expect(measure).not.toHaveBeenCalled();
    expect(throwing.display).toHaveBeenCalledTimes(2);

    const markOnly = createReadyGoogletag();
    const markWithoutMeasure = vi.fn();
    const markOnlyAdapter = createBrowserGoogletagAdapter({
      googletag: markOnly.googletag,
      performance: { mark: markWithoutMeasure },
    });
    await expect(markOnlyAdapter.run((gpt) => gpt.display('slot-mark-only')).result).resolves.toBe(
      undefined
    );
    expect(markWithoutMeasure).toHaveBeenCalledExactlyOnceWith('tsjs:first-display');
    expect(markOnly.display).toHaveBeenCalledExactlyOnceWith('slot-mark-only');
  });

  it('does not mark a display rejected from a stale GPT generation', async () => {
    const first = createReadyGoogletag();
    const replacement = createReadyGoogletag();
    const performance = { mark: vi.fn(), measure: vi.fn() };
    const target = { googletag: first.googletag, performance };
    const adapter = createBrowserGoogletagAdapter(target);
    const operation = adapter.run((gpt) => {
      target.googletag = replacement.googletag;
      gpt.display('stale-slot');
    });

    await expect(operation.result).rejects.toMatchObject({
      code: 'external_artifact_incompatible',
    });
    expect(performance.mark).not.toHaveBeenCalled();
    expect(performance.measure).not.toHaveBeenCalled();
    expect(first.display).not.toHaveBeenCalled();
    expect(replacement.display).not.toHaveBeenCalled();

    await expect(adapter.run((gpt) => gpt.display('current-slot')).result).resolves.toBeUndefined();
    expect(performance.mark).toHaveBeenCalledExactlyOnceWith('tsjs:first-display');
    expect(replacement.display).toHaveBeenCalledExactlyOnceWith('current-slot');
  });

  it('reports pending and drains live operations FIFO through a real GPT command notification', async () => {
    const readinessCommands: Command[] = [];
    const target: { googletag?: unknown } = {
      googletag: { cmd: readinessCommands },
    };
    const adapter = createBrowserGoogletagAdapter(target);
    const order: number[] = [];
    const first = adapter.run(() => order.push(1));
    const second = adapter.run(() => order.push(2));

    expect(first.status).toBe('pending');
    expect(second.status).toBe('pending');
    expect(readinessCommands).toHaveLength(1);

    target.googletag = createReadyGoogletag().googletag;
    readinessCommands[0]?.();

    await expect(first.result).resolves.toBe(1);
    await expect(second.result).resolves.toBe(2);
    expect(order).toEqual([1, 2]);
    expect(first.status).toBe('present');
    expect(second.status).toBe('present');
  });

  it.each(['before', 'after'] as const)(
    'recovers GPT notification arming when WeakSet.add throws %s insertion',
    async (failure) => {
      const readinessCommands: Command[] = [];
      const target: { googletag?: unknown } = { googletag: { cmd: readinessCommands } };
      const adapter = createBrowserGoogletagAdapter(target);
      const originalWeakSetAdd = WeakSet.prototype.add;
      WeakSet.prototype.add = function (this: WeakSet<object>, value: object): WeakSet<object> {
        if (failure === 'after') Reflect.apply(originalWeakSetAdd, this, [value]);
        throw new Error(`GPT arming failed ${failure} insertion`);
      } as typeof WeakSet.prototype.add;

      let operation: ReturnType<typeof adapter.run> | undefined;
      let thrown: unknown;
      try {
        operation = adapter.run(() => 'ready');
      } catch (error) {
        thrown = error;
      } finally {
        WeakSet.prototype.add = originalWeakSetAdd;
      }

      expect(thrown).toBeUndefined();
      if (!operation) throw new Error('Expected a published GPT operation');
      expect(readinessCommands).toHaveLength(0);
      adapter.notifyReady();
      expect(readinessCommands).toHaveLength(1);
      target.googletag = createReadyGoogletag().googletag;
      readinessCommands[0]?.();
      await expect(operation.result).resolves.toBe('ready');
      adapter.dispose();
    }
  );

  it('recovers GPT notification arming when WeakSet.has throws after publication', async () => {
    vi.useFakeTimers();
    const readinessCommands: Command[] = [];
    const target: { googletag?: unknown } = { googletag: { cmd: readinessCommands } };
    const adapter = createBrowserGoogletagAdapter(target);
    const order: number[] = [];
    const originalWeakSetHas = WeakSet.prototype.has;
    WeakSet.prototype.has = function (): boolean {
      throw new Error('GPT armed lookup failed');
    } as typeof WeakSet.prototype.has;

    let first: ReturnType<typeof adapter.run> | undefined;
    let thrown: unknown;
    try {
      first = adapter.run(() => order.push(1));
    } catch (error) {
      thrown = error;
    } finally {
      WeakSet.prototype.has = originalWeakSetHas;
    }

    expect(thrown).toBeUndefined();
    if (!first) throw new Error('Expected a published GPT operation');
    expect(readinessCommands).toHaveLength(1);
    const second = adapter.run(() => order.push(2));
    expect(readinessCommands).toHaveLength(1);
    target.googletag = createReadyGoogletag().googletag;
    readinessCommands[0]?.();
    await expect(Promise.all([first.result, second.result])).resolves.toEqual([1, 2]);
    expect(order).toEqual([1, 2]);
    expect(vi.getTimerCount()).toBe(0);

    target.googletag = undefined;
    const recovered = Array.from({ length: 64 }, () => adapter.run(vi.fn()));
    const recoveredResults = recovered.map(({ result }) => result.catch((error) => error));
    expect(() => adapter.run(vi.fn())).toThrowError(
      expect.objectContaining({ code: 'external_queue_full' })
    );
    for (const operation of recovered) operation.dispose();
    await Promise.all(recoveredResults);
    expect(vi.getTimerCount()).toBe(0);
    adapter.dispose();
  });

  it.each([
    { pushFailure: 'before', deleteFailure: 'throw' },
    { pushFailure: 'after', deleteFailure: 'retain' },
  ] as const)(
    'retries GPT notification registration after $pushFailure enqueue failure and $deleteFailure rollback',
    async ({ pushFailure, deleteFailure }) => {
      vi.useFakeTimers();
      const readinessCommands: Command[] = [];
      let queueBroken = true;
      const push = vi.fn((command: Command): number => {
        if (queueBroken) {
          if (pushFailure === 'after') readinessCommands.push(command);
          throw new Error(`GPT queue failed ${pushFailure} enqueue`);
        }
        readinessCommands.push(command);
        return readinessCommands.length;
      });
      const target: { googletag?: unknown } = { googletag: { cmd: { push } } };
      const adapter = createBrowserGoogletagAdapter(target);
      const order: number[] = [];
      const originalWeakSetDelete = WeakSet.prototype.delete;
      WeakSet.prototype.delete = function (): boolean {
        if (deleteFailure === 'throw') throw new Error('GPT arming rollback failed');
        return false;
      } as typeof WeakSet.prototype.delete;

      let first: ReturnType<typeof adapter.run> | undefined;
      let thrown: unknown;
      try {
        first = adapter.run(() => order.push(1));
      } catch (error) {
        thrown = error;
      } finally {
        WeakSet.prototype.delete = originalWeakSetDelete;
      }

      expect(thrown).toBeUndefined();
      if (!first) throw new Error('Expected a published GPT operation');
      expect(readinessCommands).toHaveLength(pushFailure === 'after' ? 1 : 0);
      queueBroken = false;
      const second = adapter.run(() => order.push(2));
      expect(readinessCommands).toHaveLength(pushFailure === 'after' ? 2 : 1);

      target.googletag = createReadyGoogletag().googletag;
      if (pushFailure === 'after') {
        readinessCommands[0]?.();
        expect(order).toEqual([]);
      }
      readinessCommands[readinessCommands.length - 1]?.();
      await expect(Promise.all([first.result, second.result])).resolves.toEqual([1, 2]);
      expect(order).toEqual([1, 2]);
      for (const notify of readinessCommands) notify();
      expect(order).toEqual([1, 2]);
      expect(vi.getTimerCount()).toBe(0);

      target.googletag = undefined;
      const recovered = Array.from({ length: 64 }, () => adapter.run(vi.fn()));
      const recoveredResults = recovered.map(({ result }) => result.catch((error) => error));
      expect(() => adapter.run(vi.fn())).toThrowError(
        expect.objectContaining({ code: 'external_queue_full' })
      );
      for (const operation of recovered) operation.dispose();
      await Promise.all(recoveredResults);
      expect(vi.getTimerCount()).toBe(0);
      adapter.dispose();
    }
  );

  it('rejects a queued operation when its pending GPT stub becomes incompatible', async () => {
    const readinessCommands: Command[] = [];
    const ready = createReadyGoogletag();
    const binding: Record<string, unknown> = {
      ...ready.googletag,
      apiReady: false,
      cmd: readinessCommands,
    };
    const adapter = createBrowserGoogletagAdapter({ googletag: binding });
    const command = vi.fn();
    const operation = adapter.run(command);

    binding['apiReady'] = true;
    delete binding['display'];
    readinessCommands[0]?.();

    await expect(operation.result).rejects.toMatchObject({
      code: 'external_artifact_incompatible',
    });
    expect(operation.status).toBe('incompatible');
    expect(command).not.toHaveBeenCalled();
  });

  it('requires the captured GPT queue and root methods to remain compatible', async () => {
    const mutations = [
      (binding: Record<string, unknown>): void => {
        binding['apiReady'] = false;
      },
      (binding: Record<string, unknown>): void => {
        binding['display'] = vi.fn();
      },
      (binding: Record<string, unknown>): void => {
        binding['pubads'] = vi.fn();
      },
      (binding: Record<string, unknown>): void => {
        binding['cmd'] = { push: vi.fn() };
      },
    ];
    for (const mutate of mutations) {
      const ready = createReadyGoogletag({ deferCommands: true });
      const binding = ready.googletag as unknown as Record<string, unknown>;
      const adapter = createBrowserGoogletagAdapter({ googletag: binding });
      const command = vi.fn();
      const operation = adapter.run(command);

      mutate(binding);
      ready.commands[0]?.();

      await expect(operation.result).rejects.toMatchObject({
        code: 'external_artifact_incompatible',
      });
      expect(operation.status).toBe('incompatible');
      expect(command).not.toHaveBeenCalled();
    }
  });

  it('rechecks GPT compatibility after an in-place getter mutation', async () => {
    const ready = createReadyGoogletag({ deferCommands: true });
    const binding = ready.googletag as unknown as Record<string, unknown>;
    const originalDisplay = ready.googletag.display;
    const adapter = createBrowserGoogletagAdapter({ googletag: binding });
    const command = vi.fn();
    const operation = adapter.run(command);
    Object.defineProperty(binding, 'display', {
      configurable: true,
      get: () => {
        binding['apiReady'] = false;
        return originalDisplay;
      },
    });

    ready.commands[0]?.();

    await expect(operation.result).rejects.toMatchObject({
      code: 'external_artifact_incompatible',
    });
    expect(command).not.toHaveBeenCalled();
  });

  it('ignores a stale GPT notification and lets the replacement notification decide', async () => {
    const oldNotifications: Command[] = [];
    const replacementNotifications: Command[] = [];
    const oldBinding = { cmd: oldNotifications };
    const replacement: Record<string, unknown> = { cmd: replacementNotifications };
    const target: { googletag?: unknown } = { googletag: oldBinding };
    const adapter = createBrowserGoogletagAdapter(target);
    const first = adapter.run(() => 'first');
    target.googletag = replacement;
    const second = adapter.run(() => 'second');

    replacement['apiReady'] = true;
    oldNotifications[0]?.();
    expect(first.status).toBe('pending');
    expect(second.status).toBe('pending');

    replacementNotifications[0]?.();
    await expect(first.result).rejects.toMatchObject({
      code: 'external_artifact_incompatible',
    });
    await expect(second.result).rejects.toMatchObject({
      code: 'external_artifact_incompatible',
    });
  });

  it('does not let a stale GPT notification condemn a primitive replacement', async () => {
    const oldNotifications: Command[] = [];
    const target: { googletag?: unknown } = { googletag: { cmd: oldNotifications } };
    const adapter = createBrowserGoogletagAdapter(target);
    const operation = adapter.run(vi.fn());
    const result = operation.result.catch((error: unknown) => error);
    target.googletag = 1;

    oldNotifications[0]?.();
    expect(operation.status).toBe('pending');
    adapter.dispose();
    await expect(result).resolves.toMatchObject({ code: 'operation_disposed' });
  });

  it('marks only the current operation incompatible and permits a later replacement', async () => {
    const target = { googletag: { apiReady: true, cmd: {} } };
    const adapter = createBrowserGoogletagAdapter(target);
    const incompatible = adapter.run(vi.fn());

    expect(incompatible.status).toBe('incompatible');
    await expect(incompatible.result).rejects.toMatchObject({
      code: 'external_artifact_incompatible',
    });

    target.googletag = createReadyGoogletag().googletag;
    const replacement = adapter.run(() => 'replacement');
    expect(replacement.status).toBe('present');
    await expect(replacement.result).resolves.toBe('replacement');
  });

  it('holds 64 pending operations and fails only overflow synchronously', async () => {
    vi.useFakeTimers();
    const target: { googletag?: unknown } = {};
    const adapter = createBrowserGoogletagAdapter(target);
    const operations = Array.from({ length: 64 }, () => adapter.run(() => undefined));

    expect(operations.every(({ status }) => status === 'pending')).toBe(true);
    expect(() => adapter.run(() => undefined)).toThrowError(
      expect.objectContaining({ code: 'external_queue_full' })
    );

    target.googletag = createReadyGoogletag().googletag;
    adapter.notifyReady();
    await expect(Promise.all(operations.map(({ result }) => result))).resolves.toHaveLength(64);
  });

  it('reserves pending GPT capacity before hostile signal registration reenters', async () => {
    const target: { googletag?: unknown } = {};
    const adapter = createBrowserGoogletagAdapter(target);
    const accepted: Array<ReturnType<typeof adapter.run>> = [];
    const overflows: unknown[] = [];
    const order: number[] = [];
    const signal = {
      aborted: false,
      addEventListener: vi.fn(() => {
        for (let index = 1; index <= 64; index += 1) {
          try {
            accepted.push(adapter.run(() => order.push(index)));
          } catch (error) {
            overflows.push(error);
          }
        }
      }),
      removeEventListener: vi.fn(),
    } as unknown as AbortSignal;

    const outer = adapter.run(() => order.push(0), { signal });

    expect(accepted).toHaveLength(63);
    expect(overflows).toHaveLength(1);
    expect(overflows[0]).toMatchObject({ code: 'external_queue_full' });

    target.googletag = createReadyGoogletag().googletag;
    adapter.notifyReady();
    await expect(
      Promise.all([outer.result, ...accepted.map(({ result }) => result)])
    ).resolves.toHaveLength(64);
    expect(order).toEqual(Array.from({ length: 64 }, (_, index) => index));
  });

  it('reserves pending GPT capacity before poisoned Set.add reenters', async () => {
    const target: { googletag?: unknown } = {};
    const adapter = createBrowserGoogletagAdapter(target);
    const accepted: Array<ReturnType<typeof adapter.run>> = [];
    const overflows: unknown[] = [];
    const order: number[] = [];
    const originalSetAdd = Set.prototype.add;
    let reentered = false;
    Set.prototype.add = function (this: Set<unknown>, value: unknown): Set<unknown> {
      if (!reentered) {
        reentered = true;
        Set.prototype.add = originalSetAdd;
        for (let index = 1; index <= 64; index += 1) {
          try {
            accepted.push(adapter.run(() => order.push(index)));
          } catch (error) {
            overflows.push(error);
          }
        }
      }
      return Reflect.apply(originalSetAdd, this, [value]) as Set<unknown>;
    } as typeof Set.prototype.add;

    let outer: ReturnType<typeof adapter.run> | undefined;
    try {
      outer = adapter.run(() => order.push(0));
    } finally {
      Set.prototype.add = originalSetAdd;
    }
    if (!outer) throw new Error('Expected a published GPT operation');

    expect(accepted).toHaveLength(63);
    expect(overflows).toHaveLength(1);
    expect(overflows[0]).toMatchObject({ code: 'external_queue_full' });

    target.googletag = createReadyGoogletag().googletag;
    adapter.notifyReady();
    await expect(
      Promise.all([outer.result, ...accepted.map(({ result }) => result)])
    ).resolves.toHaveLength(64);
    expect(order).toEqual([...Array.from({ length: 63 }, (_, index) => index + 1), 0]);

    target.googletag = undefined;
    const recovered = Array.from({ length: 64 }, () => adapter.run(vi.fn()));
    expect(() => adapter.run(vi.fn())).toThrowError(
      expect.objectContaining({ code: 'external_queue_full' })
    );
    target.googletag = createReadyGoogletag().googletag;
    adapter.notifyReady();
    await expect(Promise.all(recovered.map(({ result }) => result))).resolves.toHaveLength(64);
  });

  it('rolls back pending GPT publication when poisoned Set.add throws', async () => {
    vi.useFakeTimers();
    const target: { googletag?: unknown } = {};
    const adapter = createBrowserGoogletagAdapter(target);
    const publicationError = new Error('GPT publication failed');
    const command = vi.fn();
    const signalGetter = vi.fn(() => undefined);
    const options = Object.defineProperty({}, 'signal', {
      get: signalGetter,
    }) as { readonly signal?: AbortSignal };
    const originalSetAdd = Set.prototype.add;
    const originalSetDelete = Set.prototype.delete;
    const poisonedDelete = function (): boolean {
      throw new Error('GPT publication rollback delete failed');
    } as typeof Set.prototype.delete;
    let poisonNextAdd = true;
    Set.prototype.add = function (this: Set<unknown>, value: unknown): Set<unknown> {
      if (poisonNextAdd) {
        poisonNextAdd = false;
        Set.prototype.add = originalSetAdd;
        Reflect.apply(originalSetAdd, this, [value]);
        throw publicationError;
      }
      return Reflect.apply(originalSetAdd, this, [value]) as Set<unknown>;
    } as typeof Set.prototype.add;
    Set.prototype.delete = poisonedDelete;

    let thrown: unknown;
    try {
      adapter.run(command, options);
    } catch (error) {
      thrown = error;
    } finally {
      Set.prototype.add = originalSetAdd;
      Set.prototype.delete = originalSetDelete;
    }

    expect(thrown).toBe(publicationError);
    expect(command).not.toHaveBeenCalled();
    expect(signalGetter).not.toHaveBeenCalled();
    expect(vi.getTimerCount()).toBe(0);

    const recovered = Array.from({ length: 64 }, () => adapter.run(vi.fn()));
    expect(() => adapter.run(vi.fn())).toThrowError(
      expect.objectContaining({ code: 'external_queue_full' })
    );
    target.googletag = createReadyGoogletag().googletag;
    adapter.notifyReady();
    await expect(Promise.all(recovered.map(({ result }) => result))).resolves.toHaveLength(64);
    expect(vi.getTimerCount()).toBe(0);
    Set.prototype.delete = poisonedDelete;
    try {
      expect(() => adapter.dispose()).not.toThrow();
    } finally {
      Set.prototype.delete = originalSetDelete;
    }
    await Promise.resolve();
  });

  it.each(['signal-getter', 'aborted-getter', 'add-throw', 'abort-remove-throw'] as const)(
    'contains hostile GPT AbortSignal ownership for %s',
    async (failure) => {
      const adapter = createBrowserGoogletagAdapter({});
      const signalError = new Error(`signal failure: ${failure}`);
      const listeners = new Set<() => void>();
      const removeEventListener = vi.fn((_type: string, listener: () => void) => {
        if (failure === 'abort-remove-throw') throw signalError;
        listeners.delete(listener);
      });
      const signal = Object.defineProperties(
        {},
        {
          aborted: {
            get: () => {
              if (failure === 'aborted-getter') throw signalError;
              return false;
            },
          },
          addEventListener: {
            value: vi.fn((_type: string, listener: () => void) => {
              listeners.add(listener);
              if (failure === 'add-throw') throw signalError;
              if (failure === 'abort-remove-throw') listener();
            }),
          },
          removeEventListener: { value: removeEventListener },
        }
      ) as AbortSignal;
      const options =
        failure === 'signal-getter'
          ? (Object.defineProperty({}, 'signal', {
              get: () => {
                throw signalError;
              },
            }) as { readonly signal?: AbortSignal })
          : { signal };
      let operation: ReturnType<typeof adapter.run> | undefined;

      expect(() => {
        operation = adapter.run(vi.fn(), options);
      }).not.toThrow();
      if (!operation) throw new Error('Expected a published GPT operation');
      if (failure === 'abort-remove-throw') {
        await expect(operation.result).rejects.toMatchObject({ code: 'caller_aborted' });
      } else {
        await expect(operation.result).rejects.toBe(signalError);
      }
      if (failure === 'add-throw' || failure === 'abort-remove-throw') {
        expect(removeEventListener).toHaveBeenCalledTimes(1);
      }

      const fillers: Array<ReturnType<typeof adapter.run>> = [];
      for (let index = 0; index < 64; index += 1) fillers.push(adapter.run(vi.fn()));
      expect(() => adapter.run(vi.fn())).toThrowError(
        expect.objectContaining({ code: 'external_queue_full' })
      );
      adapter.dispose();
      await Promise.all(fillers.map(({ result }) => result.catch((error: unknown) => error)));
    }
  );

  it.each([
    'add-getter',
    'before-install',
    'after-install',
    'reentrant-callback',
    'post-check-throw',
  ] as const)(
    'settles GPT abort transitions during listener registration for %s',
    async (transition) => {
      vi.useFakeTimers();
      const target: { googletag?: unknown } = {};
      const adapter = createBrowserGoogletagAdapter(target);
      const signalError = new Error(`abort transition failure: ${transition}`);
      const listeners = new Set<() => void>();
      let aborted = false;
      let abortedReads = 0;
      const addEventListener = vi.fn((_type: string, listener: () => void) => {
        if (transition === 'before-install') aborted = true;
        listeners.add(listener);
        if (transition === 'after-install') aborted = true;
        if (transition === 'reentrant-callback') {
          aborted = true;
          listener();
        }
      });
      const removeEventListener = vi.fn((_type: string, listener: () => void) => {
        listeners.delete(listener);
      });
      const signal = Object.defineProperties(
        {},
        {
          aborted: {
            get: () => {
              abortedReads += 1;
              if (transition === 'post-check-throw' && abortedReads === 2) throw signalError;
              return aborted;
            },
          },
          addEventListener: {
            get: () => {
              if (transition === 'add-getter') aborted = true;
              return addEventListener;
            },
          },
          removeEventListener: { value: removeEventListener },
        }
      ) as AbortSignal;
      const command = vi.fn();
      const operation = adapter.run(command, { signal });
      const result = operation.result.catch((error: unknown) => error);

      await vi.advanceTimersByTimeAsync(10_000);
      if (transition === 'post-check-throw') await expect(result).resolves.toBe(signalError);
      else await expect(result).resolves.toMatchObject({ code: 'caller_aborted' });
      expect(addEventListener).toHaveBeenCalledTimes(1);
      expect(removeEventListener).toHaveBeenCalledTimes(1);
      expect(listeners).toHaveLength(0);

      target.googletag = createReadyGoogletag().googletag;
      adapter.notifyReady();
      expect(command).not.toHaveBeenCalled();

      target.googletag = undefined;
      const fillers = Array.from({ length: 64 }, () => adapter.run(vi.fn()));
      expect(() => adapter.run(vi.fn())).toThrowError(
        expect.objectContaining({ code: 'external_queue_full' })
      );
      adapter.dispose();
      await Promise.all(
        fillers.map(({ result: filler }) => filler.catch((error: unknown) => error))
      );
    }
  );

  it('uses one exact independent ten-second deadline per enqueued operation', async () => {
    vi.useFakeTimers();
    const adapter = createBrowserGoogletagAdapter({});
    const first = adapter.run(vi.fn());
    const firstResult = first.result.catch((error: unknown) => error);
    await vi.advanceTimersByTimeAsync(5_000);
    const second = adapter.run(vi.fn());
    const secondResult = second.result.catch((error: unknown) => error);

    await vi.advanceTimersByTimeAsync(4_999);
    expect(first.status).toBe('pending');
    expect(second.status).toBe('pending');

    await vi.advanceTimersByTimeAsync(1);
    expect(first.status).toBe('timed_out');
    await expect(firstResult).resolves.toMatchObject({ code: 'external_ready_timeout' });
    expect(second.status).toBe('pending');

    await vi.advanceTimersByTimeAsync(5_000);
    expect(second.status).toBe('timed_out');
    await expect(secondResult).resolves.toMatchObject({ code: 'external_ready_timeout' });
  });

  it('lets readiness immediately before the deadline win the operation latch', async () => {
    vi.useFakeTimers();
    const target: { googletag?: unknown } = {};
    const adapter = createBrowserGoogletagAdapter(target);
    const operation = adapter.run(() => 'ready');

    await vi.advanceTimersByTimeAsync(9_999);
    target.googletag = createReadyGoogletag().googletag;
    adapter.notifyReady();
    await vi.advanceTimersByTimeAsync(1);

    expect(operation.status).toBe('present');
    await expect(operation.result).resolves.toBe('ready');
  });

  it('lets the first callback at the exact deadline win and keeps the loser inert', async () => {
    vi.useFakeTimers();
    const ready = createReadyGoogletag();
    const target: { googletag?: unknown } = {};
    const adapter = createBrowserGoogletagAdapter(target);
    vi.setSystemTime(0);
    const operation = adapter.run(() => 'ready');

    vi.setSystemTime(10_000);
    target.googletag = ready.googletag;
    adapter.notifyReady();
    await vi.runOnlyPendingTimersAsync();

    expect(operation.status).toBe('present');
    await expect(operation.result).resolves.toBe('ready');
    expect(ready.googletag.cmd.push).toHaveBeenCalledTimes(1);
  });

  it('lets timeout at or after the deadline win and ignores late readiness', async () => {
    vi.useFakeTimers();
    const command = vi.fn();
    const target: { googletag?: unknown } = {};
    const adapter = createBrowserGoogletagAdapter(target);
    const operation = adapter.run(command);
    const result = operation.result.catch((error: unknown) => error);

    await vi.advanceTimersByTimeAsync(10_000);
    target.googletag = createReadyGoogletag().googletag;
    adapter.notifyReady();

    expect(operation.status).toBe('timed_out');
    await expect(result).resolves.toMatchObject({ code: 'external_ready_timeout' });
    expect(command).not.toHaveBeenCalled();
  });

  it('removes aborted and disposed operations immediately', async () => {
    vi.useFakeTimers();
    const controller = new AbortController();
    const target: { googletag?: unknown } = {};
    const adapter = createBrowserGoogletagAdapter(target);
    const abortedCommand = vi.fn();
    const disposedCommand = vi.fn();
    const aborted = adapter.run(abortedCommand, { signal: controller.signal });
    const disposed = adapter.run(disposedCommand);
    const abortedResult = aborted.result.catch((error: unknown) => error);
    const disposedResult = disposed.result.catch((error: unknown) => error);

    controller.abort();
    disposed.dispose();
    const replacements = Array.from({ length: 64 }, () => adapter.run(() => undefined));
    target.googletag = createReadyGoogletag().googletag;
    adapter.notifyReady();

    await expect(abortedResult).resolves.toMatchObject({ code: 'caller_aborted' });
    await expect(disposedResult).resolves.toMatchObject({ code: 'operation_disposed' });
    expect(abortedCommand).not.toHaveBeenCalled();
    expect(disposedCommand).not.toHaveBeenCalled();
    await expect(Promise.all(replacements.map(({ result }) => result))).resolves.toHaveLength(64);
  });

  it('disposes the adapter by removing every pending operation', async () => {
    const command = vi.fn();
    const target: { googletag?: unknown } = {};
    const adapter = createBrowserGoogletagAdapter(target);
    const operation = adapter.run(command);
    const result = operation.result.catch((error: unknown) => error);

    adapter.dispose();
    target.googletag = createReadyGoogletag().googletag;
    adapter.notifyReady();

    await expect(result).resolves.toMatchObject({ code: 'operation_disposed' });
    expect(command).not.toHaveBeenCalled();
  });

  it('invalidates an entered command immediately when the adapter is disposed', async () => {
    const ready = createReadyGoogletag();
    const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });
    const operation = adapter.run((gpt) => {
      adapter.dispose();
      gpt.display('must-not-display');
    });

    await expect(operation.result).rejects.toMatchObject({ code: 'operation_disposed' });
    expect(operation.status).toBe('present');
    expect(ready.display).not.toHaveBeenCalled();
    expect(ready.listeners.size).toBe(0);
  });

  it('throws when GPT inspection disposes the adapter before an operation is published', () => {
    const ready = createReadyGoogletag();
    const holder: { adapter?: ReturnType<typeof createBrowserGoogletagAdapter> } = {};
    const target = Object.defineProperty({}, 'googletag', {
      get: () => {
        holder.adapter?.dispose();
        return ready.googletag;
      },
    });
    const adapter = createBrowserGoogletagAdapter(target);
    holder.adapter = adapter;
    const command = vi.fn();

    expect(() => adapter.run(command)).toThrowError(
      expect.objectContaining({ code: 'operation_disposed' })
    );
    expect(command).not.toHaveBeenCalled();
    expect(ready.commands).toHaveLength(0);
  });

  it('rejects without enqueueing when GPT inspection disposes a published operation', async () => {
    const ready = createReadyGoogletag({ deferCommands: true });
    let reads = 0;
    const holder: { adapter?: ReturnType<typeof createBrowserGoogletagAdapter> } = {};
    const target = Object.defineProperty({}, 'googletag', {
      get: () => {
        reads += 1;
        if (reads === 3) holder.adapter?.dispose();
        return ready.googletag;
      },
    });
    const adapter = createBrowserGoogletagAdapter(target);
    holder.adapter = adapter;
    const command = vi.fn();
    const operation = adapter.run(command);

    await expect(operation.result).rejects.toMatchObject({ code: 'operation_disposed' });
    expect(command).not.toHaveBeenCalled();
    expect(ready.commands).toHaveLength(0);
  });

  it('contains disposal reentrant from GPT member reads and external calls', async () => {
    const ready = createReadyGoogletag();
    const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });
    const staleSet = vi.fn();
    const slot = Object.defineProperty({}, 'setTargeting', {
      get: () => {
        adapter.dispose();
        return staleSet;
      },
    });
    const memberOperation = adapter.run((gpt) => gpt.setTargeting(slot, 'key', 'value'));

    await expect(memberOperation.result).rejects.toMatchObject({ code: 'operation_disposed' });
    expect(staleSet).not.toHaveBeenCalled();

    const externalReady = createReadyGoogletag();
    const externalAdapter = createBrowserGoogletagAdapter({
      googletag: externalReady.googletag,
    });
    externalReady.display.mockImplementation(() => externalAdapter.dispose());
    const externalOperation = externalAdapter.run((gpt) => {
      gpt.display('first');
      gpt.display('must-not-display');
    });

    await expect(externalOperation.result).rejects.toMatchObject({ code: 'operation_disposed' });
    expect(externalReady.display).toHaveBeenCalledTimes(1);
  });

  it('does not enqueue after disposal reentrant from GPT configuration reads', async () => {
    const ready = createReadyGoogletag({ deferCommands: true });
    const nativeSetConfig = ready.googletag.setConfig;
    const nativeDisableInitialLoad = ready.pubads.disableInitialLoad;
    ready.googletag.getConfig.mockImplementation(() => {
      adapter.dispose();
      return { disableInitialLoad: false };
    });
    const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });
    const command = vi.fn();
    const operation = adapter.run(command);

    await expect(operation.result).rejects.toMatchObject({ code: 'operation_disposed' });
    expect(ready.commands).toHaveLength(0);
    expect(command).not.toHaveBeenCalled();
    expect(ready.googletag.setConfig).toBe(nativeSetConfig);
    expect(ready.pubads.disableInitialLoad).toBe(nativeDisableInitialLoad);

    ready.googletag.getConfig.mockImplementation((key: string) =>
      key === 'disableInitialLoad' ? { disableInitialLoad: ready.initialLoad.disabled } : {}
    );
    const laterAdapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });
    const laterOperation = laterAdapter.run((gpt) => gpt.serviceState());
    ready.commands[0]?.();
    await laterOperation.result;
    laterAdapter.dispose();

    expect(ready.googletag.setConfig).toBe(nativeSetConfig);
    expect(ready.pubads.disableInitialLoad).toBe(nativeDisableInitialLoad);
  });

  it.each(['root', 'service'] as const)(
    'rolls back a GPT %s wrapper when post-install currentness inspection disposes',
    async (wrapperKind) => {
      const ready = createReadyGoogletag({ deferCommands: true });
      const nativeSetConfig = ready.googletag.setConfig;
      const nativeDisableInitialLoad = ready.pubads.disableInitialLoad;
      let wrappedReads = 0;
      const holder: { adapter?: ReturnType<typeof createBrowserGoogletagAdapter> } = {};
      const target = Object.defineProperty({}, 'googletag', {
        get: () => {
          const wrapped =
            wrapperKind === 'root'
              ? ready.googletag.setConfig !== nativeSetConfig
              : ready.pubads.disableInitialLoad !== nativeDisableInitialLoad;
          if (wrapped) {
            wrappedReads += 1;
            if (wrappedReads === 7) holder.adapter?.dispose();
          }
          return ready.googletag;
        },
      });
      const adapter = createBrowserGoogletagAdapter(target);
      holder.adapter = adapter;
      const operation = adapter.run(vi.fn());

      await expect(operation.result).rejects.toMatchObject({ code: 'operation_disposed' });
      expect(ready.googletag.setConfig).toBe(nativeSetConfig);
      expect(ready.pubads.disableInitialLoad).toBe(nativeDisableInitialLoad);
      expect(ready.commands).toHaveLength(0);
    }
  );

  it('keeps abort ownership while a ready GPT command callback is deferred', async () => {
    const deferred = createReadyGoogletag({ deferCommands: true });
    const controller = new AbortController();
    const command = vi.fn();
    const adapter = createBrowserGoogletagAdapter({ googletag: deferred.googletag });
    const operation = adapter.run(command, { signal: controller.signal });
    const result = operation.result.catch((error: unknown) => error);

    expect(operation.status).toBe('present');
    controller.abort();
    expect(() => deferred.commands[0]?.()).not.toThrow();

    await expect(result).resolves.toMatchObject({ code: 'caller_aborted' });
    expect(command).not.toHaveBeenCalled();
  });

  it('rejects a deferred command against a replaced GPT object and accepts later work', async () => {
    const first = createReadyGoogletag({ deferCommands: true });
    const replacement = createReadyGoogletag();
    const target: { googletag?: unknown } = { googletag: first.googletag };
    const adapter = createBrowserGoogletagAdapter(target);
    const command = vi.fn();
    const operation = adapter.run(command);
    const result = operation.result.catch((error: unknown) => error);

    target.googletag = replacement.googletag;
    expect(() => first.commands[0]?.()).not.toThrow();

    await expect(result).resolves.toMatchObject({ code: 'external_artifact_incompatible' });
    expect(operation.status).toBe('incompatible');
    expect(command).not.toHaveBeenCalled();
    await expect(adapter.run(() => 'replacement').result).resolves.toBe('replacement');
  });

  it('rechecks GPT identity around hostile member reads and external calls', async () => {
    const first = createReadyGoogletag();
    const replacement = createReadyGoogletag();
    const target: { googletag?: unknown } = { googletag: first.googletag };
    const adapter = createBrowserGoogletagAdapter(target);
    const staleSet = vi.fn();
    const slot = Object.defineProperty({}, 'setTargeting', {
      get: () => {
        target.googletag = replacement.googletag;
        return staleSet;
      },
    });
    const operation = adapter.run((gpt) => gpt.setTargeting(slot, 'key', 'value'));

    await expect(operation.result).rejects.toMatchObject({
      code: 'external_artifact_incompatible',
    });
    expect(operation.status).toBe('incompatible');
    expect(staleSet).not.toHaveBeenCalled();
  });

  it('makes an old GPT listener inert after whole-object replacement', async () => {
    const first = createReadyGoogletag();
    const replacement = createReadyGoogletag();
    const target: { googletag?: unknown } = { googletag: first.googletag };
    const adapter = createBrowserGoogletagAdapter(target);
    const listener = vi.fn();
    let unsubscribe = (): void => undefined;
    await adapter.run((gpt) => {
      unsubscribe = gpt.subscribe('slotRequested', listener);
    }).result;
    const oldListener = [...(first.listeners.get('slotRequested') ?? [])][0];

    target.googletag = replacement.googletag;
    expect(() => oldListener?.({ slot: {} })).not.toThrow();
    unsubscribe();

    expect(listener).not.toHaveBeenCalled();
    expect(first.pubads.removeEventListener).toHaveBeenCalledTimes(1);
  });

  it('publishes frozen diagnostics facts after the sole adapter listener completes', async () => {
    const ready = createReadyGoogletag();
    const performance = { now: vi.fn(() => 42.25) };
    const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag, performance });
    const order: string[] = [];
    const facts: unknown[] = [];
    const releaseDiagnostics = adapter.observeDiagnostics?.((fact) => {
      order.push('diagnostics');
      facts.push(fact);
      throw new Error('fictional diagnostics failure');
    });
    expect(releaseDiagnostics).toEqual(expect.any(Function));
    expect(ready.pubads.addEventListener).not.toHaveBeenCalled();

    await adapter.run((gpt) =>
      gpt.subscribe('slotRenderEnded', () => {
        order.push('correctness');
      })
    ).result;
    expect(ready.pubads.addEventListener).toHaveBeenCalledOnce();
    const slot = Object.freeze({
      getSlotElementId: () => 'fictional-slot',
      getAdUnitPath: () => '/example/fictional-slot',
      setTargeting: vi.fn(),
    });
    const emit = (event: unknown): void => {
      for (const listener of ready.listeners.get('slotRenderEnded') ?? []) listener(event);
    };
    expect(() =>
      emit({
        slot,
        isEmpty: false,
        size: [300, 250],
        isBackfill: true,
        slotContentChanged: false,
        lineItemId: 101,
        creativeId: 202,
        campaignId: 303,
        advertiserId: 404,
        sourceAgnosticLineItemId: 505,
        sourceAgnosticCreativeId: 606,
        yieldGroupIds: [707, -1, 808, Number.NaN],
        companyIds: [909, 1010],
      })
    ).not.toThrow();

    expect(order).toEqual(['correctness', 'diagnostics']);
    expect(facts).toEqual([
      {
        kind: 'slotRenderEnded',
        observedAtMs: 42.25,
        slot: {
          token: expect.any(Object),
          traceToken: 'gt1_1',
          runtimeSlotNumber: 1,
          elementId: 'fictional-slot',
          adUnitPath: '/example/fictional-slot',
        },
        isEmpty: false,
        size: [300, 250],
        isBackfill: true,
        slotContentChanged: false,
        adManager: {
          lineItemId: 101,
          creativeId: 202,
          campaignId: 303,
          advertiserId: 404,
          sourceAgnosticLineItemId: 505,
          sourceAgnosticCreativeId: 606,
          yieldGroupIds: [707, 808],
          companyIds: [909, 1010],
        },
      },
    ]);
    expect(Object.isFrozen(facts[0])).toBe(true);
    expect(Object.isFrozen((facts[0] as { size: unknown }).size)).toBe(true);
    const adManager = (facts[0] as { adManager: Record<string, unknown> }).adManager;
    expect(Object.isFrozen(adManager)).toBe(true);
    expect(Object.isFrozen(adManager['yieldGroupIds'])).toBe(true);
    expect(Object.isFrozen(adManager['companyIds'])).toBe(true);
    const safeSlot = (facts[0] as { slot: Record<string, unknown> }).slot;
    expect(Object.isFrozen(safeSlot)).toBe(true);
    expect(Object.isFrozen(safeSlot['token'])).toBe(true);
    expect(Reflect.ownKeys(safeSlot).sort()).toEqual([
      'adUnitPath',
      'elementId',
      'runtimeSlotNumber',
      'token',
      'traceToken',
    ]);
    expect(Object.values(safeSlot).some((value) => typeof value === 'function')).toBe(false);
    expect(safeSlot).not.toBe(slot);

    releaseDiagnostics?.();
    emit({ slot, isEmpty: true });
    expect(facts).toHaveLength(1);
  });

  it.each([false, true])(
    'publishes correctness facts from the sole listeners and conditions four diagnostics-only listeners (active=%s)',
    async (diagnosticsActive) => {
      const ready = createReadyGoogletag();
      const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });
      const facts: GoogletagDiagnosticsFact[] = [];
      const accepted = createGoogletagTraceCycleHandle(() => false);
      adapter.observeDiagnostics?.((fact) => facts.push(fact));
      await adapter.run((gpt) => {
        gpt.subscribe('slotRequested', () => accepted);
        gpt.subscribe('slotRenderEnded', () => accepted);
        if (diagnosticsActive) {
          for (const eventType of [
            'slotResponseReceived',
            'slotOnload',
            'impressionViewable',
            'slotVisibilityChanged',
          ]) {
            gpt.subscribe(eventType, () => undefined, true);
          }
        }
      }).result;
      const slot = { getSlotElementId: () => 'sole-listener-slot' };
      const emit = (eventType: string, fields: Readonly<Record<string, unknown>> = {}): void => {
        const event = { slot, ...fields };
        for (const listener of ready.listeners.get(eventType) ?? []) listener(event);
      };

      emit('slotRequested');
      if (diagnosticsActive) emit('slotResponseReceived');
      emit('slotRenderEnded', { isEmpty: false });
      if (diagnosticsActive) {
        emit('slotOnload');
        emit('impressionViewable');
        emit('slotVisibilityChanged', { inViewPercentage: 75 });
      }

      expect(ready.pubads.addEventListener).toHaveBeenCalledTimes(diagnosticsActive ? 6 : 2);
      expect(facts.map(({ kind }) => kind)).toEqual(
        diagnosticsActive
          ? [
              'slotRequested',
              'slotResponseReceived',
              'slotRenderEnded',
              'slotOnload',
              'impressionViewable',
              'slotVisibilityChanged',
            ]
          : ['slotRequested', 'slotRenderEnded']
      );
      expect(facts.every(({ slot: snapshot }) => snapshot.cycleOrdinal === 1)).toBe(true);
    }
  );

  it('opens trace cycles only from accepted lifecycle handles and publishes each raw event once', async () => {
    const ready = createReadyGoogletag();
    const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });
    const facts: GoogletagDiagnosticsFact[] = [];
    const lifecycle = { retired: false };
    const accepted = createGoogletagTraceCycleHandle(() => lifecycle.retired);
    adapter.observeDiagnostics?.((fact) => facts.push(fact));
    await adapter.run((gpt) => {
      gpt.subscribe('slotRequested', () => accepted);
      gpt.subscribe('slotRenderEnded', () => accepted);
    }).result;
    const slot = { getSlotElementId: () => 'accepted-cycle-slot' };
    const emit = (eventType: string, fields: Readonly<Record<string, unknown>> = {}): void => {
      const event = { slot, ...fields };
      for (const listener of ready.listeners.get(eventType) ?? []) listener(event);
    };

    emit('slotRequested');
    emit('slotRenderEnded', { isEmpty: false, responseIdentifier: 'accepted-response' });
    lifecycle.retired = true;
    emit('slotRequested');

    expect(facts.map(({ kind, slot: snapshot }) => [kind, snapshot.cycleOrdinal])).toEqual([
      ['slotRequested', 1],
      ['slotRenderEnded', 1],
      ['slotRequested', undefined],
    ]);
  });

  it('publishes a correctness fact only after lifecycle attribution completes', async () => {
    const ready = createReadyGoogletag();
    const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });
    const facts: GoogletagDiagnosticsFact[] = [];
    const order: string[] = [];
    const accepted = createGoogletagTraceCycleHandle(() => false);
    adapter.observeDiagnostics?.((fact) => {
      order.push('diagnostics');
      facts.push(fact);
    });
    await adapter.run((gpt) => {
      gpt.subscribe('slotRequested', () => {
        order.push('correctness');
        return accepted;
      });
    }).result;
    const slot = { getSlotElementId: () => 'attributed-cycle-slot' };

    const event = { slot };
    for (const listener of ready.listeners.get('slotRequested') ?? []) listener(event);

    expect(facts).toHaveLength(1);
    expect(facts[0]?.slot.cycleOrdinal).toBe(1);
    expect(order).toEqual(['correctness', 'diagnostics']);
  });

  it('reports duplicate, reused, and already-open cycle handles once while preserving raw facts', async () => {
    const ready = createReadyGoogletag();
    const reports = vi.fn();
    const adapter = createBrowserGoogletagAdapter(
      { googletag: ready.googletag },
      { reportDiagnosticsFailure: reports }
    );
    const facts: GoogletagDiagnosticsFact[] = [];
    const first = createGoogletagTraceCycleHandle(() => false);
    const second = createGoogletagTraceCycleHandle(() => false);
    let selected = first;
    adapter.observeDiagnostics?.((fact) => facts.push(fact));
    await adapter.run((gpt) => {
      gpt.subscribe('slotRequested', () => selected);
    }).result;
    const firstSlot = { getSlotElementId: () => 'cycle-collision-one' };
    const secondSlot = { getSlotElementId: () => 'cycle-collision-two' };
    const emit = (slot: object): void => {
      const event = { slot };
      for (const listener of ready.listeners.get('slotRequested') ?? []) listener(event);
    };

    emit(firstSlot);
    emit(firstSlot);
    selected = second;
    emit(firstSlot);
    selected = first;
    emit(secondSlot);

    expect(facts).toHaveLength(4);
    expect(facts.map(({ slot }) => slot.cycleOrdinal)).toEqual([
      1,
      undefined,
      undefined,
      undefined,
    ]);
    expect(reports).toHaveBeenCalledOnce();
    expect(reports).toHaveBeenCalledWith('trace_cycle_collision');
  });

  it('reports nonunique callbacks once without suppressing raw facts', async () => {
    const ready = createReadyGoogletag();
    const reports = vi.fn();
    const adapter = createBrowserGoogletagAdapter(
      { googletag: ready.googletag },
      { reportDiagnosticsFailure: reports }
    );
    const facts: GoogletagDiagnosticsFact[] = [];
    const first = createGoogletagTraceCycleHandle(() => false);
    const second = createGoogletagTraceCycleHandle(() => false);
    adapter.observeDiagnostics?.((fact) => facts.push(fact));
    const slot = { getSlotElementId: () => 'cycle-ambiguity' };
    const emit = (eventType: string, fields: Readonly<Record<string, unknown>> = {}): void => {
      const event = { slot, ...fields };
      for (const listener of ready.listeners.get(eventType) ?? []) listener(event);
    };

    await adapter.run((gpt) => {
      gpt.subscribe('slotRequested', (event) =>
        (event as { cycle: number }).cycle === 1 ? first : second
      );
      gpt.subscribe('slotRenderEnded', (event) =>
        (event as { cycle: number }).cycle === 1 ? first : second
      );
      gpt.subscribe('slotOnload', () => undefined, true);
    }).result;
    emit('slotRequested', { cycle: 1 });
    emit('slotRenderEnded', { cycle: 1, isEmpty: false });
    emit('slotRequested', { cycle: 2 });
    emit('slotRenderEnded', { cycle: 2, isEmpty: false });
    emit('slotOnload');

    expect(facts).toHaveLength(5);
    expect(facts.map(({ slot: snapshot }) => snapshot.cycleOrdinal)).toEqual([
      1,
      1,
      2,
      2,
      undefined,
    ]);
    expect(reports).toHaveBeenCalledOnce();
    expect(reports).toHaveBeenCalledWith('trace_cycle_ambiguity');
  });

  it('never transfers a trace handle from a non-owner into a diagnostics-only fact', async () => {
    const ready = createReadyGoogletag();
    const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });
    const facts: GoogletagDiagnosticsFact[] = [];
    const first = createGoogletagTraceCycleHandle(() => false);
    const second = createGoogletagTraceCycleHandle(() => false);
    let selected = first;
    adapter.observeDiagnostics?.((fact) => facts.push(fact));
    await adapter.run((gpt) => {
      gpt.subscribe('slotRequested', () => selected);
      gpt.subscribe('slotRenderEnded', () => selected);
      gpt.subscribe('slotOnload', () => selected);
      gpt.subscribe('slotOnload', () => undefined, true);
    }).result;
    const slot = { getSlotElementId: () => 'diagnostics-only-attribution-slot' };
    const emit = (eventType: string, fields: Readonly<Record<string, unknown>> = {}): void => {
      const event = { slot, ...fields };
      for (const listener of ready.listeners.get(eventType) ?? []) listener(event);
    };

    emit('slotRequested');
    emit('slotRenderEnded', { isEmpty: false });
    selected = second;
    emit('slotRequested');
    emit('slotRenderEnded', { isEmpty: false });
    selected = first;
    emit('slotOnload');

    expect(facts.map(({ kind, slot: snapshot }) => [kind, snapshot.cycleOrdinal])).toEqual([
      ['slotRequested', 1],
      ['slotRenderEnded', 1],
      ['slotRequested', 2],
      ['slotRenderEnded', 2],
      ['slotOnload', undefined],
    ]);
  });

  it('accepts correctness attribution only from an adapter-branded trace handle', async () => {
    const ready = createReadyGoogletag();
    const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });
    const facts: GoogletagDiagnosticsFact[] = [];
    const forged = Object.freeze({ isRetired: () => false });
    adapter.observeDiagnostics?.((fact) => facts.push(fact));
    await adapter.run((gpt) => {
      gpt.subscribe('slotRequested', () => forged as never);
    }).result;
    const slot = { getSlotElementId: () => 'forged-attribution-slot' };

    for (const listener of ready.listeners.get('slotRequested') ?? []) listener({ slot });

    expect(facts).toHaveLength(1);
    expect(facts[0]?.slot.cycleOrdinal).toBeUndefined();
  });

  it('admits only one diagnostics observer without adding GPT listeners', () => {
    const ready = createReadyGoogletag();
    const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });
    const release = adapter.observeDiagnostics?.(vi.fn());

    expect(adapter.observeDiagnostics?.(vi.fn())).toBeUndefined();
    expect(ready.pubads.addEventListener).not.toHaveBeenCalled();
    release?.();
    expect(adapter.observeDiagnostics?.(vi.fn())).toEqual(expect.any(Function));
    expect(ready.pubads.addEventListener).not.toHaveBeenCalled();
  });

  it('keeps distinct opaque and canonical trace tokens per physical Slot', async () => {
    const ready = createReadyGoogletag();
    const adapter = createBrowserGoogletagAdapter({
      googletag: ready.googletag,
      performance: { now: () => 7 },
    });
    const facts: GoogletagDiagnosticsFact[] = [];
    adapter.observeDiagnostics?.((fact) => facts.push(fact));
    await adapter.run((gpt) => gpt.subscribe('slotRequested', () => undefined)).result;
    const first = {
      getSlotElementId: () => 'same-id',
      getAdUnitPath: () => '/example/first',
      setTargeting: vi.fn(),
    };
    const replacement = {
      getSlotElementId: () => 'same-id',
      getAdUnitPath: () => '/example/replacement',
      setTargeting: vi.fn(),
    };
    const emit = (slot: object): void => {
      for (const listener of ready.listeners.get('slotRequested') ?? []) listener({ slot });
    };

    emit(first);
    emit(first);
    emit(replacement);

    expect(facts).toHaveLength(3);
    expect(facts[0]?.slot.token).toBe(facts[1]?.slot.token);
    expect(facts[2]?.slot.token).not.toBe(facts[0]?.slot.token);
    expect(facts[0]?.slot.traceToken).toBe('gt1_1');
    expect(facts[1]?.slot.traceToken).toBe('gt1_1');
    expect(facts[2]?.slot.traceToken).toBe('gt1_2');
    expect(facts[0]?.slot.cycleOrdinal).toBeUndefined();
    expect(facts[1]?.slot.cycleOrdinal).toBeUndefined();
    expect(facts[2]?.slot.cycleOrdinal).toBeUndefined();
    expect(facts[0]?.slot.token).not.toBe(facts[0]?.slot.traceToken);
    expect(Object.isFrozen(first)).toBe(false);
    expect(Object.isFrozen(replacement)).toBe(false);
    expect(Reflect.ownKeys(facts[0]?.slot ?? {}).sort()).toEqual([
      'adUnitPath',
      'elementId',
      'runtimeSlotNumber',
      'token',
      'traceToken',
    ]);
  });

  it('mints canonical trace identity before events and keeps it stable for handoff', () => {
    const adapter = createBrowserGoogletagAdapter({});
    const first = {};
    const replacement = {};

    expect(adapter.traceToken(first)).toBe('gt1_1');
    expect(adapter.traceToken(first)).toBe('gt1_1');
    expect(adapter.traceToken(replacement)).toBe('gt1_2');
    expect(adapter.traceToken(first)).toMatch(/^gt1_[1-9a-z][0-9a-z]{0,6}$/);
    adapter.dispose();
    expect(adapter.traceToken(first)).toBeUndefined();
  });

  it('adopts transferred per-slot diagnostics cycles and the global token high-water value once', () => {
    const adapter = createBrowserGoogletagAdapter({});
    const adopted = {};

    expect(
      adapter.adoptDiagnosticsState?.({
        nextTraceTokenOrdinal: 9,
        slots: [
          {
            nextCycleOrdinal: 4,
            physicalSlot: adopted,
            records: [
              {
                ordinal: 1,
                responseIdentifier: null,
                seen: ['slotRequested', 'slotRenderEnded'],
                state: 'completed',
              },
              {
                ordinal: 3,
                responseIdentifier: null,
                seen: ['slotRequested'],
                state: 'retired',
              },
            ],
            traceToken: 'gt1_5',
            unknownPriorCycle: true,
          },
        ],
      })
    ).toBe(true);
    expect(adapter.traceToken(adopted)).toBe('gt1_5');
    const adoptedIdentity = adapter.diagnosticsIdentity(adopted);
    expect(adoptedIdentity).toMatchObject({
      traceToken: 'gt1_5',
      runtimeSlotNumber: 5,
      cycleOrdinal: 3,
    });
    expect(typeof adoptedIdentity?.token).toBe('object');
    expect(adapter.diagnosticsIdentity(adopted)?.token).toBe(adoptedIdentity?.token);
    expect(adapter.traceToken({})).toBe('gt1_9');
    expect(adapter.adoptDiagnosticsState?.({ nextTraceTokenOrdinal: 10, slots: [] })).toBe(false);
  });

  it('preserves transferred cycle correlation for late diagnostics after takeover', async () => {
    const ready = createReadyGoogletag();
    const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });
    const adopted = {};
    const facts: GoogletagDiagnosticsFact[] = [];

    expect(
      adapter.adoptDiagnosticsState?.({
        nextTraceTokenOrdinal: 2,
        slots: [
          {
            nextCycleOrdinal: 2,
            physicalSlot: adopted,
            records: [
              {
                ordinal: 1,
                responseIdentifier: 'response-one',
                seen: ['slotRequested', 'slotRenderEnded'],
                state: 'completed',
              },
            ],
            traceToken: 'gt1_1',
            unknownPriorCycle: true,
          },
        ],
      } as never)
    ).toBe(true);
    adapter.observeDiagnostics((fact) => facts.push(fact));
    await adapter.run((gpt) => gpt.subscribe('slotOnload', () => undefined, true)).result;

    for (const listener of ready.listeners.get('slotOnload') ?? []) {
      listener({ slot: adopted, responseIdentifier: 'response-one' });
    }

    expect(facts).toHaveLength(1);
    expect(facts[0]?.slot).toMatchObject({ traceToken: 'gt1_1', cycleOrdinal: 1 });
  });

  it('rejects malformed diagnostics adoption without consuming the token high-water value', () => {
    const adapter = createBrowserGoogletagAdapter({});
    const physicalSlot = {};

    expect(
      adapter.adoptDiagnosticsState?.({
        nextTraceTokenOrdinal: 2,
        slots: [
          {
            nextCycleOrdinal: 1,
            physicalSlot,
            records: [
              {
                ordinal: 1,
                responseIdentifier: null,
                seen: ['slotRequested', 'slotRenderEnded'],
                state: 'completed',
              },
            ],
            traceToken: 'gt1_1',
            unknownPriorCycle: false,
          },
        ],
      })
    ).toBe(false);
    expect(adapter.traceToken(physicalSlot)).toBe('gt1_1');
  });

  it('does not retain default canonical tokens in a collision ledger', () => {
    const disposalClearCalls = (() => {
      const clear = vi.spyOn(Set.prototype, 'clear');
      try {
        const adapter = createBrowserGoogletagAdapter({});
        for (let ordinal = 1; ordinal <= 1_024; ordinal += 1) {
          expect(adapter.traceToken({})).toBe(`gt1_${ordinal.toString(36)}`);
        }
        const callsBeforeDispose = clear.mock.calls.length;

        adapter.dispose();
        return clear.mock.calls.length - callsBeforeDispose;
      } finally {
        clear.mockRestore();
      }
    })();

    expect(disposalClearCalls).toBe(0);
  });

  it('clears the custom minted trace-token collision ledger once on adapter disposal', () => {
    const disposalClearCalls = (() => {
      const clear = vi.spyOn(Set.prototype, 'clear');
      try {
        const adapter = createBrowserGoogletagAdapter(
          {},
          { mintTraceToken: (ordinal) => `gt1_${ordinal.toString(36)}` }
        );
        expect(adapter.traceToken({})).toBe('gt1_1');
        expect(adapter.traceToken({})).toBe('gt1_2');
        const callsBeforeDispose = clear.mock.calls.length;

        adapter.dispose();
        adapter.dispose();
        return clear.mock.calls.length - callsBeforeDispose;
      } finally {
        clear.mockRestore();
      }
    })();

    expect(disposalClearCalls).toBe(1);
  });

  it('mints trace-token boundary ordinals and reports exhaustion once without affecting older tokens', () => {
    const boundaries = createBrowserGoogletagAdapter({}, { initialTraceTokenOrdinal: 35 });
    expect(boundaries.traceToken({})).toBe('gt1_z');
    expect(boundaries.traceToken({})).toBe('gt1_10');

    const reports = vi.fn();
    const exhausted = createBrowserGoogletagAdapter(
      {},
      { initialTraceTokenOrdinal: 4_294_967_295, reportDiagnosticsFailure: reports }
    );
    const retained = {};
    expect(exhausted.traceToken(retained)).toBe('gt1_1z141z3');
    expect(exhausted.traceToken({})).toBeUndefined();
    expect(exhausted.traceToken({})).toBeUndefined();
    expect(exhausted.traceToken(retained)).toBe('gt1_1z141z3');
    expect(reports).toHaveBeenCalledTimes(1);
    expect(reports).toHaveBeenCalledWith('trace_token_exhausted');
  });

  it('contains one injected token collision and lets the next physical object reuse the unspent ordinal', () => {
    const reports = vi.fn(() => {
      throw new Error('fictional diagnostics reporter failure');
    });
    let calls = 0;
    const adapter = createBrowserGoogletagAdapter(
      {},
      {
        mintTraceToken: (ordinal) => {
          calls += 1;
          return calls === 2 ? 'gt1_1' : `gt1_${ordinal.toString(36)}`;
        },
        reportDiagnosticsFailure: reports,
      }
    );

    expect(adapter.traceToken({})).toBe('gt1_1');
    expect(() => adapter.traceToken({})).not.toThrow();
    expect(adapter.traceToken({})).toBe('gt1_2');
    expect(reports).toHaveBeenCalledOnce();
    expect(reports).toHaveBeenCalledWith('trace_token_collision');
  });

  it('contains per-object cycle exhaustion while preserving raw diagnostics delivery', async () => {
    const ready = createReadyGoogletag();
    const reports = vi.fn();
    const adapter = createBrowserGoogletagAdapter(
      { googletag: ready.googletag },
      { initialTraceCycleOrdinal: 4_294_967_295, reportDiagnosticsFailure: reports }
    );
    const facts: GoogletagDiagnosticsFact[] = [];
    let lifecycle = { retired: true };
    let accepted: ReturnType<typeof createGoogletagTraceCycleHandle> | undefined;
    adapter.observeDiagnostics?.((fact) => facts.push(fact));
    await adapter.run((gpt) => {
      gpt.subscribe('slotRequested', () => {
        lifecycle.retired = true;
        const current = { retired: false };
        lifecycle = current;
        accepted = createGoogletagTraceCycleHandle(() => current.retired);
        return accepted;
      });
      gpt.subscribe('slotRenderEnded', () => accepted);
    }).result;
    const slot = { getSlotElementId: () => 'cycle-exhaustion-slot' };
    const emit = (eventType: string, fields: Readonly<Record<string, unknown>> = {}): void => {
      const event = { slot, ...fields };
      for (const listener of ready.listeners.get(eventType) ?? []) listener(event);
    };

    emit('slotRequested');
    emit('slotRenderEnded', { isEmpty: false });
    emit('slotRequested');

    expect(facts.map((fact) => fact.slot.cycleOrdinal)).toEqual([
      4_294_967_295,
      4_294_967_295,
      undefined,
    ]);
    expect(reports).toHaveBeenCalledOnce();
    expect(reports).toHaveBeenCalledWith('trace_cycle_exhausted');
  });

  it('assigns exact per-object cycle ordinals and omits ambiguous callbacks', async () => {
    const ready = createReadyGoogletag();
    const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });
    const facts: GoogletagDiagnosticsFact[] = [];
    let lifecycle = { retired: true };
    let accepted: ReturnType<typeof createGoogletagTraceCycleHandle> | undefined;
    adapter.observeDiagnostics?.((fact) => facts.push(fact));
    await adapter.run((gpt) => {
      gpt.subscribe('slotRequested', () => {
        lifecycle.retired = true;
        const current = { retired: false };
        lifecycle = current;
        accepted = createGoogletagTraceCycleHandle(() => current.retired);
        return accepted;
      });
      gpt.subscribe('slotRenderEnded', () => accepted);
      for (const eventType of ['slotResponseReceived', 'impressionViewable']) {
        gpt.subscribe(eventType, () => undefined, true);
      }
    }).result;
    const slot = {
      getSlotElementId: () => 'cycle-slot',
      getAdUnitPath: () => '/example/cycle-slot',
    };
    const emit = (eventType: string, fields: Readonly<Record<string, unknown>> = {}): void => {
      const event = { slot, ...fields };
      for (const listener of ready.listeners.get(eventType) ?? []) listener(event);
    };

    emit('slotRequested');
    emit('slotResponseReceived', { responseIdentifier: 'response-1' });
    emit('slotRenderEnded', { isEmpty: false, responseIdentifier: 'response-1' });
    emit('slotRequested');
    emit('impressionViewable');
    emit('slotRenderEnded', { isEmpty: false, responseIdentifier: 'response-2' });

    expect(facts.map(({ kind, slot: snapshot }) => [kind, snapshot.cycleOrdinal])).toEqual([
      ['slotRequested', 1],
      ['slotResponseReceived', 1],
      ['slotRenderEnded', 1],
      ['slotRequested', 2],
      ['impressionViewable', undefined],
      ['slotRenderEnded', 2],
    ]);
    expect(facts[1]?.responseIdentifier).toBe('response-1');
    expect(facts[2]?.responseIdentifier).toBe('response-1');
    expect(facts[5]?.responseIdentifier).toBe('response-2');
  });

  it.each([
    [9, 1],
    [10, 1],
    [11, undefined],
  ] as const)(
    'retains bounded cycle identity after %i completed requests',
    async (cycleCount, expectedOldOrdinal) => {
      const ready = createReadyGoogletag();
      const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });
      const facts: GoogletagDiagnosticsFact[] = [];
      let lifecycle = { retired: true };
      let accepted: ReturnType<typeof createGoogletagTraceCycleHandle> | undefined;
      adapter.observeDiagnostics?.((fact) => facts.push(fact));
      await adapter.run((gpt) => {
        gpt.subscribe('slotRequested', () => {
          lifecycle.retired = true;
          const current = { retired: false };
          lifecycle = current;
          accepted = createGoogletagTraceCycleHandle(() => current.retired);
          return accepted;
        });
        gpt.subscribe('slotRenderEnded', () => accepted);
        gpt.subscribe('slotOnload', () => undefined, true);
      }).result;
      const slot = { getSlotElementId: () => 'ledger-slot' };
      const emit = (eventType: string, fields: Readonly<Record<string, unknown>> = {}): void => {
        const event = { slot, ...fields };
        for (const listener of ready.listeners.get(eventType) ?? []) listener(event);
      };

      for (let ordinal = 1; ordinal <= cycleCount; ordinal += 1) {
        emit('slotRequested');
        emit('slotRenderEnded', {
          isEmpty: false,
          responseIdentifier: `response-${ordinal}`,
        });
      }
      emit('slotOnload', { responseIdentifier: 'response-1' });

      const requested = facts.filter(({ kind }) => kind === 'slotRequested');
      expect(requested.map(({ slot: snapshot }) => snapshot.cycleOrdinal)).toEqual(
        Array.from({ length: cycleCount }, (_, index) => index + 1)
      );
      expect(facts[facts.length - 1]?.kind).toBe('slotOnload');
      expect(facts[facts.length - 1]?.slot.cycleOrdinal).toBe(expectedOldOrdinal);
    }
  );

  it.each(
    [
      'slotResponseReceived',
      'slotRenderEnded',
      'slotOnload',
      'impressionViewable',
      'slotVisibilityChanged',
    ].flatMap((eventType) =>
      ['before-next-start', 'after-next-start', 'after-next-completion'].map(
        (position) => [eventType, position] as const
      )
    )
  )(
    'attributes an old %s callback %s only from exact retained evidence',
    async (eventType, position) => {
      const ready = createReadyGoogletag();
      const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });
      const facts: GoogletagDiagnosticsFact[] = [];
      const firstState = { retired: false };
      const secondState = { retired: false };
      const first = createGoogletagTraceCycleHandle(() => firstState.retired);
      const second = createGoogletagTraceCycleHandle(() => secondState.retired);
      adapter.observeDiagnostics?.((fact) => facts.push(fact));
      await adapter.run((gpt) => {
        gpt.subscribe('slotRequested', (event) =>
          (event as { cycle: number }).cycle === 1 ? first : second
        );
        gpt.subscribe('slotRenderEnded', (event) =>
          (event as { cycle: number }).cycle === 1 ? first : second
        );
        for (const type of [
          'slotResponseReceived',
          'slotOnload',
          'impressionViewable',
          'slotVisibilityChanged',
        ]) {
          gpt.subscribe(type, () => undefined, true);
        }
      }).result;
      const slot = { getSlotElementId: () => 'callback-order-slot' };
      let emitted = 0;
      const emit = (type: string, fields: Readonly<Record<string, unknown>>): void => {
        emitted += 1;
        const event = { slot, ...fields };
        for (const listener of ready.listeners.get(type) ?? []) listener(event);
      };
      const oldFields = {
        cycle: 1,
        responseIdentifier: 'response-1',
        ...(eventType === 'slotRenderEnded' ? { isEmpty: false } : {}),
        ...(eventType === 'slotVisibilityChanged' ? { inViewPercentage: 25 } : {}),
      };

      emit('slotRequested', { cycle: 1 });
      if (position !== 'before-next-start') {
        emit('slotRenderEnded', {
          cycle: 1,
          isEmpty: false,
          responseIdentifier: 'response-1',
        });
        firstState.retired = true;
        emit('slotRequested', { cycle: 2 });
        if (position === 'after-next-completion') {
          emit('slotRenderEnded', {
            cycle: 2,
            isEmpty: false,
            responseIdentifier: 'response-2',
          });
        }
      }
      emit(eventType, oldFields);

      expect(facts).toHaveLength(emitted);
      expect(facts[facts.length - 1]?.kind).toBe(eventType);
      expect(facts[facts.length - 1]?.slot.cycleOrdinal).toBe(
        eventType === 'slotRenderEnded' && position !== 'before-next-start' ? undefined : 1
      );
    }
  );

  it('rolls back an exact GPT listener when installation replaces the binding', async () => {
    const first = createReadyGoogletag();
    const replacement = createReadyGoogletag();
    const target: { googletag?: unknown } = { googletag: first.googletag };
    const adapter = createBrowserGoogletagAdapter(target);
    first.pubads.addEventListener.mockImplementation((type, listener) => {
      const registered = first.listeners.get(type) ?? new Set();
      registered.add(listener);
      first.listeners.set(type, registered);
      target.googletag = replacement.googletag;
    });
    const operation = adapter.run((gpt) => gpt.subscribe('slotRequested', vi.fn()));

    await expect(operation.result).rejects.toMatchObject({
      code: 'external_artifact_incompatible',
    });
    const installed = first.pubads.addEventListener.mock.calls[0]?.[1];
    expect(first.pubads.removeEventListener).toHaveBeenCalledWith('slotRequested', installed);
    expect(first.listeners.get('slotRequested')?.size).toBe(0);
  });

  it('rolls back a GPT listener when installation disposes and cleanup throws', async () => {
    const ready = createReadyGoogletag();
    const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });
    ready.pubads.addEventListener.mockImplementation((type, listener) => {
      const registered = ready.listeners.get(type) ?? new Set();
      registered.add(listener);
      ready.listeners.set(type, registered);
      adapter.dispose();
    });
    ready.pubads.removeEventListener.mockImplementation((type, listener) => {
      ready.listeners.get(type)?.delete(listener);
      throw new Error('cleanup failed');
    });
    const operation = adapter.run((gpt) => gpt.subscribe('slotRequested', vi.fn()));

    await expect(operation.result).rejects.toMatchObject({ code: 'operation_disposed' });
    expect(ready.pubads.removeEventListener).toHaveBeenCalledTimes(1);
    expect(ready.listeners.get('slotRequested')?.size).toBe(0);
  });

  it.each(['dispose', 'throw'] as const)(
    'rolls back GPT subscription ownership when effect registration must %s',
    async (failure) => {
      const ready = createReadyGoogletag();
      const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });
      const registryError = new Error('effect registry add failed');
      const originalDescriptor = Object.getOwnPropertyDescriptor(Set.prototype, 'add');
      const nativeAdd = Set.prototype.add;
      const existingListeners = new Set<(event: unknown) => void>();
      const failedListeners = new Set<(event: unknown) => void>();
      const existingListener = vi.fn();
      const failedListener = vi.fn();
      ready.listeners.set('existing', existingListeners);
      ready.listeners.set('failed', failedListeners);
      let operation: ReturnType<typeof adapter.run> | undefined;
      try {
        operation = adapter.run((gpt) => {
          gpt.subscribe('existing', existingListener);
          Object.defineProperty(Set.prototype, 'add', {
            configurable: true,
            writable: true,
            value: function (this: Set<unknown>, value: unknown): Set<unknown> {
              if (
                typeof value === 'function' &&
                this !== existingListeners &&
                this !== failedListeners
              ) {
                if (failure === 'dispose') adapter.dispose();
                else throw registryError;
              }
              return Reflect.apply(nativeAdd, this, [value]) as Set<unknown>;
            },
          });
          return gpt.subscribe('failed', failedListener);
        });
      } finally {
        if (originalDescriptor) Object.defineProperty(Set.prototype, 'add', originalDescriptor);
      }

      if (failure === 'dispose') {
        await expect(operation?.result).rejects.toMatchObject({ code: 'operation_disposed' });
      } else {
        await expect(operation?.result).rejects.toBe(registryError);
      }
      adapter.dispose();
      adapter.dispose();

      expect(ready.listeners.get('existing')?.size).toBe(0);
      expect(ready.listeners.get('failed')?.size).toBe(0);
      expect(ready.pubads.removeEventListener).toHaveBeenCalledTimes(2);
    }
  );

  it('settles a live GPT operation and restores exact effects when Set.delete is poisoned', async () => {
    const ready = createReadyGoogletag();
    const nativeSetConfig = ready.googletag.setConfig;
    const nativeDisable = ready.pubads.disableInitialLoad;
    const originalSetDelete = Set.prototype.delete;
    ready.pubads.removeEventListener.mockImplementation((type, listener) => {
      const registered = ready.listeners.get(type);
      if (registered) Reflect.apply(originalSetDelete, registered, [listener]);
    });
    const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });
    const listener = vi.fn();
    const operation = adapter.run((gpt) => {
      gpt.subscribe('slotRequested', listener);
      return new Promise<never>(() => undefined);
    });
    expect(ready.googletag.setConfig).not.toBe(nativeSetConfig);
    expect(ready.pubads.disableInitialLoad).not.toBe(nativeDisable);
    expect(ready.listeners.get('slotRequested')).toHaveLength(1);

    Set.prototype.delete = function (): boolean {
      throw new Error('GPT live cleanup delete failed');
    } as typeof Set.prototype.delete;
    try {
      expect(() => adapter.dispose()).not.toThrow();
    } finally {
      Set.prototype.delete = originalSetDelete;
    }

    await expect(operation.result).rejects.toMatchObject({ code: 'operation_disposed' });
    expect(ready.listeners.get('slotRequested')).toHaveLength(0);
    expect(ready.googletag.setConfig).toBe(nativeSetConfig);
    expect(ready.pubads.disableInitialLoad).toBe(nativeDisable);
    expect(() => adapter.dispose()).not.toThrow();
  });

  it.each(['keys', 'clear'] as const)(
    'restores every GPT effect when Map.%s is poisoned during disposal',
    async (method) => {
      const ready = createReadyGoogletag();
      const nativeSetConfig = ready.googletag.setConfig;
      const nativeDisable = ready.pubads.disableInitialLoad;
      const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });
      const listener = vi.fn();
      await adapter.run((gpt) => gpt.subscribe('slotRequested', listener)).result;
      expect(ready.googletag.setConfig).not.toBe(nativeSetConfig);
      expect(ready.pubads.disableInitialLoad).not.toBe(nativeDisable);
      expect(ready.listeners.get('slotRequested')).toHaveLength(1);

      const originalMapKeys = Map.prototype.keys;
      const originalMapClear = Map.prototype.clear;
      if (method === 'keys') {
        Map.prototype.keys = function (): never {
          throw new Error('GPT initial-load keys failed');
        } as typeof Map.prototype.keys;
      } else {
        Map.prototype.clear = function (): never {
          throw new Error('GPT initial-load clear failed');
        } as typeof Map.prototype.clear;
      }
      try {
        expect(() => adapter.dispose()).not.toThrow();
        expect(() => adapter.dispose()).not.toThrow();
      } finally {
        Map.prototype.keys = originalMapKeys;
        Map.prototype.clear = originalMapClear;
      }

      expect(ready.googletag.setConfig).toBe(nativeSetConfig);
      expect(ready.pubads.disableInitialLoad).toBe(nativeDisable);
      expect(ready.listeners.get('slotRequested')).toHaveLength(0);
      expect(ready.pubads.removeEventListener).toHaveBeenCalledTimes(1);

      const ownershipProbe = createBrowserGoogletagAdapter({ googletag: ready.googletag });
      await ownershipProbe.run((gpt) => gpt.serviceState()).result;
      expect(ready.googletag.setConfig).not.toBe(nativeSetConfig);
      expect(ready.pubads.disableInitialLoad).not.toBe(nativeDisable);
      ownershipProbe.dispose();
      expect(ready.googletag.setConfig).toBe(nativeSetConfig);
      expect(ready.pubads.disableInitialLoad).toBe(nativeDisable);
    }
  );

  it('settles GPT tracker publication failure and rolls back only its new owner', async () => {
    const first = createReadyGoogletag();
    const second = createReadyGoogletag();
    const firstNativeSetConfig = first.googletag.setConfig;
    const firstNativeDisable = first.pubads.disableInitialLoad;
    const secondNativeSetConfig = second.googletag.setConfig;
    const secondNativeDisable = second.pubads.disableInitialLoad;
    const target: { googletag?: unknown } = { googletag: first.googletag };
    const adapter = createBrowserGoogletagAdapter(target);
    const priorListener = vi.fn();
    await adapter.run((gpt) => gpt.subscribe('prior', priorListener)).result;
    expect(first.listeners.get('prior')).toHaveLength(1);

    target.googletag = second.googletag;
    const registryError = new Error('GPT tracker effect publication failed');
    const command = vi.fn();
    const originalSetAdd = Set.prototype.add;
    let additions = 0;
    Set.prototype.add = function (this: Set<unknown>, value: unknown): Set<unknown> {
      additions += 1;
      const added = Reflect.apply(originalSetAdd, this, [value]) as Set<unknown>;
      if (additions === 3) throw registryError;
      return added;
    } as typeof Set.prototype.add;

    let operation: ReturnType<typeof adapter.run> | undefined;
    let thrown: unknown;
    try {
      operation = adapter.run(command);
    } catch (error) {
      thrown = error;
    } finally {
      Set.prototype.add = originalSetAdd;
    }

    expect(additions).toBe(3);
    expect(thrown).toBeUndefined();
    if (!operation) throw new Error('Expected a published GPT operation');
    await expect(operation.result).rejects.toBe(registryError);
    expect(command).not.toHaveBeenCalled();
    expect(first.listeners.get('prior')).toHaveLength(1);
    expect(first.googletag.setConfig).toBe(firstNativeSetConfig);
    expect(first.pubads.disableInitialLoad).toBe(firstNativeDisable);
    expect(second.googletag.setConfig).toBe(secondNativeSetConfig);
    expect(second.pubads.disableInitialLoad).toBe(secondNativeDisable);

    const ownerProbe = createBrowserGoogletagAdapter({ googletag: second.googletag });
    await ownerProbe.run((gpt) => gpt.serviceState()).result;
    expect(second.googletag.setConfig).not.toBe(secondNativeSetConfig);
    expect(second.pubads.disableInitialLoad).not.toBe(secondNativeDisable);
    ownerProbe.dispose();
    expect(second.googletag.setConfig).toBe(secondNativeSetConfig);
    expect(second.pubads.disableInitialLoad).toBe(secondNativeDisable);

    target.googletag = undefined;
    const recovered = Array.from({ length: 64 }, () => adapter.run(() => 'recovered'));
    expect(() => adapter.run(vi.fn())).toThrowError(
      expect.objectContaining({ code: 'external_queue_full' })
    );
    target.googletag = second.googletag;
    adapter.notifyReady();
    await expect(Promise.all(recovered.map(({ result }) => result))).resolves.toEqual(
      Array.from({ length: 64 }, () => 'recovered')
    );
    expect(first.listeners.get('prior')).toHaveLength(1);

    adapter.dispose();
    expect(first.listeners.get('prior')).toHaveLength(0);
    expect(first.pubads.removeEventListener).toHaveBeenCalledTimes(1);
    expect(second.googletag.setConfig).toBe(secondNativeSetConfig);
    expect(second.pubads.disableInitialLoad).toBe(secondNativeDisable);
  });

  it.each(['owner', 'release', 'service'] as const)(
    'settles and rolls back GPT tracking when the %s registry has lookup throws',
    async (registry) => {
      const ready = createReadyGoogletag();
      const nativeSetConfig = ready.googletag.setConfig;
      const nativeDisable = ready.pubads.disableInitialLoad;
      const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });
      const command = vi.fn(() => 'settled');
      const originalSetHas = Set.prototype.has;
      const originalMapHas = Map.prototype.has;
      const originalWeakMapHas = WeakMap.prototype.has;
      if (registry === 'owner') {
        Set.prototype.has = function (): boolean {
          throw new Error('GPT owner lookup failed');
        } as typeof Set.prototype.has;
      } else if (registry === 'release') {
        Map.prototype.has = function (): boolean {
          throw new Error('GPT release lookup failed');
        } as typeof Map.prototype.has;
      } else {
        WeakMap.prototype.has = function (): boolean {
          throw new Error('GPT service lookup failed');
        } as typeof WeakMap.prototype.has;
      }

      let operation: ReturnType<typeof adapter.run> | undefined;
      let thrown: unknown;
      try {
        operation = adapter.run(command);
      } catch (error) {
        thrown = error;
      } finally {
        Set.prototype.has = originalSetHas;
        Map.prototype.has = originalMapHas;
        WeakMap.prototype.has = originalWeakMapHas;
      }

      expect(thrown).toBeUndefined();
      if (!operation) throw new Error('Expected a published GPT operation');
      await expect(operation.result).resolves.toBe('settled');
      expect(command).toHaveBeenCalledTimes(1);
      expect(ready.googletag.setConfig).toBe(nativeSetConfig);
      expect(ready.pubads.disableInitialLoad).toBe(nativeDisable);

      const ownerProbe = createBrowserGoogletagAdapter({ googletag: ready.googletag });
      await ownerProbe.run((gpt) => gpt.serviceState()).result;
      ownerProbe.dispose();
      expect(ready.googletag.setConfig).toBe(nativeSetConfig);
      expect(ready.pubads.disableInitialLoad).toBe(nativeDisable);
      adapter.dispose();
    }
  );

  it.each(['before', 'after'] as const)(
    'rolls back shared GPT tracker publication when WeakMap.set throws %s insertion',
    async (failure) => {
      vi.useFakeTimers();
      const ready = createReadyGoogletag();
      const nativeSetConfig = ready.googletag.setConfig;
      const nativeDisable = ready.pubads.disableInitialLoad;
      const target: { googletag?: unknown } = { googletag: ready.googletag };
      const adapter = createBrowserGoogletagAdapter(target);
      const publicationError = new Error(`shared tracker failed ${failure} insertion`);
      const command = vi.fn();
      const originalWeakMapSet = WeakMap.prototype.set;
      const originalWeakMapDelete = WeakMap.prototype.delete;
      let publications = 0;
      WeakMap.prototype.set = function (
        this: WeakMap<object, unknown>,
        key: object,
        value: unknown
      ): WeakMap<object, unknown> {
        publications += 1;
        if (publications === 1) {
          if (failure === 'after') Reflect.apply(originalWeakMapSet, this, [key, value]);
          throw publicationError;
        }
        return Reflect.apply(originalWeakMapSet, this, [key, value]) as WeakMap<object, unknown>;
      } as typeof WeakMap.prototype.set;
      WeakMap.prototype.delete = function (): boolean {
        throw new Error('shared tracker rollback delete failed');
      } as typeof WeakMap.prototype.delete;

      let operation: ReturnType<typeof adapter.run> | undefined;
      let thrown: unknown;
      try {
        operation = adapter.run(command);
      } catch (error) {
        thrown = error;
      } finally {
        WeakMap.prototype.set = originalWeakMapSet;
        WeakMap.prototype.delete = originalWeakMapDelete;
      }

      expect(thrown).toBeUndefined();
      expect(publications).toBe(1);
      if (!operation) throw new Error('Expected a published GPT operation');
      await expect(operation.result).rejects.toBe(publicationError);
      expect(command).not.toHaveBeenCalled();
      expect(ready.googletag.setConfig).toBe(nativeSetConfig);
      expect(ready.pubads.disableInitialLoad).toBe(nativeDisable);

      const ownerProbe = createBrowserGoogletagAdapter({ googletag: ready.googletag });
      let recoveredPublications = 0;
      WeakMap.prototype.set = function (
        this: WeakMap<object, unknown>,
        key: object,
        value: unknown
      ): WeakMap<object, unknown> {
        recoveredPublications += 1;
        return Reflect.apply(originalWeakMapSet, this, [key, value]) as WeakMap<object, unknown>;
      } as typeof WeakMap.prototype.set;
      try {
        await ownerProbe.run((gpt) => gpt.serviceState()).result;
      } finally {
        WeakMap.prototype.set = originalWeakMapSet;
      }
      expect(recoveredPublications).toBe(2);
      ownerProbe.dispose();
      expect(ready.googletag.setConfig).toBe(nativeSetConfig);
      expect(ready.pubads.disableInitialLoad).toBe(nativeDisable);

      target.googletag = undefined;
      const recovered = Array.from({ length: 64 }, () => adapter.run(vi.fn()));
      const recoveredResults = recovered.map(({ result }) => result.catch((error) => error));
      expect(() => adapter.run(vi.fn())).toThrowError(
        expect.objectContaining({ code: 'external_queue_full' })
      );
      for (const recoveredOperation of recovered) recoveredOperation.dispose();
      await Promise.all(recoveredResults);
      expect(vi.getTimerCount()).toBe(0);
      adapter.dispose();
    }
  );

  it.each(['before', 'after'] as const)(
    'removes only the new GPT owner when its release Map.set throws %s insertion',
    async (failure) => {
      vi.useFakeTimers();
      const ready = createReadyGoogletag();
      const nativeSetConfig = ready.googletag.setConfig;
      const nativeDisable = ready.pubads.disableInitialLoad;
      const first = createBrowserGoogletagAdapter({ googletag: ready.googletag });
      await first.run((gpt) => gpt.serviceState()).result;
      const sharedSetConfig = ready.googletag.setConfig;
      const sharedDisable = ready.pubads.disableInitialLoad;
      const secondTarget: { googletag?: unknown } = { googletag: ready.googletag };
      const second = createBrowserGoogletagAdapter(secondTarget);
      const publicationError = new Error(`release map failed ${failure} insertion`);
      const command = vi.fn();
      const originalMapSet = Map.prototype.set;
      const originalMapDelete = Map.prototype.delete;
      let publications = 0;
      Map.prototype.set = function (
        this: Map<unknown, unknown>,
        key: unknown,
        value: unknown
      ): Map<unknown, unknown> {
        publications += 1;
        if (publications === 1) {
          if (failure === 'after') Reflect.apply(originalMapSet, this, [key, value]);
          throw publicationError;
        }
        return Reflect.apply(originalMapSet, this, [key, value]) as Map<unknown, unknown>;
      } as typeof Map.prototype.set;
      Map.prototype.delete = function (): boolean {
        throw new Error('release map rollback delete failed');
      } as typeof Map.prototype.delete;

      let operation: ReturnType<typeof second.run> | undefined;
      let thrown: unknown;
      try {
        operation = second.run(command);
      } catch (error) {
        thrown = error;
      } finally {
        Map.prototype.set = originalMapSet;
        Map.prototype.delete = originalMapDelete;
      }

      expect(thrown).toBeUndefined();
      expect(publications).toBe(1);
      if (!operation) throw new Error('Expected a published GPT operation');
      await expect(operation.result).rejects.toBe(publicationError);
      expect(command).not.toHaveBeenCalled();
      expect(ready.googletag.setConfig).toBe(sharedSetConfig);
      expect(ready.pubads.disableInitialLoad).toBe(sharedDisable);

      first.dispose();
      expect(ready.googletag.setConfig).toBe(nativeSetConfig);
      expect(ready.pubads.disableInitialLoad).toBe(nativeDisable);

      secondTarget.googletag = undefined;
      const recovered = Array.from({ length: 64 }, () => second.run(vi.fn()));
      const recoveredResults = recovered.map(({ result }) => result.catch((error) => error));
      expect(() => second.run(vi.fn())).toThrowError(
        expect.objectContaining({ code: 'external_queue_full' })
      );
      for (const recoveredOperation of recovered) recoveredOperation.dispose();
      await Promise.all(recoveredResults);
      expect(vi.getTimerCount()).toBe(0);
      second.dispose();
      expect(ready.googletag.setConfig).toBe(nativeSetConfig);
      expect(ready.pubads.disableInitialLoad).toBe(nativeDisable);
    }
  );

  it.each(['before', 'after'] as const)(
    'identity-restores GPT service publication when WeakMap.set throws %s insertion',
    async (failure) => {
      const ready = createReadyGoogletag();
      const nativeSetConfig = ready.googletag.setConfig;
      const nativeDisable = ready.pubads.disableInitialLoad;
      const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });
      const publicationError = new Error(`service map failed ${failure} insertion`);
      const command = vi.fn();
      const originalWeakMapSet = WeakMap.prototype.set;
      const originalWeakMapDelete = WeakMap.prototype.delete;
      let publications = 0;
      WeakMap.prototype.set = function (
        this: WeakMap<object, unknown>,
        key: object,
        value: unknown
      ): WeakMap<object, unknown> {
        publications += 1;
        if (publications === 2) {
          if (failure === 'after') Reflect.apply(originalWeakMapSet, this, [key, value]);
          throw publicationError;
        }
        return Reflect.apply(originalWeakMapSet, this, [key, value]) as WeakMap<object, unknown>;
      } as typeof WeakMap.prototype.set;
      WeakMap.prototype.delete = function (): boolean {
        throw new Error('service map rollback delete failed');
      } as typeof WeakMap.prototype.delete;

      let operation: ReturnType<typeof adapter.run> | undefined;
      let thrown: unknown;
      try {
        operation = adapter.run(command);
      } catch (error) {
        thrown = error;
      } finally {
        WeakMap.prototype.set = originalWeakMapSet;
        WeakMap.prototype.delete = originalWeakMapDelete;
      }

      expect(thrown).toBeUndefined();
      expect(publications).toBe(2);
      if (!operation) throw new Error('Expected a published GPT operation');
      await expect(operation.result).rejects.toBe(publicationError);
      expect(command).not.toHaveBeenCalled();
      expect(ready.googletag.setConfig).toBe(nativeSetConfig);
      expect(ready.pubads.disableInitialLoad).toBe(nativeDisable);

      const ownerProbe = createBrowserGoogletagAdapter({ googletag: ready.googletag });
      await ownerProbe.run((gpt) => gpt.serviceState()).result;
      ownerProbe.dispose();
      expect(ready.googletag.setConfig).toBe(nativeSetConfig);
      expect(ready.pubads.disableInitialLoad).toBe(nativeDisable);
      adapter.dispose();
    }
  );

  it.each(['before', 'after'] as const)(
    'rolls back GPT tracker owner publication when Set.add throws %s insertion',
    async (failure) => {
      const ready = createReadyGoogletag();
      const nativeSetConfig = ready.googletag.setConfig;
      const nativeDisable = ready.pubads.disableInitialLoad;
      const first = createBrowserGoogletagAdapter({ googletag: ready.googletag });
      await first.run((gpt) => gpt.serviceState()).result;
      const sharedSetConfig = ready.googletag.setConfig;
      const sharedDisable = ready.pubads.disableInitialLoad;
      const second = createBrowserGoogletagAdapter({ googletag: ready.googletag });
      const registryError = new Error(`owner add failed ${failure} insertion`);
      const command = vi.fn();
      const originalSetAdd = Set.prototype.add;
      const originalSetDelete = Set.prototype.delete;
      let additions = 0;
      Set.prototype.add = function (this: Set<unknown>, value: unknown): Set<unknown> {
        additions += 1;
        if (additions === 2) {
          if (failure === 'after') Reflect.apply(originalSetAdd, this, [value]);
          throw registryError;
        }
        return Reflect.apply(originalSetAdd, this, [value]) as Set<unknown>;
      } as typeof Set.prototype.add;
      Set.prototype.delete = function (): boolean {
        throw new Error('owner rollback delete failed');
      } as typeof Set.prototype.delete;
      let operation: ReturnType<typeof second.run> | undefined;
      let thrown: unknown;
      try {
        operation = second.run(command);
      } catch (error) {
        thrown = error;
      } finally {
        Set.prototype.add = originalSetAdd;
        Set.prototype.delete = originalSetDelete;
      }

      expect(additions).toBe(2);
      expect(thrown).toBeUndefined();
      if (!operation) throw new Error('Expected a published GPT operation');
      await expect(operation.result).rejects.toBe(registryError);
      expect(command).not.toHaveBeenCalled();
      expect(ready.googletag.setConfig).toBe(sharedSetConfig);
      expect(ready.pubads.disableInitialLoad).toBe(sharedDisable);

      second.dispose();
      expect(ready.googletag.setConfig).toBe(sharedSetConfig);
      expect(ready.pubads.disableInitialLoad).toBe(sharedDisable);
      first.dispose();
      expect(ready.googletag.setConfig).toBe(nativeSetConfig);
      expect(ready.pubads.disableInitialLoad).toBe(nativeDisable);
    }
  );

  it.each(['before', 'after'] as const)(
    'identity-restores GPT root wrapper when restorer Set.add throws %s insertion',
    async (failure) => {
      const ready = createReadyGoogletag();
      const nativeSetConfig = ready.googletag.setConfig;
      const nativeDisable = ready.pubads.disableInitialLoad;
      const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });
      const registryError = new Error(`root restorer add failed ${failure} insertion`);
      const command = vi.fn();
      const originalSetAdd = Set.prototype.add;
      const originalSetDelete = Set.prototype.delete;
      let additions = 0;
      Set.prototype.add = function (this: Set<unknown>, value: unknown): Set<unknown> {
        additions += 1;
        if (additions === 4) {
          if (failure === 'after') Reflect.apply(originalSetAdd, this, [value]);
          throw registryError;
        }
        return Reflect.apply(originalSetAdd, this, [value]) as Set<unknown>;
      } as typeof Set.prototype.add;
      Set.prototype.delete = function (): boolean {
        throw new Error('root restorer cleanup delete failed');
      } as typeof Set.prototype.delete;
      let operation: ReturnType<typeof adapter.run> | undefined;
      let thrown: unknown;
      try {
        operation = adapter.run(command);
      } catch (error) {
        thrown = error;
      } finally {
        Set.prototype.add = originalSetAdd;
        Set.prototype.delete = originalSetDelete;
      }

      expect(additions).toBe(4);
      expect(thrown).toBeUndefined();
      if (!operation) throw new Error('Expected a published GPT operation');
      await expect(operation.result).rejects.toBe(registryError);
      expect(command).not.toHaveBeenCalled();
      expect(ready.googletag.setConfig).toBe(nativeSetConfig);
      expect(ready.pubads.disableInitialLoad).toBe(nativeDisable);

      const ownerProbe = createBrowserGoogletagAdapter({ googletag: ready.googletag });
      await ownerProbe.run((gpt) => gpt.serviceState()).result;
      ownerProbe.dispose();
      expect(ready.googletag.setConfig).toBe(nativeSetConfig);
      expect(ready.pubads.disableInitialLoad).toBe(nativeDisable);
      adapter.dispose();
    }
  );

  it.each(['before', 'after'] as const)(
    'identity-restores GPT service wrapper when restorer Set.add throws %s insertion',
    async (failure) => {
      const ready = createReadyGoogletag();
      const nativeSetConfig = ready.googletag.setConfig;
      const nativeDisable = ready.pubads.disableInitialLoad;
      const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });
      const registryError = new Error(`service restorer add failed ${failure} insertion`);
      const command = vi.fn();
      const originalSetAdd = Set.prototype.add;
      const originalSetDelete = Set.prototype.delete;
      let additions = 0;
      Set.prototype.add = function (this: Set<unknown>, value: unknown): Set<unknown> {
        additions += 1;
        if (additions === 5) {
          if (failure === 'after') Reflect.apply(originalSetAdd, this, [value]);
          throw registryError;
        }
        return Reflect.apply(originalSetAdd, this, [value]) as Set<unknown>;
      } as typeof Set.prototype.add;
      Set.prototype.delete = function (): boolean {
        throw new Error('service restorer cleanup delete failed');
      } as typeof Set.prototype.delete;
      let operation: ReturnType<typeof adapter.run> | undefined;
      let thrown: unknown;
      try {
        operation = adapter.run(command);
      } catch (error) {
        thrown = error;
      } finally {
        Set.prototype.add = originalSetAdd;
        Set.prototype.delete = originalSetDelete;
      }

      expect(additions).toBe(5);
      expect(thrown).toBeUndefined();
      if (!operation) throw new Error('Expected a published GPT operation');
      await expect(operation.result).rejects.toBe(registryError);
      expect(command).not.toHaveBeenCalled();
      expect(ready.googletag.setConfig).toBe(nativeSetConfig);
      expect(ready.pubads.disableInitialLoad).toBe(nativeDisable);

      const ownerProbe = createBrowserGoogletagAdapter({ googletag: ready.googletag });
      await ownerProbe.run((gpt) => gpt.serviceState()).result;
      ownerProbe.dispose();
      expect(ready.googletag.setConfig).toBe(nativeSetConfig);
      expect(ready.pubads.disableInitialLoad).toBe(nativeDisable);
      adapter.dispose();
    }
  );

  it('rolls back a failed GPT command subscription without touching prior global effects', async () => {
    const ready = createReadyGoogletag();
    const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });
    const priorListener = vi.fn();
    const failedListener = vi.fn();
    const commandError = new Error('command failed');
    await adapter.run((gpt) => gpt.subscribe('prior', priorListener)).result;

    const operation = adapter.run((gpt) => {
      gpt.subscribe('failed', failedListener);
      throw commandError;
    });

    await expect(operation.result).rejects.toBe(commandError);
    expect(ready.listeners.get('prior')?.size).toBe(1);
    expect(ready.listeners.get('failed')?.size).toBe(0);
    expect(() => [...(ready.listeners.get('prior') ?? [])][0]?.({})).not.toThrow();
    expect(priorListener).toHaveBeenCalledTimes(1);
    expect(failedListener).not.toHaveBeenCalled();

    adapter.dispose();
    expect(ready.listeners.get('prior')?.size).toBe(0);
    expect(ready.pubads.removeEventListener).toHaveBeenCalledTimes(2);
  });

  it('promotes fulfilled GPT command subscriptions and rolls back rejected ones', async () => {
    const ready = createReadyGoogletag();
    const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });
    const rejection = new Error('async command failed');
    const rejected = adapter.run((gpt) => {
      gpt.subscribe('rejected', vi.fn());
      return Promise.reject(rejection);
    });

    await expect(rejected.result).rejects.toBe(rejection);
    expect(ready.listeners.get('rejected')?.size).toBe(0);

    const fulfilled = adapter.run((gpt) => {
      gpt.subscribe('fulfilled', vi.fn());
      return Promise.resolve('complete');
    });
    await expect(fulfilled.result).resolves.toBe('complete');
    expect(ready.listeners.get('fulfilled')?.size).toBe(1);

    adapter.dispose();
    expect(ready.listeners.get('fulfilled')?.size).toBe(0);
  });

  it.each(['dispose', 'replacement'] as const)(
    'rolls back a provisional GPT subscription after async %s',
    async (failure) => {
      const first = createReadyGoogletag();
      const replacement = createReadyGoogletag();
      const target: { googletag?: unknown } = { googletag: first.googletag };
      const adapter = createBrowserGoogletagAdapter(target);
      let resolveCommand!: (value: string) => void;
      const commandResult = new Promise<string>((resolve) => {
        resolveCommand = resolve;
      });
      const operation = adapter.run((gpt) => {
        gpt.subscribe('provisional', vi.fn());
        return commandResult;
      });

      if (failure === 'dispose') adapter.dispose();
      else target.googletag = replacement.googletag;
      resolveCommand('late-success');

      await expect(operation.result).rejects.toMatchObject({
        code: failure === 'dispose' ? 'operation_disposed' : 'external_artifact_incompatible',
      });
      expect(first.listeners.get('provisional')?.size).toBe(0);
    }
  );

  it('contains command-queue and command-callback throws', async () => {
    const pushError = new Error('push failed');
    const callbackError = new Error('callback failed');
    const throwingPush = {
      apiReady: true,
      pubadsReady: true,
      cmd: {
        push: () => {
          throw pushError;
        },
      },
      display: vi.fn(),
      pubads: () => createReadyGoogletag().pubads,
    };
    const pushAdapter = createBrowserGoogletagAdapter({ googletag: throwingPush });

    let pushOperation: ReturnType<typeof pushAdapter.run> | undefined;
    expect(() => {
      pushOperation = pushAdapter.run(() => undefined);
    }).not.toThrow();
    await expect(pushOperation?.result).rejects.toBe(pushError);

    const deferred = createReadyGoogletag({ deferCommands: true });
    const callbackAdapter = createBrowserGoogletagAdapter({ googletag: deferred.googletag });
    const callbackOperation = callbackAdapter.run(() => {
      throw callbackError;
    });
    expect(() => deferred.commands[0]?.()).not.toThrow();
    await expect(callbackOperation.result).rejects.toBe(callbackError);
  });

  it('owns GPT subscriptions, refresh, targeting, and service inspection behind the facade', async () => {
    const ready = createReadyGoogletag({ initialLoadDisabled: true });
    const targeting = new Map<string, string[]>();
    const slot = {
      clearTargeting: vi.fn((key?: string) => {
        if (key === undefined) targeting.clear();
        else targeting.delete(key);
      }),
      getAdUnitPath: vi.fn(() => '/publisher/example'),
      getTargeting: vi.fn((key: string) => targeting.get(key) ?? []),
      setTargeting: vi.fn((key: string, value: string | readonly string[]) => {
        targeting.set(key, typeof value === 'string' ? [value] : [...value]);
      }),
    };
    ready.pubads.getSlots.mockReturnValue([slot]);
    const listener = vi.fn(() => {
      throw new Error('publisher callback failed');
    });
    const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });
    const operation = adapter.run((gpt) => {
      const unsubscribe = gpt.subscribe('slotRequested', listener);
      gpt.setTargeting(slot, 'hb_adid', 'reservation');
      expect(gpt.getTargeting(slot, 'hb_adid')).toEqual(['reservation']);
      expect(gpt.adUnitPath?.(slot)).toBe('/publisher/example');
      gpt.refresh([slot], { changeCorrelator: false });
      expect(gpt.slots()).toEqual([slot]);
      expect(Object.isFrozen(gpt.slots())).toBe(true);
      expect(gpt.serviceState()).toEqual({
        apiReady: true,
        initialLoadDisabled: true,
        pubadsReady: true,
      });
      const installed = [...(ready.listeners.get('slotRequested') ?? [])][0];
      expect(() => installed?.({ slot })).not.toThrow();
      unsubscribe();
      gpt.clearTargeting(slot, 'hb_adid');
    });

    await expect(operation.result).resolves.toBeUndefined();
    expect(listener).toHaveBeenCalledWith({ slot });
    expect(ready.pubads.refresh).toHaveBeenCalledWith([slot], { changeCorrelator: false });
    expect(ready.pubads.removeEventListener).toHaveBeenCalledTimes(1);
    expect(targeting.has('hb_adid')).toBe(false);
  });

  it('exposes one reversible publisher-call observer without changing ordinary calls', () => {
    const ready = createReadyGoogletag();
    const nativeDisplay = ready.googletag.display;
    const nativeRefresh = ready.pubads.refresh;
    const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });
    const boundary = adapter as unknown as {
      observePublisherCalls?: (observer: object) => () => void;
    };

    expect(boundary.observePublisherCalls).toBeTypeOf('function');
    if (!boundary.observePublisherCalls) return;

    const release = boundary.observePublisherCalls(Object.freeze({}));
    expect(ready.googletag.display).not.toBe(nativeDisplay);
    expect(ready.pubads.refresh).not.toBe(nativeRefresh);

    release();
    expect(ready.googletag.display).toBe(nativeDisplay);
    expect(ready.pubads.refresh).toBe(nativeRefresh);
  });

  it('keeps an existing event subscription live while installing publisher-call wrappers', async () => {
    const ready = createReadyGoogletag();
    const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });
    const listener = vi.fn();
    let unsubscribe: (() => void) | undefined;
    const subscription = adapter.run((gpt) => {
      unsubscribe = gpt.subscribe('slotRequested', listener);
    });
    await expect(subscription.result).resolves.toBeUndefined();
    const installed = [...(ready.listeners.get('slotRequested') ?? [])][0];
    expect(installed).toBeTypeOf('function');

    const releasePublisherObserver = adapter.observePublisherCalls(Object.freeze({}));
    installed?.({ slot: Object.freeze({ id: 'slot-a' }) });

    expect(listener).toHaveBeenCalledOnce();
    unsubscribe?.();
    releasePublisherObserver();
  });

  it('installs the publisher observer when an accepted command-queue stub becomes ready', () => {
    const commands: Array<() => void> = [];
    const pending = {
      cmd: {
        push: vi.fn((callback: () => void) => {
          commands.push(callback);
          return commands.length;
        }),
      },
    };
    const target = { googletag: pending as object };
    const adapter = createBrowserGoogletagAdapter(target);
    const handoff = {};
    const release = adapter.observePublisherCalls({
      defineSlot: () => Object.freeze({ action: 'handoff', slot: handoff }),
    });
    const ready = createReadyGoogletag();
    const nativeDefineSlot = vi.fn((_path: string, _sizes: unknown, _elementId: string) => ({}));
    Object.assign(pending, {
      apiReady: true,
      defineSlot: nativeDefineSlot,
      destroySlots: ready.googletag.destroySlots,
      display: ready.googletag.display,
      getConfig: ready.googletag.getConfig,
      pubads: ready.googletag.pubads,
      pubadsReady: true,
      setConfig: ready.googletag.setConfig,
    });

    expect(commands).toHaveLength(1);
    commands[0]?.();
    const defineSlot = (pending as typeof pending & { defineSlot: typeof nativeDefineSlot })
      .defineSlot;
    expect(defineSlot).not.toBe(nativeDefineSlot);
    expect(defineSlot('/publisher', [300, 250], 'slot')).toBe(handoff);
    expect(nativeDefineSlot).not.toHaveBeenCalled();

    release();
    expect((pending as typeof pending & { defineSlot: typeof nativeDefineSlot }).defineSlot).toBe(
      nativeDefineSlot
    );
  });

  it('does not classify facade-driven GPT calls as publisher calls', async () => {
    const ready = createReadyGoogletag();
    const nativeRefresh = ready.pubads.refresh;
    const observer = {
      display: vi.fn(() => Object.freeze({ action: 'suppress' as const })),
      refresh: vi.fn(() => Object.freeze({ action: 'suppress' as const })),
    };
    const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });
    adapter.observePublisherCalls(observer);

    await expect(
      adapter.run((gpt) => {
        gpt.display('trusted-slot');
        gpt.refresh([], { changeCorrelator: false });
      }).result
    ).resolves.toBeUndefined();

    expect(observer.display).not.toHaveBeenCalled();
    expect(observer.refresh).not.toHaveBeenCalled();
    expect(ready.display).toHaveBeenCalledExactlyOnceWith('trusted-slot');
    expect(nativeRefresh).toHaveBeenCalledExactlyOnceWith([], {
      changeCorrelator: false,
    });
  });

  it('observes publisher GPT calls reentered by one facade-driven native display', async () => {
    const ready = createReadyGoogletag();
    const slot = Object.freeze({ id: 'nested-publisher-slot' });
    ready.pubads.getSlots.mockReturnValue([slot]);
    ready.googletag.defineSlot.mockReturnValue(slot);
    ready.googletag.destroySlots.mockReturnValue(true);
    ready.display.mockImplementation(() => {
      ready.pubads.refresh([slot], { changeCorrelator: true });
      ready.googletag.defineSlot('/publisher', [300, 250], 'nested-slot');
      ready.googletag.destroySlots([slot]);
    });
    const observer = {
      defineSlot: vi.fn(() => Object.freeze({ action: 'forward' as const })),
      destroySlots: vi.fn(),
      display: vi.fn(() => Object.freeze({ action: 'forward' as const })),
      refresh: vi.fn(() => Object.freeze({ action: 'forward' as const })),
    };
    const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });
    adapter.observePublisherCalls(observer);

    await expect(adapter.run((gpt) => gpt.display('trusted-slot')).result).resolves.toBeUndefined();

    expect(observer.display).not.toHaveBeenCalled();
    expect(observer.refresh).toHaveBeenCalledExactlyOnceWith({
      options: { changeCorrelator: true },
      requestedSlots: [slot],
      slots: [slot],
    });
    expect(observer.defineSlot).toHaveBeenCalledExactlyOnceWith({
      adUnitPath: '/publisher',
      elementId: 'nested-slot',
      initialLoadDisabled: false,
      sizes: [300, 250],
    });
    expect(observer.destroySlots).toHaveBeenCalledExactlyOnceWith({ slots: [slot] });
  });

  it('observes a publisher wrapper call made inside a TS command but outside a facade invocation', async () => {
    const ready = createReadyGoogletag();
    const observer = {
      defineSlot: vi.fn(() => Object.freeze({ action: 'forward' as const })),
      display: vi.fn(() => Object.freeze({ action: 'forward' as const })),
    };
    const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });
    adapter.observePublisherCalls(observer);

    await expect(
      adapter.run((gpt) => {
        ready.googletag.defineSlot('/publisher', [300, 250], 'publisher-inside-command');
        gpt.display('trusted-slot');
      }).result
    ).resolves.toBeUndefined();

    expect(observer.defineSlot).toHaveBeenCalledExactlyOnceWith({
      adUnitPath: '/publisher',
      elementId: 'publisher-inside-command',
      initialLoadDisabled: false,
      sizes: [300, 250],
    });
    expect(observer.display).not.toHaveBeenCalled();
  });

  it('commits publisher display and refresh admissions only after exact native returns', () => {
    const ready = createReadyGoogletag();
    const slot = Object.freeze({ id: 'publisher-slot' });
    const receiver = Object.freeze({ publisher: true });
    const refreshOptions = Object.freeze({ changeCorrelator: false, publisher: 'exact' });
    const order: string[] = [];
    const displayAdmission = Object.freeze({
      commit: vi.fn(() => order.push('commit:display')),
      rollback: vi.fn(),
    });
    const refreshAdmission = Object.freeze({
      commit: vi.fn(() => order.push('commit:refresh')),
      rollback: vi.fn(),
    });
    const nativeDisplay = vi.fn(function (this: unknown, ...arguments_: unknown[]) {
      order.push('native:display');
      return Object.freeze({ arguments_, receiver: this });
    });
    const nativeRefresh = vi.fn(function (this: unknown, ...arguments_: unknown[]) {
      order.push('native:refresh');
      return Object.freeze({ arguments_, receiver: this });
    });
    ready.googletag.display = nativeDisplay;
    ready.pubads.refresh = nativeRefresh;
    const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });
    adapter.observePublisherCalls({
      display: () => Object.freeze({ action: 'forward' as const, admission: displayAdmission }),
      refresh: () => Object.freeze({ action: 'forward' as const, admission: refreshAdmission }),
    });

    const display = ready.googletag.display as (...arguments_: unknown[]) => unknown;
    expect(Reflect.apply(display, receiver, ['slot'])).toEqual({
      arguments_: ['slot'],
      receiver,
    });
    const refresh = ready.pubads.refresh as (...arguments_: unknown[]) => unknown;
    expect(Reflect.apply(refresh, receiver, [[slot], refreshOptions])).toEqual({
      arguments_: [[slot], refreshOptions],
      receiver,
    });

    expect(order).toEqual(['native:display', 'commit:display', 'native:refresh', 'commit:refresh']);
    expect(displayAdmission.rollback).not.toHaveBeenCalled();
    expect(refreshAdmission.rollback).not.toHaveBeenCalled();
  });

  it('defers one explicit refresh and forwards the complete snapshot with exact options once', async () => {
    const ready = createReadyGoogletag();
    const first = Object.freeze({ id: 'first' });
    const second = Object.freeze({ id: 'second' });
    const options = Object.freeze({ changeCorrelator: false, publisher: 'exact-options' });
    const originalSlots = [first, second];
    let complete!: () => void;
    const completion = new Promise<void>((resolve) => {
      complete = resolve;
    });
    const admission = Object.freeze({ commit: vi.fn(), rollback: vi.fn() });
    const nativeRefresh = vi.fn();
    ready.pubads.refresh = nativeRefresh;
    const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });
    adapter.observePublisherCalls({
      refresh: () =>
        Object.freeze({
          action: 'defer' as const,
          admission,
          completion,
          slots: Object.freeze([first, second]),
        }),
    });

    expect(ready.pubads.refresh(originalSlots, options)).toBeUndefined();
    originalSlots.length = 0;
    expect(nativeRefresh).not.toHaveBeenCalled();

    complete();
    await completion;
    await Promise.resolve();
    expect(nativeRefresh).toHaveBeenCalledExactlyOnceWith([first, second], options);
    expect(admission.commit).toHaveBeenCalledOnce();
    expect(admission.rollback).not.toHaveBeenCalled();
    await Promise.resolve();
    expect(nativeRefresh).toHaveBeenCalledOnce();
  });

  it('forwards a deferred global refresh exactly once when its observer is released', async () => {
    const ready = createReadyGoogletag();
    const first = Object.freeze({ id: 'first' });
    const second = Object.freeze({ id: 'second' });
    const options = Object.freeze({ changeCorrelator: true });
    ready.pubads.getSlots.mockReturnValue([first, second]);
    let complete!: () => void;
    const completion = new Promise<void>((resolve) => {
      complete = resolve;
    });
    const nativeRefresh = vi.fn();
    ready.pubads.refresh = nativeRefresh;
    const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });
    const release = adapter.observePublisherCalls({
      refresh: () =>
        Object.freeze({
          action: 'defer' as const,
          completion,
          slots: Object.freeze([first, second]),
        }),
    });

    ready.pubads.refresh(undefined, options);
    expect(nativeRefresh).not.toHaveBeenCalled();
    release();
    expect(nativeRefresh).toHaveBeenCalledExactlyOnceWith([first, second], options);

    complete();
    await completion;
    await Promise.resolve();
    expect(nativeRefresh).toHaveBeenCalledOnce();
  });

  it('rolls back each unconsumed publisher admission on native throw and rethrows the exact error', () => {
    const ready = createReadyGoogletag();
    const displayError = new Error('exact display failure');
    const refreshError = new Error('exact refresh failure');
    const displayAdmissions = [0, 1].map(() =>
      Object.freeze({ commit: vi.fn(), rollback: vi.fn() })
    );
    const refreshAdmission = Object.freeze({ commit: vi.fn(), rollback: vi.fn() });
    ready.googletag.display = vi.fn(() => {
      throw displayError;
    });
    ready.pubads.refresh = vi.fn(() => {
      throw refreshError;
    });
    const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });
    let displayAttempt = 0;
    adapter.observePublisherCalls({
      display: () =>
        Object.freeze({
          action: 'forward' as const,
          admission: displayAdmissions[displayAttempt++]!,
        }),
      refresh: () => Object.freeze({ action: 'forward' as const, admission: refreshAdmission }),
    });

    const display = ready.googletag.display as (...arguments_: unknown[]) => unknown;
    expect(() => display('slot')).toThrow(displayError);
    expect(() => display('slot')).toThrow(displayError);
    const refresh = ready.pubads.refresh as (...arguments_: unknown[]) => unknown;
    expect(() => refresh(undefined, { changeCorrelator: true })).toThrow(refreshError);

    for (const admission of displayAdmissions) {
      expect(admission.rollback).toHaveBeenCalledOnce();
      expect(admission.commit).not.toHaveBeenCalled();
    }
    expect(refreshAdmission.rollback).toHaveBeenCalledOnce();
    expect(refreshAdmission.commit).not.toHaveBeenCalled();
  });

  it.each(['pubads', 'target_getter'] as const)(
    'fails open to captured publisher natives when %s identity probing throws',
    (failure) => {
      const ready = createReadyGoogletag();
      const target: { googletag?: unknown } = { googletag: ready.googletag };
      const slot = Object.freeze({ id: 'publisher-slot' });
      const nativeDefine = vi.fn(() => 'defined');
      const nativeDisplay = vi.fn(() => 'displayed');
      const nativeRefresh = vi.fn(() => 'refreshed');
      const nativeDestroy = vi.fn(() => true);
      ready.googletag.defineSlot = nativeDefine;
      ready.googletag.display = nativeDisplay;
      ready.googletag.destroySlots = nativeDestroy;
      ready.pubads.refresh = nativeRefresh;
      ready.pubads.getSlots.mockReturnValue([slot]);
      const adapter = createBrowserGoogletagAdapter(target);
      const observer = {
        defineSlot: vi.fn(() => Object.freeze({ action: 'forward' as const })),
        destroySlots: vi.fn(),
        display: vi.fn(() => Object.freeze({ action: 'forward' as const })),
        refresh: vi.fn(() => Object.freeze({ action: 'forward' as const })),
      };
      adapter.observePublisherCalls(observer);
      const define = ready.googletag.defineSlot as (...arguments_: unknown[]) => unknown;
      const display = ready.googletag.display as (...arguments_: unknown[]) => unknown;
      const refresh = ready.pubads.refresh as (...arguments_: unknown[]) => unknown;
      const destroy = ready.googletag.destroySlots as (...arguments_: unknown[]) => unknown;
      const identityError = new Error(`throwing ${failure}`);
      if (failure === 'pubads') {
        ready.googletag.pubads.mockImplementation(() => {
          throw identityError;
        });
      } else {
        Object.defineProperty(target, 'googletag', {
          configurable: true,
          get: () => {
            throw identityError;
          },
        });
      }

      expect(define('/publisher', [300, 250], 'slot')).toBe('defined');
      expect(display('slot')).toBe('displayed');
      expect(refresh([slot], { changeCorrelator: false })).toBe('refreshed');
      expect(destroy([slot])).toBe(true);
      expect(nativeDefine).toHaveBeenCalledOnce();
      expect(nativeDisplay).toHaveBeenCalledOnce();
      expect(nativeRefresh).toHaveBeenCalledOnce();
      expect(nativeDestroy).toHaveBeenCalledOnce();
      expect(observer.defineSlot).not.toHaveBeenCalled();
      expect(observer.display).not.toHaveBeenCalled();
      expect(observer.refresh).not.toHaveBeenCalled();
      expect(observer.destroySlots).not.toHaveBeenCalled();
    }
  );

  it('mediates only explicit publisher decisions and preserves receiver, arguments, return, throw, and order', () => {
    const ready = createReadyGoogletag({ initialLoadDisabled: true });
    const handoffSlot = Object.freeze({ id: 'handoff' });
    const ordinarySlot = Object.freeze({ id: 'ordinary' });
    const refreshOptions = Object.freeze({ changeCorrelator: true, publisher: 'kept' });
    const publisherError = new Error('publisher display failed');
    const defineReceiver = Object.freeze({ receiver: 'define' });
    const refreshReceiver = Object.freeze({ receiver: 'refresh' });
    const order: string[] = [];
    const nativeDefineSlot = vi.fn(function (this: unknown, ...arguments_: unknown[]) {
      order.push('native:define');
      return Object.freeze({ arguments_, receiver: this });
    });
    const nativeDisplay = vi.fn(function (this: unknown, ...arguments_: unknown[]) {
      order.push('native:display');
      if (arguments_[0] === 'throw') throw publisherError;
      return Object.freeze({ arguments_, receiver: this });
    });
    const nativeRefresh = vi.fn(function (this: unknown, ...arguments_: unknown[]) {
      order.push('native:refresh');
      return Object.freeze({ arguments_, receiver: this });
    });
    const nativeDestroy = vi.fn(function (this: unknown, ...arguments_: unknown[]) {
      order.push('native:destroy');
      return arguments_[0] === 'throw'
        ? (() => {
            throw new Error('publisher destroy failed');
          })()
        : true;
    });
    Object.assign(ready.googletag, {
      defineSlot: nativeDefineSlot,
      destroySlots: nativeDestroy,
      display: nativeDisplay,
    });
    ready.pubads.refresh = nativeRefresh;
    ready.pubads.getSlots.mockReturnValue([handoffSlot, ordinarySlot]);
    const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });
    let suppressDisplay = true;
    const destroyed: Array<readonly object[]> = [];
    const release = adapter.observePublisherCalls({
      defineSlot: (call) => {
        order.push('observer:define');
        expect(call.initialLoadDisabled).toBe(true);
        return call.elementId === 'handoff-id'
          ? Object.freeze({ action: 'handoff' as const, slot: handoffSlot })
          : Object.freeze({ action: 'forward' as const });
      },
      destroySlots: (call) => {
        order.push('observer:destroy');
        destroyed.push(call.slots);
      },
      display: () => {
        order.push('observer:display');
        if (!suppressDisplay) return Object.freeze({ action: 'forward' as const });
        suppressDisplay = false;
        return Object.freeze({ action: 'suppress' as const });
      },
      refresh: (call) => {
        order.push('observer:refresh');
        expect(call.requestedSlots).toBeUndefined();
        expect(call.slots).toEqual([handoffSlot, ordinarySlot]);
        return Object.freeze({ action: 'replace' as const, slots: Object.freeze([ordinarySlot]) });
      },
    });

    const defineSlot = ready.googletag.defineSlot as unknown as (
      ...arguments_: unknown[]
    ) => unknown;
    expect(
      Reflect.apply(defineSlot, defineReceiver, ['/publisher', [300, 250], 'handoff-id'])
    ).toBe(handoffSlot);
    expect(nativeDefineSlot).not.toHaveBeenCalled();
    const forwarded = Reflect.apply(defineSlot, defineReceiver, [
      '/publisher',
      [728, 90],
      'ordinary-id',
      'publisher-extra',
    ]);
    expect(forwarded).toEqual({
      arguments_: ['/publisher', [728, 90], 'ordinary-id', 'publisher-extra'],
      receiver: defineReceiver,
    });

    const display = ready.googletag.display as (...arguments_: unknown[]) => unknown;
    expect(Reflect.apply(display, defineReceiver, ['handoff-id'])).toBeUndefined();
    expect(Reflect.apply(display, defineReceiver, ['handoff-id', 'publisher-extra'])).toEqual({
      arguments_: ['handoff-id', 'publisher-extra'],
      receiver: defineReceiver,
    });
    expect(() => Reflect.apply(display, defineReceiver, ['throw', 'publisher-extra'])).toThrow(
      publisherError
    );

    const refresh = ready.pubads.refresh as (...arguments_: unknown[]) => unknown;
    expect(Reflect.apply(refresh, refreshReceiver, [undefined, refreshOptions])).toEqual({
      arguments_: [[ordinarySlot], refreshOptions],
      receiver: refreshReceiver,
    });

    const destroySlots = ready.googletag.destroySlots as unknown as (
      slots?: readonly object[]
    ) => unknown;
    expect(destroySlots([handoffSlot])).toBe(true);
    expect(destroyed).toEqual([[handoffSlot]]);
    expect(order).toEqual([
      'observer:define',
      'native:define',
      'observer:display',
      'native:display',
      'native:display',
      'observer:refresh',
      'native:refresh',
      'native:destroy',
      'observer:destroy',
    ]);

    release();
  });

  it('tracks native GPT initial-load configuration without duplicate wrappers', async () => {
    const ready = createReadyGoogletag({ initialLoadDisabled: true });
    const nativeSetConfig = ready.googletag.setConfig;
    const nativeDisableInitialLoad = ready.pubads.disableInitialLoad;
    const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });

    await expect(adapter.run((gpt) => gpt.serviceState().initialLoadDisabled).result).resolves.toBe(
      true
    );
    const wrappedSetConfig = ready.googletag.setConfig;
    const wrappedDisableInitialLoad = ready.pubads.disableInitialLoad;
    expect(wrappedSetConfig).not.toBe(nativeSetConfig);
    expect(wrappedDisableInitialLoad).not.toBe(nativeDisableInitialLoad);
    expect(ready.googletag.getConfig).toHaveBeenCalledWith('disableInitialLoad');

    await adapter.run((gpt) => gpt.serviceState()).result;
    expect(ready.googletag.setConfig).toBe(wrappedSetConfig);
    expect(ready.pubads.disableInitialLoad).toBe(wrappedDisableInitialLoad);

    expect(ready.googletag.setConfig({ disableInitialLoad: false })).toBe('config-result');
    await expect(adapter.run((gpt) => gpt.serviceState().initialLoadDisabled).result).resolves.toBe(
      false
    );
    expect(ready.pubads.disableInitialLoad()).toBe('legacy-result');
    await expect(adapter.run((gpt) => gpt.serviceState().initialLoadDisabled).result).resolves.toBe(
      true
    );

    adapter.dispose();
    expect(ready.googletag.setConfig).toBe(nativeSetConfig);
    expect(ready.pubads.disableInitialLoad).toBe(nativeDisableInitialLoad);
    expect(Reflect.ownKeys(ready.googletag).some((key) => String(key).startsWith('__ts'))).toBe(
      false
    );
  });

  it('preserves GPT configuration calls and falls back only when getConfig is unavailable', async () => {
    const ready = createReadyGoogletag();
    const binding = ready.googletag as unknown as Record<string, unknown>;
    const nativeSetConfig = vi.fn(function (
      this: unknown,
      config: { readonly disableInitialLoad?: boolean | null },
      marker: string
    ) {
      ready.initialLoad.disabled = config.disableInitialLoad === true;
      return { marker, receiver: this };
    });
    binding['setConfig'] = nativeSetConfig;
    const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });
    await adapter.run((gpt) => gpt.serviceState()).result;
    const receiver = { publisher: true };
    const config = { disableInitialLoad: true };

    const wrappedSetConfig = binding['setConfig'] as (...arguments_: unknown[]) => unknown;
    const returned = Reflect.apply(wrappedSetConfig, receiver, [config, 'exact']);
    expect(nativeSetConfig).toHaveBeenCalledWith(config, 'exact');
    expect(returned).toEqual({ marker: 'exact', receiver });
    await expect(adapter.run((gpt) => gpt.serviceState().initialLoadDisabled).result).resolves.toBe(
      true
    );

    binding['getConfig'] = undefined;
    Reflect.apply(wrappedSetConfig, receiver, [{ disableInitialLoad: false }, 'fallback']);
    await expect(adapter.run((gpt) => gpt.serviceState().initialLoadDisabled).result).resolves.toBe(
      false
    );
  });

  it('shares one GPT wrapper across adapter instances until the last owner disposes', async () => {
    const ready = createReadyGoogletag();
    const nativeSetConfig = ready.googletag.setConfig;
    const nativeDisableInitialLoad = ready.pubads.disableInitialLoad;
    const first = createBrowserGoogletagAdapter({ googletag: ready.googletag });
    const second = createBrowserGoogletagAdapter({ googletag: ready.googletag });
    await first.run((gpt) => gpt.serviceState()).result;
    const sharedSetConfig = ready.googletag.setConfig;
    const sharedDisableInitialLoad = ready.pubads.disableInitialLoad;

    await second.run((gpt) => gpt.serviceState()).result;
    expect(ready.googletag.setConfig).toBe(sharedSetConfig);
    expect(ready.pubads.disableInitialLoad).toBe(sharedDisableInitialLoad);

    first.dispose();
    expect(ready.googletag.setConfig).toBe(sharedSetConfig);
    expect(ready.pubads.disableInitialLoad).toBe(sharedDisableInitialLoad);
    ready.googletag.setConfig({ disableInitialLoad: true });
    await expect(second.run((gpt) => gpt.serviceState().initialLoadDisabled).result).resolves.toBe(
      true
    );

    second.dispose();
    expect(ready.googletag.setConfig).toBe(nativeSetConfig);
    expect(ready.pubads.disableInitialLoad).toBe(nativeDisableInitialLoad);
  });

  it('releases historical GPT initial-load ownership across A to B to C', async () => {
    const first = createReadyGoogletag();
    const second = createReadyGoogletag();
    const third = createReadyGoogletag();
    const firstNativeSetConfig = first.googletag.setConfig;
    const firstNativeDisable = first.pubads.disableInitialLoad;
    const secondPublisherSetConfig = vi.fn();
    const secondPublisherDisable = vi.fn();
    const thirdNativeSetConfig = third.googletag.setConfig;
    const thirdNativeDisable = third.pubads.disableInitialLoad;
    const target: { googletag?: unknown } = { googletag: first.googletag };
    const adapter = createBrowserGoogletagAdapter(target);

    await adapter.run((gpt) => gpt.serviceState()).result;
    expect(first.googletag.setConfig).not.toBe(firstNativeSetConfig);
    target.googletag = second.googletag;
    await adapter.run((gpt) => gpt.serviceState()).result;
    expect(first.googletag.setConfig).toBe(firstNativeSetConfig);
    expect(first.pubads.disableInitialLoad).toBe(firstNativeDisable);

    second.googletag.setConfig = secondPublisherSetConfig;
    second.pubads.disableInitialLoad = secondPublisherDisable;
    target.googletag = third.googletag;
    await adapter.run((gpt) => gpt.serviceState()).result;
    expect(second.googletag.setConfig).toBe(secondPublisherSetConfig);
    expect(second.pubads.disableInitialLoad).toBe(secondPublisherDisable);
    expect(third.googletag.setConfig).not.toBe(thirdNativeSetConfig);
    expect(third.pubads.disableInitialLoad).not.toBe(thirdNativeDisable);

    adapter.dispose();
    expect(third.googletag.setConfig).toBe(thirdNativeSetConfig);
    expect(third.pubads.disableInitialLoad).toBe(thirdNativeDisable);
  });

  it('preserves shared GPT initial-load ownership when one adapter changes bindings', async () => {
    const first = createReadyGoogletag();
    const second = createReadyGoogletag();
    const firstNativeSetConfig = first.googletag.setConfig;
    const firstNativeDisable = first.pubads.disableInitialLoad;
    const secondNativeSetConfig = second.googletag.setConfig;
    const firstTarget: { googletag?: unknown } = { googletag: first.googletag };
    const secondTarget: { googletag?: unknown } = { googletag: first.googletag };
    const firstAdapter = createBrowserGoogletagAdapter(firstTarget);
    const secondAdapter = createBrowserGoogletagAdapter(secondTarget);
    await firstAdapter.run((gpt) => gpt.serviceState()).result;
    await secondAdapter.run((gpt) => gpt.serviceState()).result;
    const sharedSetConfig = first.googletag.setConfig;

    firstTarget.googletag = second.googletag;
    await firstAdapter.run((gpt) => gpt.serviceState()).result;
    expect(first.googletag.setConfig).toBe(sharedSetConfig);
    expect(first.pubads.disableInitialLoad).not.toBe(firstNativeDisable);
    expect(second.googletag.setConfig).not.toBe(secondNativeSetConfig);

    secondAdapter.dispose();
    expect(first.googletag.setConfig).toBe(firstNativeSetConfig);
    expect(first.pubads.disableInitialLoad).toBe(firstNativeDisable);
    expect(second.googletag.setConfig).not.toBe(secondNativeSetConfig);

    firstAdapter.dispose();
    expect(second.googletag.setConfig).toBe(secondNativeSetConfig);
  });

  it('does not overwrite publisher GPT method replacements during restoration', async () => {
    const ready = createReadyGoogletag();
    const adapter = createBrowserGoogletagAdapter({ googletag: ready.googletag });
    await adapter.run((gpt) => gpt.serviceState()).result;
    const publisherSetConfig = vi.fn();
    const publisherDisableInitialLoad = vi.fn();
    ready.googletag.setConfig = publisherSetConfig;
    ready.pubads.disableInitialLoad = publisherDisableInitialLoad;

    adapter.dispose();

    expect(ready.googletag.setConfig).toBe(publisherSetConfig);
    expect(ready.pubads.disableInitialLoad).toBe(publisherDisableInitialLoad);
  });
});
