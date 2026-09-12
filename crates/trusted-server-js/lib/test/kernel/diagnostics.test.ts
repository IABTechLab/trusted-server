import { describe, expect, it, vi } from 'vitest';

import {
  createDiagnosticsIngress,
  type DiagnosticsObservation,
} from '../../src/kernel/diagnostics';

function scalarRecord(valueCount: number): Record<string, unknown> {
  return Object.fromEntries(
    Array.from({ length: valueCount }, (_, index) => [`value${index}`, index])
  );
}

function nestedRecord(depth: number): Record<string, unknown> {
  const root: Record<string, unknown> = {};
  let cursor = root;
  for (let index = 0; index < depth; index += 1) {
    const child: Record<string, unknown> = {};
    cursor['child'] = child;
    cursor = child;
  }
  return root;
}

function primitiveLeafRecord(depth: number, value: unknown): Record<string, unknown> {
  const root: Record<string, unknown> = {};
  let cursor = root;
  for (let currentDepth = 1; currentDepth < depth; currentDepth += 1) {
    const child: Record<string, unknown> = {};
    cursor['child'] = child;
    cursor = child;
  }
  cursor['leaf'] = value;
  return root;
}

describe('kernel diagnostics ingress', () => {
  it('exposes only the exact frozen core-owned facade', () => {
    const ingress = createDiagnosticsIngress({ reduce: vi.fn() });

    expect(Object.isFrozen(ingress)).toBe(true);
    expect(Reflect.ownKeys(ingress).sort()).toEqual(['dispose', 'publish']);
    expect('subscribe' in ingress).toBe(false);
    expect('consumerIds' in ingress).toBe(false);
    expect('capacity' in ingress).toBe(false);
    expect('queue' in ingress).toBe(false);
    expect('scheduler' in ingress).toBe(false);
    expect('timer' in ingress).toBe(false);
    expect('overflow' in ingress).toBe(false);
  });

  it('copies ordinary and null-prototype data trees into fresh deeply frozen values', () => {
    const reduced: DiagnosticsObservation[] = [];
    const ingress = createDiagnosticsIngress({
      reduce: (observation) => reduced.push(observation),
    });
    const nested = Object.assign(Object.create(null) as Record<string, unknown>, {
      label: '診断✓',
    });
    const array = [nested, null, true, 3.25];
    const candidate = { array, name: 'publisher-value' };

    expect(ingress.publish(candidate)).toBe(true);
    expect(reduced).toHaveLength(1);
    const accepted = reduced[0]!;
    expect(accepted).not.toBe(candidate);
    expect(Object.getPrototypeOf(accepted)).toBeNull();
    expect(Object.isFrozen(accepted)).toBe(true);
    expect(accepted['array']).not.toBe(array);
    expect(Object.isFrozen(accepted['array'])).toBe(true);
    const acceptedArray = accepted['array'] as readonly unknown[];
    expect(acceptedArray[0]).not.toBe(nested);
    expect(Object.getPrototypeOf(acceptedArray[0])).toBeNull();
    expect(Object.isFrozen(acceptedArray[0])).toBe(true);
    expect(acceptedArray).toEqual([{ label: '診断✓' }, null, true, 3.25]);
  });

  it('accepts exactly 512 nodes and rejects 513 before reducer entry', () => {
    const reduce = vi.fn();
    const ingress = createDiagnosticsIngress({ reduce });

    expect(ingress.publish(scalarRecord(510))).toBe(true);
    expect(ingress.publish(scalarRecord(511))).toBe(true);
    expect(ingress.publish(scalarRecord(512))).toBe(false);
    expect(reduce).toHaveBeenCalledTimes(2);
  });

  it('accepts depth sixteen and rejects depth seventeen before reducer entry', () => {
    const reduce = vi.fn();
    const ingress = createDiagnosticsIngress({ reduce });

    expect(ingress.publish(nestedRecord(15))).toBe(true);
    expect(ingress.publish(nestedRecord(16))).toBe(true);
    expect(ingress.publish(nestedRecord(17))).toBe(false);
    expect(reduce).toHaveBeenCalledTimes(2);
  });

  it.each([
    [15, null, true],
    [16, false, true],
    [16, 42.25, true],
    [16, '診断✓', true],
    [17, null, false],
    [17, false, false],
    [17, 42.25, false],
    [17, '診断✓', false],
  ])('enforces the depth boundary for primitive leaf depth %i', (depth, value, accepted) => {
    const reduce = vi.fn();
    const ingress = createDiagnosticsIngress({ reduce });

    expect(ingress.publish(primitiveLeafRecord(depth, value))).toBe(accepted);
    expect(reduce).toHaveBeenCalledTimes(accepted ? 1 : 0);
  });

  it('enforces UTF-8 property-name and string byte limits including multibyte input', () => {
    const reduce = vi.fn();
    const ingress = createDiagnosticsIngress({ reduce });
    const property127 = 'a'.repeat(127);
    const property128 = 'é'.repeat(64);
    const property129 = `${'é'.repeat(64)}a`;
    const string4095 = 'a'.repeat(4095);
    const string4096 = 'é'.repeat(2048);
    const string4097 = `${'é'.repeat(2048)}a`;

    expect(ingress.publish({ [property127]: string4095 })).toBe(true);
    expect(ingress.publish({ [property128]: string4096 })).toBe(true);
    expect(ingress.publish({ [property129]: 'value' })).toBe(false);
    expect(ingress.publish({ value: string4097 })).toBe(false);
    expect(reduce).toHaveBeenCalledTimes(2);
  });

  it.each([
    ['sparse array', Object.assign(new Array(2), { 0: 'first' })],
    ['array extra property', Object.assign(['first'], { extra: true })],
    ['undefined', { value: undefined }],
    // @ts-expect-error The runtime supports this hostile input even though the build target does not.
    ['bigint', { value: 1n }],
    ['function', { value: () => undefined }],
    ['symbol value', { value: Symbol('fictional') }],
    ['nonfinite number', { value: Number.POSITIVE_INFINITY }],
    ['custom prototype', Object.freeze(new (class FictionalValue {})())],
  ])('rejects %s values before reducer entry', (_label, candidate) => {
    const reduce = vi.fn();
    const ingress = createDiagnosticsIngress({ reduce });

    expect(ingress.publish(candidate as Record<string, unknown>)).toBe(false);
    expect(reduce).not.toHaveBeenCalled();
  });

  it('rejects aliases, cycles, accessors, symbols, and non-enumerable record fields', () => {
    const reduce = vi.fn();
    const ingress = createDiagnosticsIngress({ reduce });
    const shared = { value: true };
    const cycle: Record<string, unknown> = {};
    cycle['self'] = cycle;
    const accessor = Object.defineProperty({}, 'value', {
      enumerable: true,
      get: vi.fn(() => true),
    });
    const symbol = Object.defineProperty({}, Symbol('fictional'), {
      enumerable: true,
      value: true,
    });
    const hidden = Object.defineProperty({}, 'hidden', {
      enumerable: false,
      value: true,
    });

    expect(ingress.publish({ first: shared, second: shared })).toBe(false);
    expect(ingress.publish(cycle)).toBe(false);
    expect(ingress.publish(accessor)).toBe(false);
    expect(ingress.publish(symbol)).toBe(false);
    expect(ingress.publish(hidden)).toBe(false);
    expect(reduce).not.toHaveBeenCalled();
  });

  it('fails closed on hostile reflection and injected copy or freeze failures', () => {
    const reduce = vi.fn();
    const reportError = vi.fn(() => {
      throw new Error('fictional reporter failure');
    });
    const ingress = createDiagnosticsIngress({ reduce, reportError });
    const hostile = new Proxy(
      {},
      {
        getPrototypeOf: () => {
          throw new Error('fictional prototype trap');
        },
      }
    );

    expect(() => ingress.publish(hostile)).not.toThrow();
    expect(ingress.publish(hostile)).toBe(false);
    const defineProperty = vi.spyOn(Object, 'defineProperty').mockImplementationOnce(() => {
      throw new Error('fictional copy failure');
    });
    expect(ingress.publish({ acceptedShape: true })).toBe(false);
    defineProperty.mockRestore();
    const freeze = vi.spyOn(Object, 'freeze').mockImplementationOnce(() => {
      throw new Error('fictional freeze failure');
    });
    expect(ingress.publish({ acceptedShape: true })).toBe(false);
    freeze.mockRestore();
    expect(reduce).not.toHaveBeenCalled();
  });

  it('returns true after acceptance even when reducer and reporter throw', () => {
    const reportError = vi.fn(() => {
      throw new Error('fictional reporter failure');
    });
    const ingress = createDiagnosticsIngress({
      reduce: () => {
        throw new Error('fictional reducer failure');
      },
      reportError,
    });

    expect(() => ingress.publish({ accepted: true })).not.toThrow();
    expect(ingress.publish({ accepted: true })).toBe(true);
    expect(reportError).toHaveBeenCalledTimes(2);
  });

  it('disposes idempotently and makes retained runtime publishers inert', () => {
    const reduce = vi.fn();
    const ingress = createDiagnosticsIngress({ reduce });
    const retainedPublish = ingress.publish;

    expect(retainedPublish({ sequence: 1 })).toBe(true);
    ingress.dispose();
    ingress.dispose();
    expect(retainedPublish({ sequence: 2 })).toBe(false);
    expect(reduce).toHaveBeenCalledOnce();
  });
});
