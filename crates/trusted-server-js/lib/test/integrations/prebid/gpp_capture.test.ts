import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  GppConsentCapture,
  type GppApi,
  type GppWindow,
} from '../../../src/integrations/prebid/gpp_capture';

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

describe('GPP API capture', () => {
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

  it('reports a not-applicable section list as unavailable, not as a decision', () => {
    const vendor = cmp(ready('DBABMA~-1', [-1]));
    target.__gpp = vendor.api;
    capture.start();
    expect(onConsent).not.toHaveBeenCalled();
    expect(onUnavailable).toHaveBeenCalled();
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
