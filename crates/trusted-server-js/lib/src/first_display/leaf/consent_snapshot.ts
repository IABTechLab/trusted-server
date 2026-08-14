import type { FirstDisplaySliceActivationContext } from '../transaction';

export type FirstDisplayConsentRouteKindV1 = 'script' | 'preload' | 'prefetch';

export interface FirstDisplayConsentRouteRuleV1 {
  readonly id: 'sourcepoint';
  readonly matches: (kind: FirstDisplayConsentRouteKindV1, url: string) => boolean;
  readonly rewrite: (url: string) => string;
}

interface CookieDocument {
  cookie: string;
}

interface StorageReader {
  readonly length: number;
  readonly getItem: (key: string) => string | null;
  readonly key: (index: number) => string | null;
}

interface SourcepointInitialBindings {
  readonly config: Readonly<{ rewriteSdk: boolean }>;
  readonly document: CookieDocument;
  readonly observe: (name: string, value: string | number) => void;
  readonly origin: string;
  readonly registerRoute: (rule: FirstDisplayConsentRouteRuleV1) => () => void;
  readonly storage: StorageReader;
}

interface OsanoInitialBindings {
  readonly clearTimer: (handle: unknown) => void;
  readonly document: CookieDocument;
  readonly observe: (name: string, value: string | number) => void;
  readonly setTimer: (callback: () => void, delayMs: number) => unknown;
  readonly target: OsanoTarget;
}

interface OsanoTarget {
  readonly __uspapi?: (
    command: 'getUSPData',
    version: 1,
    callback: (data?: unknown, success?: boolean) => void
  ) => void;
  readonly __gpp?: (command: 'ping', callback: (data?: unknown, success?: boolean) => void) => void;
  readonly __tcfapi?: (
    command: 'getTCData',
    version: 2,
    callback: (data?: unknown, success?: boolean) => void
  ) => void;
}

interface ConsentWrite {
  readonly name: string;
  readonly value: string;
}

interface ConsentResult {
  readonly writes: readonly ConsentWrite[];
  readonly clears: readonly string[];
}

const SCRIPT_KINDS = new Set<FirstDisplayConsentRouteKindV1>(['script', 'preload', 'prefetch']);
const OSANO_MARKER = '_ts_consent_src';
const OSANO_VALUE = 'osano';
const OSANO_TARGETS = ['us_privacy', '__gpp', '__gpp_sid', 'euconsent-v2'] as const;
const SOURCEPOINT_MARKER = '_ts_gpp_src';
const SOURCEPOINT_VALUE = 'sp';
const MAX_STORAGE_ENTRIES = 512;

