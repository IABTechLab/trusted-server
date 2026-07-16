import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  initializeOsanoConsentMirror,
  mirrorOsanoConsent,
  resetOsanoConsentMirrorForTest,
} from '../../../src/integrations/osano';

type TestWindow = Window & {
  Osano?: {
    cm?: {
      addEventListener?: ReturnType<typeof vi.fn>;
      removeEventListener?: ReturnType<typeof vi.fn>;
    };
  };
  __uspapi?: ReturnType<typeof vi.fn>;
  __gpp?: ReturnType<typeof vi.fn>;
  __tcfapi?: ReturnType<typeof vi.fn>;
};

const MARKER_COOKIE = '_ts_consent_src';

type UspCallback = (data?: { uspString?: string }, success?: boolean) => void;

function clearAllCookies(): void {
  document.cookie.split(';').forEach((cookie) => {
    const name = cookie.split('=')[0].trim();
    if (name) document.cookie = `${name}=; path=/; Max-Age=0`;
  });
}

function getCookie(name: string): string | undefined {
  const match = document.cookie.split('; ').find((cookie) => cookie.startsWith(`${name}=`));
  return match ? match.split('=').slice(1).join('=') : undefined;
}

function setUspApi(uspString: string | undefined, success = true): void {
  (window as TestWindow).__uspapi = vi.fn((_command, _version, callback) => {
    callback(uspString === undefined ? {} : { uspString }, success);
  });
}

function setControlledUspApi(): UspCallback[] {
  const callbacks: UspCallback[] = [];
  (window as TestWindow).__uspapi = vi.fn((_command, _version, callback: UspCallback) => {
    callbacks.push(callback);
  });
  return callbacks;
}

function setGppApi(
  gppString: string | undefined,
  applicableSections: number[] | undefined,
  signalStatus = 'ready',
  success = true
): void {
  (window as TestWindow).__gpp = vi.fn((_command, callback) => {
    callback({ signalStatus, gppString, applicableSections }, success);
  });
}

function setTcfApi(tcString: string | undefined, success = true, eventStatus = 'tcloaded'): void {
  (window as TestWindow).__tcfapi = vi.fn((_command, _version, callback) => {
    callback(tcString === undefined ? { eventStatus } : { tcString, eventStatus }, success);
  });
}

function setOsanoStub(): Record<string, (payload?: unknown) => void> {
  const listeners: Record<string, (payload?: unknown) => void> = {};
  (window as TestWindow).Osano = {
    cm: {
      addEventListener: vi.fn((eventName: string, callback: (payload?: unknown) => void) => {
        listeners[eventName] = callback;
      }),
      removeEventListener: vi.fn(),
    },
  };
  return listeners;
}

