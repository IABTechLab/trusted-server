import type { FirstDisplaySliceActivationContext } from '../transaction';

export type FirstDisplayContextRouteKindV1 = 'script' | 'preload' | 'prefetch';

export interface FirstDisplayContextRouteRuleV1 {
  readonly id: 'permutive';
  readonly matches: (kind: FirstDisplayContextRouteKindV1, url: string) => boolean;
  readonly rewrite: (url: string) => string;
}

type ContextContributor = () => Readonly<Record<string, unknown>> | undefined;

const CONFIG_FIELDS = [
  'apiHost',
  'apiProtocol',
  'cdnBaseUrl',
  'cdnProtocol',
  'secureSignalsApiHost',
  'segmentSyncApiHost',
] as const;
const SCRIPT_KINDS = new Set<FirstDisplayContextRouteKindV1>(['script', 'preload', 'prefetch']);
const MAX_SEGMENTS = 100;

type ConfigField = (typeof CONFIG_FIELDS)[number];
type PermutiveConfig = Record<ConfigField, string>;

interface PermutiveSdk {
  readonly config: PermutiveConfig;
}

interface PermutiveInitialBindings {
  readonly clearTimer: (handle: unknown) => void;
  readonly getSdk: () => PermutiveSdk | undefined;
  readonly host: string;
  readonly observe: (name: string, value: string | number) => void;
  readonly origin: string;
  readonly protocol: 'http:' | 'https:';
  readonly readStorage: (key: string) => string | null;
  readonly registerContext: (contributor: ContextContributor) => () => void;
  readonly registerRoute: (rule: FirstDisplayContextRouteRuleV1) => () => void;
  readonly setTimer: (callback: () => void, delayMs: number) => unknown;
}

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
    const fields: Record<string, unknown> = {};
    for (const key of keys) {
      const descriptor = Object.getOwnPropertyDescriptor(value, key);
      if (!descriptor?.enumerable || !('value' in descriptor)) return undefined;
      fields[key] = descriptor.value;
    }
    return fields;
  } catch {
    return undefined;
  }
}

