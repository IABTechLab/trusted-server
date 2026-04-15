import { log } from '../../core/log';

const SP_CONSENT_PREFIX = '_sp_user_consent_';

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
      return JSON.parse(raw) as SourcepointConsentPayload;
    } catch {
      log.debug('sourcepoint: failed to parse localStorage value', { key });
      return null;
    }
  }
  return null;
}

function writeCookie(name: string, value: string): void {
  document.cookie = `${name}=${value}; path=/; SameSite=Lax`;
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
    log.debug('sourcepoint: no GPP data found in localStorage');
    return false;
  }

  const { gppString, applicableSections } = payload.gppData;
  if (!gppString) {
    log.debug('sourcepoint: gppString is empty');
    return false;
  }

  writeCookie('__gpp', gppString);

  if (Array.isArray(applicableSections) && applicableSections.length > 0) {
    writeCookie('__gpp_sid', applicableSections.join(','));
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
