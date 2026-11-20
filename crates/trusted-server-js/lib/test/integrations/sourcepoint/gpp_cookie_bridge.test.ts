import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { GppApi, GppWindow } from '../../../src/shared/gpp_capture';

type Callback = Parameters<GppApi>[1];
const ready = (gppString = 'synthetic-gpp-allow', applicableSections = [7]) => ({
  gppVersion: '1.1',
  cmpStatus: 'loaded',
  signalStatus: 'ready',
  gppString,
  applicableSections,
});

function cmp(initial: Record<string, unknown> = ready()) {
  let state = initial;
  let nextId = 1;
  const listeners = new Map<number, Callback>();
  const api: GppApi = vi.fn((command, callback, parameter) => {
    if (command === 'ping') callback(state, true);
    else if (command === 'addEventListener') {
      const id = nextId++;
      listeners.set(id, callback);
      callback(
        { eventName: 'listenerRegistered', listenerId: id, data: true, pingData: state },
        true
      );
    } else if (command === 'removeEventListener') {
      callback(listeners.delete(parameter!), true);
    } else callback(undefined, false);
  });
  return {
    api,
    listeners,
    change(next: Record<string, unknown>, eventName = 'sectionChange') {
      state = next;
      for (const [id, callback] of listeners) {
        callback({ eventName, listenerId: id, pingData: state }, true);
      }
    },
  };
}

