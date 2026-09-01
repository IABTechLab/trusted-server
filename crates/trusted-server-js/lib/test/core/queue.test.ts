import { afterEach, describe, expect, it, vi } from 'vitest';

import {
  canPublishTerminalFields,
  commitQueue,
  prepareQueue,
  publishQueue,
} from '../../src/core/queue';
import { log } from '../../src/core/log';

describe('terminal queue handoff', () => {
  afterEach(() => vi.restoreAllMocks());

  it('snapshots callable data entries, commits atomically, then drains FIFO with isolation', () => {
    const calls: string[] = [];
    const warning = vi.spyOn(log, 'warn').mockImplementation(() => undefined);
    const target: { que?: unknown; ready?: boolean } = {
      que: [
        function (this: unknown) {
          expect(this).toBe(target);
          calls.push('first');
          (target.que as unknown[]).push(() => calls.push('nested'));
        },
        'ignored',
        () => {
          calls.push('throwing');
          throw new Error('publisher callback');
        },
        () => calls.push('last'),
      ],
    };
    const ingress = prepareQueue(target);

    const published = publishQueue(target, ingress, { ready: true });
    (target.que as unknown[]).push(() => calls.push('after-commit'));
    published.drain();
    published.drain();
    const queue = published.queue;

    expect(calls).toEqual(['after-commit', 'first', 'nested', 'throwing', 'last']);
    expect(warning).toHaveBeenCalledTimes(1);
    expect(target.ready).toBe(true);
    expect(target.que).toBe(queue);
    expect(Array.isArray(queue)).toBe(true);
    expect(queue).toHaveLength(0);
    expect(Object.prototype.hasOwnProperty.call(queue, 'push')).toBe(true);
    expect(Object.isFrozen(queue)).toBe(true);

    ingress.push(() => calls.push('retained'));
    expect(calls[calls.length - 1]).toBe('retained');
    expect(Object.isFrozen(ingress)).toBe(true);
    expect(Reflect.set(ingress, 'push', Array.prototype.push)).toBe(false);
    expect(() => Array.prototype.push.call(ingress, () => calls.push('lost'))).toThrow(TypeError);
    expect(calls).not.toContain('lost');
    expect(ingress).toHaveLength(0);
    expect(queue).toHaveLength(0);
  });

  it('preflights terminal fields before changing the ingress queue', () => {
    const callback = vi.fn();
    const target: { que?: unknown; version?: string } = { que: [callback] };
    Object.defineProperty(target, 'version', {
      configurable: false,
      enumerable: true,
      value: 'publisher',
      writable: false,
    });
    const ingress = prepareQueue(target);

    expect(canPublishTerminalFields(target, { version: '1.0.0' })).toBe(false);
    expect(() => publishQueue(target, ingress, { version: '1.0.0' })).toThrow(TypeError);
    expect(target.que).toBe(ingress);
    expect(ingress).toHaveLength(1);
    expect(ingress.push).toBe(Array.prototype.push);
    expect(Object.isFrozen(ingress)).toBe(false);
    expect(callback).not.toHaveBeenCalled();
  });

  it('keeps callback isolation independent of logger failures', () => {
    const calls: string[] = [];
    vi.spyOn(log, 'warn').mockImplementation(() => {
      throw new Error('hostile warning sink');
    });
    vi.spyOn(log, 'debug').mockImplementation(() => {
      throw new Error('hostile debug sink');
    });
    const target = {
      que: [
        () => {
          calls.push('throwing');
          throw new Error('publisher callback');
        },
        () => calls.push('last'),
      ],
    };
    const ingress = prepareQueue(target);

    expect(() => commitQueue(target, ingress)).not.toThrow();
    expect(calls).toEqual(['throwing', 'last']);
  });

  it('makes the committed public fields and queue immutable', () => {
    const target: { que?: unknown; version?: string } = { que: [] };
    const ingress = prepareQueue(target);
    const queue = commitQueue(target, ingress, { version: '1.0.0' });
    const callback = vi.fn();

    expect(() => Array.prototype.push.call(queue, callback)).toThrow(TypeError);
    expect(() => Array.prototype.splice.call(queue, 0, 0, callback)).toThrow(TypeError);
    expect(() => Reflect.set(queue, 0, callback)).not.toThrow();
    expect(Reflect.set(queue, 0, callback)).toBe(false);
    expect(Reflect.set(queue, 'length', 1)).toBe(false);
    expect(Reflect.deleteProperty(queue, 'push')).toBe(false);
    expect(() => Object.defineProperty(queue, '0', { value: callback })).toThrow(TypeError);
    expect(queue).toHaveLength(0);
    expect(callback).not.toHaveBeenCalled();

    expect(Reflect.set(target, 'version', 'changed')).toBe(false);
    expect(Reflect.set(target, 'que', [])).toBe(false);
    expect(Object.getOwnPropertyDescriptor(target, 'que')).toMatchObject({
      configurable: false,
      writable: false,
    });
  });

  it.each([
    ['index assignment', 'queue[0] = callback;', true, false, undefined],
    ['length assignment', 'queue.length = 1;', true, false, undefined],
    ['push deletion', 'delete queue.push;', true, false, undefined],
    ['push replacement', 'queue.push = Array.prototype.push;', true, false, undefined],
    ['inherited native splice', 'queue.splice(0, 0, callback);', true, true, undefined],
    ['borrowed native push', 'Array.prototype.push.call(queue, callback);', true, true, undefined],
    [
      'borrowed native splice',
      'Array.prototype.splice.call(queue, 0, 0, callback);',
      true,
      true,
      undefined,
    ],
    [
      'Object.defineProperty',
      "Object.defineProperty(queue, '0', { value: callback });",
      true,
      true,
      undefined,
    ],
    [
      'Object.defineProperty length',
      "Object.defineProperty(queue, 'length', { value: 1 });",
      true,
      true,
      undefined,
    ],
    [
      'Object.defineProperty push',
      "Object.defineProperty(queue, 'push', { value: Array.prototype.push });",
      true,
      true,
      undefined,
    ],
    [
      'Object.defineProperties',
      'Object.defineProperties(queue, { 0: { value: callback } });',
      true,
      true,
      undefined,
    ],
    [
      'Reflect.defineProperty',
      "return Reflect.defineProperty(queue, '0', { value: callback });",
      false,
      false,
      false,
    ],
    ['Reflect.set index', "return Reflect.set(queue, '0', callback);", false, false, false],
    ['Reflect.set length', "return Reflect.set(queue, 'length', 1);", false, false, false],
    [
      'Reflect.deleteProperty push',
      "return Reflect.deleteProperty(queue, 'push');",
      false,
      false,
      false,
    ],
  ] as const)(
    'rejects terminal %s in strict and sloppy callers',
    (_name, mutation, strictThrows, sloppyThrows, expectedResult) => {
      const target: { que?: unknown } = { que: [] };
      const queue = commitQueue(target, prepareQueue(target));
      const originalPush = queue.push;
      const callback = vi.fn();
      const strictMutation = new Function('queue', 'callback', `'use strict'; ${mutation}`) as (
        queue: unknown[],
        callback: () => void
      ) => void;
      const sloppyMutation = new Function('queue', 'callback', mutation) as (
        queue: unknown[],
        callback: () => void
      ) => unknown;

      if (strictThrows) {
        expect(() => strictMutation(queue, callback)).toThrow(TypeError);
      } else {
        expect(strictMutation(queue, callback)).toBe(expectedResult);
      }
      if (sloppyThrows) {
        expect(() => sloppyMutation(queue, callback)).toThrow(TypeError);
      } else {
        expect(sloppyMutation(queue, callback)).toBe(expectedResult);
      }
      expect(queue).toHaveLength(0);
      expect(queue.push).toBe(originalPush);
      expect(Object.prototype.hasOwnProperty.call(queue, 'push')).toBe(true);
      expect(Object.isFrozen(queue)).toBe(true);
      expect(callback).not.toHaveBeenCalled();
    }
  );

  it('prepares one actual ingress Array without reading hostile entries', () => {
    const getter = vi.fn(() => () => undefined);
    const hostile: unknown[] = [];
    Object.defineProperty(hostile, '0', { configurable: true, enumerable: true, get: getter });
    hostile.length = 1;
    const target: { que?: unknown } = { que: hostile };

    const ingress = prepareQueue(target);
    const queue = commitQueue(target, ingress);

    expect(ingress).toBe(hostile);
    expect(getter).not.toHaveBeenCalled();
    expect(queue).toHaveLength(0);
  });

  it('copies data entries out of a frozen or custom-push publisher Array', () => {
    const callback = vi.fn();
    const hostile = Object.freeze(Object.assign([callback], { push: vi.fn() }));
    const target: { que?: unknown } = { que: hostile };

    const ingress = prepareQueue(target);

    expect(ingress).not.toBe(hostile);
    expect(Array.isArray(ingress)).toBe(true);
    expect(ingress[0]).toBe(callback);
    expect(ingress.push).toBe(Array.prototype.push);
    expect(() => commitQueue(target, ingress)).not.toThrow();
    expect(callback).toHaveBeenCalledOnce();
  });
});
