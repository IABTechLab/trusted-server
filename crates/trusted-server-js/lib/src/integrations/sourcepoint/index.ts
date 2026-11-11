import { log } from '../../core/log';

import { ConsentSync } from './consent_sync';
import { GppConsentCapture, isCookieSafeGppString, type GppWindow } from './gpp_capture';
import { installSourcepointGuard } from './script_guard';

type SourcepointWindow = GppWindow & {
  __tsjs_sourcepoint?: {
    rewriteSdk?: boolean;
  };
};

function shouldInstallSourcepointGuard(): boolean {
  if (typeof window === 'undefined') {
    return false;
  }

  const config = (window as SourcepointWindow).__tsjs_sourcepoint;
  return config?.rewriteSdk !== false;
}

if (typeof window !== 'undefined' && shouldInstallSourcepointGuard()) {
  installSourcepointGuard();
  log.info('Sourcepoint integration initialized');
}

const SP_CONSENT_PREFIX = '_sp_user_consent_';
const GPP_COOKIE_NAME = '__gpp';
const GPP_SID_COOKIE_NAME = '__gpp_sid';
const GPP_SOURCE_COOKIE_NAME = '_ts_gpp_src';
const GPP_SOURCE_SOURCEPOINT = 'sp';
const INITIAL_RETRY_DELAY_MS = 500;

interface SourcepointGppData {
  gppString?: string;
  applicableSections?: number[];
}

interface SourcepointConsentStringEntry {
  sectionId?: number;
}

interface SourcepointSectionPayload {
  consentString?: string;
  applicableSections?: number[];
  consentStrings?: SourcepointConsentStringEntry[];
}

interface SourcepointConsentPayload {
  gppData?: SourcepointGppData;
  [key: string]: unknown;
}

interface MirroredSourcepointConsent {
  gppString: string;
  applicableSections?: number[];
}

let initialized = false;
let initialRetryDone = false;
let retryTimer: ReturnType<typeof window.setTimeout> | undefined;
let readyRetry: (() => void) | undefined;
let gppCapture: GppConsentCapture | undefined;
let apiMirrorSucceeded = false;
let consentSync: ConsentSync | undefined;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}

function isNumberArray(value: unknown): value is number[] {
  return Array.isArray(value) && value.every((item) => typeof item === 'number');
}

function isConsentStringEntryArray(value: unknown): value is SourcepointConsentStringEntry[] {
  return (
    Array.isArray(value) &&
    value.every(
      (item) =>
        isRecord(item) &&
        (typeof item.sectionId === 'number' || typeof item.sectionId === 'undefined')
    )
  );
}

function normalizeSectionPayload(value: unknown): SourcepointSectionPayload | null {
  if (!isRecord(value)) return null;

  return {
    consentString: typeof value.consentString === 'string' ? value.consentString : undefined,
    applicableSections: isNumberArray(value.applicableSections)
      ? value.applicableSections
      : undefined,
    consentStrings: isConsentStringEntryArray(value.consentStrings)
      ? value.consentStrings
      : undefined,
  };
}

function sectionIdsFromConsentStrings(
  consentStrings: SourcepointConsentStringEntry[] | undefined
): number[] | undefined {
  const ids = consentStrings
    ?.map((entry) => entry.sectionId)
    .filter((sectionId): sectionId is number => typeof sectionId === 'number');

  return ids && ids.length > 0 ? ids : undefined;
}

function looksLikeGpp(consentString: string): boolean {
  return consentString.includes('~');
}

function extractMirroredConsent(
  payload: SourcepointConsentPayload
): MirroredSourcepointConsent | null {
  if (payload.gppData?.gppString) {
    return {
      gppString: payload.gppData.gppString,
      applicableSections: payload.gppData.applicableSections,
    };
  }

  for (const [sectionName, rawSection] of Object.entries(payload)) {
    if (sectionName === 'gppData') continue;

    const section = normalizeSectionPayload(rawSection);
    if (!section?.consentString || !looksLikeGpp(section.consentString)) continue;

    return {
      gppString: section.consentString,
      applicableSections:
        section.applicableSections ?? sectionIdsFromConsentStrings(section.consentStrings),
    };
  }

  return null;
}