describe('Sourcepoint GPP cookie bridge', () => {
  const target = window as GppWindow;
  let module: typeof import('../../../src/integrations/sourcepoint');
  let previousFetch: PropertyDescriptor | undefined;
  let fetchMock: ReturnType<typeof vi.fn<typeof window.fetch>>;

  function clearCookies(): void {
    for (const cookie of document.cookie.split(';')) {
      document.cookie = `${cookie.trim().split('=')[0]}=; Path=/; Max-Age=0`;
    }
  }

  function cookie(name: string): string | undefined {
    return document.cookie
      .split('; ')
      .find((value) => value.startsWith(`${name}=`))
      ?.slice(name.length + 1);
  }

  beforeEach(() => {
    vi.resetModules();
    localStorage.clear();
    clearCookies();
    delete target.__gpp;
    previousFetch = Object.getOwnPropertyDescriptor(window, 'fetch');
    fetchMock = vi
      .fn<typeof window.fetch>()
      .mockResolvedValue({ status: 204, body: null } as Response);
    Object.defineProperty(window, 'fetch', { configurable: true, value: fetchMock });
    Object.assign(window, { __tsjs_sourcepoint: { rewriteSdk: false } });
  });

  afterEach(() => {
    module?.disposeSourcepointConsentMirror();
    delete target.__gpp;
    Reflect.deleteProperty(window, '__tsjs_sourcepoint');
    if (previousFetch) Object.defineProperty(window, 'fetch', previousFetch);
    else Reflect.deleteProperty(window, 'fetch');
    localStorage.clear();
    clearCookies();
    vi.useRealTimers();
  });

  it('uses the ready API rather than another property’s stale localStorage record', async () => {
    localStorage.setItem(
      '_sp_user_consent_old',
      JSON.stringify({ gppData: { gppString: 'synthetic-stale-gpp', applicableSections: [8] } })
    );
    target.__gpp = cmp(ready('synthetic-current-gpp')).api;
    module = await import('../../../src/integrations/sourcepoint');
    expect(cookie('__gpp')).toBe('synthetic-current-gpp');
    expect(cookie('__gpp_sid')).toBe('7');
  });

  it('writes actual restrictive updates immediately in the same tab', async () => {
    const vendor = cmp();
    target.__gpp = vendor.api;
    module = await import('../../../src/integrations/sourcepoint');
    vendor.change(ready('synthetic-gpp-opt-out', [7, 22]));
    expect(cookie('__gpp')).toBe('synthetic-gpp-opt-out');
    expect(cookie('__gpp_sid')).toBe('7,22');
    expect(cookie('_ts_gpp_src')).toBe('sp');
    expect(
      fetchMock.mock.calls.map(([, init]) => JSON.parse(init!.body as string).gppString)
    ).toEqual(['synthetic-gpp-allow', 'synthetic-gpp-opt-out']);
  });

  it('does not fall back to a stale grant while the GPP API is not ready', async () => {
    const vendor = cmp();
    target.__gpp = vendor.api;
    module = await import('../../../src/integrations/sourcepoint');
    localStorage.setItem(
      '_sp_user_consent_old',
      JSON.stringify({ gppData: { gppString: 'synthetic-stale-gpp', applicableSections: [7] } })
    );
    vendor.change({ ...ready(), signalStatus: 'not ready' });
    window.dispatchEvent(new Event('focus'));
    expect(cookie('__gpp')).toBeUndefined();
    expect(cookie('_ts_gpp_src')).toBeUndefined();
    expect(JSON.parse(fetchMock.mock.calls.at(-1)![1]!.body as string)).toEqual({
      gppString: null,
      applicableSections: [],
    });
  });

  it('preserves another writer’s cookies on ready and unavailable events', async () => {
    document.cookie = '__gpp=synthetic-other-cmp; Path=/';
    document.cookie = '__gpp_sid=2; Path=/';
    const vendor = cmp();
    target.__gpp = vendor.api;
    module = await import('../../../src/integrations/sourcepoint');
    expect(cookie('__gpp')).toBe('synthetic-other-cmp');
    vendor.change({ ...ready(), signalStatus: 'not ready' });
    expect(cookie('__gpp')).toBe('synthetic-other-cmp');
    expect(cookie('__gpp_sid')).toBe('2');
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("does not claim another writer's identical GPP cookie", async () => {
    document.cookie = '__gpp=synthetic-gpp-allow; Path=/';
    document.cookie = '__gpp_sid=2; Path=/';
    target.__gpp = cmp().api;
    module = await import('../../../src/integrations/sourcepoint');
    expect(cookie('_ts_gpp_src')).toBeUndefined();
    expect(cookie('__gpp_sid')).toBe('2');
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it('contains blocked localStorage without breaking publisher scripts', async () => {
    vi.useFakeTimers();
    const storage = vi.spyOn(window, 'localStorage', 'get').mockImplementation(() => {
      throw new DOMException('Synthetic blocked storage', 'SecurityError');
    });
    try {
      module = await import('../../../src/integrations/sourcepoint');
      expect(() => window.dispatchEvent(new Event('focus'))).not.toThrow();
      expect(() => vi.advanceTimersByTime(600)).not.toThrow();
      expect(fetchMock).not.toHaveBeenCalled();
    } finally {
      storage.mockRestore();
    }
  });

  it('removes a pending DOM-ready legacy retry when disposed', async () => {
    vi.useFakeTimers();
    const loading = vi.spyOn(document, 'readyState', 'get').mockReturnValue('loading');
    try {
      module = await import('../../../src/integrations/sourcepoint');
      module.disposeSourcepointConsentMirror();
      localStorage.setItem(
        '_sp_user_consent_fixture',
        JSON.stringify({
          gppData: { gppString: 'synthetic-after-stop', applicableSections: [7] },
        })
      );
      document.dispatchEvent(new Event('DOMContentLoaded'));
      vi.runAllTimers();
      expect(cookie('__gpp')).toBeUndefined();
      expect(fetchMock).not.toHaveBeenCalled();
    } finally {
      loading.mockRestore();
    }
  });

  it('captures a late GPP publication after the legacy startup retry expired', async () => {
    vi.useFakeTimers();
    module = await import('../../../src/integrations/sourcepoint');
    vi.advanceTimersByTime(5_000);
    target.__gpp = cmp().api;
    expect(cookie('__gpp')).toBe('synthetic-gpp-allow');
    expect(cookie('__gpp_sid')).toBe('7');
  });
});
