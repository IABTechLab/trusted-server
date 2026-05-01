import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { mirrorSourcepointConsent } from '../../../src/integrations/sourcepoint';

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

  it('mirrors __gpp and __gpp_sid from current Sourcepoint usnat localStorage shape', () => {
    const payload = {
      usnat: {
        applicableSections: [7],
        consentString: 'DBABLA~BVQqAAAAAgA.QA',
        consentStatus: {
          consentedToAll: true,
        },
      },
    };
    localStorage.setItem('_sp_user_consent_36922', JSON.stringify(payload));

    const result = mirrorSourcepointConsent();

    expect(result).toBe(true);
    expect(getCookie('__gpp')).toBe('DBABLA~BVQqAAAAAgA.QA');
    expect(getCookie('__gpp_sid')).toBe('7');
  });

  it('mirrors euconsent-v2 from Sourcepoint gdpr localStorage shape', () => {
    const payload = {
      gdpr: {
        consentString: 'CPXxGfAPXxGfAAHABBENBCCsAP_AAH_AAAAAHftf',
      },
    };
    localStorage.setItem('_sp_user_consent_36922', JSON.stringify(payload));

    const result = mirrorSourcepointConsent();

    expect(result).toBe(true);
    expect(getCookie('euconsent-v2')).toBe('CPXxGfAPXxGfAAHABBENBCCsAP_AAH_AAAAAHftf');
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
});
