import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const ORIGINAL_FETCH = global.fetch;

async function importModule() {
  vi.resetModules();
  return import('../../../src/integrations/ec_client_fixed/index');
}

function clearEdgeCookie() {
  document.cookie = 'ts-ec=; expires=Thu, 01 Jan 1970 00:00:00 GMT';
}

describe('ec_client_fixed', () => {
  beforeEach(() => {
    clearEdgeCookie();
    global.fetch = vi.fn().mockResolvedValue({ ok: true });
  });

  afterEach(() => {
    global.fetch = ORIGINAL_FETCH;
    clearEdgeCookie();
    vi.resetModules();
  });

  it('detects the ts-ec cookie presence', async () => {
    const { hasEdgeCookie } = await importModule();
    expect(hasEdgeCookie('a=1; ts-ec=abc; b=2')).toBe(true);
    expect(hasEdgeCookie('first-party=1; b=2')).toBe(false);
    expect(hasEdgeCookie('')).toBe(false);
  });

  it('posts the fixed known word to the resolve endpoint when no cookie is present', async () => {
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

  it('does not post when an Edge Cookie is already present', async () => {
    const fetchMock = vi.fn().mockResolvedValue({ ok: true });
    global.fetch = fetchMock as unknown as typeof fetch;
    document.cookie = 'ts-ec=existing';
    const { resolveEdgeCookie } = await importModule();
    fetchMock.mockClear();

    const value = await resolveEdgeCookie();

    expect(value).toBeNull();
    expect(fetchMock).not.toHaveBeenCalled();
  });
});