function findSourcepointConsent(): MirroredSourcepointConsent | null {
  // Sourcepoint stores one consent payload per property under `_sp_user_consent_*`.
  // We intentionally take the first valid match and mirror that origin-scoped payload.
  for (let i = 0; i < localStorage.length; i++) {
    const key = localStorage.key(i);
    if (!key?.startsWith(SP_CONSENT_PREFIX)) continue;

    const raw = localStorage.getItem(key);
    if (!raw) continue;

    try {
      const payload = JSON.parse(raw) as SourcepointConsentPayload;
      const consent = extractMirroredConsent(payload);
      if (consent) {
        return consent;
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
  try {
    if (!hasSourcepointMarker()) return;
    clearCookie(GPP_COOKIE_NAME);
    clearCookie(GPP_SID_COOKIE_NAME);
    clearCookie(GPP_SOURCE_COOKIE_NAME);
    consentSync?.publish({ gppString: null, applicableSections: [] });
  } catch {
    log.warn('sourcepoint: consent cookie storage is unavailable');
  }
}

function mirrorOnVisible(): void {
  if (document.visibilityState === 'visible') {
    mirrorSourcepointConsent();
  }
}

function clearInitialRetryTimer(): void {
  if (retryTimer !== undefined) {
    window.clearTimeout(retryTimer);
    retryTimer = undefined;
  }
  if (readyRetry) {
    document.removeEventListener('DOMContentLoaded', readyRetry);
    readyRetry = undefined;
  }
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
    readyRetry = retry;
    document.addEventListener('DOMContentLoaded', retry, { once: true });
  }

  retryTimer = window.setTimeout(retry, INITIAL_RETRY_DELAY_MS);
}

/**
 * Reads Sourcepoint consent from localStorage and mirrors it into
 * `__gpp` and `__gpp_sid` cookies for Trusted Server to read.
 *
 * Sourcepoint stores different shapes depending on the campaign/module. US
 * National data is commonly stored under `usnat.consentString` and
 * `usnat.applicableSections`, while some setups expose `gppData.gppString`.
 *
 * Returns `true` if cookies were written, `false` otherwise.
 */
export function mirrorSourcepointConsent(): boolean {
  if (typeof document === 'undefined') return false;
  if (gppCapture?.hasApi()) {
    apiMirrorSucceeded = false;
    gppCapture.refresh();
    return apiMirrorSucceeded;
  }
  try {
    if (typeof localStorage === 'undefined') return false;
    const consent = findSourcepointConsent();
    if (!consent) {
      clearSourcepointCookies();
      log.debug('sourcepoint: no GPP data found in localStorage');
      return false;
    }
    return writeMirroredConsent(consent);
  } catch {
    // Sandboxed frames and browser storage restrictions must not break the
    // publisher's scripts or revive an earlier permissive mirror.
    clearSourcepointCookies();
    log.warn('sourcepoint: consent storage is unavailable');
    return false;
  }
}

function writeMirroredConsent(consent: MirroredSourcepointConsent): boolean {
  const { gppString, applicableSections } = consent;
  if (
    !isCookieSafeGppString(gppString) ||
    applicableSections?.some((id) => !Number.isInteger(id) || id <= 0 || id > 65535)
  ) {
    clearSourcepointCookies();
    log.debug('sourcepoint: GPP snapshot cannot be represented as consent cookies');
    return false;
  }

  const existingGppCookie = readCookie(GPP_COOKIE_NAME);
  if (existingGppCookie && !hasSourcepointMarker()) {
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
  consentSync?.publish({ gppString, applicableSections: applicableSections ?? [] });

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
  consentSync =
    typeof window.fetch === 'function' ? new ConsentSync(window.fetch.bind(window)) : undefined;
  gppCapture = new GppConsentCapture(
    window as GppWindow,
    (consent) => {
      try {
        apiMirrorSucceeded = writeMirroredConsent(consent);
      } catch {
        apiMirrorSucceeded = false;
        log.warn('sourcepoint: consent cookie storage is unavailable');
      }
    },
    () => {
      apiMirrorSucceeded = false;
      clearSourcepointCookies();
    }
  );
  gppCapture.start();

  if (!mirrorSourcepointConsent() && !gppCapture.hasApi()) scheduleInitialRetry();

  // Retain legacy localStorage refresh for CMPs without the GPP API. When an
  // API exists, its ready snapshot is authoritative, including same-tab changes.
  document.addEventListener('visibilitychange', mirrorOnVisible);
  window.addEventListener('focus', mirrorSourcepointConsent);
}

/** Releases this module's listeners without removing or replacing the publisher CMP. */
export function disposeSourcepointConsentMirror(): void {
  gppCapture?.stop();
  gppCapture = undefined;
  consentSync?.stop();
  consentSync = undefined;
  clearInitialRetryTimer();
  document.removeEventListener('visibilitychange', mirrorOnVisible);
  window.removeEventListener('focus', mirrorSourcepointConsent);
  initialized = false;
  initialRetryDone = false;
}

initializeSourcepointConsentMirror();
