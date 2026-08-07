import { afterEach, describe, expect, it, vi } from 'vitest';

import { createBrowserPrebidAdapter, type PrebidEventFacade } from '../../src/adapters/prebid';

type Command = () => void;

function wrapBids(bids: object[] = []): object[] & { bids: object[] } {
  const response = [...bids] as object[] & { bids: object[] };
  response.bids = response;
  return response;
}

function recursivelyFreeze<T>(value: T): T {
  if (value && typeof value === 'object') {
    for (const child of Object.values(value)) recursivelyFreeze(child);
    Object.freeze(value);
  }
  return value;
}

function createStamp(overrides: Record<string, unknown> = {}) {
  return recursivelyFreeze({
    abi: 1,
    artifactReleaseId: 'a'.repeat(64),
    prebidVersion: '10.26.0',
    moduleStems: ['alphaBidAdapter', 'sharedIdSystem'],
    bidderCodes: ['alpha', 'alphaAlias'],
    bidderAliases: [{ code: 'alphaAlias', moduleStem: 'alphaBidAdapter' }],
    userIdModules: [
      {
        moduleName: 'sharedIdSystem',
        configNames: ['sharedId'],
        eidSources: ['sharedid.org'],
      },
    ],
    ...overrides,
  });
}

function createReadyPrebid(
  options: {
    readonly deferCommands?: boolean;
    readonly stamp?: object;
  } = {}
) {
  const commands: Command[] = [];
  const listeners = new Map<string, Set<(event: unknown) => void>>();
  const pbjs = {
    addAdUnits: vi.fn(),
    getBidResponsesForAdUnitCode: vi.fn<() => object[] & { bids: object[] }>(() => wrapBids()),
    getHighestCpmBids: vi.fn<() => object[]>(() => []),
    offEvent: vi.fn((type: string, listener: (event: unknown) => void) => {
      listeners.get(type)?.delete(listener);
    }),
    onEvent: vi.fn((type: string, listener: (event: unknown) => void) => {
      const registered = listeners.get(type) ?? new Set();
      registered.add(listener);
      listeners.set(type, registered);
    }),
    processQueue: vi.fn(),
    registerBidAdapter: vi.fn(),
    que: {
      push: vi.fn((command: Command): number => {
        if (options.deferCommands) commands.push(command);
        else command();
        return commands.length;
      }),
    },
    renderAd: vi.fn(),
    requestBids: vi.fn(),
  };
  const stamp = options.stamp ?? createStamp();
  Object.defineProperty(pbjs, '__trustedServerArtifactV1', {
    value: stamp,
    enumerable: false,
    writable: false,
    configurable: false,
  });
  return { commands, listeners, pbjs, stamp };
}

