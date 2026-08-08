import { describe, expect, it, vi } from 'vitest';

import { createBrowserGoogletagAdapter } from '../../src/adapters/googletag';
import { createTargetingService } from '../../src/services/targeting';

function createTargetingHarness(initial: Record<string, readonly string[]> = {}) {
  const values = new Map<string, readonly string[]>(Object.entries(initial));
  const clearTargeting = vi.fn((key?: string) => {
    if (key === undefined) values.clear();
    else values.delete(key);
  });
  const getTargeting = vi.fn((key: string) => Object.freeze([...(values.get(key) ?? [])]));
  const setTargeting = vi.fn((key: string, value: string | readonly string[]) => {
    values.set(key, Object.freeze(typeof value === 'string' ? [value] : [...value]));
  });
  return { clearTargeting, getTargeting, setTargeting, values };
}

describe('owner-aware targeting journal', () => {
  it('restores the exact publisher predecessor after the current TS owner releases', () => {
    const service = createTargetingService();
    const slot = {};
    const targeting = createTargetingHarness({ key: ['publisher'] });
    const frame = service.own(slot, 'key', 'trusted', 'owner-one', targeting);
    expect(frame).toBeDefined();
    expect(targeting.values.get('key')).toEqual(['trusted']);

    frame?.release();

    expect(targeting.values.get('key')).toEqual(['publisher']);
    expect(targeting.setTargeting).toHaveBeenLastCalledWith('key', ['publisher']);
  });

  it('keeps equal-string generations distinct and rebases non-top release without a GPT write', () => {
    const service = createTargetingService();
    const slot = {};
    const targeting = createTargetingHarness({ key: ['publisher'] });
    const older = service.own(slot, 'key', 'same', 'older', targeting);
    const newer = service.own(slot, 'key', 'same', 'newer', targeting);
    targeting.setTargeting.mockClear();

    older?.release();
    expect(targeting.setTargeting).not.toHaveBeenCalled();
    expect(targeting.clearTargeting).not.toHaveBeenCalled();
    newer?.release();

    expect(targeting.values.get('key')).toEqual(['publisher']);
    expect(targeting.setTargeting).toHaveBeenCalledExactlyOnceWith('key', ['publisher']);
  });

  it.each(['same', 'different'] as const)(
    'invalidates the restoration chain before a publisher %s-value write',
    (publisherValue) => {
      const service = createTargetingService();
      const slot = {};
      const targeting = createTargetingHarness({ key: ['publisher'] });
      const frame = service.own(slot, 'key', 'same', 'owner', targeting);

      service.invalidatePublisherMutation(slot, 'key');
      targeting.setTargeting('key', publisherValue === 'same' ? 'same' : 'publisher-new');
      targeting.setTargeting.mockClear();
      frame?.release();

      expect(targeting.setTargeting).not.toHaveBeenCalled();
      expect(targeting.clearTargeting).not.toHaveBeenCalled();
      expect(targeting.values.get('key')).toEqual([
        publisherValue === 'same' ? 'same' : 'publisher-new',
      ]);
    }
  );

  it('invalidates one key or all keys for publisher clear operations', () => {
    const service = createTargetingService();
    const slot = {};
    const targeting = createTargetingHarness({ one: ['publisher-one'], two: ['publisher-two'] });
    const one = service.own(slot, 'one', 'ts-one', 'owner', targeting);
    const two = service.own(slot, 'two', 'ts-two', 'owner', targeting);
    service.invalidatePublisherMutation(slot, 'one');
    targeting.clearTargeting('one');
    one?.release();
    expect(targeting.values.get('one')).toBeUndefined();

    service.invalidatePublisherMutation(slot);
    targeting.clearTargeting();
    two?.release();
    expect(targeting.values.size).toBe(0);
  });

  it('drops a stale chain instead of overwriting a publisher mutation before the next TS write', () => {
    const service = createTargetingService();
    const slot = {};
    const targeting = createTargetingHarness({ key: ['publisher'] });
    const stale = service.own(slot, 'key', 'old-ts', 'old-owner', targeting);
    targeting.setTargeting('key', 'publisher-race');
    const current = service.own(slot, 'key', 'new-ts', 'new-owner', targeting);
    stale?.release();
    current?.release();

    expect(targeting.values.get('key')).toEqual(['publisher-race']);
  });

  it('preserves sibling-key journals when a stale key is replaced', () => {
    const service = createTargetingService();
    const slot = {};
    const targeting = createTargetingHarness({ one: ['publisher-one'], two: ['publisher-two'] });
    const stale = service.own(slot, 'one', 'old-one', 'old-owner', targeting);
    const sibling = service.own(slot, 'two', 'trusted-two', 'sibling-owner', targeting);
    targeting.setTargeting('one', 'publisher-race');

    const current = service.own(slot, 'one', 'new-one', 'new-owner', targeting);
    expect(service.snapshotForTest()).toEqual({ frames: 2, slots: 1 });
    stale?.release();
    current?.release();
    sibling?.release();

    expect(targeting.values.get('one')).toEqual(['publisher-race']);
    expect(targeting.values.get('two')).toEqual(['publisher-two']);
    expect(service.snapshotForTest()).toEqual({ frames: 0, slots: 0 });
  });

  it('rolls back publication when setTargeting throws and contains cleanup failures', () => {
    const service = createTargetingService();
    const slot = {};
    const targeting = createTargetingHarness({ key: ['publisher'] });
    targeting.setTargeting.mockImplementationOnce(() => {
      throw new Error('set failed');
    });

    expect(() => service.own(slot, 'key', 'ts', 'owner', targeting)).toThrow('set failed');
    expect(service.snapshotForTest()).toEqual({ frames: 0, slots: 0 });

    const frame = service.own(slot, 'key', 'ts', 'owner', targeting);
    targeting.setTargeting.mockImplementationOnce(() => {
      throw new Error('restore failed');
    });
    expect(() => frame?.release()).not.toThrow();
    expect(service.snapshotForTest()).toEqual({ frames: 1, slots: 1 });
    service.disposeOwner('owner');
    expect(service.snapshotForTest()).toEqual({ frames: 0, slots: 0 });
  });

  it('uses the real adapter to invalidate before publisher set, per-key clear, and clear-all', async () => {
    const values = new Map<string, readonly string[]>([['key', ['publisher']]]);
    const slot = {
      clearTargeting: vi.fn((key?: string) => {
        if (key === undefined) values.clear();
        else values.delete(key);
      }),
      getTargeting: vi.fn((key: string) => Object.freeze([...(values.get(key) ?? [])])),
      setTargeting: vi.fn((key: string, value: string | readonly string[]) => {
        values.set(key, Object.freeze(typeof value === 'string' ? [value] : [...value]));
      }),
    };
    const serviceObject = {
      addEventListener: vi.fn(),
      getSlots: () => [slot],
      refresh: vi.fn(),
      removeEventListener: vi.fn(),
    };
    const googletag = {
      apiReady: true,
      cmd: { push: (command: () => void) => command() },
      display: vi.fn(),
      pubads: () => serviceObject,
      pubadsReady: true,
    };
    const adapter = createBrowserGoogletagAdapter({ googletag });
    const service = createTargetingService();
    const observation = service.observePublisherMutations(slot, adapter);
    await expect(observation.result).resolves.toBeUndefined();
    const write = adapter.run((gpt) =>
      service.own(slot, 'key', 'trusted', 'owner', {
        clearTargeting: (key) => gpt.clearTargeting(slot, key),
        getTargeting: (key) => gpt.getTargeting(slot, key),
        setTargeting: (key, value) => gpt.setTargeting(slot, key, value),
      })
    );
    const frame = await write.result;
    expect(values.get('key')).toEqual(['trusted']);

    slot.setTargeting('key', 'publisher-new');
    frame?.release();
    expect(values.get('key')).toEqual(['publisher-new']);

    const perKey = await adapter.run((gpt) =>
      service.own(slot, 'key', 'trusted-two', 'owner-two', {
        clearTargeting: (key) => gpt.clearTargeting(slot, key),
        getTargeting: (key) => gpt.getTargeting(slot, key),
        setTargeting: (key, value) => gpt.setTargeting(slot, key, value),
      })
    ).result;
    slot.clearTargeting('key');
    perKey?.release();
    expect(values.get('key')).toBeUndefined();

    values.set('key', ['publisher-three']);
    const clearAll = await adapter.run((gpt) =>
      service.own(slot, 'key', 'trusted-three', 'owner-three', {
        clearTargeting: (key) => gpt.clearTargeting(slot, key),
        getTargeting: (key) => gpt.getTargeting(slot, key),
        setTargeting: (key, value) => gpt.setTargeting(slot, key, value),
      })
    ).result;
    slot.clearTargeting();
    clearAll?.release();
    expect(values.size).toBe(0);
  });

  it.each(['same_set', 'different_set', 'per_key_clear', 'clear_all'] as const)(
    'invalidates after publisher wrapper replacement for %s without calling that replacement on release',
    async (mutation) => {
      const values = new Map<string, readonly string[]>([
        ['key', ['publisher']],
        ['sibling', ['publisher-sibling']],
      ]);
      const slot = {
        clearTargeting: vi.fn((key?: string) => {
          if (key === undefined) values.clear();
          else values.delete(key);
        }),
        getTargeting: vi.fn((key: string) => Object.freeze([...(values.get(key) ?? [])])),
        setTargeting: vi.fn((key: string, value: string | readonly string[]) => {
          values.set(key, Object.freeze(typeof value === 'string' ? [value] : [...value]));
        }),
      };
      const adapter = adapterForTargetingSlot(slot);
      const service = createTargetingService();
      await expect(
        service.observePublisherMutations(slot, adapter).result
      ).resolves.toBeUndefined();
      const frame = await adapter.run((gpt) =>
        service.own(slot, 'key', 'trusted', 'owner', {
          clearTargeting: (key) => gpt.clearTargeting(slot, key),
          getTargeting: (key) => gpt.getTargeting(slot, key),
          setTargeting: (key, value) => gpt.setTargeting(slot, key, value),
        })
      ).result;

      const publisherSet = vi.fn((key: string, value: string | readonly string[]) => {
        values.set(key, Object.freeze(typeof value === 'string' ? [value] : [...value]));
      });
      const publisherClear = vi.fn((key?: string) => {
        if (key === undefined) values.clear();
        else values.delete(key);
      });
      if (mutation === 'same_set' || mutation === 'different_set') {
        slot.setTargeting = publisherSet;
        slot.setTargeting('key', mutation === 'same_set' ? 'trusted' : 'publisher-new');
      } else {
        slot.clearTargeting = publisherClear;
        slot.clearTargeting(mutation === 'per_key_clear' ? 'key' : undefined);
      }

      frame?.release();

      expect(publisherSet).toHaveBeenCalledTimes(
        mutation === 'same_set' || mutation === 'different_set' ? 1 : 0
      );
      expect(publisherClear).toHaveBeenCalledTimes(
        mutation === 'per_key_clear' || mutation === 'clear_all' ? 1 : 0
      );
      if (mutation === 'same_set') expect(values.get('key')).toEqual(['trusted']);
      else if (mutation === 'different_set') expect(values.get('key')).toEqual(['publisher-new']);
      else expect(values.get('key')).toBeUndefined();
      if (mutation === 'clear_all') expect(values.size).toBe(0);
      expect(service.snapshotForTest()).toEqual({ frames: 0, slots: 0 });
    }
  );
});

