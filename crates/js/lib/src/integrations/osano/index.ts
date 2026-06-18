import { log } from '../../core/log';

const MARKER_COOKIE_NAME = '_ts_consent_src';
const MARKER_COOKIE_VALUE = 'osano';
const US_PRIVACY_COOKIE_NAME = 'us_privacy';
const GPP_COOKIE_NAME = '__gpp';
const GPP_SID_COOKIE_NAME = '__gpp_sid';
const TCF_COOKIE_NAME = 'euconsent-v2';
const TARGET_COOKIE_NAMES = [
  US_PRIVACY_COOKIE_NAME,
  GPP_COOKIE_NAME,
  GPP_SID_COOKIE_NAME,
  TCF_COOKIE_NAME,
];
const API_TIMEOUT_MS = 500;
const OSANO_RETRY_DELAY_MS = 250;
const OSANO_MAX_RETRIES = 20;
const MIRROR_DEBOUNCE_MS = 0;

const OSANO_EVENTS = [
  'osano-cm-initialized',
  'osano-cm-consent-saved',
  'osano-cm-consent-new',
  'osano-cm-consent-changed',
  'osano-cm-opt-out',
  'osano-cm-storage',
] as const;
const OSANO_CLEAR_READY_EVENTS = new Set<string>([
  'osano-cm-initialized',
  'osano-cm-consent-saved',
  'osano-cm-consent-new',
  'osano-cm-consent-changed',
  'osano-cm-opt-out',
]);

interface UspData {
  uspString?: string;
}

interface GppPingData {
  signalStatus?: string;
  gppString?: string;
  applicableSections?: number[];
}

interface TcfData {
  tcString?: string;
  eventStatus?: string;
}

interface OsanoCm {
  addEventListener?: (eventName: string, callback: (payload?: unknown) => void) => void;
  removeEventListener?: (eventName: string, callback: (payload?: unknown) => void) => void;
}

type UspApi = (
  command: 'getUSPData',
  version: 1,
  callback: (data?: UspData, success?: boolean) => void
) => void;

type GppApi = (command: 'ping', callback: (data?: GppPingData, success?: boolean) => void) => void;

type TcfApi = (
  command: 'getTCData',
  version: 2,
  callback: (data?: TcfData, success?: boolean) => void
) => void;

type OsanoWindow = Window & {
  Osano?: {
    cm?: OsanoCm;
  };
  __uspapi?: UspApi;
  __gpp?: GppApi;
  __tcfapi?: TcfApi;
};

interface CookieWrite {
  name: string;
  value: string;
}

interface SignalResult {
  writes: CookieWrite[];
  clears: string[];
  pending: boolean;
}

interface MirrorPlan {
  writes: CookieWrite[];
  clears: string[];
  pending: boolean;
}

let initialized = false;
let osanoListenersInstalled = false;
let osanoRetryCount = 0;
let osanoReadyForClears = false;
let osanoRetryTimer: number | undefined;
let mirrorTimer: number | undefined;
let mirrorGeneration = 0;
let osanoEventHandlers: Map<string, (payload?: unknown) => void> | undefined;
let focusHandler: (() => void) | undefined;
let visibilityHandler: (() => void) | undefined;

function getWindow(): OsanoWindow | undefined {
  if (typeof window === 'undefined') return undefined;
  return window as OsanoWindow;
}

function readCookie(name: string): string | undefined {
  if (typeof document === 'undefined') return undefined;

  const prefix = `${name}=`;
  const cookie = document.cookie.split('; ').find((entry) => entry.startsWith(prefix));
  return cookie?.slice(prefix.length);
}

function writeCookie(name: string, value: string): void {
  document.cookie = `${name}=${value}; Path=/; Secure; SameSite=Lax`;
}

function clearCookie(name: string): void {
  document.cookie = `${name}=; Path=/; Secure; SameSite=Lax; Max-Age=0`;
}

function hasAnyTargetCookie(): boolean {
  return TARGET_COOKIE_NAMES.some((name) => readCookie(name) !== undefined);
}

function ownsConsentCookies(): boolean {
  return readCookie(MARKER_COOKIE_NAME) === MARKER_COOKIE_VALUE;
}

function canWriteConsentCookies(): boolean {
  const marker = readCookie(MARKER_COOKIE_NAME);
  if (marker === MARKER_COOKIE_VALUE) return true;

  if (marker !== undefined) {
    log.debug('osano: preserving consent cookies owned by another mirror', { marker });
    return false;
  }

  if (hasAnyTargetCookie()) {
    log.debug('osano: preserving existing unmarked consent cookies');
    return false;
  }

  return true;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}

function isNumberArray(value: unknown): value is number[] {
  return Array.isArray(value) && value.every((item) => typeof item === 'number');
}

function shouldWriteGppSid(
  applicableSections: number[] | undefined
): applicableSections is number[] {
  return (
    Array.isArray(applicableSections) &&
    applicableSections.length > 0 &&
    !applicableSections.includes(-1)
  );
}

