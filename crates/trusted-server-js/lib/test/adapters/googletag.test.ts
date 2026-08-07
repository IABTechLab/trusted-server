import { afterEach, describe, expect, it, vi } from 'vitest';

import { createBrowserGoogletagAdapter } from '../../src/adapters/googletag';

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
