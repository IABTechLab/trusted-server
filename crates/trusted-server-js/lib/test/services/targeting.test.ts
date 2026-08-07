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
});