function isTcfReady(eventStatus: unknown): boolean {
  return eventStatus === 'tcloaded' || eventStatus === 'useractioncomplete';
}

function signalResult(writes: CookieWrite[] = [], clears: string[] = []): SignalResult {
  return { writes, clears, pending: false };
}

function pendingResult(): SignalResult {
  return { writes: [], clears: [], pending: true };
}

function unavailableResult(): SignalResult {
  return { writes: [], clears: [], pending: false };
}

function emptyAfterOsanoReadyResult(cookieNames: string | string[]): SignalResult {
  if (!osanoReadyForClears) {
    return pendingResult();
  }

  return signalResult([], Array.isArray(cookieNames) ? cookieNames : [cookieNames]);
}

function finishOnce<T>(finish: (value: T) => void): (value: T) => void {
  let settled = false;
  return (value: T): void => {
    if (settled) return;
    settled = true;
    finish(value);
  };
}

function readUspSignal(win: OsanoWindow): Promise<SignalResult> {
  if (typeof win.__uspapi !== 'function') return Promise.resolve(unavailableResult());

  return new Promise((resolve) => {
    const done = finishOnce((result: SignalResult) => {
      window.clearTimeout(timer);
      resolve(result);
    });
    const timer = window.setTimeout(() => done(pendingResult()), API_TIMEOUT_MS);

    try {
      win.__uspapi?.('getUSPData', 1, (data, success) => {
        if (success === false || !isRecord(data)) {
          done(pendingResult());
          return;
        }

        if ('uspString' in data && typeof data.uspString !== 'string') {
          done(pendingResult());
          return;
        }

        if (typeof data.uspString === 'string' && data.uspString.length > 0) {
          done(signalResult([{ name: US_PRIVACY_COOKIE_NAME, value: data.uspString }]));
          return;
        }

        done(emptyAfterOsanoReadyResult(US_PRIVACY_COOKIE_NAME));
      });
    } catch (error) {
      log.debug('osano: __uspapi getUSPData failed', { error });
      done(pendingResult());
    }
  });
}

function readGppSignal(win: OsanoWindow): Promise<SignalResult> {
  if (typeof win.__gpp !== 'function') return Promise.resolve(unavailableResult());

  return new Promise((resolve) => {
    const done = finishOnce((result: SignalResult) => {
      window.clearTimeout(timer);
      resolve(result);
    });
    const timer = window.setTimeout(() => done(pendingResult()), API_TIMEOUT_MS);

    try {
      win.__gpp?.('ping', (data, success) => {
        if (success === false || !isRecord(data)) {
          done(pendingResult());
          return;
        }

        if (data.signalStatus !== 'ready') {
          done(pendingResult());
          return;
        }

        if ('gppString' in data && typeof data.gppString !== 'string') {
          done(pendingResult());
          return;
        }

        if (
          'applicableSections' in data &&
          data.applicableSections !== undefined &&
          !isNumberArray(data.applicableSections)
        ) {
          done(pendingResult());
          return;
        }

        const applicableSections = data.applicableSections;
        if (typeof data.gppString === 'string' && data.gppString.length > 0) {
          const writes = [{ name: GPP_COOKIE_NAME, value: data.gppString }];
          const clears: string[] = [];

          if (shouldWriteGppSid(applicableSections)) {
            writes.push({ name: GPP_SID_COOKIE_NAME, value: applicableSections.join(',') });
          } else {
            clears.push(GPP_SID_COOKIE_NAME);
          }

          done(signalResult(writes, clears));
          return;
        }

        done(emptyAfterOsanoReadyResult([GPP_COOKIE_NAME, GPP_SID_COOKIE_NAME]));
      });
    } catch (error) {
      log.debug('osano: __gpp ping failed', { error });
      done(pendingResult());
    }
  });
}

function readTcfSignal(win: OsanoWindow): Promise<SignalResult> {
  if (typeof win.__tcfapi !== 'function') return Promise.resolve(unavailableResult());

  return new Promise((resolve) => {
    const done = finishOnce((result: SignalResult) => {
      window.clearTimeout(timer);
      resolve(result);
    });
    const timer = window.setTimeout(() => done(pendingResult()), API_TIMEOUT_MS);

    try {
      win.__tcfapi?.('getTCData', 2, (data, success) => {
        if (success === false || !isRecord(data)) {
          done(pendingResult());
          return;
        }

        if (!isTcfReady(data.eventStatus)) {
          done(pendingResult());
          return;
        }

        if ('tcString' in data && typeof data.tcString !== 'string') {
          done(pendingResult());
          return;
        }

        if (typeof data.tcString === 'string' && data.tcString.length > 0) {
          done(signalResult([{ name: TCF_COOKIE_NAME, value: data.tcString }]));
          return;
        }

        done(emptyAfterOsanoReadyResult(TCF_COOKIE_NAME));
      });
    } catch (error) {
      log.debug('osano: __tcfapi getTCData failed', { error });
      done(pendingResult());
    }
  });
}

