import type { FirstDisplaySliceActivationContext } from '../transaction';

export type FirstDisplayRouteKindV1 = 'script' | 'preload' | 'prefetch' | 'beacon' | 'fetch';

export interface FirstDisplayRouteRuleV1 {
  readonly id: 'datadome' | 'google_tag_manager' | 'lockr';
  readonly matches: (kind: FirstDisplayRouteKindV1, url: string) => boolean;
  readonly rewrite: (url: string) => string;
}

interface RouteBindings {
  readonly observe: (name: string, value: string | number) => void;
  readonly origin: string;
  readonly register: (rule: FirstDisplayRouteRuleV1) => () => void;
}

interface LockrSdk {
  host: string;
}

interface LockrBindings extends RouteBindings {
  readonly clearTimer: (handle: unknown) => void;
  readonly getSdk: () => LockrSdk | undefined;
  readonly host: string;
  readonly protocol: string;
  readonly setTimer: (callback: () => void, delayMs: number) => unknown;
}

const DATADOME_URL = /^(?:https?:)?\/\/js\.datadome\.co(?:\/|$)|^js\.datadome\.co(?:\/|$)/i;
const GTM_URL =
  /^(?:https?:)?(?:\/\/)?(www\.(googletagmanager|google-analytics)\.com|analytics\.google\.com)(?:\/|$)/i;
const GTM_PATHS = new Set(['/gtm.js', '/gtag/js', '/gtag.js', '/collect', '/g/collect']);
const SCRIPT_KINDS = new Set<FirstDisplayRouteKindV1>(['script', 'preload', 'prefetch']);
const GTM_KINDS = new Set<FirstDisplayRouteKindV1>([
  'script',
  'preload',
  'prefetch',
  'beacon',
  'fetch',
]);

function exactBindings(
  candidate: unknown,
  keys: readonly string[]
): Record<string, unknown> | undefined {
  try {
    if (
      typeof candidate !== 'object' ||
      candidate === null ||
      Array.isArray(candidate) ||
      Object.getPrototypeOf(candidate) !== Object.prototype ||
      !Object.isFrozen(candidate)
    ) {
      return undefined;
    }
    const ownKeys = Reflect.ownKeys(candidate);
    if (
      ownKeys.length !== keys.length ||
      !ownKeys.every((key) => typeof key === 'string' && keys.includes(key))
    ) {
      return undefined;
    }
    const fields: Record<string, unknown> = {};
    for (const key of keys) {
      const descriptor = Object.getOwnPropertyDescriptor(candidate, key);
      if (!descriptor?.enumerable || !('value' in descriptor)) return undefined;
      fields[key] = descriptor.value;
    }
    return fields;
  } catch {
    return undefined;
  }
}

function routeBindings(candidate: unknown): RouteBindings | undefined {
  const fields = exactBindings(candidate, ['observe', 'origin', 'register']);
  if (
    !fields ||
    typeof fields.observe !== 'function' ||
    typeof fields.origin !== 'string' ||
    typeof fields.register !== 'function'
  ) {
    return undefined;
  }
  try {
    const parsed = new URL(fields.origin);
    if (!/^https?:$/.test(parsed.protocol) || parsed.origin !== fields.origin) return undefined;
  } catch {
    return undefined;
  }
  return fields as unknown as RouteBindings;
}

function absoluteExternalUrl(url: string): URL | undefined {
  try {
    const normalized = url.startsWith('//')
      ? `https:${url}`
      : /^https?:/i.test(url)
        ? url
        : `https://${url}`;
    return new URL(normalized);
  } catch {
    return undefined;
  }
}

function ownRule(
  bindings: RouteBindings,
  own: FirstDisplaySliceActivationContext['own'],
  rule: FirstDisplayRouteRuleV1
): void {
  const release = bindings.register(Object.freeze(rule));
  if (typeof release !== 'function') throw new TypeError('route guard disposer is invalid');
  own(release);
  bindings.observe('route_guard', rule.id);
}

/** Own DataDome's initial script/preload first-party routing. */
export function installDataDomeInitial(
  candidate: unknown,
  own: FirstDisplaySliceActivationContext['own']
): void {
  const bindings = routeBindings(candidate);
  if (!bindings) throw new TypeError('invalid DataDome initial bindings');
  ownRule(bindings, own, {
    id: 'datadome',
    matches: (kind, url) => SCRIPT_KINDS.has(kind) && DATADOME_URL.test(url),
    rewrite: (url) => {
      const parsed = absoluteExternalUrl(url);
      const suffix = parsed ? `${parsed.pathname}${parsed.search}` : '/tags.js';
      return `${bindings.origin}/integrations/datadome${suffix}`;
    },
  });
}

