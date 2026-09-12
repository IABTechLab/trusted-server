import { afterEach, describe, expect, it, vi } from 'vitest';

import { publicLog as log } from '../../src/kernel/fallback';

describe('log', () => {
  afterEach(() => {
    log.setLevel('warn');
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  it('rejects invalid runtime levels without changing the current level', () => {
    log.setLevel('info');

    expect(() => log.setLevel('verbose' as never)).toThrow(TypeError);
    expect(log.getLevel()).toBe('info');
    expect(log.setLevel('debug')).toBeUndefined();
  });

  it('isolates absent and throwing console methods', () => {
    vi.stubGlobal('console', undefined);
    expect(() => log.warn('absent')).not.toThrow();

    vi.stubGlobal('console', {
      warn: vi.fn(() => {
        throw new Error('host console');
      }),
    });
    expect(() => log.warn('throwing')).not.toThrow();
  });

  it('isolates throwing console and method accessors', () => {
    const originalConsole = Object.getOwnPropertyDescriptor(globalThis, 'console');
    try {
      Object.defineProperty(globalThis, 'console', {
        configurable: true,
        get: () => {
          throw new Error('console accessor');
        },
      });
      expect(() => log.warn('hostile console')).not.toThrow();

      const hostileConsole = {};
      Object.defineProperty(hostileConsole, 'warn', {
        get: () => {
          throw new Error('method accessor');
        },
      });
      Object.defineProperty(globalThis, 'console', {
        configurable: true,
        value: hostileConsole,
        writable: true,
      });
      expect(() => log.warn('hostile method')).not.toThrow();
    } finally {
      if (originalConsole) Object.defineProperty(globalThis, 'console', originalConsole);
      else Reflect.deleteProperty(globalThis, 'console');
    }
  });
});