async function buildMirrorPlan(win: OsanoWindow): Promise<MirrorPlan> {
  const results = await Promise.all([readUspSignal(win), readGppSignal(win), readTcfSignal(win)]);

  return {
    writes: results.flatMap((result) => result.writes),
    clears: results.flatMap((result) => result.clears),
    pending: results.some((result) => result.pending),
  };
}

function applyMirrorPlan(plan: MirrorPlan): boolean {
  if (plan.writes.length === 0 && plan.clears.length === 0) {
    return false;
  }

  if (!canWriteConsentCookies()) {
    return false;
  }

  const writeNames = new Set(plan.writes.map((write) => write.name));
  for (const name of plan.clears) {
    if (!writeNames.has(name)) clearCookie(name);
  }

  for (const write of plan.writes) {
    writeCookie(write.name, write.value);
  }

  if (hasAnyTargetCookie()) {
    writeCookie(MARKER_COOKIE_NAME, MARKER_COOKIE_VALUE);
  } else if (ownsConsentCookies()) {
    clearCookie(MARKER_COOKIE_NAME);
  }

  log.info('osano: mirrored consent to standard cookies', {
    writes: plan.writes.map((write) => write.name),
    clears: plan.clears,
    pending: plan.pending,
  });

  return true;
}

/**
 * Mirrors Osano's IAB API consent signals into standard first-party cookies.
 *
 * Returns `true` when any cookie was written or cleared, `false` otherwise.
 */
export async function mirrorOsanoConsent(): Promise<boolean> {
  if (typeof document === 'undefined') return false;

  const win = getWindow();
  if (!win) return false;

  const generation = (mirrorGeneration += 1);
  const plan = await buildMirrorPlan(win);

  if (generation !== mirrorGeneration) {
    return false;
  }

  return applyMirrorPlan(plan);
}

function scheduleMirror(): void {
  if (mirrorTimer !== undefined || typeof window === 'undefined') return;

  mirrorTimer = window.setTimeout(() => {
    mirrorTimer = undefined;
    void mirrorOsanoConsent();
  }, MIRROR_DEBOUNCE_MS);
}

function installOsanoListeners(): boolean {
  const cm = getWindow()?.Osano?.cm;
  if (!cm) return false;

  if (osanoListenersInstalled) {
    scheduleMirror();
    return true;
  }

  if (typeof cm.addEventListener !== 'function') {
    return false;
  }

  osanoEventHandlers = new Map();
  for (const eventName of OSANO_EVENTS) {
    const handler = (): void => {
      if (OSANO_CLEAR_READY_EVENTS.has(eventName)) {
        osanoReadyForClears = true;
      }
      scheduleMirror();
    };
    osanoEventHandlers.set(eventName, handler);
    cm.addEventListener(eventName, handler);
  }
  osanoListenersInstalled = true;

  scheduleMirror();
  return true;
}

function scheduleOsanoRetry(): void {
  if (osanoRetryTimer !== undefined || osanoRetryCount >= OSANO_MAX_RETRIES) return;

  osanoRetryCount += 1;
  osanoRetryTimer = window.setTimeout(() => {
    osanoRetryTimer = undefined;
    if (!installOsanoListeners()) {
      scheduleOsanoRetry();
    }
  }, OSANO_RETRY_DELAY_MS);
}

function mirrorOnVisible(): void {
  if (document.visibilityState === 'visible') {
    scheduleMirror();
  }
}

/**
 * Initializes the Osano consent mirror.
 */
export function initializeOsanoConsentMirror(): void {
  if (initialized || typeof window === 'undefined' || typeof document === 'undefined') {
    return;
  }

  initialized = true;
  focusHandler = () => scheduleMirror();
  visibilityHandler = () => mirrorOnVisible();
  window.addEventListener('focus', focusHandler);
  document.addEventListener('visibilitychange', visibilityHandler);

  scheduleMirror();

  if (!installOsanoListeners()) {
    scheduleOsanoRetry();
  }
}

/** Resets module state for unit tests. */
export function resetOsanoConsentMirrorForTest(): void {
  const cm = getWindow()?.Osano?.cm;
  if (
    osanoListenersInstalled &&
    osanoEventHandlers &&
    cm &&
    typeof cm.removeEventListener === 'function'
  ) {
    for (const [eventName, handler] of osanoEventHandlers) {
      cm.removeEventListener(eventName, handler);
    }
  }

  if (focusHandler) window.removeEventListener('focus', focusHandler);
  if (visibilityHandler) document.removeEventListener('visibilitychange', visibilityHandler);
  if (osanoRetryTimer !== undefined) window.clearTimeout(osanoRetryTimer);
  if (mirrorTimer !== undefined) window.clearTimeout(mirrorTimer);

  initialized = false;
  osanoListenersInstalled = false;
  osanoRetryCount = 0;
  osanoReadyForClears = false;
  mirrorGeneration = 0;
  osanoRetryTimer = undefined;
  mirrorTimer = undefined;
  osanoEventHandlers = undefined;
  focusHandler = undefined;
  visibilityHandler = undefined;
}

initializeOsanoConsentMirror();
