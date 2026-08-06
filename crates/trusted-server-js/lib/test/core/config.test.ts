import { describe, it, expect, beforeEach, vi } from 'vitest';

describe('config', () => {
  beforeEach(async () => {
    // reset module state between tests
    await vi.resetModules();
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

  it('validates, snapshots, and freezes one exact cache fetch policy', async () => {
    const { parseCacheFetchPolicyV1 } = await import('../../src/core/config');
    const input = {
      version: 1,
      baseUrl: 'https://cache.example:8443/pbc/v1/cache',
    };

    const policy = parseCacheFetchPolicyV1(input);
    input.baseUrl = 'https://mutated.example/cache';

    expect(policy).toEqual({
      version: 1,
      baseUrl: 'https://cache.example:8443/pbc/v1/cache',
    });
    expect(Object.isFrozen(policy)).toBe(true);
  });

  it('rejects malformed cache policies before integration preparation', async () => {
    const { parseCacheFetchPolicyV1 } = await import('../../src/core/config');
    const accessor = { version: 1 } as { version: number; baseUrl?: string };
    Object.defineProperty(accessor, 'baseUrl', {
      enumerable: true,
      get: () => 'https://cache.example/pbc/v1/cache',
    });
    const inherited = Object.create({ inherited: true }) as {
      version: number;
      baseUrl: string;
    };
    inherited.version = 1;
    inherited.baseUrl = 'https://cache.example/pbc/v1/cache';

    for (const value of [
      { version: 1, baseUrl: 'http://cache.example/pbc/v1/cache' },
      { version: 1, baseUrl: 'https://user@cache.example/pbc/v1/cache' },
      { version: 1, baseUrl: 'https://cache.example/' },
      { version: 1, baseUrl: 'https://cache.example/pbc/v1/cache?existing=1' },
      { version: 1, baseUrl: 'https://cache.example/pbc/v1/cache#fragment' },
      { version: 2, baseUrl: 'https://cache.example/pbc/v1/cache' },
      { version: 1, baseUrl: 'https://cache.example/pbc/v1/cache', extra: true },
      accessor,
      inherited,
    ]) {
      expect(parseCacheFetchPolicyV1(value)).toBeUndefined();
    }
  });
});
