import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { mirrorSourcepointConsent } from '../../../src/integrations/sourcepoint';

type SourcepointWindow = Window & {
  __tsjs_sourcepoint?: {
    rewriteSdk?: boolean;
  };
  __tsjs_installSourcepointGuard?: unknown;
};

describe('Sourcepoint integration initialization', () => {
  let win: SourcepointWindow;

  beforeEach(async () => {
    win = window as SourcepointWindow;
    delete win.__tsjs_sourcepoint;

    const guard = await import('../../../src/integrations/sourcepoint/script_guard');
    guard.resetGuardState();
  });

  afterEach(async () => {
    const guard = await import('../../../src/integrations/sourcepoint/script_guard');
    guard.resetGuardState();
    delete win.__tsjs_sourcepoint;
    delete win.__tsjs_installSourcepointGuard;
  });

  it('installs the guard when rewriteSdk is enabled', async () => {
    vi.resetModules();
    win.__tsjs_sourcepoint = { rewriteSdk: true };

    const guard = await import('../../../src/integrations/sourcepoint/script_guard');
    await import('../../../src/integrations/sourcepoint/index');

    expect(guard.isGuardInstalled()).toBe(true);
  });

  it('skips the guard when rewriteSdk is disabled', async () => {
    vi.resetModules();
    win.__tsjs_sourcepoint = { rewriteSdk: false };

    const guard = await import('../../../src/integrations/sourcepoint/script_guard');
    await import('../../../src/integrations/sourcepoint/index');

    expect(guard.isGuardInstalled()).toBe(false);
  });

  it('defaults to installing the guard when rewriteSdk is missing for backward compatibility', async () => {
    vi.resetModules();

    const guard = await import('../../../src/integrations/sourcepoint/script_guard');
    await import('../../../src/integrations/sourcepoint/index');

    expect(guard.isGuardInstalled()).toBe(true);


const SOURCEPOINT_MARKER_COOKIE = '_ts_gpp_src';

function sourcepointPayload(gppString = 'DBABLA~BVQqAAAAAgA.QA', applicableSections = [7]) {
  return {
    gppData: {
      gppString,
      applicableSections,
    },
  };
}

describe('integrations/sourcepoint', () => {
  function clearAllCookies(): void {
    document.cookie.split(';').forEach((c) => {
      const name = c.split('=')[0].trim();
      if (name) document.cookie = `${name}=; path=/; Max-Age=0`;
    });
  }

  function getCookie(name: string): string | undefined {
    const match = document.cookie.split('; ').find((c) => c.startsWith(`${name}=`));
    return match ? match.split('=').slice(1).join('=') : undefined;
  }

  beforeEach(() => {
    // Clear cookies and localStorage before each test.
    clearAllCookies();
    localStorage.clear();
  });

  afterEach(() => {
    vi.useRealTimers();
    Object.defineProperty(document, 'readyState', { value: 'complete', configurable: true });
    clearAllCookies();
    localStorage.clear();
  });

  it('mirrors __gpp and __gpp_sid from _sp_user_consent_* localStorage as session cookies', () => {
    localStorage.setItem('_sp_user_consent_36026', JSON.stringify(sourcepointPayload()));

    const result = mirrorSourcepointConsent();

    expect(result).toBe(true);
    expect(document.cookie).toContain('__gpp=DBABLA~BVQqAAAAAgA.QA');
    expect(document.cookie).toContain('__gpp_sid=7');
    expect(getCookie(SOURCEPOINT_MARKER_COOKIE)).toBe('sp');
  });

  it('mirrors __gpp and __gpp_sid from Sourcepoint usnat localStorage shape', () => {
    localStorage.setItem(
      '_sp_user_consent_36922',
      JSON.stringify({
        usnat: {
          applicableSections: [7],
          consentString: 'DBABLA~BVQqAAAAAgA.QA',
          consentStrings: [
            {
              sectionId: 7,
              sectionName: 'usnat',
              consentString: 'BVQqAAAAAgA.QA',
            },
          ],
          consentStatus: {
            consentedToAll: true,
            consentedToAny: true,
            rejectedAny: false,
          },
        },
        version: 1,
      })
    );

    const result = mirrorSourcepointConsent();

    expect(result).toBe(true);
    expect(getCookie('__gpp')).toBe('DBABLA~BVQqAAAAAgA.QA');
    expect(getCookie('__gpp_sid')).toBe('7');
    expect(getCookie(SOURCEPOINT_MARKER_COOKIE)).toBe('sp');
  });

  it('handles multiple applicable sections', () => {
    localStorage.setItem(
      '_sp_user_consent_99999',
      JSON.stringify(sourcepointPayload('DBABLA~BVQqAAAAAgA.QA', [7, 8]))
    );

    mirrorSourcepointConsent();

    expect(document.cookie).toContain('__gpp_sid=7,8');
  });

  it('returns false when no _sp_user_consent_* key exists', () => {
    localStorage.setItem('unrelated_key', 'value');

    const result = mirrorSourcepointConsent();

    expect(result).toBe(false);
    expect(document.cookie).not.toContain('__gpp=');
    expect(document.cookie).not.toContain('__gpp_sid=');
  });

  it('does not clear non-Sourcepoint GPP cookies when no valid Sourcepoint payload exists', () => {
    document.cookie = '__gpp=other-cmp-gpp; path=/';
    document.cookie = '__gpp_sid=7,8; path=/';
    localStorage.setItem('unrelated_key', 'value');

    const result = mirrorSourcepointConsent();

    expect(result).toBe(false);
    expect(getCookie('__gpp')).toBe('other-cmp-gpp');
    expect(getCookie('__gpp_sid')).toBe('7,8');
  });

  it('does not overwrite GPP cookies owned by another CMP', () => {
    document.cookie = '__gpp=other-cmp-gpp; path=/';
    document.cookie = '__gpp_sid=2; path=/';
    localStorage.setItem(
      '_sp_user_consent_12345',
      JSON.stringify(sourcepointPayload('sourcepoint-gpp', [7]))
    );

    const result = mirrorSourcepointConsent();

    expect(result).toBe(false);
    expect(getCookie('__gpp')).toBe('other-cmp-gpp');
    expect(getCookie('__gpp_sid')).toBe('2');
    expect(getCookie(SOURCEPOINT_MARKER_COOKIE)).toBeUndefined();
  });

  it('clears stale Sourcepoint-owned mirrored cookies when no valid Sourcepoint payload exists', () => {
    document.cookie = '__gpp=stale-gpp; path=/';
    document.cookie = '__gpp_sid=7,8; path=/';
    document.cookie = `${SOURCEPOINT_MARKER_COOKIE}=sp; path=/`;
    localStorage.setItem('unrelated_key', 'value');

    const result = mirrorSourcepointConsent();

    expect(result).toBe(false);
    expect(getCookie('__gpp')).toBeUndefined();
    expect(getCookie('__gpp_sid')).toBeUndefined();
    expect(getCookie(SOURCEPOINT_MARKER_COOKIE)).toBeUndefined();
  });

  it('returns false for malformed JSON in localStorage', () => {
    localStorage.setItem('_sp_user_consent_12345', 'not-json!!!');

    const result = mirrorSourcepointConsent();

    expect(result).toBe(false);
    expect(document.cookie).not.toContain('__gpp=');
  });

  it('skips malformed entries when a later Sourcepoint key is valid', () => {
    localStorage.setItem('_sp_user_consent_12345', 'not-json!!!');
    localStorage.setItem('_sp_user_consent_67890', JSON.stringify(sourcepointPayload()));

    const result = mirrorSourcepointConsent();

    expect(result).toBe(true);
    expect(getCookie('__gpp')).toBe('DBABLA~BVQqAAAAAgA.QA');
    expect(getCookie('__gpp_sid')).toBe('7');
  });

  it('returns false when gppData is missing from payload', () => {
    localStorage.setItem('_sp_user_consent_12345', JSON.stringify({ otherField: true }));

    const result = mirrorSourcepointConsent();

    expect(result).toBe(false);
    expect(document.cookie).not.toContain('__gpp=');
  });

  it('returns false when gppString is empty', () => {
    localStorage.setItem('_sp_user_consent_12345', JSON.stringify(sourcepointPayload('', [7])));

    const result = mirrorSourcepointConsent();

    expect(result).toBe(false);
    expect(document.cookie).not.toContain('__gpp=');
  });

  it('clears stale __gpp_sid when the payload has no applicable sections', () => {
    document.cookie = '__gpp_sid=7,8; path=/';
    document.cookie = `${SOURCEPOINT_MARKER_COOKIE}=sp; path=/`;
    localStorage.setItem(
      '_sp_user_consent_12345',
      JSON.stringify(sourcepointPayload('DBABLA~BVQqAAAAAgA.QA', []))
    );

    const result = mirrorSourcepointConsent();

    expect(result).toBe(true);
    expect(getCookie('__gpp')).toBe('DBABLA~BVQqAAAAAgA.QA');
    expect(getCookie('__gpp_sid')).toBeUndefined();
    expect(getCookie(SOURCEPOINT_MARKER_COOKIE)).toBe('sp');
  });

  it('updates GPP cookies when Sourcepoint owns the marker', () => {
    document.cookie = '__gpp=stale-sourcepoint-gpp; path=/';
    document.cookie = '__gpp_sid=7; path=/';
    document.cookie = `${SOURCEPOINT_MARKER_COOKIE}=sp; path=/`;
    localStorage.setItem(
      '_sp_user_consent_12345',
      JSON.stringify(sourcepointPayload('updated-sourcepoint-gpp', [8]))
    );

    const result = mirrorSourcepointConsent();

    expect(result).toBe(true);
    expect(getCookie('__gpp')).toBe('updated-sourcepoint-gpp');
    expect(getCookie('__gpp_sid')).toBe('8');
    expect(getCookie(SOURCEPOINT_MARKER_COOKIE)).toBe('sp');
  });

  it('refreshes mirrored cookies when the window regains focus', () => {
    localStorage.setItem(
      '_sp_user_consent_12345',
      JSON.stringify(sourcepointPayload('initial-gpp', [7]))
    );

    mirrorSourcepointConsent();
    localStorage.setItem(
      '_sp_user_consent_12345',
      JSON.stringify(sourcepointPayload('updated-gpp', [8]))
    );
    window.dispatchEvent(new Event('focus'));

    expect(getCookie('__gpp')).toBe('updated-gpp');
    expect(getCookie('__gpp_sid')).toBe('8');
  });

  it('clears Sourcepoint-owned cookies when consent is retracted before focus', () => {
    localStorage.setItem(
      '_sp_user_consent_12345',
      JSON.stringify(sourcepointPayload('initial-gpp', [7]))
    );

    mirrorSourcepointConsent();
    localStorage.removeItem('_sp_user_consent_12345');
    window.dispatchEvent(new Event('focus'));

    expect(getCookie('__gpp')).toBeUndefined();
    expect(getCookie('__gpp_sid')).toBeUndefined();
    expect(getCookie(SOURCEPOINT_MARKER_COOKIE)).toBeUndefined();
  });

  it('retries once after module initialization when Sourcepoint data appears shortly after load', async () => {
    vi.useFakeTimers();
    vi.resetModules();
    localStorage.clear();
    clearAllCookies();

    await import('../../../src/integrations/sourcepoint');

    localStorage.setItem(
      '_sp_user_consent_12345',
      JSON.stringify(sourcepointPayload('retry-gpp', [7]))
    );
    vi.advanceTimersByTime(500);

    expect(getCookie('__gpp')).toBe('retry-gpp');
    expect(getCookie('__gpp_sid')).toBe('7');
  });

  it('clears a pending initial retry after a successful manual mirror', async () => {
    vi.useFakeTimers();
    vi.resetModules();
    localStorage.clear();
    clearAllCookies();
    Object.defineProperty(document, 'readyState', { value: 'loading', configurable: true });

    const sourcepoint = await import('../../../src/integrations/sourcepoint');

    localStorage.setItem(
      '_sp_user_consent_12345',
      JSON.stringify(sourcepointPayload('manual-gpp', [7]))
    );
    expect(sourcepoint.mirrorSourcepointConsent()).toBe(true);

    localStorage.setItem(
      '_sp_user_consent_12345',
      JSON.stringify(sourcepointPayload('timer-gpp', [8]))
    );
    document.dispatchEvent(new Event('DOMContentLoaded'));
    vi.advanceTimersByTime(500);

    expect(getCookie('__gpp')).toBe('manual-gpp');
    expect(getCookie('__gpp_sid')).toBe('7');
  });

  it('does not run both DOMContentLoaded and timer retries', async () => {
    vi.useFakeTimers();
    vi.resetModules();
    localStorage.clear();
    clearAllCookies();
    Object.defineProperty(document, 'readyState', { value: 'loading', configurable: true });

    await import('../../../src/integrations/sourcepoint');

    localStorage.setItem(
      '_sp_user_consent_12345',
      JSON.stringify(sourcepointPayload('domcontentloaded-gpp', [7]))
    );
    document.dispatchEvent(new Event('DOMContentLoaded'));

    localStorage.setItem(
      '_sp_user_consent_12345',
      JSON.stringify(sourcepointPayload('timer-gpp', [8]))
    );
    vi.advanceTimersByTime(500);

    expect(getCookie('__gpp')).toBe('domcontentloaded-gpp');
    expect(getCookie('__gpp_sid')).toBe('7');
  });
});
