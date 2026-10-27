import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  GppConsentCapture,
  type GppApi,
  type GppWindow,
} from '../../../src/integrations/sourcepoint/gpp_capture';

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

describe('Sourcepoint GPP API capture', () => {
  const target = window as GppWindow;
  let capture: GppConsentCapture;
  let onConsent: ReturnType<typeof vi.fn>;
  let onUnavailable: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    delete target.__gpp;
    onConsent = vi.fn();
    onUnavailable = vi.fn();
    capture = new GppConsentCapture(target, onConsent, onUnavailable);
  });

  afterEach(() => {
    capture.stop();
    delete target.__gpp;
    vi.useRealTimers();
  });

  it('uses callback ping and listener registration rather than unsupported getGPPData', () => {
    const vendor = cmp();
    target.__gpp = vendor.api;
    capture.start();
    expect(onConsent).toHaveBeenLastCalledWith({
      gppString: 'synthetic-gpp-allow',
      applicableSections: [7],
    });
    expect(vendor.listeners.size).toBe(1);
    expect(vi.mocked(vendor.api).mock.calls.map(([command]) => command)).not.toContain(
      'getGPPData'
    );
  });

  it('captures an API published long after the old startup retry without polling', () => {
    vi.useFakeTimers();
    capture.start();
    vi.advanceTimersByTime(60_000);
    expect(onConsent).not.toHaveBeenCalled();
    const vendor = cmp();
    target.__gpp = vendor.api;
    expect(onConsent).toHaveBeenLastCalledWith({
      gppString: 'synthetic-gpp-allow',
      applicableSections: [7],
    });
    expect(vi.getTimerCount()).toBe(0);
  });

  it('does not publish a grant when the event subscription failed', () => {
    target.__gpp = (command, callback) => {
      if (command === 'addEventListener') callback(undefined, false);
      if (command === 'ping') callback(ready(), true);
    };
    capture.start();
    expect(onConsent).not.toHaveBeenCalled();
    expect(onUnavailable).toHaveBeenCalled();
  });

  it('requires the registration data flag, not just a successful command', () => {
    target.__gpp = (command, callback) => {
      if (command === 'addEventListener')
        callback({ eventName: 'listenerRegistered', listenerId: 0, data: false }, true);
      if (command === 'ping') callback(ready(), true);
    };
    capture.start();
    expect(onConsent).not.toHaveBeenCalled();
    expect(onUnavailable).toHaveBeenCalled();
  });

  it('preserves the window receiver when invoking the publisher API', () => {
    const vendor = cmp();
    const receivers: unknown[] = [];
    target.__gpp = function (this: unknown, ...args) {
      receivers.push(this);
      vendor.api(...args);
    };
    capture.start();
    capture.stop();
    expect(receivers.length).toBeGreaterThan(0);
    expect(receivers.every((receiver) => receiver === window)).toBe(true);
  });

  it('waits for real readiness rather than accepting an unready snapshot', () => {
    const vendor = cmp({ ...ready(), signalStatus: 'not ready' });
    target.__gpp = vendor.api;
    capture.start();
    expect(onConsent).not.toHaveBeenCalled();
    expect(onUnavailable).toHaveBeenCalled();
    vendor.change(ready(), 'signalStatus');
    expect(onConsent).toHaveBeenCalledWith({
      gppString: 'synthetic-gpp-allow',
      applicableSections: [7],
    });
  });

  it('captures same-tab restrictive changes without focus or reload', () => {
    const vendor = cmp();
    target.__gpp = vendor.api;
    capture.start();
    vendor.change(ready('synthetic-gpp-opt-out', [7, 22]));
    expect(onConsent).toHaveBeenLastCalledWith({
      gppString: 'synthetic-gpp-opt-out',
      applicableSections: [7, 22],
    });
  });

  it('reports unavailable state instead of keeping an earlier permissive snapshot', () => {
    const vendor = cmp();
    target.__gpp = vendor.api;
    capture.start();
    onConsent.mockClear();
    onUnavailable.mockClear();
    vendor.change({ ...ready(), signalStatus: 'not ready' });
    expect(onUnavailable).toHaveBeenCalledOnce();
    expect(onConsent).not.toHaveBeenCalled();
  });

  it.each([
    { ...ready(), gppString: '' },
    { ...ready(), gppString: 'synthetic-grant; Path=/' },
    { ...ready(), gppString: 'synthetic-grant\n' },
    { ...ready(), applicableSections: [70000] },
    { ...ready(), applicableSections: ['7'] },
    { ...ready(), applicableSections: [7.5] },
    { ...ready(), applicableSections: [] },
    { ...ready(), applicableSections: [-1] },
    { ...ready(), applicableSections: undefined },
  ])('does not accept malformed API consent data: %j', (state) => {
    target.__gpp = cmp(state).api;
    capture.start();
    expect(onConsent).not.toHaveBeenCalled();
    expect(onUnavailable).toHaveBeenCalled();
  });

  it('transfers a queued stub subscription without registering twice', () => {
    const queued: Array<[string, Callback, number | undefined]> = [];
    target.__gpp = (command, callback, parameter) => queued.push([command, callback, parameter]);
    capture.start();
    const vendor = cmp();
    target.__gpp = vendor.api;
    for (const [command, callback, parameter] of queued) vendor.api(command, callback, parameter);
    expect(vendor.listeners.size).toBe(1);
    expect(onConsent).toHaveBeenLastCalledWith({
      gppString: 'synthetic-gpp-allow',
      applicableSections: [7],
    });
  });

  it('handles the synchronous acknowledgment in the published 1.1 stub without granting from it', () => {
    target.__gpp = (command) => {
      if (command === 'ping') return { ...ready(), cmpStatus: 'stub' };
      if (command === 'addEventListener')
        return { eventName: 'listenerRegistered', listenerId: 1, data: true };
    };
    capture.start();
    expect(onConsent).not.toHaveBeenCalled();
    expect(onUnavailable).toHaveBeenCalled();
    target.__gpp = cmp().api;
    expect(onConsent).toHaveBeenCalled();
  });

  it('uses synchronous API 1.0 getGPPData only after discovering that version', () => {
    let state = { gppString: 'synthetic-legacy-gpp', applicableSections: [7] };
    let listener: Callback | undefined;
    target.__gpp = (command, callback) => {
      if (command === 'ping') return { gppVersion: '1.0', cmpStatus: 'loaded' };
      if (command === 'getGPPData') return state;
      if (command === 'addEventListener') {
        listener = callback;
        return { eventName: 'listenerRegistered', listenerId: 1, data: true };
      }
    };
    capture.start();
    expect(onConsent).toHaveBeenLastCalledWith(state);
    state = { gppString: 'synthetic-legacy-opt-out', applicableSections: [7] };
    listener!({ eventName: 'sectionChange' });
    expect(onConsent).toHaveBeenLastCalledWith(state);
  });

  it('discards an older ping callback after a newer decision event', () => {
    const pending: Callback[] = [];
    let listener: Callback | undefined;
    target.__gpp = (command, callback) => {
      if (command === 'ping') pending.push(callback);
      if (command === 'addEventListener') {
        listener = callback;
        callback({ eventName: 'listenerRegistered', listenerId: 1, data: true }, true);
      }
    };
    capture.start();
    pending[0](ready(), true);
    const obsolete = pending[1];
    listener!({ eventName: 'sectionChange' }, true);
    pending[2](ready('synthetic-current-opt-out'), true);
    obsolete(ready('synthetic-obsolete-allow'), true);
    expect(onConsent).toHaveBeenCalledOnce();
    expect(onConsent).toHaveBeenLastCalledWith({
      gppString: 'synthetic-current-opt-out',
      applicableSections: [7],
    });
  });

  it('removes the old listener when an installed CMP API is replaced', () => {
    const old = cmp();
    const next = cmp(ready('synthetic-current-gpp'));
    target.__gpp = old.api;
    capture.start();
    target.__gpp = next.api;
    expect(old.listeners.size).toBe(0);
    expect(next.listeners.size).toBe(1);
    expect(onConsent).toHaveBeenLastCalledWith({
      gppString: 'synthetic-current-gpp',
      applicableSections: [7],
    });
    old.change(ready('synthetic-stale-gpp'));
    expect(onConsent).not.toHaveBeenLastCalledWith({
      gppString: 'synthetic-stale-gpp',
      applicableSections: [7],
    });
  });

  it('keeps the publisher API identity and descriptor semantics after disposal', () => {
    const vendor = cmp();
    capture.start();
    target.__gpp = vendor.api;
    capture.stop();
    expect(vendor.listeners.size).toBe(0);
    expect(target.__gpp).toBe(vendor.api);
    expect(Object.getOwnPropertyDescriptor(target, '__gpp')).toMatchObject({
      value: vendor.api,
      writable: true,
      configurable: true,
      enumerable: true,
    });
  });

  it('observes SDK replacement through a custom accessor at resource completion', () => {
    let published: GppApi | undefined = undefined;
    const getter = () => published;
    Object.defineProperty(target, '__gpp', { configurable: true, get: getter });
    capture.start();
    published = cmp().api;
    document.dispatchEvent(new Event('load'));
    expect(onConsent).toHaveBeenCalled();
    expect(Object.getOwnPropertyDescriptor(target, '__gpp')?.get).toBe(getter);
  });

  it('leaves a publisher custom accessor intact', () => {
    const vendor = cmp();
    const getter = () => vendor.api;
    Object.defineProperty(target, '__gpp', { configurable: true, get: getter });
    capture.start();
    expect(Object.getOwnPropertyDescriptor(target, '__gpp')?.get).toBe(getter);
    expect(onConsent).toHaveBeenCalled();
    capture.stop();
    expect(Object.getOwnPropertyDescriptor(target, '__gpp')?.get).toBe(getter);
  });
});

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
