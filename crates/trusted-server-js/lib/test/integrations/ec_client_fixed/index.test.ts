import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const ORIGINAL_FETCH = global.fetch;

async function importModule() {
  vi.resetModules();
  return import('../../../src/integrations/ec_client_fixed/index');
}

function clearResolvedMarker() {
  document.cookie = 'ts-ecr=; expires=Thu, 01 Jan 1970 00:00:00 GMT';
}

describe('ec_client_fixed', () => {
  // The page state the edge injects, as core exposes it. The default grants
  // the permission this module requires so the existing posting tests hold.
  function setPageState(set: string[] | undefined): void {
    if (set === undefined) {
      delete window.tsjs;
      return;
    }
    window.tsjs = {
      whenPermissions: () => Promise.resolve({ set }),
    } as unknown as NonNullable<typeof window.tsjs>;
  }

  beforeEach(() => {
    clearResolvedMarker();
    setPageState(['necessary.operations.storage']);
    global.fetch = vi.fn().mockResolvedValue({ ok: true });
  });

  afterEach(() => {
    global.fetch = ORIGINAL_FETCH;
    clearResolvedMarker();
    delete window.tsjs;
    vi.resetModules();
  });

  it('does not post when the required permission is not set', async () => {
    const fetchMock = vi.fn().mockResolvedValue({ ok: true });
    global.fetch = fetchMock as unknown as typeof fetch;
    setPageState(['advertising_marketing.first_party.contextual']);
    const { resolveEdgeCookie, requiredPermissionIsSet } = await importModule();
    fetchMock.mockClear();

    await expect(requiredPermissionIsSet()).resolves.toBe(false);
    await expect(resolveEdgeCookie()).resolves.toBeNull();
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it('does not post when the page carries no permission state at all', async () => {
    const fetchMock = vi.fn().mockResolvedValue({ ok: true });
    global.fetch = fetchMock as unknown as typeof fetch;
    setPageState(undefined);
    const { resolveEdgeCookie } = await importModule();
    fetchMock.mockClear();

    await expect(resolveEdgeCookie()).resolves.toBeNull();
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it('waits for permission state that arrives after the module runs', async () => {
    const fetchMock = vi.fn().mockResolvedValue({ ok: true });
    global.fetch = fetchMock as unknown as typeof fetch;
    let resolveState: (snapshot: { set: string[] }) => void = () => {};
    window.tsjs = {
      whenPermissions: () =>
        new Promise<{ set: string[] }>((resolve) => {
          resolveState = resolve;
        }),
    } as unknown as NonNullable<typeof window.tsjs>;
    const { resolveEdgeCookie } = await importModule();
    fetchMock.mockClear();

    const pending = resolveEdgeCookie();
    expect(fetchMock).not.toHaveBeenCalled();
    resolveState({ set: ['necessary.operations.storage'] });

    await expect(pending).resolves.toBe('an-ec');
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it('detects the resolved-marker cookie presence', async () => {
    const { hasResolvedMarker } = await importModule();
    expect(hasResolvedMarker('a=1; ts-ecr=1; b=2')).toBe(true);
    expect(hasResolvedMarker('first-party=1; b=2')).toBe(false);
    // The Edge Cookie itself is HttpOnly and never visible here, so its name
    // must not satisfy the marker check.
    expect(hasResolvedMarker('ts-ec=abc')).toBe(false);
    expect(hasResolvedMarker('')).toBe(false);
  });

  it('posts the fixed known word to the resolve endpoint when no marker is present', async () => {
    const fetchMock = vi.fn().mockResolvedValue({ ok: true });
    global.fetch = fetchMock as unknown as typeof fetch;
    const { resolveEdgeCookie } = await importModule();
    // Ignore the import-time auto-run; assert on an explicit call.
    fetchMock.mockClear();

    const value = await resolveEdgeCookie();

    expect(value).toBe('an-ec');
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(fetchMock).toHaveBeenCalledWith(
      '/_ts/api/v1/ec/resolve',
      expect.objectContaining({ method: 'POST', body: 'an-ec' })
    );
  });

  it('does not post when the resolved marker is already present', async () => {
    const fetchMock = vi.fn().mockResolvedValue({ ok: true });
    global.fetch = fetchMock as unknown as typeof fetch;
    document.cookie = 'ts-ecr=1';
    const { resolveEdgeCookie } = await importModule();
    fetchMock.mockClear();

    const value = await resolveEdgeCookie();

    expect(value).toBeNull();
    expect(fetchMock).not.toHaveBeenCalled();
  });
});
