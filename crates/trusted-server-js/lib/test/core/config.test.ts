import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';

const LOG_LEVEL_KEY = Symbol.for('trusted-server.logLevel');
const sharedGlobal = globalThis as typeof globalThis & { [LOG_LEVEL_KEY]?: unknown };

describe('config', () => {
  beforeEach(() => {
    delete sharedGlobal[LOG_LEVEL_KEY];
    vi.resetModules();
  });

  afterEach(() => {
    delete sharedGlobal[LOG_LEVEL_KEY];
  });

  it('sets and gets config, controls log level', async () => {
    const { setConfig, getConfig } = await import('../../src/core/config');
    const { log } = await import('../../src/core/log');

    setConfig({ a: 1 });
    expect(getConfig()).toMatchObject({ a: 1 });

    setConfig({ debug: true });
    expect(log.getLevel()).toBe('debug');

    setConfig({ logLevel: 'info' });
    expect(log.getLevel()).toBe('info');
  });
});