function adapterForTargetingSlot(slot: object) {
  const pubads = {
    addEventListener: vi.fn(),
    getSlots: () => [slot],
    refresh: vi.fn(),
    removeEventListener: vi.fn(),
  };
  return createBrowserGoogletagAdapter({
    googletag: {
      apiReady: true,
      cmd: { push: (command: () => void) => command() },
      display: vi.fn(),
      pubads: () => pubads,
      pubadsReady: true,
    },
  });
}

describe('adapter-owned targeting interception', () => {
  it('suppresses TS facade writes and preserves publisher order, arguments, return, and throw', async () => {
    const order: string[] = [];
    const publisherError = new Error('native clear failed');
    const setTargeting = vi.fn((key: string, value: string) => {
      order.push(`native-set:${key}:${value}`);
      return 'native-result';
    });
    const clearTargeting = vi.fn(() => {
      order.push('native-clear');
      throw publisherError;
    });
    const slot = { clearTargeting, getTargeting: () => [], setTargeting };
    const adapter = adapterForTargetingSlot(slot);
    const observer = vi.fn((_slot: object, key?: string) => order.push(`observer:${key ?? '*'}`));
    const operation = adapter.run((gpt) => {
      gpt.observeTargeting(slot, { beforePublisherMutation: observer });
      gpt.setTargeting(slot, 'ts-key', 'ts-value');
    });
    await expect(operation.result).resolves.toBeUndefined();
    expect(observer).not.toHaveBeenCalled();
    order.length = 0;

    expect(slot.setTargeting('publisher-key', 'publisher-value')).toBe('native-result');
    expect(order).toEqual(['observer:publisher-key', 'native-set:publisher-key:publisher-value']);
    order.length = 0;
    expect(() => slot.clearTargeting()).toThrow(publisherError);
    expect(order).toEqual(['observer:*', 'native-clear']);
  });

  it('uses one wrapper with independent observers and restores exactly after out-of-order release', async () => {
    const originalSet = vi.fn();
    const originalClear = vi.fn();
    const slot = {
      clearTargeting: originalClear,
      getTargeting: () => [],
      setTargeting: originalSet,
    };
    const adapter = adapterForTargetingSlot(slot);
    const first = vi.fn();
    const second = vi.fn();
    const releases = await adapter.run(
      (gpt) =>
        [
          gpt.observeTargeting(slot, { beforePublisherMutation: first }),
          gpt.observeTargeting(slot, { beforePublisherMutation: second }),
        ] as const
    ).result;
    const installedSet = slot.setTargeting;

    slot.setTargeting('both', 'value');
    expect(first).toHaveBeenCalledOnce();
    expect(second).toHaveBeenCalledOnce();
    releases[0]();
    expect(slot.setTargeting).toBe(installedSet);
    slot.setTargeting('second', 'value');
    expect(first).toHaveBeenCalledOnce();
    expect(second).toHaveBeenCalledTimes(2);
    releases[1]();

    expect(slot.setTargeting).toBe(originalSet);
    expect(slot.clearTargeting).toBe(originalClear);
    slot.setTargeting('native', 'value');
    expect(second).toHaveBeenCalledTimes(2);
  });

  it('reports wrapper replacement fail-closed and never overwrites a publisher replacement', async () => {
    const originalSet = vi.fn();
    const originalClear = vi.fn();
    const replacementSet = vi.fn();
    const target = {
      clearTargeting: originalClear,
      getTargeting: () => [],
      setTargeting: originalSet,
    };
    let trapDescriptors = false;
    const slot = new Proxy(target, {
      getOwnPropertyDescriptor: (current, key) => {
        if (trapDescriptors) throw new Error('publisher descriptor trap');
        return Reflect.getOwnPropertyDescriptor(current, key);
      },
    });
    const adapter = adapterForTargetingSlot(slot);
    const observation = await adapter.run((gpt) =>
      gpt.observeTargeting(slot, { beforePublisherMutation: vi.fn() })
    ).result;

    expect(observation.isCurrent()).toBe(true);
    target.setTargeting = replacementSet;
    expect(observation.isCurrent()).toBe(false);
    observation();
    expect(target.setTargeting).toBe(replacementSet);
    expect(target.clearTargeting).toBe(originalClear);

    const trapped = await adapter.run((gpt) =>
      gpt.observeTargeting(slot, { beforePublisherMutation: vi.fn() })
    ).result;
    trapDescriptors = true;
    expect(() => trapped.isCurrent()).not.toThrow();
    expect(trapped.isCurrent()).toBe(false);
    expect(() => trapped()).not.toThrow();
  });

  it('rolls back the first method when transactional observer installation cannot wrap the second', async () => {
    const originalSet = vi.fn();
    const originalClear = vi.fn();
    const slot = { getTargeting: () => [], setTargeting: originalSet } as unknown as {
      clearTargeting: () => void;
      getTargeting: () => readonly string[];
      setTargeting: (key: string, value: string) => void;
    };
    Object.defineProperty(slot, 'clearTargeting', {
      configurable: false,
      value: originalClear,
      writable: false,
    });
    const adapter = adapterForTargetingSlot(slot);
    const operation = adapter.run((gpt) =>
      gpt.observeTargeting(slot, { beforePublisherMutation: vi.fn() })
    );

    await expect(operation.result).rejects.toMatchObject({
      code: 'external_artifact_incompatible',
    });
    expect(slot.setTargeting).toBe(originalSet);
    expect(slot.clearTargeting).toBe(originalClear);
  });

  it.each(['false', 'throw'] as const)(
    'compare-restores setTargeting when a Proxy define trap mutates then returns %s',
    async (failure) => {
      const originalSet = vi.fn();
      const originalClear = vi.fn();
      const target = {
        clearTargeting: originalClear,
        getTargeting: () => [],
        setTargeting: originalSet,
      };
      let attempted = false;
      const slot = new Proxy(target, {
        defineProperty: (current, key, descriptor) => {
          const result = Reflect.defineProperty(current, key, descriptor);
          if (key === 'setTargeting' && !attempted) {
            attempted = true;
            if (failure === 'throw') throw new Error('mutated then threw');
            return false;
          }
          return result;
        },
      });
      const adapter = adapterForTargetingSlot(slot);
      const operation = adapter.run((gpt) =>
        gpt.observeTargeting(slot, { beforePublisherMutation: vi.fn() })
      );

      await expect(operation.result).rejects.toMatchObject({
        code: 'external_artifact_incompatible',
      });
      expect(target.setTargeting).toBe(originalSet);
      expect(target.clearTargeting).toBe(originalClear);
    }
  );

  it.each(['false', 'throw'] as const)(
    'restores both wrappers when the clearTargeting Proxy define trap mutates then returns %s',
    async (failure) => {
      const originalSet = vi.fn();
      const originalClear = vi.fn();
      const target = {
        clearTargeting: originalClear,
        getTargeting: () => [],
        setTargeting: originalSet,
      };
      let attempted = false;
      const slot = new Proxy(target, {
        defineProperty: (current, key, descriptor) => {
          const result = Reflect.defineProperty(current, key, descriptor);
          if (key === 'clearTargeting' && !attempted) {
            attempted = true;
            if (failure === 'throw') throw new Error('mutated then threw');
            return false;
          }
          return result;
        },
      });
      const adapter = adapterForTargetingSlot(slot);
      const operation = adapter.run((gpt) =>
        gpt.observeTargeting(slot, { beforePublisherMutation: vi.fn() })
      );

      await expect(operation.result).rejects.toMatchObject({
        code: 'external_artifact_incompatible',
      });
      expect(target.setTargeting).toBe(originalSet);
      expect(target.clearTargeting).toBe(originalClear);
    }
  );

  it('lets one observation dispose its wrappers after its adapter operation settled', async () => {
    const originalSet = vi.fn();
    const originalClear = vi.fn();
    const slot = {
      clearTargeting: originalClear,
      getTargeting: () => [],
      setTargeting: originalSet,
    };
    const adapter = adapterForTargetingSlot(slot);
    const service = createTargetingService();
    const observation = service.observePublisherMutations(slot, adapter);
    await expect(observation.result).resolves.toBeUndefined();
    expect(slot.setTargeting).not.toBe(originalSet);

    observation.dispose();

    expect(slot.setTargeting).toBe(originalSet);
    expect(slot.clearTargeting).toBe(originalClear);
  });
});

