import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import { mirrorSourcepointConsent } from '../../../src/integrations/sourcepoint';

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
    clearAllCookies();
    localStorage.clear();
  });

  it('mirrors __gpp and __gpp_sid from _sp_user_consent_* localStorage as session cookies', () => {
    const payload = {
      gppData: {
        gppString: 'DBABLA~BVQqAAAAAgA.QA',
        applicableSections: [7],
      },
    };
    localStorage.setItem('_sp_user_consent_36026', JSON.stringify(payload));

    const result = mirrorSourcepointConsent();

    expect(result).toBe(true);
    expect(document.cookie).toContain('__gpp=DBABLA~BVQqAAAAAgA.QA');
    expect(document.cookie).toContain('__gpp_sid=7');
  });

  it('handles multiple applicable sections', () => {
    const payload = {
      gppData: {
        gppString: 'DBABLA~BVQqAAAAAgA.QA',
        applicableSections: [7, 8],
      },
    };
    localStorage.setItem('_sp_user_consent_99999', JSON.stringify(payload));

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

  it('clears stale mirrored cookies when no valid Sourcepoint payload exists', () => {
    document.cookie = '__gpp=stale-gpp; path=/';
    document.cookie = '__gpp_sid=7,8; path=/';
    localStorage.setItem('unrelated_key', 'value');

    const result = mirrorSourcepointConsent();

    expect(result).toBe(false);
    expect(getCookie('__gpp')).toBeUndefined();
    expect(getCookie('__gpp_sid')).toBeUndefined();
  });

  it('returns false for malformed JSON in localStorage', () => {
    localStorage.setItem('_sp_user_consent_12345', 'not-json!!!');

    const result = mirrorSourcepointConsent();

    expect(result).toBe(false);
    expect(document.cookie).not.toContain('__gpp=');
  });

  it('skips malformed entries when a later Sourcepoint key is valid', () => {
    localStorage.setItem('_sp_user_consent_12345', 'not-json!!!');
    localStorage.setItem(
      '_sp_user_consent_67890',
      JSON.stringify({
        gppData: {
          gppString: 'DBABLA~BVQqAAAAAgA.QA',
          applicableSections: [7],
        },
      })
    );

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
    const payload = {
      gppData: {
        gppString: '',
        applicableSections: [7],
      },
    };
    localStorage.setItem('_sp_user_consent_12345', JSON.stringify(payload));

    const result = mirrorSourcepointConsent();

    expect(result).toBe(false);
    expect(document.cookie).not.toContain('__gpp=');
  });

  it('clears stale __gpp_sid when the payload has no applicable sections', () => {
    document.cookie = '__gpp_sid=7,8; path=/';
    localStorage.setItem(
      '_sp_user_consent_12345',
      JSON.stringify({
        gppData: {
          gppString: 'DBABLA~BVQqAAAAAgA.QA',
          applicableSections: [],
        },
      })
    );

    const result = mirrorSourcepointConsent();

    expect(result).toBe(true);
    expect(getCookie('__gpp')).toBe('DBABLA~BVQqAAAAAgA.QA');
    expect(getCookie('__gpp_sid')).toBeUndefined();
  });
});
