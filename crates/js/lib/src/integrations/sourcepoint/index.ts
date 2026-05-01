import { log } from '../../core/log';

const SP_CONSENT_PREFIX = '_sp_user_consent_';
const GPP_COOKIE_NAME = '__gpp';
const GPP_SID_COOKIE_NAME = '__gpp_sid';
const GPP_SOURCE_COOKIE_NAME = '_ts_gpp_src';
const GPP_SOURCE_SOURCEPOINT = 'sp';
const TCF_COOKIE_NAME = 'euconsent-v2';
const INITIAL_RETRY_DELAY_MS = 500;

interface SourcepointGppData {
  gppString: string;
  applicableSections: number[];
}

interface SourcepointConsentStringEntry {
  sectionId?: number;
  consentString?: string;
}

interface SourcepointSectionPayload {
  consentString?: string;
  applicableSections?: number[];
  consentStrings?: SourcepointConsentStringEntry[];
  tcString?: string;
  euconsent?: string;
  euconsentV2?: string;
}

interface SourcepointConsentPayload {
  gppData?: SourcepointGppData;
  gdpr?: SourcepointSectionPayload;
  usnat?: SourcepointSectionPayload;
  [key: string]: unknown;
}

interface MirroredConsent {
  gppString?: string;
  gppSections?: number[];
  tcString?: string;
}

let initialized = false;

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
        (typeof item.sectionId === 'number' || typeof item.sectionId === 'undefined') &&
        (typeof item.consentString === 'string' || typeof item.consentString === 'undefined')
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
    tcString: typeof value.tcString === 'string' ? value.tcString : undefined,
    euconsent: typeof value.euconsent === 'string' ? value.euconsent : undefined,
    euconsentV2: typeof value.euconsentV2 === 'string' ? value.euconsentV2 : undefined,
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

function extractConsentFromSection(
  sectionName: string,
  section: SourcepointSectionPayload
): MirroredConsent | null {
  const gppSections =
    section.applicableSections ?? sectionIdsFromConsentStrings(section.consentStrings);

  if (section.consentString && (gppSections || looksLikeGpp(section.consentString))) {
    return {
      gppString: section.consentString,
      gppSections,
    };
  }

  if (sectionName === 'gdpr') {
    const tcString =
      section.tcString ?? section.euconsentV2 ?? section.euconsent ?? section.consentString;
    if (tcString) {
      return { tcString };
    }
  }

  return null;
}

function mergeConsent(
  primary: MirroredConsent | null,
  secondary: MirroredConsent | null
): MirroredConsent | null {
  if (!primary) return secondary;
  if (!secondary) return primary;

  return {
    gppString: primary.gppString ?? secondary.gppString,
    gppSections: primary.gppSections ?? secondary.gppSections,
    tcString: primary.tcString ?? secondary.tcString,
  };
}

function extractMirroredConsent(payload: SourcepointConsentPayload): MirroredConsent | null {
  let consent: MirroredConsent | null = null;

  if (payload.gppData?.gppString) {
    consent = mergeConsent(consent, {
      gppString: payload.gppData.gppString,
      gppSections: payload.gppData.applicableSections,
    });
  }

  for (const [sectionName, rawSection] of Object.entries(payload)) {
    if (sectionName === 'gppData') continue;

    const section = normalizeSectionPayload(rawSection);
    if (!section) continue;

    consent = mergeConsent(consent, extractConsentFromSection(sectionName, section));
  }

  return consent;
}

function findSourcepointConsent(): MirroredConsent | null {
  for (let i = 0; i < localStorage.length; i++) {
    const key = localStorage.key(i);
    if (!key?.startsWith(SP_CONSENT_PREFIX)) continue;

    const raw = localStorage.getItem(key);
    if (!raw) continue;

    try {
      const payload = JSON.parse(raw) as SourcepointConsentPayload;
      const consent = extractMirroredConsent(payload);
      if (consent?.gppString || consent?.tcString) {
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

function scheduleInitialRetry(): void {
  const retry = (): void => {
    mirrorSourcepointConsent();
  };

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', retry, { once: true });
  }

  window.setTimeout(retry, INITIAL_RETRY_DELAY_MS);
}

/**
 * Reads Sourcepoint consent from localStorage and mirrors it into cookies for
 * Trusted Server to read on the next request.
 *
 * Sourcepoint localStorage differs by campaign/module. US National data is
 * commonly stored under `usnat.consentString`/`usnat.applicableSections`, while
 * some setups expose `gppData.gppString`. GDPR/UK setups may expose a TCF string
 * under `gdpr` fields. Trusted Server cannot read localStorage server-side, so
 * this bridge writes the standard IAB cookies.
 *
 * Returns `true` if cookies were written, `false` otherwise.
 */
export function mirrorSourcepointConsent(): boolean {
  if (typeof localStorage === 'undefined' || typeof document === 'undefined') {
    return false;
  }

  const consent = findSourcepointConsent();
  if (!consent) {
    clearSourcepointCookies();
    log.debug('sourcepoint: no consent data found in localStorage');
    return false;
  }

  let wroteCookie = false;

  if (consent.gppString) {
    writeCookie(GPP_SOURCE_COOKIE_NAME, GPP_SOURCE_SOURCEPOINT);
    writeCookie(GPP_COOKIE_NAME, consent.gppString);

    if (Array.isArray(consent.gppSections) && consent.gppSections.length > 0) {
      writeCookie(GPP_SID_COOKIE_NAME, consent.gppSections.join(','));
    } else {
      clearCookie(GPP_SID_COOKIE_NAME);
    }

    wroteCookie = true;
  } else {
    clearSourcepointCookies();
  }

  if (consent.tcString) {
    writeCookie(TCF_COOKIE_NAME, consent.tcString);
    wroteCookie = true;
  }

  if (wroteCookie) {
    log.info('sourcepoint: mirrored consent to cookies', {
      gppLength: consent.gppString?.length ?? 0,
      sections: consent.gppSections,
      tcLength: consent.tcString?.length ?? 0,
    });
  }

  return wroteCookie;
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

  document.addEventListener('visibilitychange', mirrorOnVisible);
  window.addEventListener('focus', mirrorSourcepointConsent);
}

initializeSourcepointConsentMirror();