describe('browser Prebid adapter readiness', () => {
  afterEach(() => vi.useRealTimers());

  it('binds an exact valid artifact and exposes a frozen narrow facade', async () => {
    const ready = createReadyPrebid();
    const target: { pbjs: unknown } = { pbjs: ready.pbjs };
    const adapter = createBrowserPrebidAdapter(target);
    const operation = adapter.run((prebid) => {
      expect(Object.isFrozen(prebid)).toBe(true);
      expect('que' in prebid).toBe(false);
      expect('__trustedServerArtifactV1' in prebid).toBe(false);
      prebid.addAdUnits([{ code: 'slot-a' }]);
      prebid.registerBidAdapter(undefined, 'trustedServer', { code: 'trustedServer' });
      prebid.requestBids({ adUnitCodes: ['slot-a'] });
      prebid.renderAd({}, 'bid-a');
      return prebid.highestBids('slot-a');
    });

    expect(operation.status).toBe('present');
    await expect(operation.result).resolves.toEqual([]);
    expect(ready.pbjs.addAdUnits).toHaveBeenCalledTimes(1);
    expect(ready.pbjs.registerBidAdapter).toHaveBeenCalledWith(undefined, 'trustedServer', {
      code: 'trustedServer',
    });
    expect(ready.pbjs.requestBids).toHaveBeenCalledTimes(1);
    expect(ready.pbjs.renderAd).toHaveBeenCalledWith({}, 'bid-a');
  });

  it('drains pending commands FIFO through the real Prebid queue notification', async () => {
    const readinessCommands: Command[] = [];
    const target: { pbjs?: unknown } = { pbjs: { que: readinessCommands } };
    const adapter = createBrowserPrebidAdapter(target);
    const order: number[] = [];
    const first = adapter.run(() => order.push(1));
    const second = adapter.run(() => order.push(2));

    expect(first.status).toBe('pending');
    expect(second.status).toBe('pending');
    expect(readinessCommands).toHaveLength(1);

    target.pbjs = createReadyPrebid().pbjs;
    readinessCommands[0]?.();

    await expect(Promise.all([first.result, second.result])).resolves.toEqual([1, 2]);
    expect(order).toEqual([1, 2]);
  });

  it.each(['before', 'after'] as const)(
    'recovers Prebid notification arming when WeakSet.add throws %s insertion',
    async (failure) => {
      const readinessCommands: Command[] = [];
      const target: { pbjs?: unknown } = { pbjs: { que: readinessCommands } };
      const adapter = createBrowserPrebidAdapter(target);
      const originalWeakSetAdd = WeakSet.prototype.add;
      WeakSet.prototype.add = function (this: WeakSet<object>, value: object): WeakSet<object> {
        if (failure === 'after') Reflect.apply(originalWeakSetAdd, this, [value]);
        throw new Error(`Prebid arming failed ${failure} insertion`);
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
      if (!operation) throw new Error('Expected a published Prebid operation');
      expect(readinessCommands).toHaveLength(0);
      adapter.notifyReady();
      expect(readinessCommands).toHaveLength(1);
      target.pbjs = createReadyPrebid().pbjs;
      readinessCommands[0]?.();
      await expect(operation.result).resolves.toBe('ready');
      adapter.dispose();
    }
  );

  it('recovers Prebid notification arming when WeakSet.has throws after publication', async () => {
    vi.useFakeTimers();
    const readinessCommands: Command[] = [];
    const target: { pbjs?: unknown } = { pbjs: { que: readinessCommands } };
    const adapter = createBrowserPrebidAdapter(target);
    const order: number[] = [];
    const originalWeakSetHas = WeakSet.prototype.has;
    WeakSet.prototype.has = function (): boolean {
      throw new Error('Prebid armed lookup failed');
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
    if (!first) throw new Error('Expected a published Prebid operation');
    expect(readinessCommands).toHaveLength(1);
    const second = adapter.run(() => order.push(2));
    expect(readinessCommands).toHaveLength(1);
    target.pbjs = createReadyPrebid().pbjs;
    readinessCommands[0]?.();
    await expect(Promise.all([first.result, second.result])).resolves.toEqual([1, 2]);
    expect(order).toEqual([1, 2]);
    expect(vi.getTimerCount()).toBe(0);

    target.pbjs = undefined;
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
    'retries Prebid notification registration after $pushFailure enqueue failure and $deleteFailure rollback',
    async ({ pushFailure, deleteFailure }) => {
      vi.useFakeTimers();
      const readinessCommands: Command[] = [];
      let queueBroken = true;
      const push = vi.fn((command: Command): number => {
        if (queueBroken) {
          if (pushFailure === 'after') readinessCommands.push(command);
          throw new Error(`Prebid queue failed ${pushFailure} enqueue`);
        }
        readinessCommands.push(command);
        return readinessCommands.length;
      });
      const target: { pbjs?: unknown } = { pbjs: { que: { push } } };
      const adapter = createBrowserPrebidAdapter(target);
      const order: number[] = [];
      const originalWeakSetDelete = WeakSet.prototype.delete;
      WeakSet.prototype.delete = function (): boolean {
        if (deleteFailure === 'throw') throw new Error('Prebid arming rollback failed');
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
      if (!first) throw new Error('Expected a published Prebid operation');
      expect(readinessCommands).toHaveLength(pushFailure === 'after' ? 1 : 0);
      queueBroken = false;
      const second = adapter.run(() => order.push(2));
      expect(readinessCommands).toHaveLength(pushFailure === 'after' ? 2 : 1);

      target.pbjs = createReadyPrebid().pbjs;
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

      target.pbjs = undefined;
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

  it('rejects a queued operation when its pending Prebid stub becomes incompatible', async () => {
    const readinessCommands: Command[] = [];
    const binding: Record<string, unknown> = { que: readinessCommands };
    const adapter = createBrowserPrebidAdapter({ pbjs: binding });
    const command = vi.fn();
    const operation = adapter.run(command);
    Object.defineProperty(binding, '__trustedServerArtifactV1', {
      value: Object.freeze({}),
      enumerable: false,
      writable: false,
      configurable: false,
    });

    readinessCommands[0]?.();

    await expect(operation.result).rejects.toMatchObject({
      code: 'external_artifact_incompatible',
    });
    expect(operation.status).toBe('incompatible');
    expect(command).not.toHaveBeenCalled();
  });

  it('ignores a stale Prebid notification and lets the replacement notification decide', async () => {
    const oldNotifications: Command[] = [];
    const replacementNotifications: Command[] = [];
    const oldBinding = { que: oldNotifications };
    const replacement: Record<string, unknown> = { que: replacementNotifications };
    const target: { pbjs?: unknown } = { pbjs: oldBinding };
    const adapter = createBrowserPrebidAdapter(target);
    const first = adapter.run(() => 'first');
    target.pbjs = replacement;
    const second = adapter.run(() => 'second');
    Object.defineProperty(replacement, '__trustedServerArtifactV1', {
      value: Object.freeze({}),
      enumerable: false,
      writable: false,
      configurable: false,
    });

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

  it('does not let a stale Prebid notification condemn a primitive replacement', async () => {
    const oldNotifications: Command[] = [];
    const target: { pbjs?: unknown } = { pbjs: { que: oldNotifications } };
    const adapter = createBrowserPrebidAdapter(target);
    const operation = adapter.run(vi.fn());
    const result = operation.result.catch((error: unknown) => error);
    target.pbjs = 1;

    oldNotifications[0]?.();
    expect(operation.status).toBe('pending');
    adapter.dispose();
    await expect(result).resolves.toMatchObject({ code: 'operation_disposed' });
  });

  it('requires an exact own artifact data descriptor', async () => {
    const valid = createReadyPrebid();
    const inherited = Object.create(valid.pbjs) as Record<string, unknown>;
    const accessor = { ...valid.pbjs };
    Object.defineProperty(accessor, '__trustedServerArtifactV1', { get: () => valid.stamp });
    const enumerable = { ...valid.pbjs };
    Object.defineProperty(enumerable, '__trustedServerArtifactV1', {
      value: valid.stamp,
      enumerable: true,
      writable: false,
      configurable: false,
    });

    for (const pbjs of [{ ...valid.pbjs }, inherited, accessor, enumerable]) {
      const operation = createBrowserPrebidAdapter({ pbjs }).run(vi.fn());
      expect(operation.status).toBe('incompatible');
      await expect(operation.result).rejects.toMatchObject({
        code: 'external_artifact_incompatible',
      });
    }
  });

  it('rejects stamp accessors and extra own keys without invoking them', async () => {
    const getter = vi.fn(() => 'alpha');
    const bidderCodes: unknown[] = [];
    Object.defineProperty(bidderCodes, '0', {
      get: getter,
      enumerable: true,
      configurable: false,
    });
    Object.defineProperty(bidderCodes, 'length', { writable: false });
    Object.freeze(bidderCodes);
    const accessorStamp = Object.freeze({ ...createStamp(), bidderCodes });
    const extraStamp = recursivelyFreeze({ ...createStamp(), unexpected: true });

    for (const stamp of [accessorStamp, extraStamp]) {
      const operation = createBrowserPrebidAdapter({
        pbjs: createReadyPrebid({ stamp }).pbjs,
      }).run(vi.fn());
      await expect(operation.result).rejects.toMatchObject({
        code: 'external_artifact_incompatible',
      });
    }
    expect(getter).not.toHaveBeenCalled();
  });

  it('validates ABI, version, frozen bounded metadata, and configured coverage', async () => {
    const incompatibleStamps = [
      createStamp({ abi: 2 }),
      createStamp({ prebidVersion: '10.25.0' }),
      createStamp({ artifactReleaseId: 'A'.repeat(64) }),
      createStamp({ bidderCodes: ['alpha', 'alpha'] }),
      createStamp({ moduleStems: ['sharedIdSystem', 'alphaBidAdapter'] }),
      createStamp({ bidderAliases: [{ code: 'missing', moduleStem: 'alphaBidAdapter' }] }),
      createStamp({
        userIdModules: [
          {
            moduleName: 'sharedIdSystem',
            configNames: ['sharedId'],
            eidSources: ['UPPER.example'],
          },
        ],
      }),
      createStamp({
        moduleStems: Array.from(
          { length: 257 },
          (_, index) => `module-${String(index).padStart(3, '0')}`
        ),
      }),
    ];
    const mutable = Object.freeze({
      ...createStamp(),
      bidderCodes: ['alpha', 'alphaAlias'],
    });
    incompatibleStamps.push(mutable);

    for (const stamp of incompatibleStamps) {
      const operation = createBrowserPrebidAdapter(
        { pbjs: createReadyPrebid({ stamp }).pbjs },
        {
          configuredClientSideBidders: ['alpha'],
          requiredUserIdModules: [
            {
              moduleName: 'sharedIdSystem',
              configNames: ['sharedId'],
              eidSources: ['sharedid.org'],
            },
          ],
        }
      ).run(vi.fn());
      expect(operation.status).toBe('incompatible');
      await expect(operation.result).rejects.toMatchObject({
        code: 'external_artifact_incompatible',
      });
    }

    const uncoveredBidder = createBrowserPrebidAdapter(
      { pbjs: createReadyPrebid().pbjs },
      { configuredClientSideBidders: ['unbundled'] }
    ).run(vi.fn());
    await expect(uncoveredBidder.result).rejects.toMatchObject({
      code: 'external_artifact_incompatible',
    });
  });

  it('enforces every metadata count boundary', async () => {
    const names = (prefix: string, count: number) =>
      Array.from({ length: count }, (_, index) => `${prefix}-${String(index).padStart(3, '0')}`);
    const cases: Array<{ valid: object; invalid: object }> = [];

    cases.push({
      valid: createStamp({
        moduleStems: names('module', 256),
        bidderAliases: [],
        userIdModules: [],
      }),
      invalid: createStamp({
        moduleStems: names('module', 257),
        bidderAliases: [],
        userIdModules: [],
      }),
    });
    cases.push({
      valid: createStamp({ bidderCodes: names('bidder', 512), bidderAliases: [] }),
      invalid: createStamp({ bidderCodes: names('bidder', 513), bidderAliases: [] }),
    });
    const aliasCodes = names('alias', 512);
    const aliasModules = names('adapter', 171);
    const overflowingAliases = ['alias-a', 'alias-b', 'alias-c'].flatMap((code) =>
      aliasModules.map((moduleStem) => ({ code, moduleStem }))
    );
    cases.push({
      valid: createStamp({
        moduleStems: ['adapter'],
        bidderCodes: aliasCodes,
        bidderAliases: aliasCodes.map((code) => ({ code, moduleStem: 'adapter' })),
        userIdModules: [],
      }),
      invalid: createStamp({
        moduleStems: aliasModules,
        bidderCodes: ['alias-a', 'alias-b', 'alias-c'],
        bidderAliases: overflowingAliases,
        userIdModules: [],
      }),
    });
    const moduleNames = names('user', 128);
    cases.push({
      valid: createStamp({
        moduleStems: moduleNames,
        bidderAliases: [],
        userIdModules: moduleNames.map((moduleName) => ({
          moduleName,
          configNames: [],
          eidSources: [],
        })),
      }),
      invalid: createStamp({
        moduleStems: [...moduleNames, 'user-overflow'].sort(),
        bidderAliases: [],
        userIdModules: [...moduleNames, 'user-overflow'].sort().map((moduleName) => ({
          moduleName,
          configNames: [],
          eidSources: [],
        })),
      }),
    });
    const configNames = names('config', 64);
    const eidSources = names('source', 64).map((source) => `${source}.example`);
    cases.push({
      valid: createStamp({
        moduleStems: ['identity'],
        bidderAliases: [],
        userIdModules: [{ moduleName: 'identity', configNames, eidSources }],
      }),
      invalid: createStamp({
        moduleStems: ['identity'],
        bidderAliases: [],
        userIdModules: [
          {
            moduleName: 'identity',
            configNames: [...configNames, 'config-overflow'].sort(),
            eidSources,
          },
        ],
      }),
    });
    cases.push({
      valid: createStamp({
        moduleStems: ['identity'],
        bidderAliases: [],
        userIdModules: [{ moduleName: 'identity', configNames, eidSources }],
      }),
      invalid: createStamp({
        moduleStems: ['identity'],
        bidderAliases: [],
        userIdModules: [
          {
            moduleName: 'identity',
            configNames,
            eidSources: [...eidSources, 'source-overflow.example'].sort(),
          },
        ],
      }),
    });

    for (const boundary of cases) {
      const accepted = createBrowserPrebidAdapter({
        pbjs: createReadyPrebid({ stamp: boundary.valid }).pbjs,
      }).run(() => 'accepted');
      await expect(accepted.result).resolves.toBe('accepted');
      const refused = createBrowserPrebidAdapter({
        pbjs: createReadyPrebid({ stamp: boundary.invalid }).pbjs,
      }).run(vi.fn());
      await expect(refused.result).rejects.toMatchObject({
        code: 'external_artifact_incompatible',
      });
    }
  });

  it('enforces nonempty scalar-valid UTF-8 byte limits and nested lexical uniqueness', async () => {
    const invalidStamps = [
      createStamp({ moduleStems: [''] }),
      createStamp({ moduleStems: ['é'.repeat(65)] }),
      createStamp({ bidderCodes: ['\ud800'], bidderAliases: [] }),
      createStamp({
        bidderAliases: [
          { code: 'alphaAlias', moduleStem: 'alphaBidAdapter' },
          { code: 'alphaAlias', moduleStem: 'alphaBidAdapter' },
        ],
      }),
      createStamp({
        userIdModules: [
          { moduleName: 'sharedIdSystem', configNames: ['z', 'a'], eidSources: ['sharedid.org'] },
        ],
      }),
      createStamp({
        userIdModules: [
          {
            moduleName: 'sharedIdSystem',
            configNames: ['sharedId'],
            eidSources: ['z.example', 'a.example'],
          },
        ],
      }),
      createStamp({
        userIdModules: [{ moduleName: 'missingSystem', configNames: [], eidSources: [] }],
      }),
    ];
    const accepted = createStamp({
      moduleStems: ['é'.repeat(64)],
      bidderAliases: [],
      userIdModules: [],
    });
    await expect(
      createBrowserPrebidAdapter({ pbjs: createReadyPrebid({ stamp: accepted }).pbjs }).run(
        () => 'accepted'
      ).result
    ).resolves.toBe('accepted');

    for (const [index, stamp] of invalidStamps.entries()) {
      const operation = createBrowserPrebidAdapter({
        pbjs: createReadyPrebid({ stamp }).pbjs,
      }).run(vi.fn());
      expect(operation.status, `invalid metadata case ${index}`).toBe('incompatible');
      await expect(operation.result, `invalid metadata case ${index}`).rejects.toMatchObject({
        code: 'external_artifact_incompatible',
      });
    }
  });

  it('requires every real API method and contains hostile target and member getters', async () => {
    for (const method of [
      'addAdUnits',
      'getBidResponsesForAdUnitCode',
      'getHighestCpmBids',
      'offEvent',
      'onEvent',
      'processQueue',
      'registerBidAdapter',
      'renderAd',
      'requestBids',
    ] as const) {
      const ready = createReadyPrebid();
      Object.defineProperty(ready.pbjs, method, { value: undefined });
      const operation = createBrowserPrebidAdapter({ pbjs: ready.pbjs }).run(vi.fn());
      await expect(operation.result).rejects.toMatchObject({
        code: 'external_artifact_incompatible',
      });
    }

    const hostileTarget = Object.defineProperty({}, 'pbjs', {
      get: () => {
        throw new Error('target getter failed');
      },
    });
    let hostileTargetOperation:
      ReturnType<ReturnType<typeof createBrowserPrebidAdapter>['run']> | undefined;
    expect(() => {
      hostileTargetOperation = createBrowserPrebidAdapter(hostileTarget).run(vi.fn());
    }).not.toThrow();
    await expect(hostileTargetOperation?.result).rejects.toMatchObject({
      code: 'external_artifact_incompatible',
    });
    const hostile = createReadyPrebid();
    Object.defineProperty(hostile.pbjs, 'requestBids', {
      get: () => {
        throw new Error('member getter failed');
      },
    });
    let hostileMemberOperation:
      ReturnType<ReturnType<typeof createBrowserPrebidAdapter>['run']> | undefined;
    expect(() => {
      hostileMemberOperation = createBrowserPrebidAdapter({ pbjs: hostile.pbjs }).run(vi.fn());
    }).not.toThrow();
    await expect(hostileMemberOperation?.result).rejects.toMatchObject({
      code: 'external_artifact_incompatible',
    });
  });

  it('rejects missing required user-ID coverage and diagnoses one incompatible object once', async () => {
    const warning = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
    try {
      const ready = createReadyPrebid();
      const adapter = createBrowserPrebidAdapter(
        { pbjs: ready.pbjs },
        {
          requiredUserIdModules: [
            {
              moduleName: 'sharedIdSystem',
              configNames: ['missingConfig'],
              eidSources: ['missing.example'],
            },
          ],
        }
      );
      const first = adapter.run(vi.fn());
      const second = adapter.run(vi.fn());
      await expect(first.result).rejects.toMatchObject({ code: 'external_artifact_incompatible' });
      await expect(second.result).rejects.toMatchObject({ code: 'external_artifact_incompatible' });
      expect(adapter.bindingStatus()).toBe('incompatible');
      expect(warning).toHaveBeenCalledTimes(1);
      expect(String(warning.mock.calls[0]?.[0]).length).toBeLessThanOrEqual(256);
    } finally {
      warning.mockRestore();
    }
  });

  it.each(['before', 'after'] as const)(
    'bounds Prebid diagnostics when WeakSet.add persistently throws %s insertion',
    async (failure) => {
      const warning = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
      const incompatible = createReadyPrebid({ stamp: createStamp({ abi: 2 }) });
      const adapter = createBrowserPrebidAdapter({ pbjs: incompatible.pbjs });
      const originalWeakSetAdd = WeakSet.prototype.add;
      WeakSet.prototype.add = function (this: WeakSet<object>, value: object): WeakSet<object> {
        if (failure === 'after') Reflect.apply(originalWeakSetAdd, this, [value]);
        throw new Error(`diagnostic tracking failed ${failure} insertion`);
      } as typeof WeakSet.prototype.add;

      const poisoned: Array<ReturnType<typeof adapter.run>> = [];
      const thrown: unknown[] = [];
      try {
        for (let attempt = 0; attempt < 3; attempt += 1) {
          try {
            poisoned.push(adapter.run(vi.fn()));
          } catch (error) {
            thrown.push(error);
          }
        }
      } finally {
        WeakSet.prototype.add = originalWeakSetAdd;
      }

      try {
        expect(thrown).toEqual([]);
        expect(poisoned).toHaveLength(3);
        for (const operation of poisoned) {
          await expect(operation.result).rejects.toMatchObject({
            code: 'external_artifact_incompatible',
          });
        }
        expect(warning).toHaveBeenCalledTimes(failure === 'after' ? 1 : 0);

        const healthy = adapter.run(vi.fn());
        const suppressed = adapter.run(vi.fn());
        await expect(healthy.result).rejects.toMatchObject({
          code: 'external_artifact_incompatible',
        });
        await expect(suppressed.result).rejects.toMatchObject({
          code: 'external_artifact_incompatible',
        });
        expect(warning).toHaveBeenCalledTimes(1);
      } finally {
        warning.mockRestore();
        adapter.dispose();
      }
    }
  );

  it.each(['preflight', 'observation'] as const)(
    'recovers bounded Prebid diagnostics when WeakSet.has poisons %s',
    async (failure) => {
      const warning = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
      const incompatible = createReadyPrebid({ stamp: createStamp({ abi: 2 }) });
      const adapter = createBrowserPrebidAdapter({ pbjs: incompatible.pbjs });
      const originalWeakSetHas = WeakSet.prototype.has;
      let lookups = 0;
      WeakSet.prototype.has = function (this: WeakSet<object>, value: object): boolean {
        lookups += 1;
        if (failure === 'preflight' || lookups === 2) {
          throw new Error(`diagnostic ${failure} lookup failed`);
        }
        return Reflect.apply(originalWeakSetHas, this, [value]) as boolean;
      } as typeof WeakSet.prototype.has;

      let poisoned: ReturnType<typeof adapter.run> | undefined;
      let thrown: unknown;
      try {
        poisoned = adapter.run(vi.fn());
      } catch (error) {
        thrown = error;
      } finally {
        WeakSet.prototype.has = originalWeakSetHas;
      }

      try {
        expect(thrown).toBeUndefined();
        if (!poisoned) throw new Error('Expected a published Prebid operation');
        await expect(poisoned.result).rejects.toMatchObject({
          code: 'external_artifact_incompatible',
        });
        expect(warning).not.toHaveBeenCalled();

        const healthy = adapter.run(vi.fn());
        const suppressed = adapter.run(vi.fn());
        await expect(healthy.result).rejects.toMatchObject({
          code: 'external_artifact_incompatible',
        });
        await expect(suppressed.result).rejects.toMatchObject({
          code: 'external_artifact_incompatible',
        });
        expect(warning).toHaveBeenCalledTimes(1);
      } finally {
        warning.mockRestore();
        adapter.dispose();
      }
    }
  );

  it('releases pending capacity immediately on abort and adapter disposal', async () => {
    vi.useFakeTimers();
    const target: { pbjs?: unknown } = {};
    const adapter = createBrowserPrebidAdapter(target);
    const controller = new AbortController();
    const aborted = adapter.run(vi.fn(), { signal: controller.signal });
    const abortedResult = aborted.result.catch((error: unknown) => error);
    controller.abort();
    const replacements = Array.from({ length: 64 }, () => adapter.run(() => undefined));
    adapter.dispose();

    await expect(abortedResult).resolves.toMatchObject({ code: 'caller_aborted' });
    const disposed = await Promise.all(
      replacements.map(({ result }) => result.catch((error: unknown) => error))
    );
    expect(disposed).toHaveLength(64);
    for (const value of disposed) {
      expect(value).toMatchObject({ code: 'operation_disposed' });
    }
  });

  it('invalidates an entered command immediately when the adapter is disposed', async () => {
    const ready = createReadyPrebid();
    const adapter = createBrowserPrebidAdapter({ pbjs: ready.pbjs });
    const operation = adapter.run((prebid) => {
      adapter.dispose();
      prebid.requestBids({ mustNotRun: true });
    });

    await expect(operation.result).rejects.toMatchObject({ code: 'operation_disposed' });
    expect(operation.status).toBe('present');
    expect(ready.pbjs.requestBids).not.toHaveBeenCalled();
    expect(ready.listeners.size).toBe(0);
  });

  it('throws when Prebid inspection disposes the adapter before an operation is published', () => {
    const ready = createReadyPrebid();
    const holder: { adapter?: ReturnType<typeof createBrowserPrebidAdapter> } = {};
    const target = Object.defineProperty({}, 'pbjs', {
      get: () => {
        holder.adapter?.dispose();
        return ready.pbjs;
      },
    });
    const adapter = createBrowserPrebidAdapter(target);
    holder.adapter = adapter;
    const command = vi.fn();

    expect(() => adapter.run(command)).toThrowError(
      expect.objectContaining({ code: 'operation_disposed' })
    );
    expect(command).not.toHaveBeenCalled();
    expect(ready.commands).toHaveLength(0);
  });

  it('rejects without enqueueing when Prebid inspection disposes a published operation', async () => {
    const ready = createReadyPrebid({ deferCommands: true });
    let reads = 0;
    const holder: { adapter?: ReturnType<typeof createBrowserPrebidAdapter> } = {};
    const target = Object.defineProperty({}, 'pbjs', {
      get: () => {
        reads += 1;
        if (reads === 2) holder.adapter?.dispose();
        return ready.pbjs;
      },
    });
    const adapter = createBrowserPrebidAdapter(target);
    holder.adapter = adapter;
    const command = vi.fn();
    const operation = adapter.run(command);

    await expect(operation.result).rejects.toMatchObject({ code: 'operation_disposed' });
    expect(command).not.toHaveBeenCalled();
    expect(ready.commands).toHaveLength(0);
  });

  it('contains disposal reentrant from Prebid member reads and external calls', async () => {
    const ready = createReadyPrebid();
    const adapter = createBrowserPrebidAdapter({ pbjs: ready.pbjs });
    const staleRequest = vi.fn();
    let requestBidsReads = 0;
    Object.defineProperty(ready.pbjs, 'requestBids', {
      get: () => {
        requestBidsReads += 1;
        if (requestBidsReads > 1) adapter.dispose();
        return staleRequest;
      },
    });
    const memberOperation = adapter.run((prebid) => prebid.requestBids({}));

    await expect(memberOperation.result).rejects.toMatchObject({ code: 'operation_disposed' });
    expect(staleRequest).not.toHaveBeenCalled();

    const externalReady = createReadyPrebid();
    const externalAdapter = createBrowserPrebidAdapter({ pbjs: externalReady.pbjs });
    externalReady.pbjs.requestBids.mockImplementation(() => externalAdapter.dispose());
    const externalOperation = externalAdapter.run((prebid) => {
      prebid.requestBids({ first: true });
      prebid.requestBids({ mustNotRun: true });
    });

    await expect(externalOperation.result).rejects.toMatchObject({ code: 'operation_disposed' });
    expect(externalReady.pbjs.requestBids).toHaveBeenCalledTimes(1);
  });

  it('rechecks identity after hostile facade member reads and calls', async () => {
    const first = createReadyPrebid();
    const replacement = createReadyPrebid();
    const target: { pbjs?: unknown } = { pbjs: first.pbjs };
    const staleRequest = vi.fn();
    Object.defineProperty(first.pbjs, 'requestBids', {
      get: () => {
        target.pbjs = replacement.pbjs;
        return staleRequest;
      },
    });
    const operation = createBrowserPrebidAdapter(target).run((prebid) => prebid.requestBids({}));

    await expect(operation.result).rejects.toMatchObject({
      code: 'external_artifact_incompatible',
    });
    expect(operation.status).toBe('incompatible');
    expect(staleRequest).not.toHaveBeenCalled();
  });

  it('rechecks both object and stamp identity before invoking a deferred command', async () => {
    const first = createReadyPrebid({ deferCommands: true });
    const replacement = createReadyPrebid();
    const target: { pbjs?: unknown } = { pbjs: first.pbjs };
    const adapter = createBrowserPrebidAdapter(target);
    const command = vi.fn();
    const operation = adapter.run(command);
    const result = operation.result.catch((error: unknown) => error);

    target.pbjs = replacement.pbjs;
    expect(() => first.commands[0]?.()).not.toThrow();
    await expect(result).resolves.toMatchObject({ code: 'external_artifact_incompatible' });
    expect(operation.status).toBe('incompatible');
    expect(command).not.toHaveBeenCalled();

    const later = adapter.run(() => 'replacement');
    await expect(later.result).resolves.toBe('replacement');
  });

  it('marks an operation incompatible when its command replaces the bound object', async () => {
    const first = createReadyPrebid();
    const replacement = createReadyPrebid();
    const target: { pbjs?: unknown } = { pbjs: first.pbjs };
    const operation = createBrowserPrebidAdapter(target).run(() => {
      target.pbjs = replacement.pbjs;
      return 'stale';
    });

    await expect(operation.result).rejects.toMatchObject({
      code: 'external_artifact_incompatible',
    });
    expect(operation.status).toBe('incompatible');
  });

  it('rolls back an exact Prebid listener when installation replaces the binding', async () => {
    const first = createReadyPrebid();
    const replacement = createReadyPrebid();
    const target: { pbjs?: unknown } = { pbjs: first.pbjs };
    const adapter = createBrowserPrebidAdapter(target);
    first.pbjs.onEvent.mockImplementation((type, listener) => {
      const registered = first.listeners.get(type) ?? new Set();
      registered.add(listener);
      first.listeners.set(type, registered);
      target.pbjs = replacement.pbjs;
    });
    const operation = adapter.run((prebid) => prebid.subscribe('bidResponse', vi.fn()));

    await expect(operation.result).rejects.toMatchObject({
      code: 'external_artifact_incompatible',
    });
    const installed = first.pbjs.onEvent.mock.calls[0]?.[1];
    expect(first.pbjs.offEvent).toHaveBeenCalledWith('bidResponse', installed);
    expect(first.listeners.get('bidResponse')?.size).toBe(0);
  });

  it('grants synchronous highest-bid access only for the active event callback', async () => {
    const ready = createReadyPrebid();
    const selected = Object.freeze({ adId: 'r1_selected', adUnitCode: 'slot-one' });
    ready.pbjs.getHighestCpmBids.mockReturnValue([selected]);
    const adapter = createBrowserPrebidAdapter({ pbjs: ready.pbjs });
    let eventFacade: Readonly<PrebidEventFacade> | undefined;
    const listener = vi.fn((event: unknown, prebid: Readonly<PrebidEventFacade>) => {
      eventFacade = prebid;
      expect(event).toEqual({ auctionId: 'auction-one' });
      expect(Object.isFrozen(prebid)).toBe(true);
      expect(Reflect.ownKeys(prebid)).toEqual(['highestBids']);
      expect(prebid.highestBids('slot-one')).toEqual([selected]);
    });

    await adapter.run((prebid) => prebid.subscribe('auctionEnd', listener)).result;
    const installed = [...(ready.listeners.get('auctionEnd') ?? [])][0];
    expect(() => installed?.({ auctionId: 'auction-one' })).not.toThrow();

    expect(listener).toHaveBeenCalledTimes(1);
    expect(ready.pbjs.getHighestCpmBids).toHaveBeenCalledExactlyOnceWith('slot-one');
    expect(() => eventFacade?.highestBids('slot-one')).toThrowError(
      expect.objectContaining({ code: 'external_artifact_incompatible' })
    );
    adapter.dispose();
  });

  it('rolls back a Prebid listener when installation disposes and cleanup throws', async () => {
    const ready = createReadyPrebid();
    const adapter = createBrowserPrebidAdapter({ pbjs: ready.pbjs });
    ready.pbjs.onEvent.mockImplementation((type, listener) => {
      const registered = ready.listeners.get(type) ?? new Set();
      registered.add(listener);
      ready.listeners.set(type, registered);
      adapter.dispose();
    });
    ready.pbjs.offEvent.mockImplementation((type, listener) => {
      ready.listeners.get(type)?.delete(listener);
      throw new Error('cleanup failed');
    });
    const operation = adapter.run((prebid) => prebid.subscribe('bidResponse', vi.fn()));

    await expect(operation.result).rejects.toMatchObject({ code: 'operation_disposed' });
    expect(ready.pbjs.offEvent).toHaveBeenCalledTimes(1);
    expect(ready.listeners.get('bidResponse')?.size).toBe(0);
  });

  it.each(['dispose', 'throw'] as const)(
    'rolls back Prebid subscription ownership when effect registration must %s',
    async (failure) => {
      const ready = createReadyPrebid();
      const adapter = createBrowserPrebidAdapter({ pbjs: ready.pbjs });
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
        operation = adapter.run((prebid) => {
          prebid.subscribe('existing', existingListener);
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
          return prebid.subscribe('failed', failedListener);
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
      expect(ready.pbjs.offEvent).toHaveBeenCalledTimes(2);
    }
  );

  it('settles a live Prebid operation and restores listeners when Set.delete is poisoned', async () => {
    const ready = createReadyPrebid();
    const originalSetDelete = Set.prototype.delete;
    ready.pbjs.offEvent.mockImplementation((type, listener) => {
      const registered = ready.listeners.get(type);
      if (registered) Reflect.apply(originalSetDelete, registered, [listener]);
    });
    const adapter = createBrowserPrebidAdapter({ pbjs: ready.pbjs });
    const listener = vi.fn();
    const operation = adapter.run((prebid) => {
      prebid.subscribe('bidResponse', listener);
      return new Promise<never>(() => undefined);
    });
    expect(ready.listeners.get('bidResponse')).toHaveLength(1);

    Set.prototype.delete = function (): boolean {
      throw new Error('Prebid live cleanup delete failed');
    } as typeof Set.prototype.delete;
    try {
      expect(() => adapter.dispose()).not.toThrow();
    } finally {
      Set.prototype.delete = originalSetDelete;
    }

    await expect(operation.result).rejects.toMatchObject({ code: 'operation_disposed' });
    expect(ready.listeners.get('bidResponse')).toHaveLength(0);
    expect(() => adapter.dispose()).not.toThrow();
  });

  it('rolls back a failed Prebid command subscription without touching prior global effects', async () => {
    const ready = createReadyPrebid();
    const adapter = createBrowserPrebidAdapter({ pbjs: ready.pbjs });
    const priorListener = vi.fn();
    const failedListener = vi.fn();
    const commandError = new Error('command failed');
    await adapter.run((prebid) => prebid.subscribe('prior', priorListener)).result;

    const operation = adapter.run((prebid) => {
      prebid.subscribe('failed', failedListener);
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
    expect(ready.pbjs.offEvent).toHaveBeenCalledTimes(2);
  });

  it('promotes fulfilled Prebid command subscriptions and rolls back rejected ones', async () => {
    const ready = createReadyPrebid();
    const adapter = createBrowserPrebidAdapter({ pbjs: ready.pbjs });
    const rejection = new Error('async command failed');
    const rejected = adapter.run((prebid) => {
      prebid.subscribe('rejected', vi.fn());
      return Promise.reject(rejection);
    });

    await expect(rejected.result).rejects.toBe(rejection);
    expect(ready.listeners.get('rejected')?.size).toBe(0);

    const fulfilled = adapter.run((prebid) => {
      prebid.subscribe('fulfilled', vi.fn());
      return Promise.resolve('complete');
    });
    await expect(fulfilled.result).resolves.toBe('complete');
    expect(ready.listeners.get('fulfilled')?.size).toBe(1);

    adapter.dispose();
    expect(ready.listeners.get('fulfilled')?.size).toBe(0);
  });

  it.each(['dispose', 'replacement'] as const)(
    'rolls back a provisional Prebid subscription after async %s',
    async (failure) => {
      const first = createReadyPrebid();
      const replacement = createReadyPrebid();
      const target: { pbjs?: unknown } = { pbjs: first.pbjs };
      const adapter = createBrowserPrebidAdapter(target);
      let resolveCommand!: (value: string) => void;
      const commandResult = new Promise<string>((resolve) => {
        resolveCommand = resolve;
      });
      const operation = adapter.run((prebid) => {
        prebid.subscribe('provisional', vi.fn());
        return commandResult;
      });

      if (failure === 'dispose') adapter.dispose();
      else target.pbjs = replacement.pbjs;
      resolveCommand('late-success');

      await expect(operation.result).rejects.toMatchObject({
        code: failure === 'dispose' ? 'operation_disposed' : 'external_artifact_incompatible',
      });
      expect(first.listeners.get('provisional')?.size).toBe(0);
    }
  );

  it('holds 64 pending operations and fails only overflow synchronously', async () => {
    vi.useFakeTimers();
    const target: { pbjs?: unknown } = {};
    const adapter = createBrowserPrebidAdapter(target);
    const operations = Array.from({ length: 64 }, () => adapter.run(() => undefined));
    expect(() => adapter.run(() => undefined)).toThrowError(
      expect.objectContaining({ code: 'external_queue_full' })
    );

    target.pbjs = createReadyPrebid().pbjs;
    adapter.notifyReady();
    await expect(Promise.all(operations.map(({ result }) => result))).resolves.toHaveLength(64);
  });

  it('reserves pending Prebid capacity before hostile signal registration reenters', async () => {
    const target: { pbjs?: unknown } = {};
    const adapter = createBrowserPrebidAdapter(target);
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

    target.pbjs = createReadyPrebid().pbjs;
    adapter.notifyReady();
    await expect(
      Promise.all([outer.result, ...accepted.map(({ result }) => result)])
    ).resolves.toHaveLength(64);
    expect(order).toEqual(Array.from({ length: 64 }, (_, index) => index));
  });

  it('reserves pending Prebid capacity before poisoned Set.add reenters', async () => {
    const target: { pbjs?: unknown } = {};
    const adapter = createBrowserPrebidAdapter(target);
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
    if (!outer) throw new Error('Expected a published Prebid operation');

    expect(accepted).toHaveLength(63);
    expect(overflows).toHaveLength(1);
    expect(overflows[0]).toMatchObject({ code: 'external_queue_full' });

    target.pbjs = createReadyPrebid().pbjs;
    adapter.notifyReady();
    await expect(
      Promise.all([outer.result, ...accepted.map(({ result }) => result)])
    ).resolves.toHaveLength(64);
    expect(order).toEqual([...Array.from({ length: 63 }, (_, index) => index + 1), 0]);

    target.pbjs = undefined;
    const recovered = Array.from({ length: 64 }, () => adapter.run(vi.fn()));
    expect(() => adapter.run(vi.fn())).toThrowError(
      expect.objectContaining({ code: 'external_queue_full' })
    );
    target.pbjs = createReadyPrebid().pbjs;
    adapter.notifyReady();
    await expect(Promise.all(recovered.map(({ result }) => result))).resolves.toHaveLength(64);
  });

  it('rolls back pending Prebid publication when poisoned Set.add throws', async () => {
    vi.useFakeTimers();
    const target: { pbjs?: unknown } = {};
    const adapter = createBrowserPrebidAdapter(target);
    const publicationError = new Error('Prebid publication failed');
    const command = vi.fn();
    const signalGetter = vi.fn(() => undefined);
    const options = Object.defineProperty({}, 'signal', {
      get: signalGetter,
    }) as { readonly signal?: AbortSignal };
    const originalSetAdd = Set.prototype.add;
    const originalSetDelete = Set.prototype.delete;
    const poisonedDelete = function (): boolean {
      throw new Error('Prebid publication rollback delete failed');
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
    target.pbjs = createReadyPrebid().pbjs;
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
    'contains hostile Prebid AbortSignal ownership for %s',
    async (failure) => {
      const adapter = createBrowserPrebidAdapter({});
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
      if (!operation) throw new Error('Expected a published Prebid operation');
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
    'settles Prebid abort transitions during listener registration for %s',
    async (transition) => {
      vi.useFakeTimers();
      const target: { pbjs?: unknown } = {};
      const adapter = createBrowserPrebidAdapter(target);
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

      target.pbjs = createReadyPrebid().pbjs;
      adapter.notifyReady();
      expect(command).not.toHaveBeenCalled();

      target.pbjs = undefined;
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

  it('owns an exact ten-second per-operation deadline and ignores late readiness', async () => {
    vi.useFakeTimers();
    const target: { pbjs?: unknown } = {};
    const adapter = createBrowserPrebidAdapter(target);
    const first = adapter.run(vi.fn());
    const firstResult = first.result.catch((error: unknown) => error);
    await vi.advanceTimersByTimeAsync(5_000);
    const second = adapter.run(vi.fn());
    const secondResult = second.result.catch((error: unknown) => error);

    await vi.advanceTimersByTimeAsync(5_000);
    expect(first.status).toBe('timed_out');
    expect(second.status).toBe('pending');
    target.pbjs = createReadyPrebid().pbjs;
    adapter.notifyReady();

    await expect(firstResult).resolves.toMatchObject({ code: 'external_ready_timeout' });
    await expect(secondResult).resolves.toBeUndefined();
  });

  it('removes aborts and disposal immediately, including deferred commands', async () => {
    const deferred = createReadyPrebid({ deferCommands: true });
    const controller = new AbortController();
    const adapter = createBrowserPrebidAdapter({ pbjs: deferred.pbjs });
    const command = vi.fn();
    const operation = adapter.run(command, { signal: controller.signal });
    const result = operation.result.catch((error: unknown) => error);

    controller.abort();
    expect(() => deferred.commands[0]?.()).not.toThrow();
    adapter.dispose();

    await expect(result).resolves.toMatchObject({ code: 'caller_aborted' });
    expect(command).not.toHaveBeenCalled();
  });

  it('contains queue, command, and event callback throws', async () => {
    const ready = createReadyPrebid();
    const callbackError = new Error('callback failed');
    const listener = vi.fn(() => {
      throw callbackError;
    });
    const adapter = createBrowserPrebidAdapter({ pbjs: ready.pbjs });
    const operation = adapter.run((prebid) => {
      const unsubscribe = prebid.subscribe('bidResponse', listener);
      const installed = [...(ready.listeners.get('bidResponse') ?? [])][0];
      expect(() => installed?.({ adId: 'bid-a' })).not.toThrow();
      unsubscribe();
      throw callbackError;
    });
    await expect(operation.result).rejects.toBe(callbackError);

    const pushError = new Error('queue failed');
    const throwing = createReadyPrebid();
    throwing.pbjs.que.push.mockImplementation(() => {
      throw pushError;
    });
    const pushAdapter = createBrowserPrebidAdapter({ pbjs: throwing.pbjs });
    let pushOperation: ReturnType<typeof pushAdapter.run> | undefined;
    expect(() => {
      pushOperation = pushAdapter.run(() => undefined);
    }).not.toThrow();
    await expect(pushOperation?.result).rejects.toBe(pushError);
  });
});

describe('version-pinned Trusted Server bid admission', () => {
  const preparedBid = () =>
    recursivelyFreeze({
      auctionId: 'auction-one',
      adUnitCode: 'slot-one',
      bid: {
        requestId: 'request-one',
        adId: 'r1_BwcHBwcHBwcHBwcHBwcHBw',
        cpm: 1.25,
        width: 300,
        height: 250,
        ad: '' as const,
        ttl: 300 as const,
        creativeId: 'creative-one',
        netRevenue: true as const,
        currency: 'USD' as const,
        bidderCode: 'trustedServer',
        meta: {
          advertiserDomains: [] as string[],
          tsAuctionId: 'auction-one',
          tsBidId: 'bid-one',
        },
      },
    });

  function admissionFixture() {
    const ready = createReadyPrebid();
    const stored: object[] = [];
    ready.pbjs.getBidResponsesForAdUnitCode.mockImplementation((adUnitCode?: string) =>
      wrapBids(stored.filter((bid) => (bid as { adUnitCode?: unknown }).adUnitCode === adUnitCode))
    );
    const target: { pbjs: unknown } = { pbjs: ready.pbjs };
    const adapter = createBrowserPrebidAdapter(target);
    const auctions: unknown[] = [];
    const operation = adapter.run((facade) => {
      const boundary = facade as unknown as {
        registerTrustedServerBidder(listener: (auction: unknown) => void): unknown;
      };
      return boundary.registerTrustedServerBidder((auction) => auctions.push(auction));
    });
    const bidderFactory = ready.pbjs.registerBidAdapter.mock.calls[0]?.[0] as
      | (() => {
          callBids(
            request: unknown,
            admit: (adUnitCode: string, bid: Record<string, unknown>) => void,
            done: () => void
          ): void;
        })
      | undefined;
    const bidder = bidderFactory?.();
    expect(ready.pbjs.registerBidAdapter).toHaveBeenCalledWith(bidderFactory, 'trustedServer');
    const done = vi.fn();
    const emitBidResponse = (bid: object): void => {
      for (const listener of ready.listeners.get('bidResponse') ?? []) listener(bid);
    };
    const admit = vi.fn((adUnitCode: string, bid: Record<string, unknown>) => {
      const published = { ...bid, adUnitCode };
      stored.push(published);
      emitBidResponse(published);
    });
    bidder?.callBids(
      {
        auctionId: 'auction-one',
        bids: [
          {
            adUnitCode: 'slot-one',
            adUnitId: 'ad-unit-one',
            auctionId: 'auction-one',
            bidId: 'request-one',
            src: 'client',
            transactionId: 'transaction-one',
          },
        ],
      },
      admit,
      done
    );
    const boundary = adapter as unknown as {
      admitTrustedBid(prepared: ReturnType<typeof preparedBid>): 'admitted' | 'not_admitted';
    };
    return {
      adapter,
      admit,
      auctions,
      boundary,
      done,
      emitBidResponse,
      operation,
      ready,
      stored,
      target,
    };
  }

  it('captures one exact auction callback and admits a mutable copy atomically', async () => {
    const fixture = admissionFixture();
    await expect(fixture.operation.result).resolves.toBeUndefined();

    expect(fixture.auctions).toHaveLength(1);
    const auction = fixture.auctions[0] as {
      auctionId: string;
      bids: readonly { adUnitCode: string; requestId: string }[];
      complete(): void;
    };
    expect(Object.isFrozen(auction)).toBe(true);
    expect(Object.isFrozen(auction.bids)).toBe(true);
    expect(auction).toMatchObject({
      auctionId: 'auction-one',
      bids: [{ adUnitCode: 'slot-one', requestId: 'request-one' }],
    });

    const prepared = preparedBid();
    expect(fixture.boundary.admitTrustedBid(prepared)).toBe('admitted');
    expect(fixture.admit).toHaveBeenCalledTimes(1);
    const admitted = fixture.admit.mock.calls[0]?.[1];
    expect(admitted).toMatchObject(prepared.bid);
    expect(admitted).not.toBe(prepared.bid);
    expect(admitted).toMatchObject({
      adUnitId: 'ad-unit-one',
      auctionId: 'auction-one',
      mediaType: 'banner',
      source: 'client',
      transactionId: 'transaction-one',
    });
    expect(Reflect.apply(admitted?.['getSize'] as () => string, admitted, [])).toBe('300x250');
    expect(admitted?.['meta']).not.toBe(prepared.bid.meta);
    expect((admitted?.['meta'] as { advertiserDomains?: unknown })?.advertiserDomains).not.toBe(
      prepared.bid.meta.advertiserDomains
    );
    expect(Object.isFrozen(prepared.bid)).toBe(true);

    auction.complete();
    auction.complete();
    expect(fixture.done).toHaveBeenCalledTimes(1);
  });

  it('returns not_admitted only when neither state nor an event was published', async () => {
    const fixture = admissionFixture();
    await fixture.operation.result;
    fixture.admit.mockImplementation(() => undefined);

    expect(fixture.boundary.admitTrustedBid(preparedBid())).toBe('not_admitted');
    expect(fixture.stored).toEqual([]);
  });

  it('rejects a response query that does not use the pinned self-wrapped array shape', async () => {
    const fixture = admissionFixture();
    await fixture.operation.result;
    fixture.ready.pbjs.getBidResponsesForAdUnitCode.mockImplementation(
      () => ({ bids: [] }) as never
    );

    expect(() => fixture.boundary.admitTrustedBid(preparedBid())).toThrowError(
      expect.objectContaining({ code: 'external_artifact_incompatible' })
    );
    expect(fixture.admit).not.toHaveBeenCalled();
  });

  it('makes a request terminal after not_admitted instead of retrying publication', async () => {
    const fixture = admissionFixture();
    await fixture.operation.result;
    fixture.admit.mockImplementation(() => undefined);

    expect(fixture.boundary.admitTrustedBid(preparedBid())).toBe('not_admitted');
    fixture.admit.mockImplementation((adUnitCode, bid) => {
      const published = { ...bid, adUnitCode };
      fixture.stored.push(published);
      fixture.emitBidResponse(published);
    });
    expect(fixture.boundary.admitTrustedBid(preparedBid())).toBe('not_admitted');
    expect(fixture.admit).toHaveBeenCalledTimes(1);
    expect(fixture.stored).toEqual([]);
  });

  it('matches response state and events by exact auction, request, and ad-unit identity', async () => {
    const fixture = admissionFixture();
    await fixture.operation.result;
    const prepared = preparedBid();
    fixture.stored.push({
      ...prepared.bid,
      auctionId: 'other-auction',
      adUnitCode: prepared.adUnitCode,
    });
    fixture.admit.mockImplementation((adUnitCode, bid) => {
      fixture.emitBidResponse({
        ...bid,
        auctionId: 'other-auction',
        adUnitCode,
      });
      const published = { ...bid, adUnitCode };
      fixture.stored.push(published);
      fixture.emitBidResponse(published);
    });

    expect(fixture.boundary.admitTrustedBid(prepared)).toBe('admitted');
  });

  it('refuses a second live Trusted Server bidder registration on the same binding', async () => {
    const fixture = admissionFixture();
    await fixture.operation.result;

    const duplicate = fixture.adapter.run((prebid) => prebid.registerTrustedServerBidder(vi.fn()));

    await expect(duplicate.result).rejects.toMatchObject({
      code: 'external_artifact_incompatible',
    });
    expect(fixture.ready.pbjs.registerBidAdapter).toHaveBeenCalledTimes(1);
    fixture.adapter.dispose();
  });

  it('throws a contract violation for partial publication and an ordinary callback throw otherwise', async () => {
    const partial = admissionFixture();
    await partial.operation.result;
    partial.admit.mockImplementation((adUnitCode, bid) =>
      partial.emitBidResponse({ ...bid, adUnitCode })
    );

    expect(() => partial.boundary.admitTrustedBid(preparedBid())).toThrowError(
      expect.objectContaining({ code: 'prebid_partial_publication' })
    );

    const failed = admissionFixture();
    await failed.operation.result;
    const callbackFailure = new Error('fictional response callback failure');
    failed.admit.mockImplementation(() => {
      throw callbackFailure;
    });
    expect(() => failed.boundary.admitTrustedBid(preparedBid())).toThrow(callbackFailure);
  });

  it('rejects detached requests, duplicate admission, binding replacement, and late use', async () => {
    const fixture = admissionFixture();
    await fixture.operation.result;

    expect(
      fixture.boundary.admitTrustedBid(
        recursivelyFreeze({ ...preparedBid(), adUnitCode: 'other-slot' })
      )
    ).toBe('not_admitted');
    expect(fixture.boundary.admitTrustedBid(preparedBid())).toBe('admitted');
    expect(() => fixture.boundary.admitTrustedBid(preparedBid())).toThrowError(
      expect.objectContaining({ code: 'prebid_partial_publication' })
    );

    const auction = fixture.auctions[0] as { complete(): void };
    auction.complete();
    expect(fixture.boundary.admitTrustedBid(preparedBid())).toBe('not_admitted');

    const replaced = admissionFixture();
    await replaced.operation.result;
    replaced.target.pbjs = createReadyPrebid().pbjs;
    expect(() => replaced.boundary.admitTrustedBid(preparedBid())).toThrowError(
      expect.objectContaining({ code: 'external_artifact_incompatible' })
    );
  });
});
