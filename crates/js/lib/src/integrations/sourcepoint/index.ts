import { log } from '../../core/log';

const SP_CONSENT_PREFIX = '_sp_user_consent_';
const GPP_COOKIE_NAME = '__gpp';
const GPP_SID_COOKIE_NAME = '__gpp_sid';

interface SourcepointGppData {
  gppString: string;
  applicableSections: number[];
}

interface SourcepointConsentPayload {
  gppData?: SourcepointGppData;
}

function findSourcepointConsent(): SourcepointConsentPayload | null {
  for (let i = 0; i < localStorage.length; i++) {
    const key = localStorage.key(i);
    if (!key?.startsWith(SP_CONSENT_PREFIX)) continue;

    const raw = localStorage.getItem(key);
    if (!raw) continue;

    try {
      const payload = JSON.parse(raw) as SourcepointConsentPayload;
      if (payload.gppData?.gppString) {
        return payload;
      }
    } catch {
      log.debug('sourcepoint: failed to parse localStorage value', { key });
    }
  }
  return null;
}

function writeCookie(name: string, value: string): void {
  document.cookie = `${name}=${value}; path=/; SameSite=Lax`;
}

function clearCookie(name: string): void {
  document.cookie = `${name}=; expires=Thu, 01 Jan 1970 00:00:00 GMT; path=/; SameSite=Lax`;
}

/**
 * Reads Sourcepoint consent from localStorage and mirrors it into
 * `__gpp` and `__gpp_sid` cookies for Trusted Server to read.
 *
 * Returns `true` if cookies were written, `false` otherwise.
 */
export function mirrorSourcepointConsent(): boolean {
  if (typeof localStorage === 'undefined' || typeof document === 'undefined') {
    return false;
  }

  const payload = findSourcepointConsent();
  if (!payload?.gppData) {
    clearCookie(GPP_COOKIE_NAME);
    clearCookie(GPP_SID_COOKIE_NAME);
    log.debug('sourcepoint: no GPP data found in localStorage');
    return false;
  }

  const { gppString, applicableSections } = payload.gppData;
  if (!gppString) {
    clearCookie(GPP_COOKIE_NAME);
    clearCookie(GPP_SID_COOKIE_NAME);
    log.debug('sourcepoint: gppString is empty');
    return false;
  }

  writeCookie(GPP_COOKIE_NAME, gppString);

  if (Array.isArray(applicableSections) && applicableSections.length > 0) {
    writeCookie(GPP_SID_COOKIE_NAME, applicableSections.join(','));
  } else {
    clearCookie(GPP_SID_COOKIE_NAME);
  }

  log.info('sourcepoint: mirrored GPP consent to cookies', {
    gppLength: gppString.length,
    sections: applicableSections,
  });

  return true;
}

if (typeof window !== 'undefined') {
  mirrorSourcepointConsent();
}

export default mirrorSourcepointConsent;