function isGtmUrl(url: string): boolean {
  if (!GTM_URL.test(url)) return false;
  const parsed = absoluteExternalUrl(url);
  return Boolean(parsed && GTM_PATHS.has(parsed.pathname));
}

/** Own GTM/GA's initial script, preload, beacon, and fetch first-party routing. */
export function installGoogleTagManagerInitial(
  candidate: unknown,
  own: FirstDisplaySliceActivationContext['own']
): void {
  const bindings = routeBindings(candidate);
  if (!bindings) throw new TypeError('invalid GTM initial bindings');
  ownRule(bindings, own, {
    id: 'google_tag_manager',
    matches: (kind, url) => GTM_KINDS.has(kind) && isGtmUrl(url),
    rewrite: (url) => {
      const parsed = absoluteExternalUrl(url);
      const suffix = parsed ? `${parsed.pathname}${parsed.search}` : '/gtm.js';
      return `${bindings.origin}/integrations/google_tag_manager${suffix}`;
    },
  });
}

function isLockrUrl(url: string): boolean {
  const lower = url.toLowerCase();
  return (
    lower.includes('aim.loc.kr') ||
    (lower.includes('identity.loc.kr') && lower.includes('identity-lockr') && lower.endsWith('.js'))
  );
}

function snapshotLockrBindings(candidate: unknown): LockrBindings | undefined {
  const fields = exactBindings(candidate, [
    'clearTimer',
    'getSdk',
    'host',
    'observe',
    'origin',
    'protocol',
    'register',
    'setTimer',
  ]);
  if (
    !fields ||
    typeof fields.clearTimer !== 'function' ||
    typeof fields.getSdk !== 'function' ||
    typeof fields.host !== 'string' ||
    fields.host.length === 0 ||
    fields.host.length > 255 ||
    fields.host.includes('/') ||
    typeof fields.observe !== 'function' ||
    typeof fields.origin !== 'string' ||
    typeof fields.protocol !== 'string' ||
    !['http:', 'https:'].includes(fields.protocol) ||
    typeof fields.register !== 'function' ||
    typeof fields.setTimer !== 'function'
  ) {
    return undefined;
  }
  const base = routeBindings(
    Object.freeze({ observe: fields.observe, origin: fields.origin, register: fields.register })
  );
  return base ? (fields as unknown as LockrBindings) : undefined;
}

/** Own Lockr's initial route guard and bounded API-host readiness observation. */
export function installLockrInitial(
  candidate: unknown,
  own: FirstDisplaySliceActivationContext['own']
): void {
  const bindings = snapshotLockrBindings(candidate);
  if (!bindings) throw new TypeError('invalid Lockr initial bindings');
  ownRule(bindings, own, {
    id: 'lockr',
    matches: (kind, url) => SCRIPT_KINDS.has(kind) && isLockrUrl(url),
    rewrite: () => `${bindings.origin}/integrations/lockr/sdk`,
  });

  let active = true;
  let timer: unknown;
  let ownedSdk: LockrSdk | undefined;
  let installedHost: string | undefined;
  let previousHost: string | undefined;
  own(() => {
    if (!active) return;
    active = false;
    if (timer !== undefined) {
      try {
        bindings.clearTimer(timer);
      } catch {
        // Continue through independent SDK restoration.
      }
      timer = undefined;
    }
    try {
      if (ownedSdk && installedHost !== undefined && ownedSdk.host === installedHost) {
        ownedSdk.host = previousHost ?? ownedSdk.host;
      }
    } catch {
      // Publisher replacement wins over provisional rollback.
    }
  });

  let attempts = 0;
  const check = (): void => {
    timer = undefined;
    if (!active) return;
    attempts += 1;
    try {
      const sdk = bindings.getSdk();
      if (sdk && typeof sdk.host === 'string' && sdk.host.length > 0) {
        const nextHost = `${bindings.protocol}//${bindings.host}/integrations/lockr/api`;
        previousHost = sdk.host;
        sdk.host = nextHost;
        ownedSdk = sdk;
        installedHost = nextHost;
        bindings.observe('sdk_host', nextHost);
        return;
      }
    } catch {
      // Treat an unreadable or unwritable SDK as not ready.
    }
    if (attempts >= 50) {
      bindings.observe('readiness_timeout', attempts);
      return;
    }
    timer = bindings.setTimer(check, 50);
  };
  check();
}