function exactRecord(value: unknown, keys: readonly string[]): Record<string, unknown> | undefined {
  try {
    if (
      typeof value !== 'object' ||
      value === null ||
      Array.isArray(value) ||
      Object.getPrototypeOf(value) !== Object.prototype ||
      !Object.isFrozen(value)
    ) {
      return undefined;
    }
    const ownKeys = Reflect.ownKeys(value);
    if (
      ownKeys.length !== keys.length ||
      !ownKeys.every((key) => typeof key === 'string' && keys.includes(key))
    ) {
      return undefined;
    }
    const result: Record<string, unknown> = {};
    for (const key of keys) {
      const descriptor = Object.getOwnPropertyDescriptor(value, key);
      if (!descriptor?.enumerable || !('value' in descriptor)) return undefined;
      result[key] = descriptor.value;
    }
    return result;
  } catch {
    return undefined;
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function readCookie(document: CookieDocument, name: string): string | undefined {
  try {
    const prefix = `${name}=`;
    return document.cookie
      .split('; ')
      .find((entry) => entry.startsWith(prefix))
      ?.slice(prefix.length);
  } catch {
    return undefined;
  }
}

function writeCookie(document: CookieDocument, name: string, value: string): void {
  document.cookie = `${name}=${value}; Path=/; Secure; SameSite=Lax`;
}

function clearCookie(document: CookieDocument, name: string): void {
  document.cookie = `${name}=; Path=/; Secure; SameSite=Lax; Max-Age=0`;
}

function createCookieOwner(document: CookieDocument): {
  readonly clear: (name: string) => void;
  readonly dispose: () => void;
  readonly write: (name: string, value: string) => void;
} {
  const previous = new Map<string, string | undefined>();
  const installed = new Map<string, string | undefined>();
  const remember = (name: string): void => {
    if (!previous.has(name)) previous.set(name, readCookie(document, name));
  };
  return Object.freeze({
    clear: (name: string): void => {
      remember(name);
      clearCookie(document, name);
      installed.set(name, undefined);
    },
    dispose: (): void => {
      for (const [name, installedValue] of [...installed].reverse()) {
        const current = readCookie(document, name);
        if (current !== installedValue) continue;
        const previousValue = previous.get(name);
        if (previousValue === undefined) clearCookie(document, name);
        else writeCookie(document, name, previousValue);
      }
      installed.clear();
      previous.clear();
    },
    write: (name: string, value: string): void => {
      remember(name);
      writeCookie(document, name, value);
      installed.set(name, value);
    },
  });
}

function sourcepointBindings(candidate: unknown): SourcepointInitialBindings | undefined {
  const fields = exactRecord(candidate, [
    'config',
    'document',
    'observe',
    'origin',
    'registerRoute',
    'storage',
  ]);
  const config = exactRecord(fields?.config, ['rewriteSdk']);
  if (
    !fields ||
    !config ||
    typeof config.rewriteSdk !== 'boolean' ||
    typeof fields.document !== 'object' ||
    fields.document === null ||
    typeof (fields.document as CookieDocument).cookie !== 'string' ||
    typeof fields.observe !== 'function' ||
    typeof fields.origin !== 'string' ||
    typeof fields.registerRoute !== 'function' ||
    typeof fields.storage !== 'object' ||
    fields.storage === null
  ) {
    return undefined;
  }
  const storage = fields.storage as StorageReader;
  if (
    typeof storage.length !== 'number' ||
    typeof storage.key !== 'function' ||
    typeof storage.getItem !== 'function'
  ) {
    return undefined;
  }
  try {
    if (new URL(fields.origin).origin !== fields.origin) return undefined;
  } catch {
    return undefined;
  }
  return {
    config: Object.freeze({ rewriteSdk: config.rewriteSdk }),
    document: fields.document as CookieDocument,
    observe: fields.observe as SourcepointInitialBindings['observe'],
    origin: fields.origin,
    registerRoute: fields.registerRoute as SourcepointInitialBindings['registerRoute'],
    storage,
  };
}

function normalizeSourcepointUrl(url: string): URL | undefined {
  const trimmed = url.trim();
  if (!trimmed) return undefined;
  try {
    return new URL(
      trimmed.startsWith('//')
        ? `https:${trimmed}`
        : /^https?:\/\//.test(trimmed)
          ? trimmed
          : `https://${trimmed}`
    );
  } catch {
    return undefined;
  }
}

function sourcepointConsent(
  storage: StorageReader
): Readonly<{ applicableSections?: readonly number[]; gppString: string }> | undefined {
  let length: number;
  try {
    length = Math.min(Math.max(0, storage.length), MAX_STORAGE_ENTRIES);
  } catch {
    return undefined;
  }
  for (let index = 0; index < length; index += 1) {
    let key: string | null;
    let raw: string | null;
    try {
      key = storage.key(index);
      if (!key?.startsWith('_sp_user_consent_')) continue;
      raw = storage.getItem(key);
    } catch {
      continue;
    }
    if (!raw) continue;
    try {
      const payload = JSON.parse(raw);
      const gppData = payload?.gppData;
      if (typeof gppData?.gppString === 'string' && gppData.gppString.length > 0) {
        return Object.freeze({
          gppString: gppData.gppString,
          ...(Array.isArray(gppData.applicableSections) &&
          gppData.applicableSections.every((value: unknown) => typeof value === 'number')
            ? { applicableSections: Object.freeze([...gppData.applicableSections]) }
            : {}),
        });
      }
      for (const [name, section] of Object.entries(payload ?? {})) {
        if (name === 'gppData' || !isRecord(section)) continue;
        const consentString = section.consentString;
        if (typeof consentString !== 'string' || !consentString.includes('~')) continue;
        let applicableSections: readonly number[] | undefined;
        if (
          Array.isArray(section.applicableSections) &&
          section.applicableSections.every((value) => typeof value === 'number')
        ) {
          applicableSections = Object.freeze([...section.applicableSections]);
        } else if (Array.isArray(section.consentStrings)) {
          const ids = section.consentStrings
            .map((entry: unknown) => (isRecord(entry) ? entry.sectionId : undefined))
            .filter((value: unknown): value is number => typeof value === 'number');
          if (ids.length > 0) applicableSections = Object.freeze(ids);
        }
        return Object.freeze({
          gppString: consentString,
          ...(applicableSections ? { applicableSections } : {}),
        });
      }
    } catch {
      // Continue to the next origin-scoped Sourcepoint payload.
    }
  }
  return undefined;
}

/** Own Sourcepoint's one-shot SDK route and initial GPP storage mirror. */
export function installSourcepointInitial(
  candidate: unknown,
  own: FirstDisplaySliceActivationContext['own']
): void {
  const bindings = sourcepointBindings(candidate);
  if (!bindings || typeof own !== 'function') {
    throw new TypeError('invalid Sourcepoint initial bindings');
  }
  if (bindings.config.rewriteSdk) {
    const release = bindings.registerRoute(
      Object.freeze({
        id: 'sourcepoint' as const,
        matches: (kind: FirstDisplayConsentRouteKindV1, url: string) =>
          SCRIPT_KINDS.has(kind) &&
          normalizeSourcepointUrl(url)?.hostname === 'cdn.privacy-mgmt.com',
        rewrite: (url: string) => {
          const parsed = normalizeSourcepointUrl(url);
          return parsed
            ? `${bindings.origin}/integrations/sourcepoint/cdn${parsed.pathname}${parsed.search}`
            : url;
        },
      })
    );
    if (typeof release !== 'function') throw new TypeError('invalid Sourcepoint route disposer');
    own(release);
  }

  const cookies = createCookieOwner(bindings.document);
  own(cookies.dispose);
  const consent = sourcepointConsent(bindings.storage);
  const marker = readCookie(bindings.document, SOURCEPOINT_MARKER);
  if (!consent) {
    if (marker === SOURCEPOINT_VALUE) {
      cookies.clear('__gpp');
      cookies.clear('__gpp_sid');
      cookies.clear(SOURCEPOINT_MARKER);
    }
    bindings.observe('gpp_snapshot', 0);
    return;
  }
  const existingGpp = readCookie(bindings.document, '__gpp');
  if (existingGpp && existingGpp !== consent.gppString && marker !== SOURCEPOINT_VALUE) {
    bindings.observe('gpp_snapshot', 0);
    return;
  }
  cookies.write(SOURCEPOINT_MARKER, SOURCEPOINT_VALUE);
  cookies.write('__gpp', consent.gppString);
  if (consent.applicableSections && consent.applicableSections.length > 0) {
    cookies.write('__gpp_sid', consent.applicableSections.join(','));
  } else {
    cookies.clear('__gpp_sid');
  }
  bindings.observe('gpp_snapshot', consent.gppString.length);
}

function osanoBindings(candidate: unknown): OsanoInitialBindings | undefined {
  const fields = exactRecord(candidate, [
    'clearTimer',
    'document',
    'observe',
    'setTimer',
    'target',
  ]);
  if (
    !fields ||
    typeof fields.clearTimer !== 'function' ||
    typeof fields.document !== 'object' ||
    fields.document === null ||
    typeof (fields.document as CookieDocument).cookie !== 'string' ||
    typeof fields.observe !== 'function' ||
    typeof fields.setTimer !== 'function' ||
    typeof fields.target !== 'object' ||
    fields.target === null
  ) {
    return undefined;
  }
  return fields as unknown as OsanoInitialBindings;
}

function emptyResult(): ConsentResult {
  return Object.freeze({ writes: Object.freeze([]), clears: Object.freeze([]) });
}

function canWriteOsano(document: CookieDocument): boolean {
  const marker = readCookie(document, OSANO_MARKER);
  if (marker === OSANO_VALUE) return true;
  if (marker !== undefined) return false;
  return OSANO_TARGETS.every((name) => readCookie(document, name) === undefined);
}

/** Own the initial one-shot Osano USP/GPP/TCF snapshot, without later lifecycle hooks. */
export function installOsanoInitial(
  candidate: unknown,
  own: FirstDisplaySliceActivationContext['own']
): void {
  const bindings = osanoBindings(candidate);
  if (!bindings || typeof own !== 'function') throw new TypeError('invalid Osano initial bindings');

  const cookies = createCookieOwner(bindings.document);
  const timers = new Set<unknown>();
  let active = true;
  own(() => {
    if (!active) return;
    active = false;
    for (const timer of timers) {
      try {
        bindings.clearTimer(timer);
      } catch {
        // Continue releasing the remaining initial API timeouts.
      }
    }
    timers.clear();
    cookies.dispose();
  });

  const results: ConsentResult[] = [];
  let remaining = 3;
  const finish = (result: ConsentResult): void => {
    if (!active) return;
    results.push(result);
    remaining -= 1;
    if (remaining !== 0) return;
    const writes = results.flatMap((entry) => entry.writes);
    const clears = results.flatMap((entry) => entry.clears);
    if ((writes.length === 0 && clears.length === 0) || !canWriteOsano(bindings.document)) {
      bindings.observe('consent_snapshot', 0);
      return;
    }
    const writtenNames = new Set(writes.map(({ name }) => name));
    for (const name of clears) if (!writtenNames.has(name)) cookies.clear(name);
    for (const { name, value } of writes) cookies.write(name, value);
    const hasTarget = OSANO_TARGETS.some(
      (name) => readCookie(bindings.document, name) !== undefined
    );
    if (hasTarget) cookies.write(OSANO_MARKER, OSANO_VALUE);
    else if (readCookie(bindings.document, OSANO_MARKER) === OSANO_VALUE) {
      cookies.clear(OSANO_MARKER);
    }
    bindings.observe('consent_snapshot', writes.length + clears.length);
  };

  const invoke = (
    available: boolean,
    call: (callback: (data?: unknown, success?: boolean) => void) => void,
    parse: (data: unknown, success: boolean | undefined) => ConsentResult
  ): void => {
    if (!available) {
      finish(emptyResult());
      return;
    }
    let settled = false;
    const timerBox: { value?: unknown } = {};
    const done = (result: ConsentResult): void => {
      if (settled) return;
      settled = true;
      if (timerBox.value !== undefined) {
        timers.delete(timerBox.value);
        try {
          bindings.clearTimer(timerBox.value);
        } catch {
          // Timer cancellation failure does not permit a second settlement.
        }
      }
      finish(result);
    };
    const handle = bindings.setTimer(() => done(emptyResult()), 500);
    timerBox.value = handle;
    if (settled) {
      try {
        bindings.clearTimer(handle);
      } catch {
        // A synchronous timer cannot reopen an already-settled signal.
      }
    } else {
      timers.add(handle);
    }
    try {
      call((data, success) => {
        try {
          done(parse(data, success));
        } catch {
          done(emptyResult());
        }
      });
    } catch {
      done(emptyResult());
    }
  };

  const usp = bindings.target.__uspapi;
  invoke(
    typeof usp === 'function',
    (callback) => usp?.('getUSPData', 1, callback),
    (data, success) => {
      if (success === false || !isRecord(data)) return emptyResult();
      if ('uspString' in data && typeof data.uspString !== 'string') return emptyResult();
      return typeof data.uspString === 'string' && data.uspString.length > 0
        ? Object.freeze({
            writes: Object.freeze([{ name: 'us_privacy', value: data.uspString }]),
            clears: Object.freeze([]),
          })
        : emptyResult();
    }
  );

  const gpp = bindings.target.__gpp;
  invoke(
    typeof gpp === 'function',
    (callback) => gpp?.('ping', callback),
    (data, success) => {
      if (success === false || !isRecord(data) || data.signalStatus !== 'ready') {
        return emptyResult();
      }
      if ('gppString' in data && typeof data.gppString !== 'string') return emptyResult();
      if (
        'applicableSections' in data &&
        data.applicableSections !== undefined &&
        (!Array.isArray(data.applicableSections) ||
          !data.applicableSections.every((value) => typeof value === 'number'))
      ) {
        return emptyResult();
      }
      if (typeof data.gppString !== 'string' || data.gppString.length === 0) {
        return emptyResult();
      }
      const sections = data.applicableSections as number[] | undefined;
      const includeSections =
        Array.isArray(sections) && sections.length > 0 && !sections.includes(-1);
      return Object.freeze({
        writes: Object.freeze([
          { name: '__gpp', value: data.gppString },
          ...(includeSections ? [{ name: '__gpp_sid', value: sections.join(',') }] : []),
        ]),
        clears: Object.freeze(includeSections ? [] : ['__gpp_sid']),
      });
    }
  );

  const tcf = bindings.target.__tcfapi;
  invoke(
    typeof tcf === 'function',
    (callback) => tcf?.('getTCData', 2, callback),
    (data, success) => {
      if (
        success === false ||
        !isRecord(data) ||
        !['tcloaded', 'useractioncomplete'].includes(data.eventStatus as string) ||
        ('tcString' in data && typeof data.tcString !== 'string') ||
        typeof data.tcString !== 'string' ||
        data.tcString.length === 0
      ) {
        return emptyResult();
      }
      return Object.freeze({
        writes: Object.freeze([{ name: 'euconsent-v2', value: data.tcString }]),
        clears: Object.freeze([]),
      });
    }
  );
}