function snapshotBindings(candidate: unknown): PermutiveInitialBindings | undefined {
  const fields = exactRecord(candidate, [
    'clearTimer',
    'getSdk',
    'host',
    'observe',
    'origin',
    'protocol',
    'readStorage',
    'registerContext',
    'registerRoute',
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
    !['http:', 'https:'].includes(fields.protocol as string) ||
    typeof fields.readStorage !== 'function' ||
    typeof fields.registerContext !== 'function' ||
    typeof fields.registerRoute !== 'function' ||
    typeof fields.setTimer !== 'function'
  ) {
    return undefined;
  }
  try {
    const origin = new URL(fields.origin).origin;
    if (origin !== fields.origin) return undefined;
    if (new URL(origin).protocol !== fields.protocol) return undefined;
  } catch {
    return undefined;
  }
  return fields as unknown as PermutiveInitialBindings;
}

function normalizedSegments(candidate: unknown): readonly string[] {
  if (!Array.isArray(candidate) || candidate.length === 0) return Object.freeze([]);
  const values: string[] = [];
  for (let index = 0; index < candidate.length && values.length < MAX_SEGMENTS; index += 1) {
    const value = candidate[index];
    if (typeof value === 'string' || typeof value === 'number') values.push(String(value));
  }
  return Object.freeze(values);
}

/** Parse the exact current-main Permutive storage shapes without importing its persistent owner. */
export function snapshotPermutiveInitialSegments(raw: string | null): readonly string[] {
  if (!raw) return Object.freeze([]);
  try {
    const data = JSON.parse(raw);
    const primary = normalizedSegments(data?.core?.cohorts?.all);
    if (primary.length > 0) return primary;
    const uploads = data?.eventPublication?.eventUpload;
    if (!Array.isArray(uploads)) return Object.freeze([]);
    for (let index = uploads.length - 1; index >= 0; index -= 1) {
      const entry = uploads[index];
      if (!Array.isArray(entry) || entry.length < 2) continue;
      const fallback = normalizedSegments(entry[1]?.event?.properties?.segments);
      if (fallback.length > 0) return fallback;
    }
  } catch {
    // Malformed or hostile storage is equivalent to no context.
  }
  return Object.freeze([]);
}

function isPermutiveSdkUrl(url: string): boolean {
  const lower = url.toLowerCase();
  return (
    (lower.includes('.edge.permutive.app') || lower.includes('cdn.permutive.com')) &&
    lower.endsWith('-web.js')
  );
}

function bestEffort(action: () => void): void {
  try {
    action();
  } catch {
    // Independent provisional resources must continue releasing in reverse order.
  }
}

/** Own Permutive's one-shot route, auction context, and bounded SDK config readiness. */
export function installPermutiveInitial(
  candidate: unknown,
  own: FirstDisplaySliceActivationContext['own']
): void {
  const bindings = snapshotBindings(candidate);
  if (!bindings || typeof own !== 'function') {
    throw new TypeError('invalid Permutive initial bindings');
  }

  const releaseRoute = bindings.registerRoute(
    Object.freeze({
      id: 'permutive' as const,
      matches: (kind: FirstDisplayContextRouteKindV1, url: string) =>
        SCRIPT_KINDS.has(kind) && isPermutiveSdkUrl(url),
      rewrite: () => `${bindings.origin}/integrations/permutive/sdk`,
    })
  );
  if (typeof releaseRoute !== 'function') throw new TypeError('invalid Permutive route disposer');
  own(releaseRoute);

  const releaseContext = bindings.registerContext(() => {
    let segments: readonly string[];
    try {
      segments = snapshotPermutiveInitialSegments(bindings.readStorage('permutive-app'));
    } catch {
      return undefined;
    }
    return segments.length === 0 ? undefined : Object.freeze({ permutive_segments: segments });
  });
  if (typeof releaseContext !== 'function') {
    throw new TypeError('invalid Permutive context disposer');
  }
  own(releaseContext);

  let active = true;
  let timer: unknown;
  let ownedConfig: PermutiveConfig | undefined;
  let installedValues: PermutiveConfig | undefined;
  let previousValues: PermutiveConfig | undefined;
  own(() => {
    if (!active) return;
    active = false;
    if (timer !== undefined) {
      bestEffort(() => bindings.clearTimer(timer));
      timer = undefined;
    }
    const config = ownedConfig;
    const installed = installedValues;
    const previous = previousValues;
    if (!config || !installed || !previous) return;
    for (const field of CONFIG_FIELDS) {
      bestEffort(() => {
        if (config[field] === installed[field]) config[field] = previous[field];
      });
    }
  });

  const installConfig = (config: PermutiveConfig): boolean => {
    const protocol = bindings.protocol === 'https:' ? 'https' : 'http';
    const next: PermutiveConfig = {
      apiHost: `${bindings.host}/integrations/permutive/api`,
      apiProtocol: protocol,
      cdnBaseUrl: `${bindings.host}/integrations/permutive/cdn`,
      cdnProtocol: protocol,
      secureSignalsApiHost: `${bindings.host}/integrations/permutive/secure-signal`,
      segmentSyncApiHost: `${bindings.host}/integrations/permutive/sync`,
    };
    const previous = {} as PermutiveConfig;
    const written: ConfigField[] = [];
    try {
      for (const field of CONFIG_FIELDS) previous[field] = config[field];
      for (const field of CONFIG_FIELDS) {
        config[field] = next[field];
        written.push(field);
      }
    } catch {
      for (const field of written.reverse()) {
        bestEffort(() => {
          if (config[field] === next[field]) config[field] = previous[field];
        });
      }
      return false;
    }
    ownedConfig = config;
    installedValues = next;
    previousValues = previous;
    bindings.observe('sdk_config', bindings.host);
    return true;
  };

  let attempts = 0;
  const check = (): void => {
    timer = undefined;
    if (!active) return;
    attempts += 1;
    try {
      const sdk = bindings.getSdk();
      if (sdk?.config && installConfig(sdk.config)) return;
    } catch {
      // An unreadable SDK is treated as not ready until the bounded deadline.
    }
    if (attempts >= 50) {
      bindings.observe('readiness_timeout', attempts);
      return;
    }
    timer = bindings.setTimer(check, 50);
  };
  check();
}
