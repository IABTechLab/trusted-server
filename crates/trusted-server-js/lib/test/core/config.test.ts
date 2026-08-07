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

  it('accepts an exact 4,096-byte cache base URL and rejects the next byte', async () => {
    const { parseCacheFetchPolicyV1 } = await import('../../src/core/config');
    const prefix = 'https://cache.example/';
    const exactBaseUrl = `${prefix}${'x'.repeat(4_096 - prefix.length)}`;
    expect(new TextEncoder().encode(exactBaseUrl)).toHaveLength(4_096);

    expect(parseCacheFetchPolicyV1({ version: 1, baseUrl: exactBaseUrl })).toEqual({
      version: 1,
      baseUrl: exactBaseUrl,
    });
    expect(parseCacheFetchPolicyV1({ version: 1, baseUrl: `${exactBaseUrl}x` })).toBeUndefined();
  });

  it.each([4_095, 4_096, 4_097])(
    'enforces the cache base URL byte boundary for multibyte UTF-8 at %s bytes',
    async (targetBytes) => {
      const { parseCacheFetchPolicyV1 } = await import('../../src/core/config');
      const prefix = 'https://cache.example/';
      const remainingBytes = targetBytes - new TextEncoder().encode(prefix).byteLength;
      const baseUrl = `${prefix}${'é'.repeat(Math.floor(remainingBytes / 2))}${
        remainingBytes % 2 === 0 ? '' : 'x'
      }`;
      expect(new TextEncoder().encode(baseUrl)).toHaveLength(targetBytes);

      if (targetBytes <= 4_096) {
        expect(parseCacheFetchPolicyV1({ version: 1, baseUrl })).toEqual({
          version: 1,
          baseUrl,
        });
      } else {
        expect(parseCacheFetchPolicyV1({ version: 1, baseUrl })).toBeUndefined();
      }
    }
  );

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
