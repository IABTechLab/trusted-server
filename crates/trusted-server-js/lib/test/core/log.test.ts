import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const LOG_LEVEL_KEY = Symbol.for('trusted-server.logLevel');
const sharedGlobal = globalThis as typeof globalThis & {
  [LOG_LEVEL_KEY]?: unknown;
};

describe('shared logger', () => {
  beforeEach(() => {
    delete sharedGlobal[LOG_LEVEL_KEY];
    vi.resetModules();
  });

  afterEach(() => {
    delete sharedGlobal[LOG_LEVEL_KEY];
    vi.restoreAllMocks();
  });

  it('defaults to warn', async () => {
    const { log } = await import('../../src/core/log');
    expect(log.getLevel()).toBe('warn');
  });

  it('shares level changes between separately loaded module instances', async () => {
    const first = await import('../../src/core/log');
    first.log.setLevel('debug');

    vi.resetModules();
    const second = await import('../../src/core/log');
    expect(second.log.getLevel()).toBe('debug');
    second.log.setLevel('silent');
    expect(first.log.getLevel()).toBe('silent');
  });

  it.each([
    ['silent', []],
    ['error', ['error']],
    ['warn', ['error', 'warn']],
    ['info', ['error', 'warn', 'info']],
    ['debug', ['error', 'warn', 'info', 'debug']],
  ] as const)('gates all console methods at %s', async (level, enabled) => {
    const spies = {
      error: vi.spyOn(console, 'error').mockImplementation(() => {}),
      warn: vi.spyOn(console, 'warn').mockImplementation(() => {}),
      info: vi.spyOn(console, 'info').mockImplementation(() => {}),
      debug: vi.spyOn(console, 'log').mockImplementation(() => {}),
    };
    const { log } = await import('../../src/core/log');
    log.setLevel(level);
    for (const method of ['error', 'warn', 'info', 'debug'] as const) {
      log[method]('example');
      expect(spies[method]).toHaveBeenCalledTimes(
        enabled.some((value) => value === method) ? 1 : 0
      );
    }
  });
});