describe('integrations/osano consent mirror', () => {
  beforeEach(() => {
    resetOsanoConsentMirrorForTest();
    clearAllCookies();
    delete (window as TestWindow).Osano;
    delete (window as TestWindow).__uspapi;
    delete (window as TestWindow).__gpp;
    delete (window as TestWindow).__tcfapi;
  });

  afterEach(() => {
    vi.useRealTimers();
    resetOsanoConsentMirrorForTest();
    clearAllCookies();
    delete (window as TestWindow).Osano;
    delete (window as TestWindow).__uspapi;
    delete (window as TestWindow).__gpp;
    delete (window as TestWindow).__tcfapi;
  });

  it('does nothing when Osano IAB APIs are unavailable', async () => {
    const result = await mirrorOsanoConsent();

    expect(result).toBe(false);
    expect(document.cookie).toBe('');
  });

  it('mirrors US Privacy no-opt-out values from __uspapi', async () => {
    setUspApi('1YN-');

    const result = await mirrorOsanoConsent();

    expect(result).toBe(true);
    expect(getCookie('us_privacy')).toBe('1YN-');
    expect(getCookie(MARKER_COOKIE)).toBe('osano');
  });

  it('mirrors US Privacy opt-out values from __uspapi', async () => {
    setUspApi('1YY-');

    const result = await mirrorOsanoConsent();

    expect(result).toBe(true);
    expect(getCookie('us_privacy')).toBe('1YY-');
    expect(getCookie(MARKER_COOKIE)).toBe('osano');
  });

  it('mirrors GPP cookies only when GPP signal status is ready', async () => {
    setGppApi('DBABLA~BVQqAAAAAgA.QA', [7, 8]);

    const result = await mirrorOsanoConsent();

    expect(result).toBe(true);
    expect(getCookie('__gpp')).toBe('DBABLA~BVQqAAAAAgA.QA');
    expect(getCookie('__gpp_sid')).toBe('7,8');
    expect(getCookie(MARKER_COOKIE)).toBe('osano');
  });

  it('does not mirror or clear GPP cookies while GPP is not ready', async () => {
    document.cookie = '__gpp=stale-gpp; path=/';
    document.cookie = `${MARKER_COOKIE}=osano; path=/`;
    setGppApi('updated-gpp', [7], 'not ready');

    const result = await mirrorOsanoConsent();

    expect(result).toBe(false);
    expect(getCookie('__gpp')).toBe('stale-gpp');
    expect(getCookie(MARKER_COOKIE)).toBe('osano');
  });

  it('omits __gpp_sid for empty or not-applicable section lists', async () => {
    document.cookie = '__gpp_sid=stale; path=/';
    document.cookie = `${MARKER_COOKIE}=osano; path=/`;
    setGppApi('ready-gpp', [-1]);

    const result = await mirrorOsanoConsent();

    expect(result).toBe(true);
    expect(getCookie('__gpp')).toBe('ready-gpp');
    expect(getCookie('__gpp_sid')).toBeUndefined();
    expect(getCookie(MARKER_COOKIE)).toBe('osano');
  });

  it('mirrors TCF consent strings from __tcfapi', async () => {
    setTcfApi('CPXxRfAPXxRfAAfKABENB-CgAAAAAAAAAAYgAAAAAAAA');

    const result = await mirrorOsanoConsent();

    expect(result).toBe(true);
    expect(getCookie('euconsent-v2')).toBe('CPXxRfAPXxRfAAfKABENB-CgAAAAAAAAAAYgAAAAAAAA');
    expect(getCookie(MARKER_COOKIE)).toBe('osano');
  });

  it('mirrors USP, GPP, and TCF consent cookies together', async () => {
    setUspApi('1YN-');
    setGppApi('DBABLA~BVQqAAAAAgA.QA', [7, 8]);
    setTcfApi('CPXxRfAPXxRfAAfKABENB-CgAAAAAAAAAAYgAAAAAAAA');

    const result = await mirrorOsanoConsent();

    expect(result).toBe(true);
    expect(getCookie('us_privacy')).toBe('1YN-');
    expect(getCookie('__gpp')).toBe('DBABLA~BVQqAAAAAgA.QA');
    expect(getCookie('__gpp_sid')).toBe('7,8');
    expect(getCookie('euconsent-v2')).toBe('CPXxRfAPXxRfAAfKABENB-CgAAAAAAAAAAYgAAAAAAAA');
    expect(getCookie(MARKER_COOKIE)).toBe('osano');
  });

  it('preserves existing unmarked consent cookies', async () => {
    document.cookie = 'us_privacy=external; path=/';
    setUspApi('1YN-');

    const result = await mirrorOsanoConsent();

    expect(result).toBe(false);
    expect(getCookie('us_privacy')).toBe('external');
    expect(getCookie(MARKER_COOKIE)).toBeUndefined();
  });

  it('preserves consent cookies owned by another mirror', async () => {
    document.cookie = `${MARKER_COOKIE}=sourcepoint; path=/`;
    setUspApi('1YN-');

    const result = await mirrorOsanoConsent();

    expect(result).toBe(false);
    expect(getCookie('us_privacy')).toBeUndefined();
    expect(getCookie(MARKER_COOKIE)).toBe('sourcepoint');
  });

  it('updates cookies when Osano owns the marker', async () => {
    document.cookie = 'us_privacy=stale; path=/';
    document.cookie = `${MARKER_COOKIE}=osano; path=/`;
    setUspApi('1YN-');

    const result = await mirrorOsanoConsent();

    expect(result).toBe(true);
    expect(getCookie('us_privacy')).toBe('1YN-');
    expect(getCookie(MARKER_COOKIE)).toBe('osano');
  });

  it('does not let older overlapping mirror attempts overwrite newer consent', async () => {
    const callbacks = setControlledUspApi();

    const olderAttempt = mirrorOsanoConsent();
    const newerAttempt = mirrorOsanoConsent();

    const olderCallback = callbacks[0];
    const newerCallback = callbacks[1];
    if (!olderCallback || !newerCallback) {
      throw new Error('should capture both USP API callbacks');
    }

    newerCallback({ uspString: '1YY-' }, true);

    await expect(newerAttempt).resolves.toBe(true);
    expect(getCookie('us_privacy')).toBe('1YY-');
    expect(getCookie(MARKER_COOKIE)).toBe('osano');

    olderCallback({ uspString: '1YN-' }, true);

    await expect(olderAttempt).resolves.toBe(false);
    expect(getCookie('us_privacy')).toBe('1YY-');
    expect(getCookie(MARKER_COOKIE)).toBe('osano');
  });

  it('preserves stale Osano-owned cookies until Osano is ready for clearing', async () => {
    vi.useFakeTimers();
    document.cookie = 'us_privacy=stale; path=/';
    document.cookie = `${MARKER_COOKIE}=osano; path=/`;
    setOsanoStub();
    setUspApi(undefined);

    initializeOsanoConsentMirror();
    await vi.runOnlyPendingTimersAsync();

    expect(getCookie('us_privacy')).toBe('stale');
    expect(getCookie(MARKER_COOKIE)).toBe('osano');
  });

  it('clears stale Osano-owned cookies when a ready API definitively has no value', async () => {
    vi.useFakeTimers();
    document.cookie = 'us_privacy=stale; path=/';
    document.cookie = `${MARKER_COOKIE}=osano; path=/`;
    const listeners = setOsanoStub();
    setUspApi(undefined);

    initializeOsanoConsentMirror();
    await vi.runOnlyPendingTimersAsync();
    listeners['osano-cm-initialized']?.();
    await vi.runOnlyPendingTimersAsync();

    expect(getCookie('us_privacy')).toBeUndefined();
    expect(getCookie(MARKER_COOKIE)).toBeUndefined();
  });

  it('preserves stale Osano-owned GPP cookies until Osano is ready for clearing', async () => {
    vi.useFakeTimers();
    document.cookie = '__gpp=stale-gpp; path=/';
    document.cookie = '__gpp_sid=7,8; path=/';
    document.cookie = `${MARKER_COOKIE}=osano; path=/`;
    setOsanoStub();
    setGppApi('', undefined);

    initializeOsanoConsentMirror();
    await vi.runOnlyPendingTimersAsync();

    expect(getCookie('__gpp')).toBe('stale-gpp');
    expect(getCookie('__gpp_sid')).toBe('7,8');
    expect(getCookie(MARKER_COOKIE)).toBe('osano');
  });

  it('clears stale Osano-owned GPP cookies when a ready API definitively has no value', async () => {
    vi.useFakeTimers();
    document.cookie = '__gpp=stale-gpp; path=/';
    document.cookie = '__gpp_sid=7,8; path=/';
    document.cookie = `${MARKER_COOKIE}=osano; path=/`;
    const listeners = setOsanoStub();
    setGppApi('', undefined);

    initializeOsanoConsentMirror();
    await vi.runOnlyPendingTimersAsync();
    listeners['osano-cm-initialized']?.();
    await vi.runOnlyPendingTimersAsync();

    expect(getCookie('__gpp')).toBeUndefined();
    expect(getCookie('__gpp_sid')).toBeUndefined();
    expect(getCookie(MARKER_COOKIE)).toBeUndefined();
  });

  it('does not clear cookies when an API callback returns malformed data', async () => {
    vi.useFakeTimers();
    document.cookie = 'us_privacy=stale; path=/';
    document.cookie = `${MARKER_COOKIE}=osano; path=/`;
    const listeners = setOsanoStub();
    (window as TestWindow).__uspapi = vi.fn((_command, _version, callback) => {
      callback({ uspString: 123 }, true);
    });

    initializeOsanoConsentMirror();
    listeners['osano-cm-initialized']?.();
    await vi.runOnlyPendingTimersAsync();

    expect(getCookie('us_privacy')).toBe('stale');
    expect(getCookie(MARKER_COOKIE)).toBe('osano');
  });

  it('does not mirror or clear TCF cookies before TCF is ready', async () => {
    document.cookie = 'euconsent-v2=stale-tcf; path=/';
    document.cookie = `${MARKER_COOKIE}=osano; path=/`;
    setTcfApi('CPXxRfAPXxRfAAfKABENB-CgAAAAAAAAAAYgAAAAAAAA', true, 'cmpuishown');

    const result = await mirrorOsanoConsent();

    expect(result).toBe(false);
    expect(getCookie('euconsent-v2')).toBe('stale-tcf');
    expect(getCookie(MARKER_COOKIE)).toBe('osano');
  });

  it('does not clear cookies when an API callback reports failure', async () => {
    document.cookie = 'us_privacy=stale; path=/';
    document.cookie = `${MARKER_COOKIE}=osano; path=/`;
    setUspApi('1YN-', false);

    const result = await mirrorOsanoConsent();

    expect(result).toBe(false);
    expect(getCookie('us_privacy')).toBe('stale');
    expect(getCookie(MARKER_COOKIE)).toBe('osano');
  });

  it('does not clear cookies when an API times out', async () => {
    vi.useFakeTimers();
    document.cookie = 'us_privacy=stale; path=/';
    document.cookie = `${MARKER_COOKIE}=osano; path=/`;
    (window as TestWindow).__uspapi = vi.fn();

    const pending = mirrorOsanoConsent();
    await vi.advanceTimersByTimeAsync(500);
    const result = await pending;

    expect(result).toBe(false);
    expect(getCookie('us_privacy')).toBe('stale');
    expect(getCookie(MARKER_COOKIE)).toBe('osano');
  });

  it('mirrors available IAB APIs on initialization before Osano listeners exist', async () => {
    vi.useFakeTimers();
    setUspApi('1YN-');

    initializeOsanoConsentMirror();
    await vi.runOnlyPendingTimersAsync();

    expect(getCookie('us_privacy')).toBe('1YN-');
    expect(getCookie(MARKER_COOKIE)).toBe('osano');
  });

  it('registers Osano listeners and mirrors returning consent on initialization', async () => {
    vi.useFakeTimers();
    const listeners = setOsanoStub();
    setUspApi('1YN-');

    initializeOsanoConsentMirror();
    await vi.runOnlyPendingTimersAsync();

    expect((window as TestWindow).Osano?.cm?.addEventListener).toHaveBeenCalledWith(
      'osano-cm-consent-saved',
      expect.any(Function)
    );
    expect(getCookie('us_privacy')).toBe('1YN-');

    setUspApi('1YY-');
    listeners['osano-cm-consent-saved']?.({ OPT_OUT: 'ACCEPT' });
    await vi.runOnlyPendingTimersAsync();

    expect(getCookie('us_privacy')).toBe('1YY-');
  });

  it('retries boundedly when Osano appears after initialization', async () => {
    vi.useFakeTimers();
    setUspApi('1YN-');

    initializeOsanoConsentMirror();
    const listeners = setOsanoStub();
    await vi.advanceTimersByTimeAsync(250);
    await vi.runOnlyPendingTimersAsync();

    expect((window as TestWindow).Osano?.cm?.addEventListener).toHaveBeenCalled();
    expect(listeners['osano-cm-consent-saved']).toEqual(expect.any(Function));
    expect(getCookie('us_privacy')).toBe('1YN-');
  });

  it('keeps retrying when Osano cm appears before listener methods', async () => {
    vi.useFakeTimers();
    setUspApi('1YN-');
    const listeners: Record<string, (payload?: unknown) => void> = {};
    const cm: NonNullable<NonNullable<TestWindow['Osano']>['cm']> = {};
    (window as TestWindow).Osano = { cm };

    initializeOsanoConsentMirror();
    await vi.advanceTimersByTimeAsync(250);

    cm.addEventListener = vi.fn((eventName: string, callback: (payload?: unknown) => void) => {
      listeners[eventName] = callback;
    });
    cm.removeEventListener = vi.fn();

    await vi.advanceTimersByTimeAsync(250);
    await vi.runOnlyPendingTimersAsync();

    expect(cm.addEventListener).toHaveBeenCalledWith(
      'osano-cm-consent-saved',
      expect.any(Function)
    );
    expect(listeners['osano-cm-consent-saved']).toEqual(expect.any(Function));
    expect(getCookie('us_privacy')).toBe('1YN-');
  });
});
