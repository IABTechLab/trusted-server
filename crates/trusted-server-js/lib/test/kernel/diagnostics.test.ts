import { afterEach, describe, expect, it, vi } from 'vitest';

import type { BootManifestV1 } from '../../src/core/types';
import { createDiagnosticsBus, type DiagnosticsObservation } from '../../src/kernel/diagnostics';

const RELEASE_ID = 'a'.repeat(64);

function manifest(ids: readonly string[]): BootManifestV1 {
  return Object.freeze({
    version: 1,
    releaseId: RELEASE_ID,
    integrations: Object.freeze(ids.map((id) => Object.freeze({ id, required: true as const }))),
  });
}

function observation(sequence: number): DiagnosticsObservation {
  return Object.freeze({
    kind: 'render',
    sequence,
    value: Object.freeze({ slotId: `slot-${sequence}` }),
  });
}

afterEach(() => {
  vi.useRealTimers();
});

describe('kernel diagnostics bus', () => {
  it('exposes only a frozen private-owner facade', () => {
    const bus = createDiagnosticsBus({ manifest: manifest([]) });

    expect(Object.isFrozen(bus)).toBe(true);
    expect(Reflect.ownKeys(bus).sort()).toEqual(['dispose', 'publish', 'subscribe']);
    expect('listeners' in bus).toBe(false);
    expect('pending' in bus).toBe(false);

    bus.dispose();
  });

  it('admits only one live subscription for an exact manifest member', () => {
    const bus = createDiagnosticsBus({ manifest: manifest(['gpt_diagnostics']) });
    const first = bus.subscribe('gpt_diagnostics', vi.fn());

    expect(first).toEqual(expect.any(Function));
    expect(bus.subscribe('gpt_diagnostics', vi.fn())).toBeUndefined();
    expect(bus.subscribe('not_in_manifest', vi.fn())).toBeUndefined();

    first?.();
    expect(bus.subscribe('gpt_diagnostics', vi.fn())).toEqual(expect.any(Function));
    bus.dispose();
  });

  it('admits sixteen live module identities and rejects a seventeenth without disturbance', () => {
    vi.useFakeTimers();
    const ids = Array.from({ length: 17 }, (_, index) => `module_${index}`);
    const bus = createDiagnosticsBus({ manifest: manifest(ids) });
    const listeners = ids.map(() => vi.fn());

    for (let index = 0; index < 16; index += 1) {
      expect(bus.subscribe(ids[index]!, listeners[index]!)).toEqual(expect.any(Function));
    }
    expect(bus.subscribe(ids[16]!, listeners[16]!)).toBeUndefined();

    expect(bus.publish(observation(1))).toBe(true);
    expect(listeners.every((listener) => listener.mock.calls.length === 0)).toBe(true);
    vi.runOnlyPendingTimers();
    expect(listeners.slice(0, 16).every((listener) => listener.mock.calls.length === 1)).toBe(true);
    expect(listeners[16]).not.toHaveBeenCalled();
    bus.dispose();
  });

  it('delivers frozen observations asynchronously in order and isolates subscriber throws', () => {
    vi.useFakeTimers();
    const errors: unknown[] = [];
    const bus = createDiagnosticsBus({
      manifest: manifest(['thrower', 'observer']),
      onSubscriberError: (error) => errors.push(error),
    });
    const received: number[] = [];
    bus.subscribe('thrower', () => {
      throw new Error('fictional diagnostics failure');
    });
    bus.subscribe('observer', (event) => {
      expect(Object.isFrozen(event)).toBe(true);
      if (typeof event.sequence === 'number') received.push(event.sequence);
    });

    expect(bus.publish(observation(1))).toBe(true);
    expect(bus.publish(observation(2))).toBe(true);
    expect(received).toEqual([]);

    vi.runOnlyPendingTimers();
    expect(received).toEqual([1, 2]);
    expect(errors).toHaveLength(2);
    bus.dispose();
  });

  it('uses publish-time membership while honoring unsubscribe before delivery', () => {
    vi.useFakeTimers();
    const bus = createDiagnosticsBus({ manifest: manifest(['first', 'second']) });
    const first = vi.fn();
    const second = vi.fn();
    const releaseFirst = bus.subscribe('first', first);

    bus.publish(observation(1));
    const releaseSecond = bus.subscribe('second', second);
    releaseFirst?.();
    vi.runOnlyPendingTimers();

    expect(first).not.toHaveBeenCalled();
    expect(second).not.toHaveBeenCalled();

    bus.publish(observation(2));
    vi.runOnlyPendingTimers();
    expect(second).toHaveBeenCalledOnce();
    expect(second).toHaveBeenCalledWith(observation(2));
    releaseSecond?.();
    bus.dispose();
  });

  it('bounds pending delivery and cancels all work on disposal', () => {
    vi.useFakeTimers();
    const bus = createDiagnosticsBus({
      manifest: manifest(['observer']),
      pendingCapacity: 2,
    });
    const listener = vi.fn();
    bus.subscribe('observer', listener);

    bus.publish(observation(1));
    bus.publish(observation(2));
    bus.publish(observation(3));
    vi.runOnlyPendingTimers();

    expect(listener.mock.calls.map(([event]) => event.sequence)).toEqual([2, 3]);

    bus.publish(observation(4));
    bus.dispose();
    vi.runOnlyPendingTimers();
    expect(listener).toHaveBeenCalledTimes(2);
    expect(bus.publish(observation(5))).toBe(false);
    expect(bus.subscribe('observer', vi.fn())).toBeUndefined();
  });

  it('rejects mutable observations without reading them', () => {
    const bus = createDiagnosticsBus({ manifest: manifest([]) });
    const read = vi.fn();
    const mutable = Object.defineProperty({}, 'kind', { enumerable: true, get: read });

    expect(bus.publish(mutable as DiagnosticsObservation)).toBe(false);
    expect(read).not.toHaveBeenCalled();
    bus.dispose();
  });

  it('rejects frozen functions and exotic objects instead of transporting capabilities', () => {
    const bus = createDiagnosticsBus({ manifest: manifest([]) });
    const callable = Object.freeze(() => undefined);
    const exotic = Object.freeze(new (class PublisherSlot {})());

    expect(
      bus.publish(Object.freeze({ kind: 'gpt', slot: callable }) as DiagnosticsObservation)
    ).toBe(false);
    expect(
      bus.publish(Object.freeze({ kind: 'gpt', slot: exotic }) as DiagnosticsObservation)
    ).toBe(false);
    bus.dispose();
  });

  it('commits to the private core observer before asynchronous module delivery', () => {
    vi.useFakeTimers();
    const order: string[] = [];
    const bus = createDiagnosticsBus({
      manifest: manifest(['observer']),
      onObservation: () => {
        order.push('core');
        throw new Error('fictional core observer failure');
      },
    });
    bus.subscribe('observer', () => order.push('module'));

    expect(bus.publish(observation(1))).toBe(true);
    expect(order).toEqual(['core']);
    vi.runOnlyPendingTimers();
    expect(order).toEqual(['core', 'module']);
    bus.dispose();
  });
});
