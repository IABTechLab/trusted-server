// Prebid's SharedID module also reads its opt-out from localStorage, which the
// server cannot see. Copy an active localStorage opt-out into the cookie the
// server honors (SID-2), and remove only a copy this script wrote.
const OPT_OUT_NAME = '_pubcid_optout';
const OPT_OUT_EXPIRY_NAME = `${OPT_OUT_NAME}_exp`;
const MARKER_NAME = '_ts_pubcid_optout_src';
const MARKER_VALUE = 'ls';
const COPY_VALUE = '1';
const ONE_YEAR_SECONDS = 365 * 24 * 60 * 60;
const ATTRIBUTES = 'Path=/; Secure; SameSite=Lax';

function readCookie(name: string): string | undefined {
  return document.cookie
    .split(';')
    .map((part) => part.trim())
    .find((part) => part.startsWith(`${name}=`))
    ?.slice(name.length + 1);
}

/**
 * Prebid 10.26's `readValue(_pubcid_optout, html5)` predicate: a truthy value
 * whose `_exp` companion is falsy or still in the future. Throws when storage
 * cannot be read, so callers can leave state untouched.
 */
export function localStorageOptOutActive(storage: Storage): boolean {
  const expiry = storage.getItem(OPT_OUT_EXPIRY_NAME);
  if (expiry && !(new Date(expiry).getTime() - Date.now() > 0)) return false;
  return !!storage.getItem(OPT_OUT_NAME);
}

function copyMaxAge(storage: Storage): number {
  const expiry = storage.getItem(OPT_OUT_EXPIRY_NAME);
  if (!expiry) return ONE_YEAR_SECONDS;
  const remaining = Math.floor((new Date(expiry).getTime() - Date.now()) / 1000);
  return Math.min(ONE_YEAR_SECONDS, Math.max(remaining, 0));
}

/**
 * Mirrors an active localStorage SharedID opt-out into `_pubcid_optout`, with
 * a marker so a later sync can tell its own copy from a publisher's cookie.
 * Any storage or cookie error leaves both cookies as they are.
 */
export function syncPubcidOptOut(): void {
  try {
    const storage = window.localStorage;
    const active = localStorageOptOutActive(storage);
    const maxAge = active ? copyMaxAge(storage) : 0;
    const cookieOptOut = !!readCookie(OPT_OUT_NAME);
    const marked = readCookie(MARKER_NAME) === MARKER_VALUE;
    if (active && !cookieOptOut && maxAge > 0) {
      document.cookie = `${OPT_OUT_NAME}=${COPY_VALUE}; ${ATTRIBUTES}; Max-Age=${maxAge}`;
      document.cookie = `${MARKER_NAME}=${MARKER_VALUE}; ${ATTRIBUTES}; Max-Age=${maxAge}`;
    } else if (!active && marked) {
      document.cookie = `${OPT_OUT_NAME}=; ${ATTRIBUTES}; Max-Age=0`;
      document.cookie = `${MARKER_NAME}=; ${ATTRIBUTES}; Max-Age=0`;
    }
  } catch {
    // Sandboxed frames and blocked storage must not change the reader's opt-out.
  }
}

/**
 * Reports a SharedID opt-out from a truthy cookie or localStorage, as Prebid
 * does. Fails
 * closed: when either store cannot be read, the reader is treated as opted out.
 */
export function isPubcidOptedOut(): boolean {
  try {
    return !!readCookie(OPT_OUT_NAME) || localStorageOptOutActive(window.localStorage);
  } catch {
    return true;
  }
}
