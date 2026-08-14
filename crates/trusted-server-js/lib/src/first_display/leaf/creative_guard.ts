import type { FirstDisplaySliceActivationContext } from '../transaction';

export interface FirstDisplayCreativeGuardV1 {
  readonly version: 1;
  readonly id: 'creative';
  readonly clickGuard: boolean;
  readonly renderGuard: boolean;
  readonly normalizeNavigation: (raw: string) => string | undefined;
  readonly shouldProxyResource: (raw: string) => boolean;
}

interface CreativeInitialBindings {
  readonly config: Readonly<{
    version: 1;
    enabled: true;
    clickGuard: boolean;
    renderGuard: boolean;
  }>;
  readonly location: Readonly<{ href: string; origin: string }>;
  readonly observe: (name: 'guard_count', value: number) => void;
  readonly register: (guard: FirstDisplayCreativeGuardV1) => () => void;
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

function snapshotBindings(candidate: unknown): CreativeInitialBindings | undefined {
  const fields = exactRecord(candidate, ['config', 'location', 'observe', 'register']);
  const config = exactRecord(fields?.config, ['version', 'enabled', 'clickGuard', 'renderGuard']);
  const location = exactRecord(fields?.location, ['href', 'origin']);
  if (
    !fields ||
    !config ||
    config.version !== 1 ||
    config.enabled !== true ||
    typeof config.clickGuard !== 'boolean' ||
    typeof config.renderGuard !== 'boolean' ||
    (!config.clickGuard && !config.renderGuard) ||
    !location ||
    typeof location.href !== 'string' ||
    typeof location.origin !== 'string' ||
    typeof fields.observe !== 'function' ||
    typeof fields.register !== 'function'
  ) {
    return undefined;
  }
  try {
    const href = new URL(location.href);
    if (href.origin !== location.origin || !['http:', 'https:'].includes(href.protocol)) {
      return undefined;
    }
  } catch {
    return undefined;
  }
  return {
    config: Object.freeze({
      version: 1,
      enabled: true,
      clickGuard: config.clickGuard,
      renderGuard: config.renderGuard,
    }),
    location: Object.freeze({ href: location.href, origin: location.origin }),
    observe: fields.observe as CreativeInitialBindings['observe'],
    register: fields.register as CreativeInitialBindings['register'],
  };
}

function normalizeNavigation(raw: string, base: string): string | undefined {
  try {
    const url = new URL(String(raw), base);
    if (
      !['http:', 'https:'].includes(url.protocol) ||
      url.username.length > 0 ||
      url.password.length > 0
    ) {
      return undefined;
    }
    return url.toString();
  } catch {
    return undefined;
  }
}

function shouldProxyResource(raw: string, href: string, origin: string): boolean {
  const value = String(raw || '').trim();
  if (!value || /^(data:|javascript:|blob:|about:)/i.test(value)) return false;
  if (value.startsWith('/first-party/proxy')) return false;
  try {
    const url = new URL(value, href);
    return url.origin !== origin && (url.protocol === 'http:' || url.protocol === 'https:');
  } catch {
    return false;
  }
}

/** Register the selected parser-time creative policy with one generic provisional guard owner. */
export function installCreativeInitial(
  candidate: unknown,
  own: FirstDisplaySliceActivationContext['own']
): void {
  const bindings = snapshotBindings(candidate);
  if (!bindings || typeof own !== 'function') {
    throw new TypeError('tsjs');
  }
  const guard: FirstDisplayCreativeGuardV1 = Object.freeze({
    version: 1,
    id: 'creative',
    clickGuard: bindings.config.clickGuard,
    renderGuard: bindings.config.renderGuard,
    normalizeNavigation: (raw: string) => normalizeNavigation(raw, bindings.location.href),
    shouldProxyResource: (raw: string) =>
      shouldProxyResource(raw, bindings.location.href, bindings.location.origin),
  });
  const release = bindings.register(guard);
  if (typeof release !== 'function') throw new TypeError('tsjs');
  own(release);
  bindings.observe(
    'guard_count',
    (bindings.config.clickGuard ? 1 : 0) + (bindings.config.renderGuard ? 2 : 0)
  );
}
