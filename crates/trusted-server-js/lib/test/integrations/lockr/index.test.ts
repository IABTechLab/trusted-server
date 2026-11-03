import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const { log } = vi.hoisted(() => ({
  log: { debug: vi.fn(), info: vi.fn(), warn: vi.fn(), error: vi.fn() },
}));

vi.mock('../../../src/core/log', () => ({ log }));

describe('integrations/lockr initializer', () => {
  let guard: typeof import('../../../src/integrations/lockr/script_guard');
  let apiGuard: typeof import('../../../src/integrations/lockr/api_guard');
  let originalAppendChild: typeof Element.prototype.appendChild;
  let originalInsertBefore: typeof Element.prototype.insertBefore;
  let originalOpen: typeof XMLHttpRequest.prototype.open;

  beforeEach(async () => {
    vi.resetModules();
    vi.clearAllMocks();
    vi.useFakeTimers();
    vi.stubGlobal('identityLockr', undefined);
    vi.stubGlobal('lockr', undefined);
    guard = await import('../../../src/integrations/lockr/script_guard');
    apiGuard = await import('../../../src/integrations/lockr/api_guard');
    guard.resetGuardState();
    apiGuard.resetApiGuardState();
    originalAppendChild = Element.prototype.appendChild;
    originalInsertBefore = Element.prototype.insertBefore;
    originalOpen = XMLHttpRequest.prototype.open;
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    guard.resetGuardState();
    apiGuard.resetApiGuardState();
    vi.restoreAllMocks();
    vi.clearAllTimers();
    vi.useRealTimers();
  });

  it('immediately installs the real guard and restores its hooks on reset', async () => {
    expect(guard.isGuardInstalled()).toBe(false);

    await import('../../../src/integrations/lockr');

    expect(guard.isGuardInstalled()).toBe(true);
    expect(Element.prototype.appendChild).not.toBe(originalAppendChild);
    expect(Element.prototype.insertBefore).not.toBe(originalInsertBefore);
    expect(XMLHttpRequest.prototype.open).not.toBe(originalOpen);

    guard.resetGuardState();
    apiGuard.resetApiGuardState();

    expect(guard.isGuardInstalled()).toBe(false);
    expect(Element.prototype.appendChild).toBe(originalAppendChild);
    expect(Element.prototype.insertBefore).toBe(originalInsertBefore);
    expect(XMLHttpRequest.prototype.open).toBe(originalOpen);
  });

  it('keeps API requests on the publisher origin with an external document base', async () => {
    const base = document.createElement('base');
    base.href = 'https://external-base.example/';
    document.head.appendChild(base);
    const open = vi.spyOn(XMLHttpRequest.prototype, 'open');

    try {
      await import('../../../src/integrations/lockr');
      const request = new XMLHttpRequest();
      request.open('POST', '/integrations/lockr/api/publisher/app/v2/identityLockr/settings');

      expect(open).toHaveBeenCalledWith(
        'POST',
        `${window.location.origin}/integrations/lockr/api/publisher/app/v2/identityLockr/settings`
      );
    } finally {
      base.remove();
    }
  });

  it('does not poll or warn when the SDK is missing', async () => {
    await import('../../../src/integrations/lockr');

    expect(vi.getTimerCount()).toBe(0);
    vi.advanceTimersByTime(3000);
    expect(log.warn).not.toHaveBeenCalled();
    expect(guard.isGuardInstalled()).toBe(true);
  });

  it('leaves an initialized SDK host unchanged', async () => {
    const sdk = { host: '/integrations/lockr/api' };
    vi.stubGlobal('identityLockr', sdk);

    await import('../../../src/integrations/lockr');

    expect(sdk.host).toBe('/integrations/lockr/api');
    expect(vi.getTimerCount()).toBe(0);
    expect(guard.isGuardInstalled()).toBe(true);
  });

  it('keeps the guard installed without warnings when the SDK initializes late', async () => {
    await import('../../../src/integrations/lockr');
    vi.advanceTimersByTime(3000);
    const sdk = { host: '/integrations/lockr/api' };
    vi.stubGlobal('identityLockr', sdk);
    vi.advanceTimersByTime(100);

    expect(log.warn).not.toHaveBeenCalled();
    expect(sdk.host).toBe('/integrations/lockr/api');
    expect(vi.getTimerCount()).toBe(0);
    expect(guard.isGuardInstalled()).toBe(true);
  });

  it('does not depend on or modify the public Lockr API', async () => {
    const publicApi = Object.freeze({ additionalData: vi.fn(), getOpenRTBEIDs: vi.fn() });
    vi.stubGlobal('lockr', publicApi);

    await import('../../../src/integrations/lockr');

    expect(Reflect.get(globalThis, 'lockr')).toBe(publicApi);
    expect(Object.keys(publicApi)).toEqual(['additionalData', 'getOpenRTBEIDs']);
    expect(publicApi.additionalData).not.toHaveBeenCalled();
    expect(publicApi.getOpenRTBEIDs).not.toHaveBeenCalled();
    expect(vi.getTimerCount()).toBe(0);
    expect(guard.isGuardInstalled()).toBe(true);
  });

  it('imports safely without installing DOM hooks outside a browser', async () => {
    vi.stubGlobal('window', undefined);

    await import('../../../src/integrations/lockr');

    expect(guard.isGuardInstalled()).toBe(false);
    expect(Element.prototype.appendChild).toBe(originalAppendChild);
    expect(Element.prototype.insertBefore).toBe(originalInsertBefore);
    expect(XMLHttpRequest.prototype.open).toBe(originalOpen);
    expect(vi.getTimerCount()).toBe(0);
    expect(log.warn).not.toHaveBeenCalled();
  });
});
