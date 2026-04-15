import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import { mirrorSourcepointConsent } from '../../../src/integrations/sourcepoint';

describe('integrations/sourcepoint', () => {
  beforeEach(() => {
    // Clear cookies and localStorage before each test.
    document.cookie.split(';').forEach((c) => {
      const name = c.split('=')[0].trim();
      if (name) document.cookie = `${name}=; expires=Thu, 01 Jan 1970 00:00:00 GMT; path=/`;
    });
    localStorage.clear();
  });

  afterEach(() => {
    localStorage.clear();
  });

  it('mirrors __gpp and __gpp_sid from _sp_user_consent_* localStorage', () => {
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

  it('returns false for malformed JSON in localStorage', () => {
    localStorage.setItem('_sp_user_consent_12345', 'not-json!!!');

    const result = mirrorSourcepointConsent();

    expect(result).toBe(false);
    expect(document.cookie).not.toContain('__gpp=');
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
});
