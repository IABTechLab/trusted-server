import { log } from '../../core/log';

const SP_CONSENT_PREFIX = '_sp_user_consent_';
const GPP_COOKIE_NAME = '__gpp';
const GPP_SID_COOKIE_NAME = '__gpp_sid';
const GPP_SOURCE_COOKIE_NAME = '_ts_gpp_src';
const GPP_SOURCE_SOURCEPOINT = 'sp';
const INITIAL_RETRY_DELAY_MS = 500;

interface SourcepointGppData {
  gppString: string;
  applicableSections: number[];
}

interface SourcepointConsentPayload {
  gppData?: SourcepointGppData;
}

let initialized = false;
let initialRetryDone = false;
let retryTimer: ReturnType<typeof window.setTimeout> | undefined;

function findSourcepointConsent(): SourcepointConsentPayload | null {
  // Sourcepoint stores one consent payload per property under `_sp_user_consent_*`.
  // We intentionally take the first valid match and mirror that origin-scoped payload.
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

function readCookie(name: string): string | undefined {
  const prefix = `${name}=`;
  const cookie = document.cookie.split('; ').find((entry) => entry.startsWith(prefix));
  return cookie?.slice(prefix.length);
}

function hasSourcepointMarker(): boolean {
  return readCookie(GPP_SOURCE_COOKIE_NAME) === GPP_SOURCE_SOURCEPOINT;
}

function writeCookie(name: string, value: string): void {
  document.cookie = `${name}=${value}; path=/; Secure; SameSite=Lax`;
}

function clearCookie(name: string): void {
  document.cookie = `${name}=; path=/; Secure; SameSite=Lax; Max-Age=0`;
}

function clearSourcepointCookies(): void {
  if (!hasSourcepointMarker()) {
    return;
  }

  clearCookie(GPP_COOKIE_NAME);
  clearCookie(GPP_SID_COOKIE_NAME);
  clearCookie(GPP_SOURCE_COOKIE_NAME);
}

function mirrorOnVisible(): void {
  if (document.visibilityState === 'visible') {
    mirrorSourcepointConsent();
  }
}

function clearInitialRetryTimer(): void {
  if (retryTimer === undefined) {
    return;
  }

  window.clearTimeout(retryTimer);
  retryTimer = undefined;
}

function scheduleInitialRetry(): void {
  if (initialRetryDone || retryTimer !== undefined) {
    return;
  }

  const retry = (): void => {
    if (initialRetryDone) {
      return;
    }

    initialRetryDone = true;
    clearInitialRetryTimer();
    mirrorSourcepointConsent();
  };

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', retry, { once: true });
  }

  retryTimer = window.setTimeout(retry, INITIAL_RETRY_DELAY_MS);
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
    clearSourcepointCookies();
    log.debug('sourcepoint: no GPP data found in localStorage');
    return false;
  }

  const { gppString, applicableSections } = payload.gppData;
  if (!gppString) {
    clearSourcepointCookies();
    log.debug('sourcepoint: gppString is empty');
    return false;
  }

  const existingGppCookie = readCookie(GPP_COOKIE_NAME);
  if (existingGppCookie && existingGppCookie !== gppString && !hasSourcepointMarker()) {
    log.debug('sourcepoint: preserving existing __gpp cookie from another writer');
    return false;
  }

  writeCookie(GPP_SOURCE_COOKIE_NAME, GPP_SOURCE_SOURCEPOINT);
  writeCookie(GPP_COOKIE_NAME, gppString);

  if (Array.isArray(applicableSections) && applicableSections.length > 0) {
    writeCookie(GPP_SID_COOKIE_NAME, applicableSections.join(','));
  } else {
    clearCookie(GPP_SID_COOKIE_NAME);
  }

  initialRetryDone = true;
  clearInitialRetryTimer();

  log.info('sourcepoint: mirrored GPP consent to cookies', {
    gppLength: gppString.length,
    sections: applicableSections,
  });

  return true;
}

/**
 * Initializes Sourcepoint consent mirroring and bounded refresh hooks.
 */
export function initializeSourcepointConsentMirror(): void {
  if (initialized || typeof window === 'undefined' || typeof document === 'undefined') {
    return;
  }

  initialized = true;

  if (!mirrorSourcepointConsent()) {
    scheduleInitialRetry();
  }

  // Sourcepoint persists consent changes to localStorage. Re-mirror when a
  // user returns to the page so session cookies do not remain stale.
  document.addEventListener('visibilitychange', mirrorOnVisible);
  window.addEventListener('focus', mirrorSourcepointConsent);
}

initializeSourcepointConsentMirror();
