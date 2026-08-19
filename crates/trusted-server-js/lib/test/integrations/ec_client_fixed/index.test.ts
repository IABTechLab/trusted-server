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
  beforeEach(() => {
    clearResolvedMarker();
    global.fetch = vi.fn().mockResolvedValue({ ok: true });
  });

  afterEach(() => {
    global.fetch = ORIGINAL_FETCH;
    clearResolvedMarker();
    vi.resetModules();
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