describe('targeting mutate-then-throw recovery', () => {
  it('rejects a successful no-op write and removes only its failed frame', () => {
    const service = createTargetingService();
    const slot = {};
    const targeting = createTargetingHarness({ key: ['publisher'] });
    const older = service.own(slot, 'key', 'older', 'older-owner', targeting);
    targeting.setTargeting.mockImplementationOnce(() => undefined);

    expect(() => service.own(slot, 'key', 'newer', 'newer-owner', targeting)).toThrow(
      'GPT targeting postcondition failed'
    );
    expect(targeting.values.get('key')).toEqual(['older']);
    expect(service.snapshotForTest()).toEqual({ frames: 1, slots: 1 });
    older?.release();
    expect(targeting.values.get('key')).toEqual(['publisher']);
    expect(service.snapshotForTest()).toEqual({ frames: 0, slots: 0 });
  });

  it('retains owner-disposable quarantine when a successful write leaves the wrong value', () => {
    const service = createTargetingService();
    const slot = {};
    const targeting = createTargetingHarness({ key: ['publisher'] });
    targeting.setTargeting.mockImplementationOnce((key) => {
      targeting.values.set(key, Object.freeze(['wrong-value']));
    });

    expect(() => service.own(slot, 'key', 'trusted', 'owner', targeting)).toThrow(
      'GPT targeting postcondition failed'
    );
    expect(service.snapshotForTest()).toEqual({ frames: 1, slots: 1 });
    service.disposeOwner('owner');
    expect(targeting.values.get('key')).toEqual(['wrong-value']);
    expect(service.snapshotForTest()).toEqual({ frames: 0, slots: 0 });
  });

  it('restores the publisher predecessor when installation mutates then throws', () => {
    const service = createTargetingService();
    const slot = {};
    const targeting = createTargetingHarness({ key: ['publisher'] });
    targeting.setTargeting.mockImplementationOnce((key, value) => {
      targeting.values.set(key, Object.freeze(typeof value === 'string' ? [value] : [...value]));
      throw new Error('mutated then threw');
    });

    expect(() => service.own(slot, 'key', 'trusted', 'owner', targeting)).toThrow(
      'mutated then threw'
    );
    expect(targeting.values.get('key')).toEqual(['publisher']);
    expect(service.snapshotForTest()).toEqual({ frames: 0, slots: 0 });
  });

  it('retains owner-disposable quarantine when failed restoration did not mutate', () => {
    const service = createTargetingService();
    const slot = {};
    const targeting = createTargetingHarness({ key: ['publisher'] });
    const frame = service.own(slot, 'key', 'trusted', 'owner', targeting);
    targeting.setTargeting.mockImplementationOnce(() => {
      throw new Error('failed before mutation');
    });

    frame?.release();

    expect(targeting.values.get('key')).toEqual(['trusted']);
    expect(service.snapshotForTest()).toEqual({ frames: 1, slots: 1 });
    service.disposeOwner('owner');
    expect(targeting.values.get('key')).toEqual(['publisher']);
    expect(service.snapshotForTest()).toEqual({ frames: 0, slots: 0 });
  });

  it('removes ownership when restoration mutates to the predecessor and then throws', () => {
    const service = createTargetingService();
    const slot = {};
    const targeting = createTargetingHarness({ key: ['publisher'] });
    const frame = service.own(slot, 'key', 'trusted', 'owner', targeting);
    targeting.setTargeting.mockImplementationOnce((key, value) => {
      targeting.values.set(key, Object.freeze(typeof value === 'string' ? [value] : [...value]));
      throw new Error('mutated then threw');
    });

    frame?.release();

    expect(targeting.values.get('key')).toEqual(['publisher']);
    expect(service.snapshotForTest()).toEqual({ frames: 0, slots: 0 });
  });

  it('retains an owner-disposable frame when post-failure state cannot be read', () => {
    const service = createTargetingService();
    const slot = {};
    const targeting = createTargetingHarness({ key: ['publisher'] });
    targeting.setTargeting.mockImplementationOnce((key, value) => {
      targeting.values.set(key, Object.freeze(typeof value === 'string' ? [value] : [...value]));
      throw new Error('mutated then threw');
    });
    targeting.getTargeting
      .mockImplementationOnce(() => ['publisher'])
      .mockImplementationOnce(() => {
        throw new Error('unreadable after failure');
      });

    expect(() => service.own(slot, 'key', 'trusted', 'owner', targeting)).toThrow(
      'mutated then threw'
    );
    expect(service.snapshotForTest()).toEqual({ frames: 1, slots: 1 });
    targeting.getTargeting.mockImplementation((key: string) =>
      Object.freeze([...(targeting.values.get(key) ?? [])])
    );
    service.disposeOwner('owner');
    expect(service.snapshotForTest()).toEqual({ frames: 0, slots: 0 });
  });

  it('retains owner-disposable quarantine when failed installation leaves unknown state', () => {
    const service = createTargetingService();
    const slot = {};
    const targeting = createTargetingHarness({ key: ['publisher'] });
    targeting.setTargeting.mockImplementationOnce((key) => {
      targeting.values.set(key, Object.freeze(['publisher-interference']));
      throw new Error('mutated unpredictably then threw');
    });

    expect(() => service.own(slot, 'key', 'trusted', 'owner', targeting)).toThrow(
      'mutated unpredictably then threw'
    );
    expect(service.snapshotForTest()).toEqual({ frames: 1, slots: 1 });
    service.disposeOwner('owner');
    expect(targeting.values.get('key')).toEqual(['publisher-interference']);
    expect(service.snapshotForTest()).toEqual({ frames: 0, slots: 0 });
  });

  it('rolls back only a newer failed publication when the older TS value never changed', () => {
    const service = createTargetingService();
    const slot = {};
    const targeting = createTargetingHarness({ key: ['publisher'] });
    const older = service.own(slot, 'key', 'older', 'older-owner', targeting);
    targeting.setTargeting.mockImplementationOnce(() => {
      throw new Error('newer failed before mutation');
    });

    expect(() => service.own(slot, 'key', 'newer', 'newer-owner', targeting)).toThrow(
      'newer failed before mutation'
    );
    expect(targeting.values.get('key')).toEqual(['older']);
    expect(service.snapshotForTest()).toEqual({ frames: 1, slots: 1 });
    older?.release();
    expect(targeting.values.get('key')).toEqual(['publisher']);
    expect(service.snapshotForTest()).toEqual({ frames: 0, slots: 0 });
  });

  it('releases service observation ownership when adapter promotion rejects', async () => {
    const externalRelease = vi.fn();
    const facade = {
      observeTargeting: () => externalRelease,
    } as never;
    const adapter = {
      run: (command: (gpt: never) => void) => {
        command(facade);
        return Object.freeze({
          status: 'incompatible' as const,
          result: Promise.reject(new Error('promotion rejected')),
          dispose: vi.fn(),
        });
      },
    } as never;
    const service = createTargetingService();
    const observation = service.observePublisherMutations({}, adapter);

    await expect(observation.result).rejects.toThrow('promotion rejected');
    expect(externalRelease).toHaveBeenCalledOnce();
    service.dispose();
    expect(externalRelease).toHaveBeenCalledOnce();
  });

  it('disposes frames through captured Set iterator next after prototype poisoning', () => {
    const service = createTargetingService();
    const targeting = createTargetingHarness({ key: ['publisher'] });
    service.own({}, 'key', 'trusted', 'owner', targeting);
    const iteratorPrototype = Object.getPrototypeOf(new Set().values()) as {
      next: () => IteratorResult<unknown>;
    };
    const originalNext = iteratorPrototype.next;
    iteratorPrototype.next = () => {
      throw new Error('poisoned iterator');
    };
    try {
      expect(() => service.disposeOwner('owner')).not.toThrow();
    } finally {
      iteratorPrototype.next = originalNext;
    }
    expect(service.snapshotForTest()).toEqual({ frames: 0, slots: 0 });
  });
});
