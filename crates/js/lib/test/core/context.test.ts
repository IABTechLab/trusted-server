import { describe, it, expect, beforeEach, vi } from 'vitest';

describe('context provider registry', () => {
  beforeEach(async () => {
    await vi.resetModules();
  });

  it('returns empty context when no providers registered', async () => {
    const { collectContext } = await import('../../src/core/context');
    expect(collectContext()).toEqual({});
  });

  it('collects data from a single provider', async () => {
    const { registerContextProvider, collectContext } = await import('../../src/core/context');
    registerContextProvider(() => ({ foo: 'bar' }));
    expect(collectContext()).toEqual({ foo: 'bar' });
  });

  it('merges data from multiple providers', async () => {
    const { registerContextProvider, collectContext } = await import('../../src/core/context');
    registerContextProvider(() => ({ a: 1 }));
    registerContextProvider(() => ({ b: 2 }));
    expect(collectContext()).toEqual({ a: 1, b: 2 });
  });

  it('later providers overwrite earlier ones on key collision', async () => {
    const { registerContextProvider, collectContext } = await import('../../src/core/context');
    registerContextProvider(() => ({ key: 'first' }));
    registerContextProvider(() => ({ key: 'second' }));
    expect(collectContext()).toEqual({ key: 'second' });
  });

  it('skips providers that return undefined', async () => {
    const { registerContextProvider, collectContext } = await import('../../src/core/context');
    registerContextProvider(() => undefined);
    registerContextProvider(() => ({ kept: true }));
    expect(collectContext()).toEqual({ kept: true });
  });

  it('skips providers that throw', async () => {
    const { registerContextProvider, collectContext } = await import('../../src/core/context');
    registerContextProvider(() => {
      throw new Error('boom');
    });
    registerContextProvider(() => ({ survived: true }));
    expect(collectContext()).toEqual({ survived: true });
  });
});
